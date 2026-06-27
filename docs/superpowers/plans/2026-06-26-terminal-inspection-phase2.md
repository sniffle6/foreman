# Terminal Inspection Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `foreman send` and `foreman snapshot` control-plane verbs (IMMEDIATE path only — no settle/wait/attrs/cursor/region yet) so an agent can drive input into a terminal and read back its rendered screen.

**Architecture:** Three-layer addition mirroring every existing verb: (1) new `Session` methods in `terminal.rs` expose `feed`, `term_mode`, and `snapshot_text` on the live session; (2) `control.rs` adds `SendRequest`/`SnapshotRequest` structs, `CtrlMsg::Send/Snapshot`, arg parsers, `*_main` entry points, and HELP text; (3) `wm.rs` adds `resolve_terminal`, `session_mut`, `send_dispatch`, `snapshot_dispatch` helpers plus arms in `handle_ctrl`. Tests are written first for every unit.

**Tech Stack:** Rust, alacritty_terminal, egui, serde_json, interprocess (existing deps). `src/inspect.rs` Phase 1 functions are already implemented and tested — reuse, do not reinvent.

## Global Constraints

- IMMEDIATE path only: `settle_ms` is parsed and stored in `SendRequest` but NOT honored in dispatch. Add a comment noting it's for a later phase.
- Do NOT change `OpenReply`'s existing fields. Snapshot text rides the existing `history: Option<Vec<String>>` field.
- Do NOT touch `pump()`, the `Listener`, `read_input`, or the DSR/ready latch.
- Preserve all existing tests green (no regressions).
- Follow existing code style: `#[serde(default, skip_serializing_if = "Option::is_none")]` on optionals, `skip_serializing_if = "Vec::is_empty"` on Vecs, exit codes 0/1/2.
- Self-target rule for `send`/`snapshot`: when `--terminal` is absent, fill `terminal` from `FOREMAN_TERMINAL_ID` and require `FOREMAN_PROJECT_ID` — same as `parse_close_args` with no explicit ids.
- Windows, PowerShell, GNU toolchain. Build with: `$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"` then `cargo build 2>&1 | Select-Object -Last 30`.
- Kill running app before build: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`.

---

### Task 1: `Session` methods in `src/terminal.rs`

**Files:**
- Modify: `H:\claude code\foreman\src\terminal.rs` (add three `pub` methods to `impl Session`, near `inject_input` around line 481)

**Interfaces:**
- Consumes: existing private `self.send(&[u8])` (line 555), existing `self.pump()` (line 489), existing `self.term.mode()` (returns `&alacritty_terminal::term::TermMode`), existing `crate::inspect::snapshot_text`
- Produces:
  - `Session::feed(&mut self, bytes: &[u8])` — raw PTY write
  - `Session::term_mode(&self) -> alacritty_terminal::term::TermMode`
  - `Session::snapshot_text(&mut self, region: Option<crate::inspect::Region>) -> Vec<String>`

There are no NEW tests in this task — the `inspect.rs` pure functions are already tested, and the wiring through `Session` will be exercised by the `wm.rs` handle_ctrl tests in Task 3. The three methods are thin one-liners; the risk is zero.

- [ ] **Step 1: Add the three `pub` methods to `Session`**

In `H:\claude code\foreman\src\terminal.rs`, after `inject_input` (around line 487), insert:

```rust
    /// Raw PTY write — bypasses bracketed-paste and the submit delay. Used by
    /// `foreman send` to deliver pre-encoded bytes (text + key sequences).
    pub fn feed(&mut self, bytes: &[u8]) {
        self.send(bytes);
    }

    /// The terminal's current mode flags — used by `foreman send` to encode
    /// named keys through the same path the live keyboard uses.
    pub fn term_mode(&self) -> alacritty_terminal::term::TermMode {
        *self.term.mode()
    }

    /// Pump pending PTY output into the grid, then return the rendered viewport
    /// as plain text rows (trailing spaces trimmed). Used by `foreman snapshot`.
    pub fn snapshot_text(&mut self, region: Option<crate::inspect::Region>) -> Vec<String> {
        self.pump();
        crate::inspect::snapshot_text(&self.term, region)
    }
```

- [ ] **Step 2: Build to confirm no errors**

```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
cargo build 2>&1 | Select-Object -Last 20
```

Expected: `Compiling foreman` then `Finished` with no errors.

- [ ] **Step 3: Run tests to confirm no regressions**

```powershell
cargo test 2>&1 | Select-Object -Last 20
```

Expected: all existing tests pass, 0 failures.

---

### Task 2: `control.rs` — request structs, CtrlMsg variants, parse, serve, main, HELP

**Files:**
- Modify: `H:\claude code\foreman\src\control.rs`

**Interfaces:**
- Consumes: existing `OpenReply`, `CtrlMsg`, `serve()`, `parse_close_args` pattern, `REPLY_TIMEOUT`, `PIPE`, `request()`, `report()`
- Produces:
  - `SendRequest { cmd, project, terminal, text, keys, settle_ms }` (serializable, parseable)
  - `SnapshotRequest { cmd, project, terminal }` (serializable, parseable)
  - `CtrlMsg::Send(SendRequest, mpsc::Sender<OpenReply>, std::time::Instant)`
  - `CtrlMsg::Snapshot(SnapshotRequest, mpsc::Sender<OpenReply>, std::time::Instant)`
  - `parse_send_args(args, default_project, self_terminal, self_project) -> Result<SendRequest, String>`
  - `parse_snapshot_args(args, default_project, self_terminal) -> Result<SnapshotRequest, String>`
  - `HELP_SEND`, `HELP_SNAPSHOT` constants

This is a large task. Write every test FIRST, run cargo test to see them all fail, then implement.

- [ ] **Step 1: Write the failing tests in `control.rs`**

Add the following test functions inside the existing `#[cfg(test)] mod tests` block at the bottom of `src/control.rs`, BEFORE the closing `}` of the `tests` module:

