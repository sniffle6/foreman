//! Saved project layouts on disk: `%APPDATA%\foreman\project-layouts.json`,
//! one list every Project shares plus the single New Project slot, and the
//! read-only built-ins that ship with Foreman. Spec:
//! `docs/superpowers/specs/2026-10-16-project-layouts-design.md`
//! (Persistence). Feature doc: `docs/project-layouts.md`.
//!
//! There is no in-memory cache. Every read goes to disk and every change
//! re-reads the file, edits only its own definition (or the New Project
//! slot), and writes the result, so a second Foreman process sharing the
//! file keeps definitions this one never loaded. Selection and Modified are
//! per Project instance and live in the workspace snapshot, not here.

use crate::project_layout::{
    Arrangement, ItemKind, ItemRef, LayoutDefinition, LayoutError, MAX_NAME_CHARS, SplitAxis,
    TreeSpec, WindowSpec, names_collide, validate, validate_name,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const LIBRARY_FILE: &str = "project-layouts.json";
/// Any change to the file's shape, a definition's included, bumps this. An
/// older build then refuses to write the file instead of dropping fields.
/// Version 1 kept one list per project directory; it is migrated on read.
pub const LIBRARY_VERSION: u32 = 2;

/// The whole file. Strict on purpose: an unknown field means a newer or
/// hand-edited file, which goes through the shared recovery (backed up before
/// anything replaces it) rather than being silently trimmed on the next write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryFile {
    #[serde(default = "current_version")]
    pub version: u32,
    /// Saved layouts, in creation order. Every Project lists all of them.
    #[serde(default)]
    pub layouts: Vec<LayoutDefinition>,
    /// The New Project layout: a copy, never a reference into the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_project: Option<LayoutDefinition>,
}

impl Default for LibraryFile {
    fn default() -> Self {
        Self {
            version: LIBRARY_VERSION,
            layouts: Vec::new(),
            new_project: None,
        }
    }
}

fn current_version() -> u32 {
    LIBRARY_VERSION
}

/// Version 1 of the file: one list per directory, keyed by [`project_key`].
/// Only ever read, to migrate.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryFileV1 {
    #[serde(rename = "version", default)]
    _version: u32,
    #[serde(default)]
    projects: BTreeMap<String, ProjectLibraryV1>,
    #[serde(default)]
    new_project: Option<LayoutDefinition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectLibraryV1 {
    dir: PathBuf,
    layouts: Vec<LayoutDefinition>,
}

/// Longest folder name a migrated duplicate gets as its suffix.
const MIGRATION_HINT_CHARS: usize = 32;

impl From<LibraryFileV1> for LibraryFile {
    /// Flatten the per-directory lists in key order. Ids are kept, so
    /// workspace selections still resolve, and each definition records its
    /// directory as `origin`. A name an earlier directory or a built-in
    /// already holds gets the directory's folder name appended.
    fn from(v1: LibraryFileV1) -> Self {
        let mut layouts: Vec<LayoutDefinition> = Vec::new();
        for (key, project) in v1.projects {
            let folder = project
                .dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| key.clone());
            for mut def in project.layouts {
                let taken = |name: &str| {
                    builtin_named(name).is_some()
                        || layouts.iter().any(|d| names_collide(&d.name, name))
                };
                if taken(&def.name) {
                    def.name = disambiguate(&def.name, &folder, taken);
                }
                def.origin = Some(key.clone());
                layouts.push(def);
            }
        }
        Self {
            version: LIBRARY_VERSION,
            layouts,
            new_project: v1.new_project,
        }
    }
}

/// `name` with ` (hint)`, then ` (hint 2)`, … appended until `taken` lets
/// it go, the name cut so the whole fits in [`MAX_NAME_CHARS`].
fn disambiguate(name: &str, hint: &str, taken: impl Fn(&str) -> bool) -> String {
    let hint: String = hint.chars().take(MIGRATION_HINT_CHARS).collect();
    (1..)
        .map(|n| {
            let suffix = if n == 1 {
                format!(" ({hint})")
            } else {
                format!(" ({hint} {n})")
            };
            let keep = MAX_NAME_CHARS.saturating_sub(suffix.chars().count());
            let base: String = name.chars().take(keep).collect();
            format!("{}{suffix}", base.trim_end())
        })
        .find(|candidate| !taken(candidate))
        .expect("an unbounded counter finds a free name")
}

/// Just enough of the file to refuse a newer one before strict parsing or
/// the recovery backup can touch it, and to pick the shape to parse.
#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default)]
    version: u32,
}

/// Prefix of a built-in's id. Saved ids are hex and dashes
/// ([`new_definition_id`]), so the two can never collide.
const BUILTIN_PREFIX: &str = "builtin:";

/// The layouts that ship with Foreman, listed before the saved ones in every
/// Project. Read-only: never in the file, never renamed, updated, or
/// deleted, and their names are reserved. They carry no `origin`, so Apply
/// always fills them by kind, opening plain shells (or the chat) for slots
/// the Project has nothing for.
pub fn builtin_layouts() -> &'static [LayoutDefinition] {
    static BUILTINS: OnceLock<Vec<LayoutDefinition>> = OnceLock::new();
    BUILTINS.get_or_init(|| {
        use ItemKind::{Chat, Terminal};
        use SplitAxis::{H, V};
        vec![
            builtin(
                "side-by-side",
                "Side by side",
                &[Terminal, Terminal],
                split(H, &[0.5, 0.5], vec![leaf(0), leaf(1)]),
            ),
            builtin(
                "main-and-stack",
                "Main and stack",
                &[Terminal; 3],
                split(
                    H,
                    &[0.6, 0.4],
                    vec![leaf(0), split(V, &[0.5, 0.5], vec![leaf(1), leaf(2)])],
                ),
            ),
            builtin(
                "grid",
                "Grid",
                &[Terminal; 4],
                split(
                    H,
                    &[0.5, 0.5],
                    vec![
                        split(V, &[0.5, 0.5], vec![leaf(0), leaf(1)]),
                        split(V, &[0.5, 0.5], vec![leaf(2), leaf(3)]),
                    ],
                ),
            ),
            builtin(
                "terminal-and-chat",
                "Terminal and chat",
                &[Terminal, Chat],
                split(H, &[0.7, 0.3], vec![leaf(0), leaf(1)]),
            ),
        ]
    })
}

