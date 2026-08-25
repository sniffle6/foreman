---
name: foreman-run-and-operate
description: Use when running the foreman app or driving its control plane as a developer or operator - GUI launch, the open/chat/status/close/send/snapshot/icat/view CLI verbs and flags, the \\.\pipe\foreman named pipe, exit codes 0/1/2, connect/reply timeouts, settle behavior, or errors like "cannot reach foreman", "foreman did not respond", "control server busy", "not inside a foreman terminal", "no focused project". Also foreman_panic.log, %APPDATA%\foreman artifacts, and src/control.rs questions.
---

# Run and operate foreman: app + control-plane ground truth

Developer/operator runbook for launching foreman and driving it over its Control
plane. Everything below is verified against the arg parsers and handlers in
`src/control.rs` — **not** against help text, which has a known lie (flagged
below). Code is cited by file + symbol; the `HELP_*` consts and the
`parse_*_args` fns are the ground truth, and `rg -n "fn parse_" src/control.rs`
lands you on them.

**Audience boundary:** this is the developer view. An agent running *inside*
foreman must use the **foreman-dispatch** and **foreman-chat** skills for
operational usage — those are the contract; they intentionally forbid reading
source. This skill is for people/agents working *on* foreman.

Domain terms: a **PTY** is a pseudoterminal (the OS device pair that makes a
program believe it is talking to a real terminal); on Windows the implementation
is **ConPTY**. See **terminal-emulation-reference** for the domain pack. A
**Session** is foreman's unit of one running terminal (process + PTY + emulated
screen); a **Project** is a top-level Win whose Content is its own nested window
manager of Sessions.

## Launch modes

One binary, two personalities (`fn main`, `src/main.rs`):

| Invocation | Behavior |
|---|---|
| `foreman` (zero args) | Launches the GUI. Installs the panic logger, installs global agent skills when the `install_skills` setting is on (see Artifacts), starts the pipe server on a background thread. |
| `foreman <anything>` | CLI subcommand — a thin pipe client (`control::client_main`), no GUI. Exits with the subcommand's code. |

```powershell
# From repo root "H:/claude code/foreman"
cargo run                                  # debug GUI
cargo run --release                        # release GUI (the "is it fast" build)
& .\target\debug\foreman.exe               # run a built debug exe directly
& .\target\release\foreman.exe             # release exe
& .\target\debug\foreman.exe status        # any arg = CLI, talks to the RUNNING instance
```

Build-environment traps (GNU toolchain, `Access is denied (os error 5)`, kill
before build) are **foreman-build-and-env**'s home. Screenshot-based visual
verification is the **build-screenshot** skill.

**One foreman per machine owns the pipe.** A second GUI instance still opens,
but its pipe listener fails to bind and it prints
`control: pipe unavailable after N attempts (...); agent dispatch disabled` —
GUI-only, no Control plane (`listen_retry`, `src/control.rs`).

## Subcommand reference

The dispatch table is `control::client_main`; read it first when a verb list
looks stale. Of the verbs below, `icat` runs entirely client-side; every other
one is a pipe request.

General rules:

- `foreman help | --help | -h` prints top-level help, exit 0. Per-verb
  `--help`/`-h` works only as the **first** argument after the verb (checked
  before env reads, so help works outside a foreman terminal). Anywhere else it
  is parsed as a flag/word.
- Unknown verb or no verb (with args present) prints usage to stderr, exit 2.
- Every `--project P` takes ids like `p3`; terminals are `t4`. **Ids are
  per-project and never reused within one app run** (`WindowManager`'s id
  counter is monotonic, `src/wm.rs`) — but they restart when the app
  restarts, and a `tN` is only unique *within its project*.