```rust
    // ---- send / snapshot structs wire compatibility --------------------------

    #[test]
    fn send_request_omits_none_and_empty_fields() {
        let req = SendRequest {
            cmd: "send".into(),
            project: None,
            terminal: Some("t3".into()),
            text: Some("ls\r".into()),
            keys: vec![],
            settle_ms: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"project\""), "{json}");
        assert!(!json.contains("\"keys\""), "{json}"); // empty vec must vanish
        assert!(!json.contains("\"settle_ms\""), "{json}");
        let back: SendRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn send_request_with_keys_roundtrips() {
        let req = SendRequest {
            cmd: "send".into(),
            project: Some("p1".into()),
            terminal: Some("t3".into()),
            text: None,
            keys: vec!["Ctrl+C".into(), "Enter".into()],
            settle_ms: Some(0),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""keys":["Ctrl+C","Enter"]"#), "{json}");
        assert!(json.contains(r#""settle_ms":0"#), "{json}");
        let back: SendRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn snapshot_request_omits_none_fields() {
        let req = SnapshotRequest {
            cmd: "snapshot".into(),
            project: None,
            terminal: Some("t3".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"project\""), "{json}");
        let back: SnapshotRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    // ---- parse_send_args -----------------------------------------------------

    #[test]
    fn parse_send_args_text_only() {
        let req = parse_send_args(
            &s(&["--project", "p1", "--terminal", "t3", "--text", "hello\r"]),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(req.project.as_deref(), Some("p1"));
        assert_eq!(req.terminal.as_deref(), Some("t3"));
        assert_eq!(req.text.as_deref(), Some("hello\r"));
        assert!(req.keys.is_empty());
        assert_eq!(req.settle_ms, None);
    }

    #[test]
    fn parse_send_args_keys_split_on_whitespace() {
        let req = parse_send_args(
            &s(&[
                "--project",
                "p1",
                "--terminal",
                "t3",
                "--keys",
                "Ctrl+C Enter",
            ]),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(req.keys, vec!["Ctrl+C", "Enter"]);
    }

    #[test]
    fn parse_send_args_repeated_keys_appends() {
        let req = parse_send_args(
            &s(&[
                "--project",
                "p1",
                "--terminal",
                "t3",
                "--keys",
                "Ctrl+C",
                "--keys",
                "Enter",
                "--text",
                "hi",
            ]),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(req.keys, vec!["Ctrl+C", "Enter"]);
        assert_eq!(req.text.as_deref(), Some("hi"));
    }

    #[test]
    fn parse_send_args_self_target_from_env() {
        let req = parse_send_args(
            &s(&["--text", "x"]),
            Some("p1".into()),
            Some("t4".into()),
            Some("p1".into()),
        )
        .unwrap();
        assert_eq!(req.terminal.as_deref(), Some("t4"));
        assert_eq!(req.project.as_deref(), Some("p1"));
    }

    #[test]
    fn parse_send_args_self_target_requires_both_env_vars() {
        // missing self_terminal
        let e = parse_send_args(&s(&["--text", "x"]), Some("p1".into()), None, Some("p1".into()))
            .unwrap_err();
        assert!(e.contains("FOREMAN_TERMINAL_ID"), "{e}");
        // missing self_project
        let e =
            parse_send_args(&s(&["--text", "x"]), Some("p1".into()), Some("t4".into()), None)
                .unwrap_err();
        assert!(e.contains("FOREMAN_PROJECT_ID"), "{e}");
    }

    #[test]
    fn parse_send_args_requires_text_or_keys() {
        let e = parse_send_args(
            &s(&["--project", "p1", "--terminal", "t3"]),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(e.contains("nothing to send"), "{e}");
    }

    #[test]
    fn parse_send_args_settle_ms_is_parsed() {
        let req = parse_send_args(
            &s(&["--project", "p1", "--terminal", "t3", "--text", "x", "--settle-ms", "500"]),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(req.settle_ms, Some(500));
    }

    #[test]
    fn parse_send_args_rejects_bad_flags() {
        // unknown flag
        let e = parse_send_args(
            &s(&["--project", "p1", "--terminal", "t3", "--nope", "--text", "x"]),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(e.contains("--nope"), "{e}");
        // flag without value
        assert!(parse_send_args(&s(&["--terminal"]), None, None, None).is_err());
        // bad settle-ms
        assert!(
            parse_send_args(
                &s(&["--project", "p1", "--terminal", "t3", "--text", "x", "--settle-ms", "abc"]),
                None,
                None,
                None
            )
            .is_err()
        );
    }

    // ---- parse_snapshot_args -------------------------------------------------

    #[test]
    fn parse_snapshot_args_explicit_terminal() {
        let req =
            parse_snapshot_args(&s(&["--project", "p1", "--terminal", "t3"]), None, None).unwrap();
        assert_eq!(req.project.as_deref(), Some("p1"));
        assert_eq!(req.terminal.as_deref(), Some("t3"));
    }

    #[test]
    fn parse_snapshot_args_self_target() {
        let req = parse_snapshot_args(&s(&[]), Some("p1".into()), Some("t4".into())).unwrap();
        assert_eq!(req.terminal.as_deref(), Some("t4"));
        assert_eq!(req.project.as_deref(), Some("p1"));
    }

    #[test]
    fn parse_snapshot_args_self_target_requires_project() {
        let e = parse_snapshot_args(&s(&[]), None, Some("t4".into())).unwrap_err();
        assert!(e.contains("FOREMAN_PROJECT_ID"), "{e}");
    }

    #[test]
    fn parse_snapshot_args_requires_terminal() {
        // no terminal flag and no self-target env
        let e = parse_snapshot_args(&s(&["--project", "p1"]), None, None).unwrap_err();
        assert!(e.contains("terminal"), "{e}");
    }

    // ---- pipe roundtrips -----------------------------------------------------

    #[test]
    fn send_pipe_roundtrip() {
        let pipe = format!("foreman-test-send-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Send(req, reply, _) => {
                    assert_eq!(req.text.as_deref(), Some("hello"));
                    assert_eq!(req.terminal.as_deref(), Some("t3"));
                    let _ = reply.send(OpenReply {
                        ok: true,
                        terminal: None,
                        project: None,
                        error: None,
                        history: None,
                        seq: None,
                    });
                }
                _ => panic!("expected CtrlMsg::Send"),
            }
        });
        let req = SendRequest {
            cmd: "send".into(),
            project: Some("p1".into()),
            terminal: Some("t3".into()),
            text: Some("hello".into()),
            keys: vec![],
            settle_ms: None,
        };
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
        assert!(reply.expect("no reply").ok);
    }

    #[test]
    fn snapshot_pipe_roundtrip() {
        let pipe = format!("foreman-test-snap-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Snapshot(req, reply, _) => {
                    assert_eq!(req.terminal.as_deref(), Some("t3"));
                    let _ = reply.send(OpenReply {
                        ok: true,
                        terminal: None,
                        project: None,
                        error: None,
                        history: Some(vec!["line one".into(), "line two".into()]),
                        seq: None,
                    });
                }
                _ => panic!("expected CtrlMsg::Snapshot"),
            }
        });
        let req = SnapshotRequest {
            cmd: "snapshot".into(),
            project: Some("p1".into()),
            terminal: Some("t3".into()),
        };
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
        let r = reply.expect("no reply");
        assert!(r.ok);
        assert_eq!(
            r.history.as_deref(),
            Some(&["line one".to_string(), "line two".to_string()][..])
        );
    }
```

