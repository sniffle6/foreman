//! Cut = release (docs/kanban-board.md §Cut). The board stamps the Done
//! cards synchronously, then this module runs the documented release
//! procedure (docs/installing-and-updating.md "How to cut a release") on one
//! background thread: checks, commit the card files, bump `Cargo.toml`, push
//! the branch, tag, push the tag. Each step reports over a channel; the
//! window manager drains it and the board draws the list.
//!
//! Every check runs before the first write. A failure stops the run and
//! reports git's stderr; nothing is ever undone in git. `committed` on the
//! failure says whether a local commit already exists, which decides whether
//! the manager returns the cards to Current (`uncut`) or leaves them.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// One step of the release, in run order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Checks,
    CommitCards,
    Bump,
    PushBranch,
    Tag,
    PushTag,
}

impl Step {
    pub const ALL: [Step; 6] = [
        Step::Checks,
        Step::CommitCards,
        Step::Bump,
        Step::PushBranch,
        Step::Tag,
        Step::PushTag,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Step::Checks => "checks",
            Step::CommitCards => "commit cards",
            Step::Bump => "bump version",
            Step::PushBranch => "push branch",
            Step::Tag => "tag",
            Step::PushTag => "push tag",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Running,
    Ok,
    /// Not applicable to this project (no `Cargo.toml` → no bump).
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseEvent {
    Step(Step, StepState),
    /// The run stopped at `step`. `committed`: a local commit already
    /// exists, so the cards stay shipped. `resume`: the command that
    /// finishes the release by hand, when one line can.
    Failed {
        step: Step,
        error: String,
        committed: bool,
        resume: Option<String>,
    },
    /// `actions_url`: the GitHub Actions page when `origin` is on github.com.
    Done {
        actions_url: Option<String>,
    },
}

/// How one step stands, for the board's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Pending,
    Running,
    Ok,
    Failed,
}

/// The run as the board draws it, folded from the events. The manager owns
/// the one copy and hands clones to its board views each frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub name: String,
    /// Run order; a skipped step is removed.
    pub steps: Vec<(Step, Mark)>,
    pub error: Option<String>,
    pub committed: bool,
    pub resume: Option<String>,
    pub finished: bool,
    pub actions_url: Option<String>,
}

impl Progress {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            steps: Step::ALL.iter().map(|s| (*s, Mark::Pending)).collect(),
            error: None,
            committed: false,
            resume: None,
            finished: false,
            actions_url: None,
        }
    }

    pub fn apply(&mut self, ev: &ReleaseEvent) {
        let mut mark = |step: Step, m: Mark| {
            if let Some(e) = self.steps.iter_mut().find(|(s, _)| *s == step) {
                e.1 = m;
            }
        };
        match ev {
            ReleaseEvent::Step(step, StepState::Skipped) => {
                self.steps.retain(|(s, _)| s != step);
            }
            ReleaseEvent::Step(step, StepState::Running) => mark(*step, Mark::Running),
            ReleaseEvent::Step(step, StepState::Ok) => mark(*step, Mark::Ok),
            ReleaseEvent::Failed {
                step,
                error,
                committed,
                resume,
            } => {
                mark(*step, Mark::Failed);
                self.error = Some(error.clone());
                self.committed = *committed;
                self.resume = resume.clone();
                self.finished = true;
            }
            ReleaseEvent::Done { actions_url } => {
                self.actions_url = actions_url.clone();
                self.finished = true;
            }
        }
    }
}

/// Run the release for version `name` on its own thread.
/// The manager keeps the UI repainting while the receiver is live.
pub fn spawn(cwd: PathBuf, name: String, tx: Sender<ReleaseEvent>) {
    std::thread::spawn(move || run(&cwd, &name, &tx));
}

/// The whole procedure, synchronously (tests drive this directly).
pub fn run(cwd: &Path, name: &str, tx: &Sender<ReleaseEvent>) {
    let mut r = Runner {
        tx,
        name,
        committed: false,
    };
    let _ = r.go(cwd);
}

struct Runner<'a> {
    tx: &'a Sender<ReleaseEvent>,
    name: &'a str,
    committed: bool,
}

/// What the checks learned, for the steps that write.
struct Plan {
    root: PathBuf,
    branch: String,
    tasks: String,
    cargo: Option<String>,
    actions_url: Option<String>,
}