/// One tiled window per kind, in order; `tree` places window `i` with
/// `leaf(i)`.
fn builtin(slug: &str, name: &str, kinds: &[ItemKind], tree: TreeSpec) -> LayoutDefinition {
    let items: Vec<ItemRef> = kinds
        .iter()
        .zip(1..)
        .map(|(&kind, id)| ItemRef::new(id, kind))
        .collect();
    LayoutDefinition {
        id: format!("{BUILTIN_PREFIX}{slug}"),
        name: name.to_string(),
        arrangement: Arrangement {
            windows: items
                .iter()
                .map(|item| WindowSpec::tiled(vec![item.id], 0))
                .collect(),
            tree: Some(tree),
        },
        items,
        origin: None,
    }
}

fn leaf(window: usize) -> TreeSpec {
    TreeSpec::Leaf { window }
}

fn split(dir: SplitAxis, ratios: &[f32], children: Vec<TreeSpec>) -> TreeSpec {
    TreeSpec::Split {
        dir,
        ratios: ratios.to_vec(),
        children,
    }
}

/// Whether `id` names a built-in layout.
pub fn is_builtin(id: &str) -> bool {
    id.starts_with(BUILTIN_PREFIX)
}

fn builtin_named(name: &str) -> Option<&'static LayoutDefinition> {
    builtin_layouts()
        .iter()
        .find(|d| names_collide(&d.name, name))
}

/// What Save, Save As, and Rename do when another saved definition already
/// has the name (compared case-insensitively).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnConflict {
    /// Fail with [`StoreError::NameTaken`] so the UI can offer replacement
    /// or another name.
    Reject,
    /// The user chose replacement. Save overwrites the other definition in
    /// place, keeping its id; Rename deletes it.
    Replace,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StoreError {
    /// The name or arrangement failed validation; nothing was written.
    Invalid(LayoutError),
    /// Another saved definition already uses the name.
    NameTaken { id: String, name: String },
    /// The name is a built-in's. There is nothing to replace: pick another.
    ReservedName(String),
    /// The id is a built-in's, which cannot be changed. Carries its name.
    ReadOnly(String),
    /// No definition with this id (deleted, possibly by another instance).
    NotFound(String),
    /// The file was written by a newer Foreman; it is left untouched.
    FutureVersion(u32),
    /// The file could not be read or written. Nothing was saved.
    Io(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(e) => write!(f, "Layout not saved: {e}."),
            Self::NameTaken { name, .. } => {
                write!(f, "A layout named \"{name}\" already exists.")
            }
            Self::ReservedName(name) => write!(
                f,
                "\"{name}\" is the name of a built-in layout. Type another name."
            ),
            Self::ReadOnly(name) => write!(
                f,
                "\"{name}\" is a built-in layout and cannot be changed. Save As makes a copy you can change."
            ),
            Self::NotFound(_) => write!(
                f,
                "That layout no longer exists. It may have been deleted in another window."
            ),
            Self::FutureVersion(v) => write!(
                f,
                "{LIBRARY_FILE} was written by a newer Foreman (version {v}; this build reads {LIBRARY_VERSION}). Update Foreman to change saved layouts."
            ),
            Self::Io(e) => write!(f, "Layout not saved: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<LayoutError> for StoreError {
    fn from(e: LayoutError) -> Self {
        Self::Invalid(e)
    }
}

fn reject_builtin_id(id: &str) -> Result<(), StoreError> {
    match builtin_layouts().iter().find(|d| d.id == id) {
        Some(def) => Err(StoreError::ReadOnly(def.name.clone())),
        None => Ok(()),
    }
}

fn reject_builtin_name(name: &str) -> Result<(), StoreError> {
    match builtin_named(name) {
        Some(def) => Err(StoreError::ReservedName(def.name.clone())),
        None => Ok(()),
    }
}

/// The key that identifies a project directory, recorded as a definition's
/// `origin`: absolute, `.`/`..` resolved, symlinks, junctions, substituted
/// drives, and 8.3 short names resolved as far down as the path exists,
/// without the `\\?\` prefix or a trailing separator, and case-folded on
/// Windows. A missing directory keeps the key it had while it existed as
/// long as its parent is still reachable. It only fails to line up when the
/// missing part of the path was itself spelled through an alias.
pub fn project_key(dir: &Path) -> String {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let resolved = resolve_existing_prefix(&absolute);
    let mut key = resolved.to_string_lossy().into_owned();
    if cfg!(windows) {
        key = key.replace('/', "\\").to_lowercase();
    }
    while key.len() > 1 && key.ends_with(['\\', '/']) && !key.ends_with(":\\") {
        key.pop();
    }
    key
}

/// `path` with its deepest existing ancestor canonicalized and the missing
/// rest appended as spelled. Canonicalizing only the whole path would drop
/// to the lexical spelling the moment the directory goes, and an alias in a
/// parent that still exists (CI's `C:\Users\RUNNER~1\...` temp dir) would
/// then key differently than it did a moment before.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    for base in path.ancestors() {
        if let Ok(real) = std::fs::canonicalize(base) {
            let real = strip_verbatim(real);
            let rest = path.strip_prefix(base).unwrap_or(Path::new(""));
            return if rest.as_os_str().is_empty() {
                real
            } else {
                real.join(rest)
            };
        }
    }
    path.to_path_buf()
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

/// The definition `id` names: a built-in, or one of `saved`. `None` when it
/// is neither (deleted elsewhere since).
pub fn find_layout<'a>(saved: &'a [LayoutDefinition], id: &str) -> Option<&'a LayoutDefinition> {
    builtin_layouts().iter().chain(saved).find(|d| d.id == id)
}

