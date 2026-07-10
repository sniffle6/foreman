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

/// Live-cursor grid facts for wide-char key skipping (width-2 CJK/emoji).
/// Default is a no-op so pure input tests stay free of grid fixtures.
#[derive(Clone, Copy, Debug, Default)]
pub struct WideCursorHint {
    /// Cursor sits on a `WIDE_CHAR` base cell — Right / Delete should skip the spacer.
    pub on_wide_base: bool,
    /// Cursor sits on a `WIDE_CHAR_SPACER` — Left / Backspace should treat the
    /// whole wide glyph as one unit (not base-then-before).
    pub on_wide_spacer: bool,
    /// Cell immediately left of the cursor is a `WIDE_CHAR_SPACER` — Left /
    /// Backspace should remove/skip the full wide glyph (not leave a half-cell).
    pub left_is_spacer: bool,
}

/// Decide what this frame's egui events mean for the terminal. Pure: real
/// egui/alacritty types in, an `InputOutcome` out, no I/O. `mods` is the live
/// frame modifier state (distinct from any per-event `Key` modifiers), used to
/// tell a genuine Alt+letter Text event apart from AltGr.
///
/// Wide-char key skipping defaults off; Session calls [`process_input_wide`]
/// with a live [`WideCursorHint`] so one Left/Right/Backspace/Delete crosses
/// or removes a width-2 emoji/CJK glyph instead of half a cell.
pub fn process_input(
    events: &[Event],
    mods: Modifiers,
    mode: TermMode,
    has_selection: bool,
) -> InputOutcome {
    process_input_wide(
        events,
        mods,
        mode,
        has_selection,
        WideCursorHint::default(),
    )
}

/// Like [`process_input`], plus wide-char Left/Right/Backspace/Delete doubling
/// from `wide`.
pub fn process_input_wide(
    events: &[Event],
    mods: Modifiers,
    mode: TermMode,
    has_selection: bool,
    wide: WideCursorHint,
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
                        // Ctrl+0: reset the global terminal zoom. Consumed here so
                        // the shell never sees a stray NUL.
                        (Key::Num0, false) => {
                            out.zoom_reset = true;
                            continue;
                        }
                        _ => {}
                    }
                }
                // Everything else → the pure encoder.
                let seq = encode_key(k, m, mode);
                out.pty_bytes.extend_from_slice(&seq);
                // Width-2 cells (emoji/CJK): double the key so one physical
                // press crosses/removes the whole glyph (base+spacer), not a
                // half-cell white square. Covers move, Shift+←/→ select,
                // Backspace, and Delete. Ctrl/Alt chords stay single (word-nav
                // / other bindings). Empty seq = unmapped, no double.
                let skip_wide = match k {
                    // Right / Delete: on base → skip spacer; on spacer → leave glyph.
                    Key::ArrowRight | Key::Delete => {
                        wide.on_wide_base || wide.on_wide_spacer
                    }
                    // Left / Backspace: after glyph or mid-glyph → full unit.
                    Key::ArrowLeft | Key::Backspace => {
                        wide.left_is_spacer || wide.on_wide_spacer
                    }
                    _ => false,
                };
                if !seq.is_empty() && !ctrl && !m.alt && skip_wide {
                    out.pty_bytes.extend_from_slice(&seq);
                }
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

