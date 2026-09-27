//! Live repository change detection for Git Changes and Git History.
//!
//! One `RepoWatch` per worktree root, shared by every view on it through a
//! registry of weak references: the last view to drop its `Arc` stops the
//! thread. The thread waits on `ReadDirectoryChangesW` (recursive) for the
//! worktree root, plus the common git dir when it sits outside it (a linked
//! worktree), sorts each changed path into "the working tree changed" and/or
//! "a ref moved", debounces, and bumps a generation counter per kind.
//!
//! Views compare generations instead of reading a flag, so two windows on one
//! watcher never eat each other's signal. Nothing Git runs here except the
//! ignored-path listing; the views do their own reads.
//!
//! The classifier and debounce are pure (no Win32) and table-tested.
use super::git;
use eframe::egui;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

/// Trailing quiet time before a burst is reported.
const QUIET: Duration = Duration::from_millis(300);
/// A steady stream of events still reports this often.
const MAX_WAIT: Duration = Duration::from_secs(2);
/// After the kernel buffer overflowed (events lost), let the storm settle,
/// then report everything changed.
const OVERFLOW_WAIT: Duration = Duration::from_millis(500);
/// The ignored-path list is re-read at most this often on working-tree
/// churn, so a `target/` created after the window opened stops waking it.
const IGNORE_STALE: Duration = Duration::from_secs(10);

/// What one changed path means for the views.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Hit {
    /// The working tree or index changed: Git Changes re-reads.
    pub(super) worktree: bool,
    /// A ref or HEAD moved: Git History re-fingerprints.
    pub(super) refs: bool,
    /// `.gitignore`, `info/exclude` or config changed: re-list ignored paths.
    pub(super) ignore: bool,
}
impl Hit {
    const NONE: Self = Self {
        worktree: false,
        refs: false,
        ignore: false,
    };
    const ALL: Self = Self {
        worktree: true,
        refs: true,
        ignore: true,
    };
    const WORKTREE: Self = Self {
        worktree: true,
        ..Self::NONE
    };
    const REFS: Self = Self {
        refs: true,
        ..Self::NONE
    };
    fn any(self) -> bool {
        self.worktree || self.refs || self.ignore
    }
    fn or(self, other: Self) -> Self {
        Self {
            worktree: self.worktree || other.worktree,
            refs: self.refs || other.refs,
            ignore: self.ignore || other.ignore,
        }
    }
}

/// `/`-separated, lowercase, no trailing slash: the one form every path is
/// compared in. Windows paths are case-insensitive.
pub(super) fn norm(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// `path` relative to `dir` ("" when equal), on a component boundary.
fn under<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(dir)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix('/')
    }
}

/// An 8.3 short name (`PACKED~1`, `GIT~1`): up to six characters, `~`, a
/// digit. The watcher thread expands these first; one that could not be
/// expanded (already deleted) is classified conservatively.
fn short_name(component: &str) -> bool {
    let base = component.split('.').next().unwrap_or("");
    base.len() <= 8
        && base
            .find('~')
            .is_some_and(|i| i > 0 && base[i + 1..].starts_with(|c: char| c.is_ascii_digit()))
}

/// The three places Git keeps things, normalized with `norm`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Roots {
    pub(super) worktree: String,
    /// This worktree's own git dir: `HEAD`, `index`, merge/rebase state.
    pub(super) git_dir: String,
    /// Shared by every worktree: refs, packed-refs, config.
    pub(super) common_dir: String,
}

/// Ignored paths from `git ls-files --others --ignored --exclude-standard
/// --directory -z`, relative to the worktree root and normalized; ignored
/// directories keep their trailing `/`. Load-bearing: card worktrees live
/// inside the main checkout (`.foreman/worktrees/<id>`), and `target/` churns
/// on every build.
#[derive(Debug, Default)]
pub(super) struct Ignored(HashSet<String>);
impl Ignored {
    pub(super) fn parse(bytes: &[u8]) -> Self {
        Self(
            bytes
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).replace('\\', "/").to_lowercase())
                .collect(),
        )
    }
    /// Whether `rel` is an ignored file, an ignored directory, or inside one.
    fn covers(&self, rel: &str) -> bool {
        if self.0.is_empty() {
            return false;
        }
        if self.0.contains(rel) || self.0.contains(&format!("{rel}/")) {
            return true;
        }
        rel.match_indices('/')
            .any(|(i, _)| self.0.contains(&rel[..=i]))
    }
}