/// The definition a Project instance's selection names, or `None` to show
/// Custom (nothing selected, or deleted elsewhere since).
pub fn resolve_selection<'a>(
    saved: &'a [LayoutDefinition],
    selected: Option<&str>,
) -> Option<&'a LayoutDefinition> {
    find_layout(saved, selected?)
}

fn new_definition_id() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{nanos:x}-{:x}-{sequence:x}", std::process::id())
}

/// The library file in one directory (`%APPDATA%\foreman` in the app, a temp
/// dir in tests). Cheap to construct; holds no state but the path.
#[derive(Clone, Debug)]
pub struct LayoutStore {
    dir: PathBuf,
}

impl LayoutStore {
    /// The user's store under the foreman config directory.
    pub fn user() -> Result<Self, StoreError> {
        crate::config::config_dir().map(Self::at).ok_or_else(|| {
            StoreError::Io("APPDATA is not set; cannot locate the config directory".into())
        })
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(LIBRARY_FILE)
    }

    /// The saved layouts, in creation order; the built-ins are not among
    /// them ([`builtin_layouts`]). A missing file is an empty list. A
    /// malformed file is backed up by the shared config recovery and reads
    /// as empty, with a warning toast queued.
    pub fn layouts(&self) -> Result<Vec<LayoutDefinition>, StoreError> {
        Ok(self.read()?.layouts)
    }

    /// The New Project layout, `None` when unset (one tiled terminal).
    pub fn new_project_layout(&self) -> Result<Option<LayoutDefinition>, StoreError> {
        Ok(self.read()?.new_project)
    }

    /// Save or Save As with a name, from `project`'s live windows: a new
    /// definition, or, when the user chose replacement, the colliding
    /// definition overwritten in place (same id).
    pub fn create(
        &self,
        project: &Path,
        name: &str,
        items: Vec<ItemRef>,
        arrangement: Arrangement,
        on_conflict: OnConflict,
    ) -> Result<LayoutDefinition, StoreError> {
        let name = validate_name(name)?;
        validate(&arrangement, &items)?;
        reject_builtin_name(&name)?;
        let origin = Some(project_key(project));
        self.change(|layouts| {
            if let Some(existing) = layouts.iter_mut().find(|d| names_collide(&d.name, &name)) {
                if on_conflict == OnConflict::Reject {
                    return Err(StoreError::NameTaken {
                        id: existing.id.clone(),
                        name: existing.name.clone(),
                    });
                }
                existing.name = name;
                existing.items = items;
                existing.arrangement = arrangement;
                existing.origin = origin;
                return Ok(existing.clone());
            }
            let def = LayoutDefinition {
                id: new_definition_id(),
                name,
                items,
                arrangement,
                origin,
            };
            layouts.push(def.clone());
            Ok(def)
        })
    }

    /// Save with a layout selected: replace that definition's arrangement
    /// with `project`'s, keeping its id and name.
    pub fn update(
        &self,
        project: &Path,
        id: &str,
        items: Vec<ItemRef>,
        arrangement: Arrangement,
    ) -> Result<LayoutDefinition, StoreError> {
        validate(&arrangement, &items)?;
        reject_builtin_id(id)?;
        let origin = Some(project_key(project));
        self.change(|layouts| {
            let def = layouts
                .iter_mut()
                .find(|d| d.id == id)
                .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
            def.items = items;
            def.arrangement = arrangement;
            def.origin = origin;
            Ok(def.clone())
        })
    }

    /// Rename one definition. Its id and arrangement are unchanged. With
    /// [`OnConflict::Replace`] the other definition holding the name is
    /// deleted; changing only the case of its own name never conflicts.
    pub fn rename(
        &self,
        id: &str,
        name: &str,
        on_conflict: OnConflict,
    ) -> Result<LayoutDefinition, StoreError> {
        let name = validate_name(name)?;
        reject_builtin_id(id)?;
        reject_builtin_name(&name)?;
        self.change(|layouts| {
            if !layouts.iter().any(|d| d.id == id) {
                return Err(StoreError::NotFound(id.to_string()));
            }
            if let Some(other) = layouts
                .iter()
                .find(|d| d.id != id && names_collide(&d.name, &name))
            {
                if on_conflict == OnConflict::Reject {
                    return Err(StoreError::NameTaken {
                        id: other.id.clone(),
                        name: other.name.clone(),
                    });
                }
                let other = other.id.clone();
                layouts.retain(|d| d.id != other);
            }
            let def = layouts
                .iter_mut()
                .find(|d| d.id == id)
                .expect("checked above");
            def.name = name;
            Ok(def.clone())
        })
    }

    /// Delete one definition. Deleting one that is already gone succeeds
    /// without writing; the UI shows Custom either way.
    pub fn delete(&self, id: &str) -> Result<(), StoreError> {
        reject_builtin_id(id)?;
        if !self.read()?.layouts.iter().any(|d| d.id == id) {
            return Ok(());
        }
        self.change(|layouts| {
            layouts.retain(|d| d.id != id);
            Ok(())
        })
    }

    /// Set the New Project layout to a copy of `def`, or reset it to one
    /// tiled terminal with `None`. Later edits to the source leave it alone.
    pub fn set_new_project_layout(&self, def: Option<&LayoutDefinition>) -> Result<(), StoreError> {
        if let Some(def) = def {
            validate_name(&def.name)?;
            validate(&def.arrangement, &def.items)?;
        }
        self.write_with(|file| {
            file.new_project = def.cloned();
            Ok(())
        })
    }

