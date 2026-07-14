//! Pure input-encoding seam (terminal-completeness epic, Phase 2 / arch candidate A).
//!
//! Turns a Session's egui keyboard events into the exact bytes a terminal program
//! expects — with no dependency on the GUI, the PTY, or the Session — so every
//! encoding is a byte-equality unit test (the interface IS the test surface).
//! `terminal.rs::read_input` is the thin shell that supplies live state (term
//! mode, whether there is a selection), applies the side effects (clipboard read,
//! copy, interrupt, scroll), and writes `pty_bytes` to the PTY.

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use eframe::egui::{Event, Key, Modifiers, PointerButton};

/// What one frame of input decided. The shell applies these side effects in order.
#[derive(Default, Debug)]
pub struct InputOutcome {
    /// Encoded key + (mode-gated) paste bytes, in event order, ready to write.
    pub pty_bytes: Vec<u8>,
    /// Copy the current selection to the clipboard.
    pub copy: bool,
    /// ...and drop the selection afterward (Ctrl+C / Copy / Cut, not Ctrl+Shift+C).
    pub copy_clears: bool,
    /// Ctrl+C with no selection ⇒ send SIGINT (0x03).
    pub interrupt: bool,
    /// Shift+Home/End/PageUp/PageDown scrolls the scrollback instead of the shell.
    pub scroll: Option<Scroll>,
    /// Ctrl+Shift+V — the pure pass can't read the clipboard, so it flags the
    /// request; the shell reads it and wraps through `paste_seq`.
    pub paste_clipboard: bool,
    /// Ctrl+0 — reset the global terminal font size to the default. The shell
    /// applies it to the shared zoom value; nothing is sent to the PTY.
    pub zoom_reset: bool,
    /// Ctrl/Cmd+F — open (or focus) scrollback search. When set, `pty_bytes`
    /// and other keyboard side effects from this frame are cleared so neither
    /// `0x06` nor companion Text/Enter leak into the PTY.
    pub open_search: bool,
}

/// Width class of one grid cell for key encoding (not paint).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellWide {
    #[default]
    Narrow,
    /// Base cell of a width-2 glyph (`WIDE_CHAR`). `non_bmp` = the char needs
    /// a UTF-16 surrogate pair (emoji): conhost's cooked buffer edits/moves
    /// per UTF-16 unit, so these need 2 key sequences where BMP CJK needs 1.
    /// Evidence: docs/wide-chars.md (2026-07-10 probe matrix).
    WideBase { non_bmp: bool },
    /// Trailing half of a width-2 glyph (`WIDE_CHAR_SPACER` or the wrap
    /// placeholder `LEADING_WIDE_CHAR_SPACER`).
    WideSpacer,
}

impl CellWide {
    /// The one home for wide-cell classification. Every grid walk that cares
    /// about width-2 glyphs — paint plan, snapshot text/cells, key-hint
    /// sampling — classifies through this, so a new alacritty spacer flag is
    /// one edit here (see df46b2d: LEADING_WIDE_CHAR_SPACER needed 4 edits).
    /// `ch` is the cell's char; it only matters for `WideBase` (non-BMP test).
    pub fn classify(flags: alacritty_terminal::term::cell::Flags, ch: char) -> Self {
        use alacritty_terminal::term::cell::Flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            CellWide::WideSpacer
        } else if flags.contains(Flags::WIDE_CHAR) {
            CellWide::WideBase {
                non_bmp: ch > '\u{FFFF}',
            }
        } else {
            CellWide::Narrow
        }
    }

    /// Flags-only spacer test for walks that never look at the base char
    /// (paint plan, snapshots). Routes through [`Self::classify`] so the flag
    /// set still lives in exactly one place.
    pub fn is_wide_spacer(flags: alacritty_terminal::term::cell::Flags) -> bool {
        Self::classify(flags, '\0').is_spacer()
    }

    pub fn is_spacer(self) -> bool {
        self == CellWide::WideSpacer
    }
}

/// Decide what this frame's egui events mean for the terminal. Pure: real
/// egui/alacritty types in, an `InputOutcome` out, no I/O. `mods` is the live
/// frame modifier state (distinct from any per-event `Key` modifiers), used to
/// tell a genuine Alt+letter Text event apart from AltGr.
pub fn process_input(
    events: &[Event],
    mods: Modifiers,
    mode: TermMode,
    has_selection: bool,
) -> InputOutcome {
    let mut out = InputOutcome::default();
    let mut saw_paste = false;
    let mut want_clip_paste = false; // Ctrl+V family
    let mut copy_or_interrupt = false; // Ctrl+C / Copy / Cut
    let mut copy_only = false; // Ctrl+Shift+C

    // Ctrl/Cmd+F anywhere in the frame opens search and suppresses every
    // keyboard side effect this frame (companion Text, Enter, other keys).
    let open_search = events.iter().any(|ev| {
        matches!(
            ev,
            Event::Key {
                key: Key::F,
                pressed: true,
                modifiers: m,
                ..
            } if (m.ctrl || m.command) && !m.shift && !m.alt
        )
    });
    if open_search {
        out.open_search = true;
        return out;
    }

    for ev in events {
        match ev {
            Event::Text(t) => {
                // Windows egui delivers BOTH a Key event and a Text event for
                // Alt+letter. encode_key already sends ESC+letter for alt
                // (without ctrl), so the Text copy must be dropped — but AltGr
                // arrives as Ctrl+Alt and must keep typing (intl layouts).
                // Mirrors encode_key's meta condition exactly.
                if !(mods.alt && !(mods.ctrl || mods.command)) {
                    out.pty_bytes.extend_from_slice(t.as_bytes());
                }
            }
            Event::Paste(s) if !s.is_empty() => {
                out.pty_bytes.extend_from_slice(&paste_seq(mode, s));
                saw_paste = true;
            }
            Event::Paste(_) => {} // empty paste (image-only clipboard) — fall through
            // egui may deliver Ctrl+C / Ctrl+X as these instead of Key events.
            Event::Copy | Event::Cut => copy_or_interrupt = true,
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let m = *modifiers;
                let k = *key;
                let ctrl = m.ctrl || m.command;
                // Shift + Home/End/PageUp/PageDown scrolls the scrollback.
                if m.shift && !ctrl {
                    match k {
                        Key::Home => {
                            out.scroll = Some(Scroll::Top);
                            continue;
                        }
                        Key::End => {
                            out.scroll = Some(Scroll::Bottom);
                            continue;
                        }
                        Key::PageUp => {
                            out.scroll = Some(Scroll::PageUp);
                            continue;
                        }
                        Key::PageDown => {
                            out.scroll = Some(Scroll::PageDown);
                            continue;
                        }
                        _ => {}
                    }
                }
                // Copy/paste policy chords (Ctrl held) — intercepted before
                // encoding. Ctrl+Alt combos fall through to the encoder
                // (Codex binds paste-image to Ctrl+Alt+V).
                if ctrl && !m.alt {
                    match (k, m.shift) {
                        (Key::C, false) => {
                            copy_or_interrupt = true;
                            continue;
                        }
                        (Key::C, true) => {
                            copy_only = true;
                            continue;
                        }
                        (Key::V, _) => {
                            want_clip_paste = true;
                            continue;
                        }
                        (Key::X, _) => continue, // cut handled via Event::Cut
                        // Ctrl+0: reset the global terminal font size. Consumed here so
                        // the shell never sees a stray NUL.
                        (Key::Num0, false) => {
                            out.zoom_reset = true;
                            continue;
                        }
                        _ => {}
                    }
                }
                out.pty_bytes.extend_from_slice(&encode_key(k, m, mode));
            }
            _ => {}
        }
    }

    // Ctrl+Shift+V reads the clipboard; Ctrl+V/Shift+Insert arrive as Event::Paste
    // and are already handled above, so only flag a read when no paste event came.
    if want_clip_paste && !saw_paste {
        out.paste_clipboard = true;
    }
    if copy_or_interrupt {
        if has_selection {
            out.copy = true;
            out.copy_clears = true;
        } else {
            out.interrupt = true; // Ctrl+C with no selection = interrupt
        }
    }
    if copy_only && has_selection {
        out.copy = true; // copy_clears stays false — Ctrl+Shift+C keeps the selection
    }
    out
}

