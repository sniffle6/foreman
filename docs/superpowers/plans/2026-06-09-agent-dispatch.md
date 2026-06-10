# Agent Dispatch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Any CLI inside a foreman terminal can run `foreman open -- <command...>` to spawn that command as a new, visible terminal window in its project (spec: `docs/epics/agent-dispatch-epic.md`).

**Architecture:** A named-pipe server thread (`\\.\pipe\foreman`) forwards one-line JSON requests over an `mpsc` channel that the egui App drains each frame; the desktop `WindowManager` resolves the target project and spawns a command PTY. The same exe doubles as the client (`foreman open ...` = thin pipe client, no GUI). Every PTY gets `FOREMAN*` env vars so dispatchers can self-target.

**Tech Stack:** Rust (edition 2024), egui/eframe 0.34, portable-pty 0.9 (ConPTY), serde/serde_json (already deps), `interprocess` 2 (new dep — named pipes without unsafe Win32).

**Build loop (every task):** kill the app first or linking fails:
```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
cargo test 2>&1 | Select-Object -Last 15
```

---

### Task 0: Preflight — protect the user's in-progress work

The working tree has the **user's own uncommitted changes** to `src/terminal.rs` and `src/wm.rs` (plus untracked `_mockup.html`). This plan modifies both files, and its commit steps stage them by name — which would sweep the user's edits into feature commits.

- [ ] **Step 1: Check state**

Run: `git status --short`
If `src/terminal.rs` or `src/wm.rs` show `M`: **STOP and ask the user** to commit or stash their changes first. Do not stash for them. `_mockup.html` and `docs/epics/agent-dispatch-epic.md` are untracked and harmless — leave them.

- [ ] **Step 2: Branch**

```bash
git checkout -b feature/agent-dispatch
```

---

### Task 1: Protocol types (`src/control.rs`)

**Files:**
- Create: `src/control.rs`
- Modify: `src/main.rs:1-5` (add `mod control;`)

- [ ] **Step 1: Create `src/control.rs` with types and a failing-by-absence test**

```rust
//! Agent dispatch control channel: a named pipe (`\\.\pipe\foreman`) any local
//! process can use to open a terminal inside the running foreman. See
//! docs/epics/agent-dispatch-epic.md.

/// Pipe name; `GenericNamespaced` maps it to `\\.\pipe\foreman` on Windows.
pub const PIPE: &str = "foreman";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpenRequest {
    pub cmd: String, // always "open" in v1
    #[serde(default)]
    pub project: Option<String>, // "p3"; None = focused project
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpenReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>, // "t4" — unique only within its project
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>, // "p1"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl OpenReply {
    pub fn err(msg: impl Into<String>) -> Self {
        OpenReply { ok: false, terminal: None, project: None, error: Some(msg.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_parses_with_optional_fields_missing() {
        let req: OpenRequest =
            serde_json::from_str(r#"{"cmd":"open","command":["claude","-p","fix tests"]}"#)
                .unwrap();
        assert_eq!(req.cmd, "open");
        assert_eq!(req.project, None);
        assert_eq!(req.command, vec!["claude", "-p", "fix tests"]);
    }

    #[test]
    fn reply_roundtrips_and_omits_none_fields() {
        let ok = OpenReply { ok: true, terminal: Some("t4".into()), project: Some("p1".into()), error: None };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(!s.contains("error"));
        assert_eq!(serde_json::from_str::<OpenReply>(&s).unwrap(), ok);
        assert_eq!(OpenReply::err("boom").error.as_deref(), Some("boom"));
    }
}
```

- [ ] **Step 2: Register the module** — in `src/main.rs`, the mod list at the top is:

```rust
mod dirpicker;
mod keymap;
mod settings;
mod terminal;
mod wm;
```

Add `mod control;` (alphabetical, after `mod dirpicker;` is fine: `mod control;` first).

- [ ] **Step 3: Run tests**