/// Sort one changed path (absolute, normalized) into what it means.
/// Mirrors JetBrains' GitRepositoryFiles: HEAD and index from this
/// worktree's git dir, refs from the common dir, and nothing from objects,
/// logs, FETCH_HEAD, ORIG_HEAD or lock files (git writes `index.lock` and
/// renames it onto `index`; the rename is the event that counts).
pub(super) fn classify(roots: &Roots, ignored: &Ignored, path: &str) -> Hit {
    let git_rel = under(path, &roots.git_dir)
        .map(|r| (r, true))
        .or_else(|| under(path, &roots.common_dir).map(|r| (r, roots.common_dir == roots.git_dir)));
    if let Some((rel, own)) = git_rel {
        if rel.ends_with(".lock") {
            return Hit::NONE;
        }
        if rel.split('/').any(short_name) {
            return Hit::WORKTREE.or(Hit::REFS);
        }
        let first = rel.split('/').next().unwrap_or("");
        // A linked worktree's git dir sits inside the common dir; its own
        // files are matched above, everyone else's are not ours.
        if own {
            match rel {
                "head" => return Hit::WORKTREE.or(Hit::REFS),
                "index" => return Hit::WORKTREE,
                "merge_head" => return Hit::REFS,
                _ if first == "rebase-merge" || first == "rebase-apply" => return Hit::REFS,
                _ => {}
            }
        }
        if under(path, &roots.common_dir).is_none() {
            return Hit::NONE;
        }
        let rel = under(path, &roots.common_dir).unwrap_or(rel);
        let refs = ["refs/heads", "refs/remotes", "refs/tags"]
            .iter()
            .any(|d| under(rel, d).is_some())
            || rel == "packed-refs"
            || under(rel, "reftable").is_some();
        return if refs {
            Hit::REFS
        } else if rel == "config" || rel == "info/exclude" {
            Hit {
                ignore: true,
                ..Hit::NONE
            }
        } else {
            Hit::NONE
        };
    }
    let Some(rel) = under(path, &roots.worktree) else {
        return Hit::NONE;
    };
    if rel.is_empty() {
        return Hit::NONE;
    }
    if rel == ".gitignore" || rel.ends_with("/.gitignore") {
        return Hit {
            refs: false,
            ..Hit::ALL
        };
    }
    if ignored.covers(rel) {
        return Hit::NONE;
    }
    // `.git` itself may arrive as `GIT~1`.
    if short_name(rel.split('/').next().unwrap_or("")) {
        return Hit::WORKTREE.or(Hit::REFS);
    }
    Hit::WORKTREE
}

/// Trailing debounce with a max wait. Events that mean nothing never start
/// or extend a window.
#[derive(Debug, Default)]
pub(super) struct Debounce {
    pending: Hit,
    first: Option<Instant>,
    last: Option<Instant>,
    /// Not before this: set by an overflow.
    hold: Option<Instant>,
}
impl Debounce {
    pub(super) fn event(&mut self, now: Instant, hit: Hit) {
        if !hit.any() {
            return;
        }
        self.pending = self.pending.or(hit);
        self.first.get_or_insert(now);
        self.last = Some(now);
    }
    /// The kernel dropped events: assume everything changed, once things settle.
    pub(super) fn overflow(&mut self, now: Instant) {
        self.event(now, Hit::ALL);
        self.hold = Some(now + OVERFLOW_WAIT);
    }
    pub(super) fn deadline(&self) -> Option<Instant> {
        let due = (self.last? + QUIET).min(self.first? + MAX_WAIT);
        Some(self.hold.map_or(due, |h| due.max(h)))
    }
    /// The burst's meaning once its deadline has passed.
    pub(super) fn take(&mut self, now: Instant) -> Option<Hit> {
        if self.deadline()? > now {
            return None;
        }
        let hit = self.pending;
        *self = Self::default();
        Some(hit)
    }
}

struct Shared {
    worktree_gen: AtomicU64,
    refs_gen: AtomicU64,
    /// False once the thread has exited and closed its handles.
    alive: AtomicBool,
    ctx: Mutex<egui::Context>,
}
impl Shared {
    fn bump(&self, hit: Hit) {
        if hit.worktree {
            self.worktree_gen.fetch_add(1, Ordering::Relaxed);
        }
        if hit.refs {
            self.refs_gen.fetch_add(1, Ordering::Relaxed);
        }
        if hit.worktree || hit.refs {
            self.ctx.lock().unwrap().request_repaint();
        }
    }
}

