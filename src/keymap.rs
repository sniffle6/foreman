//! Data-driven key bindings (Phase 2 of the keyboard-control epic).
//!
//! The defaults are still defined **in code** (`Keymap::default`) and reproduce
//! the Phase 1 hardcoded `match` exactly. A user file at
//! `%APPDATA%\foreman\keybindings.json` is loaded and merged *over* the defaults:
//! any command absent from the file keeps its default chord, so new commands
//! always get a binding even on an old config. There is no write path in this
//! phase — the file is hand-edited.

use crate::wm::Dir;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A leader command. Terminal-level variants act on the focused project's child
/// manager; project-level variants act on the desktop. This is the serializable
/// successor to Phase 1's `Cmd` enum.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Command {
    // terminal (inner) level
    TermFocus(Dir),
    TermSnap(Dir),
    ZoomTerm,
    CloseTerm,
    Rename,
    NewTerm,
    LastTerm,
    // project (outer) level
    ProjFocus(Dir),
    ZoomProject,
    CloseProject,
    NewProject,
    LastProject,
    // global
    Help,
}

/// A key chord: a single key plus modifier flags. The key is serialized as a
/// stable string name (see [`key_to_name`] / [`name_to_key`]) so the JSON file
/// is human-editable and resilient to egui internal changes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Chord {
    pub key: egui::Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    pub fn new(key: egui::Key, ctrl: bool, shift: bool, alt: bool) -> Self {
        Self {
            key,
            ctrl,
            shift,
            alt,
        }
    }

    /// Build a chord from a key + egui modifiers, normalizing `command` (mac ⌘)
    /// onto `ctrl` exactly as Phase 1's `resolve` did (`m.ctrl || m.command`).
    pub fn from_event(key: egui::Key, m: egui::Modifiers) -> Self {
        Self {
            key,
            ctrl: m.ctrl || m.command,
            shift: m.shift,
            alt: m.alt,
        }
    }
}

/// On-disk representation of a chord — key as a string, modifiers as bools that
/// default to false so the JSON can omit unset modifiers.
#[derive(Serialize, Deserialize)]
struct ChordRepr {
    key: String,
    #[serde(default)]
    ctrl: bool,
    #[serde(default)]
    shift: bool,
    #[serde(default)]
    alt: bool,
}

impl Serialize for Chord {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        ChordRepr {
            key: key_to_name(self.key).to_string(),
            ctrl: self.ctrl,
            shift: self.shift,
            alt: self.alt,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = ChordRepr::deserialize(d)?;
        let key = name_to_key(&r.key)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown key name {:?}", r.key)))?;
        Ok(Chord {
            key,
            ctrl: r.ctrl,
            shift: r.shift,
            alt: r.alt,
        })
    }
}

/// One entry in the on-disk keymap file: a chord plus the command it runs.
#[derive(Serialize, Deserialize)]
struct BindingRepr {
    #[serde(flatten)]
    chord: Chord,
    command: Command,
}

/// On-disk shape of the whole file. Every field is optional so a partial file is
/// valid; missing entries fall back to the in-code defaults via merge.
#[derive(Serialize, Deserialize, Default)]
struct KeymapFile {
    leader: Option<Chord>,
    #[serde(default)]
    bindings: Vec<BindingRepr>,
}

/// The active keymap: the leader chord plus a chord→command lookup table.
pub struct Keymap {
    pub leader: Chord,
    table: HashMap<Chord, Command>,
}

impl Keymap {
    /// Resolve a pressed chord to a command. Returns `None` for unbound chords.
    pub fn resolve(&self, chord: Chord) -> Option<Command> {
        self.table.get(&chord).copied()
    }

    /// Load from `%APPDATA%\foreman\keybindings.json`, merged over the defaults.
    ///
    /// Missing file → defaults (silent, the common case). Unreadable or
    /// unparseable file → defaults plus a clear stderr warning. The app never
    /// crashes on a bad config.
    pub fn load() -> Self {
        let mut km = Self::default();

        let Ok(appdata) = std::env::var("APPDATA") else {
            // No APPDATA (extremely unusual on Windows) — just use defaults.
            return km;
        };
        let path = std::path::Path::new(&appdata)
            .join("foreman")
            .join("keybindings.json");

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return km, // no file: defaults
            Err(e) => {
                eprintln!(
                    "foreman: could not read keybindings {}: {} — using defaults",
                    path.display(),
                    e
                );
                return km;
            }
        };

