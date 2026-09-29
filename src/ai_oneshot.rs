//! One-shot AI CLI launcher shared by every "ask a model for text" feature
//! (Session titles, the theme expert). It owns the provider flag lists, the
//! isolation (no tools, scrubbed env, no window), the wall-clock deadline and
//! the output cap. It returns raw text; callers own the prompt and all output
//! validation, and run it on their own background thread.

use crate::config::NamingProvider;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const SCRUBBED_ENV: &[&str] = &[
    "FOREMAN",
    "FOREMAN_EXE",
    "FOREMAN_PROJECT_ID",
    "FOREMAN_TERMINAL_ID",
    "FOREMAN_TITLE_PIPE",
    "FOREMAN_OPENAI_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "XAI_API_KEY",
    "GROK_CODE_XAI_API_KEY",
    "GROK_DEPLOYMENT_KEY",
];

pub struct Request<'a> {
    pub provider: NamingProvider,
    /// Exact provider model id; empty uses the CLI's default.
    pub model: &'a str,
    /// Replaces the provider's agent system prompt where the CLI allows it.
    /// Codex has no such flag, so it is prepended to the prompt instead.
    pub system: Option<&'a str>,
    pub prompt: &'a str,
    pub cwd: &'a Path,
    /// One deadline for spawn, stdin delivery, execution and output drain.
    pub timeout: Duration,
    /// Stdout larger than this is an error, never silently truncated.
    pub max_output: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchError {
    Unavailable,
    Failed,
    Timeout,
    TooLarge,
}

pub fn program(provider: NamingProvider) -> &'static str {
    match provider {
        NamingProvider::Codex => "codex",
        NamingProvider::Claude => "claude",
        NamingProvider::Grok => "grok",
    }
}