/// An owned Win32 handle, closed on drop.
struct Handle(windows_sys::Win32::Foundation::HANDLE);
// SAFETY: a kernel handle is a process-wide value; the functions used on it
// here (SetEvent, waits, CloseHandle) are thread-safe.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

pub(super) struct RepoWatch {
    shared: Arc<Shared>,
    stop: Arc<Handle>,
}
impl Drop for RepoWatch {
    /// Only signals: the thread cancels its reads and closes its handles
    /// itself, so no view ever blocks on it.
    fn drop(&mut self) {
        // SAFETY: the event handle is alive while `stop` is.
        unsafe { windows_sys::Win32::System::Threading::SetEvent(self.stop.0) };
    }
}
impl RepoWatch {
    pub(super) fn worktree_gen(&self) -> u64 {
        self.shared.worktree_gen.load(Ordering::Relaxed)
    }
    pub(super) fn refs_gen(&self) -> u64 {
        self.shared.refs_gen.load(Ordering::Relaxed)
    }
    /// False once the watch root was deleted or a read failed; views fall
    /// back to reading on focus.
    pub(super) fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Relaxed)
    }
}

static REGISTRY: Mutex<Vec<(String, Weak<RepoWatch>)>> = Mutex::new(Vec::new());

/// Where the repository keeps things, from one `git rev-parse`.
struct Layout {
    roots: Roots,
    /// Directories to watch recursively, as real paths.
    dirs: Vec<PathBuf>,
    worktree: PathBuf,
}

/// Real path without the `\\?\` prefix; `None` for a network path, where
/// `ReadDirectoryChangesW` is unreliable (SMB drops events, `\\wsl$`
/// reports none): those views keep reading on focus.
fn local_path(path: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    let text = real.to_string_lossy();
    let plain = text.strip_prefix(r"\\?\").unwrap_or(&text);
    if plain.starts_with(r"\\") || plain.starts_with("UNC\\") {
        return None;
    }
    let drive: Vec<u16> = format!("{}\\", &plain[..2.min(plain.len())])
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: NUL-terminated buffer.
    let kind = unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(drive.as_ptr()) };
    // DRIVE_REMOTE, which lives behind a feature this crate doesn't enable.
    if kind == 4 {
        return None;
    }
    Some(PathBuf::from(plain))
}

fn discover(cwd: &Path) -> Option<Layout> {
    let out = git::output(
        cwd,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
        ],
        &Arc::new(AtomicBool::new(false)),
        64 << 10,
        Duration::from_secs(10),
    )
    .ok()?;
    let text = String::from_utf8_lossy(&out);
    let mut lines = text.lines().map(str::trim);
    let (top, git_dir, common) = (lines.next()?, lines.next()?, lines.next()?);
    // `--git-common-dir` is relative to the cwd when it can be.
    let worktree = local_path(Path::new(top))?;
    let git_dir = local_path(Path::new(git_dir))?;
    let common = local_path(&cwd.join(common))?;
    let roots = Roots {
        worktree: norm(&worktree.to_string_lossy()),
        git_dir: norm(&git_dir.to_string_lossy()),
        common_dir: norm(&common.to_string_lossy()),
    };
    let mut dirs = vec![worktree.clone()];
    if under(&roots.common_dir, &roots.worktree).is_none() {
        dirs.push(common);
    }
    Some(Layout {
        roots,
        dirs,
        worktree,
    })
}

/// The shared watch for the repository around `cwd`, starting one if none
/// is running. Runs Git: call it on a worker, never the GUI thread. `None`
/// when `cwd` is not in a worktree or the filesystem cannot be watched.
pub(super) fn open(cwd: &Path, ctx: &egui::Context) -> Option<Arc<RepoWatch>> {
    let layout = discover(cwd)?;
    let key = layout.roots.worktree.clone();
    let mut registry = REGISTRY.lock().unwrap();
    registry.retain(|(_, w)| w.strong_count() > 0);
    let found = registry
        .iter()
        .find(|(k, _)| *k == key)
        .and_then(|(_, w)| w.upgrade());
    if let Some(watch) = found.filter(|w| w.alive()) {
        *watch.shared.ctx.lock().unwrap() = ctx.clone();
        return Some(watch);
    }
    let watch = Arc::new(start(layout, ctx.clone())?);
    registry.retain(|(k, _)| *k != key);
    registry.push((key, Arc::downgrade(&watch)));
    Some(watch)
}