Run: `cargo test control:: 2>&1 | Select-Object -Last 8`
Expected: `test control::tests::request_parses_with_optional_fields_missing ... ok` and `reply_roundtrips_and_omits_none_fields ... ok` (a `dead_code` warning on PIPE is fine for now).

- [ ] **Step 4: Commit**

```bash
git add src/control.rs src/main.rs
git commit -m "Agent dispatch: control protocol types (OpenRequest/OpenReply)"
```

---

### Task 2: Client argument parsing

**Files:**
- Modify: `src/control.rs` (append)

- [ ] **Step 1: Write failing tests** (append inside `mod tests`)

```rust
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_full_flags() {
        let req = parse_open_args(
            &s(&["--project", "p2", "--title", "agent · t", "--cwd", "H:\\x", "--", "claude", "-p", "task"]),
            None,
        )
        .unwrap();
        assert_eq!(req.project.as_deref(), Some("p2"));
        assert_eq!(req.title.as_deref(), Some("agent · t"));
        assert_eq!(req.cwd.as_deref(), Some("H:\\x"));
        assert_eq!(req.command, vec!["claude", "-p", "task"]);
    }

    #[test]
    fn parse_defaults_project_from_env_value() {
        let req = parse_open_args(&s(&["--", "cmd.exe"]), Some("p7".into())).unwrap();
        assert_eq!(req.project.as_deref(), Some("p7"));
        // explicit flag beats the default
        let req = parse_open_args(&s(&["--project", "p1", "--", "cmd.exe"]), Some("p7".into())).unwrap();
        assert_eq!(req.project.as_deref(), Some("p1"));
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert!(parse_open_args(&s(&["--title"]), None).is_err()); // flag without value
        assert!(parse_open_args(&s(&["--", ""]), None).is_ok()); // empty word is server's problem
        assert!(parse_open_args(&s(&["--"]), None).is_err()); // no command
        assert!(parse_open_args(&s(&["claude"]), None).is_err()); // missing --
        assert!(parse_open_args(&s(&["--nope", "--", "x"]), None).is_err());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test control:: 2>&1 | Select-Object -Last 8`
Expected: compile error — `parse_open_args` not found.

- [ ] **Step 3: Implement** (in `src/control.rs`, above `mod tests`)

```rust
/// Parse `foreman open` args: `[--project P] [--title T] [--cwd D] -- <command...>`.
/// `default_project` is the dispatcher's own project (from FOREMAN_PROJECT_ID).
pub fn parse_open_args(
    args: &[String],
    default_project: Option<String>,
) -> Result<OpenRequest, String> {
    let mut project = default_project;
    let (mut title, mut cwd) = (None, None);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                let command = args[i + 1..].to_vec();
                if command.is_empty() {
                    return Err("no command after --".into());
                }
                return Ok(OpenRequest { cmd: "open".into(), project, cwd, title, command });
            }
            flag @ ("--project" | "--title" | "--cwd") => {
                let v = args.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?.clone();
                match flag {
                    "--project" => project = Some(v),
                    "--title" => title = Some(v),
                    _ => cwd = Some(v),
                }
                i += 2;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Err("missing -- <command...>".into())
}
```

- [ ] **Step 4: Run tests** — `cargo test control:: 2>&1 | Select-Object -Last 8` → all pass.

- [ ] **Step 5: Commit**

```bash
git add src/control.rs
git commit -m "Agent dispatch: parse 'foreman open' client arguments"
```

---

### Task 3: Pipe server + client request (loopback-tested)

**Files:**
- Modify: `Cargo.toml` (add dep), `src/control.rs` (append)

- [ ] **Step 1: Add the dependency** — in `Cargo.toml` `[dependencies]`:

```toml
interprocess = "2"
```

- [ ] **Step 2: Write the failing loopback test** (append inside `mod tests`)

