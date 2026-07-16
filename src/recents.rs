//! Recent-projects MRU, persisted to `%APPDATA%\foreman\recents.json` via
//! config.rs's tolerant loader (missing/corrupt file → empty list). A separate
//! file from settings.json on purpose: settings are *preferences* written on a
//! zoom debounce, this is *state* written on project opens — keeping them apart
//! avoids interleaved writes. Spec:
//! docs/superpowers/specs/2026-07-08-landing-recent-projects-design.md

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const RECENTS_FILE: &str = "recents.json";
/// Store 5, show 5 — one number (grug-review amendment; no ghost entries).
pub const MAX_RECENTS: usize = 5;

/// One remembered open. `kind` is a plain string ("claude" | "codex" | "grok" |
/// "terminal") — deliberately NOT the landing's provisional `SessionKind`, so
/// the disk format survives phase-2 renaming that enum, an unknown kind can
/// never fail the parse, and this module doesn't depend on a UI module.
/// Unknown strings degrade to Terminal at the landing edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub kind: String,
}

/// The MRU list. Mutation is pure (`push`); `record` adds persistence.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Recents {
    entries: Vec<RecentEntry>,
}

impl Recents {
    /// Load once at startup; corruption-tolerant per config.rs.
    pub fn load() -> Self {
        crate::config::load_json(RECENTS_FILE)
    }

    /// Record an open and persist. Best-effort: a failed save is logged and
    /// never blocks the open that triggered it.
    pub fn record(&mut self, path: PathBuf, kind: &str) {
        self.push(path, kind);
        if let Err(e) = crate::config::save_json(RECENTS_FILE, self) {
            eprintln!("foreman: could not save recents: {e}");
        }
    }

    /// Pure MRU mutation (split from `record` so tests never touch a disk):
    /// dedup by case-folded path, insert at front, cap.
    fn push(&mut self, path: PathBuf, kind: &str) {
        let key = fold(&path);
        self.entries.retain(|e| fold(&e.path) != key);
        self.entries.insert(
            0,
            RecentEntry {
                path,
                kind: kind.to_string(),
            },
        );
        self.entries.truncate(MAX_RECENTS);
    }

    /// Most-recent-first. Callers filter for display (e.g. missing dirs) —
    /// this module never touches the filesystem beyond its own JSON file.
    pub fn entries(&self) -> &[RecentEntry] {
        &self.entries
    }
}

/// Dedup key: Windows paths are case-insensitive but `PathBuf` equality is
/// not — `H:\Foo` and `h:\foo` are the same project. Lossy+lowercase is
/// deliberate: no filesystem calls (canonicalize) in the model.
fn fold(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// Kind string for a drained open: `None` (plain shell) → terminal, otherwise
/// the injected command's first token. Matches the strings
/// `SessionKind::launch_command` produces; anything unrecognized is terminal
/// (honest fallback — never guess, per grug review).
pub fn kind_of_command(cmd: Option<&str>) -> &'static str {
    match cmd.and_then(|c| c.split_whitespace().next()) {
        Some("claude") => "claude",
        Some("codex") => "codex",
        Some("grok") => "grok",
        _ => "terminal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn push_is_mru_with_case_insensitive_dedup_and_cap() {
        let mut r = Recents::default();
        r.push(PathBuf::from("H:\\Foo"), "terminal");
        r.push(PathBuf::from("h:\\foo"), "claude"); // same project on Windows
        assert_eq!(r.entries().len(), 1, "case-folded dedup");
        assert_eq!(
            r.entries()[0].kind,
            "claude",
            "re-record adopts the new kind"
        );
        assert_eq!(r.entries()[0].path, PathBuf::from("h:\\foo"));
        for i in 0..10 {
            r.push(PathBuf::from(format!("C:\\p{i}")), "terminal");
        }
        assert_eq!(r.entries().len(), MAX_RECENTS, "capped");
        assert_eq!(
            r.entries()[0].path,
            PathBuf::from("C:\\p9"),
            "most-recent-first"
        );
    }

    #[test]
    fn unknown_kind_strings_survive_load() {
        // A future build may write kinds this build doesn't know. Kind is a
        // plain String precisely so the file still parses (spec amendment).
        let r: Recents =
            serde_json::from_str(r#"{"entries":[{"path":"H:\\x","kind":"future-agent"}]}"#)
                .unwrap();
        assert_eq!(r.entries()[0].kind, "future-agent");
    }

    #[test]
    fn empty_object_loads_as_default() {
        let r: Recents = serde_json::from_str("{}").unwrap();
        assert!(r.entries().is_empty());
    }

    #[test]
    fn kind_of_command_maps_stems() {
        assert_eq!(kind_of_command(None), "terminal");
        assert_eq!(kind_of_command(Some("claude")), "claude");
        assert_eq!(kind_of_command(Some("codex")), "codex");
        assert_eq!(kind_of_command(Some("grok")), "grok");
        assert_eq!(kind_of_command(Some("some-other-tool --flag")), "terminal");
    }
}