    /// Re-read, apply `edit` to the saved layouts only, write.
    fn change<R>(
        &self,
        edit: impl FnOnce(&mut Vec<LayoutDefinition>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        self.write_with(|file| edit(&mut file.layouts))
    }

    fn write_with<R>(
        &self,
        edit: impl FnOnce(&mut LibraryFile) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let mut file = self.read()?;
        let out = edit(&mut file)?;
        file.version = LIBRARY_VERSION;
        crate::config::save_json_in(&self.dir, LIBRARY_FILE, &file).map_err(StoreError::Io)?;
        Ok(out)
    }

    /// Read the file fresh. Missing = empty. Unreadable = error (so no write
    /// follows). Newer version = error, before anything can back it up or
    /// replace it. Version 1 = migrated in memory; the file keeps that shape
    /// until the next write, so an older build can still use it until then.
    /// Malformed = the shared recovery: `load_json_from` backs the bytes up
    /// (or blocks writes if it cannot) and yields the default.
    fn read(&self) -> Result<LibraryFile, StoreError> {
        let path = self.path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LibraryFile::default()),
            Err(e) => {
                return Err(StoreError::Io(format!(
                    "could not read {}: {e}",
                    path.display()
                )));
            }
        };
        let version = serde_json::from_slice::<VersionProbe>(&bytes).map(|p| p.version);
        if let Ok(v) = version
            && v > LIBRARY_VERSION
        {
            return Err(StoreError::FutureVersion(v));
        }
        let v1 = || serde_json::from_slice::<LibraryFileV1>(&bytes).map(LibraryFile::from);
        let v2 = || serde_json::from_slice::<LibraryFile>(&bytes);
        let parsed = match version {
            Ok(1) => v1(),
            // No version key: only a hand edit writes that, and version 1
            // read it as itself. Let the shape decide.
            Ok(0) => v1().or_else(|_| v2()),
            _ => v2(),
        };
        match parsed {
            Ok(file) => Ok(file),
            Err(_) => Ok(crate::config::load_json_from(&self.dir, LIBRARY_FILE)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_layout::{NormRect, SplitAxis, TreeSpec, WindowSpec};

    fn term(id: u64) -> ItemRef {
        ItemRef::new(id, ItemKind::Terminal)
    }

    /// Two terminals side by side.
    fn pair() -> (Vec<ItemRef>, Arrangement) {
        (
            vec![term(1), term(2)],
            Arrangement {
                windows: vec![WindowSpec::tiled(vec![1], 0), WindowSpec::tiled(vec![2], 0)],
                tree: Some(TreeSpec::Split {
                    dir: SplitAxis::H,
                    ratios: vec![0.5, 0.5],
                    children: vec![TreeSpec::Leaf { window: 0 }, TreeSpec::Leaf { window: 1 }],
                }),
            },
        )
    }

    /// One floating terminal.
    fn single() -> (Vec<ItemRef>, Arrangement) {
        (
            vec![term(7)],
            Arrangement {
                windows: vec![WindowSpec::floating(
                    vec![7],
                    0,
                    NormRect::new(0.1, 0.1, 0.5, 0.5),
                )],
                tree: None,
            },
        )
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        config: PathBuf,
        project: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let project = tmp.path().join("repo");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Fixture {
            config,
            project,
            _tmp: tmp,
        }
    }

    impl Fixture {
        fn store(&self) -> LayoutStore {
            LayoutStore::at(&self.config)
        }
        fn save(&self, name: &str) -> LayoutDefinition {
            let (items, arrangement) = pair();
            self.store()
                .create(&self.project, name, items, arrangement, OnConflict::Reject)
                .unwrap()
        }
        fn saved(&self) -> Vec<LayoutDefinition> {
            self.store().layouts().unwrap()
        }
        fn names(&self) -> Vec<String> {
            self.saved().into_iter().map(|d| d.name).collect()
        }
        fn file(&self) -> PathBuf {
            self.config.join(LIBRARY_FILE)
        }
        /// Another existing project directory next to `project`.
        fn other(&self, name: &str) -> PathBuf {
            let dir = self.project.parent().unwrap().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }
    }

    // ── keys ──────────────────────────────────────────────────────────────

    #[test]
    fn equivalent_spellings_of_one_directory_share_a_key() {
        let f = fixture();
        let key = project_key(&f.project);
        assert_eq!(project_key(&f.project.join(".")), key);
        assert_eq!(project_key(&f.project.join("sub").join("..")), key);
        let mut trailing = f.project.clone().into_os_string();
        trailing.push(std::path::MAIN_SEPARATOR_STR);
        assert_eq!(project_key(Path::new(&trailing)), key);
        if cfg!(windows) {
            let upper = f.project.to_string_lossy().to_uppercase();
            assert_eq!(project_key(Path::new(&upper)), key);
            assert!(!key.starts_with(r"\\?\"), "{key}");
        }
    }

    #[test]
    fn an_unreachable_directory_still_has_a_stable_key() {
        let f = fixture();
        let gone = f.project.join("moved-away");
        assert_eq!(project_key(&gone), project_key(&gone.join(".")));
        assert_ne!(project_key(&gone), project_key(&f.project));
    }

    /// CI's temp dir is `C:\Users\RUNNER~1\...`: a short name in a parent
    /// that outlives the project directory must not change its key. Proven
    /// here with a short name of our own so it fails on any machine.
    #[cfg(windows)]
    #[test]
    fn a_short_name_parent_keeps_the_key_after_the_directory_goes() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let f = fixture();
        let parent = f.project.join("a-parent-longer-than-eight");
        std::fs::create_dir_all(parent.join("repo")).unwrap();
        let wide: Vec<u16> = parent.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: NUL-terminated input; the length passed is the buffer's.
        let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        assert!(n > 0 && (n as usize) < buf.len(), "GetShortPathNameW: {n}");
        let short = PathBuf::from(std::ffi::OsString::from_wide(&buf[..n as usize]));
        if short == parent {
            eprintln!("8.3 names are off on this volume; nothing to prove");
            return;
        }
        let repo = short.join("repo");
        let before = project_key(&repo);
        std::fs::remove_dir_all(parent.join("repo")).unwrap();
        assert_eq!(project_key(&repo), before);
    }

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\a")),
            PathBuf::from(r"C:\a")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share\a")),
            PathBuf::from(r"\\srv\share\a")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from(r"C:\a")),
            PathBuf::from(r"C:\a")
        );
    }

