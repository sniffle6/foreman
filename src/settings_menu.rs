//! Settings menu (phase 1): pure model half. Categories, declarative row
//! descriptors, and a pure `adjust` over &mut Settings — all unit-tested
//! without a GUI. The egui view lives in the same file below (Task 3).

use crate::config::{DefaultShell, Settings};

/// Left-rail categories, in display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // constructed/matched by the view (Task 3)
pub enum Pane {
    Terminal,
    Bell,
    WindowManager,
    Keybindings,
    Agents,
    Startup,
}

#[allow(dead_code)] // used by the view (Task 3)
impl Pane {
    pub const ALL: [Pane; 6] = [
        Pane::Terminal,
        Pane::Bell,
        Pane::WindowManager,
        Pane::Keybindings,
        Pane::Agents,
        Pane::Startup,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Pane::Terminal => "Terminal",
            Pane::Bell => "Bell & Alerts",
            Pane::WindowManager => "Window Manager",
            Pane::Keybindings => "Keybindings",
            Pane::Agents => "Agents",
            Pane::Startup => "Startup & Updates",
        }
    }
}

/// One editable setting. Task 3+ match on these exact variants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // constructed by rows()/adjust(); consumed by the view (Task 3)
pub enum Field {
    DefaultShellF,
    ScrollbackLines,
    ScrollSpeed,
    ZoomStep,
    CopyOnSelect,
    PasteWarn,
    BellOn,
    BellPeriod,
    ToastSecs,
    NewWindowsFloat,
    FocusFollowsMouse,
    DimUnfocused,
    InstallSkills,
    CrewStale,
    SendSettle,
    RestoreWorkspace,
    DefaultProjectDir,
    UpdateCheck,
    OpenKeybindings,
    CheckUpdatesNow,
    OpenConfigFolder,
}

/// How a row's value is edited. Drives the view's widget choice (Task 3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // read by the view (Task 3)
pub enum Kind {
    Toggle,
    Stepper,
    Choice,
    Text,
    Action,
}

/// A declarative row: what field it edits, its label/description, and how
/// it should be presented. `rows()` returns static slices of these per pane.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // fields read by the view (Task 3)
pub struct RowSpec {
    pub field: Field,
    pub label: &'static str,
    pub desc: &'static str,
    pub kind: Kind,
}

