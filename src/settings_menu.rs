//! Settings menu (phase 1): pure model half. Categories, declarative row
//! descriptors, and a pure `adjust` over &mut Settings — all unit-tested
//! without a GUI. The egui view lives in the same file below.

use crate::config::{DefaultShell, NamingProvider, Settings};
use crate::keymap::Keymap;
use crate::settings::{Outcome as EditorOutcome, SettingsView};
use eframe::egui;

/// Left-rail categories, in display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Appearance,
    Terminal,
    Bell,
    WindowManager,
    Keybindings,
    Agents,
    Remotes,
    Startup,
}

impl Pane {
    pub const ALL: [Pane; 8] = [
        Pane::Appearance,
        Pane::Terminal,
        Pane::Bell,
        Pane::WindowManager,
        Pane::Keybindings,
        Pane::Agents,
        Pane::Remotes,
        Pane::Startup,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Pane::Appearance => "Appearance",
            Pane::Terminal => "Terminal",
            Pane::Bell => "Bell & Alerts",
            Pane::WindowManager => "Window Manager",
            Pane::Keybindings => "Keybindings",
            Pane::Agents => "Agents",
            Pane::Remotes => "Remotes",
            Pane::Startup => "Startup & Updates",
        }
    }
}

/// One editable setting. The view matches on these exact variants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    NewProjectLayout,
    InstallSkills,
    DispatchWorktrees,
    RemoteListen,
    AutoNameAgentSessions,
    AgentStateBadges,
    TitleProvider,
    TitleModel,
    CrewStale,
    SendSettle,
    RestoreWorkspace,
    DefaultProjectDir,
    UpdateCheck,
    CheckUpdatesNow,
    OpenConfigFolder,
}

/// How a row's value is edited. Drives the view's widget choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
pub struct RowSpec {
    pub field: Field,
    pub label: &'static str,
    pub desc: &'static str,
    pub kind: Kind,
}

/// The static row list for a pane, in display order.
pub fn rows(pane: Pane) -> &'static [RowSpec] {
    match pane {
        // The pane itself *is* the editor (draw_pane special-cases it before
        // touching rows()); handle_keys also delegates to it before rows()
        // could be indexed, so this is never read.
        Pane::Appearance => &[],
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
            RowSpec {
                field: Field::NewProjectLayout,
                label: "New Project layout",
                desc: "How a Project opened from a directory starts; set in its Layout menu",
                kind: Kind::Action,
            },
        ],
        // The pane itself *is* the editor (draw_pane special-cases it before
        // touching rows()); handle_keys also delegates to it before rows()
        // could be indexed, so this is never read.
        Pane::Keybindings => &[],
        // Custom body too: `remotes_pane` draws and reads its own keys.
        Pane::Remotes => &[],
        Pane::Agents => &[
            RowSpec {
                field: Field::InstallSkills,
                label: "Install agent skills on launch",
                desc: "Writes foreman-dispatch / foreman-chat into Claude & Codex skill dirs",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::DispatchWorktrees,
                label: "Dispatch cards into git worktrees",
                desc: "Each dispatched card works in .foreman/worktrees/<id> on branch card/<id>",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::RemoteListen,
                label: "Accept chat from other PCs (Tailscale)",
                desc: "Lets the PCs you allow (foreman peers add <name>) chat with and read your terminals",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::AutoNameAgentSessions,
                label: "Automatically name agent Sessions",
                desc: "Enabling installs guarded global hooks; Codex may ask you to trust its hook",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::AgentStateBadges,
                label: "Show agent state in the Sessions panel",
                desc: "Installs guarded global hooks on a few lifecycle events; Codex may ask you to trust them",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::TitleProvider,
                label: "Naming provider",
                desc: "Can differ from the source agent; Foreman never injects an API key",
                kind: Kind::Choice,
            },
            RowSpec {
                field: Field::TitleModel,
                label: "Naming model",
                desc: "Model id for the selected CLI (blank = provider default)",
                kind: Kind::Text,
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

fn description(spec: &RowSpec, settings: &Settings) -> &'static str {
    if spec.field != Field::AutoNameAgentSessions || !settings.auto_name_agent_sessions {
        return spec.desc;
    }
    match settings.title_provider {
        NamingProvider::Codex => {
            "Shares up to 3,800 prompt chars + prior title from any agent with Codex; consumes its allowance"
        }
        NamingProvider::Claude => {
            "Shares up to 3,800 prompt chars + prior title with Claude; counts against subscription usage"
        }
        NamingProvider::Grok => {
            "Shares up to 3,800 prompt chars + prior title from any agent with Grok; consumes its plan allowance"
        }
    }
}

/// Direction a row's value should move. `Toggle` is meaningless for steppers
/// (ignored) and vice versa (flip ignores it) — each `adjust` arm picks what
/// applies to its field's `Kind`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Adjust {
    Toggle,
    Inc,
    Dec,
}

/// Apply one nav action to one field. Returns whether the value actually
/// changed (steppers at a clamp bound report `false`; `Action` fields are
/// handled by the view and always report `false` here).
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
        Field::DispatchWorktrees => flip(&mut s.dispatch_worktrees),
        Field::RemoteListen => flip(&mut s.remote_listen),
        Field::AutoNameAgentSessions => flip(&mut s.auto_name_agent_sessions),
        Field::AgentStateBadges => flip(&mut s.agent_state_badges),
        Field::TitleProvider => {
            let next = match (s.title_provider, a) {
                (NamingProvider::Codex, Adjust::Inc | Adjust::Toggle) => NamingProvider::Claude,
                (NamingProvider::Claude, Adjust::Inc | Adjust::Toggle) => NamingProvider::Grok,
                (NamingProvider::Grok, Adjust::Inc | Adjust::Toggle) => NamingProvider::Codex,
                (NamingProvider::Codex, Adjust::Dec) => NamingProvider::Grok,
                (NamingProvider::Claude, Adjust::Dec) => NamingProvider::Codex,
                (NamingProvider::Grok, Adjust::Dec) => NamingProvider::Claude,
            };
            if next == s.title_provider {
                false
            } else {
                s.title_provider = next;
                s.title_model = next.initial_model().to_string();
                true
            }
        }
        Field::TitleModel => false,
        Field::CrewStale => step_u32(&mut s.crew_stale_secs, a, 30, 30, 3600),
        Field::SendSettle => step_u64(&mut s.send_settle_ms, a, 20, 0, 2000),
        Field::RestoreWorkspace => flip(&mut s.restore_workspace),
        Field::DefaultProjectDir => false,
        Field::UpdateCheck => flip(&mut s.update_check),
        Field::CheckUpdatesNow | Field::OpenConfigFolder | Field::NewProjectLayout => false,
    }
}

/// Render a field's current value as the view's row-trailing text.
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
        Field::DispatchWorktrees => s.dispatch_worktrees.to_string(),
        Field::RemoteListen => s.remote_listen.to_string(),
        Field::AutoNameAgentSessions => s.auto_name_agent_sessions.to_string(),
        Field::AgentStateBadges => s.agent_state_badges.to_string(),
        Field::TitleProvider => s.title_provider.label().to_string(),
        Field::TitleModel => s.title_model.clone(),
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
        // Lives in project-layouts.json, not Settings: the view reads it.
        Field::CheckUpdatesNow | Field::OpenConfigFolder | Field::NewProjectLayout => String::new(),
    }
}

/// Navigation/focus state for the settings menu. Pure — no egui here; the
/// view drives it from key events and reads it back to render.
#[derive(Debug)]
pub struct SettingsMenu {
    pub pane: Pane,
    pub row: usize,
    pub in_rail: bool,
    /// When `Some`, an inline text field is open for the selected `Text` row,
    /// holding the in-progress edit buffer. `None` = browsing.
    pub editing: Option<String>,
    /// A window-lifecycle outcome (Close / CheckUpdatesNow) produced this
    /// frame, stashed for the WM's `drain_settings` to act on after the
    /// render loop (content cannot mutate the WM mid-loop).
    pub pending: Option<MenuOutcome>,
    /// Set when the keyboard moved the selection (row nav, pane switch, diving
    /// into the pane); consumed by `draw_pane` to scroll the selected row into
    /// view. Lets keyboard nav reach rows below the fold on a short window
    /// without fighting free mouse-wheel scrolling.
    pub scroll_to_selected: bool,
    /// The keybindings editor, rendered inline when the Keybindings pane is
    /// active (replaces the old stacked modal).
    pub keybindings: SettingsView,
    /// The appearance/theme editor, rendered inline when the Appearance pane is
    /// active (custom body, like Keybindings).
    pub appearance: crate::appearance::AppearanceView,
    /// The Remotes editor, rendered inline when the Remotes pane is active.
    pub remotes: crate::remotes_pane::RemotesView,
    /// Set for exactly one frame when the rail dives into the Keybindings pane
    /// (Enter/→). `draw_pane` consumes it to skip that frame's editor input —
    /// otherwise the same un-consumed Enter that dove in is read by the
    /// editor's Idle branch as "start a rebind".
    pub editor_just_entered: bool,
    /// The New Project layout's name as last read from
    /// `project-layouts.json` (`Ok(None)`: Single terminal), or why it could
    /// not be read. `None` until read: on first draw, and again whenever the
    /// pane is selected or the window becomes active, never per frame.
    new_project_layout: Option<Result<Option<String>, String>>,
    /// Whether the window was active last frame, to re-read on activation.
    was_active: bool,
    /// Test seam: the layout store to use instead of the user's.
    #[cfg(test)]
    pub layout_store: Option<crate::layout_library::LayoutStore>,
}

impl SettingsMenu {
    pub fn new() -> Self {
        Self {
            pane: Pane::Terminal,
            row: 0,
            in_rail: true,
            editing: None,
            pending: None,
            scroll_to_selected: false,
            keybindings: SettingsView::new(),
            appearance: crate::appearance::AppearanceView::new(),
            remotes: crate::remotes_pane::RemotesView::new(),
            editor_just_entered: false,
            new_project_layout: None,
            was_active: false,
            #[cfg(test)]
            layout_store: None,
        }
    }

    /// The menu's intrinsic content size (title + body + footer bands), used to
    /// size the floating window when it is first opened.
    pub fn size() -> egui::Vec2 {
        egui::vec2(WIN_W, TITLE_H + BODY_H + FOOTER_H)
    }

