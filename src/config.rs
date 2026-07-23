//! Reusable settings persistence: a tolerant JSON config store under
//! `%APPDATA%\foreman`. Defaults-in-code, atomic writes, corruption-tolerant
//! loads. This is the canonical layer for persisted *settings* — reuse
//! `load_json`/`save_json` for any new flat config rather than hand-rolling the
//! dir-resolve + serde + fallback dance again.
//!
//! (`keybindings.json` in `keymap.rs` is the older, hand-rolled precedent; the
//! append-only chat log in `docs/chat-persistence.md` is a *different* problem —
//! a growing event log, not a settings bag — and intentionally does not use this.)

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;

/// Default terminal text size, in egui points. The size every pane starts at and
/// what Ctrl+0 resets to.
pub const DEFAULT_FONT_SIZE: f32 = 13.0;
/// The active theme a fresh install starts on. MUST equal `appearance::BUILTIN`
/// (the built-in theme's name) so a default `Settings` resolves to the built-in,
/// never a missing user-theme file.
pub const DEFAULT_THEME: &str = "Foreman Warm";
/// Clamp bounds for Ctrl+Scroll zoom — small enough to stay legible, large enough
/// to be useful, and bounded so a wild scroll can't make a pane unusable.
pub const MIN_FONT_SIZE: f32 = 6.0;
pub const MAX_FONT_SIZE: f32 = 40.0;
/// Points added/removed per whole wheel notch while Ctrl is held.
pub const FONT_ZOOM_STEP: f32 = 1.0;

const SETTINGS_FILE: &str = "settings.json";

/// Resolve (creating if needed) the foreman config directory: `%APPDATA%\foreman`.
/// `None` when `APPDATA` is unset (extremely unusual on Windows) or the directory
/// can't be created — callers then fall back to in-code defaults.
pub fn config_dir() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    let dir = PathBuf::from(appdata).join("foreman");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Resolve (creating if needed) the per-name theme directory:
/// `%APPDATA%\foreman\themes`. User themes are stored one JSON file per name
/// here; the built-in is code-only and never written. Same `None`-on-failure
/// contract as [`config_dir`] — callers fall back to in-code defaults.
pub fn themes_dir() -> Option<PathBuf> {
    let dir = config_dir()?.join("themes");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Load a JSON config file from `dir`. Tolerant by design: a missing file, an
/// unreadable file, or invalid JSON all fall back to `T::default()` (with a
/// stderr warning for the non-missing cases). Never panics — a bad config must
/// not take the app down. [`load_json`] is the `config_dir()` flavor.
pub fn load_json_from<T: DeserializeOwned + Default>(dir: &std::path::Path, file: &str) -> T {
    let path = dir.join(file);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            eprintln!(
                "foreman: could not read {}: {} — using defaults",
                path.display(),
                e
            );
            return T::default();
        }
    };
    match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "foreman: {} is invalid JSON: {} — using defaults",
                path.display(),
                e
            );
            T::default()
        }
    }
}

/// Write a JSON file atomically into `dir`: serialize, write a sibling `.tmp`,
/// then rename it over the target. A crash mid-write leaves the previous good
/// file intact (a bare `write` could truncate it). Errors are returned, never
/// panicked. [`save_json`] is the `config_dir()` flavor.
pub fn save_json_in<T: Serialize>(
    dir: &std::path::Path,
    file: &str,
    value: &T,
) -> Result<(), String> {
    let path = dir.join(file);
    let tmp = dir.join(format!("{file}.tmp"));
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| format!("could not serialize {file}: {e}"))?;
    std::fs::write(&tmp, json).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("could not commit {}: {e}", path.display()))?;
    Ok(())
}

/// Load a JSON config file from the foreman config dir. Tolerant by design:
/// a missing file, an unreadable file, or invalid JSON all fall back to
/// `T::default()`. Never panics — a bad config must not take the app down.
pub fn load_json<T: DeserializeOwned + Default>(file: &str) -> T {
    let Some(dir) = config_dir() else {
        return T::default();
    };
    load_json_from(&dir, file)
}