- [ ] **Step 2: Run tests to confirm they all fail (compile error is acceptable)**

```powershell
cargo test 2>&1 | Select-Object -Last 30
```

Expected: compilation failure because `SendRequest`, `SnapshotRequest`, `CtrlMsg::Send`, `CtrlMsg::Snapshot`, `parse_send_args`, `parse_snapshot_args` do not yet exist. Good — the tests are driving the implementation.

- [ ] **Step 3: Add `SendRequest` and `SnapshotRequest` structs to `control.rs`**

After the `CloseRequest` struct definition (around line 121), add:

```rust
/// Drive raw input into a terminal. `text` is written verbatim (UTF-8);
/// `keys` are named key presses encoded through `inspect::parse_keys` with
/// the session's live `TermMode`. Text first, then keys. `settle_ms` is
/// parsed and stored but not yet honored (settle is the next phase).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SendRequest {
    pub cmd: String, // always "send"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_ms: Option<u64>,
}

/// Read the rendered viewport of a terminal as plain text rows.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotRequest {
    pub cmd: String, // always "snapshot"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
}
```

- [ ] **Step 4: Add `CtrlMsg::Send` and `CtrlMsg::Snapshot` variants**

In the `CtrlMsg` enum (around line 174), add after `Close`:

```rust
    Send(SendRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Snapshot(SnapshotRequest, mpsc::Sender<OpenReply>, std::time::Instant),
```

- [ ] **Step 5: Add `"send"` and `"snapshot"` arms to `serve()`**

In the `match v.cmd.as_str()` block inside `serve()` (around line 218), add before the `other =>` arm:

```rust
                "send" => serde_json::from_str::<SendRequest>(&line)
                    .map(|r| CtrlMsg::Send(r, rtx, now))
                    .map_err(|e| format!("bad request: {e}")),
                "snapshot" => serde_json::from_str::<SnapshotRequest>(&line)
                    .map(|r| CtrlMsg::Snapshot(r, rtx, now))
                    .map_err(|e| format!("bad request: {e}")),
```