| Verb | Syntax (exact) | Notes |
|---|---|---|
| open | `foreman open [--project P] [--title T] [--cwd D] -- <command...>` | `--` is **mandatory**; command after it must be non-empty; nothing is shell-interpreted. Unknown flag = error. Default project: `FOREMAN_PROJECT_ID`, else the focused Project. Default cwd: the Project's directory. Default title: `agent · <argv[0]>`. |
| chat (post) | `foreman chat [--project P] [--to TARGET]... [--re N] [--] <message...>` | Flags come **first**; the first positional word (or explicit `--`) ends flag parsing, rest is the message verbatim (words joined with single spaces). Requires `FOREMAN_TERMINAL_ID` (the sender). `--to` repeatable; each TARGET is either a `tN` or the word `you` (leading `@` stripped). `--re N` = reply to room seq N. |
| chat (read) | `foreman chat [--project P] --history [N]` | N optional (default 20, `DEFAULT_HISTORY`). Mutually exclusive with a message AND with `--to`; `--re` is post-only. Needs no terminal id; never joins the room. |
| status | `foreman status [--project P]` | **Deliberately no env default**: bare `status` lists ALL projects; it is an overview verb. `--project pN` filters; unknown pN is an error. Any stray positional is an error. |
| close | `foreman close [tN ...] [--project P]` | Ids must match `t<digits>`. **`close` takes no other flags** — no `--terminal`; any other `--` token is `unknown flag: X`, exit 2, and a bare word that is not a `tN` is `bad terminal id: X (expected tN)`. With ids: closes those in P (default `FOREMAN_PROJECT_ID`, else focused). Validation is **all-or-nothing**: any unknown/non-terminal id fails the whole request, nothing closes. With **no** ids = self-close: requires BOTH `FOREMAN_TERMINAL_ID` and `FOREMAN_PROJECT_ID`, and **refuses** an explicit `--project` (`--project needs explicit terminal ids; bare close closes your own terminal`). |
| send | `foreman send [--project P] [--terminal T] [--text TXT] [--keys "K K..."]... [--settle-ms N]` | At least one of `--text`/`--keys`. `--text` is raw UTF-8 written verbatim (last one wins if repeated); `--keys` splits its value on whitespace and is repeatable (appends). Write order: text first, then keys. No `--terminal` = self-target: requires BOTH env vars and **refuses** an explicit `--project` (a `tN` is only unique within its project — pass `--terminal` to cross projects). Key names validated against the Session's live terminal mode BEFORE any byte is written — errors have no side effects (`send_dispatch`, `src/wm.rs`). |
| snapshot | `foreman snapshot [--project P] [--terminal T] [--attrs] [--cursor] [--tail N]` | `--attrs`/`--cursor` are valueless opt-ins. `--tail N` (positive integer; `0`, a missing value, or a non-number all fail with `--tail needs a positive integer`) is the last N buffer lines including scrollback, not the displayed viewport; larger than the buffer returns the whole buffer. No `--terminal`: **refuses** an explicit `--project` (same rule as send); self-targets only when `FOREMAN_TERMINAL_ID` is set (then `FOREMAN_PROJECT_ID` is required); otherwise errors `--terminal is required`. |
| icat | `foreman icat <file.png> [--cols N]` | **Client-side only — no pipe request.** Prints the PNG into the calling pane as kitty graphics; `--cols N` (positive) overrides the width. Bad `--cols`, a missing/unreadable file, a non-PNG, or a second positional all exit 2. Operational use by an agent inside foreman: the **foreman-icat** skill. |
| view | `foreman view <file.png> [--project P]` | Opens a persistent `Content::Image` window (a normal pane: tile/float/tab/close/minimize, restored across restarts by path). PNG only in v1. The path is canonicalized **client-side, relative to this process's cwd, not the GUI's** — a nonexistent file or non-`.png` extension fails before the pipe is touched. Reply is `open`-shaped: `{"ok":true,"terminal":"tN","project":"pN"}`. |

### send key-name grammar (`inspect::parse_keys`, encoding in `input::encode_key`)

Recognized names: `F1`..`F12`; `Up Down Left Right`; `Home End PageUp PageDown`;
`Insert`/`Ins`, `Delete`/`Del`; `Enter Tab Esc Escape Backspace`; single letters.
Modifier prefixes `Ctrl+`/`Control+`, `Alt+`/`Meta+`/`Option+`, `Shift+`,
combinable (`Ctrl+Shift+F5`). Names are matched case-sensitively except letters
and modifiers.

Traps (all exit 2 with
`key '<token>' has no input sequence — use --text for literal characters`):

- **A bare letter never encodes — uppercase or lowercase.** Letters only work
  with `Ctrl+` (control code) or `Alt+` (ESC-prefixed); `Shift+letter` alone
  also encodes nothing (`input::encode_key`). Use `--text` for literal
  characters.
- **`Space` is a lie in the help text.** `send --help` lists it, and the name
  parses, but `encode_key` has no Space arm and egui names the key `"Space"`
  (not a 1-byte letter), so it encodes to nothing in every modifier
  combination. Use `--text " "`.
- Enter/Tab/Backspace/Esc ignore modifiers (fixed bytes `\r`, `\t`, `0x7f`,
  `0x1b`).

Arrow/F-key byte sequences honor the Session's live mode (e.g. app-cursor mode
switches `ESC[A` to `ESC OA`) — `send --keys` and the real keyboard go through
the same encoding seam, so they can never diverge.

### Quiescence settle (`send`)