    // ── picker contract ───────────────────────────────────────────────────

    #[test]
    fn missing_file_is_an_empty_library() {
        let f = fixture();
        assert_eq!(f.store().layouts(), Ok(vec![]));
        assert_eq!(f.store().new_project_layout(), Ok(None));
        assert!(!f.file().exists(), "reading never creates the file");
    }

    #[test]
    fn save_trims_the_name_and_persists_a_valid_definition() {
        let f = fixture();
        let def = f.save("  Coding  ");
        assert_eq!(def.name, "Coding");
        assert!(!def.id.is_empty());
        assert!(!is_builtin(&def.id));
        assert_eq!(f.saved(), vec![def]);
    }

    #[test]
    fn save_rejects_bad_names_and_arrangements_without_writing() {
        let f = fixture();
        let (items, arrangement) = pair();
        let store = f.store();
        let err = store
            .create(
                &f.project,
                "   ",
                items.clone(),
                arrangement.clone(),
                OnConflict::Reject,
            )
            .unwrap_err();
        assert_eq!(err, StoreError::Invalid(LayoutError::EmptyName));
        let long = "x".repeat(81);
        assert!(matches!(
            store.create(
                &f.project,
                &long,
                items.clone(),
                arrangement.clone(),
                OnConflict::Reject
            ),
            Err(StoreError::Invalid(LayoutError::NameTooLong(81)))
        ));
        let mut broken = arrangement;
        broken.windows[0].tabs.clear();
        assert!(matches!(
            store.create(&f.project, "ok", items, broken, OnConflict::Reject),
            Err(StoreError::Invalid(LayoutError::EmptyStack(0)))
        ));
        assert!(!f.file().exists());
        // Exactly 80 characters is allowed.
        let (items, arrangement) = pair();
        store
            .create(
                &f.project,
                &"y".repeat(80),
                items,
                arrangement,
                OnConflict::Reject,
            )
            .unwrap();
    }

    #[test]
    fn duplicate_names_are_case_insensitive_and_need_explicit_replacement() {
        let f = fixture();
        let first = f.save("Coding");
        let (items, arrangement) = single();
        let err = f
            .store()
            .create(
                &f.project,
                "CODING",
                items.clone(),
                arrangement.clone(),
                OnConflict::Reject,
            )
            .unwrap_err();
        assert_eq!(
            err,
            StoreError::NameTaken {
                id: first.id.clone(),
                name: "Coding".into()
            }
        );
        assert_eq!(f.saved(), vec![first.clone()]);

        let replaced = f
            .store()
            .create(
                &f.project,
                "CODING",
                items.clone(),
                arrangement.clone(),
                OnConflict::Replace,
            )
            .unwrap();
        assert_eq!(
            replaced.id, first.id,
            "replacement keeps the definition's id"
        );
        assert_eq!(replaced.name, "CODING");
        assert_eq!(replaced.arrangement, arrangement);
        assert_eq!(f.saved(), vec![replaced]);
    }

    #[test]
    fn save_as_creates_a_separate_definition() {
        let f = fixture();
        let a = f.save("A");
        let b = f.save("B");
        assert_ne!(a.id, b.id);
        assert_eq!(f.names(), ["A", "B"], "creation order");
    }

    #[test]
    fn update_replaces_only_the_arrangement_of_the_selected_definition() {
        let f = fixture();
        let a = f.save("A");
        let b = f.save("B");
        let (items, arrangement) = single();
        let updated = f
            .store()
            .update(&f.project, &a.id, items.clone(), arrangement.clone())
            .unwrap();
        assert_eq!(
            (updated.id.as_str(), updated.name.as_str()),
            (a.id.as_str(), "A")
        );
        assert_eq!(
            (updated.items.clone(), updated.arrangement.clone()),
            (items, arrangement)
        );
        assert_eq!(f.saved(), vec![updated, b]);
    }

    #[test]
    fn update_of_a_definition_deleted_elsewhere_reports_not_found() {
        let f = fixture();
        let a = f.save("A");
        f.store().delete(&a.id).unwrap();
        let (items, arrangement) = single();
        assert_eq!(
            f.store().update(&f.project, &a.id, items, arrangement),
            Err(StoreError::NotFound(a.id.clone()))
        );
        assert!(f.names().is_empty(), "a failed update never recreates it");
    }

    #[test]
    fn rename_keeps_id_and_arrangement() {
        let f = fixture();
        let a = f.save("A");
        let renamed = f
            .store()
            .rename(&a.id, " Review ", OnConflict::Reject)
            .unwrap();
        assert_eq!(renamed.id, a.id);
        assert_eq!(renamed.name, "Review");
        assert_eq!(renamed.arrangement, a.arrangement);
        // Re-casing its own name is not a conflict.
        f.store()
            .rename(&a.id, "REVIEW", OnConflict::Reject)
            .unwrap();
        assert_eq!(f.names(), ["REVIEW"]);
    }

    #[test]
    fn rename_onto_a_taken_name_rejects_or_replaces() {
        let f = fixture();
        let a = f.save("A");
        let b = f.save("B");
        assert_eq!(
            f.store().rename(&a.id, "b", OnConflict::Reject),
            Err(StoreError::NameTaken {
                id: b.id.clone(),
                name: "B".into()
            })
        );
        assert_eq!(f.names(), ["A", "B"]);
        f.store().rename(&a.id, "b", OnConflict::Replace).unwrap();
        let left = f.saved();
        assert_eq!(left.len(), 1);
        assert_eq!(
            (left[0].id.as_str(), left[0].name.as_str()),
            (a.id.as_str(), "b")
        );
    }