impl Runner<'_> {
    fn send(&self, ev: ReleaseEvent) {
        let _ = self.tx.send(ev);
    }

    fn fail(&self, step: Step, error: String, branch: &str) -> Result<(), ()> {
        let resume = if self.committed {
            resume_command(step, branch, self.name)
        } else {
            None
        };
        self.send(ReleaseEvent::Failed {
            step,
            error,
            committed: self.committed,
            resume,
        });
        Err(())
    }

    /// Run one step: Running, then Ok (or Skipped when `f` says so), or
    /// Failed with its error.
    fn step(
        &mut self,
        step: Step,
        branch: &str,
        f: impl FnOnce(&mut Self) -> Result<StepState, String>,
    ) -> Result<(), ()> {
        self.send(ReleaseEvent::Step(step, StepState::Running));
        match f(self) {
            Ok(state) => {
                self.send(ReleaseEvent::Step(step, state));
                Ok(())
            }
            Err(e) => self.fail(step, e, branch),
        }
    }

    fn go(&mut self, cwd: &Path) -> Result<(), ()> {
        let mut plan = None;
        self.step(Step::Checks, "", |r| {
            plan = Some(checks(cwd, r.name)?);
            Ok(StepState::Ok)
        })?;
        let plan = plan.expect("checks succeeded");
        let (root, branch, name) = (&plan.root, plan.branch.as_str(), self.name);
        let version = name.trim_start_matches('v');

        self.step(Step::CommitCards, branch, |r| {
            git(root, &["add", "--", &plan.tasks])?;
            // `diff --cached --quiet` exits 1 when something is staged.
            if git(root, &["diff", "--cached", "--quiet"]).is_ok() {
                return Ok(StepState::Ok);
            }
            git(
                root,
                &["commit", "-q", "-m", &format!("chore(kanban): cut {name}")],
            )?;
            r.committed = true;
            Ok(StepState::Ok)
        })?;

        self.step(Step::Bump, branch, |r| {
            let Some(toml) = &plan.cargo else {
                return Ok(StepState::Skipped);
            };
            let (new_toml, pkg) = bump_cargo_toml(toml, version)?;
            let toml_path = root.join("Cargo.toml");
            write(&toml_path, &new_toml)?;
            let mut files = vec!["Cargo.toml"];
            let lock_path = root.join("Cargo.lock");
            if let Ok(lock) = std::fs::read_to_string(&lock_path)
                && let Some(new_lock) = bump_cargo_lock(&lock, &pkg, version)
            {
                write(&lock_path, &new_lock)?;
                files.push("Cargo.lock");
            }
            let mut args = vec!["add", "--"];
            args.extend(&files);
            git(root, &args)?;
            let msg = format!("chore(release): bump version to {version}");
            let mut args = vec!["commit", "-q", "-m", msg.as_str(), "--"];
            args.extend(&files);
            git(root, &args)?;
            r.committed = true;
            Ok(StepState::Ok)
        })?;

        self.step(Step::PushBranch, branch, |_| {
            git(root, &["push", "origin", branch])?;
            Ok(StepState::Ok)
        })?;
        self.step(Step::Tag, branch, |_| {
            git(root, &["tag", name])?;
            Ok(StepState::Ok)
        })?;
        self.step(Step::PushTag, branch, |_| {
            git(root, &["push", "origin", name])?;
            Ok(StepState::Ok)
        })?;
        self.send(ReleaseEvent::Done {
            actions_url: plan.actions_url.clone(),
        });
        Ok(())
    }
}

