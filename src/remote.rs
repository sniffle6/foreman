//! Remotes: PCs foreman opens Sessions on over SSH, and the pure builder that
//! turns a Remote plus a logical argv into the local `ssh` argv. Nothing here
//! spawns or touches the network. Spec:
//! `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Most entries `Settings::remotes` keeps.
pub const MAX_REMOTES: usize = 32;

/// Longest Remote name. It is a tab label and a `--on` argument.
pub const MAX_NAME: usize = 32;

/// The name the launcher's where-strip uses for this PC.
pub const RESERVED_NAME: &str = "local";

/// Longest remote line a Windows Remote gets. cmd's limit is 8191 for the
/// whole command line, and sshd wraps ours in `cmd.exe /c`; the gap is
/// headroom for that wrapper.
pub const WINDOWS_LINE_CAP: usize = 8000;

/// The login shell sshd runs on the Remote, which decides the script's
/// language. Posix is the default so a half-filled entry still loads; Test
/// fills in the real one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteShell {
    Pwsh,
    WindowsPowerShell,
    #[default]
    Posix,
}

/// A named PC foreman can open Sessions on over SSH.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Remote {
    /// Launcher label, tab tooltip, and the `--on` argument.
    pub name: String,
    /// Whatever `ssh` accepts as a destination: `user@pc` or a config alias.
    pub target: String,
    #[serde(default)]
    pub shell: RemoteShell,
    /// Seat-PC Project directory -> directory on this Remote. A blank value
    /// is a saved answer meaning the Remote's home.
    #[serde(default)]
    pub dirs: BTreeMap<String, String>,
}

/// Name rule: `config::valid_peer` characters, short, and not `local`.
/// Uniqueness is a list property; `sanitize` checks it.
pub fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && crate::config::valid_peer(name)
        && !name.eq_ignore_ascii_case(RESERVED_NAME)
}

/// Target rule. A leading `-` would be read by `ssh` as an option, so it is
/// refused here even though the builder also puts `--` before the target.
pub fn valid_target(target: &str) -> bool {
    !target.is_empty()
        && !target.starts_with('-')
        && !target.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Trim, validate, de-duplicate (case-insensitive) and cap a loaded list.
/// The first entry with a name wins.
pub fn sanitize(remotes: Vec<Remote>) -> Vec<Remote> {
    let mut seen = std::collections::HashSet::new();
    remotes
        .into_iter()
        .map(|mut r| {
            r.name = r.name.trim().to_string();
            r.target = r.target.trim().to_string();
            for dir in r.dirs.values_mut() {
                *dir = dir.trim().to_string();
            }
            r.dirs.retain(|project, _| !project.trim().is_empty());
            r
        })
        .filter(|r| valid_name(&r.name) && valid_target(&r.target))
        .filter(|r| seen.insert(r.name.to_ascii_lowercase()))
        .take(MAX_REMOTES)
        .collect()
}

/// `Settings::remotes` deserializer: an entry that does not parse (a typo'd
/// shell, a missing target) is dropped alone instead of failing the whole
/// settings file.
pub fn deserialize_lenient<'de, D>(d: D) -> Result<Vec<Remote>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// The Remote `--on <name>` names. Case-insensitive, like `sanitize`'s
/// uniqueness rule.
pub fn find<'a>(remotes: &'a [Remote], name: &str) -> Result<&'a Remote, String> {
    if let Some(r) = remotes.iter().find(|r| r.name.eq_ignore_ascii_case(name)) {
        return Ok(r);
    }
    if remotes.is_empty() {
        return Err(format!(
            "no Remote named {name:?}: none are set up (remotes in settings.json)"
        ));
    }
    let known: Vec<&str> = remotes.iter().map(|r| r.name.as_str()).collect();
    Err(format!(
        "no Remote named {name:?} (known: {})",
        known.join(", ")
    ))
}