```rust
    #[test]
    fn pipe_roundtrip() {
        // Unique name so parallel test runs / a live foreman don't collide.
        let pipe = format!("foreman-test-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx));
        // Fake GUI thread: answer the first request.
        std::thread::spawn(move || {
            let CtrlMsg::Open(req, reply) = rx.recv().unwrap();
            assert_eq!(req.command, vec!["cmd.exe", "/c", "echo hi"]);
            let _ = reply.send(OpenReply {
                ok: true,
                terminal: Some("t9".into()),
                project: Some("p1".into()),
                error: None,
            });
        });
        let req = OpenRequest {
            cmd: "open".into(),
            project: None,
            cwd: None,
            title: None,
            command: vec!["cmd.exe".into(), "/c".into(), "echo hi".into()],
        };
        // Retry while the listener binds (no sleep-and-hope).
        let mut reply = None;
        for _ in 0..100 {
            match request(&pipe, &req) {
                Ok(r) => {
                    reply = Some(r);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        let reply = reply.expect("no reply from pipe server");
        assert!(reply.ok);
        assert_eq!(reply.terminal.as_deref(), Some("t9"));
    }
```

- [ ] **Step 3: Run to verify failure** — `cargo test control:: 2>&1 | Select-Object -Last 8` → compile error: `serve`/`CtrlMsg`/`request` not found.

- [ ] **Step 4: Implement server + client** (in `src/control.rs`, above `mod tests`)

```rust
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, prelude::*};
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;

/// One control request, plus the channel the GUI thread answers on.
pub enum CtrlMsg {
    Open(OpenRequest, mpsc::Sender<OpenReply>),
}

/// Pipe server. Runs on a background thread for the GUI's whole lifetime; the
/// GUI drains `tx`'s receiver each frame. One JSON line in, one JSON line out,
/// per connection.
pub fn serve(pipe: &str, tx: mpsc::Sender<CtrlMsg>) {
    let Ok(name) = pipe.to_ns_name::<GenericNamespaced>() else { return };
    let listener = match ListenerOptions::new().name(name).create_sync() {
        Ok(l) => l,
        // Another foreman owns the pipe (or it's blocked): GUI still works,
        // dispatch is just unavailable in this instance.
        Err(e) => {
            eprintln!("control: pipe unavailable ({e}); agent dispatch disabled");
            return;
        }
    };
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let mut conn = BufReader::new(conn);
        let mut line = String::new();
        if conn.read_line(&mut line).is_err() {
            continue;
        }
        let reply = match serde_json::from_str::<OpenRequest>(&line) {
            Ok(req) => {
                let (rtx, rrx) = mpsc::channel();
                if tx.send(CtrlMsg::Open(req, rtx)).is_err() {
                    return; // GUI gone; stop serving
                }
                rrx.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap_or_else(|_| OpenReply::err("foreman did not respond"))
            }
            Err(e) => OpenReply::err(format!("bad request: {e}")),
        };
        let mut out = serde_json::to_string(&reply).unwrap_or_default();
        out.push('\n');
        let _ = conn.get_mut().write_all(out.as_bytes());
    }
}

/// Client side: send one request, wait for the one-line reply.
pub fn request(pipe: &str, req: &OpenRequest) -> std::io::Result<OpenReply> {
    let name = pipe.to_ns_name::<GenericNamespaced>()?;
    let conn = Stream::connect(name)?;
    let mut conn = BufReader::new(conn);
    let mut line = serde_json::to_string(req).map_err(std::io::Error::other)?;
    line.push('\n');
    conn.get_mut().write_all(line.as_bytes())?;
    let mut reply = String::new();
    conn.read_line(&mut reply)?;
    serde_json::from_str(&reply).map_err(std::io::Error::other)
}

/// `foreman open ...` entry point (no GUI). Returns the process exit code.
pub fn client_main(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("open") {
        eprintln!("usage: foreman open [--project P] [--title T] [--cwd D] -- <command...>");
        return 2;
    }
    let req = match parse_open_args(&args[1..], std::env::var("FOREMAN_PROJECT_ID").ok()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman open: {e}");
            return 2;
        }
    };
    match request(PIPE, &req) {
        Ok(r) if r.ok => {
            println!("{}", serde_json::to_string(&r).unwrap_or_default());
            0
        }
        Ok(r) => {
            eprintln!("foreman open: {}", r.error.unwrap_or_default());
            1
        }
        Err(e) => {
            eprintln!("foreman open: cannot reach foreman ({e}) — is it running?");
            1
        }
    }
}
```