/// The one-line hand finish after a failure at `step`, once a local commit
/// exists. `None` where no single command finishes it (the bump itself
/// failed: the tree needs a look first).
fn resume_command(step: Step, branch: &str, name: &str) -> Option<String> {
    match step {
        Step::Checks | Step::CommitCards | Step::Bump => None,
        Step::PushBranch | Step::Tag => {
            Some(format!("git tag {name} && git push origin {branch} {name}"))
        }
        Step::PushTag => Some(format!("git push origin {name}")),
    }
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Every check, before any write (see the module doc for the list).
fn checks(cwd: &Path, name: &str) -> Result<Plan, String> {
    let new = parse_tag(name).ok_or_else(|| format!("{name} is not a vX.Y.Z version name"))?;
    let root = PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?);
    let url = git(&root, &["remote", "get-url", "origin"])
        .map_err(|e| format!("no origin remote: {e}"))?;
    let branch = git(&root, &["symbolic-ref", "--short", "HEAD"])
        .map_err(|_| "HEAD is detached; check out the release branch".to_string())?;
    let default = git(
        &root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|s| s.strip_prefix("origin/").map(str::to_string))
    .unwrap_or_else(|| "main".into());
    if branch != default {
        return Err(format!(
            "on branch {branch}; releases are cut from {default}"
        ));
    }
    // The card files, relative to the root (the project may sit in a
    // subdirectory of the repository).
    let prefix = git(cwd, &["rev-parse", "--show-prefix"])?;
    let tasks = format!("{prefix}.foreman/tasks");
    let status = git(&root, &["status", "--porcelain", "--untracked-files=all"])?;
    if let Some(path) = dirty_outside(&status, &format!("{tasks}/")) {
        return Err(format!(
            "uncommitted change outside .foreman/tasks: {path}; commit or stash it first"
        ));
    }
    git(&root, &["fetch", "-q", "origin"])?;
    let remote_ref = format!("refs/remotes/origin/{branch}");
    if git(&root, &["rev-parse", "-q", "--verify", &remote_ref]).is_ok() {
        let behind = git(
            &root,
            &["rev-list", "--count", &format!("HEAD..{remote_ref}")],
        )?;
        if behind.trim() != "0" {
            return Err(format!(
                "{branch} is {} commits behind origin; pull first",
                behind.trim()
            ));
        }
    }
    let tag_ref = format!("refs/tags/{name}");
    if git(&root, &["rev-parse", "-q", "--verify", &tag_ref]).is_ok() {
        return Err(format!("tag {name} already exists"));
    }
    if !git(&root, &["ls-remote", "--tags", "origin", &tag_ref])?.is_empty() {
        return Err(format!("tag {name} already exists on origin"));
    }
    let mut cargo = std::fs::read_to_string(root.join("Cargo.toml")).ok();
    let latest_tag = crate::kanban::latest_v_tag(&root).and_then(|t| parse_tag(&t));
    let current = match &cargo {
        Some(toml) => {
            let v = cargo_version(toml)
                .and_then(|v| parse_version(&v))
                .ok_or("cannot read the [package] version in Cargo.toml")?;
            if v == new {
                // Bumped by hand already (the old procedure's first step);
                // the tag is free (checked above), so skip our bump. It must
                // still beat the last release.
                cargo = None;
                latest_tag
            } else {
                Some(v)
            }
        }
        None => latest_tag,
    };
    if let Some(cur) = current
        && new <= cur
    {
        return Err(format!(
            "{name} is not newer than the current version {}.{}.{}",
            cur.0, cur.1, cur.2
        ));
    }
    Ok(Plan {
        root,
        branch,
        tasks,
        cargo,
        actions_url: actions_url(&url),
    })
}

/// The first porcelain path not under `tasks_prefix`, if any.
fn dirty_outside(porcelain: &str, tasks_prefix: &str) -> Option<String> {
    for line in porcelain.lines() {
        let Some(rest) = line.get(3..) else { continue };
        for path in rest.split(" -> ") {
            let path = path.trim_matches('"');
            if !path.starts_with(tasks_prefix) {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Run `git -C <cwd> <args>`: `Ok(stdout, end-trimmed)` on exit 0, else
/// `Err(stderr trimmed)`. Same console-hiding as `kanban::git`, but keeps
/// all of stderr (a push rejection's reason is not on its first line) and
/// never prompts for credentials (there is no terminal to answer).
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(cwd).args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        // Trim only the end: porcelain status lines start with a space.
        return Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if err.is_empty() {
        Err(format!(
            "git {} failed ({})",
            args.first().unwrap_or(&""),
            out.status
        ))
    } else {
        Err(err)
    }
}

/// `X.Y.Z` (a `-pre`/`+build` suffix is ignored) → numbers.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.');
    let mut n = || -> Option<u64> {
        let p = it.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let v = (n()?, n()?, n()?);
    it.next().is_none().then_some(v)
}

/// A release name: `v` + strict `X.Y.Z` digits, nothing else.
pub fn parse_tag(name: &str) -> Option<(u64, u64, u64)> {
    let rest = name.strip_prefix('v')?;
    if !rest.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    parse_version(rest)
}

/// The next patch after `v` (`X.Y.Z`), as a tag.
pub fn next_patch((a, b, c): (u64, u64, u64)) -> String {
    format!("v{a}.{b}.{}", c + 1)
}

/// The Cut field's prefill: `Cargo.toml`'s version in `cwd`'s repository
/// when it has no tag yet (bumped by hand, not released), else the patch
/// after it; without `Cargo.toml`, the patch after the newest `v*` tag;
/// else nothing.
pub fn prefill(cwd: &Path) -> Option<String> {
    let root = git(cwd, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|_| cwd.to_path_buf());
    let Ok(toml) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return parse_tag(&crate::kanban::latest_v_tag(&root)?).map(next_patch);
    };
    let v = parse_version(&cargo_version(&toml)?)?;
    let tag = format!("v{}.{}.{}", v.0, v.1, v.2);
    let tagged = git(
        &root,
        &["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")],
    )
    .is_ok();
    let latest = crate::kanban::latest_v_tag(&root).and_then(|t| parse_tag(&t));
    if !tagged && latest.is_none_or(|t| t < v) {
        return Some(tag);
    }
    // Past whichever is newer, so the default always clears the checks.
    Some(next_patch(latest.map_or(v, |t| t.max(v))))
}

/// Section header of a TOML line (`[package]`, `[[package]]`), if it is one.
fn header(line: &str) -> Option<&str> {
    let t = line.trim();
    t.starts_with('[').then_some(t)
}

/// `key = "value"` → value, when the line assigns `key`.
fn string_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.trim_start().strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    let rest = rest.strip_prefix('"')?;
    Some(&rest[..rest.find('"')?])
}

