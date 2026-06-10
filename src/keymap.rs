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
    /// Create a new terminal and snap it to the pointed zone; if a window is
    /// already snapped there, tab the newcomer onto it instead (Phase 2 split).
    Split(Dir),
    ZoomTerm,
    CloseTerm,
    Rename,
    NewTerm,
    LastTerm,
    /// Cycle to the next tab in the focused stack; if the focused window is not a
    /// stack, falls back to the last-focused toggle (supersedes plain `Tab`).
    TabCycle,
    /// Cycle to the previous tab in the focused stack (no fallback).
    TabPrev,
    // project (outer) level
    ProjFocus(Dir),
    ProjSnap(Dir),
    ZoomProject,
    CloseProject,
    NewProject,
    LastProject,
    // global
    Help,
    OpenSettings,
}

impl Command {
    /// Every command, in display order, paired with its UI group and label.
    /// The settings editor iterates this to build its grouped rows; the `?`
    /// overlay reuses the labels. Keep this exhaustive — a missing arm is a
    /// compile error once the `match` below is updated.
    pub const ALL: &'static [Command] = {
        use Command::*;
        use Dir::*;
        &[
            // Projects
            ProjFocus(Left),
            ProjFocus(Down),
            ProjFocus(Up),
            ProjFocus(Right),
            ProjSnap(Left),
            ProjSnap(Down),
            ProjSnap(Up),
            ProjSnap(Right),
            ZoomProject,
            CloseProject,
            NewProject,
            LastProject,
            // Terminals
            TermFocus(Left),
            TermFocus(Down),
            TermFocus(Up),
            TermFocus(Right),
            TermSnap(Left),
            TermSnap(Down),
            TermSnap(Up),
            TermSnap(Right),
            Split(Left),
            Split(Down),
            Split(Up),
            Split(Right),
            ZoomTerm,
            CloseTerm,
            NewTerm,
            Rename,
            LastTerm,
            TabCycle,
            TabPrev,
            // Actions
            Help,
            OpenSettings,
        ]
    };

    /// The group a command belongs to in the settings editor and help overlay.
    pub fn group(self) -> Group {
        use Command::*;
        match self {
            ProjFocus(_) | ProjSnap(_) | ZoomProject | CloseProject | NewProject | LastProject => {
                Group::Projects
            }
            TermFocus(_) | TermSnap(_) | Split(_) | ZoomTerm | CloseTerm | NewTerm | Rename
            | LastTerm | TabCycle | TabPrev => Group::Terminals,
            Help | OpenSettings => Group::Actions,
        }
    }

    /// Human-readable label for the editor / help overlay.
    pub fn label(self) -> &'static str {
        use Command::*;
        match self {
            ProjFocus(d) => match d {
                Dir::Left => "Focus project left",
                Dir::Down => "Focus project down",
                Dir::Up => "Focus project up",
                Dir::Right => "Focus project right",
            },
            ProjSnap(d) => match d {
                Dir::Left => "Snap project left",
                Dir::Down => "Snap project down",
                Dir::Up => "Snap project up",
                Dir::Right => "Snap project right",
            },
            ZoomProject => "Zoom (maximize) project",
            CloseProject => "Close project",
            NewProject => "New project (picker)",
            LastProject => "Toggle last project",
            TermFocus(d) => match d {
                Dir::Left => "Focus terminal left",
                Dir::Down => "Focus terminal down",
                Dir::Up => "Focus terminal up",
                Dir::Right => "Focus terminal right",
            },
            TermSnap(d) => match d {
                Dir::Left => "Snap terminal left",
                Dir::Down => "Snap terminal down",
                Dir::Up => "Snap terminal up",
                Dir::Right => "Snap terminal right",
            },
            Split(d) => match d {
                Dir::Left => "Split terminal left",
                Dir::Down => "Split terminal down",
                Dir::Up => "Split terminal up",
                Dir::Right => "Split terminal right",
            },
            ZoomTerm => "Zoom (maximize) terminal",
            CloseTerm => "Close terminal",
            NewTerm => "New terminal",
            Rename => "Rename focused window",
            LastTerm => "Toggle last terminal",
            TabCycle => "Next tab / last terminal",
            TabPrev => "Previous tab",
            Help => "Show bindings cheat sheet",
            OpenSettings => "Open keybindings editor",
        }
    }
}

/// UI grouping for the settings editor and help overlay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Projects,
    Terminals,
    Actions,
}

