//! A Remote's git identity, for the Remotes pane: the words for what Test
//! read ([`line`]), which of this PC's name and email a Remote lacks
//! ([`fix`]), and the one `ssh` that writes them to the Remote's global git
//! config ([`run_set`]). Only a value the Remote does not have is written,
//! and only on the user's confirm. Everything but the two process runs is
//! pure.

use std::time::Duration;

use crate::remote::RemoteShell;
use crate::remote_probe::GitInfo;

/// Prefix on the write script's answer line.
const MARK: &str = "foreman-git";
/// The whole write: a login plus two `git config`.
const SET_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest name or email written, in bytes.
const MAX_VALUE: usize = 256;

/// A git name and email; blank = not set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

/// What to write on a Remote: only what it lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl Fix {
    /// The values as the confirm shows them, one `key = value` per line.
    pub fn shown(&self) -> String {
        let mut out = Vec::new();
        if let Some(n) = &self.name {
            out.push(format!("user.name = {n}"));
        }
        if let Some(e) = &self.email {
            out.push(format!("user.email = {e}"));
        }
        out.join("\n")
    }
}

/// A row's git line, and whether the Remote lacks a name or an email.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub lacks: bool,
}

/// The words for what Test read of a Remote's git.
pub fn line(git: Option<&GitInfo>) -> Line {
    let Some(g) = git else {
        return Line {
            text: "git is not installed".into(),
            lacks: false,
        };
    };
    let who = match (g.name.is_empty(), g.email.is_empty()) {
        (false, false) => format!("{} <{}>", g.name, g.email),
        (true, true) => "no name or email set: commits here get a made-up author".into(),
        (true, false) => format!("no name set (email {})", g.email),
        (false, true) => format!("{}, no email set", g.name),
    };
    let version = if g.version.is_empty() {
        "git".to_string()
    } else {
        format!("git {}", g.version)
    };
    Line {
        text: format!("{version} · {who}"),
        lacks: g.name.is_empty() || g.email.is_empty(),
    }
}

/// Why `value` cannot be written, or `Ok`. One line; no `"`, which a
/// Windows Remote's command line cannot carry to git.
fn check_value(what: &str, value: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) || value.contains('"') {
        return Err(format!(
            "This PC's git {what} holds a quote or a control character, so it can't be copied"
        ));
    }
    if value.len() > MAX_VALUE {
        return Err(format!("This PC's git {what} is too long to copy"));
    }
    Ok(())
}

/// What `remote` (named `name`) lacks that this PC (`local`) has. `Err` says
/// in plain words why there is nothing to write.
pub fn fix(name: &str, remote: Option<&GitInfo>, local: &Identity) -> Result<Fix, String> {
    let Some(g) = remote else {
        return Err(format!("git is not installed on {name}"));
    };
    if !g.name.is_empty() && !g.email.is_empty() {
        return Err(format!("{name} already has a git name and email"));
    }
    let want = |have: &str, mine: &str, what: &str| -> Result<Option<String>, String> {
        if !have.is_empty() {
            return Ok(None);
        }
        if mine.is_empty() {
            return Err(format!(
                "This PC has no git {what} to copy: set it here first (git config --global user.{what})"
            ));
        }
        check_value(what, mine)?;
        Ok(Some(mine.to_string()))
    };
    Ok(Fix {
        name: want(&g.name, &local.name, "name")?,
        email: want(&g.email, &local.email, "email")?,
    })
}

/// The Posix write: the values go in as environment, so the login shell
/// that finds `git` (the PATH Test looked on) never parses them.
pub fn posix_script(fix: &Fix) -> String {
    let mut s = String::new();
    let mut inner = String::new();
    for (var, key, value) in [
        ("FOREMAN_GIT_NAME", "user.name", &fix.name),
        ("FOREMAN_GIT_EMAIL", "user.email", &fix.email),
    ] {
        if let Some(v) = value {
            s.push_str(&format!("export {var}={}\n", crate::remote::sh_quote(v)));
            inner.push_str(&format!("git config --global {key} \"${var}\" || exit 1; "));
        }
    }
    s.push_str(&format!(
        "exec \"${{SHELL:-sh}}\" -lic '{inner}echo \"{MARK} set\"'\n"
    ));
    s
}

/// The PowerShell write (pwsh and 5.1). Untested: no Windows Remote has
/// existed.
pub fn ps_script(fix: &Fix) -> String {
    let mut s = String::from("$ErrorActionPreference = 'Stop'\ntry {\n");
    for (key, value) in [("user.name", &fix.name), ("user.email", &fix.email)] {
        if let Some(v) = value {
            s.push_str(&format!(
                "  git config --global {key} {}\n  if ($LASTEXITCODE) {{ throw 'git config {key} failed' }}\n",
                crate::remote::ps_quote(v)
            ));
        }
    }
    s.push_str(&format!(
        "  '{MARK} set'\n}} catch {{ '{MARK} fail ' + $_.Exception.Message }}\n"
    ));
    s
}

/// Read the write's stdout: `Ok` when it said it set them.
pub fn parse_set(stdout: &str) -> Option<Result<(), String>> {
    let rest = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix(MARK))?
        .trim();
    if rest == "set" {
        return Some(Ok(()));
    }
    Some(Err(rest
        .strip_prefix("fail")
        .unwrap_or(rest)
        .trim()
        .to_string()))
}

