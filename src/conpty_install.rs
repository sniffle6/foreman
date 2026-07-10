//! Install the sideloaded ConPTY host beside the exe at startup.
//!
//! The in-box Windows conhost strips kitty graphics APC sequences inside
//! ConPTY (see docs/terminal-images.md); the vendored OpenConsole build
//! (assets/conpty/, MIT) passes them through. portable-pty prefers a
//! conpty.dll found beside the exe, so dropping the pair there upgrades
//! every PTY foreman spawns. Startup holds both installed files open so another
//! process cannot replace either member while this process may still spawn a
//! PTY. A failed update disables the sideloaded DLL and degrades to the in-box
//! ConPTY; startup aborts only when an unverified `conpty.dll` would stay
//! loadable from Windows' DLL search path (disable failed, or the pair is
//! locked by another process mid-update).

use std::ffi::c_void;
use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

const CONPTY_DLL: &[u8] = include_bytes!("../assets/conpty/conpty.dll");
const OPENCONSOLE_EXE: &[u8] = include_bytes!("../assets/conpty/OpenConsole.exe");

type Asset<'a> = (&'a str, &'a [u8]);

static CONPTY_LEASE: OnceLock<ConptyLease> = OnceLock::new();

struct InstallLock(*mut c_void);

impl InstallLock {
    fn acquire() -> std::io::Result<Self> {
        // Foreman installs are per-user. Local\ serializes every updater in
        // this Windows login session, which is the supported launch model.
        let name: Vec<u16> = "Local\\Foreman-ConPTY-Asset-Install"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        match unsafe { WaitForSingleObject(handle, 10_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
            WAIT_TIMEOUT => {
                unsafe { CloseHandle(handle) };
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for another ConPTY installer",
                ))
            }
            _ => {
                let error = std::io::Error::last_os_error();
                unsafe { CloseHandle(handle) };
                Err(error)
            }
        }
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// Excludes both a DLL already mapped by another Foreman and a concurrent
/// LoadLibrary while the live pair is being replaced. FILE_SHARE_DELETE keeps
/// the guarded old DLL renameable during the transaction.
struct DllUpdateGuard {
    _file: Option<std::fs::File>,
}

impl DllUpdateGuard {
    fn acquire(dir: &std::path::Path) -> std::io::Result<Self> {
        let path = dir.join("conpty.dll");
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_DELETE)
            .open(&path)
        {
            Ok(file) => Ok(Self { _file: Some(file) }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self { _file: None }),
            Err(e) => Err(std::io::Error::new(
                e.kind(),
                format!(
                    "cannot safely update {} while another process may be using it: {e}",
                    path.display()
                ),
            )),
        }
    }
}

/// Read leases keep the exact DLL/host pair stable for this process's entire
/// lifetime, including before portable-pty initializes its lazy ConPTY API.
struct ConptyLease {
    _files: Vec<std::fs::File>,
}

impl ConptyLease {
    fn acquire_exact(dir: &std::path::Path, files: &[Asset<'_>]) -> std::io::Result<Self> {
        let mut leases = Vec::with_capacity(files.len());
        for (name, expected) in files {
            let path = dir.join(name);
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&path)
                .map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("cannot lock installed ConPTY asset {}: {e}", path.display()),
                    )
                })?;
            let mut actual = Vec::with_capacity(expected.len());
            file.read_to_end(&mut actual)?;
            if actual != *expected {
                return Err(std::io::Error::other(format!(
                    "ConPTY asset changed while locking it: {}",
                    path.display()
                )));
            }
            leases.push(file);
        }
        Ok(Self { _files: leases })
    }
}

fn assets() -> [Asset<'static>; 2] {
    [
        ("conpty.dll", CONPTY_DLL),
        ("OpenConsole.exe", OPENCONSOLE_EXE),
    ]
}

/// How `install_into` left the exe directory: the exact embedded pair, or no
/// sideloaded DLL at all (safe in-box ConPTY fallback after a failed update).
#[derive(Debug, PartialEq, Eq)]
enum InstallOutcome {
    Installed,
    SideloadDisabled,
}