/// The static row list for a pane, in display order.
#[allow(dead_code)] // called by the view (Task 3)
pub fn rows(pane: Pane) -> &'static [RowSpec] {
    match pane {
        Pane::Terminal => &[
            RowSpec {
                field: Field::DefaultShellF,
                label: "Default shell",
                desc: "What a new pane runs; per-pane chips still override",
                kind: Kind::Choice,
            },
            RowSpec {
                field: Field::ScrollbackLines,
                label: "Scrollback lines",
                desc: "History kept per pane (new terminals)",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::ScrollSpeed,
                label: "Scroll speed",
                desc: "Lines per wheel notch",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::ZoomStep,
                label: "Zoom step",
                desc: "Font points per Ctrl+Scroll notch",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::CopyOnSelect,
                label: "Copy on select",
                desc: "Selection lands on the clipboard immediately",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::PasteWarn,
                label: "Warn on multi-line paste",
                desc: "Confirm before pasting text containing newlines",
                kind: Kind::Toggle,
            },
        ],
        Pane::Bell => &[
            RowSpec {
                field: Field::BellOn,
                label: "Bell attention",
                desc: "Master switch — pulse (and any future sound) honors it",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::BellPeriod,
                label: "Pulse speed",
                desc: "One full breathe of the amber pulse",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::ToastSecs,
                label: "Toast duration",
                desc: "How long notifications linger top-right",
                kind: Kind::Stepper,
            },
        ],
        Pane::WindowManager => &[
            RowSpec {
                field: Field::NewWindowsFloat,
                label: "New terminals open floating",
                desc: "Off = new terminals join the tiling tree",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::FocusFollowsMouse,
                label: "Focus follows mouse",
                desc: "Hovering a pane focuses it without a click",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::DimUnfocused,
                label: "Dim unfocused panes",
                desc: "Slight darkening on everything but the focused terminal",
                kind: Kind::Toggle,
            },
        ],
        Pane::Keybindings => &[RowSpec {
            field: Field::OpenKeybindings,
            label: "Edit keybindings…",
            desc: "Leader, chords, conflicts — the full editor",
            kind: Kind::Action,
        }],
        Pane::Agents => &[
            RowSpec {
                field: Field::InstallSkills,
                label: "Install agent skills on launch",
                desc: "Writes foreman-dispatch / foreman-chat into Claude & Codex skill dirs",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::CrewStale,
                label: "Crew stale after",
                desc: "A member unheard this long shows its age in amber",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::SendSettle,
                label: "Send settle default",
                desc: "Quiescence wait for foreman send when the caller doesn't pass one",
                kind: Kind::Stepper,
            },
        ],
        Pane::Startup => &[
            RowSpec {
                field: Field::RestoreWorkspace,
                label: "Restore workspace on launch",
                desc: "Reopen last session's projects and layout",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::DefaultProjectDir,
                label: "Default project directory",
                desc: "Where the picker starts browsing (blank = home)",
                kind: Kind::Text,
            },
            RowSpec {
                field: Field::UpdateCheck,
                label: "Check for updates on launch",
                desc: "GitHub releases, background check",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::CheckUpdatesNow,
                label: "Check for updates now",
                desc: "",
                kind: Kind::Action,
            },
            RowSpec {
                field: Field::OpenConfigFolder,
                label: "Open settings folder",
                desc: "%APPDATA%\\foreman",
                kind: Kind::Action,
            },
        ],
    }
}

/// Direction a row's value should move. `Toggle` is meaningless for steppers
/// (ignored) and vice versa (flip ignores it) — each `adjust` arm picks what
/// applies to its field's `Kind`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // constructed by the view (Task 3)
pub enum Adjust {
    Toggle,
    Inc,
    Dec,
}