/// Write a JSON config file atomically into the foreman config dir. Errors are
/// returned, never panicked, so the caller can surface them.
pub fn save_json<T: Serialize>(file: &str, value: &T) -> Result<(), String> {
    let dir = config_dir()
        .ok_or_else(|| "APPDATA is not set; cannot locate the config directory".to_string())?;
    save_json_in(&dir, file, value)
}

/// What a bare "new terminal" runs. Custom command lines are a later phase
/// (Shell is a Copy enum; a String variant ripples through every spawn site).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultShell {
    PowerShell,
    Cmd,
    Sh,
}

impl DefaultShell {
    pub fn to_shell(self) -> crate::terminal::Shell {
        match self {
            DefaultShell::PowerShell => crate::terminal::Shell::PowerShell,
            DefaultShell::Cmd => crate::terminal::Shell::Cmd,
            DefaultShell::Sh => crate::terminal::Shell::Bash,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            DefaultShell::PowerShell => "PowerShell",
            DefaultShell::Cmd => "CMD",
            DefaultShell::Sh => "SH",
        }
    }
}

/// Persisted app settings (`%APPDATA%\foreman\settings.json`). Flat by design:
/// `#[serde(default)]` means a missing file, a missing field, or extra fields
/// written by a newer foreman all load cleanly — so adding a setting later never
/// breaks an existing file. Add new fields here as more settings become persisted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Global terminal text size, shared by every pane.
    pub font_size: f32,
    /// Task-manager panel collapsed to the icon rail.
    pub panel_collapsed: bool,
    /// Task-manager panel width when expanded (px).
    pub panel_width: f32,
    /// Edge the task-manager panel is docked against. Survives minimize-all
    /// and restart; only changes when the user moves the panel in the tree.
    pub panel_dock: crate::wm::Dir,
    /// Master switch for Bell attention (the visual pulse; any later sound or
    /// push notification must honor the same key). File-only in v1 — no
    /// settings UI, no leader chord. Missing key = on.
    pub bell: bool,
    // -- terminal --
    /// What a bare new terminal spawns.
    pub default_shell: DefaultShell,
    /// History kept per pane, in lines.
    pub scrollback_lines: u32,
    /// Lines scrolled per wheel notch.
    pub scroll_speed: f32,
    /// Font points added/removed per Ctrl+Scroll notch.
    pub zoom_step: f32,
    /// Selection lands on the clipboard immediately.
    pub copy_on_select: bool,
    /// Confirm before pasting text containing newlines.
    pub paste_warn_multiline: bool,
    // -- bell & alerts --
    /// Seconds for one full breathe of the amber bell pulse.
    pub bell_period: f32,
    /// How long a toast notification lingers, in seconds.
    pub toast_secs: f32,
    // -- window manager --
    /// New terminals open floating instead of joining the tiling tree.
    pub new_windows_float: bool,
    /// Hovering a pane focuses it without a click.
    pub focus_follows_mouse: bool,
    /// Slight darkening on everything but the focused terminal.
    pub dim_unfocused: bool,
    // -- agents --
    /// Write foreman-dispatch/foreman-chat skills into Claude & Codex dirs on launch.
    pub install_skills: bool,
    /// Seconds since last heard-from before a Crew member shows its age in amber.
    pub crew_stale_secs: u32,
    /// Default quiescence wait for `foreman send` when the caller omits one.
    pub send_settle_ms: u64,
    // -- startup --
    /// Reopen last session's projects and layout on launch.
    pub restore_workspace: bool,
    /// Where the directory picker starts browsing (blank = home).
    pub default_project_dir: String,
    /// Check GitHub releases for updates in the background on launch.
    pub update_check: bool,
    // -- appearance --
    /// Active color theme by name: the built-in `"Foreman Warm"` or a user
    /// theme file stem under `themes_dir()`. The Appearance pane sets this; the
    /// App resolves it to the live `Theme`.
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            panel_collapsed: false,
            panel_width: crate::panel::PANEL_W,
            panel_dock: crate::wm::Dir::Right,
            bell: true,
            default_shell: DefaultShell::PowerShell,
            scrollback_lines: 10_000,
            scroll_speed: crate::input::LINES_PER_NOTCH as f32,
            zoom_step: FONT_ZOOM_STEP,
            copy_on_select: false,
            paste_warn_multiline: true,
            bell_period: crate::theme::BELL_PERIOD as f32,
            toast_secs: 6.0,
            new_windows_float: false,
            focus_follows_mouse: false,
            dim_unfocused: false,
            install_skills: true,
            crew_stale_secs: 300,
            send_settle_ms: 120,
            restore_workspace: true,
            default_project_dir: String::new(),
            update_check: true,
            theme: DEFAULT_THEME.into(),
        }
    }
}