/// Windows path equality for `Remote::dirs` keys: case, slash direction and
/// a trailing separator do not make a different Project directory.
fn same_dir(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).replace('/', "\\");
    norm(a).eq_ignore_ascii_case(&norm(b))
}

/// The directory on `remote` a launch from a Project at `project_dir` runs
/// in. An explicit `cwd` wins and is not saved; otherwise the Project's
/// mapping (blank = the Remote's home). Neither is an error naming both fixes,
/// because guessing would start an agent in the wrong place.
pub fn launch_dir<'a>(
    remote: &'a Remote,
    cwd: Option<&'a str>,
    project_dir: Option<&str>,
) -> Result<&'a str, String> {
    if let Some(dir) = cwd {
        return Ok(dir);
    }
    let name = &remote.name;
    let Some(project_dir) = project_dir else {
        return Err(format!(
            "this Project has no directory to map to one on {name}; pass --cwd <directory on {name}>"
        ));
    };
    remote
        .dirs
        .iter()
        .find(|(seat, _)| same_dir(seat, project_dir))
        .map(|(_, there)| there.as_str())
        .ok_or_else(|| {
            format!(
                "{name} has no directory for this Project ({project_dir}); pass --cwd <directory on {name}>, or map {project_dir} in {name}'s dirs in settings.json (edit it with foreman closed)"
            )
        })
}

/// The per-Session reverse forward back to the seat PC's listener.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(test), allow(dead_code))] // the listener lands in a later card
pub struct Tunnel<'a> {
    /// Port the Remote's agent dials on its own loopback.
    pub remote_port: u16,
    /// The seat PC's loopback listener port.
    pub local_port: u16,
    /// Bearer token naming this Session to the listener.
    pub token: &'a str,
}

/// Everything one Remote Session launch needs.
pub struct Launch<'a> {
    pub remote: &'a Remote,
    /// Directory on the Remote. Blank = the Remote's home.
    pub dir: &'a str,
    /// `t{id}`, as `FOREMAN_TERMINAL_ID` is locally.
    pub terminal_id: &'a str,
    /// `p{id}`; `None` outside a Project, as locally.
    pub project_id: Option<&'a str>,
    /// `None` until the way-back listener exists: no forward, no URL/token.
    pub tunnel: Option<Tunnel<'a>>,
    /// What the user asked to run (`claude …`). Empty = an interactive shell.
    pub argv: &'a [String],
}

#[derive(Debug, PartialEq)]
pub struct Built {
    /// The local process: `ssh … -- <target> <remote line>`.
    pub ssh: Vec<String>,
    /// The logical argv, unchanged, for `IconKind::from_argv`. The wrapped
    /// line never contains the agent's name in readable form.
    pub dispatched: Vec<String>,
}