/// Apply one nav action to one field. Returns whether the value actually
/// changed (steppers at a clamp bound report `false`; `Action` fields are
/// handled by the view and always report `false` here).
#[allow(dead_code)] // called by the view (Task 3)
pub fn adjust(field: Field, a: Adjust, s: &mut Settings) -> bool {
    fn step_f32(v: &mut f32, a: Adjust, step: f32, min: f32, max: f32) -> bool {
        let next = match a {
            Adjust::Inc => (*v + step).min(max),
            Adjust::Dec => (*v - step).max(min),
            Adjust::Toggle => *v,
        };
        let changed = (next - *v).abs() > f32::EPSILON;
        *v = next;
        changed
    }
    fn step_u32(v: &mut u32, a: Adjust, step: u32, min: u32, max: u32) -> bool {
        let next = match a {
            Adjust::Inc => v.saturating_add(step).min(max),
            Adjust::Dec => v.saturating_sub(step).max(min),
            Adjust::Toggle => *v,
        };
        let changed = next != *v;
        *v = next;
        changed
    }
    fn step_u64(v: &mut u64, a: Adjust, step: u64, min: u64, max: u64) -> bool {
        let next = match a {
            Adjust::Inc => v.saturating_add(step).min(max),
            Adjust::Dec => v.saturating_sub(step).max(min),
            Adjust::Toggle => *v,
        };
        let changed = next != *v;
        *v = next;
        changed
    }
    fn flip(v: &mut bool) -> bool {
        *v = !*v;
        true
    }
    match field {
        Field::DefaultShellF => {
            let order = [
                DefaultShell::PowerShell,
                DefaultShell::Cmd,
                DefaultShell::Sh,
            ];
            let i = order
                .iter()
                .position(|x| *x == s.default_shell)
                .unwrap_or(0);
            let n = order.len();
            let next = match a {
                Adjust::Inc | Adjust::Toggle => order[(i + 1) % n],
                Adjust::Dec => order[(i + n - 1) % n],
            };
            let changed = next != s.default_shell;
            s.default_shell = next;
            changed
        }
        Field::ScrollbackLines => step_u32(&mut s.scrollback_lines, a, 1000, 100, 1_000_000),
        Field::ScrollSpeed => step_f32(&mut s.scroll_speed, a, 1.0, 1.0, 30.0),
        Field::ZoomStep => step_f32(&mut s.zoom_step, a, 0.25, 0.25, 5.0),
        Field::CopyOnSelect => flip(&mut s.copy_on_select),
        Field::PasteWarn => flip(&mut s.paste_warn_multiline),
        Field::BellOn => flip(&mut s.bell),
        Field::BellPeriod => step_f32(&mut s.bell_period, a, 0.1, 0.4, 5.0),
        Field::ToastSecs => step_f32(&mut s.toast_secs, a, 1.0, 1.0, 30.0),
        Field::NewWindowsFloat => flip(&mut s.new_windows_float),
        Field::FocusFollowsMouse => flip(&mut s.focus_follows_mouse),
        Field::DimUnfocused => flip(&mut s.dim_unfocused),
        Field::InstallSkills => flip(&mut s.install_skills),
        Field::CrewStale => step_u32(&mut s.crew_stale_secs, a, 30, 30, 3600),
        Field::SendSettle => step_u64(&mut s.send_settle_ms, a, 20, 0, 2000),
        Field::RestoreWorkspace => flip(&mut s.restore_workspace),
        Field::DefaultProjectDir => false,
        Field::UpdateCheck => flip(&mut s.update_check),
        Field::OpenKeybindings | Field::CheckUpdatesNow | Field::OpenConfigFolder => false,
    }
}

/// Render a field's current value as the view's row-trailing text.
#[allow(dead_code)] // called by the view (Task 3)
pub fn display(field: Field, s: &Settings) -> String {
    match field {
        Field::DefaultShellF => s.default_shell.label().to_string(),
        Field::ScrollbackLines => format!("{}", s.scrollback_lines),
        Field::ScrollSpeed => format!("{}", s.scroll_speed),
        Field::ZoomStep => format!("{}", s.zoom_step),
        Field::CopyOnSelect => s.copy_on_select.to_string(),
        Field::PasteWarn => s.paste_warn_multiline.to_string(),
        Field::BellOn => s.bell.to_string(),
        Field::BellPeriod => format!("{} s", s.bell_period),
        Field::ToastSecs => format!("{} s", s.toast_secs),
        Field::NewWindowsFloat => s.new_windows_float.to_string(),
        Field::FocusFollowsMouse => s.focus_follows_mouse.to_string(),
        Field::DimUnfocused => s.dim_unfocused.to_string(),
        Field::InstallSkills => s.install_skills.to_string(),
        Field::CrewStale => {
            if s.crew_stale_secs % 60 == 0 {
                format!("{} min", s.crew_stale_secs / 60)
            } else {
                format!("{} s", s.crew_stale_secs)
            }
        }
        Field::SendSettle => format!("{} ms", s.send_settle_ms),
        Field::RestoreWorkspace => s.restore_workspace.to_string(),
        Field::DefaultProjectDir => s.default_project_dir.clone(),
        Field::UpdateCheck => s.update_check.to_string(),
        Field::OpenKeybindings | Field::CheckUpdatesNow | Field::OpenConfigFolder => String::new(),
    }
}

/// Navigation/focus state for the settings menu. Pure — no egui here; the
/// view (Task 3) drives it from key events and reads it back to render.
#[derive(Clone, Debug)]
#[allow(dead_code)] // driven/read by the view (Task 3)
pub struct SettingsMenu {
    pub pane: Pane,
    pub row: usize,
    pub in_rail: bool,
}

