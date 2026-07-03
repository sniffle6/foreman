//! Install the sideloaded ConPTY host beside the exe at startup.
//!
//! The in-box Windows conhost strips kitty graphics APC sequences inside
//! ConPTY (see docs/terminal-images.md); the vendored OpenConsole build
//! (assets/conpty/, MIT) passes them through. portable-pty prefers a
//! conpty.dll found beside the exe, so dropping the pair there upgrades
//! every PTY foreman spawns. Best-effort like skills_install: failures are
//! logged, never block launch — foreman degrades to text-only images.

const CONPTY_DLL: &[u8] = include_bytes!("../assets/conpty/conpty.dll");
const OPENCONSOLE_EXE: &[u8] = include_bytes!("../assets/conpty/OpenConsole.exe");

/// Write the pair beside the running exe if missing or a different size
/// (cheap version check). Must run before the first PTY spawn.
pub fn ensure_conpty() {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("conpty install: current_exe unavailable");
        return;
    };
    let Some(dir) = exe.parent() else { return };
    install_into(dir);
}

fn install_into(dir: &std::path::Path) {
    for (name, bytes) in [
        ("conpty.dll", CONPTY_DLL),
        ("OpenConsole.exe", OPENCONSOLE_EXE),
    ] {
        let dest = dir.join(name);
        let current_len = std::fs::metadata(&dest).map(|m| m.len()).ok();
        if current_len == Some(bytes.len() as u64) {
            continue;
        }
        // A running session may hold the old file open; best-effort only.
        if let Err(e) = std::fs::write(&dest, bytes) {
            eprintln!("conpty install: {} -> {}: {e}", name, dest.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_both_files_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        install_into(dir.path());
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
        install_into(dir.path()); // same size -> untouched
        assert_eq!(std::fs::metadata(&dll).unwrap().modified().unwrap(), before);
    }
}