`--settle-ms N` controls the Quiescence settle: after writing, foreman waits
until the Session has produced no new output for N ms before replying, so a
following Snapshot reads a settled screen. The Send arm of
`WindowManager::handle_ctrl` parks a `PendingSettle`; `advance_settles` drains
it each frame (both `src/wm.rs`).

| Value | Behavior |
|---|---|
| omitted | the user's `Settings::send_settle_ms` (src/config.rs) — ships at 120 ms, editable in the Settings pane, `sanitize`-clamped to 2000 |
| `0` | fire-and-forget: reply immediately, no wait |
| `N` | quiet window `min(N, MAX_SETTLE_MS)`; the same hard total deadline applies regardless, so a chatty Session can't stall the reply |

The deadline chain is deliberate and must stay ordered:
`send_settle_ms` (≤2000) < `MAX_SETTLE_MS` (4000, src/wm.rs) < `REPLY_TIMEOUT`
(5 s) < `CONNECT_TIMEOUT` (10 s) — a settle reply always lands before the pipe
server gives up, and the server before the client. New output resets the quiet
timer; the deadline does not reset.

## Exit codes and reply conventions

Exit codes (`fn report`, `src/control.rs`, plus the parse paths):

| Code | Meaning |
|---|---|
| 0 | ok (including all `--help` paths) |
| 1 | foreman refused (`ok:false` reply, error printed to stderr) or is unreachable (pipe missing, busy past the connect deadline) |
| 2 | bad arguments (parse error, unknown verb/flag, bad key name) |

Reply printing:

- Line-oriented payloads (`chat --history`, `status`, plain `snapshot`) print
  **line per line** on stdout.
- `snapshot --attrs`/`--cursor` prints the **whole reply as one JSON line**
  (fields: `cells` = per-cell `{ch, fg:[r,g,b], bg, bold, italic, underline,
  strikethrough, inverse, dim, wide}`; `cursor` = `{row, col, shape}` with shape
  in `block|beam|underline|hollow|hidden`).
- Everything else prints the reply JSON: `open` and `view` →
  `{"ok":true,"terminal":"tN","project":"pN"}` (record `terminal` — it is how
  the Worker is addressed); chat post → `{"ok":true,"seq":N}`; `close` →
  `{"ok":true,"project":"pN"}`; `send` → `{"ok":true}` after the settle.
- Plain `snapshot` rows are the rendered viewport as currently displayed
  (scrollback-scroll included), trailing spaces trimmed, wide-char spacer cells
  skipped (`inspect::snapshot_text`). `--tail N` instead returns the last N
  lines of the buffer (history + live screen) and ignores display offset
  (`inspect::snapshot_tail`).

**`open`'s `ok:true` means "a terminal opened", never "the command succeeded".**
A Worker that spawned and instantly died still returned ok; `status` shows it as
`exited(code)` because status polls the live process, not the title.

## Transport

