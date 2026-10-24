//! Resolve npm-installed Codex to a shell-free argv, preserving prompt bytes.

use std::path::{Path, PathBuf};

/// Card workers stay interactive. Model selection is local to this launch,
/// never a mutation of the provider's persisted default or a shell command.
pub fn card_argv(agent: &str, prompt: &str, model: Option<&str>) -> Result<Vec<String>, String> {
    let mut argv = vec![agent.to_owned(), prompt.to_owned()];
    if let Some(model) = model {
        if !matches!(agent, "claude" | "codex") {
            return Err("--model is supported for claude and codex only".into());
        }
        if model.is_empty() || model.starts_with('-') || model.chars().any(char::is_whitespace) {
            return Err(
                "--model needs a non-empty model ID without whitespace or a leading '-'".into(),
            );
        }
        argv.extend(["--model".to_owned(), model.to_owned()]);
    }
    Ok(argv)
}

/// The PATH a PTY session sees: `portable_pty` rebuilds it from the registry
/// (machine entries, then user entries) on every spawn. The GUI's own PATH
/// can lack the user entries entirely -- a launch from an installer or an
/// elevated updater inherits a machine-only PATH, and every in-app restart
/// inherits that. npm's codex.cmd and ~/.local/bin/claude.exe live in the
/// user entries, so anything spawned with `std::process::Command` must
/// resolve on this PATH rather than the process one. Falls back to the
/// process PATH when the builder has none.
pub fn session_path() -> std::ffi::OsString {
    portable_pty::CommandBuilder::new("")
        .get_env("PATH")
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default()
}

/// The first `git` on `path` alone. A bare `Command::new("git")` is not
/// enough on Windows: `std` searches the process PATH after the child PATH,
/// so a `path` with no git still runs the GUI's machine Git. A terminal
/// does not do that fallback.
pub(crate) fn git_program(path: &std::ffi::OsStr) -> Option<PathBuf> {
    // `.exe` before the batch shims, matching PATHEXT's order for the names
    // Git actually ships. A bare `git` covers a Unix layout.
    let names: &[&str] = if cfg!(windows) {
        &["git.exe", "git.cmd", "git.bat", "git"]
    } else {
        &["git"]
    };
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

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

/// Subcommands that take `--no-daemon` after their own name.
const CODEX_SESSION_SUBCOMMANDS: &[&str] = &["resume", "fork"];

/// Subcommands that never start a TUI; Codex rejects `--no-daemon` on them.
const CODEX_OTHER_SUBCOMMANDS: &[&str] = &[
    "agents",
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "plugin",
    "app-server",
    "remote-control",
    "app",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "queue",
    "archive",
    "delete",
    "migrate-rollouts",
    "unarchive",
    "cloud",
    "exec-server",
    "features",
    "help",
];

/// `argv` with `--no-daemon` added when it launches an interactive Codex TUI;
/// `None` when it is not one or already chooses its server.
///
/// Codex 0.157+ attaches every interactive TUI to one shared app-server
/// daemon, which runs tools and hooks with the environment of the TUI that
/// started it. Every Codex Session after the first would then post chat and
/// report agent state as another Session's FOREMAN_TERMINAL_ID. In-process,
/// the server inherits this Session's own environment. The flag needs Codex
/// 0.156+, the first release with the daemon.
pub fn codex_in_process(argv: &[String]) -> Option<Vec<String>> {
    let stem = Path::new(argv.first()?).file_stem()?.to_str()?;
    if !stem.eq_ignore_ascii_case("codex") {
        return None;
    }
    // `--remote` is an explicit server choice; Codex rejects it with --no-daemon.
    if argv[1..]
        .iter()
        .any(|a| a == "--no-daemon" || a == "--remote" || a.starts_with("--remote="))
    {
        return None;
    }
    let at = match argv.get(1).map(String::as_str) {
        Some(sub) if CODEX_SESSION_SUBCOMMANDS.contains(&sub) => 2,
        Some(sub) if CODEX_OTHER_SUBCOMMANDS.contains(&sub) => return None,
        _ => 1,
    };
    let mut out = argv.to_vec();
    out.insert(at, "--no-daemon".to_owned());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_models_are_launch_local_and_preserve_the_interactive_prompt() {
        let prompt = "Implement the card.\nKeep its closeout commands.";
        for (agent, model) in [
            ("claude", "claude-haiku-5-5"),
            ("claude", "claude-sonnet-5-5"),
            ("claude", "claude-opus-5-5"),
            ("codex", "gpt-6.1-sol"),
            ("codex", "gpt-6-astra"),
        ] {
            assert_eq!(
                card_argv(agent, prompt, Some(model)).unwrap(),
                [agent, prompt, "--model", model]
            );
            assert_eq!(card_argv(agent, prompt, None).unwrap(), [agent, prompt]);
        }
        assert!(card_argv("grok", prompt, Some("anything")).is_err());
        for model in ["", " ", "--help", "model id"] {
            assert!(card_argv("codex", prompt, Some(model)).is_err());
        }
    }

    fn install(root: &Path) {
        std::fs::create_dir_all(root.join("node_modules/@openai/codex/bin")).unwrap();
        std::fs::write(root.join("codex.cmd"), "unused batch wrapper").unwrap();
        std::fs::write(root.join("node_modules/@openai/codex/bin/codex.js"), "").unwrap();
    }

    fn words(argv: &[&str]) -> Vec<String> {
        argv.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn interactive_codex_runs_its_server_in_process() {
        for (argv, expected) in [
            (vec!["codex"], vec!["codex", "--no-daemon"]),
            (vec!["codex", "task"], vec!["codex", "--no-daemon", "task"]),
            (
                vec!["codex", "-m", "x"],
                vec!["codex", "--no-daemon", "-m", "x"],
            ),
            (
                vec![r"C:\npm\codex.cmd", "t"],
                vec![r"C:\npm\codex.cmd", "--no-daemon", "t"],
            ),
            (vec!["CODEX.EXE"], vec!["CODEX.EXE", "--no-daemon"]),
            (
                vec!["codex", "resume", "--last"],
                vec!["codex", "resume", "--no-daemon", "--last"],
            ),
            (vec!["codex", "fork"], vec!["codex", "fork", "--no-daemon"]),
        ] {
            assert_eq!(
                codex_in_process(&words(&argv)).as_deref(),
                Some(words(&expected).as_slice()),
                "{argv:?}"
            );
        }
    }

    #[test]
    fn codex_server_flag_is_left_out_where_codex_rejects_or_ignores_it() {
        for argv in [
            vec!["codex", "exec", "task"],
            vec!["codex", "e", "task"],
            vec!["codex", "review"],
            vec!["codex", "agents"],
            vec!["codex", "login"],
            vec!["codex", "--no-daemon"],
            vec!["codex", "--remote", "ws://h"],
            vec!["codex", "--remote=ws://h"],
            vec!["claude", "task"],
            vec!["codexx"],
        ] {
            assert_eq!(codex_in_process(&words(&argv)), None, "{argv:?}");
        }
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