#[allow(dead_code)] // driven by the view (Task 3)
impl SettingsMenu {
    pub fn new() -> Self {
        Self {
            pane: Pane::Terminal,
            row: 0,
            in_rail: true,
        }
    }

    /// Move up one row in the current pane; clamps at 0 (no wrap).
    pub fn nav_up(&mut self) {
        self.row = self.row.saturating_sub(1);
    }

    /// Move down one row in the current pane; clamps at the last row.
    pub fn nav_down(&mut self) {
        let last = rows(self.pane).len().saturating_sub(1);
        self.row = (self.row + 1).min(last);
    }

    /// Flip focus between the pane rail and the row list.
    pub fn nav_tab(&mut self) {
        self.in_rail = !self.in_rail;
    }

    /// Switch to a different pane, resetting the row cursor.
    pub fn select_pane(&mut self, p: Pane) {
        self.pane = p;
        self.row = 0;
    }
}

impl Default for SettingsMenu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DefaultShell, Settings};

    #[test]
    fn every_pane_has_rows_and_labels() {
        for p in Pane::ALL {
            assert!(!p.label().is_empty());
            assert!(!rows(p).is_empty(), "{:?} has no rows", p);
        }
    }

    #[test]
    fn toggle_flips_and_reports_change() {
        let mut s = Settings::default();
        assert!(adjust(Field::CopyOnSelect, Adjust::Toggle, &mut s));
        assert!(s.copy_on_select);
        assert!(adjust(Field::CopyOnSelect, Adjust::Toggle, &mut s));
        assert!(!s.copy_on_select);
    }

    #[test]
    fn stepper_clamps_at_bounds_and_reports_no_change() {
        let mut s = Settings::default();
        s.send_settle_ms = 2000;
        assert!(
            !adjust(Field::SendSettle, Adjust::Inc, &mut s),
            "inc at max is a no-op"
        );
        assert_eq!(s.send_settle_ms, 2000);
        s.scroll_speed = 1.0;
        assert!(!adjust(Field::ScrollSpeed, Adjust::Dec, &mut s));
        assert_eq!(s.scroll_speed, 1.0);
    }

    #[test]
    fn shell_choice_cycles_through_all_variants() {
        let mut s = Settings::default();
        adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
        assert_eq!(s.default_shell, DefaultShell::Cmd);
        adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
        assert_eq!(s.default_shell, DefaultShell::Sh);
        adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
        assert_eq!(s.default_shell, DefaultShell::PowerShell, "wraps");
    }

    #[test]
    fn nav_stays_in_bounds_and_tab_switches_focus() {
        let mut m = SettingsMenu::new();
        assert!(m.in_rail);
        m.nav_tab();
        assert!(!m.in_rail);
        m.nav_up(); // row 0 → stays 0 (no wrap; matches keymap editor feel)
        assert_eq!(m.row, 0);
        let last = rows(m.pane).len() - 1;
        for _ in 0..rows(m.pane).len() + 5 {
            m.nav_down();
        }
        assert_eq!(m.row, last, "clamps at last row");
    }

    #[test]
    fn display_formats_units() {
        let s = Settings::default();
        assert_eq!(display(Field::SendSettle, &s), "120 ms");
        assert_eq!(display(Field::ScrollbackLines, &s), "10000");
        assert_eq!(display(Field::BellPeriod, &s), "1.2 s");
        assert_eq!(display(Field::DefaultShellF, &s), "PowerShell");
    }

    #[test]
    fn crew_stale_displays_minutes_when_divisible_by_60() {
        let mut s = Settings::default();
        s.crew_stale_secs = 300;
        assert_eq!(display(Field::CrewStale, &s), "5 min");
        s.crew_stale_secs = 90;
        assert_eq!(display(Field::CrewStale, &s), "90 s");
    }
}