fn start(layout: Layout, ctx: egui::Context) -> Option<RepoWatch> {
    use windows_sys::Win32::System::Threading::CreateEventW;
    // SAFETY: plain manual-reset event, no name or security attributes.
    let stop = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if stop.is_null() {
        return None;
    }
    let stop = Arc::new(Handle(stop));
    let shared = Arc::new(Shared {
        worktree_gen: AtomicU64::new(0),
        refs_gen: AtomicU64::new(0),
        alive: AtomicBool::new(true),
        ctx: Mutex::new(ctx),
    });
    let (ready_tx, ready) = mpsc::sync_channel(1);
    let (thread_shared, thread_stop) = (shared.clone(), stop.clone());
    std::thread::Builder::new()
        .name("git-watch".into())
        .spawn(move || {
            run(layout, &thread_shared, &thread_stop, ready_tx);
            thread_shared.alive.store(false, Ordering::Relaxed);
        })
        .ok()?;
    ready.recv().ok()?.then_some(RepoWatch { shared, stop })
}

fn ignored_paths(worktree: &Path) -> Ignored {
    git::output(
        worktree,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
        &Arc::new(AtomicBool::new(false)),
        32 << 20,
        Duration::from_secs(30),
    )
    .map(|b| Ignored::parse(&b))
    .unwrap_or_default()
}

/// One watched directory and its outstanding read.
struct Dir {
    handle: Handle,
    event: Handle,
    overlapped: Box<windows_sys::Win32::System::IO::OVERLAPPED>,
    /// `FILE_NOTIFY_INFORMATION` records are DWORD-aligned.
    buf: Box<[u32; 16 * 1024]>,
    real: PathBuf,
    norm: String,
    reading: bool,
}
impl Dir {
    fn open(real: &Path) -> Option<Self> {
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::Threading::CreateEventW;
        let wide: Vec<u16> = real.as_os_str().encode_wide_nul();
        // SAFETY: NUL-terminated path; sharing everything so the watch never
        // blocks a rename or delete (card teardown renames trees).
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return None;
        }
        let handle = Handle(handle);
        // SAFETY: plain manual-reset event.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return None;
        }
        let mut overlapped: Box<windows_sys::Win32::System::IO::OVERLAPPED> =
            Box::new(unsafe { std::mem::zeroed() });
        overlapped.hEvent = event;
        Some(Self {
            handle,
            event: Handle(event),
            overlapped,
            buf: Box::new([0; 16 * 1024]),
            real: real.to_path_buf(),
            norm: norm(&real.to_string_lossy()),
            reading: false,
        })
    }
    /// Queue the next read. False when the directory can no longer be read
    /// (deleted, or a filesystem without change notifications).
    fn read(&mut self) -> bool {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
            FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, ReadDirectoryChangesW,
        };
        // SAFETY: `buf` and `overlapped` are boxed and outlive the read:
        // `Drop` cancels and waits for it before freeing them.
        let ok = unsafe {
            windows_sys::Win32::System::Threading::ResetEvent(self.event.0);
            ReadDirectoryChangesW(
                self.handle.0,
                self.buf.as_mut_ptr().cast(),
                std::mem::size_of_val(&*self.buf) as u32,
                1,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_DIR_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE,
                std::ptr::null_mut(),
                &mut *self.overlapped,
                None,
            )
        };
        self.reading = ok != 0;
        self.reading
    }
    /// The finished read's byte count; `None` when it failed.
    fn finish(&mut self) -> Option<u32> {
        let mut n = 0;
        // SAFETY: the event fired, so the read is complete.
        let ok = unsafe {
            windows_sys::Win32::System::IO::GetOverlappedResult(
                self.handle.0,
                &*self.overlapped,
                &mut n,
                0,
            )
        };
        self.reading = false;
        (ok != 0).then_some(n)
    }
    /// Changed paths in the finished read, absolute and normalized.
    fn paths(&self, len: u32) -> Vec<String> {
        use windows_sys::Win32::Storage::FileSystem::FILE_NOTIFY_INFORMATION;
        let base = self.buf.as_ptr().cast::<u8>();
        let len = (len as usize).min(std::mem::size_of_val(&*self.buf));
        let mut out = Vec::new();
        let mut offset = 0;
        while offset + std::mem::size_of::<FILE_NOTIFY_INFORMATION>() <= len {
            // SAFETY: the kernel wrote well-formed, DWORD-aligned records
            // within `len` bytes of the buffer.
            let (next, name) = unsafe {
                let info = &*base.add(offset).cast::<FILE_NOTIFY_INFORMATION>();
                let chars = info.FileNameLength as usize / 2;
                let name = std::slice::from_raw_parts(info.FileName.as_ptr(), chars);
                (
                    info.NextEntryOffset as usize,
                    String::from_utf16_lossy(name),
                )
            };
            out.push(self.expand(&name));
            if next == 0 {
                break;
            }
            offset += next;
        }
        out
    }
    /// `rel` joined onto the root, with 8.3 short names expanded where the
    /// file (or failing that, its directory) still exists.
    fn expand(&self, rel: &str) -> String {
        if !rel.split('\\').any(short_name) {
            return format!("{}/{}", self.norm, norm(rel));
        }
        let full = self.real.join(rel);
        let long = long_path(&full).or_else(|| {
            let parent = long_path(full.parent()?)?;
            Some(parent.join(full.file_name()?))
        });
        norm(&long.unwrap_or(full).to_string_lossy())
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        if self.reading {
            let mut n = 0;
            // SAFETY: cancel the outstanding read and wait for it, so the
            // kernel is done with `buf` and `overlapped` before they're freed.
            unsafe {
                windows_sys::Win32::System::IO::CancelIoEx(self.handle.0, &*self.overlapped);
                windows_sys::Win32::System::IO::GetOverlappedResult(
                    self.handle.0,
                    &*self.overlapped,
                    &mut n,
                    1,
                );
            }
        }
    }
}