/// Write the matched pair beside the running exe if either file differs.
/// Must run before the first PTY spawn. Errors are reserved for genuinely
/// unsafe states (an unverified `conpty.dll` left loadable); a failed update
/// that reached the no-sideload fallback returns `Ok` and Foreman degrades to
/// the in-box ConPTY (text-only images).
pub fn ensure_conpty() -> std::io::Result<()> {
    if CONPTY_LEASE.get().is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| std::io::Error::other("current exe has no parent directory"))?;
    let _lock = InstallLock::acquire()?;
    if CONPTY_LEASE.get().is_some() {
        return Ok(());
    }
    if install_into(dir)? == InstallOutcome::SideloadDisabled {
        return Ok(());
    }
    let lease = ConptyLease::acquire_exact(dir, &assets())?;
    CONPTY_LEASE
        .set(lease)
        .map_err(|_| std::io::Error::other("ConPTY asset lease was initialized twice"))
}

fn install_into(dir: &std::path::Path) -> std::io::Result<InstallOutcome> {
    let files = assets();
    if pair_matches(dir, &files) {
        cleanup_artifacts(dir, &files);
        return Ok(InstallOutcome::Installed);
    }

    // Guard the currently-live DLL before recovery touches either sidecar. If
    // an older Foreman has it mapped, abort rather than give that DLL a host
    // from another package version on its next PTY spawn.
    let recovery_guard = DllUpdateGuard::acquire(dir)?;
    if let Err(e) = restore_backups(dir, &files) {
        drop(recovery_guard);
        let _guard = DllUpdateGuard::acquire(dir)?;
        disable_sideload(dir)?;
        eprintln!(
            "conpty install: recovery in {} failed; sideload disabled, using in-box ConPTY: {e}",
            dir.display()
        );
        return Ok(InstallOutcome::SideloadDisabled);
    }
    drop(recovery_guard);

    // Recovery may have placed a backed-up DLL at the live path. Reacquire the
    // guard so a racing LoadLibrary either wins (and this startup aborts) or is
    // held off until the new host is in place and the new DLL commits the pair.
    let _update_guard = DllUpdateGuard::acquire(dir)?;
    if let Err(e) = replace_pair(dir, &files) {
        disable_sideload(dir)?;
        eprintln!(
            "conpty install: update in {} failed; sideload disabled, using in-box ConPTY: {e}",
            dir.display()
        );
        return Ok(InstallOutcome::SideloadDisabled);
    }
    Ok(InstallOutcome::Installed)
}