/// Replace the quoted value on a `key = "..."` line, keeping everything
/// around it (indent, spacing, comment, line ending).
fn replace_value(line: &str, new: &str) -> String {
    let open = line.find('"').expect("caller matched a quoted value");
    let close = open + 1 + line[open + 1..].find('"').expect("matched");
    format!("{}\"{new}\"{}", &line[..open], &line[close + 1..])
}

/// The `[package]` table's `(name, version)` lines, in `Cargo.toml` text.
fn package_fields(toml: &str) -> (Option<String>, Option<String>) {
    let (mut in_pkg, mut name, mut version) = (false, None, None);
    for line in toml.lines() {
        if let Some(h) = header(line) {
            in_pkg = h == "[package]";
            continue;
        }
        if in_pkg {
            if name.is_none() {
                name = string_value(line, "name").map(str::to_string);
            }
            if version.is_none() {
                version = string_value(line, "version").map(str::to_string);
            }
        }
    }
    (name, version)
}

/// `[package] version` in `Cargo.toml` text.
pub fn cargo_version(toml: &str) -> Option<String> {
    package_fields(toml).1
}

/// Rewrite `[package] version` to `version`. Returns the new text and the
/// package name (to find its entry in `Cargo.lock`).
pub fn bump_cargo_toml(toml: &str, version: &str) -> Result<(String, String), String> {
    let (Some(name), Some(_)) = package_fields(toml) else {
        return Err("Cargo.toml has no [package] name and version".into());
    };
    let (mut in_pkg, mut done) = (false, false);
    let mut out = String::with_capacity(toml.len());
    for line in toml.split_inclusive('\n') {
        if let Some(h) = header(line) {
            in_pkg = h == "[package]";
        } else if in_pkg && !done && string_value(line, "version").is_some() {
            out.push_str(&replace_value(line, version));
            done = true;
            continue;
        }
        out.push_str(line);
    }
    Ok((out, name))
}

/// Rewrite the `version` of the `[[package]]` entry named `pkg` in
/// `Cargo.lock` text. `None` when there is no such entry.
pub fn bump_cargo_lock(lock: &str, pkg: &str, version: &str) -> Option<String> {
    let (mut in_ours, mut done) = (false, false);
    let mut out = String::with_capacity(lock.len());
    for line in lock.split_inclusive('\n') {
        if header(line).is_some() {
            in_ours = false;
        } else if string_value(line, "name") == Some(pkg) {
            in_ours = true;
        } else if in_ours && !done && string_value(line, "version").is_some() {
            out.push_str(&replace_value(line, version));
            done = true;
            continue;
        }
        out.push_str(line);
    }
    done.then_some(out)
}