trait WideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}
impl WideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain([0]).collect()
    }
}

fn long_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let wide = path.as_os_str().encode_wide_nul();
    let mut buf = vec![0u16; 1024];
    // SAFETY: NUL-terminated input, output buffer of the stated length.
    let n = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetLongPathNameW(
            wide.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
        )
    } as usize;
    (n > 0 && n < buf.len()).then(|| PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])))
}

/// The watcher thread. Reports setup success through `ready`, then runs
/// until `stop` is signalled or a directory can no longer be read (its root
/// was deleted); it never retries, so a deleted card worktree is never held.
fn run(layout: Layout, shared: &Shared, stop: &Handle, ready: mpsc::SyncSender<bool>) {
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};
    let mut dirs: Vec<Dir> = Vec::new();
    for real in &layout.dirs {
        match Dir::open(real) {
            Some(mut dir) => {
                if !dir.read() {
                    let _ = ready.send(false);
                    return;
                }
                dirs.push(dir);
            }
            None => {
                let _ = ready.send(false);
                return;
            }
        }
    }
    let _ = ready.send(true);
    // Listed after the reads are queued, so nothing in between is lost.
    let mut ignored = ignored_paths(&layout.worktree);
    let mut listed = Instant::now();
    let mut debounce = Debounce::default();
    let handles: Vec<_> = std::iter::once(stop.0)
        .chain(dirs.iter().map(|d| d.event.0))
        .collect();
    loop {
        let timeout = debounce.deadline().map_or(INFINITE, |d| {
            let left = d.saturating_duration_since(Instant::now());
            (left.as_millis() as u32).saturating_add(1)
        });
        // SAFETY: every handle stays open for the whole loop.
        let woke =
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout) };
        let now = Instant::now();
        if woke == WAIT_OBJECT_0 {
            return;
        }
        if woke != WAIT_TIMEOUT {
            let Some(dir) = (woke.wrapping_sub(WAIT_OBJECT_0) as usize)
                .checked_sub(1)
                .and_then(|i| dirs.get_mut(i))
            else {
                return;
            };
            match dir.finish() {
                None => return,
                // Overflow: the kernel dropped events. A deleted root can
                // also complete empty, so check it still exists.
                Some(0) => {
                    if !dir.real.is_dir() {
                        return;
                    }
                    debounce.overflow(now);
                }
                Some(n) => {
                    for path in dir.paths(n) {
                        debounce.event(now, classify(&layout.roots, &ignored, &path));
                    }
                }
            }
            if !dir.read() {
                return;
            }
        }
        if let Some(hit) = debounce.take(Instant::now()) {
            let stale = hit.worktree && listed.elapsed() >= IGNORE_STALE;
            if hit.ignore || stale {
                ignored = ignored_paths(&layout.worktree);
                listed = Instant::now();
            }
            shared.bump(hit);
        }
    }
}