/// Build the local `ssh` argv for a launch. Refuses (never truncates) a
/// Windows launch whose remote line would not fit cmd.
pub fn build(l: &Launch) -> Result<Built, String> {
    // Same rule as a local spawn, applied before the argv is buried in base64.
    let run = crate::agent_command::codex_in_process(l.argv).unwrap_or_else(|| l.argv.to_vec());

    let mut env: Vec<(&str, String)> = vec![
        ("FOREMAN", "1".into()),
        ("FOREMAN_TERMINAL_ID", l.terminal_id.into()),
    ];
    if let Some(p) = l.project_id {
        env.push(("FOREMAN_PROJECT_ID", p.into()));
    }
    env.push(("COLORTERM", "truecolor".into()));
    if let Some(t) = l.tunnel {
        env.push(("FOREMAN_URL", format!("http://127.0.0.1:{}", t.remote_port)));
        env.push(("FOREMAN_TOKEN", t.token.into()));
    }

    let line = match l.remote.shell {
        RemoteShell::Posix => posix_line(&posix_script(l.dir, &env, &run)),
        shell @ (RemoteShell::Pwsh | RemoteShell::WindowsPowerShell) => {
            let exe = if shell == RemoteShell::Pwsh {
                "pwsh"
            } else {
                "powershell"
            };
            if shell == RemoteShell::WindowsPowerShell
                && let Some(bad) = run.iter().skip(1).find(|a| !legacy_passable(a))
            {
                let preview: String = bad.chars().take(40).collect();
                return Err(format!(
                    "Windows PowerShell 5.1 on {} cannot pass the argument {preview:?}… to a program intact (a quote comes before every space); set the Remote's shell to pwsh, or open without it and send it by chat",
                    l.remote.name
                ));
            }
            let no_exit = if run.is_empty() { " -NoExit" } else { "" };
            let line = format!(
                "{exe} -NoLogo{no_exit} -EncodedCommand {}",
                utf16le_b64(&ps_script(
                    l.dir,
                    &env,
                    &run,
                    shell == RemoteShell::WindowsPowerShell
                ))
            );
            if line.len() > WINDOWS_LINE_CAP {
                return Err(format!(
                    "this launch is {} characters on {}'s command line, over the {WINDOWS_LINE_CAP} a Windows Remote allows; open the Session without the prompt and send the brief by chat",
                    line.len(),
                    l.remote.name
                ));
            }
            line
        }
    };

    let mut ssh: Vec<String> = ["ssh", "-t"].map(String::from).to_vec();
    if l.tunnel.is_some() {
        ssh.extend(["-o", "ExitOnForwardFailure=yes"].map(String::from));
    }
    ssh.extend(
        [
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]
        .map(String::from),
    );
    if let Some(t) = l.tunnel {
        ssh.push("-R".into());
        ssh.push(format!("{}:127.0.0.1:{}", t.remote_port, t.local_port));
    }
    ssh.push("--".into());
    ssh.push(l.remote.target.clone());
    ssh.push(line);
    Ok(Built {
        ssh,
        dispatched: l.argv.to_vec(),
    })
}

/// POSIX single-quote: `'` becomes `'\''`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// PowerShell single-quote: `'` doubled. PowerShell also treats the curly
/// quotes U+2018..U+201B as single quotes, so they are doubled too.
fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// `~` and `~/rest` mean the Remote's home; anything else is literal.
fn home_relative(dir: &str) -> Option<&str> {
    if dir == "~" {
        return Some("");
    }
    dir.strip_prefix("~/").or_else(|| dir.strip_prefix("~\\"))
}

fn posix_script(dir: &str, env: &[(&str, String)], argv: &[String]) -> String {
    let mut s = String::new();
    if !dir.is_empty() {
        let target = match home_relative(dir) {
            Some("") => "\"$HOME\"".to_string(),
            Some(rest) => format!("\"$HOME\"/{}", sh_quote(rest)),
            None => sh_quote(dir),
        };
        s.push_str(&format!("cd -- {target} || exit 1\n"));
    }
    for (k, v) in env {
        s.push_str(&format!("export {k}={}\n", sh_quote(v)));
    }
    // The user's own login shell, so PATH from their startup files applies;
    // the argv rides as positional parameters so it is quoted exactly once.
    if argv.is_empty() {
        s.push_str("exec \"${SHELL:-sh}\" -li\n");
    } else {
        s.push_str("exec \"${SHELL:-sh}\" -lic 'exec \"$@\"' foreman-launch");
        for a in argv {
            s.push(' ');
            s.push_str(&sh_quote(a));
        }
        s.push('\n');
    }
    s
}

fn posix_line(script: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    format!("sh -c \"$(printf %s {b64} | base64 -d)\"")
}

fn ps_script(dir: &str, env: &[(&str, String)], argv: &[String], legacy: bool) -> String {
    let mut s = String::new();
    if !dir.is_empty() {
        let target = match home_relative(dir) {
            Some("") => "$HOME".to_string(),
            Some(rest) => format!("(Join-Path $HOME {})", ps_quote(rest)),
            None => ps_quote(dir),
        };
        s.push_str(&format!(
            "try {{ Set-Location -LiteralPath {target} -ErrorAction Stop }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}\n"
        ));
    }
    for (k, v) in env {
        s.push_str(&format!("$env:{k} = {}\n", ps_quote(v)));
    }
    if !argv.is_empty() {
        s.push('&');
        for (i, a) in argv.iter().enumerate() {
            s.push(' ');
            if legacy && i > 0 {
                s.push_str(&ps_quote(&legacy_native(a)));
            } else {
                s.push_str(&ps_quote(a));
            }
        }
        s.push_str("\nexit $LASTEXITCODE\n");
    }
    s
}

