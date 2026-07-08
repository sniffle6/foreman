//! In-app keybindings editor (Phase 3 of the keyboard-control epic).
//!
//! A **desktop-level modal overlay** — settings are global, so this mirrors the
//! `dirpicker.rs` overlay pattern (dim the desktop, centered panel, keyboard
//! driven, all input captured) rather than being a `Content` window.
//!
//! It edits the live [`Keymap`] in place and tells the caller (the desktop
//! `WindowManager`) when to persist via [`Keymap::save`]. The overlay never
//! touches disk itself; the wm owns the keymap and the persistence trigger.

use crate::keymap::{Chord, Command, Group, Keymap};
use crate::theme::*;
use eframe::egui;

// Palette — kept in step with wm.rs so the overlay reads as part of the app.
/// What the editor wants the caller to do after a frame.
pub enum Outcome {
    /// Stay open, nothing to persist.
    Pending,
    /// The keymap was mutated this frame — caller should `keymap.save()`.
    Changed,
    /// Close the overlay.
    Close,
}

/// One selectable row in the editor. The leader row is special; command rows map
/// to a `Command`.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Leader,
    Command(Command),
}

/// Capture sub-state of the editor.
enum Mode {
    /// Browsing rows.
    Idle,
    /// Waiting for the user to press the new chord for `row`. The first key event
    /// (ignoring lone modifier presses) is captured.
    Capturing { row: Row },
    /// A captured chord for `row` conflicts with `existing`; awaiting Enter
    /// (replace) or Esc (cancel).
    Conflict {
        row: Row,
        chord: Chord,
        existing: Command,
    },
}

/// The keybindings editor modal. Holds only UI state; the keymap lives in the wm.
pub struct SettingsView {
    /// Flat, ordered list of rows (leader first, then commands grouped).
    rows: Vec<Row>,
    selected: usize,
    mode: Mode,
    /// Transient message shown in the footer (errors, hints, leader collisions).
    message: Option<String>,
}

impl SettingsView {
    pub fn new() -> Self {
        let mut rows = vec![Row::Leader];
        for &g in Group::ALL {
            for &cmd in Command::ALL {
                if cmd.group() == g {
                    rows.push(Row::Command(cmd));
                }
            }
        }
        Self {
            rows,
            selected: 0,
            mode: Mode::Idle,
            message: None,
        }
    }

    /// Surface a persistence failure in the footer. Called by the caller (wm)
    /// when `Keymap::save` fails after a `Changed` outcome, so the editor stays
    /// open and the user sees why their change did not stick on disk.
    pub fn set_save_error(&mut self, msg: String) {
        self.message = Some(format!("Could not save: {msg}"));
    }

    fn move_sel(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        let mut i = self.selected as isize + delta;
        i = i.clamp(0, n - 1);
        self.selected = i as usize;
    }

    fn selected_row(&self) -> Row {
        self.rows[self.selected]
    }

    /// Begin capturing a new chord for the selected row.
    fn start_capture(&mut self) {
        self.message = None;
        self.mode = Mode::Capturing {
            row: self.selected_row(),
        };
    }

    /// Apply a captured chord to a row, after conflict/leader checks.
    /// Returns true if the keymap changed (caller persists).
    fn apply(&mut self, km: &mut Keymap, row: Row, chord: Chord) -> bool {
        match row {
            Row::Leader => {
                // The leader cannot also be a command chord.
                if km.resolve(chord).is_some() {
                    let cmd = km.resolve(chord).unwrap();
                    self.message = Some(format!(
                        "{} is bound to \"{}\" — pick a leader that is not a command chord.",
                        chord.pretty(),
                        cmd.label()
                    ));
                    self.mode = Mode::Idle;
                    return false;
                }
                km.set_leader(chord);
                self.mode = Mode::Idle;
                self.message = Some(format!("Leader set to {}.", chord.pretty()));
                true
            }
            Row::Command(cmd) => {
                // The leader cannot double as a command chord.
                if chord == km.leader {
                    self.message = Some(format!(
                        "{} is the leader key and cannot be a command.",
                        chord.pretty()
                    ));
                    self.mode = Mode::Idle;
                    return false;
                }
                match km.resolve(chord) {
                    Some(existing) if existing != cmd => {
                        // Conflict — ask before overwriting.
                        self.mode = Mode::Conflict {
                            row,
                            chord,
                            existing,
                        };
                        false
                    }
                    _ => {
                        km.rebind(cmd, chord);
                        self.mode = Mode::Idle;
                        self.message = Some(format!("{} → {}", chord.pretty(), cmd.label()));
                        true
                    }
                }
            }
        }
    }