- [ ] **Step 6: Add `parse_send_args` function**

After `parse_close_args` (around line 458), add:

```rust
/// Parse `foreman send` args: `[--project P] [--terminal T] [--text TXT]
/// [--keys "K K …"]... [--settle-ms N]`. `--keys` splits its value on
/// whitespace; repeatable `--keys` appends. When `--terminal` is absent,
/// fills from `self_terminal` (FOREMAN_TERMINAL_ID) and requires
/// `self_project` (FOREMAN_PROJECT_ID) — same self-target rule as `close`.
/// Requires at least one of `--text` or `--keys`.
/// NOTE: `settle_ms` is parsed but not yet honored (settle is the next phase).
pub fn parse_send_args(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
    self_project: Option<String>,
) -> Result<SendRequest, String> {
    let mut project = default_project;
    let mut terminal: Option<String> = None;
    let mut text: Option<String> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut settle_ms: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            "--terminal" => {
                terminal = Some(args.get(i + 1).ok_or("--terminal needs a value")?.clone());
                i += 2;
            }
            "--text" => {
                text = Some(args.get(i + 1).ok_or("--text needs a value")?.clone());
                i += 2;
            }
            "--keys" => {
                let v = args.get(i + 1).ok_or("--keys needs a value")?;
                keys.extend(v.split_whitespace().map(str::to_string));
                i += 2;
            }
            "--settle-ms" => {
                let v = args.get(i + 1).ok_or("--settle-ms needs a value")?;
                settle_ms = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("--settle-ms needs a number, got: {v}"))?,
                );
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    if terminal.is_none() {
        // self-target: both env vars required
        let me = self_terminal
            .ok_or("not inside a foreman terminal (FOREMAN_TERMINAL_ID unset)")?;
        let proj = self_project.ok_or(
            "cannot resolve your own pane without FOREMAN_PROJECT_ID (terminal ids are only unique within a project)",
        )?;
        terminal = Some(me);
        if project.is_none() {
            project = Some(proj);
        }
    }
    if text.is_none() && keys.is_empty() {
        return Err("nothing to send: give --text and/or --keys".into());
    }
    Ok(SendRequest {
        cmd: "send".into(),
        project,
        terminal,
        text,
        keys,
        settle_ms,
    })
}
```

- [ ] **Step 7: Add `parse_snapshot_args` function**

After `parse_send_args`, add:

```rust
/// Parse `foreman snapshot` args: `[--project P] [--terminal T]`. When
/// `--terminal` is absent, fills from `self_terminal` (FOREMAN_TERMINAL_ID)
/// and requires `self_project` (FOREMAN_PROJECT_ID).
pub fn parse_snapshot_args(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
) -> Result<SnapshotRequest, String> {
    let mut project = default_project;
    let mut terminal: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            "--terminal" => {
                terminal = Some(args.get(i + 1).ok_or("--terminal needs a value")?.clone());
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    if terminal.is_none() {
        let me = self_terminal
            .ok_or("not inside a foreman terminal (FOREMAN_TERMINAL_ID unset)")?;
        let proj = project.ok_or(
            "cannot resolve your own pane without FOREMAN_PROJECT_ID (terminal ids are only unique within a project)",
        )?;
        terminal = Some(me);
        project = Some(proj);
    }
    let terminal =
        terminal.ok_or("--terminal is required (or run inside a foreman terminal)")?;
    Ok(SnapshotRequest {
        cmd: "snapshot".into(),
        project,
        terminal: Some(terminal),
    })
}
```

- [ ] **Step 8: Add HELP constants**

After `HELP_CLOSE` (around line 536), add:

```rust
const HELP_SEND: &str = "\
foreman send [--project P] [--terminal T] [--text TXT] [--keys \"K K …\"] [--settle-ms N]

Write input to terminal T (default: your own). --text is raw UTF-8 written
verbatim (\\r = Enter). --keys is a space-separated sequence of named key
presses encoded with the session's live TermMode. --text and --keys are
additive: text first, then keys. --settle-ms N (not yet honored; settle is
the next phase). Reply: {\"ok\":true} or {\"ok\":false,\"error\":\"...\"}.
Key names: F1..F12, Up Down Left Right, Home End PageUp PageDown Insert
Delete, Enter Tab Esc Backspace Space, single letters; Ctrl+/Alt+/Shift+
prefixes combinable. Unknown name → exit 2.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";

const HELP_SNAPSHOT: &str = "\
foreman snapshot [--project P] [--terminal T]

Read terminal T's rendered viewport as plain text (default: your own).
One string per visible row, trailing spaces trimmed, printed line per line.
Reply rides the same history field as status. A snapshot of a settled
terminal (after foreman send) gives you the current screen state.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";
```

- [ ] **Step 9: Update `HELP` top-level text and `client_main`**

In `HELP`, add two lines after `foreman close`:

```
  foreman send [flags] --text TXT / --keys \"K...\"  drive input into a terminal
  foreman snapshot [--project P] [--terminal T]       read the rendered viewport
```

In `client_main`, add two arms before `Some("help" | "--help" | "-h")`:

```rust
        Some("send") => send_main(&args[1..]),
        Some("snapshot") => snapshot_main(&args[1..]),
```

In the `_` fallback `eprintln!` block, add two lines:

```
       foreman send [--project P] [--terminal T] --text TXT [--keys \"K\"] [--settle-ms N]
       foreman snapshot [--project P] [--terminal T]
```

- [ ] **Step 10: Add `send_main` and `snapshot_main` functions**

After `close_main` (around line 629), add:

```rust
fn send_main(args: &[String]) -> i32 {
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_SEND}");
        return 0;
    }
    let req = match parse_send_args(
        args,
        std::env::var("FOREMAN_PROJECT_ID").ok(),
        std::env::var("FOREMAN_TERMINAL_ID").ok(),
        std::env::var("FOREMAN_PROJECT_ID").ok(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman send: {e}");
            return 2;
        }
    };
    report("foreman send", request(PIPE, &req))
}

fn snapshot_main(args: &[String]) -> i32 {
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_SNAPSHOT}");
        return 0;
    }
    let req = match parse_snapshot_args(
        args,
        std::env::var("FOREMAN_PROJECT_ID").ok(),
        std::env::var("FOREMAN_TERMINAL_ID").ok(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman snapshot: {e}");
            return 2;
        }
    };
    report("foreman snapshot", request(PIPE, &req))
}
```

- [ ] **Step 11: Update `help_prints_and_exits_zero_everywhere` test**

The existing test in `tests` already covers `open --help`, `chat --help`, etc. Add `send` and `snapshot` to it:

```rust
        assert_eq!(client_main(&s(&["send", "--help"])), 0);
        assert_eq!(client_main(&s(&["send", "-h"])), 0);
        assert_eq!(client_main(&s(&["snapshot", "--help"])), 0);
        assert_eq!(client_main(&s(&["snapshot", "-h"])), 0);