/// Windows PowerShell 5.1 hands a native program its arguments without
/// escaping embedded `"` (it only wraps an argument holding whitespace in
/// quotes), so `say "hi"` arrives as `say hi`. Pre-escape for the receiving
/// program's command-line parser: `"` becomes `\"`, backslashes before a `"`
/// double, and trailing backslashes double when 5.1 will add the closing
/// quote. pwsh 7.3+ escapes correctly itself and must not get this.
/// Whether 5.1 can pass `a` at all. It wraps an argument in quotes only when
/// it sees whitespace outside quotes, and it counts escaped `\"` as quotes
/// too. If every space follows an odd number of `"`, the escaped form is
/// passed bare and splits, and no escaping fixes that.
fn legacy_passable(a: &str) -> bool {
    if !a.chars().any(char::is_whitespace) {
        return true;
    }
    let mut quotes = 0;
    legacy_native(a).chars().any(|c| {
        if c == '"' {
            quotes += 1;
        }
        c.is_whitespace() && quotes % 2 == 0
    })
}

fn legacy_native(a: &str) -> String {
    const BS: char = '\\';
    if a.is_empty() {
        return "\"\"".into(); // 5.1 drops an empty argument outright
    }
    let mut out = String::with_capacity(a.len());
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            BS => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n(BS, slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n(BS, slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    let wrapped = a.chars().any(char::is_whitespace);
    out.extend(std::iter::repeat_n(
        BS,
        if wrapped { slashes * 2 } else { slashes },
    ));
    out
}

fn utf16le_b64(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(shell: RemoteShell) -> Remote {
        Remote {
            name: "desk".into(),
            target: "andy@desk".into(),
            shell,
            dirs: BTreeMap::new(),
        }
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn launch<'a>(r: &'a Remote, dir: &'a str, argv: &'a [String]) -> Launch<'a> {
        Launch {
            remote: r,
            dir,
            terminal_id: "t7",
            project_id: Some("p3"),
            tunnel: Some(Tunnel {
                remote_port: 41234,
                local_port: 50001,
                token: "tok",
            }),
            argv,
        }
    }

    /// The script a built line carries, decoded.
    fn script(b: &Built, shell: RemoteShell) -> String {
        let line = b.ssh.last().unwrap();
        let engine = base64::engine::general_purpose::STANDARD;
        match shell {
            RemoteShell::Posix => {
                let b64 = line
                    .strip_prefix("sh -c \"$(printf %s ")
                    .and_then(|r| r.strip_suffix(" | base64 -d)\""))
                    .expect("posix wrapper");
                String::from_utf8(engine.decode(b64).unwrap()).unwrap()
            }
            _ => {
                let b64 = line.rsplit(' ').next().unwrap();
                let bytes = engine.decode(b64).unwrap();
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16(&units).unwrap()
            }
        }
    }

    #[test]
    fn remote_posix_launch_matches_the_hand_proved_form() {
        let r = remote(RemoteShell::Posix);
        let a = argv(&["claude", "--model", "x"]);
        let b = build(&launch(&r, "/home/andy/src", &a)).unwrap();
        assert_eq!(
            b.ssh[..b.ssh.len() - 1],
            argv(&[
                "ssh",
                "-t",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-R",
                "41234:127.0.0.1:50001",
                "--",
                "andy@desk",
            ])[..]
        );
        let line = b.ssh.last().unwrap();
        // The line sshd's login shell parses: letters, digits, +/= and a fixed wrapper.
        let b64 = &line["sh -c \"$(printf %s ".len()..line.len() - " | base64 -d)\"".len()];
        assert!(
            b64.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
        );
        assert_eq!(
            script(&b, RemoteShell::Posix),
            "cd -- '/home/andy/src' || exit 1\n\
             export FOREMAN='1'\n\
             export FOREMAN_TERMINAL_ID='t7'\n\
             export FOREMAN_PROJECT_ID='p3'\n\
             export COLORTERM='truecolor'\n\
             export FOREMAN_URL='http://127.0.0.1:41234'\n\
             export FOREMAN_TOKEN='tok'\n\
             exec \"${SHELL:-sh}\" -lic 'exec \"$@\"' foreman-launch 'claude' '--model' 'x'\n"
        );
    }

    #[test]
    fn remote_scripts_quote_hostile_text_per_shell() {
        // (input, Posix literal, pwsh literal, Windows PowerShell argv literal).
        // Each was run through the real shell into a program printing its argv.
        let cases = [
            ("two words", "'two words'", "'two words'", "'two words'"),
            ("it's", r"'it'\''s'", "'it''s'", "'it''s'"),
            (
                r#"say "hi""#,
                r#"'say "hi"'"#,
                r#"'say "hi"'"#,
                r#"'say \"hi\"'"#,
            ),
            (r"x y\", r"'x y\'", r"'x y\'", r"'x y\\'"),
            (r#"a\"b"#, r#"'a\"b'"#, r#"'a\"b'"#, r#"'a\\\"b'"#),
            ("", "''", "''", r#"'""'"#),
            (
                "$HOME and $(id)",
                "'$HOME and $(id)'",
                "'$HOME and $(id)'",
                "'$HOME and $(id)'",
            ),
            (
                "a\u{2019}b",
                "'a\u{2019}b'",
                "'a\u{2019}\u{2019}b'",
                "'a\u{2019}\u{2019}b'",
            ),
        ];
        for (input, posix, pwsh, legacy) in cases {
            let a = argv(&["claude", input]);
            let dir = format!("/x/{input}");
            for (shell, want_arg, want_dir) in [
                (RemoteShell::Posix, posix, posix),
                (RemoteShell::Pwsh, pwsh, pwsh),
                // Set-Location is a cmdlet: only native argv gets the 5.1 escape.
                (RemoteShell::WindowsPowerShell, legacy, pwsh),
            ] {
                let r = remote(shell);
                let s = script(&build(&launch(&r, &dir, &a)).unwrap(), shell);
                assert!(
                    s.contains(&format!(" {want_arg}\n")),
                    "{shell:?} argv {input:?}:\n{s}"
                );
                // dir is "/x/" + input, quoted by the same rule.
                assert!(
                    s.contains(&format!("'/x/{}", &want_dir[1..])),
                    "{shell:?} dir {input:?}:\n{s}"
                );
            }
        }
        // The command name itself is never 5.1-escaped.
        let r = remote(RemoteShell::WindowsPowerShell);
        let a = argv(&[r#"C:\tools\"x"\bin.exe"#]);
        assert!(
            script(&build(&launch(&r, "", &a)).unwrap(), r.shell)
                .contains(r#"& 'C:\tools\"x"\bin.exe'"#)
        );
    }

    #[test]
    fn remote_windows_powershell_refuses_an_argument_it_would_split() {
        // 5.1 sees the space as inside quotes, passes the arg bare, and it splits.
        let a = argv(&["claude", r#""quoted all""#]);
        let r = remote(RemoteShell::WindowsPowerShell);
        let e = build(&launch(&r, "", &a)).unwrap_err();
        assert!(e.contains("pwsh") && e.contains("chat"), "{e}");
        // pwsh passes it fine, and a space before the quote is fine on 5.1.
        assert!(build(&launch(&remote(RemoteShell::Pwsh), "", &a)).is_ok());
        let a = argv(&["claude", r#"so "quoted all""#]);
        assert!(build(&launch(&r, "", &a)).is_ok());
    }

    #[test]
    fn remote_windows_scripts_and_lines() {
        let a = argv(&["claude"]);
        for (shell, exe) in [
            (RemoteShell::Pwsh, "pwsh"),
            (RemoteShell::WindowsPowerShell, "powershell"),
        ] {
            let r = remote(shell);
            let b = build(&launch(&r, r"C:\src\my repo", &a)).unwrap();
            assert!(
                b.ssh
                    .last()
                    .unwrap()
                    .starts_with(&format!("{exe} -NoLogo -EncodedCommand ")),
                "{}",
                b.ssh.last().unwrap()
            );
            assert_eq!(
                script(&b, shell),
                "try { Set-Location -LiteralPath 'C:\\src\\my repo' -ErrorAction Stop } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }\n\
                 $env:FOREMAN = '1'\n\
                 $env:FOREMAN_TERMINAL_ID = 't7'\n\
                 $env:FOREMAN_PROJECT_ID = 'p3'\n\
                 $env:COLORTERM = 'truecolor'\n\
                 $env:FOREMAN_URL = 'http://127.0.0.1:41234'\n\
                 $env:FOREMAN_TOKEN = 'tok'\n\
                 & 'claude'\n\
                 exit $LASTEXITCODE\n"
            );
        }
    }

    #[test]
    fn remote_blank_dir_skips_the_cd_and_tilde_means_home() {
        let a = argv(&["claude"]);
        for shell in [RemoteShell::Posix, RemoteShell::Pwsh] {
            let r = remote(shell);
            let s = script(&build(&launch(&r, "", &a)).unwrap(), shell);
            assert!(!s.contains("cd --") && !s.contains("Set-Location"), "{s}");
        }
        let r = remote(RemoteShell::Posix);
        let s = script(
            &build(&launch(&r, "~/my src", &a)).unwrap(),
            RemoteShell::Posix,
        );
        assert!(s.starts_with("cd -- \"$HOME\"/'my src' || exit 1\n"), "{s}");
        let r = remote(RemoteShell::Pwsh);
        let s = script(&build(&launch(&r, "~", &a)).unwrap(), RemoteShell::Pwsh);
        assert!(
            s.starts_with("try { Set-Location -LiteralPath $HOME "),
            "{s}"
        );
    }

    #[test]
    fn remote_shell_launch_leaves_an_interactive_shell() {
        let r = remote(RemoteShell::Posix);
        let s = script(&build(&launch(&r, "", &[])).unwrap(), RemoteShell::Posix);
        assert!(s.ends_with("exec \"${SHELL:-sh}\" -li\n"), "{s}");
        let r = remote(RemoteShell::Pwsh);
        let b = build(&launch(&r, "", &[])).unwrap();
        assert!(
            b.ssh
                .last()
                .unwrap()
                .starts_with("pwsh -NoLogo -NoExit -EncodedCommand ")
        );
        assert!(!script(&b, RemoteShell::Pwsh).contains('&'));
        assert!(b.dispatched.is_empty());
    }

    #[test]
    fn remote_windows_over_cap_is_refused_not_truncated() {
        let brief = "x".repeat(4000); // ~2.7 line chars per script char
        let a = argv(&["claude", &brief]);
        for shell in [RemoteShell::Pwsh, RemoteShell::WindowsPowerShell] {
            let r = remote(shell);
            let e = build(&launch(&r, "", &a)).unwrap_err();
            assert!(e.contains("desk") && e.contains("chat"), "{e}");
        }
        // Posix has no practical cap.
        let r = remote(RemoteShell::Posix);
        assert!(build(&launch(&r, "", &a)).is_ok());
    }

    #[test]
    fn remote_keeps_the_logical_argv_and_applies_the_codex_rule_inside() {
        let a = argv(&["codex", "do the thing"]);
        let r = remote(RemoteShell::Posix);
        let b = build(&launch(&r, "", &a)).unwrap();
        assert_eq!(b.dispatched, a);
        assert!(b.ssh.iter().all(|w| !w.contains("codex")));
        assert!(
            script(&b, RemoteShell::Posix)
                .contains("foreman-launch 'codex' '--no-daemon' 'do the thing'\n")
        );
    }

    #[test]
    fn remote_without_a_tunnel_has_no_forward_and_no_url() {
        let r = remote(RemoteShell::Posix);
        let a = argv(&["claude"]);
        let mut l = launch(&r, "", &a);
        l.tunnel = None;
        l.project_id = None;
        let b = build(&l).unwrap();
        assert!(
            !b.ssh
                .iter()
                .any(|w| w == "-R" || w.contains("ExitOnForwardFailure"))
        );
        let s = script(&b, RemoteShell::Posix);
        assert!(!s.contains("FOREMAN_URL") && !s.contains("FOREMAN_TOKEN"));
        assert!(!s.contains("FOREMAN_PROJECT_ID"));
        // The target always follows `--`.
        let at = b.ssh.iter().position(|w| w == "--").unwrap();
        assert_eq!(b.ssh[at + 1], "andy@desk");
    }

    #[test]
    fn remote_sanitize_drops_bad_dup_and_reserved_entries() {
        let mk = |name: &str, target: &str| Remote {
            name: name.into(),
            target: target.into(),
            shell: RemoteShell::Posix,
            dirs: BTreeMap::new(),
        };
        let got = sanitize(vec![
            mk(" desk ", " andy@desk "),
            mk("DESK", "other"),               // duplicate, case-insensitive
            mk("local", "x"),                  // reserved
            mk("Local", "x"),                  // reserved
            mk("evil", "-oProxyCommand=calc"), // would be an ssh option
            mk("spaced", "a b"),
            mk("", "x"),
            mk("a/b", "x"),
            mk(&"n".repeat(MAX_NAME + 1), "x"),
            mk("laptop", "laptop-alias"),
        ]);
        let names: Vec<_> = got
            .iter()
            .map(|r| (r.name.as_str(), r.target.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![("desk", "andy@desk"), ("laptop", "laptop-alias")]
        );
    }

    #[test]
    fn remote_find_is_case_insensitive_and_names_the_known_ones() {
        let list = vec![remote(RemoteShell::Posix), {
            let mut r = remote(RemoteShell::Pwsh);
            r.name = "laptop".into();
            r
        }];
        assert_eq!(find(&list, "DESK").unwrap().name, "desk");
        let e = find(&list, "garage").unwrap_err();
        assert!(
            e.contains("\"garage\"") && e.contains("desk, laptop"),
            "{e}"
        );
        let e = find(&[], "desk").unwrap_err();
        assert!(e.contains("none are set up"), "{e}");
    }

    #[test]
    fn remote_launch_dir_prefers_cwd_then_mapping_and_names_both_fixes() {
        let mut r = remote(RemoteShell::Posix);
        r.dirs.insert(
            r"H:\claude code\foreman".into(),
            "/home/andy/foreman".into(),
        );
        r.dirs.insert(r"H:\scratch".into(), String::new());
        // --cwd wins over a mapping.
        assert_eq!(
            launch_dir(&r, Some("/tmp"), Some(r"H:\claude code\foreman")),
            Ok("/tmp")
        );
        // Case, slash direction and a trailing separator still match.
        assert_eq!(
            launch_dir(&r, None, Some("h:/Claude Code/foreman/")),
            Ok("/home/andy/foreman")
        );
        // A blank mapping is a saved answer: the Remote's home.
        assert_eq!(launch_dir(&r, None, Some(r"H:\scratch")), Ok(""));
        let e = launch_dir(&r, None, Some(r"H:\other")).unwrap_err();
        assert!(
            e.contains("--cwd") && e.contains("dirs") && e.contains(r"H:\other"),
            "{e}"
        );
        let e = launch_dir(&r, None, None).unwrap_err();
        assert!(e.contains("--cwd"), "{e}");
    }
}