    /// Render one frame and report what the caller should do. `km` is the live
    /// keymap, mutated in place; a `Changed` outcome means the caller persists.
    pub fn show(&mut self, ui: &mut egui::Ui, km: &mut Keymap) -> Outcome {
        let mut changed = false;
        let mut close = false;

        // --- input ---------------------------------------------------------
        // Capture mode reads the raw next chord; otherwise normal navigation.
        match self.mode {
            Mode::Capturing { row } => {
                if let Some(captured) = capture_chord(ui) {
                    // Esc cancels capture without binding.
                    if captured.key == egui::Key::Escape
                        && !captured.ctrl
                        && !captured.shift
                        && !captured.alt
                    {
                        self.mode = Mode::Idle;
                    } else if self.apply(km, row, captured) {
                        changed = true;
                    }
                }
            }
            Mode::Conflict { row, chord, .. } => {
                ui.input(|i| {
                    if i.key_pressed(egui::Key::Enter) {
                        // Confirmed replace.
                        km.rebind(
                            match row {
                                Row::Command(c) => c,
                                Row::Leader => unreachable!("leader never enters conflict"),
                            },
                            chord,
                        );
                        changed = true;
                        self.mode = Mode::Idle;
                        self.message = Some(format!("{} reassigned.", chord.pretty()));
                    } else if i.key_pressed(egui::Key::Escape) {
                        self.mode = Mode::Idle;
                    }
                });
            }
            Mode::Idle => {
                ui.input(|i| {
                    if i.key_pressed(egui::Key::Escape) {
                        close = true;
                    }
                    if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::J) {
                        self.move_sel(1);
                    }
                    if i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::K) {
                        self.move_sel(-1);
                    }
                });
                // Enter starts a rebind on the selected row (separate borrow:
                // start_capture mutates self, can't be inside the input closure).
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter {
                    self.start_capture();
                }
            }
        }

        // --- dim + panel ---------------------------------------------------
        let screen = ui.ctx().content_rect();
        ui.painter()
            .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(170));

        egui::Window::new("keybindings")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::NONE
                    .fill(WIN_BG)
                    .stroke(egui::Stroke::new(1.0, BORDER_FOCUS))
                    .inner_margin(egui::Margin::same(16))
                    .corner_radius(egui::CornerRadius::same(8)),
            )
            .show(ui.ctx(), |ui| {
                ui.set_min_width(540.0);
                ui.set_max_width(540.0);
                ui.visuals_mut().override_text_color = Some(TEXT);

                ui.label(
                    egui::RichText::new("Keyboard bindings")
                        .color(BORDER_FOCUS)
                        .size(16.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(
                        "j/k or ↑/↓ select · Enter rebind · Esc close / cancel capture",
                    )
                    .color(DIM)
                    .size(11.5),
                );
                ui.add_space(8.0);

                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.render_rows(ui, km, &mut changed);
                    });

                ui.add_space(6.0);
                ui.separator();

                // Footer: message line + actions.
                if let Some(msg) = &self.message {
                    ui.label(egui::RichText::new(msg).color(DIM).size(11.5));
                }
                ui.horizontal(|ui| {
                    if ui.button("Reset all to defaults").clicked() {
                        km.reset_all();
                        self.mode = Mode::Idle;
                        self.message = Some("All bindings reset to defaults.".into());
                        changed = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });

        if close {
            Outcome::Close
        } else if changed {
            Outcome::Changed
        } else {
            Outcome::Pending
        }
    }

    /// Render the leader row and grouped command rows.
    fn render_rows(&mut self, ui: &mut egui::Ui, km: &mut Keymap, changed: &mut bool) {
        // Track the running flat index to keep selection in sync.
        let mut idx = 0usize;
        let mut last_group: Option<Group> = None;

        // We collect deferred actions to avoid borrowing self mutably while
        // iterating self.rows.
        enum RowAct {
            Select(usize),
            Capture(usize),
            ResetOne(Command),
            ConfirmConflict,
            CancelCapture,
        }
        let mut acts: Vec<RowAct> = vec![];

        let rows = self.rows.clone();
        for row in rows {
            // Group header before the first command of each group.
            if let Row::Command(cmd) = row {
                let g = cmd.group();
                if last_group != Some(g) {
                    last_group = Some(g);
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(g.title())
                            .color(BORDER_FOCUS)
                            .size(12.5)
                            .strong(),
                    );
                }
            }

            let is_sel = idx == self.selected;
            let (label, chord_str) = match row {
                Row::Leader => ("Leader".to_string(), km.leader.pretty()),
                Row::Command(cmd) => (
                    cmd.label().to_string(),
                    km.chord_for(cmd)
                        .map(|c| c.pretty())
                        .unwrap_or_else(|| "—".to_string()),
                ),
            };

            // Row background + selection highlight via an allocated strip.
            let row_h = 24.0;
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), row_h),
                egui::Sense::click(),
            );
            if resp.clicked() {
                acts.push(RowAct::Select(idx));
            }
            if is_sel {
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(4), SEL_BG);
            }

            // Per-row capture / conflict inline state.
            let capturing = matches!(self.mode, Mode::Capturing { row: r } if r == row);
            let conflict = match &self.mode {
                Mode::Conflict {
                    row: r, existing, ..
                } if *r == row => Some(*existing),
                _ => None,
            };

            // Label (left).
            ui.painter().text(
                egui::pos2(rect.min.x + 6.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &label,
                egui::FontId::proportional(12.5),
                if is_sel { TEXT } else { DIM },
            );

            // Chord / status (right region) + buttons drawn via a child UI.
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            child.add_space(4.0);
            // Rightmost: reset-one (✕→default) then Rebind, then the chord text.
            if let Row::Command(cmd) = row {
                if child.small_button("reset").clicked() {
                    acts.push(RowAct::ResetOne(cmd));
                }
            } else {
                child.add_space(44.0); // align leader row with command rows
            }
            if capturing {
                if child.small_button("cancel").clicked() {
                    acts.push(RowAct::CancelCapture);
                }
            } else if conflict.is_some() {
                if child.small_button("replace").clicked() {
                    acts.push(RowAct::ConfirmConflict);
                }
            } else if child.small_button("rebind").clicked() {
                acts.push(RowAct::Capture(idx));
            }

            // Status text (chord, or capture/conflict prompt).
            let status = if capturing {
                egui::RichText::new("press keys…")
                    .color(BORDER_FOCUS)
                    .strong()
            } else if let Some(existing) = conflict {
                egui::RichText::new(format!(
                    "conflicts with \"{}\" — replace?",
                    existing.label()
                ))
                .color(DANGER)
            } else {
                egui::RichText::new(chord_str)
                    .color(if is_sel { BORDER_FOCUS } else { TEXT })
                    .monospace()
            };
            child.label(status);

            idx += 1;
        }

        // Apply deferred actions.
        for a in acts {
            match a {
                RowAct::Select(i) => self.selected = i,
                RowAct::Capture(i) => {
                    self.selected = i;
                    self.start_capture();
                }
                RowAct::CancelCapture => self.mode = Mode::Idle,
                RowAct::ResetOne(cmd) => {
                    km.reset_one(cmd);
                    self.message = Some(format!("Reset \"{}\" to default.", cmd.label()));
                    *changed = true;
                }
                RowAct::ConfirmConflict => {
                    if let Mode::Conflict { row, chord, .. } = self.mode {
                        if let Row::Command(c) = row {
                            km.rebind(c, chord);
                            *changed = true;
                            self.message = Some(format!("{} reassigned.", chord.pretty()));
                        }
                    }
                    self.mode = Mode::Idle;
                }
            }
        }
    }
}