        match serde_json::from_str::<KeymapFile>(&text) {
            Ok(file) => km.merge(file),
            Err(e) => {
                eprintln!(
                    "foreman: keybindings {} is invalid JSON: {} — using defaults",
                    path.display(),
                    e
                );
            }
        }
        km
    }

    /// Merge a parsed file *over* the defaults: override the leader if present,
    /// and add/replace each binding. Defaults for commands not mentioned in the
    /// file are left intact.
    fn merge(&mut self, file: KeymapFile) {
        if let Some(leader) = file.leader {
            self.leader = leader;
        }
        for b in file.bindings {
            self.table.insert(b.chord, b.command);
        }
    }
}

impl Default for Keymap {
    /// The in-code defaults — a faithful, exhaustive reproduction of the Phase 1
    /// `resolve` match. If you add a `Command`, add its default binding here.
    fn default() -> Self {
        use egui::Key as K;
        use Command::*;
        use Dir::*;

        // Helper closures keep the table terse and unambiguous about modifiers.
        let plain = |k: K| Chord::new(k, false, false, false);
        let ctrl = |k: K| Chord::new(k, true, false, false);
        let shift = |k: K| Chord::new(k, false, true, false);

        let mut t: HashMap<Chord, Command> = HashMap::new();

        // --- directional: arrows ---
        // plain = terminal focus, Shift = terminal snap, Ctrl = project focus.
        for (k, d) in [
            (K::ArrowLeft, Left),
            (K::ArrowDown, Down),
            (K::ArrowUp, Up),
            (K::ArrowRight, Right),
        ] {
            t.insert(plain(k), TermFocus(d));
            t.insert(shift(k), TermSnap(d));
            t.insert(ctrl(k), ProjFocus(d));
        }

        // --- vi h/j/k/l: terminal focus only (no ctrl/shift in Phase 1) ---
        for (k, d) in [(K::H, Left), (K::J, Down), (K::K, Up), (K::L, Right)] {
            t.insert(plain(k), TermFocus(d));
        }

        // --- new / close / zoom / rename ---
        t.insert(plain(K::C), NewTerm);
        // `P`: Phase 1 bound plain *and* shift+P to NewProject. Bind both so the
        // common shift-typed `P` resolves regardless of the shift flag.
        t.insert(plain(K::P), NewProject);
        t.insert(shift(K::P), NewProject);
        t.insert(plain(K::X), CloseTerm);
        t.insert(ctrl(K::X), CloseProject);
        t.insert(plain(K::Z), ZoomTerm);
        t.insert(ctrl(K::Z), ZoomProject);
        t.insert(plain(K::Comma), Rename);

        // --- last-focused toggle ---
        t.insert(plain(K::Tab), LastTerm);
        t.insert(ctrl(K::Tab), LastProject);

        // --- discoverability ---
        // Phase 1 matched `Questionmark` regardless of modifiers (it is typed as
        // Shift+/ on many layouts). Bind both plain and shift forms.
        t.insert(plain(K::Questionmark), Help);
        t.insert(shift(K::Questionmark), Help);

        Keymap {
            leader: Chord::new(K::B, true, false, false), // Ctrl+b
            table: t,
        }
    }
}

/// Map an egui `Key` to a stable, human-readable name for the JSON file.
/// Covers at least every key used in the default bindings; extend as commands
/// are added. Falls back to the egui `Debug` name for keys not special-cased.
pub fn key_to_name(key: egui::Key) -> &'static str {
    use egui::Key as K;
    match key {
        K::ArrowLeft => "Left",
        K::ArrowRight => "Right",
        K::ArrowUp => "Up",
        K::ArrowDown => "Down",
        K::Tab => "Tab",
        K::Comma => "Comma",
        K::Questionmark => "Questionmark",
        K::OpenBracket => "OpenBracket",
        K::A => "A",
        K::B => "B",
        K::C => "C",
        K::D => "D",
        K::E => "E",
        K::F => "F",
        K::G => "G",
        K::H => "H",
        K::I => "I",
        K::J => "J",
        K::K => "K",
        K::L => "L",
        K::M => "M",
        K::N => "N",
        K::O => "O",
        K::P => "P",
        K::Q => "Q",
        K::R => "R",
        K::S => "S",
        K::T => "T",
        K::U => "U",
        K::V => "V",
        K::W => "W",
        K::X => "X",
        K::Y => "Y",
        K::Z => "Z",
        // egui's own name is a reasonable stable fallback for anything else.
        other => other.name(),
    }
}