/// Ctrl+Scroll zoom step: move the terminal font size by `steps` whole wheel
/// notches (sign = direction) and clamp to the legible range. Pure: same inputs →
/// same size, so the clamp behavior is a unit test.
pub fn zoom_step(cur: f32, steps: f32) -> f32 {
    (cur + steps * crate::config::FONT_ZOOM_STEP)
        .clamp(crate::config::MIN_FONT_SIZE, crate::config::MAX_FONT_SIZE)
}

/// egui reports ~50 points of `smooth_scroll_delta` per physical wheel notch
/// (Windows default). Scroll and Ctrl+Scroll zoom both accumulate against this
/// so one physical notch is one logical step — independent of font size / row
/// height (issue #8).
pub const WHEEL_NOTCH_PX: f32 = 50.0;

/// Lines of local scrollback (or alt-scroll arrow keys) per physical notch.
/// Keeps one notch a modest, predictable jump (~3 lines) across font zooms
/// and OS "lines per notch" settings (issue #8).
pub const LINES_PER_NOTCH: i32 = 3;

/// Accumulate a smoothed wheel delta and emit only whole steps. egui delivers a
/// wheel notch as sub-frame fractions; dividing `delta` by `unit` (typically
/// [`WHEEL_NOTCH_PX`]) and truncating both drops gentle scrolls (→0) and
/// over-emits fast ones, so the caller carries the fractional part between
/// frames. Returns `(whole_steps, remainder)`: apply `whole_steps` (sign =
/// direction) and feed `remainder` back as the next frame's `accum`. Pure so
/// the accumulate→trunc glue is unit-testable without a live egui `Context`.
pub fn wheel_steps(accum: f32, delta: f32, unit: f32) -> (f32, f32) {
    let acc = accum + delta / unit;
    let steps = acc.trunc();
    (steps, acc - steps)
}

/// Bracketed-paste wrap, gated on the app actually enabling it. ESC is always
/// stripped from the payload so a quoted `ESC[201~` can't end the block early and
/// turn the rest into live keystrokes.
pub fn paste_seq(mode: TermMode, text: &str) -> Vec<u8> {
    let body = text.bytes().filter(|&b| b != 0x1b);
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let mut v = Vec::with_capacity(text.len() + 12);
        v.extend_from_slice(b"\x1b[200~");
        v.extend(body);
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        body.collect()
    }
}

/// What a mouse-wheel tick over the pane decided. Either bytes for the running
/// app (mouse-report events or arrow keys, when the TUI wants them) or a
/// scroll of foreman's own scrollback (the default when the app doesn't).
#[derive(Debug)]
pub enum WheelAction {
    /// Forward to the app: SGR/X10 mouse events, or arrow keys (alt-scroll).
    Pty(Vec<u8>),
    /// Scroll foreman's local scrollback (today's behavior).
    Scrollback(Scroll),
}

/// Decide what one wheel gesture means for the terminal under the pointer.
///
/// `delta_notches`: whole physical wheel notches from
/// `wheel_steps(..., WHEEL_NOTCH_PX)` — + = wheel up / toward older history,
/// − = wheel down. `(col, row)`: the 1-based viewport cell under the pointer,
/// used only for mouse encoding. Pure: same inputs → same bytes.
///
/// Precedence: (1) if the app is in any mouse-reporting mode, forward **one
/// wheel event per notch** (TUIs already multi-line per event — one event per
/// computed line overshoots, issue #8); (2) else if it's on the alternate
/// screen with alternate-scroll enabled, emit `LINES_PER_NOTCH` arrow keys per
/// notch (one arrow = one line); (3) else scroll local scrollback by
/// `delta_notches * LINES_PER_NOTCH` lines.
pub fn wheel_input(delta_notches: i32, mode: TermMode, col: u16, row: u16) -> WheelAction {
    if delta_notches == 0 {
        return WheelAction::Scrollback(Scroll::Delta(0));
    }
    let up = delta_notches > 0;
    let notches = delta_notches.unsigned_abs() as usize;
    let lines = delta_notches.saturating_mul(LINES_PER_NOTCH);

    // (1) Mouse reporting: one wheel event per notch. Wheel up = button 64,
    // wheel down = button 65 (xterm's wheel buttons). Wheel has no release.
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button: u16 = if up { 64 } else { 65 };
        let mut bytes = Vec::new();
        for _ in 0..notches {
            if let Some(ev) = encode_mouse_report(button, col, row, false, mode) {
                bytes.extend_from_slice(&ev);
            }
        }
        return WheelAction::Pty(bytes);
    }

    // (2) Alternate screen + alternate-scroll: one arrow per line. Reuse
    // encode_key so APP_CURSOR (ESC O A vs ESC [ A) is honored exactly as for
    // real arrows.
    if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        let key = if up { Key::ArrowUp } else { Key::ArrowDown };
        let no_mods = Modifiers::default();
        let one = encode_key(key, no_mods, mode);
        let count = notches * (LINES_PER_NOTCH as usize);
        let mut bytes = Vec::with_capacity(one.len() * count);
        for _ in 0..count {
            bytes.extend_from_slice(&one);
        }
        return WheelAction::Pty(bytes);
    }

    // (3) Default: scroll foreman's own scrollback.
    WheelAction::Scrollback(Scroll::Delta(lines))
}

// ---- Mouse reporting (click / drag / motion) --------------------------------

/// Bits frozen at press for the lifetime of a gesture (encoding + tracking).
fn mouse_capture_mode_mask() -> TermMode {
    TermMode::MOUSE_MODE | TermMode::SGR_MOUSE | TermMode::UTF8_MOUSE
}

/// Snapshot encoding/tracking bits for a new capture.
pub fn freeze_mouse_mode(mode: TermMode) -> TermMode {
    mode & mouse_capture_mode_mask()
}

/// Physical mouse button that terminals report (left/middle/right only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseBtn {
    Left,
    Middle,
    Right,
}

impl MouseBtn {
    pub fn from_pointer(b: PointerButton) -> Option<Self> {
        match b {
            PointerButton::Primary => Some(MouseBtn::Left),
            PointerButton::Middle => Some(MouseBtn::Middle),
            PointerButton::Secondary => Some(MouseBtn::Right),
            _ => None,
        }
    }

    /// xterm base button code: left=0, middle=1, right=2.
    pub fn code(self) -> u16 {
        match self {
            MouseBtn::Left => 0,
            MouseBtn::Middle => 1,
            MouseBtn::Right => 2,
        }
    }
}

/// Who owns a gesture for its full press→release lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseOwner {
    /// Local selection / paste (Shift override, scrollback, or no app mode).
    Local,
    /// Application mouse reporting.
    Application,
}

/// One captured button gesture: owner and encoding frozen at press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MouseCapture {
    pub button: MouseBtn,
    pub owner: MouseOwner,
    /// `MOUSE_MODE | SGR_MOUSE | UTF8_MOUSE` snapshot at press.
    pub mode: TermMode,
    /// Base button code frozen at press (before motion/modifier bits).
    pub button_code: u16,
    pub last_col: u16,
    pub last_row: u16,
    pub last_mods: u16,
    /// False when the press was never transmitted (unencodable legacy coords).
    /// Drag/release/cancel must not synthesize events for a silent press.
    pub press_sent: bool,
}

/// Pure mouse-event kind fed into the encoder/router.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    /// Held-button cell motion (drag) or no-button motion (hover).
    Motion,
}

/// Modifier bits added to the xterm button code: Shift=4, Alt=8, Ctrl=16.
pub fn mouse_mod_bits(m: Modifiers) -> u16 {
    let mut bits = 0u16;
    if m.shift {
        bits |= 4;
    }
    if m.alt {
        bits |= 8;
    }
    if m.ctrl || m.command {
        bits |= 16;
    }
    bits
}