/// Capture the next chord from this frame's input, ignoring presses that are a
/// lone modifier key (so holding Ctrl before the real key does not register).
/// Returns `None` if no usable key was pressed this frame. Ctrl+C / Ctrl+X may
/// arrive as `Copy`/`Cut` events — translate those back to chords so they can be
/// bound like any other.
fn capture_chord(ui: &egui::Ui) -> Option<Chord> {
    ui.input(|i| {
        for e in &i.events {
            match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if is_modifier_key(*key) {
                        continue;
                    }
                    return Some(Chord::from_event(*key, *modifiers));
                }
                egui::Event::Copy => return Some(Chord::new(egui::Key::C, true, false, false)),
                egui::Event::Cut => return Some(Chord::new(egui::Key::X, true, false, false)),
                _ => {}
            }
        }
        None
    })
}

/// egui delivers modifier keys as ordinary `Key`s on some platforms; never let
/// one be captured as a chord on its own.
fn is_modifier_key(_key: egui::Key) -> bool {
    // egui 0.34's `egui::Key` has no Ctrl/Shift/Alt/Meta variants — modifiers
    // arrive only via `Modifiers`, never as `Key`. Kept as a guard hook in case
    // that changes; currently nothing is a lone modifier key.
    false
}

#[cfg(test)]
mod tests {
    //! `SettingsView::apply` is the pure rebind core (no egui). These tests drive
    //! it directly with real `Keymap`/`Chord`/`Command` values and assert on the
    //! observable state: the keymap table, the returned "changed" bool, and the
    //! resulting `Mode`. Conflict *resolution* (Enter → replace) lives in `show`/
    //! `render_rows`, not `apply`, so it is intentionally not exercised here.
    use super::*;
    use eframe::egui::Key as K;