```

- [ ] **Step 12: Build and run tests**

```powershell
cargo build 2>&1 | Select-Object -Last 20
cargo test 2>&1 | Select-Object -Last 30
```

Expected: all tests pass, including the new ones added in Step 1.

---

### Task 3: `wm.rs` — `resolve_terminal`, `session_mut`, `send_dispatch`, `snapshot_dispatch`, `handle_ctrl` arms, tests

**Files:**
- Modify: `H:\claude code\foreman\src\wm.rs`

**Interfaces:**
- Consumes: `crate::control::{SendRequest, SnapshotRequest, CtrlMsg, OpenReply, REPLY_TIMEOUT}`, `crate::inspect::parse_keys`, `Session::feed`, `Session::term_mode`, `Session::snapshot_text`, existing `resolve_project`, existing `term_id`, existing `close_dispatch` pattern, `chat_fixture`, `pause_argv`
- Produces:
  - `WindowManager::resolve_terminal(project: Option<&str>, terminal: &str) -> Result<(WinId, WinId), String>` — resolves project+terminal to `(pid, tid)`
  - `WindowManager::session_mut(pid, tid) -> Result<&mut Session, String>` — borrow of the active terminal's session
  - `WindowManager::send_dispatch(req: &SendRequest) -> Result<(), String>`
  - `WindowManager::snapshot_dispatch(req: &SnapshotRequest) -> Result<Vec<String>, String>`
  - `handle_ctrl` arms for `CtrlMsg::Send` and `CtrlMsg::Snapshot`

- [ ] **Step 1: Write the failing `handle_ctrl` tests for Send and Snapshot**

Add the following inside the `#[cfg(test)] mod tests` block in `wm.rs`, after the close tests (around line 4900), BEFORE the closing `}`:

```rust
    // --- send / snapshot verbs ---

    fn send_msg(
        project: Option<&str>,
        terminal: &str,
        text: &str,
        sent: std::time::Instant,
    ) -> (
        crate::control::CtrlMsg,
        std::sync::mpsc::Receiver<crate::control::OpenReply>,
    ) {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let req = crate::control::SendRequest {
            cmd: "send".into(),
            project: project.map(str::to_string),
            terminal: Some(terminal.to_string()),
            text: Some(text.to_string()),
            keys: vec![],
            settle_ms: None,
        };
        (crate::control::CtrlMsg::Send(req, rtx, sent), rrx)
    }

    fn snapshot_msg(
        project: Option<&str>,
        terminal: &str,
        sent: std::time::Instant,
    ) -> (
        crate::control::CtrlMsg,
        std::sync::mpsc::Receiver<crate::control::OpenReply>,
    ) {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let req = crate::control::SnapshotRequest {
            cmd: "snapshot".into(),
            project: project.map(str::to_string),
            terminal: Some(terminal.to_string()),
        };
        (crate::control::CtrlMsg::Snapshot(req, rtx, sent), rrx)
    }

    #[test]
    fn send_replies_ok_for_valid_terminal() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let (msg, rrx) = send_msg(Some("p1"), &ta, "hello", std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no send reply");
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.history, None); // send does not return a snapshot
    }

    #[test]
    fn send_unknown_terminal_errors() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        let (msg, rrx) = send_msg(Some("p1"), "t99", "x", std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no send reply");
        assert!(!r.ok);
        assert!(
            r.error.as_deref().unwrap_or("").contains("t99"),
            "{:?}",
            r.error
        );
    }

    #[test]
    fn stale_send_is_dropped() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let stale = std::time::Instant::now()
            - (crate::control::REPLY_TIMEOUT + std::time::Duration::from_secs(1));
        let (msg, rrx) = send_msg(Some("p1"), &ta, "x", stale);
        d.handle_ctrl(msg, &ctx);
        assert!(
            rrx.try_recv().is_err(),
            "stale send must be dropped unanswered"
        );
    }

    #[test]
    fn snapshot_replies_history_some_and_nonempty() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let (msg, rrx) = snapshot_msg(Some("p1"), &ta, std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no snapshot reply");
        assert!(r.ok, "{:?}", r.error);
        // snapshot always returns Some(lines) — even an idle terminal has rows
        assert!(
            r.history.is_some(),
            "snapshot must populate history field"
        );
        assert!(
            !r.history.as_ref().unwrap().is_empty(),
            "snapshot rows must be non-empty"
        );
    }

    #[test]
    fn snapshot_unknown_terminal_errors() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        let (msg, rrx) = snapshot_msg(Some("p1"), "t99", std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no snapshot reply");
        assert!(!r.ok);
        assert!(
            r.error.as_deref().unwrap_or("").contains("t99"),
            "{:?}",
            r.error
        );
    }

    #[test]
    fn stale_snapshot_is_dropped() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let stale = std::time::Instant::now()
            - (crate::control::REPLY_TIMEOUT + std::time::Duration::from_secs(1));
        let (msg, rrx) = snapshot_msg(Some("p1"), &ta, stale);
        d.handle_ctrl(msg, &ctx);
        assert!(
            rrx.try_recv().is_err(),
            "stale snapshot must be dropped unanswered"
        );
    }
```

- [ ] **Step 2: Run to confirm new tests fail**

```powershell
cargo test 2>&1 | Select-Object -Last 20
```

Expected: `CtrlMsg::Send` and `CtrlMsg::Snapshot` don't exist in the match — likely a compile error listing the missing arms. This confirms the tests drive the implementation.

- [ ] **Step 3: Add `resolve_terminal` helper to `wm.rs`**

After `close_dispatch` (around line 1083), add:

```rust
    /// Resolve a `(project, terminal)` pair to their `WinId`s.
    /// `project` uses the existing `resolve_project` logic; `terminal` is
    /// validated to exist in that project's child manager and to have at
    /// least one `Content::Terminal` tab.
    fn resolve_terminal(
        &self,
        project: Option<&str>,
        terminal: &str,
    ) -> Result<(WinId, WinId), String> {
        let pid = self.resolve_project(project)?;
        let tid = term_id(terminal)?;
        let win = self.windows.iter().find(|w| w.id == pid).expect("resolved");
        let Content::Project(child) = &win.tabs[win.active].content else {
            return Err("not a project".into()); // unreachable after resolve
        };
        let tw = child
            .windows
            .iter()
            .find(|w| w.id == tid)
            .ok_or_else(|| format!("no such terminal: {terminal}"))?;
        if !tw
            .tabs
            .iter()
            .any(|t| matches!(t.content, Content::Terminal(_)))
        {
            return Err(format!("not a terminal: {terminal}"));
        }
        Ok((pid, tid))
    }
```

- [ ] **Step 4: Add `session_mut` helper to `wm.rs`**

After `resolve_terminal`, add:

```rust
    /// Get a mutable reference to the `Session` for the given (pid, tid).
    /// Prefers the active tab if it's a `Content::Terminal`; otherwise the
    /// first terminal tab. Uses immutable checks first to find the tab
    /// index, then takes a single mutable borrow — satisfying the borrow
    /// checker without unsafe.
    fn session_mut(
        &mut self,
        pid: WinId,
        tid: WinId,
    ) -> Result<&mut crate::terminal::Session, String> {
        // Immutable pass: find which tab index holds a terminal.
        let tab_idx = {
            let win = self.windows.iter().find(|w| w.id == pid).expect("resolved");
            let Content::Project(child) = &win.tabs[win.active].content else {
                return Err("not a project".into());
            };
            let tw = child
                .windows
                .iter()
                .find(|w| w.id == tid)
                .ok_or_else(|| format!("no such terminal: t{tid}"))?;
            // Prefer the active tab if it's a terminal, else the first terminal tab.
            let active = tw.active;
            if matches!(tw.tabs[active].content, Content::Terminal(_)) {
                active
            } else {
                tw.tabs
                    .iter()
                    .position(|t| matches!(t.content, Content::Terminal(_)))
                    .ok_or_else(|| format!("no terminal tab in t{tid}"))?
            }
        };
        // Mutable pass: take the borrow with the known index.
        let win = self.windows.iter_mut().find(|w| w.id == pid).expect("resolved");
        let Content::Project(child) = &mut win.tabs[win.active].content else {
            return Err("not a project".into());
        };
        let tw = child
            .windows
            .iter_mut()
            .find(|w| w.id == tid)
            .ok_or_else(|| format!("no such terminal: t{tid}"))?;
        let Content::Terminal(s) = &mut tw.tabs[tab_idx].content else {
            return Err(format!("tab {tab_idx} is not a terminal"));
        };
        Ok(s)
    }
```