impl Settings {
    /// Load from `settings.json`, falling back to defaults on any problem.
    pub fn load() -> Self {
        let mut s: Self = load_json(SETTINGS_FILE);
        s.sanitize();
        s
    }

    /// Persist atomically to `settings.json`.
    pub fn save(&self) -> Result<(), String> {
        save_json(SETTINGS_FILE, self)
    }

    /// Clamp every numeric field to its legal range. Runs on load so a
    /// hand-edited file can't violate invariants (notably: settle must stay
    /// far below control.rs REPLY_TIMEOUT via wm.rs MAX_SETTLE_MS).
    pub fn sanitize(&mut self) {
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.scrollback_lines = self.scrollback_lines.clamp(100, 1_000_000);
        self.scroll_speed = self.scroll_speed.clamp(1.0, 30.0);
        self.zoom_step = self.zoom_step.clamp(0.25, 5.0);
        self.bell_period = self.bell_period.clamp(0.4, 5.0);
        self.toast_secs = self.toast_secs.clamp(1.0, 30.0);
        self.crew_stale_secs = self.crew_stale_secs.clamp(30, 3600);
        self.send_settle_ms = self.send_settle_ms.min(2000);
        // Empty theme name self-heals to the built-in (full unknown-name
        // validation against the theme list lands with the Duplicate wiring).
        if self.theme.trim().is_empty() {
            self.theme = DEFAULT_THEME.into();
        }
    }
}

/// Seed the frame's settings into egui context data so deep consumers
/// (terminal.rs, wm.rs, chat_view.rs) can read them without threading a
/// parameter through every call. Same pattern as terminal::font_size.
pub fn seed_live(ctx: &eframe::egui::Context, s: &Settings) {
    let arc = std::sync::Arc::new(s.clone());
    ctx.data_mut(|d| d.insert_temp(eframe::egui::Id::new("foreman::settings"), arc));
}