impl Group {
    pub const ALL: &'static [Group] = &[Group::Projects, Group::Terminals, Group::Actions];
    pub fn title(self) -> &'static str {
        match self {
            Group::Projects => "Projects",
            Group::Terminals => "Terminals",
            Group::Actions => "Actions",
        }
    }
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

    /// Pretty-print a chord for the editor / help overlay, e.g. `Ctrl+Shift+→`.
    /// Modifier order is fixed (Ctrl, Shift, Alt) so equal chords render
    /// identically.
    pub fn pretty(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        s.push_str(key_label(self.key));
        s
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

    /// The first chord currently bound to `command`, if any. Several chords may
    /// map to one command (e.g. plain *and* shift `?` → Help); the editor shows
    /// and rebinds the canonical (default) one. We pick the default chord if it
    /// is still bound to this command, else any bound chord, in a stable order.
    pub fn chord_for(&self, command: Command) -> Option<Chord> {
        if let Some(def) = default_chord(command) {
            if self.table.get(&def) == Some(&command) {
                return Some(def);
            }
        }
        // Deterministic fallback: smallest chord by its on-disk key name + mods.
        let mut hits: Vec<Chord> = self
            .table
            .iter()
            .filter(|&(_, &c)| c == command)
            .map(|(&ch, _)| ch)
            .collect();
        hits.sort_by(|a, b| chord_sort_key(*a).cmp(&chord_sort_key(*b)));
        hits.into_iter().next()
    }

    /// Rebind `command` to `chord`: remove every chord currently mapped to this
    /// command (so a command never has stale duplicates) and any chord previously
    /// mapped to a *different* command at `chord` (the conflict the caller already
    /// confirmed), then insert. Returns the command that previously owned `chord`
    /// and was displaced (for the caller's records), if any.
    pub fn rebind(&mut self, command: Command, chord: Chord) -> Option<Command> {
        let displaced = self.table.get(&chord).copied().filter(|&c| c != command);
        self.table.retain(|_, &mut c| c != command);
        self.table.insert(chord, command);
        displaced
    }

    /// Reset a single command to its in-code default chord. Removes any current
    /// chords for it first. Returns the command that the default chord displaced,
    /// if it was bound elsewhere.
    pub fn reset_one(&mut self, command: Command) -> Option<Command> {
        if let Some(def) = default_chord(command) {
            self.rebind(command, def)
        } else {
            self.table.retain(|_, &mut c| c != command);
            None
        }
    }

    /// Replace the entire keymap (leader + table) with the in-code defaults.
    pub fn reset_all(&mut self) {
        *self = Self::default();
    }

    /// Set the leader chord.
    pub fn set_leader(&mut self, chord: Chord) {
        self.leader = chord;
    }

    /// Persist the current keymap to `%APPDATA%\foreman\keybindings.json`,
    /// creating the `foreman` directory if needed. Errors are returned (never
    /// panicked) so the caller can surface them in-UI; they are also logged.
    pub fn save(&self) -> Result<(), String> {
        let appdata = std::env::var("APPDATA")
            .map_err(|_| "APPDATA is not set; cannot locate the config directory".to_string())?;
        let dir = std::path::Path::new(&appdata).join("foreman");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            let msg = format!("could not create {}: {}", dir.display(), e);
            eprintln!("foreman: {msg}");
            return Err(msg);
        }
        let path = dir.join("keybindings.json");

        let mut bindings: Vec<BindingRepr> = self
            .table
            .iter()
            .map(|(&chord, &command)| BindingRepr { chord, command })
            .collect();
        // Stable, diff-friendly on-disk order.
        bindings.sort_by(|a, b| chord_sort_key(a.chord).cmp(&chord_sort_key(b.chord)));

        let file = KeymapFile {
            leader: Some(self.leader),
            bindings,
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("could not serialize keybindings: {e}"))?;

        if let Err(e) = std::fs::write(&path, json) {
            let msg = format!("could not write {}: {}", path.display(), e);
            eprintln!("foreman: {msg}");
            return Err(msg);
        }
        Ok(())
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
        // The file is *authoritative* for every command it mentions: drop all of
        // that command's default chords first, so a rebind that moved a command
        // off its default chord doesn't resurrect the default on reload. Commands
        // absent from the file keep their in-code defaults (the merge contract).
        let mentioned: std::collections::HashSet<Command> =
            file.bindings.iter().map(|b| b.command).collect();
        self.table.retain(|_, c| !mentioned.contains(c));
        // Now apply the file's chords. Inserting a chord that a *different*
        // command held by default overwrites it (last-writer-wins), matching the
        // editor's conflict-resolution semantics.
        for b in file.bindings {
            self.table.insert(b.chord, b.command);
        }
    }
}

