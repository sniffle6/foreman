//! Resolve npm-installed Codex to a shell-free argv, preserving prompt bytes.

use std::path::{Path, PathBuf};

/// Resolve before spawning: the PTY backend can silently wrap a bare npm
/// command in cmd.exe, so a successful process creation is not sufficient.
/// Invoke npm's JS entry point with Node so package updates, platform selection,
/// and environment setup remain owned by Codex. Never interpret a batch file.
pub fn npm_codex(argv: &[String], cwd: &Path, path: &std::ffi::OsStr) -> Option<Vec<String>> {
    let program = Path::new(argv.first()?);
    let name = program.file_name()?.to_str()?;
    if !name.eq_ignore_ascii_case("codex") && !name.eq_ignore_ascii_case("codex.cmd") {
        return None;
    }
    let dirs: Vec<PathBuf> = std::env::split_paths(path)
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            }
        })
        .collect();
    let shim = if program.components().count() > 1 {
        let program = if program.is_absolute() {
            program.to_path_buf()
        } else {
            cwd.join(program)
        };
        if name.eq_ignore_ascii_case("codex")
            && ["exe", "com"]
                .iter()
                .any(|ext| program.with_extension(ext).is_file())
        {
            return None;
        }
        program.with_extension("cmd")
    } else {
        // Do not skip an earlier installation in favor of a later npm package.
        std::iter::once(cwd.to_path_buf())
            .chain(dirs.iter().cloned())
            .flat_map(|dir| {
                if name.eq_ignore_ascii_case("codex.cmd") {
                    vec![dir.join("codex.cmd")]
                } else {
                    ["exe", "com", "bat", "cmd"]
                        .iter()
                        .map(|ext| dir.join(format!("codex.{ext}")))
                        .collect()
                }
            })
            .find(|p| p.is_file())?
    };
    if !shim.is_file() || !shim.extension()?.eq_ignore_ascii_case("cmd") {
        return None;
    }
    let dir = shim.parent()?;
    let script = dir.join("node_modules/@openai/codex/bin/codex.js");
    if !script.is_file() {
        return None;
    }
    // npm's wrapper prefers a sibling node.exe before searching PATH.
    let node = std::iter::once(dir.to_path_buf())
        .chain(dirs)
        .map(|dir| dir.join("node.exe"))
        .find(|p| p.is_file())?;
    let mut resolved = vec![node.to_str()?.to_owned(), script.to_str()?.to_owned()];
    resolved.extend_from_slice(&argv[1..]);
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(root: &Path) {
        std::fs::create_dir_all(root.join("node_modules/@openai/codex/bin")).unwrap();
        std::fs::write(root.join("codex.cmd"), "unused batch wrapper").unwrap();
        std::fs::write(root.join("node_modules/@openai/codex/bin/codex.js"), "").unwrap();
    }

    #[test]
    fn npm_launch_preserves_multiline_quotes_and_shell_metacharacters() {
        let tmp = tempfile::tempdir().unwrap();
        let npm = tmp.path().join("npm with spaces");
        install(&npm);
        std::fs::write(npm.join("node.exe"), "").unwrap();
        let prompt = "# Task\r\nUse \"quotes\", %PATH%, & | < > ^ and Unicode: café";
        let argv = vec!["codex".into(), prompt.into()];
        let path = std::env::join_paths([&npm]).unwrap();
        let resolved = npm_codex(&argv, tmp.path(), &path).unwrap();
        assert_eq!(Path::new(&resolved[0]), npm.join("node.exe"));
        assert_eq!(
            Path::new(&resolved[1]),
            npm.join("node_modules/@openai/codex/bin/codex.js")
        );
        assert_eq!(&resolved[2..], &argv[1..]);
    }

    #[test]
    fn explicit_shim_uses_path_node_and_never_substitutes_another_package() {
        let tmp = tempfile::tempdir().unwrap();
        let npm = tmp.path().join("npm");
        install(&npm);
        let node = tmp.path().join("node");
        std::fs::create_dir(&node).unwrap();
        std::fs::write(node.join("node.exe"), "").unwrap();
        let path = std::env::join_paths([&node, &npm]).unwrap();
        let argv = vec!["npm/codex.cmd".into(), "task\nbody".into()];
        assert_eq!(
            Path::new(&npm_codex(&argv, tmp.path(), &path).unwrap()[0]),
            node.join("node.exe")
        );
        for command in ["missing/codex.cmd", "claude", "codex.exe", "other.cmd"] {
            assert!(npm_codex(&[command.into()], tmp.path(), &path).is_none());
        }
        std::fs::remove_file(npm.join("node_modules/@openai/codex/bin/codex.js")).unwrap();
        assert!(npm_codex(&argv, tmp.path(), &path).is_none());
    }

    #[test]
    fn native_install_before_npm_is_not_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let npm = tmp.path().join("npm");
        install(&npm);
        std::fs::write(npm.join("node.exe"), "").unwrap();
        std::fs::write(tmp.path().join("codex.exe"), "native").unwrap();
        let path = std::env::join_paths([&npm]).unwrap();
        assert!(npm_codex(&["codex".into()], tmp.path(), &path).is_none());
        assert!(
            npm_codex(
                &[tmp.path().join("codex").to_str().unwrap().into()],
                tmp.path(),
                &path
            )
            .is_none()
        );
    }

    #[test]
    fn broken_first_shim_does_not_fall_through_to_later_install() {
        let tmp = tempfile::tempdir().unwrap();
        let later = tmp.path().join("later");
        install(&later);
        std::fs::write(later.join("node.exe"), "").unwrap();
        std::fs::write(tmp.path().join("codex.cmd"), "custom wrapper").unwrap();
        let path = std::env::join_paths([&later]).unwrap();
        assert!(npm_codex(&["codex".into()], tmp.path(), &path).is_none());
    }
}