/// Decide gesture owner at press. Frozen until matching release.
///
/// Local when: Shift held, main-screen scrolled into history, or no app mouse
/// mode. Otherwise Application.
pub fn mouse_press_owner(shift: bool, display_offset: usize, mode: TermMode) -> MouseOwner {
    if shift || display_offset > 0 || !mode.intersects(TermMode::MOUSE_MODE) {
        MouseOwner::Local
    } else {
        MouseOwner::Application
    }
}

/// Whether the frozen tracking mode emits motion events for this situation.
///
/// - 1000 click-only: never
/// - 1002 drag: only while a button is held
/// - 1003 motion: held drag and no-button hover
pub fn mouse_motion_allowed(mode: TermMode, button_held: bool) -> bool {
    if mode.contains(TermMode::MOUSE_MOTION) {
        true
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        button_held
    } else {
        false // MOUSE_REPORT_CLICK only
    }
}

/// Encode one mouse report. `button` is the full xterm code (incl. motion 32+
/// and mod bits). Release uses lowercase `m` in SGR and code `3+mods` in
/// legacy/UTF-8. Returns `None` when the coordinate is unencodable (legacy
/// col/row > 223, UTF-8 > 2015) so callers drop the event instead of
/// saturating to a wrong cell.
pub fn encode_mouse_report(
    button: u16,
    col: u16,
    row: u16,
    release: bool,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if mode.contains(TermMode::SGR_MOUSE) {
        // SGR 1006: ESC [ < b ; col ; row M/m  (release keeps original button)
        let mut v = Vec::with_capacity(16);
        v.extend_from_slice(b"\x1b[<");
        v.extend_from_slice(button.to_string().as_bytes());
        v.push(b';');
        v.extend_from_slice(col.to_string().as_bytes());
        v.push(b';');
        v.extend_from_slice(row.to_string().as_bytes());
        v.push(if release { b'm' } else { b'M' });
        return Some(v);
    }

    // Release in non-SGR: button code becomes 3 + modifiers (mods already in
    // `button` for press; for release callers pass 3+mods).
    let code = if release {
        // Callers pass the release code directly when release=true for legacy.
        button
    } else {
        button
    };

    if mode.contains(TermMode::UTF8_MOUSE) {
        // UTF-8 mouse 1005: ESC [ M then UTF-8 encoded (32+value) for each field.
        // Coordinates that need more than one UTF-8 continuation beyond U+07FF
        // (value > 2047+? actually xterm limit is 2015 for the coord value
        // before +32 → encoded as 2-byte UTF-8 up to 0x7FF = 2047; plan says 2015).
        if col > 2015 || row > 2015 || code > 2015 {
            return None;
        }
        let mut v = Vec::with_capacity(10);
        v.extend_from_slice(b"\x1b[M");
        push_utf8_mouse_byte(&mut v, code);
        push_utf8_mouse_byte(&mut v, col);
        push_utf8_mouse_byte(&mut v, row);
        return Some(v);
    }

    // Legacy X10: three single bytes, each value+32, must fit in u8 → max 223.
    if col > 223 || row > 223 || code > 223 {
        return None;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        (32 + code) as u8,
        (32 + col) as u8,
        (32 + row) as u8,
    ])
}

fn push_utf8_mouse_byte(out: &mut Vec<u8>, value: u16) {
    // Encode (32 + value) as UTF-8 so coords past 95 still fit in multi-byte.
    let n = 32u32 + value as u32;
    let mut buf = [0u8; 4];
    let s = char::from_u32(n)
        .unwrap_or('\u{FFFD}')
        .encode_utf8(&mut buf);
    out.extend_from_slice(s.as_bytes());
}

/// Build the xterm button code for a press/release/motion event.
///
/// - press: base 0/1/2 + mods
/// - release (SGR): same as press (caller sets release flag in encoder)
/// - release (legacy): 3 + mods
/// - held motion: base + 32 + mods
/// - no-button motion: 35 + mods
pub fn mouse_button_code(kind: MouseEventKind, base: u16, mods: u16, sgr: bool) -> u16 {
    match kind {
        MouseEventKind::Press => base + mods,
        MouseEventKind::Release => {
            if sgr {
                base + mods
            } else {
                3 + mods
            }
        }
        MouseEventKind::Motion => {
            if base == 35 {
                // already no-button motion marker
                35 + mods
            } else {
                base + 32 + mods
            }
        }
    }
}

/// Drive app mouse reporting for a single capture (or hover motion).
///
/// Call after deciding owner. For Application gestures: press/release/motion
/// bytes. Dedupes motion when cell+mods unchanged. `capture` is updated in
/// place (last cell/mods).
/// True when (col,row) can be encoded under the mode's coordinate limits.
pub fn mouse_coords_encodable(col: u16, row: u16, mode: TermMode) -> bool {
    if mode.contains(TermMode::SGR_MOUSE) {
        true
    } else if mode.contains(TermMode::UTF8_MOUSE) {
        col <= 2015 && row <= 2015
    } else {
        col <= 223 && row <= 223
    }
}

pub fn mouse_app_event(
    capture: &mut MouseCapture,
    kind: MouseEventKind,
    col: u16,
    row: u16,
    mods: u16,
) -> Option<Vec<u8>> {
    // A gesture whose press never left the host must not emit drag/release.
    if !matches!(kind, MouseEventKind::Press) && !capture.press_sent {
        return None;
    }
    let sgr = capture.mode.contains(TermMode::SGR_MOUSE);
    let release = matches!(kind, MouseEventKind::Release);
    // Releases must always reach the app: if the pointer cell is unencodable
    // (legacy >223), fall back to the last encodable cell from this gesture.
    let (col, row) = if release && !mouse_coords_encodable(col, row, capture.mode) {
        (capture.last_col, capture.last_row)
    } else {
        (col, row)
    };
    let code = match kind {
        MouseEventKind::Motion => {
            if !mouse_motion_allowed(capture.mode, true) {
                return None;
            }
            if col == capture.last_col && row == capture.last_row && mods == capture.last_mods {
                return None; // same-cell motion dedupe
            }
            mouse_button_code(MouseEventKind::Motion, capture.button_code, mods, sgr)
        }
        MouseEventKind::Press => {
            // Drop unencodable presses rather than saturating a wrong cell.
            if !mouse_coords_encodable(col, row, capture.mode) {
                return None;
            }
            mouse_button_code(MouseEventKind::Press, capture.button_code, mods, sgr)
        }
        MouseEventKind::Release => {
            mouse_button_code(MouseEventKind::Release, capture.button_code, mods, sgr)
        }
    };
    let bytes = encode_mouse_report(code, col, row, release, capture.mode)?;
    capture.last_col = col;
    capture.last_row = row;
    capture.last_mods = mods;
    if matches!(kind, MouseEventKind::Press) {
        capture.press_sent = true;
    }
    Some(bytes)
}

/// No-button hover motion (1003 only). Returns bytes when the cell changed.
pub fn mouse_hover_motion(
    mode: TermMode,
    col: u16,
    row: u16,
    mods: u16,
    last: &mut Option<(u16, u16, u16)>,
) -> Option<Vec<u8>> {
    if !mode.contains(TermMode::MOUSE_MOTION) {
        return None;
    }
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    if let Some((lc, lr, lm)) = *last {
        if lc == col && lr == row && lm == mods {
            return None;
        }
    }
    let code = 35 + mods;
    let bytes = encode_mouse_report(code, col, row, false, mode)?;
    *last = Some((col, row, mods));
    Some(bytes)
}