/// The settings seeded this frame (defaults before the app's first seed).
pub fn live(ctx: &eframe::egui::Context) -> std::sync::Arc<Settings> {
    ctx.data_mut(|d| d.get_temp(eframe::egui::Id::new("foreman::settings")))
        .unwrap_or_else(|| std::sync::Arc::new(Settings::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_helpers_round_trip_in_an_arbitrary_dir() {
        // The dir-parameterized helpers write and read from any directory (used
        // for per-name theme files under themes_dir()), keeping the atomic
        // tmp+rename + tolerant-load contract of the config_dir() flavor.
        #[derive(Serialize, Deserialize, Default, PartialEq, Debug)]
        struct Sample {
            n: u32,
            s: String,
        }
        // Fixed unique subdir name — no time/random in the test body.
        let dir = std::env::temp_dir().join("foreman-test-json-helpers-16");
        std::fs::create_dir_all(&dir).unwrap();
        let file = "sample.json";
        let v = Sample {
            n: 42,
            s: "hi".into(),
        };
        save_json_in(&dir, file, &v).unwrap();
        let back: Sample = load_json_from(&dir, file);
        assert_eq!(back, v);
        // A missing file loads as the default (tolerant-load contract).
        let missing: Sample = load_json_from(&dir, "does-not-exist.json");
        assert_eq!(missing, Sample::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // An empty object (or a file written before `font_size` existed) loads as
        // the full default — the forward/back-compat property the layer promises.
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.font_size, DEFAULT_FONT_SIZE);
        assert!(!s.panel_collapsed);
        assert_eq!(s.panel_width, crate::panel::PANEL_W);
        assert_eq!(s.panel_dock, crate::wm::Dir::Right);
    }

    #[test]
    fn known_field_round_trips() {
        let s: Settings = serde_json::from_str(r#"{"font_size": 20.0}"#).unwrap();
        assert_eq!(s.font_size, 20.0);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.font_size, 20.0);
    }

    #[test]
    fn bell_defaults_on_and_round_trips() {
        // Missing key = on (the #[serde(default)] contract for new fields).
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.bell, "missing bell key must mean on");
        // Explicit false parses and survives a round trip.
        let s: Settings = serde_json::from_str(r#"{"bell": false}"#).unwrap();
        assert!(!s.bell);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(!back.bell);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // A newer foreman's extra keys must not fail an older parse.
        let s: Settings =
            serde_json::from_str(r#"{"font_size": 15.0, "future_setting": true}"#).unwrap();
        assert_eq!(s.font_size, 15.0);
    }

    #[test]
    fn new_fields_default_when_missing_from_old_file() {
        // An old settings.json (font_size only) must load with every new field
        // at its default — the serde(default) contract.
        let s: Settings = serde_json::from_str(r#"{ "font_size": 15.0 }"#).unwrap();
        assert_eq!(s.default_shell, DefaultShell::PowerShell);
        assert_eq!(s.scrollback_lines, 10_000);
        assert_eq!(s.scroll_speed, 3.0);
        assert_eq!(s.zoom_step, 1.0);
        assert!(!s.copy_on_select);
        assert!(s.paste_warn_multiline);
        assert_eq!(s.bell_period, 1.2);
        assert_eq!(s.toast_secs, 6.0);
        assert!(!s.new_windows_float);
        assert!(!s.focus_follows_mouse);
        assert!(!s.dim_unfocused);
        assert!(s.install_skills);
        assert_eq!(s.crew_stale_secs, 300);
        assert_eq!(s.send_settle_ms, 120);
        assert!(s.restore_workspace);
        assert_eq!(s.default_project_dir, "");
        assert!(s.update_check);
        assert_eq!(s.theme, "Foreman Warm");
    }

    #[test]
    fn theme_defaults_to_foreman_warm_and_sanitizes_empty() {
        assert_eq!(Settings::default().theme, "Foreman Warm");
        // The default name must match the built-in theme's name so a fresh
        // Settings resolves to the built-in (never a missing user file).
        assert_eq!(DEFAULT_THEME, crate::appearance::BUILTIN);
        let mut s = Settings::default();
        s.theme = String::new();
        s.sanitize();
        assert_eq!(s.theme, "Foreman Warm"); // empty self-heals
    }

    #[test]
    fn sanitize_clamps_hand_edited_values() {
        let mut s = Settings::default();
        s.send_settle_ms = 999_999; // must never approach MAX_SETTLE_MS (4000)
        s.scrollback_lines = 7;
        s.scroll_speed = 0.0;
        s.zoom_step = 100.0;
        s.bell_period = 0.0;
        s.toast_secs = 0.0;
        s.crew_stale_secs = 1;
        s.sanitize();
        assert_eq!(s.send_settle_ms, 2000);
        assert_eq!(s.scrollback_lines, 100);
        assert_eq!(s.scroll_speed, 1.0);
        assert_eq!(s.zoom_step, 5.0);
        assert_eq!(s.bell_period, 0.4);
        assert_eq!(s.toast_secs, 1.0);
        assert_eq!(s.crew_stale_secs, 30);
    }

    #[test]
    fn default_shell_maps_to_terminal_shell() {
        use crate::terminal::Shell;
        assert_eq!(DefaultShell::PowerShell.to_shell(), Shell::PowerShell);
        assert_eq!(DefaultShell::Cmd.to_shell(), Shell::Cmd);
        assert_eq!(DefaultShell::Sh.to_shell(), Shell::Bash);
    }

    #[test]
    fn settings_roundtrip_preserves_new_fields() {
        let mut s = Settings::default();
        s.default_shell = DefaultShell::Cmd;
        s.copy_on_select = true;
        s.default_project_dir = "H:\\claude code".into();
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.default_shell, DefaultShell::Cmd);
        assert!(back.copy_on_select);
        assert_eq!(back.default_project_dir, "H:\\claude code");
    }
}