- [ ] **Step 5: Add `send_dispatch` helper**

After `session_mut`, add:

```rust
    fn send_dispatch(
        &mut self,
        req: &crate::control::SendRequest,
    ) -> Result<(), String> {
        let terminal = req.terminal.as_deref().ok_or("send: missing terminal")?;
        let (pid, tid) = self.resolve_terminal(req.project.as_deref(), terminal)?;
        // Read mode with an immutable borrow BEFORE taking the mutable session borrow.
        let mode = {
            let win = self.windows.iter().find(|w| w.id == pid).expect("resolved");
            let Content::Project(child) = &win.tabs[win.active].content else {
                return Err("not a project".into());
            };
            let tw = child.windows.iter().find(|w| w.id == tid).expect("resolved");
            let active = tw.active;
            let idx = if matches!(tw.tabs[active].content, Content::Terminal(_)) {
                active
            } else {
                tw.tabs
                    .iter()
                    .position(|t| matches!(t.content, Content::Terminal(_)))
                    .ok_or_else(|| format!("no terminal tab in t{tid}"))?
            };
            let Content::Terminal(s) = &tw.tabs[idx].content else {
                return Err("not a terminal tab".into());
            };
            s.term_mode()
        };
        // Validate key names BEFORE any write (atomic — errors before side effects).
        let key_bytes = crate::inspect::parse_keys(&req.keys, mode)?;
        let session = self.session_mut(pid, tid)?;
        if let Some(text) = &req.text {
            session.feed(text.as_bytes());
        }
        if !key_bytes.is_empty() {
            session.feed(&key_bytes);
        }
        // settle_ms is not yet honored (settle is the next phase)
        Ok(())
    }
```

- [ ] **Step 6: Add `snapshot_dispatch` helper**

After `send_dispatch`, add:

```rust
    fn snapshot_dispatch(
        &mut self,
        req: &crate::control::SnapshotRequest,
    ) -> Result<Vec<String>, String> {
        let terminal = req.terminal.as_deref().ok_or("snapshot: missing terminal")?;
        let (pid, tid) = self.resolve_terminal(req.project.as_deref(), terminal)?;
        let session = self.session_mut(pid, tid)?;
        Ok(session.snapshot_text(None))
    }
```

- [ ] **Step 7: Add `CtrlMsg::Send` and `CtrlMsg::Snapshot` arms to `handle_ctrl`**

In `handle_ctrl` (inside the `match msg` block, after the `CtrlMsg::Close` arm, before the closing `}`), add:

```rust
            CtrlMsg::Send(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                let res = self.send_dispatch(&req);
                let ok = res.is_ok();
                let _ = reply.send(if ok {
                    ctx.request_repaint();
                    OpenReply {
                        ok: true,
                        terminal: None,
                        project: None,
                        error: None,
                        history: None,
                        seq: None,
                    }
                } else {
                    OpenReply::err(res.unwrap_err())
                });
            }
            CtrlMsg::Snapshot(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                let _ = reply.send(match self.snapshot_dispatch(&req) {
                    Ok(lines) => OpenReply {
                        ok: true,
                        terminal: None,
                        project: None,
                        error: None,
                        history: Some(lines),
                        seq: None,
                    },
                    Err(e) => OpenReply::err(e),
                });
            }
```

Note: The `use crate::control::{CtrlMsg, OpenReply, REPLY_TIMEOUT};` line at the top of `handle_ctrl` already imports these — `Send` and `Snapshot` are new variants on the same enum, no extra imports needed.

- [ ] **Step 8: Build and run all tests**

```powershell
cargo build 2>&1 | Select-Object -Last 30
cargo test 2>&1 | Select-Object -Last 40
```

Expected: all tests pass, including the six new `wm.rs` handle_ctrl tests. Zero failures.

- [ ] **Step 9: Review for borrow-checker cleanness**

`send_dispatch` duplicates the tab-index lookup from `session_mut` because Rust cannot hold both an immutable `mode` borrow and a mutable session borrow to the same data simultaneously. This is intentional and correct — it's a two-pass borrow pattern used elsewhere in the codebase (see `status_dispatch` which also takes a `display_name` title borrow then a mutable content borrow). No `unsafe` is needed.

---

### Task 4: Documentation update

**Files:**
- Check `docs/terminal-inspection.md` — if it already exists, update it; otherwise create it.

- [ ] **Step 1: Check for existing doc**

```powershell
ls "H:\claude code\foreman\docs" | Select-String "inspection"
```

- [ ] **Step 2: Create or update `docs/terminal-inspection.md`**

Create `H:\claude code\foreman\docs\terminal-inspection.md`:

```markdown
# Terminal Inspection

Lets an agent (or script) drive input into a terminal and read back its screen
without touching the GUI. Phase 1 (inspect.rs pure functions) and Phase 2 (the
`foreman send` and `foreman snapshot` verbs) are done.

## What it does

- `foreman send` — write raw UTF-8 text and/or named key presses into a terminal's PTY.
- `foreman snapshot` — read the terminal's rendered viewport as plain text rows.

Together they close the feedback loop: an agent can `send` a command and then
`snapshot` to see the result.

## How to use

```sh
# From inside a foreman terminal (self-target via env):
foreman send --text "echo hello\r"
foreman snapshot

# Targeting another terminal explicitly:
foreman send --project p1 --terminal t3 --text "ls\r"
foreman snapshot --project p1 --terminal t3

# Named key presses (same encoding as the GUI keyboard):
foreman send --project p1 --terminal t3 --keys "F5"
foreman send --project p1 --terminal t3 --text "ls" --keys "Tab Enter"
```

## Key names for --keys

`F1`..`F12`, `Up Down Left Right`, `Home End PageUp PageDown Insert Delete`,
`Enter Tab Esc Backspace Space`, single uppercase letters; `Ctrl+`/`Alt+`/`Shift+`
prefixes (combinable). A bare lowercase letter has no key sequence — use `--text`
for literal characters. Unknown name → exit 2.

## Settle (next phase)

`--settle-ms N` is accepted but not yet honored. Phase 3 will add quiescence-based
settle so a `send` blocks until the terminal goes quiet before the reply.

## Key files

- `src/inspect.rs` — pure grid-walk functions: `snapshot_text`, `parse_keys`, etc.
- `src/terminal.rs` — `Session::feed`, `Session::term_mode`, `Session::snapshot_text`
- `src/control.rs` — `SendRequest`, `SnapshotRequest`, `CtrlMsg::Send/Snapshot`, `parse_send_args`, `parse_snapshot_args`, `send_main`, `snapshot_main`
- `src/wm.rs` — `resolve_terminal`, `session_mut`, `send_dispatch`, `snapshot_dispatch`, `handle_ctrl` arms
```

- [ ] **Step 3: Final build+test gate**

```powershell
cargo build 2>&1 | Select-Object -Last 20
cargo test 2>&1 | Select-Object -Last 20
```

Expected: clean build, all tests green.

---

## Self-Review

**Spec coverage:**

| Spec requirement | Task |
|---|---|
| `SendRequest` struct with all fields | Task 2 Step 3 |
| `SnapshotRequest` struct | Task 2 Step 3 |
| `CtrlMsg::Send/Snapshot` | Task 2 Step 4 |
| `serve()` arms | Task 2 Step 5 |
| `parse_send_args` (all flags, self-target, requires text/keys) | Task 2 Step 6 |
| `parse_snapshot_args` (self-target, requires terminal) | Task 2 Step 7 |
| HELP_SEND / HELP_SNAPSHOT | Task 2 Step 8 |
| `send_main` / `snapshot_main` | Task 2 Step 10 |
| `help` test coverage for send/snapshot | Task 2 Step 11 |
| `Session::feed` / `term_mode` / `snapshot_text` | Task 1 |
| `resolve_terminal` | Task 3 Step 3 |
| `session_mut` (borrow-checker-safe two-pass) | Task 3 Step 4 |
| `send_dispatch` (read mode before mutable borrow; key bytes before write) | Task 3 Step 5 |
| `snapshot_dispatch` | Task 3 Step 6 |
| `handle_ctrl` arms (REPLY_TIMEOUT stale-drop, request_repaint on send) | Task 3 Step 7 |
| Wire compat tests (None fields omitted) | Task 2 Step 1 |
| Pipe roundtrip tests | Task 2 Step 1 |
| parse tests (all cases) | Task 2 Step 1 |
| handle_ctrl tests | Task 3 Step 1 |
| `settle_ms` parsed but not honored (comment) | Task 2 Step 6 / Task 3 Step 5 |
| `snapshot` reply uses `history` field | Task 3 Step 6 |
| `send` reply has `history: None` | Task 3 Step 7 |
| `request_repaint` after send | Task 3 Step 7 |

**Placeholder scan:** No placeholders found — all code is fully written out.

**Type consistency:** `SendRequest.terminal` is `Option<String>` throughout; `session_mut` takes `(WinId, WinId)` and both callers use it that way. `send_dispatch` and `snapshot_dispatch` both call `resolve_terminal` returning `Result<(WinId, WinId), String>` and pass the pair to `session_mut`.

**Borrow-checker note on `send_dispatch`:** The `mode` read requires an immutable borrow into the session's `term`; then `session_mut` takes a mutable borrow of the same path. Rust's NLL cannot prove these don't overlap (they don't, but it can't see through the Option match). The workaround is to read `mode` in a separate block that drops before calling `session_mut` — this is the standard two-pass pattern used elsewhere (e.g., `status_dispatch`). Alternatively, `send_dispatch` could inline the mode-read to avoid the intermediate `session_mut` abstraction entirely — this would be simpler and is also valid; the implementer may choose either.