/// Parse a key name from the JSON file back to an egui `Key`. Delegates to
/// egui's own `from_name`, which understands the names produced by
/// [`key_to_name`] (arrows, letters, "Comma", "Questionmark", "OpenBracket",
/// "Space", …) plus many friendly aliases ("Left", "?", ",", "[", " ", …).
/// Returns `None` for unknown names so the loader can reject the entry with a
/// warning instead of crashing.
pub fn name_to_key(name: &str) -> Option<egui::Key> {
    egui::Key::from_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Key as K;

    #[test]
    fn default_leader_is_ctrl_b() {
        let km = Keymap::default();
        assert_eq!(km.leader, Chord::new(K::B, true, false, false));
    }

    #[test]
    fn default_resolves_known_chords() {
        let km = Keymap::default();
        // arrows
        assert_eq!(
            km.resolve(Chord::new(K::ArrowLeft, false, false, false)),
            Some(Command::TermFocus(Dir::Left))
        );
        assert_eq!(
            km.resolve(Chord::new(K::ArrowRight, false, true, false)),
            Some(Command::TermSnap(Dir::Right))
        );
        assert_eq!(
            km.resolve(Chord::new(K::ArrowUp, true, false, false)),
            Some(Command::ProjFocus(Dir::Up))
        );
        // action keys
        assert_eq!(
            km.resolve(Chord::new(K::C, false, false, false)),
            Some(Command::NewTerm)
        );
        assert_eq!(
            km.resolve(Chord::new(K::X, true, false, false)),
            Some(Command::CloseProject)
        );
        assert_eq!(
            km.resolve(Chord::new(K::Z, false, false, false)),
            Some(Command::ZoomTerm)
        );
        assert_eq!(
            km.resolve(Chord::new(K::Tab, true, false, false)),
            Some(Command::LastProject)
        );
        assert_eq!(
            km.resolve(Chord::new(K::Questionmark, false, false, false)),
            Some(Command::Help)
        );
        // vi keys
        assert_eq!(
            km.resolve(Chord::new(K::J, false, false, false)),
            Some(Command::TermFocus(Dir::Down))
        );
    }

    #[test]
    fn unbound_chord_resolves_none() {
        let km = Keymap::default();
        assert_eq!(km.resolve(Chord::new(K::F, false, false, false)), None);
    }

    #[test]
    fn malformed_json_falls_back_to_defaults() {
        // We can't easily point load() at a temp path (it reads APPDATA), but we
        // can prove the merge/parse contract: bad JSON does not parse, so a
        // freshly-defaulted keymap is what callers keep.
        let parsed = serde_json::from_str::<KeymapFile>("{ this is not json ]");
        assert!(parsed.is_err());
        let km = Keymap::default();
        assert!(km.resolve(Chord::new(K::C, false, false, false)).is_some());
    }

    #[test]
    fn file_merges_over_defaults() {
        // Override the leader and one binding; everything else stays default.
        let json = r#"{
            "leader": { "key": "Space", "ctrl": true },
            "bindings": [
                { "key": "Q", "command": "CloseTerm" }
            ]
        }"#;
        let file: KeymapFile = serde_json::from_str(json).expect("valid json");
        let mut km = Keymap::default();
        km.merge(file);
        // leader overridden
        assert_eq!(km.leader, Chord::new(K::Space, true, false, false));
        // new binding present
        assert_eq!(
            km.resolve(Chord::new(K::Q, false, false, false)),
            Some(Command::CloseTerm)
        );
        // default still intact for an untouched command
        assert_eq!(
            km.resolve(Chord::new(K::C, false, false, false)),
            Some(Command::NewTerm)
        );
    }

    #[test]
    fn unknown_key_name_is_rejected() {
        let json = r#"{ "bindings": [ { "key": "NotAKey", "command": "Help" } ] }"#;
        // The whole file fails to parse because the chord can't deserialize; the
        // loader treats that as "corrupt → defaults", which is the safe outcome.
        assert!(serde_json::from_str::<KeymapFile>(json).is_err());
    }

    #[test]
    fn chord_roundtrips_through_json() {
        let c = Chord::new(K::ArrowLeft, true, true, false);
        let s = serde_json::to_string(&c).unwrap();
        let back: Chord = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}