NOTE: if `interprocess` 2.x API names drift from the above (`to_ns_name`, `create_sync`, `Stream::connect`), `cargo build` errors will say so — fix against https://docs.rs/interprocess/2 (`local_socket` module docs have this exact server/client example shape).

- [ ] **Step 5: Run tests** — `cargo test control:: 2>&1 | Select-Object -Last 8` → all pass, including `pipe_roundtrip`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/control.rs
git commit -m "Agent dispatch: named-pipe server + client over interprocess"
```

---

### Task 4: Command sessions in `src/terminal.rs` (spawn refactor, env, cmd/c retry, exit status)

**Files:**
- Modify: `src/terminal.rs:159-225` (`Session::spawn`), `src/terminal.rs:140-153` (`Session` fields)
- Caller fixed in this task: `src/wm.rs:471` (only `Session::spawn` call site)

- [ ] **Step 1: Write failing tests** (new `#[cfg(test)] mod tests` at the bottom of `src/terminal.rs` — the file has none yet)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_argv_runs_a_plain_exe() {
        let ctx = egui::Context::default();
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "exit 0".to_string()];
        let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
        // The child exits immediately; try_wait needs a beat on ConPTY.
        let mut code = None;
        for _ in 0..100 {
            if let Some(c) = s.exited() {
                code = Some(c);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(code, Some(0));
    }

    #[test]
    fn spawn_argv_falls_back_to_cmd_for_shims() {
        // npm-style shim: a .cmd file is not directly CreateProcess-able.
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("fake-agent.cmd");
        std::fs::write(&shim, "@echo shim ran\r\n@exit 0\r\n").unwrap();
        let ctx = egui::Context::default();
        let argv = vec![shim.to_string_lossy().to_string()];
        assert!(Session::spawn_argv(&argv, None, &[], ctx).is_ok());
    }

    #[test]
    fn shell_sessions_still_spawn_with_env() {
        let ctx = egui::Context::default();
        let env = [("FOREMAN".to_string(), "1".to_string())];
        assert!(Session::spawn(Shell::Cmd, None, &env, ctx).is_ok());
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test terminal:: 2>&1 | Select-Object -Last 8` → compile errors (`spawn_argv`, `exited` not found; `spawn` arity).

- [ ] **Step 3: Implement.** Three changes:

**(a) Fields** — in `struct Session` (line ~140): rename `_child` to `child` (it's used now) and add an exit cache:

```rust
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exit: Option<u32>,
```

**(b) Split `spawn` and add `spawn_argv`.** Replace the current `pub fn spawn` with:

```rust
    pub fn spawn(
        shell: Shell,
        cwd: Option<&Path>,
        env: &[(String, String)],
        ctx: egui::Context,
    ) -> std::io::Result<Session> {
        let mut cmd = CommandBuilder::new(shell.program());
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        Self::spawn_with(cmd, shell, ctx)
    }

    /// Spawn an explicit argv (an agent command, not a shell). npm shims like
    /// `claude` are `.cmd` files CreateProcess can't run directly — if the
    /// direct spawn fails, retry once through `cmd /c`.
    pub fn spawn_argv(
        argv: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
        ctx: egui::Context,
    ) -> std::io::Result<Session> {
        let build = |words: &[String]| {
            let mut c = CommandBuilder::new(&words[0]);
            for a in &words[1..] {
                c.arg(a);
            }
            if let Some(dir) = cwd {
                c.cwd(dir);
            }
            for (k, v) in env {
                c.env(k, v);
            }
            c
        };
        Self::spawn_with(build(argv), Shell::Cmd, ctx.clone()).or_else(|_| {
            let mut wrapped = vec!["cmd.exe".to_string(), "/c".to_string()];
            wrapped.extend_from_slice(argv);
            Self::spawn_with(build(&wrapped), Shell::Cmd, ctx)
        })
    }

    fn spawn_with(cmd: CommandBuilder, shell: Shell, ctx: egui::Context) -> std::io::Result<Session> {
        // ... the ENTIRE body of the old `spawn` from `let (cols, rows) = ...`
        // down, minus the CommandBuilder construction (use the `cmd` param in
        // `pair.slave.spawn_command(cmd)`). In the final struct literal use
        // `child, exit: None,` instead of `_child: child,`.
    }
```

**(c) Exit status accessor** (new method on `Session`, near `keepalive`):

```rust
    /// Exit code of the child process, once it has ended. Cached — `try_wait`
    /// is a cheap non-blocking poll until then.
    pub fn exited(&mut self) -> Option<u32> {
        if self.exit.is_none() {
            self.exit = self.child.try_wait().ok().flatten().map(|s| s.exit_code());
        }
        self.exit
    }
```

**(d) Fix the call site** — `src/wm.rs:471`:

```rust
        let s = Session::spawn(shell, self.cwd.as_deref(), &[], ctx.clone()).ok()?;
```

(`&[]` is temporary — Task 5 replaces it with real env injection. If `cargo build` reveals other `Session::spawn` call sites, give them `&[]` too.)

- [ ] **Step 4: Run tests** — `cargo test terminal:: 2>&1 | Select-Object -Last 10` → 3 pass. Then `cargo build` → clean (the old `shell` dead-code warning may remain; ignore).

- [ ] **Step 5: Commit**

```bash
git add src/terminal.rs src/wm.rs
git commit -m "Agent dispatch: command sessions — spawn_argv with cmd/c shim retry, exit status"
```

---

### Task 5: WindowManager plumbing (`src/wm.rs`)

**Files:**
- Modify: `src/wm.rs` — struct (~line 364), `new()` (~411), `add_terminal` (~470), `add_project` (~484), `show()` entry, plus new methods. Tests go in the existing `#[cfg(test)]` mod at the bottom (~line 2150).

- [ ] **Step 1: Write failing tests** (append inside wm's existing tests mod; look at neighbors like `len1_window_has_no_tab_bar_and_title_is_the_tab_title` for construction conventions)

```rust
    fn mgr_with_project(id_focused: bool) -> WindowManager {
        let mut m = WindowManager::new();
        let (id, rect) = m.next_slot(egui::vec2(100.0, 100.0));
        let mut child = WindowManager::new();
        child.tag = Some(format!("p{id}"));
        m.push_win(id, "proj".into(), rect, Content::Project(Box::new(child)));
        if !id_focused {
            m.focused = None;
        }
        m
    }

    #[test]
    fn resolve_project_by_id_and_focus() {
        let m = mgr_with_project(true);
        assert_eq!(m.resolve_project(Some("p1")), Ok(1));
        assert_eq!(m.resolve_project(None), Ok(1)); // focused project
        assert!(m.resolve_project(Some("p9")).is_err());
        assert!(m.resolve_project(Some("zzz")).is_err());
        let unfocused = mgr_with_project(false);
        assert!(unfocused.resolve_project(None).is_err());
    }

    #[test]
    fn term_env_carries_ids() {
        let mut child = WindowManager::new();
        child.tag = Some("p3".into());
        let env = child.term_env(7);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("FOREMAN").as_deref(), Some("1"));
        assert_eq!(get("FOREMAN_PROJECT_ID").as_deref(), Some("p3"));
        assert_eq!(get("FOREMAN_TERMINAL_ID").as_deref(), Some("t7"));
        assert!(get("FOREMAN_EXE").is_some());
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test wm:: 2>&1 | Select-Object -Last 8` → compile errors (`tag`, `resolve_project`, `term_env`).

- [ ] **Step 3: Implement.** Five additions:

**(a) Tag field** — in `struct WindowManager` (after `cwd`):

```rust
    /// Stable id string ("p3") when this manager is a project's child manager;
    /// env-injected into its terminals so dispatchers can self-target. None on
    /// the desktop.
    tag: Option<String>,
```

and `tag: None,` in `new()`.

**(b) Set it at project creation** — in `add_project` (~line 491), after `let mut child = WindowManager::new();`:

```rust
        child.tag = Some(format!("p{}", id));
```

**(c) Env assembly** (new method):

```rust
    /// Env injected into every PTY this manager spawns (spec: agent-dispatch).
    fn term_env(&self, term_id: WinId) -> Vec<(String, String)> {
        let mut v = vec![
            ("FOREMAN".to_string(), "1".to_string()),
            ("FOREMAN_TERMINAL_ID".to_string(), format!("t{term_id}")),
        ];
        if let Some(t) = &self.tag {
            v.push(("FOREMAN_PROJECT_ID".to_string(), t.clone()));
        }
        // The client needs to find this exe; PATH won't have target\debug.
        if let Ok(exe) = std::env::current_exe() {
            v.push(("FOREMAN_EXE".to_string(), exe.display().to_string()));
        }
        v
    }
```

and use it in `add_terminal` (replacing Task 4's `&[]`; `self.next` is the id `next_slot` will hand out):

```rust
        let env = self.term_env(self.next);
        let s = Session::spawn(shell, self.cwd.as_deref(), &env, ctx.clone()).ok()?;
```

**(d) Project resolution + open handling** (new methods on `WindowManager`):

```rust
    /// Resolve a control-request project spec ("p3"; None = focused project)
    /// to a desktop window id. Only checks the ACTIVE tab — after tab-merging
    /// projects, the swallowed project's old id is stale (documented gotcha).
    fn resolve_project(&self, spec: Option<&str>) -> Result<WinId, String> {
        let is_project = |w: &&Win| matches!(w.tabs[w.active].content, Content::Project(_));
        match spec {
            Some(s) => {
                let id: WinId = s
                    .strip_prefix('p')
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| format!("bad project id: {s}"))?;
                self.windows
                    .iter()
                    .filter(is_project)
                    .find(|w| w.id == id)
                    .map(|w| w.id)
                    .ok_or_else(|| format!("no such project: {s}"))
            }
            None => self
                .focused
                .and_then(|id| self.windows.iter().filter(is_project).find(|w| w.id == id))
                .map(|w| w.id)
                .ok_or_else(|| "no focused project (pass --project)".to_string()),
        }
    }

    /// Handle a control-channel open request (desktop manager only).
    pub fn handle_open(&mut self, req: crate::control::OpenRequest, ctx: &egui::Context) -> crate::control::OpenReply {
        use crate::control::OpenReply;
        if req.command.is_empty() || req.command[0].is_empty() {
            return OpenReply::err("empty command");
        }
        let pid = match self.resolve_project(req.project.as_deref()) {
            Ok(id) => id,
            Err(e) => return OpenReply::err(e),
        };
        let win = self.windows.iter_mut().find(|w| w.id == pid).expect("resolved");
        let Content::Project(child) = &mut win.tabs[win.active].content else {
            return OpenReply::err("not a project"); // unreachable after resolve
        };
        match child.add_terminal_cmd(
            &req.command,
            req.cwd.as_deref().map(std::path::Path::new),
            req.title.as_deref(),
            ctx,
        ) {
            Ok(tid) => OpenReply {
                ok: true,
                terminal: Some(format!("t{tid}")),
                project: Some(format!("p{pid}")),
                error: None,
            },
            Err(e) => OpenReply::err(format!("spawn failed: {e}")),
        }
    }

    /// Spawn an explicit command (agent dispatch) as a terminal in this manager.
    fn add_terminal_cmd(
        &mut self,
        argv: &[String],
        cwd: Option<&Path>,
        title: Option<&str>,
        ctx: &egui::Context,
    ) -> std::io::Result<WinId> {
        let env = self.term_env(self.next);
        let cwd = cwd.or(self.cwd.as_deref());
        let s = Session::spawn_argv(argv, cwd, &env, ctx.clone())?;
        let (id, rect) = self.next_slot(egui::vec2(580.0, 380.0));
        let title = title.map(str::to_string).unwrap_or_else(|| format!("agent · {}", argv[0]));
        self.push_win(id, title, rect, Content::Terminal(s));
        Ok(id)
    }
```

**(e) Exited-title refresh** (new method + one call). Method:

```rust
    /// Append an `exited (code)` marker to terminals whose process ended. Runs
    /// over every tab (not just visible ones) so background agents update too.
    fn refresh_exit_titles(&mut self) {
        for w in &mut self.windows {
            for t in &mut w.tabs {
                match &mut t.content {
                    Content::Terminal(s) => {
                        if let Some(code) = s.exited() {
                            if !t.title.contains("· exited") {
                                t.title.push_str(&format!("  ·  exited ({code})"));
                            }
                        }
                    }
                    Content::Project(wm) => wm.refresh_exit_titles(),
                }
            }
        }
    }
```

Call it once per frame from the TOP of `pub fn show(...)`, desktop-only (it recurses itself — calling it from every nested manager would double-walk):

```rust
        if self.desktop {
            self.refresh_exit_titles();
        }
```

- [ ] **Step 4: Run tests** — `cargo test 2>&1 | Select-Object -Last 10` → everything passes (wm, terminal, control).

- [ ] **Step 5: Commit**

```bash
git add src/wm.rs
git commit -m "Agent dispatch: project resolution, command terminals, env injection, exit titles"
```

---

### Task 6: Wire it up in `src/main.rs` + end-to-end verify

**Files:**
- Modify: `src/main.rs` — `main()` (~line 350), `App` struct (~11), `App::ui` (~304)

- [ ] **Step 1: App carries the control receiver.** Replace `App`'s `Default` impl with a constructor (struct gains one field):

```rust
struct App {
    desktop: WindowManager,
    started: bool,
    /// Is the hover-revealed OS title bar currently shown?
    chrome_open: bool,
    /// When the pointer entered the top reveal zone (for the dwell timer).
    chrome_hot_since: Option<f64>,
    /// Agent-dispatch requests from the control pipe thread.
    ctrl: std::sync::mpsc::Receiver<control::CtrlMsg>,
}
impl App {
    fn new(ctrl: std::sync::mpsc::Receiver<control::CtrlMsg>) -> Self {
        Self {
            desktop: WindowManager::new().as_desktop(),
            started: false,
            chrome_open: false,
            chrome_hot_since: None,
            ctrl,
        }
    }
}
```

- [ ] **Step 2: Split client from GUI in `main()` and start the server.** Current `main()` is at line ~350; make it:

```rust
fn main() -> eframe::Result {
    // Subcommand = thin pipe client (`foreman open ...`), no GUI.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        std::process::exit(control::client_main(&args[1..]));
    }
    install_panic_logger();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || control::serve(control::PIPE, tx));
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_decorations(false),
        ..Default::default()
    };
    eframe::run_native("Foreman", opts, Box::new(move |_cc| Ok(Box::new(App::new(rx)))))
}
```

- [ ] **Step 3: Drain requests each frame.** In `App::ui`, right after the `if !self.started { ... }` block and BEFORE `self.desktop.show(...)` (so a new terminal renders the same frame):

```rust
        while let Ok(control::CtrlMsg::Open(req, reply)) = self.ctrl.try_recv() {
            let res = self.desktop.handle_open(req, &ctx);
            let _ = reply.send(res);
        }
```

- [ ] **Step 4: Build + full test suite**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
cargo test 2>&1 | Select-Object -Last 10
```
Expected: clean build, all tests pass.

- [ ] **Step 5: End-to-end verify (no mouse hijack needed)**

```powershell
$p = Start-Process -FilePath ".\target\debug\foreman.exe" -PassThru
Start-Sleep -Seconds 6
# Dispatch from "outside" — no FOREMAN_PROJECT_ID, so it targets the focused project
.\target\debug\foreman.exe open --title "agent · smoke" -- cmd.exe /c "echo dispatched agent & timeout /t 2 >nul"
```
Expected stdout: `{"ok":true,"terminal":"t2","project":"p1"}` (ids may differ).
Then screenshot the window (script in `docs/HANDOFF.md` § 3) and `Read` it:
- a second terminal titled `agent · smoke` exists inside the project, showing `dispatched agent`;
- ~3s later, screenshot again: its title ends with `· exited (0)`.

Also verify the failure path: `.\target\debug\foreman.exe open --project p99 -- cmd.exe` → stderr `no such project: p99`, exit code 1 (`$LASTEXITCODE`).

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "Agent dispatch: wire pipe server + client mode into the app"
```

---

### Task 7: Claude skill + docs

**Files:**
- Create: `.claude/skills/foreman-dispatch/SKILL.md`
- Modify: `docs/epics/agent-dispatch-epic.md`

- [ ] **Step 1: Write the skill**

```markdown
---
name: foreman-dispatch
description: Dispatch a visible worker agent into a new foreman terminal. Use when the user asks to dispatch/spawn an agent (or run a task) in a new visible terminal while running inside foreman (the FOREMAN env var is set to 1).
---

# Dispatch a visible agent into foreman

Only available when running inside a foreman terminal — check `$env:FOREMAN`
is `1` first; if not, tell the user this needs to run inside foreman.

Dispatch (PowerShell):

    & $env:FOREMAN_EXE open --title "agent · <short-label>" -- claude -p "<full task prompt>"

- The worker appears as a new terminal in YOUR project (foreman reads
  `FOREMAN_PROJECT_ID` from your environment). Pass `--project pN` to target
  another project, `--cwd <dir>` to set its working directory.
- The reply JSON gives the new terminal's id — NOT the worker's results. This
  is fire-and-watch: the human supervises the worker's terminal. Do not poll
  for results; tell the user the agent is running and where.
- For a worker the user wants to steer interactively, drop `-p`:
  `-- claude "<task>"`.
- Nothing is Claude-specific: any CLI works after `--` (codex, plain
  commands, build scripts).
```

- [ ] **Step 2: Update the epic** (`docs/epics/agent-dispatch-epic.md`):
  - Status line → `Status: **built** (date)`.
  - Env list: add `FOREMAN_EXE` — full path to the foreman exe, injected so clients don't need PATH setup.
  - Gotchas: add "After tab-merging two projects, the swallowed project's old `pN` id is stale — resolution only sees window ids of the active tabs."
  - Key files: drop "(planned)".

- [ ] **Step 3: Live test with Claude** (the real acceptance test; needs the user or a running `claude`): inside a foreman terminal, run `claude` and ask it to "dispatch an agent to <trivial task>". Expected: it invokes the skill, a new `agent · ...` terminal appears in the same project. If the skill doesn't trigger, check the terminal actually has `FOREMAN=1` (`echo $env:FOREMAN`).

- [ ] **Step 4: Commit**

```bash
git add .claude/skills/foreman-dispatch/SKILL.md docs/epics/agent-dispatch-epic.md
git commit -m "Agent dispatch: Claude skill + epic doc update"
```

---

## Self-review notes

- Spec coverage: pipe+protocol (T1/T3), client mode (T2/T3/T6), env injection incl. kept `FOREMAN_TERMINAL_ID` (T5), direct-spawn-then-cmd/c retry (T4), exited-title (T4/T5), placement in dispatcher's project via focused/`pN` (T5), Claude-as-first-client (T7). Round-trip & teams-watcher correctly absent (out of scope per spec).
- `FOREMAN_EXE` is an addition beyond the spec's env list (clients can't find a non-PATH exe without it); T7 step 2 folds it back into the spec doc.
- Type threads: `OpenRequest`/`OpenReply`/`CtrlMsg` defined T1/T3, consumed T5 (`handle_open`) and T6 (drain) with matching names; `Session::spawn` arity change lands with its call-site fix in the same task (T4).