    /// The smallest usable content size, used as the settings window's resize
    /// floor (larger than the global window minimum). Wide enough that a row's
    /// label and its control don't collide (rail + a usable pane), and tall
    /// enough to show the whole rail plus the title and footer. Below this the
    /// layout would cramp horizontally, and there is no horizontal scroll.
    pub fn min_size() -> egui::Vec2 {
        let rail_h = Pane::ALL.len() as f32 * 40.0; // 40.0 = draw_rail row height
        // + 20 leaves room for the vertical scrollbar so the floor itself never
        // triggers the horizontal scroll.
        egui::vec2(RAIL_W + PANE_MIN_W + 20.0, rail_h + TITLE_H + FOOTER_H)
    }

    /// Move up one row in the current pane; clamps at 0 (no wrap).
    pub fn nav_up(&mut self) {
        self.row = self.row.saturating_sub(1);
        self.scroll_to_selected = true;
    }

    /// Move down one row in the current pane; clamps at the last row.
    pub fn nav_down(&mut self) {
        let last = rows(self.pane).len().saturating_sub(1);
        self.row = (self.row + 1).min(last);
        self.scroll_to_selected = true;
    }

    /// Flip focus between the pane rail and the row list.
    pub fn nav_tab(&mut self) {
        self.in_rail = !self.in_rail;
    }

    /// Switch to a different pane, resetting the row cursor.
    pub fn select_pane(&mut self, p: Pane) {
        self.pane = p;
        self.row = 0;
        self.scroll_to_selected = true;
        self.new_project_layout = None;
    }

    fn layout_store(
        &self,
    ) -> Result<crate::layout_library::LayoutStore, crate::layout_library::StoreError> {
        #[cfg(test)]
        {
            // Never the user's file from a test.
            Ok(self.layout_store.clone().unwrap_or_else(|| {
                crate::layout_library::LayoutStore::at(
                    std::env::temp_dir().join("foreman-settings-layout-unset"),
                )
            }))
        }
        #[cfg(not(test))]
        crate::layout_library::LayoutStore::user()
    }

    /// The New Project layout row's value and whether Reset has anything to
    /// do. Reads the file only when the cached copy was dropped.
    fn new_project_layout_value(&mut self) -> (String, bool) {
        if self.new_project_layout.is_none() {
            let read = self
                .layout_store()
                .and_then(|store| store.new_project_layout())
                .map(|def| def.map(|d| d.name))
                .map_err(|e| e.to_string());
            self.new_project_layout = Some(read);
        }
        match self.new_project_layout.as_ref().expect("read above") {
            Ok(Some(name)) => (format!("\u{201c}{name}\u{201d}"), true),
            Ok(None) => ("Single terminal".to_string(), false),
            Err(_) => ("Unreadable".to_string(), false),
        }
    }

    /// Reset the New Project layout to Single terminal: one tiled terminal.
    fn reset_new_project_layout(&mut self, ctx: &egui::Context) {
        let written = self
            .layout_store()
            .and_then(|store| store.set_new_project_layout(None));
        if let Err(e) = written {
            crate::notify::queue(
                ctx,
                crate::notify::Level::Error,
                format!("New Project layout not reset: {e}"),
            );
        }
        self.new_project_layout = None;
    }
}

impl Default for SettingsMenu {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// View: an egui window over the pure model above. Mirrors the
// keybindings editor's overlay scaffolding (dim layer, centered panel, full
// input capture) — see `src/settings.rs`.
// ---------------------------------------------------------------------------

/// Panel geometry (points). Fixed so the layout reads the same on every pane.
const WIN_W: f32 = 850.0;
const RAIL_W: f32 = 190.0;
const TITLE_H: f32 = 38.0;
const BODY_H: f32 = 500.0;
const FOOTER_H: f32 = 30.0;
/// Width the pane rows are laid out at when the window is too narrow to fit them
/// — a comfortable row width (label + control + gap). Below this the pane
/// scrolls horizontally instead of cramping; it's also the floor the window's
/// resize minimum is built on.
const PANE_MIN_W: f32 = 360.0;

/// What the settings menu wants the caller (wm) to do after a frame.
#[derive(Clone, Debug)]
pub enum MenuOutcome {
    /// Stay open, nothing to persist.
    Pending,
    /// A setting changed this frame — caller publishes it + arms the save debounce.
    Changed,
    /// "Check for updates now" was clicked — caller fires the fetch through
    /// `update_fx` (the menu has no path to that channel itself).
    CheckUpdatesNow,
    /// Settings > Remotes confirmed an agent CLI install: the desktop opens
    /// it as a Remote Session in the focused Project.
    Install {
        remote: String,
        provider: &'static str,
    },
    /// Close the menu.
    Close,
}

/// Merge a newly-produced outcome into the running one, keeping the
/// highest-priority (Close > CheckUpdatesNow > Changed > Pending). Keyboard and
/// mouse can both fire in one frame; this stops a stray mouse `Changed` from
/// clobbering a keyboard `Close`.
fn bump(cur: &mut MenuOutcome, new: MenuOutcome) {
    fn rank(o: &MenuOutcome) -> u8 {
        match o {
            MenuOutcome::Pending => 0,
            MenuOutcome::Changed => 1,
            MenuOutcome::CheckUpdatesNow => 2,
            MenuOutcome::Install { .. } => 3,
            MenuOutcome::Close => 4,
        }
    }
    if rank(&new) > rank(cur) {
        *cur = new;
    }
}

impl SettingsMenu {
    /// Render one frame of the settings menu and report what the caller should
    /// do. `s` is the live settings, mutated in place; a `Changed` outcome means
    /// the caller republishes it and arms the save debounce. `km` is the live
    /// keymap, threaded through to the inline Keybindings pane.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        s: &mut Settings,
        km: &mut Keymap,
        theme: &mut crate::theme::Theme,
    ) -> MenuOutcome {
        let th = crate::theme::live(ui.ctx());
        // The New Project layout is set from a Project's picker: coming back
        // to this window re-reads it.
        if active && !self.was_active {
            self.new_project_layout = None;
        }
        self.was_active = active;
        // Keyboard drives the menu only when this window is focused and no
        // inline text edit owns input.
        let mut outcome = if active && self.editing.is_none() {
            self.handle_keys(ui, s)
        } else {
            MenuOutcome::Pending
        };

        // Fill the window body (the WM draws the header/border chrome around it).
        ui.painter_at(rect).rect_filled(rect, 0.0, th.win_bg);

        // A scoped child UI bounded to `rect` so `override_text_color`/spacing
        // mutations stay local and never leak to sibling windows drawn later in
        // the same parent `ui` (idiom: landing.rs `ui.new_child`).
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let ui = &mut child;
        // Contain the fixed-size bands if the window is resized smaller than the
        // menu's intrinsic size — clip to `rect` so nothing bleeds onto siblings.
        ui.set_clip_rect(rect);
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        ui.visuals_mut().override_text_color = Some(th.text);
        // Reactive layout: bands span the window's width and the body fills the
        // height between the top title band and the bottom-pinned footer, so the
        // panel grows and shrinks with the window instead of a fixed 660×368.
        let w = rect.width();
        let body_h = (rect.height() - TITLE_H - FOOTER_H).max(0.0);
        let rail_w = RAIL_W.min(w);

        // --- title band ---
        let (title, _) = ui.allocate_exact_size(egui::vec2(w, TITLE_H), egui::Sense::hover());
        ui.painter().rect_filled(
            title,
            egui::CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            },
            th.title_bg_focus,
        );
        ui.painter().text(
            egui::pos2(title.min.x + 18.0, title.center().y),
            egui::Align2::LEFT_CENTER,
            self.pane.label(),
            egui::FontId::proportional(15.0),
            th.text,
        );

        // --- body: rail | pane ---
        let (body, _) = ui.allocate_exact_size(egui::vec2(w, body_h), egui::Sense::hover());
        let rail = egui::Rect::from_min_size(body.min, egui::vec2(rail_w, body_h));
        let pane = egui::Rect::from_min_max(egui::pos2(body.min.x + rail_w, body.min.y), body.max);
        self.draw_rail(ui, rail);
        self.draw_pane(ui, pane, s, km, theme, active, &mut outcome);

        // --- footer ---
        let (footer, _) = ui.allocate_exact_size(egui::vec2(w, FOOTER_H), egui::Sense::hover());
        ui.painter().line_segment(
            [footer.left_top(), footer.right_top()],
            egui::Stroke::new(1.0, th.border),
        );
        ui.painter().text(
            egui::pos2(footer.min.x + 18.0, footer.center().y),
            egui::Align2::LEFT_CENTER,
            "↑↓ navigate · Tab rail⇄pane · Enter edit · ←→ adjust · Esc close",
            egui::FontId::proportional(11.5),
            th.dim,
        );