/// A view's handle on the shared watch: opened lazily on a worker the
/// first time the view is shown.
#[derive(Default)]
pub(super) struct Follow {
    opening: Option<mpsc::Receiver<Option<Arc<RepoWatch>>>>,
    watch: Option<Arc<RepoWatch>>,
    tried: bool,
}
impl Follow {
    /// Never watches, as on a filesystem without change notifications.
    #[cfg(test)]
    pub(super) fn off() -> Self {
        Self {
            tried: true,
            ..Self::default()
        }
    }
    /// The live watch, starting to open one on first call. `None` while
    /// opening, when the repository cannot be watched, or after the watcher
    /// stopped: callers then keep their read-on-focus behavior.
    pub(super) fn live(&mut self, cwd: Option<&Path>, ctx: &egui::Context) -> Option<&RepoWatch> {
        if !self.tried {
            self.tried = true;
            if let Some(cwd) = cwd {
                let (tx, rx) = mpsc::sync_channel(1);
                let (cwd, ctx) = (cwd.to_path_buf(), ctx.clone());
                std::thread::spawn(move || {
                    let watch = open(&cwd, &ctx);
                    if tx.send(watch).is_ok() {
                        ctx.request_repaint();
                    }
                });
                self.opening = Some(rx);
            }
        }
        if let Some(rx) = &self.opening
            && let Ok(watch) = rx.try_recv()
        {
            self.watch = watch;
            self.opening = None;
        }
        self.watch.as_deref().filter(|w| w.alive())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

    fn roots(worktree: &str, git_dir: &str, common: &str) -> Roots {
        Roots {
            worktree: norm(worktree),
            git_dir: norm(git_dir),
            common_dir: norm(common),
        }
    }

    #[test]
    fn classify_sorts_paths_into_worktree_refs_and_ignore() {
        let main = roots("C:/r", "C:/r/.git", "C:/r/.git");
        let linked = roots(
            "C:/r/.foreman/worktrees/a1",
            "C:/r/.git/worktrees/a1",
            "C:/r/.git",
        );
        let ignored = Ignored::parse(b"target/\0.foreman/worktrees/\0notes.log\0");
        const W: Hit = Hit::WORKTREE;
        const R: Hit = Hit::REFS;
        const N: Hit = Hit::NONE;
        let both = W.or(R);
        let ign = Hit { ignore: true, ..N };
        let gi = Hit {
            worktree: true,
            ignore: true,
            refs: false,
        };
        let cases: &[(&Roots, &str, Hit)] = &[
            // Main checkout: its git dir is its common dir.
            (&main, "c:/r/src/main.rs", W),
            (&main, "C:\\R\\SRC\\Main.rs", W),
            (&main, "c:/r/cargo.lock", W),
            (&main, "c:/r/.git/HEAD", both),
            (&main, "c:/r/.git/index", W),
            (&main, "c:/r/.git/index.lock", N),
            (&main, "c:/r/.git/refs/heads/main", R),
            (&main, "c:/r/.git/refs/heads/card/x", R),
            (&main, "c:/r/.git/refs/heads/main.lock", N),
            (&main, "c:/r/.git/refs/remotes/origin/main", R),
            (&main, "c:/r/.git/refs/tags/v1", R),
            (&main, "c:/r/.git/refs/stash", N),
            (&main, "c:/r/.git/packed-refs", R),
            (&main, "c:/r/.git/reftable/tables.list", R),
            (&main, "c:/r/.git/MERGE_HEAD", R),
            (&main, "c:/r/.git/rebase-merge/done", R),
            (&main, "c:/r/.git/rebase-apply", R),
            (&main, "c:/r/.git/objects/ab/cdef", N),
            (&main, "c:/r/.git/logs/HEAD", N),
            (&main, "c:/r/.git/logs/refs/heads/main", N),
            (&main, "c:/r/.git/FETCH_HEAD", N),
            (&main, "c:/r/.git/ORIG_HEAD", N),
            (&main, "c:/r/.git/config", ign),
            (&main, "c:/r/.git/info/exclude", ign),
            // Other worktrees' HEAD and index are theirs.
            (&main, "c:/r/.git/worktrees/a1/HEAD", N),
            (&main, "c:/r/.git/worktrees/a1/index", N),
            // Ignored prefixes, and the directory entries themselves.
            (&main, "c:/r/target/debug/foreman.exe", N),
            (&main, "c:/r/target", N),
            (&main, "c:/r/.foreman/worktrees/a1/src/lib.rs", N),
            (&main, "c:/r/.foreman/worktrees", N),
            (&main, "c:/r/.foreman/tasks/a1.json", W),
            (&main, "c:/r/notes.log", N),
            (&main, "c:/r/targets/x", W),
            (&main, "c:/r/.gitignore", gi),
            (&main, "c:/r/target/.gitignore", gi),
            // Short names: an expanded one never reaches here; one that
            // could not be expanded is taken as everything.
            (&main, "c:/r/git~1/index", both),
            (&main, "c:/r/.git/packed~1", both),
            (&main, "c:/r/src/backup~", W),
            (&main, "c:/elsewhere/x", N),
            (&main, "c:/r", N),
            // Linked worktree: HEAD/index from its own git dir, refs from
            // the shared common dir.
            (&linked, "c:/r/.foreman/worktrees/a1/src/lib.rs", W),
            (&linked, "c:/r/.foreman/worktrees/a1/.git", W),
            (&linked, "c:/r/.git/worktrees/a1/HEAD", both),
            (&linked, "c:/r/.git/worktrees/a1/index", W),
            (&linked, "c:/r/.git/worktrees/a1/index.lock", N),
            (&linked, "c:/r/.git/worktrees/a1/rebase-merge/msgnum", R),
            (&linked, "c:/r/.git/worktrees/a1/logs/HEAD", N),
            (&linked, "c:/r/.git/worktrees/b2/HEAD", N),
            (&linked, "c:/r/.git/HEAD", N),
            (&linked, "c:/r/.git/index", N),
            (&linked, "c:/r/.git/refs/heads/card/a1", R),
            (&linked, "c:/r/.git/packed-refs", R),
            (&linked, "c:/r/.git/objects/pack/x.pack", N),
            (&linked, "c:/r/.git/config", ign),
        ];
        for (roots, path, want) in cases {
            let empty = Ignored::default();
            let set = if roots.worktree == main.worktree {
                &ignored
            } else {
                &empty
            };
            assert_eq!(classify(roots, set, &norm(path)), *want, "{path}");
        }
    }

    #[test]
    fn short_names_are_recognized() {
        for (name, short) in [
            ("GIT~1", true),
            ("PACKED~1", true),
            ("MERGE_~1", true),
            ("MERG~12", true),
            ("PROGRA~1.TXT", true),
            ("backup~", false),
            ("~1", false),
            ("a~b", false),
            ("verylongname~1", false),
            ("index", false),
        ] {
            assert_eq!(short_name(name), short, "{name}");
        }
    }

    #[test]
    fn debounce_trails_by_quiet_time_and_caps_at_max_wait() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut d = Debounce::default();
        // Meaningless events never open a window.
        d.event(ms(0), Hit::NONE);
        assert_eq!(d.deadline(), None);
        // A burst reports once, QUIET after its last event.
        for i in 0..10 {
            d.event(ms(i * 20), Hit::WORKTREE);
        }
        assert_eq!(d.take(ms(300)), None);
        assert_eq!(d.take(ms(480)), Some(Hit::WORKTREE));
        assert_eq!(d.take(ms(1000)), None);
        // Steady churn, never QUIET for long enough: reported every MAX_WAIT.
        let mut reports = Vec::new();
        let mut t = 0;
        while t < 10_000 {
            d.event(
                ms(t),
                if t % 1000 == 0 {
                    Hit::REFS
                } else {
                    Hit::WORKTREE
                },
            );
            if let Some(hit) = d.take(ms(t)) {
                reports.push((t, hit));
            }
            t += 100;
        }
        assert_eq!(reports.len(), 4, "{reports:?}");
        assert!(reports.iter().all(|(_, h)| h.worktree && h.refs));
        assert_eq!(reports[0].0, 2000);
        // Overflow: everything, but not before OVERFLOW_WAIT.
        let mut d = Debounce::default();
        d.overflow(ms(0));
        assert_eq!(d.take(ms(300)), None);
        assert_eq!(d.take(ms(500)), Some(Hit::ALL));
    }

