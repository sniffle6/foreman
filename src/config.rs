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

/// Load a JSON config file from the foreman config dir. Tolerant by design:
/// a missing file, an unreadable file, or invalid JSON all fall back to
/// `T::default()` (with a stderr warning for the non-missing cases). Never panics
/// — a bad config must not take the app down.
pub fn load_json<T: DeserializeOwned + Default>(file: &str) -> T {
    let Some(dir) = config_dir() else {
        return T::default();
    };
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

/// Write a JSON config file atomically: serialize, write a sibling `.tmp`, then
/// rename it over the target. A crash mid-write leaves the previous good file
/// intact (a bare `write` could truncate it). Errors are returned, never panicked,
/// so the caller can surface them.
pub fn save_json<T: Serialize>(file: &str, value: &T) -> Result<(), String> {
    let dir = config_dir()
        .ok_or_else(|| "APPDATA is not set; cannot locate the config directory".to_string())?;
    let path = dir.join(file);
    let tmp = dir.join(format!("{file}.tmp"));
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| format!("could not serialize {file}: {e}"))?;
    std::fs::write(&tmp, json).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("could not commit {}: {e}", path.display()))?;
    Ok(())
}

/// Persisted app settings (`%APPDATA%\foreman\settings.json`). Flat by design:
/// `#[serde(default)]` means a missing file, a missing field, or extra fields
/// written by a newer foreman all load cleanly — so adding a setting later never
/// breaks an existing file. Add new fields here as more settings become persisted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Global terminal text size, shared by every pane.
    pub font_size: f32,
    /// Task-manager panel collapsed to the icon rail.
    pub panel_collapsed: bool,
    /// Task-manager panel width when expanded (px).
    pub panel_width: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            panel_collapsed: false,
            panel_width: crate::panel::PANEL_W,
        }
    }
}

impl Settings {
    /// Load from `settings.json`, falling back to defaults on any problem.
    pub fn load() -> Self {
        load_json(SETTINGS_FILE)
    }

    /// Persist atomically to `settings.json`.
    pub fn save(&self) -> Result<(), String> {
        save_json(SETTINGS_FILE, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // An empty object (or a file written before `font_size` existed) loads as
        // the full default — the forward/back-compat property the layer promises.
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.font_size, DEFAULT_FONT_SIZE);
        assert!(!s.panel_collapsed);
        assert_eq!(s.panel_width, crate::panel::PANEL_W);
    }

    #[test]
    fn known_field_round_trips() {
        let s: Settings = serde_json::from_str(r#"{"font_size": 20.0}"#).unwrap();
        assert_eq!(s.font_size, 20.0);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.font_size, 20.0);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // A newer foreman's extra keys must not fail an older parse.
        let s: Settings =
            serde_json::from_str(r#"{"font_size": 15.0, "future_setting": true}"#).unwrap();
        assert_eq!(s.font_size, 15.0);
    }
}