/// Run the provider CLI once and return its stdout.
pub fn run(request: &Request) -> Result<String, LaunchError> {
    run_process(
        &command_spec(request),
        request.cwd,
        request.timeout,
        request.max_output,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandSpec {
    program: String,
    args: Vec<String>,
    stdin: Option<String>,
}

fn command_spec(request: &Request) -> CommandSpec {
    let model = request.model.trim();
    let words = |list: &[&str]| list.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let (mut args, stdin) = match request.provider {
        NamingProvider::Codex => {
            let args = words(&[
                "exec",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config",
                "--ignore-rules",
                "--disable",
                "shell_tool",
                "--disable",
                "unified_exec",
                "--color",
                "never",
            ]);
            let stdin = match request.system {
                Some(system) => format!("{system}\n\n{}", request.prompt),
                None => request.prompt.to_owned(),
            };
            (args, Some(stdin))
        }
        NamingProvider::Claude => {
            let mut args = words(&[
                "-p",
                "--safe-mode",
                "--tools",
                "",
                "--disable-slash-commands",
                "--no-chrome",
                "--no-session-persistence",
                "--output-format",
                "text",
            ]);
            if let Some(system) = request.system {
                args.extend(["--system-prompt".into(), system.into()]);
            }
            (args, Some(request.prompt.to_owned()))
        }
        NamingProvider::Grok => {
            let mut args = words(&["--no-auto-update", "--single"]);
            args.push(request.prompt.into());
            args.extend(words(&[
                "--output-format",
                "plain",
                "--tools",
                "",
                "--disable-web-search",
                "--no-subagents",
                "--max-turns",
                "1",
                "--permission-mode",
                "dontAsk",
                "--verbatim",
            ]));
            if let Some(system) = request.system {
                args.extend(["--system-prompt-override".into(), system.into()]);
            }
            (args, None)
        }
    };
    if !model.is_empty() {
        args.extend(["--model".into(), model.into()]);
    }
    if request.provider == NamingProvider::Codex {
        args.push("-".into());
    }
    CommandSpec {
        program: program(request.provider).into(),
        args,
        stdin,
    }
}

fn run_process(
    spec: &CommandSpec,
    cwd: &Path,
    timeout: Duration,
    max_output: usize,
) -> Result<String, LaunchError> {
    use std::process::{Command, Stdio};
    // One wall-clock deadline covers spawn, stdin delivery, process execution,
    // and output drain. No individual pipe operation may hold the caller.
    let started = Instant::now();
    let argv = std::iter::once(spec.program.clone())
        .chain(spec.args.iter().cloned())
        .collect::<Vec<_>>();
    // npm's codex.cmd shim would force the prompt through cmd.exe; run its
    // node entry point directly instead so argv needs no shell quoting.
    let npm =
        crate::agent_command::npm_codex(&argv, cwd, &std::env::var_os("PATH").unwrap_or_default());
    let build = |through_cmd: bool| -> Result<Command, LaunchError> {
        let mut command;
        #[cfg(windows)]
        {
            if through_cmd {
                // Other npm-installed CLIs are .cmd shims too. A bare
                // CreateProcess lookup does not execute those, so mirror
                // Session::spawn_argv's one-shot cmd fallback. Keep every
                // shell-interpreted word conservative; prompts for Codex and
                // Claude travel over stdin, never through this argv.
                if argv.iter().any(|word| !cmd_fallback_word_is_safe(word)) {
                    return Err(LaunchError::Failed);
                }
                command = Command::new("cmd.exe");
                command.args(["/d", "/c"]).args(&argv);
            } else {
                let argv = npm.as_ref().unwrap_or(&argv);
                command = Command::new(&argv[0]);
                command.args(&argv[1..]);
            }
        }
        #[cfg(not(windows))]
        {
            debug_assert!(!through_cmd);
            let argv = npm.as_ref().unwrap_or(&argv);
            command = Command::new(&argv[0]);
            command.args(&argv[1..]);
        }
        command
            .current_dir(cwd)
            .stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in SCRUBBED_ENV {
            command.env_remove(name);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        Ok(command)
    };

    let mut child = match build(false)?.spawn() {
        Ok(child) => child,
        #[cfg(windows)]
        Err(_) => build(true)?.spawn().map_err(|_| LaunchError::Unavailable)?,
        #[cfg(not(windows))]
        Err(_) => return Err(LaunchError::Unavailable),
    };
    #[cfg(windows)]
    let child_job = crate::job::Job::assign(child.id());
    let stdin_result = if let Some(input) = spec.stdin.clone() {
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            #[cfg(windows)]
            drop(child_job);
            let _ = child.wait();
            return Err(LaunchError::Failed);
        };
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(stdin.write_all(input.as_bytes()));
        });
        Some(rx)
    } else {
        None
    };

    let stdout = child.stdout.take().ok_or(LaunchError::Failed)?;
    let stderr = child.stderr.take().ok_or(LaunchError::Failed)?;
    let (out_tx, out_rx) = mpsc::sync_channel(1);
    let (err_tx, err_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = out_tx.send(read_capped(stdout, max_output + 1));
    });
    std::thread::spawn(move || {
        let _ = err_tx.send(read_capped(stderr, max_output));
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                #[cfg(windows)]
                drop(child_job);
                let _ = child.wait();
                return Err(LaunchError::Timeout);
            }
            Err(_) => {
                let _ = child.kill();
                #[cfg(windows)]
                drop(child_job);
                let _ = child.wait();
                return Err(LaunchError::Failed);
            }
        }
    };
    #[cfg(windows)]
    drop(child_job);
    if !status.success() {
        return Err(LaunchError::Failed);
    }
    if let Some(stdin_result) = stdin_result {
        let remaining = timeout.saturating_sub(started.elapsed());
        stdin_result
            .recv_timeout(remaining)
            .map_err(|_| LaunchError::Timeout)?
            .map_err(|_| LaunchError::Failed)?;
    }
    let remaining = timeout.saturating_sub(started.elapsed());
    let stdout = out_rx
        .recv_timeout(remaining)
        .map_err(|_| LaunchError::Timeout)?;
    let remaining = timeout.saturating_sub(started.elapsed());
    err_rx
        .recv_timeout(remaining)
        .map_err(|_| LaunchError::Timeout)?;
    if stdout.len() > max_output {
        return Err(LaunchError::TooLarge);
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

#[cfg(windows)]
fn cmd_fallback_word_is_safe(word: &str) -> bool {
    !word.chars().any(|ch| {
        matches!(
            ch,
            '\0' | '\r' | '\n' | '"' | '%' | '!' | '^' | '&' | '|' | '<' | '>'
        )
    })
}

/// Keep at most `cap` bytes but drain the pipe to EOF so the child never
/// blocks on a full pipe.
fn read_capped(mut reader: impl Read, cap: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = reader.read(&mut buf) {
        if n == 0 {
            break;
        }
        let remaining = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(remaining)]);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(
        provider: NamingProvider,
        model: &'a str,
        system: Option<&'a str>,
        prompt: &'a str,
    ) -> Request<'a> {
        Request {
            provider,
            model,
            system,
            prompt,
            cwd: Path::new("."),
            timeout: Duration::from_secs(1),
            max_output: 1024,
        }
    }

    #[test]
    fn provider_commands_are_isolated_and_model_exact() {
        let codex = command_spec(&request(
            NamingProvider::Codex,
            "gpt-5.6-luna",
            Some("SYSTEM"),
            "fix auth",
        ));
        assert_eq!(codex.program, "codex");
        assert!(
            codex
                .args
                .windows(2)
                .any(|w| w == ["--model", "gpt-5.6-luna"])
        );
        assert!(codex.args.iter().any(|a| a == "--ephemeral"));
        assert!(codex.args.iter().any(|a| a == "--ignore-user-config"));
        assert!(
            codex
                .args
                .windows(2)
                .any(|w| w == ["--disable", "shell_tool"])
        );
        assert_eq!(codex.args.last().map(String::as_str), Some("-"));
        let stdin = codex.stdin.as_deref().unwrap();
        assert!(stdin.starts_with("SYSTEM\n\n") && stdin.ends_with("fix auth"));

        let claude = command_spec(&request(
            NamingProvider::Claude,
            "sonnet",
            Some("SYSTEM"),
            "fix auth",
        ));
        assert_eq!(claude.program, "claude");
        assert!(claude.args.iter().any(|a| a == "--safe-mode"));
        assert!(claude.args.windows(2).any(|w| w == ["--tools", ""]));
        assert!(
            claude
                .args
                .windows(2)
                .any(|w| w == ["--system-prompt", "SYSTEM"])
        );
        assert_eq!(claude.stdin.as_deref(), Some("fix auth"));

        let grok = command_spec(&request(NamingProvider::Grok, "", None, "fix auth"));
        assert_eq!(grok.program, "grok");
        assert!(!grok.args.iter().any(|a| a == "--model"));
        assert!(!grok.args.iter().any(|a| a == "--system-prompt-override"));
        // --single takes the prompt as its value; nothing may sit between.
        assert!(grok.args.windows(2).any(|w| w == ["--single", "fix auth"]));
        assert!(grok.stdin.is_none());
    }

    #[test]
    fn no_system_prompt_sends_the_prompt_unchanged() {
        let codex = command_spec(&request(NamingProvider::Codex, "", None, "hi"));
        assert_eq!(codex.stdin.as_deref(), Some("hi"));
        assert!(!codex.args.iter().any(|a| a == "--model"));
        let claude = command_spec(&request(NamingProvider::Claude, "", None, "hi"));
        assert!(!claude.args.iter().any(|a| a == "--system-prompt"));
    }

    #[cfg(windows)]
    fn cmd(script: &str, stdin: Option<String>) -> CommandSpec {
        CommandSpec {
            program: "cmd.exe".into(),
            args: vec!["/d".into(), "/s".into(), "/c".into(), script.into()],
            stdin,
        }
    }

    #[cfg(windows)]
    #[test]
    fn provider_process_scrubs_hook_routing() {
        let cwd = tempfile::tempdir().unwrap();
        let output = run_process(
            &cmd("echo [%FOREMAN%]", None),
            cwd.path(),
            Duration::from_secs(10),
            1024,
        )
        .unwrap();
        assert!(!output.contains("[1]"), "FOREMAN leaked to one-shot child");
    }

    #[cfg(windows)]
    #[test]
    fn provider_process_kills_slow_child_at_deadline() {
        let cwd = tempfile::tempdir().unwrap();
        assert_eq!(
            run_process(
                &cmd("ping 127.0.0.1 -n 6 >nul", None),
                cwd.path(),
                Duration::from_millis(50),
                1024
            ),
            Err(LaunchError::Timeout)
        );
    }

    #[cfg(windows)]
    #[test]
    fn provider_process_bounds_blocked_stdin() {
        let cwd = tempfile::tempdir().unwrap();
        let blocked_stdin = cmd(
            "ping 127.0.0.1 -n 3 >nul",
            Some("x".repeat(2 * 1024 * 1024)),
        );
        let started = Instant::now();
        assert_eq!(
            run_process(&blocked_stdin, cwd.path(), Duration::from_millis(50), 1024),
            Err(LaunchError::Timeout)
        );
        assert!(
            started.elapsed() < Duration::from_millis(750),
            "stdin must be inside the process deadline, elapsed {:?}",
            started.elapsed()
        );
    }

    #[cfg(windows)]
    #[test]
    fn provider_process_reports_nonzero_exit_as_failed() {
        let cwd = tempfile::tempdir().unwrap();
        assert_eq!(
            run_process(
                &cmd("exit /b 7", None),
                cwd.path(),
                Duration::from_secs(10),
                1024
            ),
            Err(LaunchError::Failed)
        );
    }

    #[cfg(windows)]
    #[test]
    fn provider_process_rejects_oversized_output() {
        let cwd = tempfile::tempdir().unwrap();
        let spec = cmd("echo 0123456789abcdef", None);
        assert_eq!(
            run_process(&spec, cwd.path(), Duration::from_secs(10), 8),
            Err(LaunchError::TooLarge)
        );
        assert!(run_process(&spec, cwd.path(), Duration::from_secs(10), 64).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn provider_process_falls_back_to_safe_windows_cmd_shim() {
        let cwd = tempfile::tempdir().unwrap();
        let shim_dir = cwd.path().join("shim dir");
        std::fs::create_dir(&shim_dir).unwrap();
        let shim = shim_dir.join("fake-namer.cmd");
        std::fs::write(
            &shim,
            "@echo off\r\nset /p ignored=\r\necho Useful Shim Title\r\n",
        )
        .unwrap();
        let bare_program = shim.with_extension("").to_string_lossy().into_owned();
        let spec = CommandSpec {
            program: bare_program.clone(),
            args: Vec::new(),
            stdin: Some("name this prompt\n".into()),
        };

        let output = run_process(&spec, cwd.path(), Duration::from_secs(10), 1024).unwrap();
        assert_eq!(output.trim(), "Useful Shim Title");

        let unsafe_spec = CommandSpec {
            program: bare_program,
            args: vec!["safe&echo injected".into()],
            stdin: None,
        };
        assert_eq!(
            run_process(&unsafe_spec, cwd.path(), Duration::from_secs(2), 1024),
            Err(LaunchError::Failed)
        );
    }
}