/// Synthesize a matching release for a captured Application gesture (focus
/// loss / cancel / search-open). Returns bytes once; caller must clear capture.
/// No-op when the original press was never transmitted.
pub fn mouse_cancel_release(capture: &MouseCapture) -> Option<Vec<u8>> {
    if !capture.press_sent {
        return None;
    }
    let sgr = capture.mode.contains(TermMode::SGR_MOUSE);
    let code = mouse_button_code(
        MouseEventKind::Release,
        capture.button_code,
        capture.last_mods,
        sgr,
    );
    encode_mouse_report(code, capture.last_col, capture.last_row, true, capture.mode)
}

/// Pure seam: new press requires content-rect containment AND topmost layer
/// ownership (not a higher menu/popup layer). Existing captures may complete
/// outside the pane.
pub fn mouse_press_topmost_ok(pos_in_content: bool, topmost_is_content_layer: bool) -> bool {
    pos_in_content && topmost_is_content_layer
}

/// Hover (1003) is suppressed while any capture exists or history is scrolled.
pub fn mouse_hover_allowed(
    any_capture: bool,
    display_offset: usize,
    pointer_in_content: bool,
) -> bool {
    pointer_in_content && !any_capture && display_offset == 0
}

/// Resolve a wheel action for a hovered pane.
///
/// Caller already gated on hover. Focus is intentionally ignored: both
/// scrollback and `WheelAction::Pty` (SGR mouse wheel / alt-scroll arrows)
/// apply under the pointer without requiring the pane to be focused
/// (issue #7). Pty bytes from the wheel are navigation, not typed text.
/// Keyboard input stays focus-gated elsewhere in `Session::show`.
///
/// The `focused` parameter is accepted so call sites stay explicit and unit
/// tests pin the unfocused path.
pub fn wheel_action_for_hover(action: WheelAction, _focused: bool) -> WheelAction {
    action
}

/// xterm modifier parameter: `1 + shift + 2*alt + 4*ctrl`. `None` when no
/// modifiers are held (caller emits the unparameterized form).
fn mods_param(m: Modifiers) -> Option<u8> {
    let ctrl = m.ctrl || m.command;
    let bits = (m.shift as u8) | ((m.alt as u8) << 1) | ((ctrl as u8) << 2);
    (bits != 0).then_some(1 + bits)
}