- Named pipe `\\.\pipe\foreman` (`PIPE = "foreman"` mapped via the
  `interprocess` crate's `GenericNamespaced`, `src/control.rs`).
- Protocol: **one JSON line request → one JSON line reply per connection**,
  verbs discriminated by the `"cmd"` field. You can drive it from any local
  process, not just the CLI (that is how test harnesses script it — see
  **foreman-diagnostics-and-tooling**).
- Server: one background thread accepts; **each connection gets its own
  short-lived thread** so a stalled client blocks only itself.
  `MAX_INFLIGHT` bounds concurrent handlers; over the cap the connection
  gets a fast `{"ok":false,"error":"foreman: control server busy"}`
  (`src/control.rs`).
- Each queued request calls `ctx.request_repaint()` so an idle GUI wakes and
  drains it immediately instead of on the idle repaint tick. The GUI drains the
  channel every frame and drives settles after all Sessions have pumped
  (`App::ui` → `WindowManager::handle_ctrl` / `advance_settles`).

## Timeout ladder and at-most-once semantics

Two constants (`src/control.rs`):

| Constant | Value | Meaning |
|---|---|---|
| `CONNECT_TIMEOUT` | 10 s | Client-side deadline to connect while the pipe is busy (queues via `WaitNamedPipeW`). A pipe that does not exist fails immediately — no 10 s hang. |
| `REPLY_TIMEOUT` | 5 s | Server-side wait for the GUI to answer one request; on expiry the client gets `foreman did not respond`. |

The contract that makes retries safe lives in `WindowManager::handle_ctrl`
(`src/wm.rs`):

- The GUI **drops any request older than `REPLY_TIMEOUT` unexecuted** (checked
  at the top of every handler arm). A timed-out request NEVER executes later, so
  a retrying dispatcher cannot create a duplicate spawn.
- `open` whose reply channel died (client gone) gets its **spawn undone**
  (`close_terminal`).
- `close` replies **before** closing (a self-close kills the caller's own
  process tree, reply must be on the wire first); if the reply channel is dead
  the close is skipped entirely — ids are never reused, so a retry errs loudly
  instead of double-closing.
- A chat post whose reply channel died **stays in the log** (append-only room);
  only the injection is skipped. A retrying client may duplicate a history
  line — accepted v1.

Operator triage for client-side errors:

| stderr message | Meaning | Action |
|---|---|---|
| `cannot reach foreman (...) — is it running?` | No pipe: foreman not running, or this instance lost the bind race | Start foreman / find the owning instance |
| `foreman is running but its control pipe stayed busy for 10s — retry, or check for a wedged dispatch` | Pipe exists but connect deadline expired | Retry; if persistent, a handler thread is wedged by a dead client |
| `foreman did not respond` | GUI did not answer within `REPLY_TIMEOUT` | The request did NOT and will NOT execute; safe to retry |
| `foreman: control server busy` | In-flight handlers past `MAX_INFLIGHT` | Back off and retry |
| `foreman is not accepting requests` | GUI drain channel closed (shutting down) | Restart foreman |

## Env contract and per-verb self-targeting

Every foreman-spawned Session gets `FOREMAN=1`, `FOREMAN_EXE` (path to the
binary — PATH won't have `target\debug`), `FOREMAN_PROJECT_ID` (`pN`), and
`FOREMAN_TERMINAL_ID` (`tN`), plus terminal-capability vars
(`WindowManager::term_env`, `src/wm.rs`). The full injected-env table and how to extend it is
**foreman-config-and-flags**' home. The CLI reads them to default targets:

| Verb | `FOREMAN_PROJECT_ID` | `FOREMAN_TERMINAL_ID` |
|---|---|---|
| open | default for `--project` | unused |
| chat post | default for `--project` | **required** (sender identity) |
| chat --history | default for `--project` | optional |
| status | **ignored — deliberate** | ignored |
| close tN... | default for `--project` | unused |
| close (bare = self) | **required**; `--project` refused | **required** |
| send without `--terminal` | **required**; `--project` refused | **required** |
| send/snapshot with `--terminal` | default for `--project` | unused |
| snapshot without `--terminal` | **required**; `--project` refused | used as the target when set; otherwise `--terminal is required (or run inside a foreman terminal to target your own pane)` |
| view | default for `--project` | unused |
| icat | unused (never touches the pipe) | unused |

The recurring error strings are exact: `not inside a foreman terminal
(FOREMAN_TERMINAL_ID unset)` and `cannot resolve your own pane without
FOREMAN_PROJECT_ID (terminal ids are only unique within a project)`.

## Dispatch semantics (what `open` actually does)

- **The Worker never steals focus.** The new Win spawns visually on top but
  keyboard focus is saved and restored around `push_win`
  (`add_terminal_cmd`, `src/wm.rs`) — fire-and-watch: the new terminal is to
  look at, not type into.
- A dim banner line `── dispatched: <truncated argv>… ──` is injected into the
  Session's **emulator, not its PTY** (`Session::inject_note`, `src/terminal.rs`;
  text from `dispatch_banner`, `src/wm.rs`) so a silent `claude -p` Worker
  doesn't read as hung. It is deferred to the first resize so the first-frame
  grid shrink can't strand it in scrollback.
- **npm-shim handling** (`Session::spawn_argv`, `src/terminal.rs`): npm
  installs agents as `.cmd` shims that CreateProcess can't run directly; a
  failed direct spawn retries once through `cmd.exe /c`. Anything routed through
  cmd.exe re-parses the command line, so an argv containing a newline, `\r`, or
  `"` targeting a `.cmd`/`.bat` (or falling back to the wrap) is **refused
  loudly**: `... runs via a cmd-shim (...) which cannot carry newlines or " in
  arguments — flatten the prompt to one quote-free line or install the tool as a
  native exe`.
- Dispatched Sessions auto-join the Project's Chat room with their stable
  Member id (`term_tag(id)` — the same id baked into `FOREMAN_TERMINAL_ID`).
  Chat posts are delivered into member
  Sessions as typed input (bracketed paste + deferred submit), gated on the
  Session being Ready, with the per-frame delivery decided by the Outbox — room
  usage and etiquette live in **foreman-chat**; design rationale in
  **foreman-architecture-contract**.

## Fleet operation loop

The minimal operator loop for driving many Sessions (measurement-grade
verification recipes belong to **foreman-diagnostics-and-tooling**):

```powershell
$exe = ".\target\debug\foreman.exe"                  # inside a foreman terminal: $exe = $env:FOREMAN_EXE
& $exe status                                        # map the fleet: pN / tN / running|exited(code) / chat|-
& $exe open --project p1 --title "worker" -- claude -p "fix the tests"
& $exe send --project p1 --terminal t3 --text "hello`r"   # \r = Enter; reply lands after the settle
& $exe snapshot --project p1 --terminal t3                # read the settled screen
& $exe close t3 --project p1
```

`status` line format (`WindowManager::status_dispatch`, `src/wm.rs`):
`p1  <name>  <cwd>` then indented `t3  running  chat  <title>` (state
`running|exited(code)`, chat membership `chat|-`). Prints `no projects` when
there are none.

Known targeting gotcha: after tab-merging two Projects, only the **active** tab
resolves — the swallowed Project's old `pN` goes stale
(`WindowManager::resolve_project`, `src/wm.rs`).

## Artifacts map

| Artifact | Location | Writer |
|---|---|---|
| `settings.json` | `%APPDATA%\foreman\` | Persisted app settings — the live field set is `struct Settings` in `src/config.rs`; read it there rather than trusting any list written down elsewhere. Atomic tmp+rename writes; corrupt/missing files fall back to defaults, never crash. |
| `keybindings.json` | `%APPDATA%\foreman\` | User Keymap overrides, merged over in-code defaults (`src/keymap.rs`). Details: **foreman-config-and-flags**. |
| `recents.json`, `workspace.json`, `themes\<slug>.json` | `%APPDATA%\foreman\` | Recent project list, restored workspace layout, user themes. Same atomic-write, defaults-on-corruption discipline. |
| `foreman_panic.log` | `%APPDATA%\foreman\` | Appended by the panic hook before the process aborts — a panic inside the egui event loop is otherwise invisible (`install_panic_logger` → `crash_log_path` → `config::config_dir()`, `src/main.rs`). Entries are `=== foreman panic {ts} ===`, so a stale entry is identifiable. A CWD-relative file is only the fallback for an unresolvable `APPDATA`; one sitting in a repo root is probably a fossil. First stop when the GUI vanished. |
| `win.png` | repo root | The **build-screenshot** skill's capture output. |
| Global skill installs | `<CLAUDE_CONFIG_DIR or ~/.claude>/skills/` and `<CODEX_HOME or ~/.codex>/skills/` | On every GUI startup — **gated on the `install_skills` setting** — foreman writes its embedded agent-facing skills (`CLAUDE_SKILLS` / `CODEX_SKILLS` in `src/skills_install.rs`, each an `include_str!`; `OBSOLETE_SKILLS` is the rename/removal hook), marked "managed by foreman; edits are overwritten on launch". Byte-compare idempotent, atomic tmp+rename, best-effort — failures log and never block launch. Source copies live in `.claude/skills/` and `.codex/skills/` in this repo. |

## Security posture

The pipe is a **same-user command-execution surface**: any local process can
connect, claim any `from` identity, and make foreman spawn arbitrary argv. The
env-var requirements and id validation are a **guardrail against confused
agents, NOT a security boundary** — stated in code in `src/control.rs`
(the `ChatRequest.from` doc comment).
Do not build anything that treats a pipe caller as authenticated. Changing this
posture is a design decision → **foreman-change-control**.

## When NOT to use this skill

- Agent running **inside** foreman that just needs to dispatch, chat, or show
  an image → the **foreman-dispatch** / **foreman-chat** / **foreman-icat**
  skills (the user-facing contract — they forbid reading source on purpose).
- A live failure to triage rather than a verb to look up →
  **foreman-debugging-playbook**.

## Provenance and maintenance

Re-verify these before leaning on them — load-bearing and known to move:

| Claim | Re-verify (repo root) |
|---|---|
| The verb list is complete | `rg -n "Some\(\"" src/control.rs` inside `client_main` — that match arm IS the verb list |
| The `Space` lie in `HELP_SEND` | `rg -n "Key::Space" src/input.rs` — no arm in `encode_key` means `--keys Space` still encodes nothing while the help text still advertises it |
| Panic log destination | `rg -n "crash_log_path" src/main.rs` — must route through `config::config_dir()` (`%APPDATA%\foreman\`), not the CWD |
| Deadline order still holds | `rg -n "send_settle_ms" src/config.rs; rg -n "MAX_SETTLE_MS" src/wm.rs; rg -n "_TIMEOUT" src/control.rs` — settle < cap < reply < connect |