/// Accumulate a smoothed wheel delta and emit only whole steps. egui delivers a
/// wheel notch as sub-frame fractions; dividing `delta` by `unit` (a line height
/// for scrollback, the zoom notch for Ctrl+Scroll) and truncating both drops
/// gentle scrolls (→0) and over-emits fast ones, so the caller carries the
/// fractional part between frames. Returns `(whole_steps, remainder)`: apply
/// `whole_steps` (sign = direction) and feed `remainder` back as the next
/// frame's `accum`. Pure so the accumulate→trunc glue is unit-testable without a
/// live egui `Context`.
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
/// `delta_lines`: + = wheel up / toward older history, - = wheel down. `(col,
/// row)`: the 1-based viewport cell under the pointer, used only for mouse
/// encoding. Pure: same inputs → same bytes.
///
/// Precedence: (1) if the app is in any mouse-reporting mode, forward wheel as
/// mouse events; (2) else if it's on the alternate screen with alternate-scroll
/// enabled, translate the wheel to arrow keys (so pagers/TUIs scroll); (3) else
/// scroll foreman's scrollback.
pub fn wheel_input(delta_lines: i32, mode: TermMode, col: u16, row: u16) -> WheelAction {
    if delta_lines == 0 {
        return WheelAction::Scrollback(Scroll::Delta(0));
    }
    let up = delta_lines > 0;
    let count = delta_lines.unsigned_abs() as usize;

    // (1) Mouse reporting: emit one wheel event per line. Wheel up = button 64,
    // wheel down = button 65 (xterm's wheel buttons). Wheel has no release.
    if mode.intersects(TermMode::MOUSE_MODE) {
        let button: u16 = if up { 64 } else { 65 };
        let mut bytes = Vec::new();
        for _ in 0..count {
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

    // (2) Alternate screen + alternate-scroll: feed arrow keys. Reuse encode_key
    // so APP_CURSOR (ESC O A vs ESC [ A) is honored exactly as for real arrows.
    if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        let key = if up { Key::ArrowUp } else { Key::ArrowDown };
        let no_mods = Modifiers::default();
        let one = encode_key(key, no_mods, mode);
        let mut bytes = Vec::with_capacity(one.len() * count);
        for _ in 0..count {
            bytes.extend_from_slice(&one);
        }
        return WheelAction::Pty(bytes);
    }

    // (3) Default: scroll foreman's own scrollback.
    WheelAction::Scrollback(Scroll::Delta(delta_lines))
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

    // ---- wide-char arrow skip ------------------------------------------------
    #[test]
    fn right_on_wide_base_emits_two_csi() {
        let wide = WideCursorHint {
            on_wide_base: true,
            on_wide_spacer: false,
            left_is_spacer: false,
        };
        let out = process_input_wide(
            &[key_ev(Key::ArrowRight, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, b"\x1b[C\x1b[C");
    }
    #[test]
    fn left_when_left_is_spacer_emits_two_csi() {
        let wide = WideCursorHint {
            on_wide_base: false,
            on_wide_spacer: false,
            left_is_spacer: true,
        };
        let out = process_input_wide(
            &[key_ev(Key::ArrowLeft, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, b"\x1b[D\x1b[D");
    }
    #[test]
    fn right_without_wide_hint_is_single() {
        let out = process_input(
            &[key_ev(Key::ArrowRight, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, b"\x1b[C");
    }
    #[test]
    fn ctrl_right_on_wide_base_stays_single() {
        // Word-nav must not double.
        let wide = WideCursorHint {
            on_wide_base: true,
            on_wide_spacer: false,
            left_is_spacer: false,
        };
        let out = process_input_wide(
            &[key_ev(Key::ArrowRight, mods(true, false, false))],
            mods(true, false, false),
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, b"\x1b[1;5C");
    }
    #[test]
    fn shift_left_when_left_is_spacer_emits_two_csi() {
        // Shift+← selection extend must skip the spacer the same as plain ←.
        let wide = WideCursorHint {
            on_wide_base: false,
            on_wide_spacer: false,
            left_is_spacer: true,
        };
        let shift = mods(false, false, true);
        let out = process_input_wide(
            &[key_ev(Key::ArrowLeft, shift)],
            shift,
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, b"\x1b[1;2D\x1b[1;2D");
    }
    #[test]
    fn shift_right_on_wide_base_emits_two_csi() {
        let wide = WideCursorHint {
            on_wide_base: true,
            on_wide_spacer: false,
            left_is_spacer: false,
        };
        let shift = mods(false, false, true);
        let out = process_input_wide(
            &[key_ev(Key::ArrowRight, shift)],
            shift,
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, b"\x1b[1;2C\x1b[1;2C");
    }
    #[test]
    fn left_on_wide_spacer_emits_two_csi() {
        // Parked mid-glyph: one ← should leave the whole wide char.
        let wide = WideCursorHint {
            on_wide_base: false,
            on_wide_spacer: true,
            left_is_spacer: false,
        };
        let out = process_input_wide(
            &[key_ev(Key::ArrowLeft, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, b"\x1b[D\x1b[D");
    }
    #[test]
    fn backspace_when_left_is_spacer_emits_two_del() {
        // After a width-2 emoji: one Backspace must remove base+spacer, not
        // leave a white half-cell (spacer orphan).
        let wide = WideCursorHint {
            on_wide_base: false,
            on_wide_spacer: false,
            left_is_spacer: true,
        };
        let out = process_input_wide(
            &[key_ev(Key::Backspace, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, [0x7f, 0x7f]);
    }
    #[test]
    fn backspace_on_wide_spacer_emits_two_del() {
        let wide = WideCursorHint {
            on_wide_base: false,
            on_wide_spacer: true,
            left_is_spacer: false,
        };
        let out = process_input_wide(
            &[key_ev(Key::Backspace, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, [0x7f, 0x7f]);
    }
    #[test]
    fn backspace_without_wide_hint_is_single() {
        let out = process_input(
            &[key_ev(Key::Backspace, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, [0x7f]);
    }
    #[test]
    fn delete_on_wide_base_emits_two() {
        let wide = WideCursorHint {
            on_wide_base: true,
            on_wide_spacer: false,
            left_is_spacer: false,
        };
        let out = process_input_wide(
            &[key_ev(Key::Delete, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
            wide,
        );
        // encode_key Delete → CSI 3~
        assert_eq!(out.pty_bytes, b"\x1b[3~\x1b[3~");
    }
    #[test]
    fn delete_on_wide_spacer_emits_two() {
        let wide = WideCursorHint {
            on_wide_base: false,
            on_wide_spacer: true,
            left_is_spacer: false,
        };
        let out = process_input_wide(
            &[key_ev(Key::Delete, none())],
            Modifiers::default(),
            TermMode::empty(),
            false,
            wide,
        );
        assert_eq!(out.pty_bytes, b"\x1b[3~\x1b[3~");
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

    // ---- wheel_input ---------------------------------------------------------
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

    #[test]
    fn wheel_primary_screen_scrolls_local_scrollback() {
        assert_eq!(scrollback_delta(wheel_input(3, TermMode::empty(), 1, 1)), 3);
        assert_eq!(
            scrollback_delta(wheel_input(-2, TermMode::empty(), 1, 1)),
            -2
        );
    }
    #[test]
    fn wheel_alt_scroll_emits_arrow_keys() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        // Up 3 → ArrowUp ×3 (plain CSI, since APP_CURSOR is off).
        assert_eq!(pty(wheel_input(3, mode, 1, 1)), b"\x1b[A\x1b[A\x1b[A");
        // Down 3 → ArrowDown ×3.
        assert_eq!(pty(wheel_input(-3, mode, 1, 1)), b"\x1b[B\x1b[B\x1b[B");
    }
    #[test]
    fn wheel_alt_scroll_honors_app_cursor_mode() {
        let mode = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL | TermMode::APP_CURSOR;
        // SS3 form (ESC O A / ESC O B) when the app set DECCKM.
        assert_eq!(pty(wheel_input(1, mode, 1, 1)), b"\x1bOA");
        assert_eq!(pty(wheel_input(-1, mode, 1, 1)), b"\x1bOB");
    }
    #[test]
    fn wheel_sgr_mouse_one_line() {
        let mode = TermMode::MOUSE_MODE | TermMode::SGR_MOUSE;
        assert_eq!(pty(wheel_input(1, mode, 5, 10)), b"\x1b[<64;5;10M");
        assert_eq!(pty(wheel_input(-1, mode, 5, 10)), b"\x1b[<65;5;10M");
    }
    #[test]
    fn wheel_sgr_mouse_repeats_per_line() {
        let mode = TermMode::MOUSE_MODE | TermMode::SGR_MOUSE;
        assert_eq!(
            pty(wheel_input(2, mode, 5, 10)),
            b"\x1b[<64;5;10M\x1b[<64;5;10M"
        );
    }
    #[test]
    fn wheel_x10_mouse_one_line() {
        let mode = TermMode::MOUSE_MODE; // no SGR → legacy X10 encoding
        // ESC [ M then (32+button), (32+col), (32+row).
        let expected = vec![0x1b, b'[', b'M', 32 + 64, 32 + 5, 32 + 10];
        assert_eq!(pty(wheel_input(1, mode, 5, 10)), expected);
    }
    #[test]
    fn wheel_mouse_mode_beats_alt_scroll() {
        // Both mouse-reporting AND alt-scroll flags set → mouse wins.
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