    #[test]
    fn rename_and_delete_of_a_missing_id() {
        let f = fixture();
        f.save("A");
        assert_eq!(
            f.store().rename("nope", "X", OnConflict::Reject),
            Err(StoreError::NotFound("nope".into()))
        );
        assert_eq!(f.store().delete("nope"), Ok(()));
        assert_eq!(f.names(), ["A"]);
    }

    #[test]
    fn delete_removes_one_definition() {
        let f = fixture();
        let a = f.save("A");
        let b = f.save("B");
        f.store().delete(&a.id).unwrap();
        assert_eq!(f.names(), ["B"]);
        f.store().delete(&b.id).unwrap();
        let file: LibraryFile = serde_json::from_slice(&std::fs::read(f.file()).unwrap()).unwrap();
        assert!(file.layouts.is_empty(), "{file:?}");
    }

    #[test]
    fn selection_resolves_to_custom_when_its_definition_is_gone() {
        let f = fixture();
        let a = f.save("A");
        let layouts = f.saved();
        assert_eq!(resolve_selection(&layouts, Some(&a.id)), Some(&a));
        assert_eq!(resolve_selection(&layouts, None), None);
        f.store().delete(&a.id).unwrap();
        let layouts = f.saved();
        assert_eq!(resolve_selection(&layouts, Some(&a.id)), None);
    }

    // ── one list for every Project ────────────────────────────────────────

    #[test]
    fn every_project_shares_one_list_and_one_namespace() {
        let f = fixture();
        let a = f.save("Main");
        // A worktree is its own directory, and still sees the same list.
        let worktree = f.project.join(".foreman").join("worktrees").join("abc");
        std::fs::create_dir_all(&worktree).unwrap();
        let (items, arrangement) = single();
        assert_eq!(
            f.store().create(
                &worktree,
                "main",
                items.clone(),
                arrangement.clone(),
                OnConflict::Reject
            ),
            Err(StoreError::NameTaken {
                id: a.id.clone(),
                name: "Main".into()
            }),
            "names collide across Projects"
        );
        let b = f
            .store()
            .create(&worktree, "Review", items, arrangement, OnConflict::Reject)
            .unwrap();
        assert_eq!(f.saved(), vec![a, b]);
    }

    #[test]
    fn a_definition_records_the_directory_its_ids_came_from() {
        let f = fixture();
        let a = f.save("A");
        assert_eq!(a.origin, Some(project_key(&f.project)));
        // Another spelling of the same directory is the same origin.
        std::fs::create_dir_all(f.project.join("sub")).unwrap();
        let alias = f.project.join("sub").join("..");
        let (items, arrangement) = single();
        let b = f
            .store()
            .create(
                &alias,
                "B",
                items.clone(),
                arrangement.clone(),
                OnConflict::Reject,
            )
            .unwrap();
        assert_eq!(b.origin, a.origin);
        // Saving over it from another Project moves the origin there: the
        // items are now that Project's.
        let other = f.other("other");
        let moved = f
            .store()
            .update(&other, &a.id, items.clone(), arrangement.clone())
            .unwrap();
        assert_eq!(moved.origin, Some(project_key(&other)));
        let replaced = f
            .store()
            .create(&other, "b", items, arrangement, OnConflict::Replace)
            .unwrap();
        assert_eq!(replaced.id, b.id);
        assert_eq!(replaced.origin, Some(project_key(&other)));
        // Rename keeps the items, so it keeps the origin.
        let renamed = f.store().rename(&b.id, "Bee", OnConflict::Reject).unwrap();
        assert_eq!(renamed.origin, Some(project_key(&other)));
    }

    #[test]
    fn two_processes_saving_different_definitions_keep_both() {
        // Two stores with no shared state stand in for the installed app and
        // a dev build: each writes after the other has already read once.
        let f = fixture();
        let installed = LayoutStore::at(&f.config);
        let dev = LayoutStore::at(&f.config);
        let other = f.other("other");
        assert!(installed.layouts().unwrap().is_empty());
        assert!(dev.layouts().unwrap().is_empty());
        let (items, arrangement) = pair();
        let a = installed
            .create(
                &f.project,
                "A",
                items.clone(),
                arrangement.clone(),
                OnConflict::Reject,
            )
            .unwrap();
        let b = dev
            .create(
                &f.project,
                "B",
                items.clone(),
                arrangement.clone(),
                OnConflict::Reject,
            )
            .unwrap();
        let c = dev
            .create(&other, "C", items, arrangement, OnConflict::Reject)
            .unwrap();
        installed.set_new_project_layout(Some(&c)).unwrap();
        dev.delete(&b.id).unwrap();
        assert_eq!(installed.layouts().unwrap(), vec![a, c.clone()]);
        assert_eq!(dev.new_project_layout().unwrap(), Some(c));
    }

    // ── built-ins ─────────────────────────────────────────────────────────

    #[test]
    fn builtins_are_valid_distinct_and_carry_no_origin() {
        let all = builtin_layouts();
        assert!(all.len() >= 3, "a few to start from");
        for (i, def) in all.iter().enumerate() {
            assert!(is_builtin(&def.id), "{}", def.id);
            assert_eq!(validate_name(&def.name).as_deref(), Ok(def.name.as_str()));
            validate(&def.arrangement, &def.items).unwrap_or_else(|e| panic!("{}: {e}", def.name));
            assert_eq!(def.origin, None, "{}", def.name);
            assert!(
                def.items
                    .iter()
                    .all(|item| item.kind.opens_without_target()),
                "{}: every slot can open on an empty Project",
                def.name
            );
            for other in &all[i + 1..] {
                assert_ne!(def.id, other.id);
                assert!(!names_collide(&def.name, &other.name), "{}", def.name);
            }
        }
        assert!(std::ptr::eq(all, builtin_layouts()), "built once");
    }