    fn plain(k: K) -> Chord {
        Chord::new(k, false, false, false)
    }

    #[test]
    fn clean_rebind_returns_changed_and_updates_keymap() {
        let mut view = SettingsView::new();
        let mut km = Keymap::default();
        // `Y` is unbound by default, so this rebind has no conflict.
        let chord = plain(K::Y);
        let changed = view.apply(&mut km, Row::Command(Command::CloseTerm), chord);
        assert!(changed, "a clean rebind reports the keymap changed");
        assert_eq!(km.resolve(chord), Some(Command::CloseTerm));
        // The command's old default chord (`x`) is released by `rebind`.
        assert_eq!(km.resolve(plain(K::X)), None);
        assert!(matches!(view.mode, Mode::Idle));
    }

    #[test]
    fn rebinding_command_to_current_chord_is_idempotent_success() {
        let mut view = SettingsView::new();
        let mut km = Keymap::default();
        // `x` already *is* CloseTerm — the `_` arm rebinds it to itself.
        let changed = view.apply(&mut km, Row::Command(Command::CloseTerm), plain(K::X));
        assert!(changed);
        assert_eq!(km.resolve(plain(K::X)), Some(Command::CloseTerm));
        assert!(matches!(view.mode, Mode::Idle));
    }

    #[test]
    fn rebinding_command_to_leader_chord_is_rejected() {
        let mut view = SettingsView::new();
        let mut km = Keymap::default();
        let leader = km.leader; // Ctrl+B by default
        let changed = view.apply(&mut km, Row::Command(Command::CloseTerm), leader);
        assert!(!changed, "the leader chord cannot double as a command");
        // Nothing moved: CloseTerm still on `x`, leader untouched.
        assert_eq!(km.resolve(plain(K::X)), Some(Command::CloseTerm));
        assert_eq!(km.leader, leader);
        assert!(matches!(view.mode, Mode::Idle));
        assert!(
            view.message.as_deref().unwrap_or("").contains("leader"),
            "the footer explains the leader collision"
        );
    }

    #[test]
    fn leader_row_rejects_a_chord_already_bound_to_a_command() {
        let mut view = SettingsView::new();
        let mut km = Keymap::default();
        let original_leader = km.leader;
        // `c` is NewTerm — the leader may not also be a command chord.
        let changed = view.apply(&mut km, Row::Leader, plain(K::C));
        assert!(!changed);
        assert_eq!(km.leader, original_leader, "leader unchanged on rejection");
        // The command that owned the chord is untouched.
        assert_eq!(km.resolve(plain(K::C)), Some(Command::NewTerm));
        assert!(matches!(view.mode, Mode::Idle));
    }

    #[test]
    fn leader_row_accepts_an_unbound_chord() {
        let mut view = SettingsView::new();
        let mut km = Keymap::default();
        // `Y` is unbound, so it is a valid leader.
        let chord = plain(K::Y);
        let changed = view.apply(&mut km, Row::Leader, chord);
        assert!(changed);
        assert_eq!(km.leader, chord);
        assert!(matches!(view.mode, Mode::Idle));
    }

    #[test]
    fn conflicting_command_chord_enters_conflict_without_rebinding() {
        let mut view = SettingsView::new();
        let mut km = Keymap::default();
        // `c` is NewTerm; binding CloseTerm to it is a conflict.
        let chord = plain(K::C);
        let changed = view.apply(&mut km, Row::Command(Command::CloseTerm), chord);
        assert!(
            !changed,
            "a conflict defers the rebind, so nothing changed yet"
        );
        // Nothing was rebound: `c` still NewTerm, CloseTerm still on `x`.
        assert_eq!(km.resolve(chord), Some(Command::NewTerm));
        assert_eq!(km.resolve(plain(K::X)), Some(Command::CloseTerm));
        // The mode captures the pending conflict for the UI to resolve.
        match &view.mode {
            Mode::Conflict {
                row,
                chord: c,
                existing,
            } => {
                // `Row` doesn't derive Debug, so compare with `==` rather than
                // `assert_eq!` (which would require a Debug impl just for tests).
                assert!(*row == Row::Command(Command::CloseTerm));
                assert_eq!(*c, chord);
                assert_eq!(*existing, Command::NewTerm);
            }
            _ => panic!("expected Mode::Conflict, got a different mode"),
        }
    }
}
