//! Saved project layouts on disk: `%APPDATA%\foreman\project-layouts.json`,
//! one library per normalized project directory plus the single New Project
//! slot. Spec: `docs/superpowers/specs/2026-10-16-project-layouts-design.md`
//! (Ownership and boundaries, Persistence).
//!
//! There is no in-memory cache. Every read goes to disk and every change
//! re-reads the file, edits only its own definition (or the New Project
//! slot), and writes the result, so a second Foreman process sharing the
//! file keeps definitions this one never loaded. Selection and Modified are
//! per Project instance and live in the workspace snapshot, not here.

use crate::project_layout::{
    Arrangement, ItemRef, LayoutDefinition, LayoutError, names_collide, validate, validate_name,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

pub const LIBRARY_FILE: &str = "project-layouts.json";
/// Any change to the file's shape, a definition's included, bumps this. An
/// older build then refuses to write the file instead of dropping fields.
pub const LIBRARY_VERSION: u32 = 1;

/// The whole file. Strict on purpose: an unknown field means a newer or
/// hand-edited file, which goes through the shared recovery (backed up before
/// anything replaces it) rather than being silently trimmed on the next write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryFile {
    #[serde(default = "current_version")]
    pub version: u32,
    /// Keyed by [`project_key`].
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectLibrary>,
    /// The New Project layout: a copy, never a reference into a library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_project: Option<LayoutDefinition>,
}

impl Default for LibraryFile {
    fn default() -> Self {
        Self {
            version: LIBRARY_VERSION,
            projects: BTreeMap::new(),
            new_project: None,
        }
    }
}

fn current_version() -> u32 {
    LIBRARY_VERSION
}

/// One project directory's saved layouts, in creation order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLibrary {
    /// The directory as last saved from, for people reading the file. The
    /// map key, not this, decides ownership.
    pub dir: PathBuf,
    pub layouts: Vec<LayoutDefinition>,
}

/// Just enough of the file to refuse a newer one before strict parsing or
/// the recovery backup can touch it.
#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default)]
    version: u32,
}

/// What Save, Save As, and Rename do when another definition in the Project
/// already has the name (compared case-insensitively).
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
    /// Another definition in this Project already uses the name.
    NameTaken { id: String, name: String },
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
                write!(
                    f,
                    "A layout named \"{name}\" already exists in this Project."
                )
            }
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