    #[test]
    fn builtins_resolve_like_saved_layouts_but_are_not_in_the_file() {
        let f = fixture();
        let a = f.save("A");
        let saved = f.saved();
        let grid = &builtin_layouts()[2];
        assert_eq!(find_layout(&saved, &grid.id), Some(grid));
        assert_eq!(find_layout(&saved, &a.id), Some(&a));
        assert_eq!(resolve_selection(&[], Some(&grid.id)), Some(grid));
        assert_eq!(find_layout(&saved, "builtin:retired"), None);
        assert_eq!(saved, vec![a], "the file holds only saved layouts");
    }

    #[test]
    fn builtins_are_read_only_and_their_names_reserved() {
        let f = fixture();
        let builtin = &builtin_layouts()[0];
        let a = f.save("A");
        let (items, arrangement) = single();
        let reserved = StoreError::ReservedName(builtin.name.clone());
        let read_only = StoreError::ReadOnly(builtin.name.clone());
        for on_conflict in [OnConflict::Reject, OnConflict::Replace] {
            assert_eq!(
                f.store().create(
                    &f.project,
                    &builtin.name.to_uppercase(),
                    items.clone(),
                    arrangement.clone(),
                    on_conflict
                ),
                Err(reserved.clone())
            );
            assert_eq!(
                f.store().rename(&a.id, &builtin.name, on_conflict),
                Err(reserved.clone())
            );
            assert_eq!(
                f.store().rename(&builtin.id, "Mine", on_conflict),
                Err(read_only.clone())
            );
        }
        assert_eq!(
            f.store()
                .update(&f.project, &builtin.id, items, arrangement),
            Err(read_only.clone())
        );
        assert_eq!(f.store().delete(&builtin.id), Err(read_only));
        assert_eq!(f.saved(), vec![a]);
        assert!(reserved.to_string().contains("another name"));
    }

    #[test]
    fn a_builtin_can_be_the_new_project_layout() {
        let f = fixture();
        let builtin = &builtin_layouts()[1];
        f.store().set_new_project_layout(Some(builtin)).unwrap();
        assert_eq!(
            f.store().new_project_layout().unwrap().as_ref(),
            Some(builtin)
        );
    }

    // ── version 1 ─────────────────────────────────────────────────────────

    /// A v1 definition as v1 wrote it: no `origin`.
    fn v1_def(id: &str, name: &str) -> serde_json::Value {
        let (items, arrangement) = pair();
        serde_json::json!({ "id": id, "name": name, "items": items, "arrangement": arrangement })
    }