        outcome
    }

    /// True while the embedded keybindings editor is mid chord-capture. The WM
    /// reads this to suppress the leader while a settings window is focused.
    pub fn is_capturing(&self) -> bool {
        self.pane == Pane::Keybindings && self.keybindings.is_capturing()
    }

    #[cfg(test)]
    pub fn begin_capture_for_test(&mut self) {
        self.keybindings.begin_capture_for_test();
    }

    /// Read this frame's navigation/adjust keys and apply them. Returns the
    /// keyboard-driven outcome; mouse handling in the draw can only raise it.
    fn handle_keys(&mut self, ui: &egui::Ui, s: &mut Settings) -> MenuOutcome {
        let (up, down, tab, left, right, enter, esc) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Tab),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });

        // The Keybindings pane's editor reads its own input in draw_pane; don't
        // double-handle its keys here. Tab still backs out to the rail. This must
        // run before the Esc check below, so Esc inside the editor reaches
        // `SettingsView` (cancel-capture / back-to-rail) instead of closing the
        // window.
        if self.pane == Pane::Keybindings && !self.in_rail {
            if tab {
                self.in_rail = true;
            }
            return MenuOutcome::Pending;
        }

        // The Appearance pane's editor reads its own input in draw_pane; don't
        // double-handle here. Tab backs out to the rail. Runs before the Esc
        // check so Esc reaches the pane (back-to-rail) rather than closing.
        if self.pane == Pane::Appearance && !self.in_rail {
            if tab {
                self.in_rail = true;
            }
            return MenuOutcome::Pending;
        }

        // Same for Remotes, except Tab moves between the edit form's fields
        // while the form is open.
        if self.pane == Pane::Remotes && !self.in_rail {
            if tab && !self.remotes.is_editing() {
                self.in_rail = true;
            }
            return MenuOutcome::Pending;
        }

        if esc {
            return MenuOutcome::Close;
        }
        if tab {
            self.nav_tab();
        }

        if self.in_rail {
            if up {
                self.prev_pane();
            }
            if down {
                self.next_pane();
            }
            // Enter or → dives from the rail into the pane's rows.
            if enter || right {
                self.in_rail = false;
                self.scroll_to_selected = true;
                // Skip the Keybindings editor's input for the dive-in frame —
                // otherwise this same Enter (never consumed above) is read by
                // the editor's Idle branch as "start a rebind".
                self.editor_just_entered = true;
            }
            return MenuOutcome::Pending;
        }

        if up {
            self.nav_up();
        }
        if down {
            self.nav_down();
        }

        let spec = rows(self.pane)[self.row];
        let mut changed = false;
        if left {
            changed |= adjust(spec.field, Adjust::Dec, s);
        }
        if right {
            changed |= adjust(spec.field, Adjust::Inc, s);
        }
        if enter {
            match spec.kind {
                Kind::Toggle => changed |= adjust(spec.field, Adjust::Toggle, s),
                Kind::Stepper | Kind::Choice => changed |= adjust(spec.field, Adjust::Inc, s),
                Kind::Text => self.editing = Some(display(spec.field, s)),
                Kind::Action => return self.do_action(spec.field, ui.ctx()),
            }
        }
        if changed {
            MenuOutcome::Changed
        } else {
            MenuOutcome::Pending
        }
    }

    fn pane_index(&self) -> usize {
        Pane::ALL.iter().position(|p| *p == self.pane).unwrap_or(0)
    }

    /// Move the rail selection up a pane (clamps; no wrap, matching row nav).
    fn prev_pane(&mut self) {
        let i = self.pane_index().saturating_sub(1);
        self.select_pane(Pane::ALL[i]);
    }

    /// Move the rail selection down a pane (clamps at the last pane).
    fn next_pane(&mut self) {
        let i = (self.pane_index() + 1).min(Pane::ALL.len() - 1);
        self.select_pane(Pane::ALL[i]);
    }

    /// Run an `Action` row.
    fn do_action(&mut self, field: Field, ctx: &egui::Context) -> MenuOutcome {
        match field {
            Field::CheckUpdatesNow => MenuOutcome::CheckUpdatesNow,
            Field::NewProjectLayout => {
                // Mouse and Enter alike: nothing to reset while it is
                // already Single terminal (or unreadable).
                if self.new_project_layout_value().1 {
                    self.reset_new_project_layout(ctx);
                }
                MenuOutcome::Pending
            }
            Field::OpenConfigFolder => {
                if let Some(dir) = crate::config::config_dir() {
                    std::process::Command::new("explorer").arg(dir).spawn().ok();
                }
                MenuOutcome::Pending
            }
            _ => MenuOutcome::Pending,
        }
    }

    /// Left rail: one clickable row per pane; the active pane gets a wash and a
    /// bright left edge.
    fn draw_rail(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let th = crate::theme::live(ui.ctx());
        ui.painter().line_segment(
            [rect.right_top(), rect.right_bottom()],
            egui::Stroke::new(1.0, th.border),
        );
        let row_h = 40.0;
        for (i, p) in Pane::ALL.iter().enumerate() {
            let r = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, rect.min.y + i as f32 * row_h),
                egui::vec2(rect.width(), row_h),
            );
            let active = *p == self.pane;
            let resp = ui.interact(r, egui::Id::new(("settings_rail", i)), egui::Sense::click());
            if resp.clicked() {
                self.select_pane(*p);
                self.in_rail = false;
                self.editing = None; // abandon any inline text edit on pane change
            }
            if active {
                ui.painter().rect_filled(r, 0.0, th.sel_bg);
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(r.min, egui::vec2(2.0, row_h)),
                    0.0,
                    th.border_focus,
                );
            }
            let color = if active || resp.hovered() {
                th.text
            } else {
                th.dim
            };
            ui.painter().text(
                egui::pos2(r.min.x + 16.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                p.label(),
                egui::FontId::proportional(13.5),
                color,
            );
        }
    }

    /// Right pane: one row per `RowSpec` — label + dim description on the left,
    /// the kind-specific control on the right.
    fn draw_pane(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        s: &mut Settings,
        km: &mut Keymap,
        theme: &mut crate::theme::Theme,
        active: bool,
        outcome: &mut MenuOutcome,
    ) {
        let th = crate::theme::live(ui.ctx());
        // Consumed every frame regardless of pane: the one-frame suppression
        // only matters on the Keybindings pane, but the flag must not linger
        // if the user dove in then immediately switched panes some other way.
        let just_entered = std::mem::take(&mut self.editor_just_entered);
        if self.pane == Pane::Keybindings {
            let reads_input = active && !self.in_rail && !just_entered;
            match self.keybindings.show(ui, rect, reads_input, km) {
                EditorOutcome::Changed => bump(outcome, MenuOutcome::Changed),
                EditorOutcome::Close => self.in_rail = true, // Esc/back-out returns to the rail
                EditorOutcome::Pending => {}
            }
            return;
        }
        if self.pane == Pane::Remotes {
            let reads_input = active && !self.in_rail && !just_entered;
            match self.remotes.show(ui, rect, reads_input, &mut s.remotes) {
                crate::remotes_pane::Outcome::Changed => bump(outcome, MenuOutcome::Changed),
                crate::remotes_pane::Outcome::Close => self.in_rail = true,
                crate::remotes_pane::Outcome::Install { remote, provider } => {
                    bump(outcome, MenuOutcome::Install { remote, provider })
                }
                crate::remotes_pane::Outcome::Pending => {}
            }
            return;
        }
        if self.pane == Pane::Appearance {
            // Resync the pane to the live theme when its name changed (preset
            // switch / Duplicate / rename) or when the live theme diverged from
            // the pane's copy (the App adopted a hand edit from disk). Never
            // mid-edit: the pane's own edits ARE the live theme, so they compare
            // equal and are never clobbered.
            if self.appearance.needs_resync(&s.theme, theme) {
                self.appearance.set_active(&s.theme, theme.clone());
            }
            let reads_input = active && !self.in_rail && !just_entered;
            match self.appearance.show(ui, rect, reads_input, theme) {
                crate::appearance::Outcome::Changed => bump(outcome, MenuOutcome::Changed),
                crate::appearance::Outcome::Duplicate(name) => {
                    // Write the fork as a new user theme, then make it active; the
                    // App reloads it (now editable) and the pane resyncs to it. On a
                    // write failure, don't advance to a phantom name with no file.
                    match self.appearance.working().save(&name) {
                        Ok(()) => {
                            s.theme = name;
                            bump(outcome, MenuOutcome::Changed);
                        }
                        Err(e) => crate::notify::queue(
                            ui.ctx(),
                            crate::notify::Level::Error,
                            format!("Could not create theme: {e}"),
                        ),
                    }
                }
                crate::appearance::Outcome::SelectPreset(name) => {
                    // Just switch the name. The App is the only writer of theme
                    // files: on the name change it flushes an edit still inside
                    // the save debounce to the outgoing file (and only then), so a
                    // clean theme whose file was hand-edited is never clobbered.
                    s.theme = name;
                    bump(outcome, MenuOutcome::Changed);
                }
                crate::appearance::Outcome::Reload => {
                    // Strict re-read; the App adopts the republished theme and
                    // re-records the file mtime after its (no-op) save.
                    match crate::theme::Theme::try_load(&s.theme) {
                        Ok(t) => {
                            *theme = t.clone();
                            self.appearance.set_active(&s.theme, t);
                            bump(outcome, MenuOutcome::Changed);
                        }
                        Err(e) => crate::notify::queue(
                            ui.ctx(),
                            crate::notify::Level::Warning,
                            format!("Theme file not applied: {e}"),
                        ),
                    }
                }
                crate::appearance::Outcome::OpenThemesFolder => {
                    if let Some(dir) = crate::config::themes_dir() {
                        std::process::Command::new("explorer").arg(dir).spawn().ok();
                    }
                }
                crate::appearance::Outcome::Rename(name) => {
                    // Write the current content under the new name + drop the old
                    // file, then switch (the App reloads it and the pane resyncs).
                    match self.appearance.working().rename(&s.theme, &name) {
                        Ok(new_slug) => {
                            s.theme = new_slug;
                            bump(outcome, MenuOutcome::Changed);
                        }
                        Err(e) => crate::notify::queue(
                            ui.ctx(),
                            crate::notify::Level::Error,
                            format!("Could not rename theme: {e}"),
                        ),
                    }
                }
                crate::appearance::Outcome::Delete(name) => {
                    // Remove the user theme file and fall back to the built-in.
                    if let Err(e) = crate::theme::Theme::delete(&name) {
                        crate::notify::queue(
                            ui.ctx(),
                            crate::notify::Level::Error,
                            format!("Could not delete theme: {e}"),
                        );
                    }
                    s.theme = crate::appearance::BUILTIN.to_string();
                    bump(outcome, MenuOutcome::Changed);
                }
                crate::appearance::Outcome::Pending => {}
            }
            return;
        }
        let specs = rows(self.pane);
        // Safety net: an inline edit is only valid while its own Text row is the
        // selection. If anything moved the selection away (e.g. a mouse click on
        // the rail or another control) drop the edit — otherwise its TextEdit
        // stops being drawn, never loses focus, and the keyboard stays frozen.
        if self.editing.is_some()
            && !(!self.in_rail && self.row < specs.len() && specs[self.row].kind == Kind::Text)
        {
            self.editing = None;
        }
        let row_h = 46.0;
        let pad = 18.0;
        let pane = self.pane;
        let cur_row = self.row;
        let in_rail = self.in_rail;
        // Consumed once: scroll the selected row into view after a keyboard move,
        // without snapping back while the user free-scrolls with the wheel.
        let scroll_to = std::mem::take(&mut self.scroll_to_selected);

        // Rows live in a vertical ScrollArea clipped to `rect`, so a window too
        // short to show every row scrolls instead of clipping the lower rows.
        let mut pane_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        egui::ScrollArea::both()
            .id_salt(("settings_pane", pane.label()))
            .auto_shrink([false, false])
            .show(&mut pane_ui, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                ui.visuals_mut().override_text_color = Some(th.text);
                // Fill the viewport, but never draw rows narrower than PANE_MIN_W
                // — below that the pane scrolls horizontally instead of cramping
                // the label into its control. set_min_width makes the ScrollArea
                // register the content as wider than the viewport so it shows a
                // horizontal scrollbar.
                let width = ui.available_width().max(PANE_MIN_W);
                ui.set_min_width(width);
                for (i, spec) in specs.iter().enumerate() {
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::hover());
                    let selected = !in_rail && i == cur_row;
                    if selected {
                        ui.painter().rect_filled(
                            r.shrink2(egui::vec2(6.0, 3.0)),
                            egui::CornerRadius::same(4),
                            th.sel_bg,
                        );
                        if scroll_to {
                            ui.scroll_to_rect(r, None);
                        }
                    }
                    ui.painter().text(
                        egui::pos2(r.min.x + pad, r.min.y + 16.0),
                        egui::Align2::LEFT_CENTER,
                        spec.label,
                        egui::FontId::proportional(13.0),
                        th.text,
                    );
                    let desc = description(spec, s);
                    if !desc.is_empty() {
                        ui.painter().text(
                            egui::pos2(r.min.x + pad, r.min.y + 32.0),
                            egui::Align2::LEFT_CENTER,
                            desc,
                            egui::FontId::proportional(11.0),
                            th.dim,
                        );
                    }
                    let anchor_x = r.max.x - pad;
                    let cy = r.center().y;
                    self.draw_control(ui, spec, s, anchor_x, cy, r, selected, outcome, pane, i);
                }
                // Static version stamp, Startup pane only — allocated (not
                // painted at an absolute y) so it scrolls with the rows.
                if pane == Pane::Startup {
                    let (vr, _) =
                        ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::hover());
                    ui.painter().text(
                        egui::pos2(vr.min.x + pad, vr.min.y + 20.0),
                        egui::Align2::LEFT_CENTER,
                        format!("Foreman v{}", env!("CARGO_PKG_VERSION")),
                        egui::FontId::proportional(11.0),
                        th.dim,
                    );
                }
            });
    }

    /// Draw the control for one row and route its mouse interaction.
    #[allow(clippy::too_many_arguments)]
    fn draw_control(
        &mut self,
        ui: &mut egui::Ui,
        spec: &RowSpec,
        s: &mut Settings,
        anchor_x: f32,
        cy: f32,
        row_rect: egui::Rect,
        selected: bool,
        outcome: &mut MenuOutcome,
        pane: Pane,
        idx: usize,
    ) {
        let th = crate::theme::live(ui.ctx());
        let id = egui::Id::new(("settings_ctl", pane.label(), idx));
        match spec.kind {
            Kind::Toggle => {
                let on = display(spec.field, s) == "true";
                let (w, h) = (34.0, 18.0);
                let bx = egui::Rect::from_min_size(
                    egui::pos2(anchor_x - w, cy - h / 2.0),
                    egui::vec2(w, h),
                );
                let resp = ui.interact(bx, id, egui::Sense::click());
                if resp.clicked() && adjust(spec.field, Adjust::Toggle, s) {
                    bump(outcome, MenuOutcome::Changed);
                }
                let col = if on { th.bell } else { th.border };
                ui.painter().rect_stroke(
                    bx,
                    egui::CornerRadius::same(9),
                    egui::Stroke::new(1.5, col),
                    egui::StrokeKind::Inside,
                );
                let knob_r = 6.0;
                let kx = if on {
                    bx.max.x - knob_r - 2.0
                } else {
                    bx.min.x + knob_r + 2.0
                };
                ui.painter().circle_filled(
                    egui::pos2(kx, cy),
                    knob_r,
                    if on { th.bell } else { th.dim },
                );
            }
            Kind::Stepper => {
                let val = display(spec.field, s);
                let plus = egui::Rect::from_min_size(
                    egui::pos2(anchor_x - 20.0, cy - 10.0),
                    egui::vec2(20.0, 20.0),
                );
                let vw = 88.0;
                let value_rect = egui::Rect::from_min_size(
                    egui::pos2(plus.min.x - vw, cy - 10.0),
                    egui::vec2(vw, 20.0),
                );
                let minus = egui::Rect::from_min_size(
                    egui::pos2(value_rect.min.x - 20.0, cy - 10.0),
                    egui::vec2(20.0, 20.0),
                );
                let rp = ui.interact(plus, id.with("plus"), egui::Sense::click());
                let rm = ui.interact(minus, id.with("minus"), egui::Sense::click());
                if rp.clicked() && adjust(spec.field, Adjust::Inc, s) {
                    bump(outcome, MenuOutcome::Changed);
                }
                if rm.clicked() && adjust(spec.field, Adjust::Dec, s) {
                    bump(outcome, MenuOutcome::Changed);
                }
                let border = if selected { th.border_focus } else { th.border };
                for (rc, sym, hov) in [(minus, "−", rm.hovered()), (plus, "+", rp.hovered())] {
                    ui.painter().rect_stroke(
                        rc,
                        egui::CornerRadius::same(4),
                        egui::Stroke::new(1.0, border),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        rc.center(),
                        egui::Align2::CENTER_CENTER,
                        sym,
                        egui::FontId::proportional(14.0),
                        if hov { th.text } else { th.dim },
                    );
                }
                ui.painter().text(
                    egui::pos2(value_rect.max.x - 4.0, cy),
                    egui::Align2::RIGHT_CENTER,
                    val,
                    egui::FontId::proportional(12.5),
                    th.text,
                );
            }
            Kind::Choice => {
                let val = display(spec.field, s);
                let galley = ui.painter().layout_no_wrap(
                    val.clone(),
                    egui::FontId::proportional(12.5),
                    th.text,
                );
                let w = galley.size().x + 24.0;
                let chip = egui::Rect::from_min_size(
                    egui::pos2(anchor_x - w, cy - 11.0),
                    egui::vec2(w, 22.0),
                );
                let resp = ui.interact(chip, id, egui::Sense::click());
                if resp.clicked() && adjust(spec.field, Adjust::Inc, s) {
                    bump(outcome, MenuOutcome::Changed);
                }
                let border = if selected || resp.hovered() {
                    th.border_focus
                } else {
                    th.border
                };
                ui.painter().rect_stroke(
                    chip,
                    egui::CornerRadius::same(4),
                    egui::Stroke::new(1.0, border),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    chip.center(),
                    egui::Align2::CENTER_CENTER,
                    val,
                    egui::FontId::proportional(12.5),
                    th.text,
                );
            }
            Kind::Text => {
                if self.editing.is_some() && selected {
                    let te_w = (row_rect.width() - 220.0).clamp(160.0, 300.0);
                    let te_rect = egui::Rect::from_min_size(
                        egui::pos2(anchor_x - te_w, cy - 12.0),
                        egui::vec2(te_w, 24.0),
                    );
                    let buf = self.editing.as_mut().unwrap();
                    let resp = ui.put(te_rect, egui::TextEdit::singleline(buf).desired_width(te_w));
                    // Canonical egui commit/cancel: Enter loses focus AND is
                    // pressed → commit; any other focus loss (Esc, click-away)
                    // cancels. request_focus keeps the field hot until then.
                    if resp.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let committed = self.editing.take().unwrap();
                            // Every Kind::Text field needs a commit arm here —
                            // there is no generic string path through adjust().
                            match spec.field {
                                Field::DefaultProjectDir => {
                                    s.default_project_dir = committed;
                                    bump(outcome, MenuOutcome::Changed);
                                }
                                Field::TitleModel => {
                                    s.title_model = committed;
                                    s.sanitize();
                                    bump(outcome, MenuOutcome::Changed);
                                }
                                other => {
                                    debug_assert!(
                                        false,
                                        "Kind::Text field {other:?} has no commit arm"
                                    );
                                }
                            }
                        } else {
                            self.editing = None;
                        }
                    } else {
                        resp.request_focus();
                    }
                } else {
                    let raw = display(spec.field, s);
                    let shown = if raw.is_empty() {
                        match spec.field {
                            Field::TitleModel => "(provider default)".to_string(),
                            _ => "(home)".to_string(),
                        }
                    } else {
                        raw
                    };
                    let galley = ui.painter().layout_no_wrap(
                        shown.clone(),
                        egui::FontId::proportional(12.5),
                        th.text,
                    );
                    let w = (galley.size().x + 8.0).max(60.0);
                    let hit = egui::Rect::from_min_size(
                        egui::pos2(anchor_x - w, cy - 12.0),
                        egui::vec2(w, 24.0),
                    );
                    let resp = ui.interact(hit, id, egui::Sense::click());
                    if resp.clicked() {
                        self.editing = Some(display(spec.field, s));
                    }
                    ui.painter().text(
                        egui::pos2(anchor_x, cy),
                        egui::Align2::RIGHT_CENTER,
                        shown,
                        egui::FontId::proportional(12.5),
                        if resp.hovered() { th.text } else { th.dim },
                    );
                }
            }
            Kind::Action => {
                let caption = match spec.field {
                    Field::OpenConfigFolder => "Open folder",
                    Field::CheckUpdatesNow => "Check now",
                    Field::NewProjectLayout => "Reset",
                    _ => "Open",
                };
                // The New Project layout shows its value before the button,
                // which does nothing while it is already Single terminal.
                let (value, enabled) = if spec.field == Field::NewProjectLayout {
                    let (value, enabled) = self.new_project_layout_value();
                    (Some(value), enabled)
                } else {
                    (None, true)
                };
                let galley = ui.painter().layout_no_wrap(
                    caption.to_string(),
                    egui::FontId::proportional(12.5),
                    th.text,
                );
                let w = galley.size().x + 24.0;
                let btn = egui::Rect::from_min_size(
                    egui::pos2(anchor_x - w, cy - 12.0),
                    egui::vec2(w, 24.0),
                );
                let resp = ui.interact(btn, id, egui::Sense::click());
                if resp.clicked() && enabled {
                    bump(outcome, self.do_action(spec.field, ui.ctx()));
                }
                let border = if enabled && (selected || resp.hovered()) {
                    th.border_focus
                } else {
                    th.border
                };
                ui.painter().rect_stroke(
                    btn,
                    egui::CornerRadius::same(4),
                    egui::Stroke::new(1.0, border),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    btn.center(),
                    egui::Align2::CENTER_CENTER,
                    caption,
                    egui::FontId::proportional(12.5),
                    if enabled { th.text } else { th.dim },
                );
                if let Some(value) = value {
                    let color = match &self.new_project_layout {
                        Some(Err(_)) => th.danger,
                        _ => th.text,
                    };
                    ui.painter().text(
                        egui::pos2(btn.min.x - 10.0, cy),
                        egui::Align2::RIGHT_CENTER,
                        value,
                        egui::FontId::proportional(12.5),
                        color,
                    );
                    if spec.field == Field::NewProjectLayout {
                        let tip = match &self.new_project_layout {
                            Some(Err(e)) => e.clone(),
                            _ => "Reset to Single terminal: new Projects open with one terminal"
                                .to_string(),
                        };
                        resp.on_hover_text(tip);
                    }
                }
            }
        }
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
            // Keybindings and Appearance have no RowSpecs — draw_pane/handle_keys
            // special-case them and delegate to their embedded editors instead.
            if p != Pane::Keybindings && p != Pane::Appearance && p != Pane::Remotes {
                assert!(!rows(p).is_empty(), "{:?} has no rows", p);
            }
        }
    }

    /// One frame of the whole menu with `key` pressed.
    fn frame_with_key(menu: &mut SettingsMenu, s: &mut Settings, key: egui::Key, shift: bool) {
        let mods = egui::Modifiers {
            shift,
            ..Default::default()
        };
        frame_at(&egui::Context::default(), menu, s, None, Some((key, mods)));
    }

    /// One frame of the whole menu on `ctx`, filling a window of `size`
    /// (egui's default screen when `None`), with an optional key press.
    fn frame_at(
        ctx: &egui::Context,
        menu: &mut SettingsMenu,
        s: &mut Settings,
        size: Option<egui::Vec2>,
        key: Option<(egui::Key, egui::Modifiers)>,
    ) {
        let mut input = egui::RawInput {
            screen_rect: size.map(|sz| egui::Rect::from_min_size(egui::Pos2::ZERO, sz)),
            ..Default::default()
        };
        if let Some((key, modifiers)) = key {
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            });
            input.modifiers = modifiers;
        }
        let mut km = Keymap::default();
        let mut theme = crate::theme::Theme::default();
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default()
                .frame(egui::Frame::new())
                .show(ctx, |ui| {
                    let r = ui.max_rect();
                    let outcome = menu.show(ui, r, true, s, &mut km, &mut theme);
                    if matches!(outcome, MenuOutcome::Changed) {
                        s.sanitize(); // what the debounced save's reload would do
                    }
                });
        });
    }

    fn remote(name: &str, target: &str) -> crate::remote::Remote {
        crate::remote::Remote {
            name: name.into(),
            target: target.into(),
            shell: Default::default(),
            dirs: Default::default(),
        }
    }

    /// At the narrowest pane the whole add form is on screen and no row's
    /// text runs under its status or buttons.
    #[test]
    fn remotes_pane_fits_at_its_minimum_width() {
        let mut menu = SettingsMenu::new();
        let mut s = Settings {
            remotes: vec![
                remote(
                    "a-remote-with-a-long-name",
                    "someone@a-very-long-host-name.example.internal.lan",
                ),
                remote("pi", "pi"),
            ],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        let ctx = egui::Context::default();
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let none = egui::Modifiers::NONE;
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        // The rows, before the add form scrolls the list to itself.
        let laid = menu.remotes.laid();
        assert_eq!(laid.rows.len(), 2);
        for (text, controls) in &laid.rows {
            assert!(laid.pane.contains_rect(*controls), "{controls:?}");
            assert!(
                text.max.x <= controls.min.x,
                "text {text:?} runs under {controls:?}"
            );
        }
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            Some(size),
            Some((egui::Key::A, none)),
        );
        assert!(menu.remotes.is_editing());
        for _ in 0..3 {
            frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        }
        let laid = menu.remotes.laid();
        assert!(
            (laid.pane.width() - PANE_MIN_W).abs() < 0.5,
            "{:?}",
            laid.pane
        );
        // Name, Target, Shell, add, cancel.
        assert_eq!(laid.form.len(), 5, "{:?}", laid.form);
        for r in &laid.form {
            assert!(laid.pane.contains_rect(*r), "{r:?} outside {:?}", laid.pane);
        }
    }

    /// At the narrowest pane, the folder list under a Directory row: every
    /// row and every Directory hint inside the pane and uncut, in the field
    /// and in the list; and its reserved area the same height whichever
    /// field is focused and whatever it shows, so no form row moves.
    #[test]
    fn remotes_folder_list_fits_at_minimum_width_and_moves_nothing() {
        use crate::remote::RemoteShell::Posix;
        use crate::remote_browse::{Entry, Failure, Listed, State};
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let none = egui::Modifiers::NONE;
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut desk = remote("desk", "andy@desk");
        desk.dirs.insert(r"H:\src\app".into(), "/srv/".into());
        desk.dirs.insert(r"H:\src\tool".into(), "/nope/".into());
        let mut s = Settings {
            remotes: vec![desk],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            Some(size),
            Some((egui::Key::Enter, none)),
        );
        assert!(menu.remotes.is_editing());
        let long = "a-folder-name-much-too-long-for-the-narrowest-pane-to-show-it-whole";
        let names = ["app", long, "data", "x1", "x2", "x3", "x4", "x5"];
        // Form rects relative to the Name field: the list scrolls the form,
        // which must not count as a row moving.
        let shape = |menu: &mut SettingsMenu| {
            let laid = menu.remotes.laid();
            let o = laid.form[0].min;
            let form: Vec<egui::Rect> = laid
                .form
                .iter()
                .map(|r| r.translate(-o.to_vec2()))
                .collect();
            let area = menu.remotes.browse().laid.area;
            (form, area.height(), area.min - o)
        };
        let fits = |menu: &mut SettingsMenu, state: &str| {
            let laid = menu.remotes.laid();
            let pane = laid.pane;
            assert!(laid.hint_fits, "{state}: hint cut at {pane:?}");
            assert!(laid.hint.height() < 16.0, "{state}: {:?}", laid.hint);
            let b = &menu.remotes.browse().laid;
            assert!(
                b.area.min.x >= pane.min.x && b.area.max.x <= pane.max.x,
                "{state}: area {:?} outside {pane:?}",
                b.area
            );
            for r in &b.rows {
                assert!(
                    r.min.x >= b.area.min.x && r.max.x <= b.area.max.x,
                    "{state}: row {r:?} outside {:?}",
                    b.area
                );
            }
            b.rows.len()
        };
        let tab = |menu: &mut SettingsMenu, s: &mut Settings, key: egui::Key| {
            frame_at(&ctx, menu, s, Some(size), Some((key, none)));
            frame_at(&ctx, menu, s, Some(size), None);
            frame_at(&ctx, menu, s, Some(size), None);
        };
        // Name: the area waits, idle.
        let b = menu.remotes.browse();
        b.put(
            "andy@desk",
            Posix,
            "/srv/",
            State::Listed(Listed {
                resolved: "/srv".into(),
                entries: names.iter().map(|n| Entry::dir(n)).collect(),
                truncated: false,
            }),
        );
        b.put(
            "andy@desk",
            Posix,
            "/nope/",
            crate::remote_browse::unreachable(
                "ssh: connect to host desk port 22: Connection refused",
            ),
        );
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        let idle = shape(&mut menu);
        fits(&mut menu, "Name");
        for _ in 0..3 {
            tab(&mut menu, &mut s, egui::Key::Tab);
        }
        // Directory 1, listed: the rows in view, the area naming whose
        // Directory it lists, and the field and its list both in view.
        let n = fits(&mut menu, "Dir field");
        assert!((4..=names.len() + 1).contains(&n), "rows in view: {n}");
        assert_eq!(
            menu.remotes.browse().laid.crumb_text,
            "Folders for app on desk in /srv/"
        );
        // The scroll animates: let it land.
        for _ in 0..40 {
            frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        }
        {
            let laid = menu.remotes.laid();
            let (seen, field) = (laid.list, laid.form[3]);
            let area = menu.remotes.browse().laid.area;
            assert!(seen.contains_rect(field), "field {field:?} out of {seen:?}");
            assert!(seen.contains_rect(area), "list {area:?} out of {seen:?}");
        }
        assert_eq!(
            shape(&mut menu),
            idle,
            "focusing a Directory moved the form"
        );
        tab(&mut menu, &mut s, egui::Key::ArrowDown);
        assert!(menu.remotes.browse().list_focused());
        fits(&mut menu, "list");
        assert_eq!(shape(&mut menu), idle, "entering the list moved the form");
        // Into "data": not listed yet, so Loading.
        tab(&mut menu, &mut s, egui::Key::ArrowDown);
        tab(&mut menu, &mut s, egui::Key::ArrowDown);
        tab(&mut menu, &mut s, egui::Key::ArrowRight);
        assert_eq!(fits(&mut menu, "loading"), 0);
        assert_eq!(shape(&mut menu).1, idle.1, "Loading changed the height");
        // Directory 2 cannot be reached: a retry row and why.
        tab(&mut menu, &mut s, egui::Key::Tab);
        tab(&mut menu, &mut s, egui::Key::Tab);
        assert!(!menu.remotes.browse().list_focused());
        assert_eq!(
            menu.remotes.browse().laid.crumb_text,
            "Folders for tool on desk in /nope/"
        );
        // Landing here asked again (an unreachable answer is not kept);
        // it still refuses.
        menu.remotes.browse().put(
            "andy@desk",
            Posix,
            "/nope/",
            crate::remote_browse::unreachable(
                "ssh: connect to host desk port 22: Connection refused",
            ),
        );
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        assert_eq!(fits(&mut menu, "retry"), 1);
        assert!(menu.remotes.browse().laid.message.is_some());
        assert_eq!(
            shape(&mut menu).1,
            idle.1,
            "the retry state changed the height"
        );
        // ...or answers that it cannot be listed.
        menu.remotes.browse().put(
            "andy@desk",
            Posix,
            "/nope/",
            State::Failed(Failure::new(
                "No such directory on this Remote. Check the path, or type another.",
            )),
        );
        tab(&mut menu, &mut s, egui::Key::F13);
        assert_eq!(fits(&mut menu, "failed"), 0);
        assert!(menu.remotes.browse().laid.message.is_some());
        assert_eq!(shape(&mut menu).1, idle.1, "Failed changed the height");
    }

    /// The Remotes form at its narrowest, making a folder: N types in the
    /// Directory field and opens the name field only from the list; the
    /// hint stays one line and the form and list area never move across
    /// naming, creating and a failure; Esc in the name field goes back to
    /// the list, not out of the form; Enter creates once and never saves,
    /// nor does its held repeat after the folder lands in the Directory.
    #[test]
    fn remotes_folder_list_makes_a_folder() {
        use crate::remote::RemoteShell::Posix;
        use crate::remote_browse::{Entry, Failure, Listed, State};
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut desk = remote("desk", "andy@desk");
        desk.dirs.insert(r"H:\src\app".into(), "/srv/".into());
        let mut s = Settings {
            remotes: vec![desk],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        let ev = |key: egui::Key, pressed: bool| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let run = |menu: &mut SettingsMenu, s: &mut Settings, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events,
                ..Default::default()
            };
            let mut km = Keymap::default();
            let mut theme = crate::theme::Theme::default();
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ctx, |ui| {
                        let r = ui.max_rect();
                        if matches!(
                            menu.show(ui, r, true, s, &mut km, &mut theme),
                            MenuOutcome::Changed
                        ) {
                            s.sanitize();
                        }
                    });
            });
        };
        let tap = |k: egui::Key| vec![ev(k, false), ev(k, true), ev(k, false)];
        let n_key = || {
            vec![
                ev(egui::Key::N, true),
                egui::Event::Text("n".into()),
                ev(egui::Key::N, false),
            ]
        };
        let shape = |menu: &mut SettingsMenu| {
            let laid = menu.remotes.laid();
            let o = laid.form[0].min;
            let form: Vec<egui::Rect> = laid
                .form
                .iter()
                .map(|r| r.translate(-o.to_vec2()))
                .collect();
            let b = &menu.remotes.browse().laid;
            let rows: Vec<egui::Rect> = b.rows.iter().map(|r| r.translate(-o.to_vec2())).collect();
            (form, b.area.translate(-o.to_vec2()), rows)
        };
        let fits = |menu: &mut SettingsMenu, state: &str| {
            let laid = menu.remotes.laid();
            assert!(laid.hint_fits, "{state}: hint cut at {:?}", laid.pane);
            assert!(laid.hint.height() < 16.0, "{state}: {:?}", laid.hint);
        };
        let saved = |s: &Settings| s.remotes[0].dirs[r"H:\src\app"].clone();

        run(&mut menu, &mut s, vec![]);
        run(&mut menu, &mut s, tap(egui::Key::Enter));
        assert!(menu.remotes.is_editing());
        menu.remotes.browse().put(
            "andy@desk",
            Posix,
            "/srv/",
            State::Listed(Listed {
                resolved: "/srv".into(),
                entries: ["app", "data"].iter().map(|n| Entry::dir(n)).collect(),
                truncated: false,
            }),
        );
        for _ in 0..3 {
            run(&mut menu, &mut s, tap(egui::Key::Tab));
        }
        // In the Directory field, N is typing.
        run(&mut menu, &mut s, n_key());
        assert_eq!(menu.remotes.draft_dir(0), Some("/srv/n"));
        assert!(!menu.remotes.browse().naming());
        run(&mut menu, &mut s, tap(egui::Key::Backspace));
        assert_eq!(menu.remotes.draft_dir(0), Some("/srv/"));
        run(&mut menu, &mut s, tap(egui::Key::ArrowDown));
        run(&mut menu, &mut s, vec![]);
        assert!(menu.remotes.browse().list_focused());
        fits(&mut menu, "list");
        // The scroll animates: let it land before measuring.
        for _ in 0..40 {
            run(&mut menu, &mut s, vec![]);
        }
        let list = shape(&mut menu);

        run(&mut menu, &mut s, n_key());
        assert!(menu.remotes.browse().naming());
        run(&mut menu, &mut s, vec![egui::Event::Text("made".into())]);
        assert_eq!(menu.remotes.browse().name_text(), Some("made"));
        assert_eq!(menu.remotes.draft_dir(0), Some("/srv/"));
        fits(&mut menu, "naming");
        assert_eq!(shape(&mut menu), list, "naming moved the form");
        assert_eq!(
            menu.remotes.browse().laid.crumb_text,
            "New folder in /srv/ on desk"
        );
        // Ctrl+Backspace edits the name; it never forgets the row.
        run(
            &mut menu,
            &mut s,
            vec![egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            }],
        );
        assert_eq!(menu.remotes.draft_dir(0), Some("/srv/"), "the row is kept");
        assert!(menu.remotes.browse().naming());
        run(&mut menu, &mut s, vec![egui::Event::Text("made".into())]);
        assert_eq!(menu.remotes.browse().name_text(), Some("made"));

        run(&mut menu, &mut s, tap(egui::Key::Enter));
        assert_eq!(menu.remotes.browse().started.len(), 1);
        assert!(menu.remotes.is_editing(), "Enter in the name never saves");
        assert_eq!(saved(&s), "/srv/");
        run(&mut menu, &mut s, vec![]);
        fits(&mut menu, "creating");
        assert_eq!(shape(&mut menu), list, "creating moved the form");
        menu.remotes.browse().land(Err(Failure::new(
            "Permission denied: you can't make folders here.",
        )));
        run(&mut menu, &mut s, vec![]);
        fits(&mut menu, "failed");
        assert!(menu.remotes.browse().laid.crumb_alert);
        assert_eq!(shape(&mut menu), list, "a failure moved the form");

        run(&mut menu, &mut s, tap(egui::Key::Escape));
        assert!(menu.remotes.is_editing(), "Esc left only the name field");
        assert!(menu.remotes.browse().list_focused() && !menu.remotes.browse().naming());

        run(&mut menu, &mut s, n_key());
        run(&mut menu, &mut s, vec![egui::Event::Text("made".into())]);
        run(
            &mut menu,
            &mut s,
            vec![ev(egui::Key::Enter, false), ev(egui::Key::Enter, true)],
        );
        assert_eq!(menu.remotes.browse().started.len(), 2);
        menu.remotes.browse().land(Ok(()));
        // Enter still held: its repeat lands nothing and saves nothing.
        run(&mut menu, &mut s, vec![ev(egui::Key::Enter, true)]);
        assert_eq!(menu.remotes.draft_dir(0), Some("/srv/made/"));
        run(&mut menu, &mut s, vec![ev(egui::Key::Enter, true)]);
        assert!(menu.remotes.is_editing(), "the held Enter did not save");
        assert_eq!(saved(&s), "/srv/");
        run(&mut menu, &mut s, tap(egui::Key::Enter));
        assert!(!menu.remotes.is_editing());
        assert_eq!(saved(&s), "/srv/made/");
    }

    /// In the Remotes form, the first Esc in the folder list goes back to
    /// the Directory field and the second cancels the form; typing while
    /// the list has the keyboard lands in the field.
    #[test]
    fn remotes_folder_list_esc_returns_and_typing_lands_in_the_field() {
        use crate::remote::RemoteShell::Posix;
        use crate::remote_browse::{Entry, Listed, State};
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let none = egui::Modifiers::NONE;
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut desk = remote("desk", "andy@desk");
        desk.dirs.insert(r"H:\src\app".into(), "/srv/".into());
        let mut s = Settings {
            remotes: vec![desk],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        let open = |menu: &mut SettingsMenu, s: &mut Settings| {
            frame_at(&ctx, menu, s, Some(size), None);
            frame_at(&ctx, menu, s, Some(size), Some((egui::Key::Enter, none)));
            assert!(menu.remotes.is_editing());
            menu.remotes.browse().put(
                "andy@desk",
                Posix,
                "/srv/",
                State::Listed(Listed {
                    resolved: "/srv".into(),
                    entries: ["app", "data"].iter().map(|n| Entry::dir(n)).collect(),
                    truncated: false,
                }),
            );
            for _ in 0..3 {
                frame_at(&ctx, menu, s, Some(size), Some((egui::Key::Tab, none)));
            }
            frame_at(
                &ctx,
                menu,
                s,
                Some(size),
                Some((egui::Key::ArrowDown, none)),
            );
            frame_at(&ctx, menu, s, Some(size), None);
            assert!(menu.remotes.browse().list_focused());
        };
        open(&mut menu, &mut s);
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            Some(size),
            Some((egui::Key::Escape, none)),
        );
        assert!(
            menu.remotes.is_editing(),
            "the first Esc only left the list"
        );
        assert!(!menu.remotes.browse().list_focused());
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            Some(size),
            Some((egui::Key::Escape, none)),
        );
        assert!(!menu.remotes.is_editing(), "the second cancelled");
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        assert!(
            !menu.remotes.browse().used(),
            "the closed form's listings are dropped"
        );

        open(&mut menu, &mut s);
        let typed = |menu: &mut SettingsMenu, s: &mut Settings, t: &str| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events: vec![egui::Event::Text(t.into())],
                ..Default::default()
            };
            let mut km = Keymap::default();
            let mut theme = crate::theme::Theme::default();
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ctx, |ui| {
                        let r = ui.max_rect();
                        menu.show(ui, r, true, s, &mut km, &mut theme);
                    });
            });
        };
        typed(&mut menu, &mut s, "da");
        assert_eq!(menu.remotes.draft_dir(0), Some("/srv/da"));
        assert!(!menu.remotes.browse().list_focused());
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        typed(&mut menu, &mut s, "ta");
        assert_eq!(
            menu.remotes.draft_dir(0),
            Some("/srv/data"),
            "the field types on"
        );
    }

    /// At the narrowest pane the hint and every row's status line stay one
    /// line tall in each state, so nothing below them moves when the state
    /// changes or a long Test result lands.
    #[test]
    fn remotes_pane_hint_and_status_are_one_line_at_minimum_width() {
        use crate::remotes_pane::LINE_H;
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let none = egui::Modifiers::NONE;
        let one_line = |menu: &SettingsMenu, state: &str| {
            let laid = menu.remotes.laid();
            assert!(
                laid.hint.height() > 0.0 && laid.hint.height() < 16.0,
                "{state}: hint {:?}",
                laid.hint
            );
            assert!(
                laid.hint_fits,
                "{state}: the hint is cut at {:?}",
                laid.pane
            );
            for r in &laid.status {
                assert!(r.height() <= LINE_H + 0.5, "{state}: status {r:?}");
            }
        };
        // No Remotes: the Add-row hint.
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut s = Settings::default();
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        one_line(&menu, "empty");

        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut s = Settings {
            remotes: vec![
                remote("desk", "someone@a-very-long-host-name.example.internal.lan"),
                remote("pi", "pi"),
            ],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        // The longest results there are, on both rows.
        menu.remotes.set_test_result(
            "someone@a-very-long-host-name.example.internal.lan",
            crate::remote_probe::TestOutcome::NeedsLogin,
        );
        menu.remotes.set_test_result(
            "pi",
            crate::remote_probe::TestOutcome::Ok(crate::remote_probe::TestReport {
                shell: crate::remote::RemoteShell::Posix,
                login_shell: Some("/usr/local/bin/a-rather-long-shell-path".into()),
                curl: false,
                bash: false,
                agents: vec!["claude".into()],
            }),
        );
        for (key, state) in [
            (None, "row selected"),
            (Some(egui::Key::Delete), "confirm delete"),
            (Some(egui::Key::Escape), "kept"),
            (Some(egui::Key::ArrowDown), "second row"),
            (Some(egui::Key::ArrowDown), "Add row"),
            (Some(egui::Key::ArrowUp), "back up"),
            (Some(egui::Key::Enter), "edit form"),
        ] {
            frame_at(&ctx, &mut menu, &mut s, Some(size), key.map(|k| (k, none)));
            frame_at(&ctx, &mut menu, &mut s, Some(size), None);
            one_line(&menu, state);
            if key.is_none() {
                assert_eq!(menu.remotes.laid().status.len(), 2);
            }
        }
        assert!(menu.remotes.is_editing());
    }

    /// At the narrowest pane: the install buttons (three on one row, the
    /// widest line there is) and the hint offering I sit inside the pane; a
    /// Test landing and the confirm opening move no row; the confirm's
    /// command block fits the two lines it replaces.
    #[test]
    fn remotes_install_actions_fit_at_minimum_width_and_move_nothing() {
        use crate::remote::RemoteShell;
        use crate::remote_probe::{TestOutcome, TestReport};
        use crate::remotes_pane::LINE_H;
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut s = Settings {
            remotes: vec![
                remote("desk", "layout-install-desk"),
                remote("pi", "layout-install-pi"),
            ],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        // A real reach probe of these made-up hosts answers mid-test and
        // changes each row's status word, and with it the row's width.
        for t in ["layout-install-desk", "layout-install-pi"] {
            crate::remote_probe::put_reach(t, crate::remote_probe::Reach::Reachable);
        }
        menu.remotes.set_install_project(Some("proj"));
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        let untested = menu.remotes.laid().rows.clone();
        let report = |curl, bash, agents: &[&str]| {
            TestOutcome::Ok(TestReport {
                shell: RemoteShell::Posix,
                login_shell: None,
                curl,
                bash,
                agents: agents.iter().map(|a| a.to_string()).collect(),
            })
        };
        menu.remotes
            .set_test_result("layout-install-desk", report(true, true, &[]));
        menu.remotes
            .set_test_result("layout-install-pi", report(true, false, &["claude"]));
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        let laid = menu.remotes.laid();
        assert_eq!(laid.rows, untested, "a Test result moved a row");
        // desk: install claude, codex, grok. pi: install codex.
        assert_eq!(laid.installs.len(), 4, "{:?}", laid.installs);
        for b in &laid.installs {
            assert!(laid.pane.contains_rect(*b), "{b:?} past {:?}", laid.pane);
            assert!(laid.list.contains_rect(*b), "{b:?} past {:?}", laid.list);
        }
        for a in &laid.agents {
            assert!(a.height() <= LINE_H + 0.5, "agents line {a:?}");
            assert!(laid.pane.contains_rect(*a), "{a:?}");
        }
        assert!(laid.hint_fits, "the hint with I is cut");
        assert!(laid.hint.height() < 16.0);
        assert!(laid.agents_fit, "an agents line's words are cut");

        let none = egui::Modifiers::NONE;
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            Some(size),
            Some((egui::Key::I, none)),
        );
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        let laid = menu.remotes.laid();
        let block = laid.confirm.expect("the confirm shows its command");
        assert!(laid.pane.contains_rect(block), "{block:?}");
        assert!(block.height() <= LINE_H * 2.0 + 0.5, "{block:?}");
        assert!(laid.hint_fits, "the confirm hint is cut");
        assert!(!laid.confirm_elided, "the command is cut");
        assert_eq!(laid.rows[1], untested[1], "the confirm moved the next row");
        for (row, (_, controls)) in laid.rows.iter().enumerate() {
            assert!(
                laid.pane.contains_rect(*controls),
                "row {row}: {controls:?}"
            );
        }
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            Some(size),
            Some((egui::Key::Escape, none)),
        );

        // The main sequence: two installed from here, the third still to
        // go (and the same with its install failed), then all three. Like
        // states group, so every piece and button stays inside the pane.
        use crate::remote_install::{Mark, set_mark};
        let desk = "layout-install-desk";
        set_mark(desk, "claude", Mark::Ended);
        set_mark(desk, "codex", Mark::Ended);
        for (agents, grok_ended, buttons) in [
            (&["claude", "codex"][..], false, 1),
            (&["claude", "codex"][..], true, 1),
            (&["claude", "codex", "grok"][..], true, 0),
        ] {
            if grok_ended {
                set_mark(desk, "grok", Mark::Ended);
            }
            menu.remotes
                .set_test_result(desk, report(true, true, agents));
            frame_at(&ctx, &mut menu, &mut s, Some(size), None);
            frame_at(&ctx, &mut menu, &mut s, Some(size), None);
            let laid = menu.remotes.laid();
            // pi still has its one install button.
            assert_eq!(laid.installs.len(), buttons + 1, "{agents:?}");
            for b in &laid.installs {
                assert!(
                    laid.pane.contains_rect(*b),
                    "{agents:?} {grok_ended}: {b:?} past {:?} {:?}",
                    laid.pane,
                    laid.agents
                );
            }
            for a in &laid.agents {
                assert!(laid.pane.contains_rect(*a), "{agents:?}: {a:?}");
                assert!(a.height() <= LINE_H + 0.5, "{agents:?}: {a:?}");
            }
            assert!(laid.agents_fit, "{agents:?}: words cut");
            assert_eq!(laid.rows, untested, "{agents:?} moved a row");
        }
    }

    /// At the narrowest pane, every combination of the three agents' states
    /// draws one agents line whose words and buttons are all inside the
    /// pane: no word cut, no button clipped past the edge.
    #[test]
    fn every_agents_line_fits_at_minimum_width() {
        use crate::remote::Prereq;
        use crate::remote::RemoteShell;
        use crate::remote_install::{AgentState, Gate, Piece, agents_line};
        use crate::remotes_pane::LINE_H;
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let target = "agents-line-fit";
        let mut s = Settings {
            remotes: vec![remote(&"m".repeat(crate::remote::MAX_NAME), target)],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        let each = |p: &'static str| {
            let ready = |after_install| AgentState::Missing {
                gate: Gate::Ready(crate::remote::install_for(p, RemoteShell::Posix).unwrap()),
                after_install,
            };
            [
                AgentState::Found,
                AgentState::Installed,
                AgentState::Installing,
                AgentState::Checking,
                ready(false),
                ready(true),
                AgentState::Missing {
                    gate: Gate::Needs(vec![Prereq::Curl, Prereq::Bash]),
                    after_install: false,
                },
                AgentState::Missing {
                    gate: Gate::ByHand,
                    after_install: false,
                },
            ]
        };
        let mut misfits = Vec::new();
        let mut combos = 0;
        for a in each("claude") {
            for b in each("codex") {
                for c in each("grok") {
                    combos += 1;
                    let states = vec![("claude", a.clone()), ("codex", b.clone()), ("grok", c)];
                    let words: Vec<String> = agents_line("desk", RemoteShell::Posix, &states)
                        .iter()
                        .map(|p| match p {
                            Piece::Text { text, .. } => text.clone(),
                            Piece::Install { label, .. } => format!("[{label}]"),
                        })
                        .collect();
                    menu.remotes.force_agent_states(target, states);
                    frame_at(&ctx, &mut menu, &mut s, Some(size), None);
                    let laid = menu.remotes.laid();
                    let line = laid.agents[0];
                    let inside = laid.pane.contains_rect(line)
                        && line.height() <= LINE_H + 0.5
                        && laid
                            .installs
                            .iter()
                            .all(|b| laid.pane.contains_rect(*b) && laid.list.contains_rect(*b));
                    if !(inside && laid.agents_fit) {
                        misfits.push((line.right() - laid.pane.right(), words.join(" · ")));
                    }
                }
            }
        }
        assert_eq!(combos, 8 * 8 * 8);
        misfits.sort_by(|x, y| y.0.total_cmp(&x.0));
        assert!(
            misfits.is_empty(),
            "{} lines overflow the pane, widest first:
{}",
            misfits.len(),
            misfits
                .iter()
                .map(|(over, w)| format!("{over:+.0}px  {w}"))
                .collect::<Vec<_>>()
                .join(
                    "
"
                )
        );
    }

    /// At the narrowest pane, every install command in the table shows
    /// whole in the confirm's two lines (none is cut), on every shell kind,
    /// and the question never loses the Remote's name, even a longest one.
    #[test]
    fn every_install_command_and_the_remote_name_fit_the_confirm() {
        use crate::remote::RemoteShell;
        use crate::remote_probe::{TestOutcome, TestReport};
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let ctx = egui::Context::default();
        let long = "m".repeat(crate::remote::MAX_NAME);
        let shells = [
            RemoteShell::Posix,
            RemoteShell::Pwsh,
            RemoteShell::WindowsPowerShell,
        ];
        for name in ["desk", "build-server-in-the-garage-rack2", long.as_str()] {
            for (k, shell) in shells.into_iter().enumerate() {
                let target = format!("confirm-fit-{k}-{}", name.len());
                let mut menu = SettingsMenu::new();
                let mut s = Settings {
                    remotes: vec![crate::remote::Remote {
                        name: name.into(),
                        target: target.clone(),
                        shell,
                        dirs: Default::default(),
                    }],
                    ..Default::default()
                };
                menu.select_pane(Pane::Remotes);
                menu.in_rail = false;
                menu.remotes.set_install_project(Some("proj"));
                menu.remotes.set_test_result(
                    &target,
                    TestOutcome::Ok(TestReport {
                        shell,
                        login_shell: None,
                        curl: true,
                        bash: true,
                        agents: vec![],
                    }),
                );
                frame_at(&ctx, &mut menu, &mut s, Some(size), None);
                let none = egui::Modifiers::NONE;
                let mut seen = Vec::new();
                for key in [egui::Key::I, egui::Key::ArrowRight, egui::Key::ArrowRight] {
                    frame_at(&ctx, &mut menu, &mut s, Some(size), Some((key, none)));
                    frame_at(&ctx, &mut menu, &mut s, Some(size), None);
                    let laid = menu.remotes.laid();
                    assert!(laid.confirm.is_some(), "{name} {shell:?}: no confirm");
                    assert!(!laid.confirm_elided, "{name} {shell:?}: a command is cut");
                    let (q, fits) = laid.question.clone().expect("a question");
                    assert!(q.contains(name), "{shell:?}: {q:?} lost the name");
                    assert!(fits, "{shell:?}: {q:?} is cut");
                    let (text, controls) = laid.rows[0];
                    assert!(laid.pane.contains_rect(controls), "{name}: {controls:?}");
                    assert!(
                        text.max.x <= controls.min.x,
                        "{name}: the row's text {text:?} runs under {controls:?}"
                    );
                    seen.push(q);
                }
                assert_eq!(seen.len(), 3);
            }
        }
    }

    /// A refused save at the smallest pane: the error shows and the save and
    /// cancel buttons are still on screen, not under the list's scroll edge.
    #[test]
    fn remotes_form_buttons_stay_on_screen_with_an_error() {
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut desk = remote("desk", "desk");
        for i in 0..4 {
            desk.dirs
                .insert(format!(r"H:\src\project-{i}"), format!("/srv/project-{i}"));
        }
        // A directory no save accepts, so Enter is refused.
        desk.dirs.insert(r"H:\src\zz".into(), "/a\tb".into());
        let mut s = Settings {
            remotes: vec![desk],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        let size = egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y);
        let enter = Some((egui::Key::Enter, egui::Modifiers::NONE));
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        frame_at(&ctx, &mut menu, &mut s, Some(size), enter);
        assert!(menu.remotes.is_editing());
        frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        frame_at(&ctx, &mut menu, &mut s, Some(size), enter);
        for _ in 0..3 {
            frame_at(&ctx, &mut menu, &mut s, Some(size), None);
        }
        assert!(menu.remotes.is_editing(), "the save was refused");
        let laid = menu.remotes.laid();
        let error = laid.error.expect("the error shows");
        assert!(
            laid.list.contains_rect(error),
            "{error:?} not in {:?}",
            laid.list
        );
        let buttons = &laid.form[laid.form.len() - 2..];
        for b in buttons {
            assert!(laid.list.contains_rect(*b), "{b:?} not in {:?}", laid.list);
            assert!(laid.pane.contains_rect(*b), "{b:?}");
        }
    }

    /// At the narrowest pane: with a directory focused the hint offers its
    /// clear key on one line, and the key forgets that row without the
    /// TextEdit that takes its place deleting a word of the next one.
    #[test]
    fn ctrl_backspace_clears_the_focused_project_directory() {
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut desk = remote("desk", "desk");
        desk.dirs.insert(r"H:\src\app".into(), "/srv/app".into());
        desk.dirs
            .insert(r"H:\src\tool".into(), "/srv/the tool".into());
        let mut s = Settings {
            remotes: vec![desk],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        let size = Some(egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y));
        let none = egui::Modifiers::NONE;
        frame_at(&ctx, &mut menu, &mut s, size, None);
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            size,
            Some((egui::Key::Enter, none)),
        );
        for _ in 0..3 {
            frame_at(&ctx, &mut menu, &mut s, size, Some((egui::Key::Tab, none)));
        }
        frame_at(&ctx, &mut menu, &mut s, size, None);
        let laid = menu.remotes.laid();
        assert!(laid.hint_fits, "the hint is cut at {:?}", laid.pane);
        assert!(laid.dirs_note_fits);
        let ctrl = egui::Modifiers::CTRL;
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            size,
            Some((egui::Key::Backspace, ctrl)),
        );
        frame_at(&ctx, &mut menu, &mut s, size, None);
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            size,
            Some((egui::Key::Enter, none)),
        );
        assert!(!menu.remotes.is_editing(), "saved");
        let dirs = &s.remotes[0].dirs;
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        assert_eq!(dirs[r"H:\src\tool"], "/srv/the tool", "untouched");
    }

    #[test]
    fn the_no_directories_line_fits_at_minimum_width() {
        let ctx = egui::Context::default();
        let mut menu = SettingsMenu::new();
        let mut s = Settings {
            remotes: vec![remote("desk", "desk")],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        let size = Some(egui::vec2(RAIL_W + PANE_MIN_W, SettingsMenu::min_size().y));
        frame_at(&ctx, &mut menu, &mut s, size, None);
        let enter = Some((egui::Key::Enter, egui::Modifiers::NONE));
        frame_at(&ctx, &mut menu, &mut s, size, enter);
        frame_at(&ctx, &mut menu, &mut s, size, None);
        assert!(menu.remotes.is_editing());
        assert!(menu.remotes.laid().dirs_note_fits);
    }

    #[test]
    fn ctrl_d_does_not_ask_to_delete_a_remote() {
        let mut menu = SettingsMenu::new();
        let mut s = Settings {
            remotes: vec![remote("desk", "desk")],
            ..Default::default()
        };
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        let ctx = egui::Context::default();
        frame_at(&ctx, &mut menu, &mut s, None, None);
        let ctrl = egui::Modifiers::CTRL;
        frame_at(&ctx, &mut menu, &mut s, None, Some((egui::Key::D, ctrl)));
        // Not confirming, so Enter edits rather than deletes.
        frame_at(
            &ctx,
            &mut menu,
            &mut s,
            None,
            Some((egui::Key::Enter, egui::Modifiers::NONE)),
        );
        assert_eq!(s.remotes.len(), 1);
        assert!(menu.remotes.is_editing());
    }

    #[test]
    fn remotes_pane_keeps_tab_while_its_form_is_open() {
        let mut menu = SettingsMenu::new();
        let mut s = Settings::default();
        menu.select_pane(Pane::Remotes);
        menu.in_rail = false;
        frame_with_key(&mut menu, &mut s, egui::Key::A, false);
        assert!(menu.remotes.is_editing(), "A opens the add form");
        frame_with_key(&mut menu, &mut s, egui::Key::Tab, false);
        assert!(!menu.in_rail, "Tab moves between the form's fields");
        frame_with_key(&mut menu, &mut s, egui::Key::Escape, false);
        assert!(!menu.remotes.is_editing());
        frame_with_key(&mut menu, &mut s, egui::Key::Tab, false);
        assert!(menu.in_rail, "browsing, Tab backs out to the rail");
        assert!(s.remotes.is_empty());
    }

    #[test]
    fn remotes_is_a_custom_body_pane() {
        assert!(Pane::ALL.contains(&Pane::Remotes));
        assert!(rows(Pane::Remotes).is_empty());
    }

    #[test]
    fn appearance_is_a_custom_body_pane() {
        assert!(Pane::ALL.contains(&Pane::Appearance));
        assert!(rows(Pane::Appearance).is_empty());
        assert_eq!(Pane::Appearance.label(), "Appearance");
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
    fn naming_provider_choice_selects_a_valid_initial_model() {
        let mut s = Settings::default();
        assert_eq!(s.title_provider, crate::config::NamingProvider::Codex);
        assert_eq!(s.title_model, "gpt-5.6-luna");
        assert!(adjust(Field::TitleProvider, Adjust::Inc, &mut s));
        assert_eq!(s.title_provider, crate::config::NamingProvider::Claude);
        assert_eq!(s.title_model, "");
        s.title_model = "claude-haiku-4-5".into();
        assert!(adjust(Field::TitleProvider, Adjust::Inc, &mut s));
        assert_eq!(s.title_provider, crate::config::NamingProvider::Grok);
        assert_eq!(s.title_model, "");
        assert!(adjust(Field::TitleProvider, Adjust::Inc, &mut s));
        assert_eq!(s.title_provider, crate::config::NamingProvider::Codex);
        assert_eq!(s.title_model, "gpt-5.6-luna");
    }

    #[test]
    fn agents_pane_discloses_cross_provider_prompt_sharing() {
        let rows = rows(Pane::Agents);
        assert!(rows.iter().any(|r| r.field == Field::AutoNameAgentSessions));
        assert!(rows.iter().any(|r| r.field == Field::TitleProvider));
        assert!(rows.iter().any(|r| r.field == Field::TitleModel));
        let spec = rows
            .iter()
            .find(|r| r.field == Field::AutoNameAgentSessions)
            .unwrap();
        let mut settings = Settings {
            auto_name_agent_sessions: true,
            ..Default::default()
        };
        let disclosure = description(spec, &settings);
        assert!(disclosure.contains("3,800"));
        assert!(disclosure.contains("prior title"));
        assert!(disclosure.contains("allowance"));
        assert!(disclosure.contains("any agent"));
        settings.title_provider = crate::config::NamingProvider::Claude;
        assert!(description(spec, &settings).contains("subscription usage"));
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

    #[test]
    fn check_updates_now_action_requests_a_check() {
        let mut m = SettingsMenu::new();
        assert!(matches!(
            m.do_action(Field::CheckUpdatesNow, &egui::Context::default()),
            MenuOutcome::CheckUpdatesNow
        ));
    }

    #[test]
    fn new_project_layout_row_names_the_slot_and_resets_to_single_terminal() {
        use crate::layout_library::LayoutStore;
        use crate::project_layout::{
            Arrangement, ItemKind, ItemRef, LayoutDefinition, TreeSpec, WindowSpec,
        };
        let row = rows(Pane::WindowManager)
            .iter()
            .find(|r| r.field == Field::NewProjectLayout)
            .expect("a Window Manager row");
        assert_eq!(row.kind, Kind::Action);

        let tmp = tempfile::tempdir().unwrap();
        let store = LayoutStore::at(tmp.path());
        let mut m = SettingsMenu::new();
        m.layout_store = Some(store.clone());
        assert_eq!(
            m.new_project_layout_value(),
            ("Single terminal".into(), false)
        );
        m.do_action(Field::NewProjectLayout, &egui::Context::default());
        assert!(
            !tmp.path()
                .join(crate::layout_library::LIBRARY_FILE)
                .exists(),
            "Reset on Single terminal writes nothing"
        );

        let quad = LayoutDefinition {
            id: "q".into(),
            name: "Quad".into(),
            items: vec![ItemRef::new(1, ItemKind::Terminal)],
            arrangement: Arrangement {
                windows: vec![WindowSpec::tiled(vec![1], 0)],
                tree: Some(TreeSpec::Leaf { window: 0 }),
            },
            origin: None,
        };
        store.set_new_project_layout(Some(&quad)).unwrap();
        assert_eq!(
            m.new_project_layout_value(),
            ("Single terminal".into(), false),
            "read once, never per frame"
        );
        m.select_pane(Pane::WindowManager);
        assert_eq!(
            m.new_project_layout_value(),
            ("\u{201c}Quad\u{201d}".into(), true),
            "re-read when the pane is shown again"
        );

        let ctx = egui::Context::default();
        assert!(matches!(
            m.do_action(Field::NewProjectLayout, &ctx),
            MenuOutcome::Pending
        ));
        assert_eq!(store.new_project_layout().unwrap(), None, "reset on disk");
        assert_eq!(
            m.new_project_layout_value(),
            ("Single terminal".into(), false)
        );
    }

    #[test]
    fn check_updates_now_outranks_changed_but_not_close() {
        let mut o = MenuOutcome::Changed;
        bump(&mut o, MenuOutcome::CheckUpdatesNow);
        assert!(matches!(o, MenuOutcome::CheckUpdatesNow));
        bump(&mut o, MenuOutcome::Close);
        assert!(matches!(o, MenuOutcome::Close));
    }
}