/// Write `fix` on the Remote now. Blocks up to [`SET_TIMEOUT`]: worker
/// threads only. `Err` is one plain sentence.
pub fn run_set(target: &str, shell: RemoteShell, fix: &Fix) -> Result<(), String> {
    let line = crate::remote_browse::remote_line(shell, || posix_script(fix), || ps_script(fix))
        .ok_or("the values are too long for a Windows Remote's command line")?;
    let args = crate::remote_browse::ssh_args(target, line);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = crate::remote_probe::run(&args, SET_TIMEOUT)?;
    match parse_set(&out.stdout) {
        Some(Ok(())) => Ok(()),
        Some(Err(e)) if !e.is_empty() => Err(e),
        _ => Err(crate::remote_probe::last_line(
            &out.stderr,
            "git config failed on the Remote",
        )),
    }
}

/// This PC's global git name and email. Runs `git` twice: call it on a
/// click, never per frame.
pub fn local_identity() -> Identity {
    let get = |key: &str| {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["config", "--global", "--get", key])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd.output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    Identity {
        name: get("user.name"),
        email: get("user.email"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, email: &str) -> GitInfo {
        GitInfo {
            version: "2.43.0".into(),
            name: name.into(),
            email: email.into(),
        }
    }

    fn me() -> Identity {
        Identity {
            name: "Ann O'Dev".into(),
            email: "ann@example.com".into(),
        }
    }

    #[test]
    fn the_line_says_what_is_there_and_what_is_missing() {
        for (git, want, lacks) in [
            (None, "git is not installed", false),
            (
                Some(info("Ann", "a@b.c")),
                "git 2.43.0 · Ann <a@b.c>",
                false,
            ),
            (
                Some(info("", "")),
                "git 2.43.0 · no name or email set: commits here get a made-up author",
                true,
            ),
            (
                Some(info("Ann", "")),
                "git 2.43.0 · Ann, no email set",
                true,
            ),
            (
                Some(info("", "a@b.c")),
                "git 2.43.0 · no name set (email a@b.c)",
                true,
            ),
        ] {
            let l = line(git.as_ref());
            assert_eq!((l.text.as_str(), l.lacks), (want, lacks));
        }
    }

    #[test]
    fn a_fix_writes_only_what_the_remote_lacks() {
        let f = fix("desk", Some(&info("", "")), &me()).unwrap();
        assert_eq!(f.name.as_deref(), Some("Ann O'Dev"));
        assert_eq!(f.email.as_deref(), Some("ann@example.com"));
        assert_eq!(
            f.shown(),
            "user.name = Ann O'Dev\nuser.email = ann@example.com"
        );
        // A value the Remote has is never replaced.
        let f = fix("desk", Some(&info("Other", "")), &me()).unwrap();
        assert_eq!(
            (f.name, f.email.as_deref()),
            (None, Some("ann@example.com"))
        );
    }

    #[test]
    fn nothing_to_fix_says_why_in_plain_words() {
        let e = |r: Option<GitInfo>, l: Identity| fix("desk", r.as_ref(), &l).unwrap_err();
        assert_eq!(e(None, me()), "git is not installed on desk");
        assert!(e(Some(info("A", "a@b")), me()).contains("already has"));
        let none = Identity::default();
        assert!(e(Some(info("", "")), none).contains("This PC has no git name"));
        let no_email = Identity {
            name: "Ann".into(),
            email: String::new(),
        };
        assert!(e(Some(info("", "")), no_email.clone()).contains("no git email"));
        // The Remote has the one this PC lacks: nothing missing to refuse.
        assert!(fix("desk", Some(&info("", "x@y")), &no_email).is_ok());
        let quoted = Identity {
            name: "Ann \"A\" Dev".into(),
            email: "a@b".into(),
        };
        assert!(e(Some(info("", "")), quoted).contains("quote"));
    }

    #[test]
    fn the_posix_write_carries_values_as_environment() {
        let f = fix("desk", Some(&info("", "")), &me()).unwrap();
        let s = posix_script(&f);
        assert!(
            s.contains("export FOREMAN_GIT_NAME='Ann O'\\''Dev'\n"),
            "{s}"
        );
        assert!(s.contains("export FOREMAN_GIT_EMAIL='ann@example.com'\n"));
        let inner = s.lines().last().unwrap();
        assert!(inner.contains("git config --global user.name \"$FOREMAN_GIT_NAME\" || exit 1;"));
        assert!(inner.contains("git config --global user.email \"$FOREMAN_GIT_EMAIL\" || exit 1;"));
        assert!(
            !inner.contains("Ann"),
            "no value inside the login shell's words"
        );
        // Only the missing one.
        let f = fix("desk", Some(&info("Other", "")), &me()).unwrap();
        let s = posix_script(&f);
        assert!(!s.contains("user.name") && s.contains("user.email"));
    }

    #[test]
    fn the_powershell_write_quotes_each_value_once() {
        let f = fix("desk", Some(&info("", "")), &me()).unwrap();
        let s = ps_script(&f);
        assert!(
            s.contains("git config --global user.name 'Ann O''Dev'\n"),
            "{s}"
        );
        assert!(s.contains("git config --global user.email 'ann@example.com'\n"));
        assert!(s.contains("'foreman-git set'"));
    }

    #[test]
    fn the_answer_is_read_past_login_noise() {
        assert_eq!(parse_set("motd\nforeman-git set\n"), Some(Ok(())));
        assert_eq!(
            parse_set("foreman-git fail git config user.name failed\r\n"),
            Some(Err("git config user.name failed".into()))
        );
        assert_eq!(parse_set("motd only\n"), None);
    }

    #[test]
    fn a_write_never_starts_under_test() {
        let f = fix("desk", Some(&info("", "")), &me()).unwrap();
        assert!(run_set("nowhere.invalid", RemoteShell::Posix, &f).is_err());
    }
}