/// `https://github.com/<owner>/<repo>/actions` for a github.com remote URL
/// (https, `git@github.com:` or `ssh://` form), else `None`.
pub fn actions_url(remote: &str) -> Option<String> {
    let r = remote.trim();
    let path = r
        .strip_prefix("https://github.com/")
        .or_else(|| r.strip_prefix("http://github.com/"))
        .or_else(|| r.strip_prefix("git@github.com:"))
        .or_else(|| r.strip_prefix("ssh://git@github.com/"))
        .or_else(|| {
            // https://user@github.com/… (a token or name before the host)
            let rest = r.strip_prefix("https://")?;
            let (_, after) = rest.split_once('@')?;
            after.strip_prefix("github.com/")
        })?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repo}/actions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_strictly() {
        assert_eq!(parse_tag("v0.5.1"), Some((0, 5, 1)));
        assert_eq!(parse_tag("v10.20.30"), Some((10, 20, 30)));
        for bad in [
            "0.5.1",
            "v0.5",
            "v0.5.1.2",
            "v0.5.x",
            "v0.5.1-rc1",
            "v",
            "V0.5.1",
            "v.5.1",
            "v0..1",
            " v0.5.1",
        ] {
            assert_eq!(parse_tag(bad), None, "{bad}");
        }
        assert_eq!(parse_version("0.5.0"), Some((0, 5, 0)));
        assert_eq!(parse_version("1.2.3-beta.1"), Some((1, 2, 3)));
        assert!(parse_tag("v0.10.0") > parse_tag("v0.9.9"));
        assert!(parse_tag("v1.0.0") > parse_tag("v0.99.99"));
    }

    #[test]
    fn next_patch_bumps_the_last_number() {
        assert_eq!(next_patch((0, 5, 0)), "v0.5.1");
        assert_eq!(next_patch((1, 9, 9)), "v1.9.10");
    }

    const TOML: &str = "[package]\r\nname = \"foreman\"\r\nversion = \"0.5.0\"  # keep\r\nedition = \"2024\"\r\n\r\n[dependencies]\r\nfoo = { version = \"1.0\" }\r\nversion = \"9.9.9\"\r\n";

    #[test]
    fn cargo_toml_bump_touches_only_the_package_version() {
        assert_eq!(cargo_version(TOML).as_deref(), Some("0.5.0"));
        let (out, name) = bump_cargo_toml(TOML, "0.5.1").unwrap();
        assert_eq!(name, "foreman");
        assert_eq!(
            out,
            TOML.replace("\"0.5.0\"  # keep", "\"0.5.1\"  # keep"),
            "only [package] version changes; CRLF, comment and other tables kept"
        );
        assert!(bump_cargo_toml("[workspace]\nmembers = []\n", "1.0.0").is_err());
    }

    #[test]
    fn cargo_lock_bump_touches_only_the_named_package() {
        let lock = "version = 4\n\n[[package]]\nname = \"foo\"\nversion = \"0.5.0\"\n\n[[package]]\nname = \"foreman\"\nversion = \"0.5.0\"\ndependencies = [\n \"foo\",\n]\n";
        let out = bump_cargo_lock(lock, "foreman", "0.5.1").unwrap();
        assert_eq!(
            out,
            lock.replace(
                "name = \"foreman\"\nversion = \"0.5.0\"",
                "name = \"foreman\"\nversion = \"0.5.1\""
            )
        );
        assert_eq!(bump_cargo_lock(lock, "absent", "1.0.0"), None);
    }

    #[test]
    fn github_remotes_map_to_their_actions_page() {
        let want = Some("https://github.com/sniffle6/foreman/actions".to_string());
        for url in [
            "https://github.com/sniffle6/foreman.git",
            "https://github.com/sniffle6/foreman",
            "https://github.com/sniffle6/foreman/",
            "git@github.com:sniffle6/foreman.git",
            "ssh://git@github.com/sniffle6/foreman.git",
            "https://x-token@github.com/sniffle6/foreman.git",
        ] {
            assert_eq!(actions_url(url), want, "{url}");
        }
        for url in [
            "https://gitlab.com/a/b.git",
            "C:/tmp/origin.git",
            "/srv/git/x.git",
            "https://github.com/onlyowner",
        ] {
            assert_eq!(actions_url(url), None, "{url}");
        }
    }

    #[test]
    fn porcelain_outside_tasks_is_found() {
        let p = ".foreman/tasks/";
        assert_eq!(dirty_outside("", p), None);
        assert_eq!(
            dirty_outside(" M .foreman/tasks/a.json\n?? .foreman/tasks/b.json", p),
            None
        );
        assert_eq!(
            dirty_outside(" M .foreman/tasks/a.json\n M src/x.rs", p).as_deref(),
            Some("src/x.rs")
        );
        assert_eq!(
            dirty_outside("R  old.rs -> .foreman/tasks/a.json", p).as_deref(),
            Some("old.rs")
        );
    }

    #[test]
    fn resume_names_only_what_is_left() {
        assert_eq!(resume_command(Step::Bump, "main", "v1.0.1"), None);
        assert_eq!(
            resume_command(Step::PushBranch, "main", "v1.0.1").as_deref(),
            Some("git tag v1.0.1 && git push origin main v1.0.1")
        );
        assert_eq!(
            resume_command(Step::PushTag, "main", "v1.0.1").as_deref(),
            Some("git push origin v1.0.1")
        );
    }

    #[test]
    fn progress_folds_events_and_drops_skipped_steps() {
        let mut p = Progress::new("v1.0.1");
        assert!(!p.finished);
        p.apply(&ReleaseEvent::Step(Step::Checks, StepState::Running));
        assert_eq!(p.steps[0], (Step::Checks, Mark::Running));
        p.apply(&ReleaseEvent::Step(Step::Checks, StepState::Ok));
        p.apply(&ReleaseEvent::Step(Step::Bump, StepState::Skipped));
        assert!(!p.steps.iter().any(|(s, _)| *s == Step::Bump));
        p.apply(&ReleaseEvent::Failed {
            step: Step::PushTag,
            error: "denied".into(),
            committed: true,
            resume: Some("git push origin v1.0.1".into()),
        });
        assert!(p.finished && p.committed);
        assert_eq!(p.steps.last(), Some(&(Step::PushTag, Mark::Failed)));
        assert_eq!(p.error.as_deref(), Some("denied"));
    }

    // --- end to end against a local bare "origin" --------------------------

    /// Plain git in `dir`; panics with stderr on failure.
    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        bare: PathBuf,
        work: PathBuf,
    }

    /// A bare repo standing in for `origin` and a clone of it on `main`
    /// holding `Cargo.toml`/`Cargo.lock` at 0.5.0 (when `cargo`) and one
    /// card file, all pushed. Identity and signing are local config so the
    /// runner's own `git commit` works without the machine's config. `None`
    /// when git is missing.
    fn fixture(cargo: bool) -> Option<Fixture> {
        if !crate::kanban::git_available() {
            eprintln!("git not on PATH; skipping");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("origin.git");
        let work = tmp.path().join("work");
        sh(
            tmp.path(),
            &["init", "-q", "--bare", "-b", "main", "origin.git"],
        );
        sh(tmp.path(), &["clone", "-q", "origin.git", "work"]);
        for (k, v) in [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            sh(&work, &["config", k, v]);
        }
        sh(&work, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        if cargo {
            std::fs::write(
                work.join("Cargo.toml"),
                "[package]\nname = \"demo\"\nversion = \"0.5.0\"\n",
            )
            .unwrap();
            std::fs::write(
                work.join("Cargo.lock"),
                "version = 4\n\n[[package]]\nname = \"demo\"\nversion = \"0.5.0\"\n",
            )
            .unwrap();
        } else {
            std::fs::write(work.join("README"), "hi\n").unwrap();
        }
        std::fs::create_dir_all(work.join(".foreman/tasks")).unwrap();
        std::fs::write(work.join(".foreman/tasks/x.json"), "{}\n").unwrap();
        sh(&work, &["add", "."]);
        sh(&work, &["commit", "-q", "-m", "init"]);
        sh(&work, &["push", "-q", "-u", "origin", "main"]);
        sh(&work, &["remote", "set-head", "origin", "main"]);
        if cargo {
            // 0.5.0 is released: its tag exists, as in a real repo.
            sh(&work, &["tag", "v0.5.0"]);
            sh(&work, &["push", "-q", "origin", "v0.5.0"]);
        }
        Some(Fixture {
            _tmp: tmp,
            bare,
            work,
        })
    }

    fn release(dir: &Path, name: &str) -> Vec<ReleaseEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        run(dir, name, &tx);
        drop(tx);
        rx.into_iter().collect()
    }

    fn failure(events: &[ReleaseEvent]) -> Option<(Step, String, bool)> {
        events.iter().find_map(|e| match e {
            ReleaseEvent::Failed {
                step,
                error,
                committed,
                ..
            } => Some((*step, error.clone(), *committed)),
            _ => None,
        })
    }

    fn subjects(dir: &Path) -> Vec<String> {
        sh(dir, &["log", "--format=%s"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A card was stamped: the tasks dir has a change to commit.
    fn stamp(f: &Fixture) {
        std::fs::write(f.work.join(".foreman/tasks/x.json"), "{\"shipped\":1}\n").unwrap();
    }

    #[test]
    fn full_release_commits_bumps_pushes_and_tags() {
        let Some(f) = fixture(true) else { return };
        stamp(&f);
        let ev = release(&f.work, "v0.5.1");
        assert_eq!(failure(&ev), None, "{ev:?}");
        assert_eq!(ev.last(), Some(&ReleaseEvent::Done { actions_url: None }));
        assert_eq!(
            subjects(&f.work)[..3],
            [
                "chore(release): bump version to 0.5.1",
                "chore(kanban): cut v0.5.1",
                "init"
            ]
        );
        // The bump commit holds only the two Cargo files.
        let files = sh(&f.work, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(
            files.lines().collect::<Vec<_>>(),
            ["Cargo.lock", "Cargo.toml"]
        );
        let toml = std::fs::read_to_string(f.work.join("Cargo.toml")).unwrap();
        assert!(toml.contains("version = \"0.5.1\""), "{toml}");
        let lock = std::fs::read_to_string(f.work.join("Cargo.lock")).unwrap();
        assert!(lock.contains("version = \"0.5.1\""), "{lock}");
        // The remote has the branch tip and the tag.
        let head = sh(&f.work, &["rev-parse", "HEAD"]);
        assert_eq!(sh(&f.bare, &["rev-parse", "main"]), head);
        assert_eq!(sh(&f.bare, &["rev-parse", "v0.5.1^{commit}"]), head);
        assert_eq!(sh(&f.work, &["status", "--porcelain"]), "");
    }

    #[test]
    fn dirty_tree_outside_tasks_is_refused_before_any_commit() {
        let Some(f) = fixture(true) else { return };
        stamp(&f);
        std::fs::write(f.work.join("stray.txt"), "x").unwrap();
        let (step, err, committed) = failure(&release(&f.work, "v0.5.1")).unwrap();
        assert_eq!(step, Step::Checks);
        assert!(err.contains("stray.txt"), "{err}");
        assert!(!committed);
        assert_eq!(subjects(&f.work), ["init"]);
    }

    #[test]
    fn behind_origin_is_refused() {
        let Some(f) = fixture(true) else { return };
        // Someone else pushed: a second clone advances origin/main.
        let other = f.work.parent().unwrap().join("other");
        sh(
            f.work.parent().unwrap(),
            &["clone", "-q", "origin.git", "other"],
        );
        std::fs::write(other.join("n.txt"), "n").unwrap();
        sh(&other, &["add", "n.txt"]);
        sh(
            &other,
            &[
                "-c",
                "user.name=o",
                "-c",
                "user.email=o@o",
                "commit",
                "-q",
                "-m",
                "other",
            ],
        );
        sh(&other, &["push", "-q", "origin", "main"]);
        stamp(&f);
        let (step, err, committed) = failure(&release(&f.work, "v0.5.1")).unwrap();
        assert_eq!(step, Step::Checks);
        assert!(err.contains("behind"), "{err}");
        assert!(!committed);
        assert_eq!(subjects(&f.work), ["init"]);
    }

    #[test]
    fn taken_tags_are_refused_locally_and_on_origin() {
        let Some(f) = fixture(true) else { return };
        stamp(&f);
        sh(&f.work, &["tag", "v0.5.1"]);
        let (step, err, _) = failure(&release(&f.work, "v0.5.1")).unwrap();
        assert_eq!(step, Step::Checks);
        assert!(err.contains("already exists"), "{err}");
        // Only on origin: push it, then drop the local copy.
        sh(&f.work, &["push", "-q", "origin", "v0.5.1"]);
        sh(&f.work, &["tag", "-d", "v0.5.1"]);
        // The checks' fetch would auto-follow the tag back; stop it so the
        // remote probe is what refuses.
        sh(&f.work, &["config", "remote.origin.tagOpt", "--no-tags"]);
        let (step, err, _) = failure(&release(&f.work, "v0.5.1")).unwrap();
        assert_eq!(step, Step::Checks);
        assert!(err.contains("on origin"), "{err}");
        assert_eq!(subjects(&f.work), ["init"]);
    }

    #[test]
    fn bad_or_non_increasing_names_are_refused() {
        let Some(f) = fixture(true) else { return };
        stamp(&f);
        for name in ["0.5.1", "v0.5", "release-1", "v0.5.0", "v0.4.9"] {
            let (step, _, committed) = failure(&release(&f.work, name)).unwrap();
            assert_eq!(step, Step::Checks, "{name}");
            assert!(!committed);
        }
        assert_eq!(subjects(&f.work), ["init"]);
    }

    #[test]
    fn push_rejected_after_commit_reports_committed_and_keeps_it() {
        let Some(f) = fixture(true) else { return };
        let hook = f.bare.join("hooks").join("pre-receive");
        std::fs::write(&hook, "#!/bin/sh\necho no pushes today >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        stamp(&f);
        let ev = release(&f.work, "v0.5.1");
        let (step, err, committed) = failure(&ev).unwrap();
        assert_eq!(step, Step::PushBranch);
        assert!(committed);
        assert!(err.contains("no pushes today"), "{err}");
        let resume = ev.iter().find_map(|e| match e {
            ReleaseEvent::Failed { resume, .. } => resume.clone(),
            _ => None,
        });
        assert_eq!(
            resume.as_deref(),
            Some("git tag v0.5.1 && git push origin main v0.5.1")
        );
        // Nothing undone: both local commits stay, origin untouched.
        assert_eq!(subjects(&f.work).len(), 3);
        assert_eq!(subjects(&f.bare), ["init"]);
    }

    #[test]
    fn project_without_cargo_toml_tags_without_a_bump() {
        let Some(f) = fixture(false) else { return };
        stamp(&f);
        let ev = release(&f.work, "v1.0.0");
        assert_eq!(failure(&ev), None, "{ev:?}");
        assert!(ev.contains(&ReleaseEvent::Step(Step::Bump, StepState::Skipped)));
        assert_eq!(subjects(&f.work), ["chore(kanban): cut v1.0.0", "init"]);
        let head = sh(&f.work, &["rev-parse", "HEAD"]);
        assert_eq!(sh(&f.bare, &["rev-parse", "v1.0.0^{commit}"]), head);
        // With a tag in place, the next name must beat it.
        let (step, _, _) = failure(&release(&f.work, "v0.9.0")).unwrap();
        assert_eq!(step, Step::Checks);
    }

    /// Bump `Cargo.toml`/`Cargo.lock` to `version` by hand and commit it,
    /// the first step of the old manual procedure.
    fn bump_by_hand(f: &Fixture, version: &str) {
        let toml = std::fs::read_to_string(f.work.join("Cargo.toml")).unwrap();
        let (toml, pkg) = bump_cargo_toml(&toml, version).unwrap();
        std::fs::write(f.work.join("Cargo.toml"), toml).unwrap();
        let lock = std::fs::read_to_string(f.work.join("Cargo.lock")).unwrap();
        let lock = bump_cargo_lock(&lock, &pkg, version).unwrap();
        std::fs::write(f.work.join("Cargo.lock"), lock).unwrap();
        sh(&f.work, &["add", "Cargo.toml", "Cargo.lock"]);
        sh(&f.work, &["commit", "-q", "-m", "manual bump"]);
    }

    // Regression: bumping Cargo.toml by hand first (the old procedure's
    // step 1) made Cut refuse `v0.5.1` as "not newer than 0.5.1".
    #[test]
    fn a_hand_bumped_cargo_toml_releases_without_a_second_bump() {
        let Some(f) = fixture(true) else { return };
        bump_by_hand(&f, "0.5.1");
        assert_eq!(prefill(&f.work).as_deref(), Some("v0.5.1"), "untagged");
        stamp(&f);
        let ev = release(&f.work, "v0.5.1");
        assert_eq!(failure(&ev), None, "{ev:?}");
        assert!(ev.contains(&ReleaseEvent::Step(Step::Bump, StepState::Skipped)));
        assert_eq!(
            subjects(&f.work)[..3],
            ["chore(kanban): cut v0.5.1", "manual bump", "init"]
        );
        let head = sh(&f.work, &["rev-parse", "HEAD"]);
        assert_eq!(sh(&f.bare, &["rev-parse", "v0.5.1^{commit}"]), head);
        assert_eq!(prefill(&f.work).as_deref(), Some("v0.5.2"), "now tagged");
    }

    #[test]
    fn a_cargo_version_behind_the_latest_tag_is_still_refused() {
        let Some(f) = fixture(true) else { return };
        // Cargo.toml says 0.4.0 (untagged) but v0.5.0 is already out.
        bump_by_hand(&f, "0.4.0");
        assert_eq!(prefill(&f.work).as_deref(), Some("v0.5.1"), "past the tag");
        stamp(&f);
        let (step, err, committed) = failure(&release(&f.work, "v0.4.0")).unwrap();
        assert_eq!(step, Step::Checks);
        assert!(err.contains("not newer"), "{err}");
        assert!(!committed);
    }

    #[test]
    fn prefill_is_the_next_patch() {
        let Some(f) = fixture(true) else { return };
        assert_eq!(prefill(&f.work).as_deref(), Some("v0.5.1"));
        let Some(g) = fixture(false) else { return };
        assert_eq!(prefill(&g.work), None);
        sh(&g.work, &["tag", "v2.3.4"]);
        assert_eq!(prefill(&g.work).as_deref(), Some("v2.3.5"));
    }
}