/// The library key for a project directory: absolute, `.`/`..` resolved,
/// symlinks, junctions, and substituted drives resolved when the directory
/// is reachable, without the `\\?\` prefix or a trailing separator, and
/// case-folded on Windows. An unreachable directory keeps its lexical key,
/// so a library outlives a temporarily missing drive; it only fails to line
/// up with a key taken through an alias that is now unresolvable.
pub fn project_key(dir: &Path) -> String {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let resolved = std::fs::canonicalize(&absolute)
        .map(strip_verbatim)
        .unwrap_or(absolute);
    let mut key = resolved.to_string_lossy().into_owned();
    if cfg!(windows) {
        key = key.replace('/', "\\").to_lowercase();
    }
    while key.len() > 1 && key.ends_with(['\\', '/']) && !key.ends_with(":\\") {
        key.pop();
    }
    key
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

/// The definition a Project instance's selection names, or `None` to show
/// Custom (nothing selected, or deleted elsewhere since).
pub fn resolve_selection<'a>(
    layouts: &'a [LayoutDefinition],
    selected: Option<&str>,
) -> Option<&'a LayoutDefinition> {
    let id = selected?;
    layouts.iter().find(|d| d.id == id)
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

    /// The saved layouts of `project`, in creation order. A missing file is
    /// an empty library. A malformed file is backed up by the shared config
    /// recovery and reads as empty, with a warning toast queued.
    pub fn project_layouts(&self, project: &Path) -> Result<Vec<LayoutDefinition>, StoreError> {
        let mut file = self.read()?;
        Ok(file
            .projects
            .remove(&project_key(project))
            .map(|p| p.layouts)
            .unwrap_or_default())
    }

    /// The New Project layout, `None` when unset (one tiled terminal).
    pub fn new_project_layout(&self) -> Result<Option<LayoutDefinition>, StoreError> {
        Ok(self.read()?.new_project)
    }

    /// Save or Save As with a name: a new definition, or, when the user chose
    /// replacement, the colliding definition overwritten in place (same id).
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
        self.change(project, |layouts| {
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
                return Ok(existing.clone());
            }
            let def = LayoutDefinition {
                id: new_definition_id(),
                name,
                items,
                arrangement,
            };
            layouts.push(def.clone());
            Ok(def)
        })
    }

    /// Save with a layout selected: replace that definition's arrangement,
    /// keeping its id and name.
    pub fn update(
        &self,
        project: &Path,
        id: &str,
        items: Vec<ItemRef>,
        arrangement: Arrangement,
    ) -> Result<LayoutDefinition, StoreError> {
        validate(&arrangement, &items)?;
        self.change(project, |layouts| {
            let def = layouts
                .iter_mut()
                .find(|d| d.id == id)
                .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
            def.items = items;
            def.arrangement = arrangement;
            Ok(def.clone())
        })
    }

    /// Rename one definition. Its id and arrangement are unchanged. With
    /// [`OnConflict::Replace`] the other definition holding the name is
    /// deleted; changing only the case of its own name never conflicts.
    pub fn rename(
        &self,
        project: &Path,
        id: &str,
        name: &str,
        on_conflict: OnConflict,
    ) -> Result<LayoutDefinition, StoreError> {
        let name = validate_name(name)?;
        self.change(project, |layouts| {
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
    pub fn delete(&self, project: &Path, id: &str) -> Result<(), StoreError> {
        let key = project_key(project);
        if !self
            .read()?
            .projects
            .get(&key)
            .is_some_and(|p| p.layouts.iter().any(|d| d.id == id))
        {
            return Ok(());
        }
        self.change(project, |layouts| {
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

    /// Re-read, apply `edit` to `project`'s layouts only, write. An emptied
    /// library drops its entry so deleted Projects do not linger as keys.
    fn change<R>(
        &self,
        project: &Path,
        edit: impl FnOnce(&mut Vec<LayoutDefinition>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let key = project_key(project);
        self.write_with(|file| {
            let entry = file.projects.entry(key.clone()).or_default();
            entry.dir = project.to_path_buf();
            let out = edit(&mut entry.layouts)?;
            if entry.layouts.is_empty() {
                file.projects.remove(&key);
            }
            Ok(out)
        })
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
    /// replace it. Malformed = the shared recovery: `load_json_from` backs
    /// the bytes up (or blocks writes if it cannot) and yields the default.
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
        if let Ok(probe) = serde_json::from_slice::<VersionProbe>(&bytes)
            && probe.version > LIBRARY_VERSION
        {
            return Err(StoreError::FutureVersion(probe.version));
        }
        match serde_json::from_slice::<LibraryFile>(&bytes) {
            Ok(file) => Ok(file),
            Err(_) => Ok(crate::config::load_json_from(&self.dir, LIBRARY_FILE)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_layout::{ItemKind, NormRect, SplitAxis, TreeSpec, WindowSpec};

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
        fn names(&self) -> Vec<String> {
            self.store()
                .project_layouts(&self.project)
                .unwrap()
                .into_iter()
                .map(|d| d.name)
                .collect()
        }
        fn file(&self) -> PathBuf {
            self.config.join(LIBRARY_FILE)
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
        assert_eq!(f.store().project_layouts(&f.project), Ok(vec![]));
        assert_eq!(f.store().new_project_layout(), Ok(None));
        assert!(!f.file().exists(), "reading never creates the file");
    }

    #[test]
    fn save_trims_the_name_and_persists_a_valid_definition() {
        let f = fixture();
        let def = f.save("  Coding  ");
        assert_eq!(def.name, "Coding");
        assert!(!def.id.is_empty());
        let back = f.store().project_layouts(&f.project).unwrap();
        assert_eq!(back, vec![def]);
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
        assert_eq!(
            f.store().project_layouts(&f.project).unwrap(),
            vec![first.clone()]
        );

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
        assert_eq!(
            f.store().project_layouts(&f.project).unwrap(),
            vec![replaced]
        );
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
        assert_eq!(
            f.store().project_layouts(&f.project).unwrap(),
            vec![updated, b]
        );
    }

    #[test]
    fn update_of_a_definition_deleted_elsewhere_reports_not_found() {
        let f = fixture();
        let a = f.save("A");
        f.store().delete(&f.project, &a.id).unwrap();
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
            .rename(&f.project, &a.id, " Review ", OnConflict::Reject)
            .unwrap();
        assert_eq!(renamed.id, a.id);
        assert_eq!(renamed.name, "Review");
        assert_eq!(renamed.arrangement, a.arrangement);
        // Re-casing its own name is not a conflict.
        f.store()
            .rename(&f.project, &a.id, "REVIEW", OnConflict::Reject)
            .unwrap();
        assert_eq!(f.names(), ["REVIEW"]);
    }

    #[test]
    fn rename_onto_a_taken_name_rejects_or_replaces() {
        let f = fixture();
        let a = f.save("A");
        let b = f.save("B");
        assert_eq!(
            f.store().rename(&f.project, &a.id, "b", OnConflict::Reject),
            Err(StoreError::NameTaken {
                id: b.id.clone(),
                name: "B".into()
            })
        );
        assert_eq!(f.names(), ["A", "B"]);
        f.store()
            .rename(&f.project, &a.id, "b", OnConflict::Replace)
            .unwrap();
        let left = f.store().project_layouts(&f.project).unwrap();
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
            f.store()
                .rename(&f.project, "nope", "X", OnConflict::Reject),
            Err(StoreError::NotFound("nope".into()))
        );
        assert_eq!(f.store().delete(&f.project, "nope"), Ok(()));
        assert_eq!(f.names(), ["A"]);
    }

    #[test]
    fn delete_removes_one_definition_and_drops_an_empty_library() {
        let f = fixture();
        let a = f.save("A");
        let b = f.save("B");
        f.store().delete(&f.project, &a.id).unwrap();
        assert_eq!(f.names(), ["B"]);
        f.store().delete(&f.project, &b.id).unwrap();
        let file: LibraryFile = serde_json::from_slice(&std::fs::read(f.file()).unwrap()).unwrap();
        assert!(file.projects.is_empty(), "{file:?}");
    }

    #[test]
    fn selection_resolves_to_custom_when_its_definition_is_gone() {
        let f = fixture();
        let a = f.save("A");
        let layouts = f.store().project_layouts(&f.project).unwrap();
        assert_eq!(resolve_selection(&layouts, Some(&a.id)), Some(&a));
        assert_eq!(resolve_selection(&layouts, None), None);
        f.store().delete(&f.project, &a.id).unwrap();
        let layouts = f.store().project_layouts(&f.project).unwrap();
        assert_eq!(resolve_selection(&layouts, Some(&a.id)), None);
    }

    // ── ownership ─────────────────────────────────────────────────────────

    #[test]
    fn two_instances_of_one_directory_share_definitions() {
        let f = fixture();
        let a = f.save("A");
        // A second Project instance reaches the same directory another way.
        let alias = f.project.join("sub").join("..");
        std::fs::create_dir_all(f.project.join("sub")).unwrap();
        assert_eq!(f.store().project_layouts(&alias).unwrap(), vec![a]);
        let (items, arrangement) = single();
        f.store()
            .create(&alias, "B", items, arrangement, OnConflict::Reject)
            .unwrap();
        assert_eq!(f.names(), ["A", "B"]);
    }

    #[test]
    fn distinct_directories_and_worktrees_stay_isolated() {
        let f = fixture();
        f.save("Main");
        let worktree = f.project.join(".foreman").join("worktrees").join("abc");
        std::fs::create_dir_all(&worktree).unwrap();
        assert_eq!(f.store().project_layouts(&worktree), Ok(vec![]));
        let (items, arrangement) = single();
        f.store()
            .create(&worktree, "Main", items, arrangement, OnConflict::Reject)
            .expect("names only collide within one Project");
        assert_eq!(f.names(), ["Main"]);
        assert_eq!(f.store().project_layouts(&worktree).unwrap().len(), 1);
    }

    #[test]
    fn libraries_survive_the_directory_disappearing() {
        let f = fixture();
        let a = f.save("A");
        std::fs::remove_dir_all(&f.project).unwrap();
        assert_eq!(f.store().project_layouts(&f.project).unwrap(), vec![a]);
    }

    #[test]
    fn two_processes_saving_different_definitions_keep_both() {
        // Two stores with no shared state stand in for the installed app and
        // a dev build: each writes after the other has already read once.
        let f = fixture();
        let installed = LayoutStore::at(&f.config);
        let dev = LayoutStore::at(&f.config);
        let other = f.project.join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert!(installed.project_layouts(&f.project).unwrap().is_empty());
        assert!(dev.project_layouts(&f.project).unwrap().is_empty());
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
        dev.delete(&f.project, &b.id).unwrap();
        assert_eq!(installed.project_layouts(&f.project).unwrap(), vec![a]);
        assert_eq!(installed.project_layouts(&other).unwrap(), vec![c.clone()]);
        assert_eq!(dev.new_project_layout().unwrap(), Some(c));
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
            .rename(&f.project, &a.id, "Renamed", OnConflict::Reject)
            .unwrap();
        assert_eq!(f.store().new_project_layout().unwrap(), Some(a.clone()));
        f.store().delete(&f.project, &a.id).unwrap();
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
            assert_eq!(f.store().project_layouts(&f.project).unwrap(), vec![a]);
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
        let future = br#"{"version": 2, "projects": {"x": {"shape": "new"}}}"#;
        std::fs::write(f.file(), future).unwrap();
        assert_eq!(
            f.store().project_layouts(&f.project),
            Err(StoreError::FutureVersion(2))
        );
        let (items, arrangement) = pair();
        assert_eq!(
            f.store()
                .create(&f.project, "A", items, arrangement, OnConflict::Reject),
            Err(StoreError::FutureVersion(2))
        );
        assert_eq!(
            f.store().set_new_project_layout(None),
            Err(StoreError::FutureVersion(2))
        );
        assert_eq!(std::fs::read(f.file()).unwrap(), future);
        let entries = std::fs::read_dir(&f.config).unwrap().count();
        assert_eq!(entries, 1, "a newer file is not backed up as corrupt");
    }

    #[test]
    fn corrupt_data_is_backed_up_before_it_is_replaced() {
        let f = fixture();
        let corrupt = br#"{"version": 1, "projects": "#;
        std::fs::write(f.file(), corrupt).unwrap();
        assert_eq!(f.store().project_layouts(&f.project), Ok(vec![]));
        let a = f.save("A");
        assert_eq!(f.store().project_layouts(&f.project).unwrap(), vec![a]);
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
        let odd = br#"{"version": 1, "projects": {}, "surprise": true}"#;
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
        assert!(matches!(
            f.store().project_layouts(&f.project),
            Err(StoreError::Io(_))
        ));
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
    }
}