    #[test]
    fn a_version_1_file_migrates_into_one_list_on_read() {
        let f = fixture();
        let web = f.other("web");
        let api = f.other("api");
        let (web_key, api_key) = (project_key(&web), project_key(&api));
        let v1 = serde_json::json!({
            "version": 1,
            "projects": {
                web_key.clone(): { "dir": web, "layouts": [v1_def("w1", "Coding"), v1_def("w2", "grid")] },
                api_key.clone(): { "dir": api, "layouts": [v1_def("a1", "coding"), v1_def("a2", "Solo")] },
            },
            "new_project": v1_def("n", "Slot"),
        });
        let bytes = serde_json::to_vec_pretty(&v1).unwrap();
        std::fs::write(f.file(), &bytes).unwrap();

        // Keys sort, so api's layouts come first and keep their names.
        // A name another directory or a built-in holds gets its folder.
        let got: Vec<(String, String, String)> = f
            .saved()
            .into_iter()
            .map(|d| (d.id, d.name, d.origin.unwrap()))
            .collect();
        let want = [
            ("a1", "coding", &api_key),
            ("a2", "Solo", &api_key),
            ("w1", "Coding (web)", &web_key),
            ("w2", "grid (web)", &web_key),
        ]
        .map(|(id, name, key)| (id.to_string(), name.to_string(), key.clone()));
        assert_eq!(got, want);
        assert_eq!(
            f.store().new_project_layout().unwrap().map(|d| d.name),
            Some("Slot".into())
        );
        assert_eq!(
            std::fs::read(f.file()).unwrap(),
            bytes,
            "reading leaves a v1 file for older builds"
        );

        // The first write stores the new shape.
        f.save("New");
        let file: LibraryFile = serde_json::from_slice(&std::fs::read(f.file()).unwrap()).unwrap();
        assert_eq!(file.version, LIBRARY_VERSION);
        assert_eq!(file.layouts.len(), 5);
        assert!(
            !std::fs::read_dir(&f.config).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".corrupt-")),
            "a v1 file is migrated, not treated as corrupt"
        );
    }

    #[test]
    fn migrated_names_fit_the_limit_and_never_collide() {
        let long = "n".repeat(MAX_NAME_CHARS);
        let hint = "h".repeat(MIGRATION_HINT_CHARS + 20);
        // The first two candidates are taken.
        let name = disambiguate(&long, &hint, |n| !n.ends_with(" 3)"));
        let cut = "h".repeat(MIGRATION_HINT_CHARS);
        assert!(name.ends_with(&format!(" ({cut} 3)")), "{name}");
        assert!(name.starts_with("nnn"), "{name}");
        assert_eq!(validate_name(&name).as_deref(), Ok(name.as_str()));
    }

    #[test]
    fn a_file_without_a_version_is_read_by_its_shape() {
        let f = fixture();
        let key = project_key(&f.project);
        let v1 = serde_json::json!({
            "projects": { key.clone(): { "dir": f.project, "layouts": [v1_def("a", "A")] } },
        });
        std::fs::write(f.file(), serde_json::to_vec(&v1).unwrap()).unwrap();
        let saved = f.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].origin.as_deref(), Some(key.as_str()));

        let v2 = serde_json::json!({ "layouts": [v1_def("b", "B")] });
        std::fs::write(f.file(), serde_json::to_vec(&v2).unwrap()).unwrap();
        assert_eq!(f.names(), ["B"]);
        assert!(
            !std::fs::read_dir(&f.config).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".corrupt-")),
            "neither shape is treated as corrupt"
        );
    }

    #[test]
    fn a_malformed_version_1_file_is_backed_up() {
        let f = fixture();
        let odd = br#"{"version": 1, "projects": {}, "surprise": true}"#;
        std::fs::write(f.file(), odd).unwrap();
        assert_eq!(f.store().layouts(), Ok(vec![]));
        f.save("A");
        let backup = std::fs::read_dir(&f.config)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().contains(".corrupt-"))
            .expect("the bytes are preserved");
        assert_eq!(std::fs::read(backup).unwrap(), odd);
        crate::config::take_load_warnings();
    }

    // ── New Project slot ──────────────────────────────────────────────────

    #[test]
    fn new_project_slot_is_a_copy_not_a_reference() {
        let f = fixture();
        let a = f.save("A");
        f.store().set_new_project_layout(Some(&a)).unwrap();
        let (items, arrangement) = single();
        f.store()
            .update(&f.project, &a.id, items, arrangement)
            .unwrap();
        f.store()
            .rename(&a.id, "Renamed", OnConflict::Reject)
            .unwrap();
        assert_eq!(f.store().new_project_layout().unwrap(), Some(a.clone()));
        f.store().delete(&a.id).unwrap();
        assert_eq!(f.store().new_project_layout().unwrap(), Some(a));
        f.store().set_new_project_layout(None).unwrap();
        assert_eq!(f.store().new_project_layout().unwrap(), None);
    }

    // ── failures ──────────────────────────────────────────────────────────

    #[test]
    fn a_failed_write_reports_failure_and_changes_nothing() {
        let f = fixture();
        let a = f.save("A");
        let before = std::fs::read(f.file()).unwrap();
        // A directory where the staging file must go makes the write fail
        // after the re-read succeeded.
        let blocked = LayoutStore::at(f.config.join("missing-dir"));
        let (items, arrangement) = pair();
        assert!(matches!(
            blocked.create(
                &f.project,
                "B",
                items.clone(),
                arrangement.clone(),
                OnConflict::Reject
            ),
            Err(StoreError::Io(_))
        ));
        // The real file is untouched by a failed commit too: make it
        // read-only so the rename over it fails.
        let mut perms = std::fs::metadata(f.file()).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(f.file(), perms.clone()).unwrap();
        let result = f
            .store()
            .create(&f.project, "B", items, arrangement, OnConflict::Reject);
        perms.set_readonly(false);
        std::fs::set_permissions(f.file(), perms).unwrap();
        if cfg!(windows) {
            assert!(matches!(result, Err(StoreError::Io(_))), "{result:?}");
            assert_eq!(std::fs::read(f.file()).unwrap(), before);
            assert_eq!(f.saved(), vec![a]);
        }
        let leftovers: Vec<_> = std::fs::read_dir(&f.config)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_future_version_is_never_overwritten() {
        let f = fixture();
        let next = LIBRARY_VERSION + 1;
        let future = format!(r#"{{"version": {next}, "layouts": {{"shape": "new"}}}}"#);
        std::fs::write(f.file(), &future).unwrap();
        assert_eq!(f.store().layouts(), Err(StoreError::FutureVersion(next)));
        let (items, arrangement) = pair();
        assert_eq!(
            f.store()
                .create(&f.project, "A", items, arrangement, OnConflict::Reject),
            Err(StoreError::FutureVersion(next))
        );
        assert_eq!(
            f.store().set_new_project_layout(None),
            Err(StoreError::FutureVersion(next))
        );
        assert_eq!(std::fs::read(f.file()).unwrap(), future.as_bytes());
        let entries = std::fs::read_dir(&f.config).unwrap().count();
        assert_eq!(entries, 1, "a newer file is not backed up as corrupt");
    }

    #[test]
    fn corrupt_data_is_backed_up_before_it_is_replaced() {
        let f = fixture();
        let corrupt = br#"{"version": 2, "layouts": "#;
        std::fs::write(f.file(), corrupt).unwrap();
        assert_eq!(f.store().layouts(), Ok(vec![]));
        let a = f.save("A");
        assert_eq!(f.saved(), vec![a]);
        let backup = std::fs::read_dir(&f.config)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().contains(".corrupt-"))
            .expect("corrupt bytes must be preserved");
        assert_eq!(std::fs::read(backup).unwrap(), corrupt);
        crate::config::take_load_warnings();
    }

    #[test]
    fn unknown_fields_are_preserved_as_corrupt_not_trimmed() {
        let f = fixture();
        let odd = br#"{"version": 2, "layouts": [], "surprise": true}"#;
        std::fs::write(f.file(), odd).unwrap();
        f.save("A");
        let backup = std::fs::read_dir(&f.config)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().contains(".corrupt-"))
            .expect("an unknown field is not silently dropped");
        assert_eq!(std::fs::read(backup).unwrap(), odd);
        crate::config::take_load_warnings();
    }

    #[test]
    fn an_unreadable_file_blocks_writes() {
        let f = fixture();
        // A directory at the file's path cannot be read as a file.
        std::fs::create_dir_all(f.file()).unwrap();
        assert!(matches!(f.store().layouts(), Err(StoreError::Io(_))));
        let (items, arrangement) = pair();
        assert!(matches!(
            f.store()
                .create(&f.project, "A", items, arrangement, OnConflict::Reject),
            Err(StoreError::Io(_))
        ));
        assert!(f.file().is_dir());
    }

    #[test]
    fn errors_read_as_actionable_messages() {
        let taken = StoreError::NameTaken {
            id: "x".into(),
            name: "Coding".into(),
        };
        assert!(taken.to_string().contains("\"Coding\" already exists"));
        assert!(
            StoreError::FutureVersion(3)
                .to_string()
                .contains("Update Foreman")
        );
        assert!(
            StoreError::ReadOnly("Grid".into())
                .to_string()
                .contains("Save As")
        );
    }
}