impl Default for Keymap {
    /// The in-code defaults — a faithful, exhaustive reproduction of the Phase 1
    /// `resolve` match. If you add a `Command`, add its default binding here.
    fn default() -> Self {
        use Command::*;
        use Dir::*;
        use egui::Key as K;

        // Helper closures keep the table terse and unambiguous about modifiers.
        let plain = |k: K| Chord::new(k, false, false, false);
        let ctrl = |k: K| Chord::new(k, true, false, false);
        let shift = |k: K| Chord::new(k, false, true, false);
        let alt = |k: K| Chord::new(k, false, false, true);

        let mut t: HashMap<Chord, Command> = HashMap::new();

        // --- directional: arrows (final §2 scheme) ---
        // plain arrows = terminal focus; Ctrl+arrows = project focus.
        // (Terminal snap moved to WASD; project snap to Ctrl+WASD; both below.)
        for (k, d) in [
            (K::ArrowLeft, Left),
            (K::ArrowDown, Down),
            (K::ArrowUp, Up),
            (K::ArrowRight, Right),
        ] {
            t.insert(plain(k), TermFocus(d));
            t.insert(ctrl(k), ProjFocus(d));
        }

        // --- WASD (W=up, A=left, S=down, D=right): snap + split ---
        // plain WASD  = terminal snap (replaces the old Shift+arrows)
        // Ctrl+WASD   = project snap
        // Alt+WASD    = split (new terminal → zone, tab on collision; Phase 2)
        // `h/j/k/l` terminal focus is intentionally dropped — re-addable via the
        // settings editor.
        for (k, d) in [(K::W, Up), (K::A, Left), (K::S, Down), (K::D, Right)] {
            t.insert(plain(k), TermSnap(d));
            t.insert(ctrl(k), ProjSnap(d));
            t.insert(alt(k), Split(d));
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

        // --- tab cycle / last-focused toggle ---
        // `Tab` cycles tabs in the focused stack, falling back to the last-focused
        // toggle when the focused window is not a stack (handled in `dispatch`).
        // This supersedes the old plain-`Tab` → LastTerm binding. `Shift+Tab`
        // cycles backwards. `LastTerm` is still a dispatchable command (the
        // fallback path) but no longer owns a default chord of its own.
        t.insert(plain(K::Tab), TabCycle);
        t.insert(shift(K::Tab), TabPrev);
        t.insert(ctrl(K::Tab), LastProject);

        // --- discoverability ---
        // Phase 1 matched `Questionmark` regardless of modifiers (it is typed as
        // Shift+/ on many layouts). Bind both plain and shift forms.
        t.insert(plain(K::Questionmark), Help);
        t.insert(shift(K::Questionmark), Help);

        // --- settings / rebinding editor ---
        // `Ctrl+,` after the leader — the conventional "preferences" chord, and
        // unused here (plain `,` is Rename). Documented in the `?` overlay.
        t.insert(ctrl(K::Comma), OpenSettings);

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

/// The in-code default chord for a command, if one exists. Used by the editor's
/// reset-one and by `chord_for` to prefer the canonical chord. Built from a
/// fresh default keymap so it always matches `Keymap::default`.
pub fn default_chord(command: Command) -> Option<Chord> {
    let def = Keymap::default();
    if let Some(def_chord) = default_canonical(command) {
        if def.table.get(&def_chord) == Some(&command) {
            return Some(def_chord);
        }
    }
    let mut hits: Vec<Chord> = def
        .table
        .iter()
        .filter(|&(_, &c)| c == command)
        .map(|(&ch, _)| ch)
        .collect();
    hits.sort_by(|a, b| chord_sort_key(*a).cmp(&chord_sort_key(*b)));
    hits.into_iter().next()
}

/// For commands bound to multiple default chords (Help, NewProject), the one to
/// treat as canonical (shown/reset-to). `None` falls through to the sort-order
/// pick in [`default_chord`].
fn default_canonical(command: Command) -> Option<Chord> {
    use egui::Key as K;
    match command {
        Command::Help => Some(Chord::new(K::Questionmark, false, true, false)), // Shift+? (typed form)
        Command::NewProject => Some(Chord::new(K::P, false, true, false)),      // Shift+P
        _ => None,
    }
}

/// A total, stable ordering key for a chord: (key name, ctrl, shift, alt).
fn chord_sort_key(c: Chord) -> (String, bool, bool, bool) {
    (key_to_name(c.key).to_string(), c.ctrl, c.shift, c.alt)
}

/// Display label for a key in the pretty-printed chord (arrows as glyphs,
/// punctuation as the symbol, letters/words otherwise). Distinct from
/// [`key_to_name`], which produces the stable on-disk name.
pub fn key_label(key: egui::Key) -> &'static str {
    use egui::Key as K;
    match key {
        K::ArrowLeft => "←",
        K::ArrowRight => "→",
        K::ArrowUp => "↑",
        K::ArrowDown => "↓",
        K::Comma => ",",
        K::Questionmark => "?",
        K::OpenBracket => "[",
        K::CloseBracket => "]",
        K::Space => "Space",
        K::Enter => "Enter",
        K::Tab => "Tab",
        K::Backspace => "Backspace",
        K::Escape => "Esc",
        other => key_to_name(other),
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
        // arrows: plain = terminal focus, Ctrl = project focus.
        assert_eq!(
            km.resolve(Chord::new(K::ArrowLeft, false, false, false)),
            Some(Command::TermFocus(Dir::Left))
        );
        assert_eq!(
            km.resolve(Chord::new(K::ArrowUp, true, false, false)),
            Some(Command::ProjFocus(Dir::Up))
        );
        // WASD: plain = terminal snap, Ctrl = project snap, Alt = split.
        assert_eq!(
            km.resolve(Chord::new(K::D, false, false, false)),
            Some(Command::TermSnap(Dir::Right))
        );
        assert_eq!(
            km.resolve(Chord::new(K::W, false, false, false)),
            Some(Command::TermSnap(Dir::Up))
        );
        assert_eq!(
            km.resolve(Chord::new(K::A, true, false, false)),
            Some(Command::ProjSnap(Dir::Left))
        );
        assert_eq!(
            km.resolve(Chord::new(K::S, true, false, false)),
            Some(Command::ProjSnap(Dir::Down))
        );
        assert_eq!(
            km.resolve(Chord::new(K::W, false, false, true)),
            Some(Command::Split(Dir::Up))
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
        // vi keys are no longer bound by default (dropped in the §2 rebind).
        assert_eq!(km.resolve(Chord::new(K::H, false, false, false)), None);
        assert_eq!(km.resolve(Chord::new(K::J, false, false, false)), None);
        assert_eq!(km.resolve(Chord::new(K::K, false, false, false)), None);
        assert_eq!(km.resolve(Chord::new(K::L, false, false, false)), None);
    }

    #[test]
    fn tab_bindings_are_cycle_prev_and_last_project() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(Chord::new(K::Tab, false, false, false)),
            Some(Command::TabCycle)
        );
        assert_eq!(
            km.resolve(Chord::new(K::Tab, false, true, false)),
            Some(Command::TabPrev)
        );
        assert_eq!(
            km.resolve(Chord::new(K::Tab, true, false, false)),
            Some(Command::LastProject)
        );
    }

    #[test]
    fn wasd_snap_and_split_defaults() {
        let km = Keymap::default();
        // terminal snap on plain WASD (W=up, A=left, S=down, D=right)
        for (k, d) in [
            (K::W, Dir::Up),
            (K::A, Dir::Left),
            (K::S, Dir::Down),
            (K::D, Dir::Right),
        ] {
            assert_eq!(
                km.resolve(Chord::new(k, false, false, false)),
                Some(Command::TermSnap(d)),
                "plain {k:?} should be TermSnap({d:?})"
            );
            // project snap on Ctrl+WASD
            assert_eq!(
                km.resolve(Chord::new(k, true, false, false)),
                Some(Command::ProjSnap(d)),
                "Ctrl+{k:?} should be ProjSnap({d:?})"
            );
            // split on Alt+WASD
            assert_eq!(
                km.resolve(Chord::new(k, false, false, true)),
                Some(Command::Split(d)),
                "Alt+{k:?} should be Split({d:?})"
            );
        }
    }

    #[test]
    fn letter_chords_pretty_print_with_modifiers() {
        assert_eq!(Chord::new(K::W, false, false, true).pretty(), "Alt+W");
        assert_eq!(Chord::new(K::A, true, false, false).pretty(), "Ctrl+A");
        assert_eq!(Chord::new(K::D, false, false, false).pretty(), "D");
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

    /// Build a `KeymapFile` from the live keymap exactly as `save()` does, then
    /// serialize → parse → merge back into a fresh default and confirm every
    /// binding (including a rebound one) survives. This exercises the whole
    /// write→read contract without touching APPDATA.
    #[test]
    fn keymap_write_read_roundtrip_preserves_all_bindings() {
        let mut km = Keymap::default();
        // Mutate via the editor API: change the leader and rebind one command.
        km.set_leader(Chord::new(K::Space, true, false, false));
        km.rebind(Command::CloseTerm, Chord::new(K::Q, false, false, false));

        // Serialize exactly like save() (same KeymapFile construction).
        let mut bindings: Vec<BindingRepr> = km
            .table
            .iter()
            .map(|(&chord, &command)| BindingRepr { chord, command })
            .collect();
        bindings.sort_by(|a, b| chord_sort_key(a.chord).cmp(&chord_sort_key(b.chord)));
        let file = KeymapFile {
            leader: Some(km.leader),
            bindings,
        };
        let json = serde_json::to_string_pretty(&file).unwrap();

        // Read back: parse + merge over a fresh default, as load() does.
        let parsed: KeymapFile = serde_json::from_str(&json).unwrap();
        let mut reloaded = Keymap::default();
        reloaded.merge(parsed);

        assert_eq!(reloaded.leader, Chord::new(K::Space, true, false, false));
        assert_eq!(
            reloaded.resolve(Chord::new(K::Q, false, false, false)),
            Some(Command::CloseTerm)
        );
        // The rebind unbound the old chord for CloseTerm (plain `x`).
        assert_ne!(
            reloaded.resolve(Chord::new(K::X, false, false, false)),
            Some(Command::CloseTerm)
        );
        // An untouched default still resolves.
        assert_eq!(
            reloaded.resolve(Chord::new(K::C, false, false, false)),
            Some(Command::NewTerm)
        );
    }

    #[test]
    fn rebind_displaces_and_unbinds_old() {
        let mut km = Keymap::default();
        // C is NewTerm; bind it to NewTerm's slot... actually rebind NewTerm to F.
        km.rebind(Command::NewTerm, Chord::new(K::F, false, false, false));
        assert_eq!(
            km.resolve(Chord::new(K::F, false, false, false)),
            Some(Command::NewTerm)
        );
        // Old chord `c` is now free.
        assert_eq!(km.resolve(Chord::new(K::C, false, false, false)), None);

        // Now rebind CloseTerm onto F: NewTerm is displaced and reported.
        let displaced = km.rebind(Command::CloseTerm, Chord::new(K::F, false, false, false));
        assert_eq!(displaced, Some(Command::NewTerm));
        assert_eq!(
            km.resolve(Chord::new(K::F, false, false, false)),
            Some(Command::CloseTerm)
        );
        // NewTerm no longer has any chord.
        assert_eq!(km.chord_for(Command::NewTerm), None);
    }

    #[test]
    fn reset_one_restores_default_chord() {
        let mut km = Keymap::default();
        km.rebind(Command::CloseTerm, Chord::new(K::Q, false, false, false));
        km.reset_one(Command::CloseTerm);
        assert_eq!(
            km.resolve(Chord::new(K::X, false, false, false)),
            Some(Command::CloseTerm)
        );
        assert_eq!(km.resolve(Chord::new(K::Q, false, false, false)), None);
    }

    #[test]
    fn chord_for_prefers_default_then_canonical() {
        let km = Keymap::default();
        // Help is bound plain and shift; canonical is the typed Shift+? form.
        assert_eq!(
            km.chord_for(Command::Help),
            Some(Chord::new(K::Questionmark, false, true, false))
        );
        // Single-binding command.
        assert_eq!(
            km.chord_for(Command::OpenSettings),
            Some(Chord::new(K::Comma, true, false, false))
        );
    }

    #[test]
    fn all_commands_have_a_default_chord_and_metadata() {
        for &cmd in Command::ALL {
            // `LastTerm` deliberately has no default chord: `Tab` is now
            // `TabCycle`, which falls back to last-terminal behaviour. The command
            // remains dispatchable so a user can still bind it.
            if cmd == Command::LastTerm {
                continue;
            }
            assert!(
                default_chord(cmd).is_some(),
                "command {cmd:?} has no default chord"
            );
            // label + group must not panic and label is non-empty.
            assert!(!cmd.label().is_empty());
            let _ = cmd.group();
        }
    }

    #[test]
    fn pretty_prints_modifiers_and_arrows() {
        assert_eq!(
            Chord::new(K::ArrowRight, true, true, false).pretty(),
            "Ctrl+Shift+→"
        );
        assert_eq!(Chord::new(K::Comma, true, false, false).pretty(), "Ctrl+,");
        assert_eq!(Chord::new(K::C, false, false, false).pretty(), "C");
    }
}