fn pair_matches(dir: &std::path::Path, files: &[Asset<'_>]) -> bool {
    files.iter().all(|(name, expected)| {
        let path = dir.join(name);
        std::fs::metadata(&path).is_ok_and(|m| m.len() == expected.len() as u64)
            && std::fs::read(path).is_ok_and(|current| current == *expected)
    })
}

fn artifact_path(dir: &std::path::Path, name: &str, suffix: &str) -> std::path::PathBuf {
    dir.join(format!("{name}.{suffix}"))
}

fn remove_file_if_present(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn cleanup_artifacts(dir: &std::path::Path, files: &[Asset<'_>]) {
    for (name, _) in files {
        let _ = remove_file_if_present(&artifact_path(dir, name, "new"));
        let _ = remove_file_if_present(&artifact_path(dir, name, "old"));
    }
    let _ = remove_file_if_present(&artifact_path(dir, "conpty.dll", "disabled"));
}

fn disable_sideload(dir: &std::path::Path) -> std::io::Result<()> {
    let dll = dir.join("conpty.dll");
    if !dll.try_exists()? {
        return Ok(());
    }
    let disabled = artifact_path(dir, "conpty.dll", "disabled");
    let _ = remove_file_if_present(&disabled);
    if std::fs::rename(&dll, &disabled).is_err() {
        let _ = remove_file_if_present(&dll);
    }
    if dll.try_exists()? {
        return Err(std::io::Error::other(format!(
            "could not disable unsafe sideloaded DLL at {}",
            dll.display()
        )));
    }
    Ok(())
}

fn restore_backups(dir: &std::path::Path, files: &[Asset<'_>]) -> std::io::Result<()> {
    let mut backups = Vec::new();
    for (name, _) in files {
        let backup = artifact_path(dir, name, "old");
        match std::fs::metadata(&backup) {
            Ok(metadata) if metadata.is_file() => backups.push((dir.join(name), backup)),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }

    if !backups.is_empty() && backups.len() != files.len() {
        disable_sideload(dir)?;
        for (_, backup) in backups {
            remove_file_if_present(&backup)?;
        }
        for (name, _) in files {
            remove_file_if_present(&artifact_path(dir, name, "new"))?;
        }
        return Ok(());
    }

    // The DLL is the commit point. Remove it before changing the host, then
    // restore the host first and DLL last so every observable state is either
    // a matched pair or a safe kernel32 fallback.
    if !backups.is_empty() {
        remove_file_if_present(&dir.join("conpty.dll"))?;
    }
    for (dest, _) in &backups {
        remove_file_if_present(dest)?;
    }
    for (dest, backup) in backups.iter().rev() {
        std::fs::rename(backup, dest)?;
    }
    for (name, _) in files {
        remove_file_if_present(&artifact_path(dir, name, "new"))?;
    }
    Ok(())
}

fn replace_pair(dir: &std::path::Path, files: &[Asset<'_>]) -> std::io::Result<()> {
    for (name, bytes) in files {
        let staged = artifact_path(dir, name, "new");
        remove_file_if_present(&staged)?;
        let write = (|| {
            let mut file = std::fs::File::create(&staged)?;
            std::io::Write::write_all(&mut file, bytes)?;
            file.sync_all()
        })();
        if let Err(e) = write {
            cleanup_artifacts(dir, files);
            return Err(e);
        }
    }

    let mut backups = Vec::new();
    let mut promoted = Vec::new();
    let result = (|| {
        for (name, _) in files {
            let dest = dir.join(name);
            if dest.try_exists()? {
                let backup = artifact_path(dir, name, "old");
                remove_file_if_present(&backup)?;
                std::fs::rename(&dest, &backup)?;
                backups.push((dest, backup));
            }
        }

        // OpenConsole is installed first. conpty.dll lands last and commits
        // the pair for portable-pty's LoadLibrary("conpty.dll") lookup.
        for (name, _) in files.iter().rev() {
            let dest = dir.join(name);
            std::fs::rename(artifact_path(dir, name, "new"), &dest)?;
            promoted.push(dest);
        }

        if !pair_matches(dir, files) {
            return Err(std::io::Error::other(
                "installed files do not match embedded ConPTY pair",
            ));
        }
        Ok(())
    })();

    if let Err(e) = result {
        let mut rollback_error = None;
        for dest in promoted.iter().rev() {
            if let Err(err) = remove_file_if_present(dest) {
                rollback_error.get_or_insert(err);
            }
        }
        for (dest, backup) in backups.iter().rev() {
            if let Err(err) = remove_file_if_present(dest) {
                rollback_error.get_or_insert(err);
            }
            if let Err(err) = std::fs::rename(backup, dest) {
                rollback_error.get_or_insert(err);
            }
        }
        for (name, _) in files {
            let _ = remove_file_if_present(&artifact_path(dir, name, "new"));
        }
        if let Some(rollback) = rollback_error {
            return Err(std::io::Error::other(format!(
                "{e}; rollback failed: {rollback}"
            )));
        }
        return Err(e);
    }

    cleanup_artifacts(dir, files);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_both_files_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        install_into(dir.path()).unwrap();
        let dll = dir.path().join("conpty.dll");
        let exe = dir.path().join("OpenConsole.exe");
        assert_eq!(
            std::fs::metadata(&dll).unwrap().len(),
            CONPTY_DLL.len() as u64
        );
        assert_eq!(
            std::fs::metadata(&exe).unwrap().len(),
            OPENCONSOLE_EXE.len() as u64
        );
        let before = std::fs::metadata(&dll).unwrap().modified().unwrap();
        install_into(dir.path()).unwrap(); // exact pair -> untouched
        assert_eq!(std::fs::metadata(&dll).unwrap().modified().unwrap(), before);
    }

    #[test]
    fn install_replaces_same_size_stale_files_as_a_pair() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("conpty.dll");
        let exe = dir.path().join("OpenConsole.exe");
        std::fs::write(&dll, vec![0; CONPTY_DLL.len()]).unwrap();
        std::fs::write(&exe, vec![0; OPENCONSOLE_EXE.len()]).unwrap();

        install_into(dir.path()).unwrap();

        assert_eq!(std::fs::read(dll).unwrap(), CONPTY_DLL);
        assert_eq!(std::fs::read(exe).unwrap(), OPENCONSOLE_EXE);
    }

    #[test]
    fn pair_replacement_supports_a_same_size_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("conpty.dll"), CONPTY_DLL).unwrap();
        std::fs::write(dir.path().join("OpenConsole.exe"), OPENCONSOLE_EXE).unwrap();
        let mut older_dll = CONPTY_DLL.to_vec();
        let mut older_exe = OPENCONSOLE_EXE.to_vec();
        older_dll[0] ^= 0xff;
        older_exe[0] ^= 0xff;
        let older = [
            ("conpty.dll", older_dll.as_slice()),
            ("OpenConsole.exe", older_exe.as_slice()),
        ];

        replace_pair(dir.path(), &older).unwrap();

        assert_eq!(
            std::fs::read(dir.path().join("conpty.dll")).unwrap(),
            older_dll
        );
        assert_eq!(
            std::fs::read(dir.path().join("OpenConsole.exe")).unwrap(),
            older_exe
        );
    }

    #[test]
    fn pair_replacement_rolls_back_if_the_second_backup_fails() {
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("a.bin", b"new-a".as_slice()),
            ("b.bin", b"new-b".as_slice()),
        ];
        std::fs::write(dir.path().join("a.bin"), b"old-a").unwrap();
        std::fs::write(dir.path().join("b.bin"), b"old-b").unwrap();
        std::fs::create_dir(dir.path().join("b.bin.old")).unwrap();

        assert!(replace_pair(dir.path(), &files).is_err());

        assert_eq!(std::fs::read(dir.path().join("a.bin")).unwrap(), b"old-a");
        assert_eq!(std::fs::read(dir.path().join("b.bin")).unwrap(), b"old-b");
        assert!(!dir.path().join("a.bin.new").exists());
        assert!(!dir.path().join("b.bin.new").exists());
    }

    #[test]
    fn install_recovers_an_interrupted_pair_promotion() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("conpty.dll");
        let exe = dir.path().join("OpenConsole.exe");
        let stale_dll = vec![0; CONPTY_DLL.len()];
        let stale_exe = vec![0; OPENCONSOLE_EXE.len()];
        std::fs::write(&dll, &stale_dll).unwrap();
        std::fs::write(&exe, &stale_exe).unwrap();
        std::fs::rename(&dll, dir.path().join("conpty.dll.old")).unwrap();
        std::fs::rename(&exe, dir.path().join("OpenConsole.exe.old")).unwrap();
        std::fs::write(&dll, CONPTY_DLL).unwrap();

        install_into(dir.path()).unwrap();

        assert_eq!(std::fs::read(dll).unwrap(), CONPTY_DLL);
        assert_eq!(std::fs::read(exe).unwrap(), OPENCONSOLE_EXE);
        assert!(!dir.path().join("conpty.dll.old").exists());
        assert!(!dir.path().join("OpenConsole.exe.old").exists());
    }

    #[test]
    fn install_repairs_a_missing_member_and_stale_staging() {
        let dir = tempfile::tempdir().unwrap();
        install_into(dir.path()).unwrap();
        std::fs::remove_file(dir.path().join("OpenConsole.exe")).unwrap();
        std::fs::write(dir.path().join("conpty.dll.new"), b"stale").unwrap();

        install_into(dir.path()).unwrap();

        assert!(pair_matches(dir.path(), &assets()));
        assert!(!dir.path().join("conpty.dll.new").exists());
        assert!(!dir.path().join("conpty.dll.disabled").exists());
    }

    #[test]
    fn install_does_not_restore_a_lone_dll_backup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("conpty.dll.old"), b"orphaned-old-dll").unwrap();
        std::fs::write(dir.path().join("conpty.dll"), b"partial-new-dll").unwrap();

        install_into(dir.path()).unwrap();

        assert!(pair_matches(dir.path(), &assets()));
        assert!(!dir.path().join("conpty.dll.old").exists());
        assert!(!dir.path().join("conpty.dll.disabled").exists());
    }

    #[test]
    fn failed_install_with_lone_dll_disables_sideloading() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("conpty.dll"), b"unmatched-dll").unwrap();
        let blocker = dir.path().join("conpty.dll.new");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("keep"), b"force remove_file failure").unwrap();

        assert_eq!(
            install_into(dir.path()).unwrap(),
            InstallOutcome::SideloadDisabled
        );

        assert!(!dir.path().join("conpty.dll").exists());
        assert!(!pair_matches(dir.path(), &assets()));
    }

    #[test]
    fn failed_install_never_launches_an_unknown_complete_pair() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("conpty.dll"), b"unknown-dll").unwrap();
        std::fs::write(dir.path().join("OpenConsole.exe"), b"unknown-host").unwrap();
        let blocker = dir.path().join("conpty.dll.new");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("keep"), b"force staging failure").unwrap();

        assert_eq!(
            install_into(dir.path()).unwrap(),
            InstallOutcome::SideloadDisabled
        );

        assert!(
            !dir.path().join("conpty.dll").exists(),
            "an unverified pair remained eligible for portable-pty"
        );
        assert_eq!(
            std::fs::read(dir.path().join("OpenConsole.exe")).unwrap(),
            b"unknown-host"
        );
    }

    #[test]
    fn disabling_an_unremovable_dll_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("conpty.dll");
        let disabled = dir.path().join("conpty.dll.disabled");
        std::fs::create_dir(&dll).unwrap();
        std::fs::write(dll.join("keep"), b"live blocker").unwrap();
        std::fs::create_dir(&disabled).unwrap();
        std::fs::write(disabled.join("keep"), b"rename blocker").unwrap();

        assert!(disable_sideload(dir.path()).is_err());
        assert!(dll.exists(), "the unsafe DLL path is still live");
    }

    #[test]
    fn install_lock_serializes_concurrent_updaters() {
        let first = InstallLock::acquire().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let second = InstallLock::acquire().unwrap();
            tx.send(()).unwrap();
            drop(second);
        });

        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "second updater entered while the mutex was held"
        );
        drop(first);
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("second updater never acquired the released mutex");
        worker.join().unwrap();
    }

    #[test]
    fn loaded_dll_blocks_a_pair_update() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::FreeLibrary;
        use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("conpty.dll");
        std::fs::write(&dll, CONPTY_DLL).unwrap();
        let wide: Vec<u16> = dll
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        assert!(!module.is_null(), "could not load the ConPTY test DLL");

        assert!(
            DllUpdateGuard::acquire(dir.path()).is_err(),
            "a mapped DLL did not block a cross-version update"
        );

        unsafe { FreeLibrary(module) };
        assert!(DllUpdateGuard::acquire(dir.path()).is_ok());
    }

    #[test]
    fn process_lease_blocks_a_later_pair_update() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("conpty.dll"), CONPTY_DLL).unwrap();
        std::fs::write(dir.path().join("OpenConsole.exe"), OPENCONSOLE_EXE).unwrap();
        let lease = ConptyLease::acquire_exact(dir.path(), &assets()).unwrap();

        assert!(
            DllUpdateGuard::acquire(dir.path()).is_err(),
            "the process lease allowed its DLL to be replaced"
        );
        assert!(
            std::fs::rename(
                dir.path().join("OpenConsole.exe"),
                dir.path().join("OpenConsole.exe.old")
            )
            .is_err(),
            "the process lease allowed its host to be replaced"
        );

        drop(lease);
        assert!(DllUpdateGuard::acquire(dir.path()).is_ok());
    }
}