    #[test]
    fn ignored_covers_files_directories_and_their_contents() {
        let ig = Ignored::parse(b"target/\0a/b/\0x.log\0");
        for (rel, want) in [
            ("target", true),
            ("target/debug/x", true),
            ("a/b", true),
            ("a/b/c/d", true),
            ("a", false),
            ("a/bc", false),
            ("x.log", true),
            ("x.log.bak", false),
            ("src/x.log", false),
        ] {
            assert_eq!(ig.covers(rel), want, "{rel}");
        }
        assert!(!Ignored::default().covers("anything"));
    }

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Watch Test"],
            &["config", "user.email", "watch@example.test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(dir, args);
        }
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "a\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "root"]);
        std::fs::create_dir(dir.join("target")).unwrap();
        repo
    }
    fn gens(w: &RepoWatch) -> (u64, u64) {
        (w.worktree_gen(), w.refs_gen())
    }
    /// Wait out any debounce window still open (setup noise).
    fn quiet(w: &RepoWatch) -> (u64, u64) {
        let mut last = gens(w);
        loop {
            std::thread::sleep(QUIET * 3);
            let now = gens(w);
            if now == last {
                return now;
            }
            last = now;
        }
    }
    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn bursts_in_ignored_dirs_are_silent_and_source_bursts_report_once() {
        let repo = repo();
        let dir = repo.path();
        let ctx = egui::Context::default();
        let w = open(dir, &ctx).expect("local repos are watchable");
        // A second view shares the same watch.
        let again = open(&dir.join("src"), &ctx).unwrap();
        assert!(Arc::ptr_eq(&w, &again));
        let before = quiet(&w);
        for i in 0..1000 {
            std::fs::write(dir.join(format!("target/{i}.o")), "x").unwrap();
        }
        assert_eq!(quiet(&w), before, "target/ is ignored");
        for i in 0..50 {
            std::fs::write(dir.join(format!("src/{i}.rs")), "x").unwrap();
        }
        wait_until("a worktree bump", || w.worktree_gen() > before.0);
        let after = quiet(&w);
        assert_eq!(after, (before.0 + 1, before.1), "one read per burst");
        // A commit moves a ref (and HEAD's target) and rewrites the index.
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "more"]);
        wait_until("a refs bump", || w.refs_gen() > after.1);
        assert!(w.worktree_gen() > after.0);
    }

    #[test]
    fn our_own_reads_never_wake_the_watch() {
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join("src/a.rs"), "changed\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        let w = open(dir, &egui::Context::default()).unwrap();
        let before = quiet(&w);
        let cancel = Arc::new(AtomicBool::new(false));
        let read = |args: &[&str]| {
            git::output(dir, args, &cancel, 32 << 20, Duration::from_secs(30)).unwrap();
        };
        for _ in 0..3 {
            read(&[
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--untracked-files=all",
                "--find-renames",
            ]);
            read(&["for-each-ref", "--format=%(objectname) %(refname)"]);
            read(&["rev-parse", "-q", "--verify", "HEAD"]);
            read(&[
                "log",
                "--date-order",
                "-z",
                "--format=%H%x00%P",
                "HEAD",
                "--",
            ]);
            read(&["rev-list", "--count", "HEAD"]);
            read(&["diff", "-U200000", "--", "src/a.rs"]);
            read(&["diff-tree", "-r", "--root", "HEAD"]);
            read(&[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "-z",
            ]);
        }
        assert_eq!(quiet(&w), before);
    }

    #[test]
    fn deleting_the_watch_root_stops_the_thread() {
        let repo = repo();
        let dir = repo.path().to_path_buf();
        let w = open(&dir, &egui::Context::default()).unwrap();
        quiet(&w);
        // The watch never blocks the delete.
        std::fs::remove_dir_all(&dir).unwrap();
        wait_until("the thread to exit", || !w.alive());
        assert!(!dir.exists());
        // A dead watch is replaced, not shared.
        drop(repo);
    }

    #[test]
    fn a_watched_main_checkout_or_card_worktree_never_blocks_teardown() {
        use crate::kanban::{BringUp, Card, TeardownOutcome, bring_up_worktree, teardown_worktree};
        let repo = repo();
        let dir = repo.path();
        let ctx = egui::Context::default();
        let main = open(dir, &ctx).unwrap();
        for id in ["a1b2c3", "d4e5f6"] {
            let card = Card::new(id.into(), "t".into(), None, crate::kanban::now_stamp());
            let BringUp::Worktree(wt) = bring_up_worktree(dir, &card).unwrap() else {
                panic!("expected a worktree");
            };
            let tree = PathBuf::from(&wt.path);
            // The second card is also open as its own Project.
            let own = (id == "d4e5f6").then(|| open(&tree, &ctx).unwrap());
            if let Some(own) = &own {
                assert!(!Arc::ptr_eq(own, &main));
                let before = quiet(own);
                std::fs::write(tree.join("src/a.rs"), "card\n").unwrap();
                git(&tree, &["commit", "-qam", "card work"]);
                wait_until("the card's refs bump", || own.refs_gen() > before.1);
            }
            git(dir, &["merge", "-q", "--ff-only", &wt.branch]);
            assert_eq!(teardown_worktree(dir, &wt, false), TeardownOutcome::Removed);
            assert!(!tree.exists());
            if let Some(own) = own {
                wait_until("the card watch to stop", || !own.alive());
            }
        }
        assert!(main.alive(), "the main checkout's watch is unaffected");
    }
}