/// Encode one non-policy key press into PTY bytes. Honors DECCKM (application
/// cursor keys) via `mode`, the CSI modifier param for special keys, Ctrl+letter
/// control codes, and Alt-as-Meta. Returns empty for keys it does not map.
pub(crate) fn encode_key(key: Key, mods: Modifiers, mode: TermMode) -> Vec<u8> {
    let ctrl = mods.ctrl || mods.command;
    let app = mode.contains(TermMode::APP_CURSOR);
    let param = mods_param(mods);

    // Cursor / Home / End: SS3 (`ESC O x`) only when unmodified AND in app-cursor
    // mode; CSI with the modifier param when modified; plain CSI otherwise.
    let cursor = |fin: u8| -> Vec<u8> {
        match param {
            Some(p) => vec![0x1b, b'[', b'1', b';', b'0' + p, fin],
            None if app => vec![0x1b, b'O', fin],
            None => vec![0x1b, b'[', fin],
        }
    };
    // Edit / page / F5+ keys: `ESC[<code>~`, with `;<param>` when modified.
    let tilde = |code: &[u8]| -> Vec<u8> {
        let mut v = vec![0x1b, b'['];
        v.extend_from_slice(code);
        if let Some(p) = param {
            v.push(b';');
            v.push(b'0' + p);
        }
        v.push(b'~');
        v
    };
    // F1–F4: SS3 (`ESC O P..S`), CSI `1;<p> P..S` when modified.
    let f1_4 = |fin: u8| -> Vec<u8> {
        match param {
            Some(p) => vec![0x1b, b'[', b'1', b';', b'0' + p, fin],
            None => vec![0x1b, b'O', fin],
        }
    };

    match key {
        Key::ArrowUp => cursor(b'A'),
        Key::ArrowDown => cursor(b'B'),
        Key::ArrowRight => cursor(b'C'),
        Key::ArrowLeft => cursor(b'D'),
        Key::Home => cursor(b'H'),
        Key::End => cursor(b'F'),
        Key::Insert => tilde(b"2"),
        Key::Delete => tilde(b"3"),
        Key::PageUp => tilde(b"5"),
        Key::PageDown => tilde(b"6"),
        Key::F1 => f1_4(b'P'),
        Key::F2 => f1_4(b'Q'),
        Key::F3 => f1_4(b'R'),
        Key::F4 => f1_4(b'S'),
        Key::F5 => tilde(b"15"),
        Key::F6 => tilde(b"17"),
        Key::F7 => tilde(b"18"),
        Key::F8 => tilde(b"19"),
        Key::F9 => tilde(b"20"),
        Key::F10 => tilde(b"21"),
        Key::F11 => tilde(b"23"),
        Key::F12 => tilde(b"24"),
        Key::Enter => vec![b'\r'],
        Key::Tab => vec![b'\t'],
        Key::Backspace => vec![0x7f],
        Key::Escape => vec![0x1b],
        _ => {
            let name = key.name();
            let b = name.as_bytes();
            if b.len() == 1 {
                // Ctrl+Alt+V only → ESC + 0x16 (Codex's paste-image binding).
                // Deliberately NOT all letters: AltGr arrives as Ctrl+Alt on
                // Windows, so an unscoped branch would double-inject a stray
                // ESC+ctrl-code alongside genuine AltGr text (e.g. AltGr+E=€).
                // AltGr+V produces no character on major layouts.
                if ctrl && mods.alt {
                    if b[0].to_ascii_uppercase() == b'V' {
                        return vec![0x1b, 0x16];
                    }
                    return Vec::new();
                }
                // Ctrl+letter → control code (0x01..0x1a).
                if ctrl && !mods.alt {
                    let up = b[0].to_ascii_uppercase();
                    if up.is_ascii_uppercase() {
                        return vec![up - 0x40];
                    }
                }
                // Alt+letter → meta (ESC-prefixed); Alt suppresses the Text event,
                // so case comes from Shift here.
                if mods.alt && !ctrl && b[0].is_ascii_alphabetic() {
                    let letter = if mods.shift {
                        b[0].to_ascii_uppercase()
                    } else {
                        b[0].to_ascii_lowercase()
                    };
                    return vec![0x1b, letter];
                }
            }
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool, shift: bool) -> Modifiers {
        Modifiers {
            alt,
            ctrl,
            shift,
            mac_cmd: false,
            command: false,
        }
    }
    fn none() -> Modifiers {
        mods(false, false, false)
    }
    fn key_ev(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    // ---- mods_param ----------------------------------------------------------
    #[test]
    fn mods_param_none_when_unmodified() {
        assert_eq!(mods_param(none()), None);
    }
    #[test]
    fn mods_param_encodes_each_modifier() {
        assert_eq!(mods_param(mods(true, false, false)), Some(5)); // ctrl
        assert_eq!(mods_param(mods(false, false, true)), Some(2)); // shift
        assert_eq!(mods_param(mods(false, true, false)), Some(3)); // alt
        assert_eq!(mods_param(mods(true, false, true)), Some(6)); // ctrl+shift
    }

    // ---- encode_key: cursor keys + DECCKM ------------------------------------
    #[test]
    fn arrow_up_plain_is_csi() {
        assert_eq!(
            encode_key(Key::ArrowUp, none(), TermMode::empty()),
            b"\x1b[A"
        );
    }
    #[test]
    fn arrow_up_in_app_cursor_mode_is_ss3() {
        assert_eq!(
            encode_key(Key::ArrowUp, none(), TermMode::APP_CURSOR),
            b"\x1bOA"
        );
    }
    #[test]
    fn ctrl_arrow_right_uses_modifier_param() {
        assert_eq!(
            encode_key(Key::ArrowRight, mods(true, false, false), TermMode::empty()),
            b"\x1b[1;5C"
        );
    }
    #[test]
    fn modified_arrow_is_csi_even_in_app_cursor_mode() {
        // DECCKM only affects the UNMODIFIED form.
        assert_eq!(
            encode_key(Key::ArrowUp, mods(true, false, false), TermMode::APP_CURSOR),
            b"\x1b[1;5A"
        );
    }
    #[test]
    fn home_end_follow_decckm() {
        assert_eq!(encode_key(Key::Home, none(), TermMode::empty()), b"\x1b[H");
        assert_eq!(
            encode_key(Key::Home, none(), TermMode::APP_CURSOR),
            b"\x1bOH"
        );
        assert_eq!(encode_key(Key::End, none(), TermMode::empty()), b"\x1b[F");
    }

    // ---- encode_key: function + edit keys ------------------------------------
    #[test]
    fn f1_through_f4_are_ss3() {
        assert_eq!(encode_key(Key::F1, none(), TermMode::empty()), b"\x1bOP");
        assert_eq!(encode_key(Key::F4, none(), TermMode::empty()), b"\x1bOS");
    }
    #[test]
    fn f5_and_f12_are_tilde_sequences() {
        assert_eq!(encode_key(Key::F5, none(), TermMode::empty()), b"\x1b[15~");
        assert_eq!(encode_key(Key::F12, none(), TermMode::empty()), b"\x1b[24~");
    }
    #[test]
    fn shift_f5_carries_the_modifier_param() {
        assert_eq!(
            encode_key(Key::F5, mods(false, false, true), TermMode::empty()),
            b"\x1b[15;2~"
        );
    }
    #[test]
    fn delete_and_insert_and_page_keys() {
        assert_eq!(
            encode_key(Key::Delete, none(), TermMode::empty()),
            b"\x1b[3~"
        );
        assert_eq!(
            encode_key(Key::Insert, none(), TermMode::empty()),
            b"\x1b[2~"
        );
        assert_eq!(
            encode_key(Key::PageUp, none(), TermMode::empty()),
            b"\x1b[5~"
        );
        assert_eq!(
            encode_key(Key::PageDown, none(), TermMode::empty()),
            b"\x1b[6~"
        );
    }

    // ---- encode_key: control codes, meta, plain keys -------------------------
    #[test]
    fn ctrl_letter_is_a_control_code() {
        assert_eq!(
            encode_key(Key::A, mods(true, false, false), TermMode::empty()),
            vec![0x01]
        );
    }
    #[test]
    fn alt_letter_is_meta_esc_prefixed() {
        assert_eq!(
            encode_key(Key::B, mods(false, true, false), TermMode::empty()),
            vec![0x1b, b'b']
        );
        assert_eq!(
            encode_key(Key::B, mods(false, true, true), TermMode::empty()),
            vec![0x1b, b'B']
        );
    }
    #[test]
    fn plain_control_keys() {
        assert_eq!(
            encode_key(Key::Enter, none(), TermMode::empty()),
            vec![b'\r']
        );
        assert_eq!(encode_key(Key::Tab, none(), TermMode::empty()), vec![b'\t']);
        assert_eq!(
            encode_key(Key::Backspace, none(), TermMode::empty()),
            vec![0x7f]
        );
        assert_eq!(
            encode_key(Key::Escape, none(), TermMode::empty()),
            vec![0x1b]
        );
    }

    // ---- paste_seq -----------------------------------------------------------
    #[test]
    fn paste_seq_wraps_only_when_bracketed_mode_set() {
        assert_eq!(
            paste_seq(TermMode::BRACKETED_PASTE, "hi"),
            b"\x1b[200~hi\x1b[201~"
        );
        assert_eq!(paste_seq(TermMode::empty(), "hi"), b"hi");
    }
    #[test]
    fn paste_seq_strips_esc_from_payload() {
        // Embedded ESC[201~ must not survive to terminate the block early.
        let out = paste_seq(TermMode::BRACKETED_PASTE, "a\x1b[201~b");
        assert_eq!(out, b"\x1b[200~a[201~b\x1b[201~");
    }

    // ---- process_input: routing + policy -------------------------------------
    #[test]
    fn typed_text_passes_through() {
        let out = process_input(
            &[Event::Text("a".into())],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, b"a");
        assert!(!out.copy && !out.interrupt && out.scroll.is_none());
    }
    #[test]
    fn arrow_routes_through_encoder() {
        let out = process_input(
            &[key_ev(Key::ArrowUp, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, b"\x1b[A");
    }
    #[test]
    fn ctrl_c_with_selection_copies_and_clears() {
        let out = process_input(
            &[key_ev(Key::C, mods(true, false, false))],
            Modifiers::default(),
            TermMode::empty(),
            true,
        );
        assert!(out.copy && out.copy_clears && !out.interrupt);
        assert!(out.pty_bytes.is_empty());
    }
    #[test]
    fn ctrl_c_without_selection_interrupts() {
        let out = process_input(
            &[key_ev(Key::C, mods(true, false, false))],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert!(out.interrupt && !out.copy);
    }
    #[test]
    fn ctrl_shift_c_copies_without_clearing() {
        let out = process_input(
            &[key_ev(Key::C, mods(true, false, true))],
            Modifiers::default(),
            TermMode::empty(),
            true,
        );
        assert!(out.copy && !out.copy_clears && !out.interrupt);
    }
    #[test]
    fn copy_event_copies_and_clears() {
        let out = process_input(
            &[Event::Copy],
            Modifiers::default(),
            TermMode::empty(),
            true,
        );
        assert!(out.copy && out.copy_clears);
    }
    #[test]
    fn ctrl_shift_v_requests_clipboard_paste() {
        let out = process_input(
            &[key_ev(Key::V, mods(true, false, true))],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert!(out.paste_clipboard);
        assert!(out.pty_bytes.is_empty());
    }
    #[test]
    fn paste_event_takes_precedence_over_ctrl_v_clipboard_read() {
        // Ctrl+V also yields an Event::Paste; the event wins, no clipboard re-read.
        let out = process_input(
            &[
                key_ev(Key::V, mods(true, false, false)),
                Event::Paste("x".into()),
            ],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, b"x");
        assert!(!out.paste_clipboard);
    }
    #[test]
    fn paste_event_is_bracketed_when_mode_set() {
        let out = process_input(
            &[Event::Paste("x".into())],
            Modifiers::default(),
            TermMode::BRACKETED_PASTE,
            false,
        );
        assert_eq!(out.pty_bytes, b"\x1b[200~x\x1b[201~");
    }
    #[test]
    fn shift_pageup_scrolls_instead_of_sending() {
        let out = process_input(
            &[key_ev(Key::PageUp, mods(false, false, true))],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert!(matches!(out.scroll, Some(Scroll::PageUp)));
        assert!(out.pty_bytes.is_empty());
    }
    #[test]
    fn alt_letter_sends_meta_only_once_despite_text_event() {
        // Windows egui delivers BOTH the Key event and a Text event for
        // Alt+letter; only the ESC-prefixed meta byte may reach the PTY.
        let live = Modifiers {
            alt: true,
            ..Default::default()
        };
        let out = process_input(
            &[
                key_ev(Key::V, mods(false, true, false)),
                Event::Text("v".into()),
            ],
            live,
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, b"\x1bv");
    }
    #[test]
    fn altgr_text_still_types() {
        // AltGr arrives as Ctrl+Alt on Windows; intl layouts must keep typing.
        let live = Modifiers {
            alt: true,
            ctrl: true,
            ..Default::default()
        };
        let out = process_input(&[Event::Text("@".into())], live, TermMode::empty(), false);
        assert_eq!(out.pty_bytes, b"@");
    }
    #[test]
    fn ctrl_alt_v_encodes_meta_ctrl_v_not_clipboard_paste() {
        // Codex's second paste-image binding; must not be shadowed by the
        // Ctrl+V clipboard chord.
        let live = Modifiers {
            alt: true,
            ctrl: true,
            ..Default::default()
        };
        let out = process_input(
            &[key_ev(Key::V, mods(true, true, false))],
            live,
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, b"\x1b\x16");
        assert!(!out.paste_clipboard);
    }
    #[test]
    fn ctrl_alt_other_letters_produce_nothing() {
        // Ctrl+Alt on any letter other than V produces no bytes. This prevents
        // the Ctrl+Alt branch from interfering with AltGr text on intl layouts.
        let live = Modifiers {
            alt: true,
            ctrl: true,
            ..Default::default()
        };
        let out = process_input(
            &[key_ev(Key::C, mods(true, true, false))],
            live,
            TermMode::empty(),
            true,
        );
        assert!(out.pty_bytes.is_empty());
        assert!(!out.copy && !out.interrupt);
    }
    #[test]
    fn altgr_letter_with_text_types_only_the_text() {
        // German AltGr+E: Key::E with ctrl+alt PLUS Text("€") in one frame —
        // only the € may reach the PTY.
        let live = Modifiers {
            alt: true,
            ctrl: true,
            ..Default::default()
        };
        let out = process_input(
            &[
                key_ev(Key::E, mods(true, true, false)),
                Event::Text("€".into()),
            ],
            live,
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, "€".as_bytes());
    }
    #[test]
    fn empty_paste_event_still_flags_clipboard_read() {
        // With an image-only clipboard, egui may deliver Paste("") for Ctrl+V.
        // The empty event must neither type anything nor satisfy the request —
        // the shell then falls back to the clipboard (text, else image → 0x16).
        let live = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let out = process_input(
            &[
                key_ev(Key::V, mods(true, false, false)),
                Event::Paste(String::new()),
            ],
            live,
            TermMode::empty(),
            false,
        );
        assert!(out.paste_clipboard);
        assert!(out.pty_bytes.is_empty());
    }

    // ---- wide-cell classification (paint + snapshot seam) --------------------
    #[test]
    fn cellwide_classify_is_the_single_classification_home() {
        use alacritty_terminal::term::cell::Flags;
        assert_eq!(CellWide::classify(Flags::empty(), 'a'), CellWide::Narrow);
        // BMP wide (CJK) vs non-BMP wide (emoji).
        assert_eq!(
            CellWide::classify(Flags::WIDE_CHAR, '中'),
            CellWide::WideBase { non_bmp: false }
        );
        assert_eq!(
            CellWide::classify(Flags::WIDE_CHAR, '🤣'),
            CellWide::WideBase { non_bmp: true }
        );
        assert_eq!(
            CellWide::classify(Flags::WIDE_CHAR_SPACER, ' '),
            CellWide::WideSpacer
        );
        // The df46b2d lesson: wrap placeholders are spacers too.
        assert_eq!(
            CellWide::classify(Flags::LEADING_WIDE_CHAR_SPACER, ' '),
            CellWide::WideSpacer
        );
        // Style flags must not affect classification; flags-only helper agrees.
        assert_eq!(
            CellWide::classify(Flags::BOLD | Flags::WIDE_CHAR, '中'),
            CellWide::WideBase { non_bmp: false }
        );
        assert!(CellWide::is_wide_spacer(Flags::LEADING_WIDE_CHAR_SPACER));
        assert!(!CellWide::is_wide_spacer(Flags::WIDE_CHAR));
    }

    // ---- zoom ----------------------------------------------------------------
    #[test]
    fn ctrl_0_requests_zoom_reset_and_sends_nothing() {
        let out = process_input(
            &[key_ev(Key::Num0, mods(true, false, false))],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert!(out.zoom_reset);
        assert!(out.pty_bytes.is_empty());
    }
    #[test]
    fn plain_0_types_through_without_reset() {
        let out = process_input(
            &[key_ev(Key::Num0, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert!(!out.zoom_reset);
    }
    #[test]
    fn zoom_step_moves_by_whole_notches() {
        assert_eq!(zoom_step(13.0, 1.0), 13.0 + crate::config::FONT_ZOOM_STEP);
        assert_eq!(
            zoom_step(13.0, -2.0),
            13.0 - 2.0 * crate::config::FONT_ZOOM_STEP
        );
    }
    #[test]
    fn zoom_step_clamps_to_bounds() {
        assert_eq!(
            zoom_step(crate::config::MIN_FONT_SIZE, -100.0),
            crate::config::MIN_FONT_SIZE
        );
        assert_eq!(
            zoom_step(crate::config::MAX_FONT_SIZE, 100.0),
            crate::config::MAX_FONT_SIZE
        );
    }

    // ---- wheel_input (delta = whole notches; issue #8) ------------------------
    // Scroll doesn't derive PartialEq, so match and read the inner delta.
    fn scrollback_delta(a: WheelAction) -> i32 {
        match a {
            WheelAction::Scrollback(Scroll::Delta(d)) => d,
            other => panic!("expected Scrollback(Delta), got {other:?}"),
        }
    }
    fn pty(a: WheelAction) -> Vec<u8> {
        match a {
            WheelAction::Pty(b) => b,
            other => panic!("expected Pty, got {other:?}"),
        }
    }
    fn arrows_up(n: usize) -> Vec<u8> {
        std::iter::repeat_n(b"\x1b[A".as_slice(), n)
            .flatten()
            .copied()
            .collect()
    }
    fn arrows_down(n: usize) -> Vec<u8> {
        std::iter::repeat_n(b"\x1b[B".as_slice(), n)
            .flatten()
            .copied()
            .collect()
    }
    fn ss3_up(n: usize) -> Vec<u8> {
        std::iter::repeat_n(b"\x1bOA".as_slice(), n)
            .flatten()
            .copied()
            .collect()
    }
    fn ss3_down(n: usize) -> Vec<u8> {
        std::iter::repeat_n(b"\x1bOB".as_slice(), n)
            .flatten()
            .copied()
            .collect()
    }

    #[test]
    fn wheel_one_notch_scrolls_lines_per_notch() {
        // Issue #8: one physical notch → fixed line count, not rh-dependent.
        assert_eq!(
            scrollback_delta(wheel_input(1, TermMode::empty(), 1, 1)),
            LINES_PER_NOTCH
        );
        assert_eq!(
            scrollback_delta(wheel_input(-1, TermMode::empty(), 1, 1)),
            -LINES_PER_NOTCH
        );
        assert_eq!(
            scrollback_delta(wheel_input(2, TermMode::empty(), 1, 1)),
            2 * LINES_PER_NOTCH
        );
    }

    #[test]
    fn wheel_alt_scroll_emits_lines_per_notch_arrows() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        let n = LINES_PER_NOTCH as usize;
        // One notch → LINES_PER_NOTCH arrows (plain CSI; APP_CURSOR off).
        assert_eq!(pty(wheel_input(1, mode, 1, 1)), arrows_up(n));
        assert_eq!(pty(wheel_input(-1, mode, 1, 1)), arrows_down(n));
        // Two notches → 2× lines.
        assert_eq!(pty(wheel_input(2, mode, 1, 1)), arrows_up(2 * n));
    }

    #[test]
    fn wheel_alt_scroll_honors_app_cursor_mode() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL | TermMode::APP_CURSOR;
        // SS3 form (ESC O A / ESC O B) when the app set DECCKM — still one
        // arrow per line within the notch.
        let n = LINES_PER_NOTCH as usize;
        assert_eq!(pty(wheel_input(1, mode, 1, 1)), ss3_up(n));
        assert_eq!(pty(wheel_input(-1, mode, 1, 1)), ss3_down(n));
    }

    #[test]
    fn wheel_sgr_mouse_one_event_per_notch_not_per_line() {
        // Issue #8 cause (2): TUIs already multi-line per wheel event, so one
        // notch must be ONE SGR event — not LINES_PER_NOTCH events.
        let mode = TermMode::MOUSE_MODE | TermMode::SGR_MOUSE;
        let one = pty(wheel_input(1, mode, 5, 10));
        assert_eq!(one, b"\x1b[<64;5;10M");
        assert_eq!(pty(wheel_input(-1, mode, 5, 10)), b"\x1b[<65;5;10M");
        // Two notches → two events (still not 2×LINES_PER_NOTCH).
        assert_eq!(
            pty(wheel_input(2, mode, 5, 10)),
            b"\x1b[<64;5;10M\x1b[<64;5;10M"
        );
        // Must not emit one event per scrollback line within a notch.
        let mut if_per_line = Vec::new();
        for _ in 0..LINES_PER_NOTCH {
            if_per_line.extend_from_slice(&one);
        }
        assert_ne!(one, if_per_line);
    }

    #[test]
    fn wheel_x10_mouse_one_event_per_notch() {
        let mode = TermMode::MOUSE_MODE; // no SGR → legacy X10 encoding
        // ESC [ M then (32+button), (32+col), (32+row).
        let expected = vec![0x1b, b'[', b'M', 32 + 64, 32 + 5, 32 + 10];
        assert_eq!(pty(wheel_input(1, mode, 5, 10)), expected);
    }

    #[test]
    fn wheel_mouse_mode_beats_alt_scroll() {
        // Both mouse-reporting AND alt-scroll flags set → mouse wins (one event,
        // not LINES_PER_NOTCH arrows).
        let mode = TermMode::MOUSE_MODE
            | TermMode::SGR_MOUSE
            | TermMode::ALT_SCREEN
            | TermMode::ALTERNATE_SCROLL;
        assert_eq!(pty(wheel_input(1, mode, 5, 10)), b"\x1b[<64;5;10M");
    }

    #[test]
    fn wheel_zero_delta_is_a_noop_scrollback() {
        assert_eq!(scrollback_delta(wheel_input(0, TermMode::empty(), 1, 1)), 0);
    }

    #[test]
    fn wheel_full_notch_points_map_to_one_step_independent_of_row_height() {
        // Issue #8 cause (1): accumulate against WHEEL_NOTCH_PX, not row height.
        let (steps, rem) = wheel_steps(0.0, WHEEL_NOTCH_PX, WHEEL_NOTCH_PX);
        assert_eq!(steps, 1.0);
        assert_eq!(rem, 0.0);
        assert_eq!(
            scrollback_delta(wheel_input(steps as i32, TermMode::empty(), 1, 1)),
            LINES_PER_NOTCH
        );
        // A row-height unit of 16pt would have emitted 3 steps for 50pt; we must
        // not use that path — one notch is still one step.
        let (if_rh, _) = wheel_steps(0.0, WHEEL_NOTCH_PX, 16.0);
        assert!(if_rh > 1.0, "documents the old over-sensitive rh path");
        assert_ne!(if_rh, steps);
    }

    // ---- wheel_action_for_hover (issue #7) -----------------------------------
    // Hover is the only gate for wheel. Focus is irrelevant: Pty bytes from
    // the wheel are navigation (SGR / arrows), not typed input.

    #[test]
    fn wheel_pty_forwards_on_unfocused_hover() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        let action = wheel_input(1, mode, 1, 1);
        assert!(matches!(action, WheelAction::Pty(_)));
        // Unfocused must still forward — regression pin for issue #7.
        let applied = wheel_action_for_hover(action, false);
        assert_eq!(pty(applied), arrows_up(LINES_PER_NOTCH as usize));
    }

    #[test]
    fn wheel_pty_forwards_when_focused() {
        let mode = TermMode::MOUSE_MODE | TermMode::SGR_MOUSE;
        let action = wheel_input(1, mode, 5, 10);
        let applied = wheel_action_for_hover(action, true);
        assert_eq!(pty(applied), b"\x1b[<64;5;10M");
    }

    #[test]
    fn wheel_scrollback_applies_on_unfocused_hover() {
        let action = wheel_input(1, TermMode::empty(), 1, 1);
        let applied = wheel_action_for_hover(action, false);
        assert_eq!(scrollback_delta(applied), LINES_PER_NOTCH);
    }

    // ---- wheel_steps ---------------------------------------------------------
    // The accumulate→trunc glue: egui delivers a wheel notch as smoothed
    // per-frame fractions, so `show()` carries the sub-step remainder between
    // frames and emits only whole steps. Feed each returned remainder back in as
    // the next call's `accum`, exactly as the two call sites do.

    #[test]
    fn wheel_steps_gentle_scroll_accumulates_across_frames_to_one_step() {
        // A quarter-unit per frame: three frames emit nothing; the fourth
        // crosses one whole step and resets the carry.
        let unit = 4.0;
        let mut accum = 0.0;
        for _ in 0..3 {
            let (steps, rem) = wheel_steps(accum, 1.0, unit);
            assert_eq!(steps, 0.0);
            accum = rem;
        }
        let (steps, rem) = wheel_steps(accum, 1.0, unit);
        assert_eq!(steps, 1.0);
        assert_eq!(rem, 0.0);
    }

    #[test]
    fn wheel_steps_fast_flick_emits_multiple_steps_without_over_emitting() {
        // One fat frame worth 3.5 units emits exactly 3 whole steps and carries
        // 0.5 — never rounds up to 4.
        let (steps, rem) = wheel_steps(0.0, 14.0, 4.0);
        assert_eq!(steps, 3.0);
        assert_eq!(rem, 0.5);
    }

    #[test]
    fn wheel_steps_remainder_carries_sign() {
        // Scrolling the other way keeps a negative remainder so the next frame
        // keeps accumulating downward instead of cancelling the carry.
        let (steps, rem) = wheel_steps(0.0, -6.0, 4.0);
        assert_eq!(steps, -1.0);
        assert_eq!(rem, -0.5);
    }

    #[test]
    fn wheel_steps_zero_delta_is_a_noop() {
        // No wheel this frame: no steps, and the carried sub-unit remainder is
        // returned untouched.
        let (steps, rem) = wheel_steps(0.42, 0.0, 4.0);
        assert_eq!(steps, 0.0);
        assert_eq!(rem, 0.42);
    }

    // ---- mouse protocol encoding --------------------------------------------

    #[test]
    fn sgr_left_press_at_5_10() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            encode_mouse_report(0, 5, 10, false, mode).unwrap(),
            b"\x1b[<0;5;10M"
        );
    }

    #[test]
    fn sgr_right_release() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            encode_mouse_report(2, 5, 10, true, mode).unwrap(),
            b"\x1b[<2;5;10m"
        );
    }

    #[test]
    fn sgr_left_drag() {
        let mode = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(
            encode_mouse_report(32, 5, 10, false, mode).unwrap(),
            b"\x1b[<32;5;10M"
        );
    }

    #[test]
    fn sgr_no_button_motion() {
        let mode = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(
            encode_mouse_report(35, 5, 10, false, mode).unwrap(),
            b"\x1b[<35;5;10M"
        );
    }

    #[test]
    fn legacy_left_press_bytes() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        // ESC [ M  space(0+32)  %(5+32)  *(10+32)
        assert_eq!(
            encode_mouse_report(0, 5, 10, false, mode).unwrap(),
            vec![0x1b, 0x5b, 0x4d, 0x20, 0x25, 0x2a]
        );
    }

    #[test]
    fn legacy_release_bytes() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        // release code 3 → 32+3 = 0x23 '#'
        assert_eq!(
            encode_mouse_report(3, 5, 10, true, mode).unwrap(),
            vec![0x1b, 0x5b, 0x4d, 0x23, 0x25, 0x2a]
        );
    }

    #[test]
    fn sgr_beats_utf8_when_both_set() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE | TermMode::UTF8_MOUSE;
        let b = encode_mouse_report(0, 5, 10, false, mode).unwrap();
        assert!(b.starts_with(b"\x1b[<"), "SGR form wins: {b:?}");
    }

    #[test]
    fn legacy_drops_unencodable_coords() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        assert!(encode_mouse_report(0, 224, 1, false, mode).is_none());
        assert!(encode_mouse_report(0, 1, 224, false, mode).is_none());
    }

    #[test]
    fn release_beyond_legacy_limit_uses_clamped_last_cell() {
        let mode = freeze_mouse_mode(TermMode::MOUSE_REPORT_CLICK);
        let mut cap = MouseCapture {
            button: MouseBtn::Left,
            owner: MouseOwner::Application,
            mode,
            button_code: 0,
            last_col: 5,
            last_row: 10,
            last_mods: 0,
            press_sent: true,
        };
        // Press was valid; release at col 300 must still emit button-up at last cell.
        let rel = mouse_app_event(&mut cap, MouseEventKind::Release, 300, 10, 0);
        assert!(rel.is_some(), "release must not be dropped");
        assert_eq!(
            rel.unwrap(),
            encode_mouse_report(3, 5, 10, true, mode).unwrap()
        );
    }

    #[test]
    fn utf8_drops_coords_past_2015() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::UTF8_MOUSE;
        assert!(encode_mouse_report(0, 2016, 1, false, mode).is_none());
        assert!(encode_mouse_report(0, 2015, 1, false, mode).is_some());
    }

    #[test]
    fn mouse_mod_bits_shift_alt_ctrl() {
        assert_eq!(mouse_mod_bits(Modifiers::SHIFT), 4);
        assert_eq!(mouse_mod_bits(Modifiers::ALT), 8);
        assert_eq!(mouse_mod_bits(Modifiers::CTRL), 16);
        let all = Modifiers {
            shift: true,
            alt: true,
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(mouse_mod_bits(all), 4 + 8 + 16);
    }

    #[test]
    fn press_owner_shift_or_scrollback_or_no_mode_is_local() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse_press_owner(true, 0, mode), MouseOwner::Local);
        assert_eq!(mouse_press_owner(false, 3, mode), MouseOwner::Local);
        assert_eq!(
            mouse_press_owner(false, 0, TermMode::empty()),
            MouseOwner::Local
        );
        assert_eq!(mouse_press_owner(false, 0, mode), MouseOwner::Application);
    }

    #[test]
    fn motion_gates_by_mode() {
        assert!(!mouse_motion_allowed(TermMode::MOUSE_REPORT_CLICK, true));
        assert!(mouse_motion_allowed(TermMode::MOUSE_DRAG, true));
        assert!(!mouse_motion_allowed(TermMode::MOUSE_DRAG, false));
        assert!(mouse_motion_allowed(TermMode::MOUSE_MOTION, false));
        assert!(mouse_motion_allowed(TermMode::MOUSE_MOTION, true));
    }

    #[test]
    fn app_gesture_press_motion_release_and_dedupe() {
        let mode = freeze_mouse_mode(TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE);
        let mut cap = MouseCapture {
            button: MouseBtn::Left,
            owner: MouseOwner::Application,
            mode,
            button_code: 0,
            last_col: 5,
            last_row: 10,
            last_mods: 0,
            press_sent: false,
        };
        assert_eq!(
            mouse_app_event(&mut cap, MouseEventKind::Press, 5, 10, 0).unwrap(),
            b"\x1b[<0;5;10M"
        );
        // same cell motion → none
        assert!(mouse_app_event(&mut cap, MouseEventKind::Motion, 5, 10, 0).is_none());
        assert_eq!(
            mouse_app_event(&mut cap, MouseEventKind::Motion, 6, 10, 0).unwrap(),
            b"\x1b[<32;6;10M"
        );
        assert_eq!(
            mouse_app_event(&mut cap, MouseEventKind::Release, 6, 10, 0).unwrap(),
            b"\x1b[<0;6;10m"
        );
    }

    #[test]
    fn mid_gesture_mode_change_uses_frozen_mode() {
        // Capture frozen with SGR; live mode later drops SGR — still encodes SGR.
        let mut cap = MouseCapture {
            button: MouseBtn::Left,
            owner: MouseOwner::Application,
            mode: freeze_mouse_mode(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE),
            button_code: 0,
            last_col: 1,
            last_row: 1,
            last_mods: 0,
            press_sent: true,
        };
        let rel = mouse_app_event(&mut cap, MouseEventKind::Release, 2, 3, 0).unwrap();
        assert_eq!(rel, b"\x1b[<0;2;3m");
    }

    #[test]
    fn cancel_release_matches_last_cell() {
        let cap = MouseCapture {
            button: MouseBtn::Right,
            owner: MouseOwner::Application,
            mode: freeze_mouse_mode(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE),
            button_code: 2,
            last_col: 5,
            last_row: 10,
            last_mods: 0,
            press_sent: true,
        };
        assert_eq!(mouse_cancel_release(&cap).unwrap(), b"\x1b[<2;5;10m");
    }

    #[test]
    fn unencodable_press_suppresses_drag_and_release() {
        let mode = freeze_mouse_mode(TermMode::MOUSE_REPORT_CLICK); // legacy
        let mut cap = MouseCapture {
            button: MouseBtn::Left,
            owner: MouseOwner::Application,
            mode,
            button_code: 0,
            last_col: 224,
            last_row: 1,
            last_mods: 0,
            press_sent: false,
        };
        // Press at col 224 is unencodable — no bytes, press_sent stays false.
        assert!(mouse_app_event(&mut cap, MouseEventKind::Press, 224, 1, 0).is_none());
        assert!(!cap.press_sent);
        assert!(mouse_app_event(&mut cap, MouseEventKind::Motion, 10, 1, 0).is_none());
        assert!(mouse_app_event(&mut cap, MouseEventKind::Release, 10, 1, 0).is_none());
        assert!(mouse_cancel_release(&cap).is_none());
    }

    #[test]
    fn press_requires_topmost_content_ownership() {
        assert!(mouse_press_topmost_ok(true, true));
        assert!(!mouse_press_topmost_ok(true, false)); // menu above
        assert!(!mouse_press_topmost_ok(false, true)); // titlebar / outside
    }

    #[test]
    fn hover_suppressed_with_capture_or_history() {
        assert!(mouse_hover_allowed(false, 0, true));
        assert!(!mouse_hover_allowed(true, 0, true)); // local or app capture
        assert!(!mouse_hover_allowed(false, 3, true)); // scrolled history
        assert!(!mouse_hover_allowed(false, 0, false));
    }

    #[test]
    fn hover_motion_only_in_1003_and_dedupes() {
        let mode = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        let mut last = None;
        assert_eq!(
            mouse_hover_motion(mode, 5, 10, 0, &mut last).unwrap(),
            b"\x1b[<35;5;10M"
        );
        assert!(mouse_hover_motion(mode, 5, 10, 0, &mut last).is_none());
        assert!(
            mouse_hover_motion(
                TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE,
                6,
                10,
                0,
                &mut last
            )
            .is_none()
        );
    }

    #[test]
    fn ctrl_f_opens_search_and_suppresses_pty_bytes() {
        let live = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let events = [
            key_ev(Key::F, mods(true, false, false)),
            Event::Text("f".into()),
            key_ev(Key::Enter, Modifiers::default()),
        ];
        let out = process_input(&events, live, TermMode::empty(), false);
        assert!(out.open_search);
        assert!(out.pty_bytes.is_empty(), "no 0x06 or companion keys");
        assert!(!out.interrupt && !out.copy && !out.paste_clipboard);
    }

    #[test]
    fn ctrl_f_with_shift_is_not_search() {
        // Ctrl+Shift+F must not open search (plan: no Shift/Alt).
        let live = Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        let out = process_input(
            &[key_ev(
                Key::F,
                Modifiers {
                    ctrl: true,
                    shift: true,
                    ..Default::default()
                },
            )],
            live,
            TermMode::empty(),
            false,
        );
        assert!(!out.open_search);
    }

    #[test]
    fn wheel_still_uses_shared_encoder_for_sgr() {
        let mode = TermMode::MOUSE_MODE | TermMode::SGR_MOUSE;
        assert_eq!(pty(wheel_input(1, mode, 5, 10)), b"\x1b[<64;5;10M");
        assert_eq!(pty(wheel_input(-1, mode, 5, 10)), b"\x1b[<65;5;10M");
    }
}
