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
use eframe::egui::{Event, Key, Modifiers};

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
            if mode.contains(TermMode::SGR_MOUSE) {
                // ESC [ < button ; col ; row M   (press; ASCII decimal params)
                bytes.extend_from_slice(b"\x1b[<");
                bytes.extend_from_slice(button.to_string().as_bytes());
                bytes.push(b';');
                bytes.extend_from_slice(col.to_string().as_bytes());
                bytes.push(b';');
                bytes.extend_from_slice(row.to_string().as_bytes());
                bytes.push(b'M');
            } else {
                // Legacy X10: ESC [ M then three bytes, each offset by 32 and
                // clamped to a single byte so col/row past 223 saturate.
                let enc = |v: u32| -> u8 { (32 + v).min(255) as u8 };
                bytes.extend_from_slice(b"\x1b[M");
                bytes.push(enc(button as u32));
                bytes.push(enc(col as u32));
                bytes.push(enc(row as u32));
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
}
