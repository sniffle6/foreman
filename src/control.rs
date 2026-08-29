//! Agent dispatch control channel: a named pipe (`\\.\pipe\foreman`) any local
//! process can use to open a terminal inside the running foreman. See
//! docs/epics/agent-dispatch-epic.md.

/// Pipe name; `GenericNamespaced` maps it to `\\.\pipe\foreman` on Windows.
pub const PIPE: &str = "foreman";

/// This GUI instance's own control pipe name, served alongside [`PIPE`] and
/// injected into every terminal as `FOREMAN_PIPE`. With several foremans
/// running at once (installed daily driver + a dev build under test), the
/// well-known name routes to whichever instance answers first — an agent
/// inside the dev build could reach the installed host and get "unknown cmd"
/// for verbs its own host supports. The instance pipe makes in-foreman
/// clients bind to the host that spawned them, deterministically. Pid + nonce
/// for uniqueness among live instances (same recipe as the title pipe).
pub fn instance_pipe() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("foreman-ctl-{}-{nonce}", std::process::id())
    })
}

/// Which pipe a CLI invocation talks to: the host-injected `FOREMAN_PIPE`
/// inside a foreman terminal, the well-known [`PIPE`] outside one.
fn client_pipe() -> String {
    pipe_for(std::env::var("FOREMAN_PIPE").ok())
}

/// Pure seam for [`client_pipe`]: blank or unset env falls back to [`PIPE`].
fn pipe_for(env: Option<String>) -> String {
    env.filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| PIPE.to_string())
}

/// How long the pipe server waits for the GUI to answer one request. The GUI
/// drain uses the same constant to drop requests the server has given up on.
pub const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Client-side deadline for connecting to a busy pipe. The server handles one
/// connection at a time, so concurrent dispatchers queue on the pipe (the
/// `interprocess` crate waits on `ERROR_PIPE_BUSY` via `WaitNamedPipeW`); the
/// deadline turns "server wedged by a bad client" from an infinite hang into
/// an error the dispatching agent can act on.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How many chat lines a bare `--history` (no count) returns.
pub const DEFAULT_HISTORY: usize = 20;

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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct OpenReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>, // "t4" — unique only within its project
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>, // "p1"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Generic line-per-line payload — chat `--history` results and `status`
    /// listings both ride here; `report()` prints it line per line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<String>>,
    /// The posted message's seq — what a later reply cites via `--re`. Set
    /// only on a successful post reply; skipped on the wire when None so v1
    /// replies stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The id `kanban add` created. Skipped on the wire when None so v1
    /// replies stay byte-identical (same pattern as `seq`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Per-cell attribute grid for `snapshot --attrs`. None (omitted on the
    /// wire) unless `--attrs` was requested, so v1 replies stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<Vec<crate::inspect::CellData>>>,
    /// Cursor position + shape for `snapshot --cursor`. None (omitted on the
    /// wire) unless `--cursor` was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<crate::inspect::CursorInfo>,
}

impl OpenReply {
    pub fn err(msg: impl Into<String>) -> Self {
        OpenReply {
            ok: false,
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

/// Project chat post or history read (spec: agent-group-chat §1). Exactly one
/// of `text` (post) / `history` (read last N) must be set — the client
/// enforces this; the server treats `history` as the discriminator. `from` is
/// the sender's own terminal id from its env: required to post, optional and
/// ignored on history reads (any caller may read). As with `open`, this is a
/// guardrail against confused agents, NOT a security boundary — any local
/// process can speak to the pipe and claim any `from`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChatRequest {
    pub cmd: String, // always "chat"
    #[serde(default)]
    pub project: Option<String>, // "p1"; None = focused project
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>, // "t2"; posts always carry it
    /// Delivery targets from `--to` flags (mentions spec §1). Inline leading-@
    /// mentions are NOT carried here — they ride in `text` and the server
    /// extracts them. Empty = no explicit targets; skipped on the wire so
    /// untargeted requests stay byte-identical to v1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub history: Option<usize>,
    /// Handshake back-pointer (`--re N`): the seq this post replies to. The
    /// server decides whether it actually closes a handshake (cited post must
    /// be a `Post` whose to-set includes `from`). Skipped when None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<u64>,
}

/// List projects and their terminals. `project` is an explicit opt-in filter;
/// `None` means ALL projects — deliberately NOT the caller's
/// FOREMAN_PROJECT_ID or the focused project (status is an overview verb).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StatusRequest {
    pub cmd: String, // always "status"
    #[serde(default)]
    pub project: Option<String>, // "p2"; None = ALL projects (no focused fallback)
}

/// Close terminal panes in one project. Self-close is resolved CLIENT-side:
/// bare `foreman close` puts the caller's own FOREMAN_TERMINAL_ID into
/// `terminals` (and requires FOREMAN_PROJECT_ID — a tN is only unique within
/// its project), so the server never sees an empty list. Validation is
/// all-or-nothing: any unknown/non-terminal id fails the whole request and
/// nothing closes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloseRequest {
    pub cmd: String, // always "close"
    #[serde(default)]
    pub project: Option<String>, // "p1"; None = focused project
    pub terminals: Vec<String>, // "tN" ids; client guarantees non-empty
}

/// Drive raw input into a terminal. `text` is written verbatim (UTF-8);
/// `keys` are named key presses encoded through `inspect::parse_keys` with
/// the session's live `TermMode`. Text first, then keys. `settle_ms` defers
/// the reply until the screen goes quiet for that long (see `wm::PendingSettle`).
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

/// Serde skip predicate for boolean opt-in flags: omit `false` on the wire so a
/// plain snapshot request/reply stays byte-identical to v1.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Open a persistent `Content::Image` window showing a PNG. `path` is always
/// an absolute, canonicalized path — resolved CLIENT-side (see
/// `parse_view_args`) so the GUI (which may have a different cwd) never
/// guesses. The GUI still decodes defensively: a path that goes stale between
/// client-side validation and the GUI opening it lands in the viewer's
/// placeholder state, never a dropped request.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ViewRequest {
    pub cmd: String, // always "view" in v1
    #[serde(default)]
    pub project: Option<String>, // "p3"; None = focused project
    pub path: String,
}

/// Read a terminal's grid as text. Default is the currently displayed viewport;
/// `tail` of Some(N) is the last N buffer lines (scrollback included). `attrs`
/// and `cursor` are opt-ins that attach structured fields to the reply.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotRequest {
    pub cmd: String, // always "snapshot"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// `--attrs`: include per-cell colors + style flags (`cells`) in the reply.
    #[serde(default, skip_serializing_if = "is_false")]
    pub attrs: bool,
    /// `--cursor`: include cursor position + shape (`cursor`) in the reply.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cursor: bool,
    /// `--tail N`: last N lines of the buffer (scrollback + live screen), not
    /// the currently displayed viewport. Omitted on the wire when None so v1
    /// requests stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<usize>,
}

/// Kanban board verb (spec: kanban-board §wire shape). `action` selects the
/// operation; unused fields are omitted on the wire so a plain `list`
/// request stays minimal and future v1 additions don't affect old callers.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct KanbanRequest {
    pub cmd: String,    // always "kanban"
    pub action: String, // "add" | "list" | "start" | "done" | "block" | "rm"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>, // None = caller's FOREMAN_PROJECT_ID, else focused
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>, // caller's FOREMAN_TERMINAL_ID; required for start
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>, // list filter
    #[serde(default, skip_serializing_if = "is_false")]
    pub json: bool, // list output mode
}

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
                return Ok(OpenRequest {
                    cmd: "open".into(),
                    project,
                    cwd,
                    title,
                    command,
                });
            }
            flag @ ("--project" | "--title" | "--cwd") => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| format!("{flag} needs a value"))?
                    .clone();
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

use interprocess::ConnectWaitMode;
use interprocess::local_socket::{ConnectOptions, GenericNamespaced, ListenerOptions, prelude::*};
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

/// One control request, the channel the GUI thread answers on, and when the
/// server queued it. The GUI must NOT execute requests older than
/// [`REPLY_TIMEOUT`]: the server has already told that client "foreman did not
/// respond", so spawning would open a terminal the dispatcher believes failed —
/// and a retrying dispatcher would then create a duplicate.
pub enum CtrlMsg {
    Open(OpenRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Chat(ChatRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Status(StatusRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Close(CloseRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Send(SendRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Snapshot(SnapshotRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    View(ViewRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Kanban(KanbanRequest, mpsc::Sender<OpenReply>, std::time::Instant),
}

/// Create the pipe listener, retrying briefly: after an update-restart the
/// old instance can hold the pipe a beat past its window closing, and two
/// instances launched fast race it. First success wins; None = give up
/// (dispatch disabled), same behavior as the old one-shot failure.
fn listen_retry(
    name: interprocess::local_socket::Name<'_>,
    attempts: u32,
    delay: std::time::Duration,
) -> Option<interprocess::local_socket::Listener> {
    for i in 0..attempts {
        match ListenerOptions::new().name(name.clone()).create_sync() {
            Ok(l) => return Some(l),
            Err(e) if i + 1 == attempts => {
                eprintln!(
                    "control: pipe unavailable after {attempts} attempts ({e}); agent dispatch disabled"
                );
            }
            Err(_) => std::thread::sleep(delay),
        }
    }
    None
}

/// Pipe server. Runs on a background thread for the GUI's whole lifetime; the
/// GUI drains `tx`'s receiver each frame. One JSON line in, one JSON line out,
/// per connection.
pub fn serve(pipe: &str, tx: mpsc::Sender<CtrlMsg>, ctx: eframe::egui::Context) {
    let Ok(name) = pipe.to_ns_name::<GenericNamespaced>() else {
        return;
    };
    let Some(listener) = listen_retry(name, 8, std::time::Duration::from_millis(250)) else {
        return;
    };
    // Each connection is handled on its own short-lived thread: read the request,
    // hand it to the GUI, wait for the reply, write it back. A client that
    // connects and stalls — or a slow GUI reply — then blocks only its own thread,
    // never dispatch for everyone else (the flaw the single-threaded loop had).
    // MAX_INFLIGHT bounds concurrent handlers so a flood of stalled clients can't
    // spawn threads without limit; over the cap we reject fast. interprocess' sync
    // stream exposes no clean read timeout, so a wedged handler is reclaimed only
    // when its client goes away — acceptable because the cap bounds the leak.
    const MAX_INFLIGHT: usize = 64;
    let inflight = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        if inflight.load(Ordering::Relaxed) >= MAX_INFLIGHT {
            let mut conn = BufReader::new(conn);
            let mut out = serde_json::to_string(&OpenReply::err("foreman: control server busy"))
                .expect("OpenReply is always serializable");
            out.push('\n');
            let _ = conn.get_mut().write_all(out.as_bytes());
            continue;
        }
        inflight.fetch_add(1, Ordering::Relaxed);
        let tx = tx.clone();
        let ctx = ctx.clone();
        let inflight = inflight.clone();
        std::thread::spawn(move || {
            let mut conn = BufReader::new(conn);
            let mut line = String::new();
            if conn.read_line(&mut line).is_ok() {
                #[derive(serde::Deserialize)]
                struct Verb {
                    cmd: String,
                }
                let now = std::time::Instant::now();
                let (rtx, rrx) = mpsc::channel();
                let msg = match serde_json::from_str::<Verb>(&line) {
                    Err(e) => Err(format!("bad request: {e}")),
                    Ok(v) => match v.cmd.as_str() {
                        "open" => serde_json::from_str::<OpenRequest>(&line)
                            .map(|r| CtrlMsg::Open(r, rtx, now))
                            .map_err(|e| format!("bad request: {e}")),
                        "chat" => serde_json::from_str::<ChatRequest>(&line)
                            .map(|r| CtrlMsg::Chat(r, rtx, now))
                            .map_err(|e| format!("bad request: {e}")),
                        "status" => serde_json::from_str::<StatusRequest>(&line)
                            .map(|r| CtrlMsg::Status(r, rtx, now))
                            .map_err(|e| format!("bad request: {e}")),
                        "close" => serde_json::from_str::<CloseRequest>(&line)
                            .map(|r| CtrlMsg::Close(r, rtx, now))
                            .map_err(|e| format!("bad request: {e}")),
                        "send" => serde_json::from_str::<SendRequest>(&line)
                            .map(|r| CtrlMsg::Send(r, rtx, now))
                            .map_err(|e| format!("bad request: {e}")),
                        "snapshot" => serde_json::from_str::<SnapshotRequest>(&line)
                            .map(|r| CtrlMsg::Snapshot(r, rtx, now))
                            .map_err(|e| format!("bad request: {e}")),
                        "view" => serde_json::from_str::<ViewRequest>(&line)
                            .map(|r| CtrlMsg::View(r, rtx, now))
                            .map_err(|e| format!("bad request: {e}")),
                        "kanban" => serde_json::from_str::<KanbanRequest>(&line)
                            .map(|r| CtrlMsg::Kanban(r, rtx, now))
                            .map_err(|e| format!("bad request: {e}")),
                        other => Err(format!("unknown cmd: {other}")),
                    },
                };
                let reply = match msg {
                    Err(e) => OpenReply::err(e),
                    Ok(m) => {
                        if tx.send(m).is_err() {
                            OpenReply::err("foreman is not accepting requests")
                        } else {
                            // Wake the (possibly idle) render loop so it drains this
                            // message and replies now, not on the idle repaint tick.
                            ctx.request_repaint();
                            rrx.recv_timeout(REPLY_TIMEOUT)
                                .unwrap_or_else(|_| OpenReply::err("foreman did not respond"))
                        }
                    }
                };
                let mut out =
                    serde_json::to_string(&reply).expect("OpenReply is always serializable");
                out.push('\n');
                let _ = conn.get_mut().write_all(out.as_bytes());
            }
            inflight.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

/// Client side: send one request, wait for the one-line reply.
///
/// Connecting waits (deadline-bounded) while the serial server is busy with
/// another client; a pipe that doesn't exist at all still fails immediately.
pub fn request(pipe: &str, req: &impl serde::Serialize) -> std::io::Result<OpenReply> {
    let name = pipe.to_ns_name::<GenericNamespaced>()?;
    let conn = ConnectOptions::new()
        .name(name)
        .wait_mode(ConnectWaitMode::Timeout(CONNECT_TIMEOUT))
        .connect_sync()?;
    let mut conn = BufReader::new(conn);
    let mut line = serde_json::to_string(req).map_err(std::io::Error::other)?;
    line.push('\n');
    conn.get_mut().write_all(line.as_bytes())?;
    let mut reply = String::new();
    conn.read_line(&mut reply)?;
    serde_json::from_str(&reply).map_err(std::io::Error::other)
}

/// Parse `foreman chat` args: `[--project P] [--history [N]] [--] <message...>`.
/// Flags come first; the first positional word (or an explicit `--`) ends flag
/// parsing and the remainder is the message verbatim — so flag-like text inside
/// the message body is never reinterpreted. A message post and `--history`
/// (default [`DEFAULT_HISTORY`]) are mutually exclusive. `default_project` /
/// `self_terminal` come from the caller's FOREMAN_* env; a caller outside a
/// foreman terminal cannot use chat.
pub fn parse_chat_args(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
) -> Result<ChatRequest, String> {
    let mut project = default_project;
    let mut history: Option<usize> = None;
    let mut to: Vec<String> = Vec::new();
    let mut re: Option<u64> = None;
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            "--history" => {
                // optional count: `--history 5` or bare `--history`
                match args.get(i + 1).and_then(|v| v.parse::<usize>().ok()) {
                    Some(n) => {
                        history = Some(n);
                        i += 2;
                    }
                    None => {
                        history = Some(DEFAULT_HISTORY);
                        i += 1;
                    }
                }
            }
            "--to" => {
                let v = args.get(i + 1).ok_or("--to needs a value (tN or you)")?;
                let id = v.strip_prefix('@').unwrap_or(v);
                if !crate::chat::valid_chat_target(id) {
                    return Err(format!("bad --to target: {v} (expected tN or you)"));
                }
                to.push(id.to_string());
                i += 2;
            }
            "--re" => {
                let v = args.get(i + 1).ok_or("--re needs a seq number")?;
                re = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("--re needs a seq number, got: {v}"))?,
                );
                i += 2;
            }
            "--" => {
                // explicit end of flags: everything after is the message verbatim
                words.extend_from_slice(&args[i + 1..]);
                break;
            }
            other if other.starts_with("--") => {
                return Err(format!(
                    "unknown flag: {other} (use -- to post a message starting with --)"
                ));
            }
            _ => {
                // first positional ends flag parsing: the rest is the message verbatim
                words.extend_from_slice(&args[i..]);
                break;
            }
        }
    }
    match (words.is_empty(), history) {
        (false, None) => {
            // posting needs a sender identity; reading history does not
            let from = Some(
                self_terminal.ok_or("not inside a foreman terminal (FOREMAN_TERMINAL_ID unset)")?,
            );
            let text = words.join(" ");
            Ok(ChatRequest {
                cmd: "chat".into(),
                project,
                from,
                to,
                text: Some(text),
                history: None,
                re,
            })
        }
        (true, Some(n)) => {
            if !to.is_empty() {
                return Err("--to and --history are mutually exclusive".into());
            }
            if re.is_some() {
                return Err("--re is post-only, not valid with --history".into());
            }
            Ok(ChatRequest {
                cmd: "chat".into(),
                project,
                // include the sender when available, omit otherwise — history
                // works for any caller (FOREMAN_TERMINAL_ID not required)
                from: self_terminal,
                to: Vec::new(),
                text: None,
                history: Some(n),
                re: None,
            })
        }
        (true, None) => Err("nothing to do: give a message or --history".into()),
        (false, Some(_)) => Err("--history and a message are mutually exclusive".into()),
    }
}

/// Parse `foreman status` args: `[--project P]`. No env default — bare
/// `status` deliberately lists ALL projects (see [`StatusRequest`]).
pub fn parse_status_args(args: &[String]) -> Result<StatusRequest, String> {
    let mut project = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    Ok(StatusRequest {
        cmd: "status".into(),
        project,
    })
}

/// Parse `foreman close` args: `[tN ...] [--project P]`. No ids = self-close
/// (see [`CloseRequest`]): requires BOTH env vars and refuses an explicit
/// `--project` (a bare close must mean "my own pane", never a guess into
/// another project).
pub fn parse_close_args(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
) -> Result<CloseRequest, String> {
    let mut project = None;
    let mut explicit_project = false;
    let mut terminals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                explicit_project = true;
                i += 2;
            }
            t if t
                .strip_prefix('t')
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) =>
            {
                terminals.push(t.to_string());
                i += 1;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("bad terminal id: {other} (expected tN)")),
        }
    }
    if terminals.is_empty() {
        // self-close: the caller's own identity, never a cross-project guess
        if explicit_project {
            return Err(
                "--project needs explicit terminal ids; bare close closes your own terminal".into(),
            );
        }
        let me =
            self_terminal.ok_or("not inside a foreman terminal (FOREMAN_TERMINAL_ID unset)")?;
        let proj = default_project.ok_or(
            "cannot resolve your own pane without FOREMAN_PROJECT_ID (terminal ids are only unique within a project)",
        )?;
        return Ok(CloseRequest {
            cmd: "close".into(),
            project: Some(proj),
            terminals: vec![me],
        });
    }
    Ok(CloseRequest {
        cmd: "close".into(),
        project: if explicit_project {
            project
        } else {
            default_project
        },
        terminals,
    })
}

/// Parse `foreman send` args: `[--project P] [--terminal T] [--text TXT]
/// [--keys "K K …"]... [--settle-ms N]`. `--keys` splits its value on
/// whitespace; repeatable `--keys` appends. When `--terminal` is absent,
/// fills from `self_terminal` (FOREMAN_TERMINAL_ID) and requires
/// `self_project` (FOREMAN_PROJECT_ID) — same self-target rule as `close`;
/// an explicit `--project` then errors (terminal ids are only unique within
/// a project, so it would be a cross-project guess).
/// Requires at least one of `--text` or `--keys`.
/// `settle_ms` defers the reply until the screen quiets; see `wm::advance_settles`.
pub fn parse_send_args(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
    self_project: Option<String>,
) -> Result<SendRequest, String> {
    let mut project = default_project;
    let mut explicit_project = false;
    let mut terminal: Option<String> = None;
    let mut text: Option<String> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut settle_ms: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                explicit_project = true;
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
        // self-target: the caller's own identity, never a cross-project guess
        if explicit_project {
            return Err(
                "--project needs an explicit --terminal; bare send targets your own pane".into(),
            );
        }
        let me =
            self_terminal.ok_or("not inside a foreman terminal (FOREMAN_TERMINAL_ID unset)")?;
        let proj = self_project.ok_or(
            "cannot resolve your own pane without FOREMAN_PROJECT_ID (terminal ids are only unique within a project)",
        )?;
        terminal = Some(me);
        project = Some(proj);
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

/// Parse `foreman snapshot` args: `[--project P] [--terminal T] [--attrs]
/// [--cursor] [--tail N]`. When `--terminal` is absent, fills from `self_terminal`
/// (FOREMAN_TERMINAL_ID) and requires FOREMAN_PROJECT_ID (`default_project`);
/// an explicit `--project` then errors (terminal ids are only unique within
/// a project, so it would be a cross-project guess).
/// `--attrs`/`--cursor` are valueless boolean opt-ins. `--tail N` is the last
/// N lines of the buffer (scrollback included), not the displayed viewport.
pub fn parse_snapshot_args(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
) -> Result<SnapshotRequest, String> {
    let mut project = default_project;
    let mut explicit_project = false;
    let mut terminal: Option<String> = None;
    let mut attrs = false;
    let mut cursor = false;
    let mut tail: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                explicit_project = true;
                i += 2;
            }
            "--terminal" => {
                terminal = Some(args.get(i + 1).ok_or("--terminal needs a value")?.clone());
                i += 2;
            }
            "--attrs" => {
                attrs = true;
                i += 1;
            }
            "--cursor" => {
                cursor = true;
                i += 1;
            }
            "--tail" => {
                let raw = args.get(i + 1).ok_or("--tail needs a positive integer")?;
                let n: usize = raw
                    .parse()
                    .map_err(|_| "--tail needs a positive integer".to_string())?;
                if n == 0 {
                    return Err("--tail needs a positive integer".into());
                }
                tail = Some(n);
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    // An explicit --project with no --terminal is never a self-target: terminal
    // ids are only unique within a project, so filling the terminal from the
    // caller's env would silently cross into another project's pane.
    if terminal.is_none() && explicit_project {
        return Err(
            "--project needs an explicit --terminal; bare snapshot reads your own pane".into(),
        );
    }
    // Self-target only when we actually have a FOREMAN_TERMINAL_ID; otherwise fall
    // through to the clear "--terminal is required" error.
    if terminal.is_none() && self_terminal.is_some() {
        let proj = project.ok_or(
            "cannot resolve your own pane without FOREMAN_PROJECT_ID (terminal ids are only unique within a project)",
        )?;
        terminal = self_terminal;
        project = Some(proj);
    }
    let terminal = terminal.ok_or(
        "--terminal is required (or run inside a foreman terminal to target your own pane)",
    )?;
    Ok(SnapshotRequest {
        cmd: "snapshot".into(),
        project,
        terminal: Some(terminal),
        attrs,
        cursor,
        tail,
    })
}

/// Parse `foreman view` args: `[--project P] <path.png>`. Resolves the path to
/// an absolute, canonicalized form CLIENT-side (the GUI may run with a
/// different cwd) and strips Windows' `\\?\` verbatim prefix so the stored/
/// displayed path stays a normal one. Cheap validation happens here too — a
/// nonexistent file or non-`.png` extension exits 2 before touching the pipe;
/// the GUI still degrades gracefully on any later decode failure regardless.
pub fn parse_view_args(
    args: &[String],
    default_project: Option<String>,
) -> Result<ViewRequest, String> {
    let mut project = default_project;
    let mut path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => {
                if path.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                path = Some(other.to_string());
                i += 1;
            }
        }
    }
    let raw = path.ok_or("missing <path.png>")?;
    let is_png = std::path::Path::new(&raw)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    if !is_png {
        return Err(format!("not a .png file: {raw}"));
    }
    let abs = std::fs::canonicalize(&raw).map_err(|e| format!("cannot find {raw}: {e}"))?;
    let display = abs.display().to_string();
    let display = display
        .strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or(display);
    Ok(ViewRequest {
        cmd: "view".into(),
        project,
        path: display,
    })
}

/// Result of parsing `foreman kanban <action> ...`. `wait` never becomes a
/// wire request — it drives a client-side poll loop (see `kanban_wait`)
/// against repeated `list` requests, so the server never blocks a connection
/// open (the pipe server is serial; a held connection would wedge dispatch
/// for every other agent).
#[derive(Debug)]
pub enum KanbanAction {
    Request(KanbanRequest),
    Wait {
        project: Option<String>,
        target: crate::kanban::WaitTarget,
        timeout: Option<u64>,
    },
}

/// Parse `foreman kanban <action> ...` per the spec's closed verb table.
/// Every action accepts `--project P`, overriding `default_project`
/// (FOREMAN_PROJECT_ID).
pub fn parse_kanban_args(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
) -> Result<KanbanAction, String> {
    let action = args.first().ok_or("missing kanban action")?.clone();
    let rest = &args[1..];
    match action.as_str() {
        "add" => parse_kanban_add(rest, default_project),
        "list" => parse_kanban_list(rest, default_project),
        "start" => parse_kanban_start(rest, default_project, self_terminal),
        "done" => parse_kanban_simple(rest, default_project, "done"),
        "rm" => parse_kanban_simple(rest, default_project, "rm"),
        "block" => parse_kanban_block(rest, default_project),
        "wait" => parse_kanban_wait(rest, default_project),
        other => Err(format!("unknown kanban action: {other}")),
    }
}

/// `add <title words...> [--body B] [--project P]` — positional words join
/// (space-separated) into the title.
fn parse_kanban_add(
    args: &[String],
    default_project: Option<String>,
) -> Result<KanbanAction, String> {
    let mut project = default_project;
    let mut body: Option<String> = None;
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            "--body" => {
                body = Some(args.get(i + 1).ok_or("--body needs a value")?.clone());
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => {
                words.push(other.to_string());
                i += 1;
            }
        }
    }
    let title = words.join(" ");
    if title.trim().is_empty() {
        return Err("title cannot be empty".into());
    }
    Ok(KanbanAction::Request(KanbanRequest {
        cmd: "kanban".into(),
        action: "add".into(),
        project,
        title: Some(title),
        body,
        ..Default::default()
    }))
}

/// `list [--state backlog|in_progress|blocked|done] [--json] [--project P]`.
fn parse_kanban_list(
    args: &[String],
    default_project: Option<String>,
) -> Result<KanbanAction, String> {
    let mut project = default_project;
    let mut state: Option<String> = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            "--state" => {
                let v = args.get(i + 1).ok_or("--state needs a value")?.clone();
                if !matches!(v.as_str(), "backlog" | "in_progress" | "blocked" | "done") {
                    return Err(format!(
                        "bad --state value: {v} (expected backlog|in_progress|blocked|done)"
                    ));
                }
                state = Some(v);
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    Ok(KanbanAction::Request(KanbanRequest {
        cmd: "kanban".into(),
        action: "list".into(),
        project,
        state,
        json,
        ..Default::default()
    }))
}

/// One positional `<id>` plus `[--project P]` — shared by `start`, `done`,
/// and `rm`. Any other flag is an error (no other verb-specific flags exist
/// for these three actions).
fn parse_kanban_id_and_project(
    args: &[String],
    default_project: Option<String>,
) -> Result<(String, Option<String>), String> {
    let mut project = default_project;
    let mut id: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => {
                if id.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                id = Some(other.to_string());
                i += 1;
            }
        }
    }
    let id = id.ok_or("missing <id>")?;
    Ok((id, project))
}

/// `start <id> [--project P]` — requires `self_terminal` (FOREMAN_TERMINAL_ID).
fn parse_kanban_start(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
) -> Result<KanbanAction, String> {
    let (id, project) = parse_kanban_id_and_project(args, default_project)?;
    let from = self_terminal.ok_or("not inside a foreman terminal (FOREMAN_TERMINAL_ID unset)")?;
    Ok(KanbanAction::Request(KanbanRequest {
        cmd: "kanban".into(),
        action: "start".into(),
        project,
        from: Some(from),
        id: Some(id),
        ..Default::default()
    }))
}

/// `done <id> [--project P]` / `rm <id> [--project P]`.
fn parse_kanban_simple(
    args: &[String],
    default_project: Option<String>,
    action: &str,
) -> Result<KanbanAction, String> {
    let (id, project) = parse_kanban_id_and_project(args, default_project)?;
    Ok(KanbanAction::Request(KanbanRequest {
        cmd: "kanban".into(),
        action: action.into(),
        project,
        id: Some(id),
        ..Default::default()
    }))
}

/// `block <id> --reason R [--project P]` — `--reason` is mandatory and
/// non-empty (a Blocked column without reasons costs the human an
/// investigation per card).
fn parse_kanban_block(
    args: &[String],
    default_project: Option<String>,
) -> Result<KanbanAction, String> {
    let mut project = default_project;
    let mut id: Option<String> = None;
    let mut reason: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            "--reason" => {
                reason = Some(args.get(i + 1).ok_or("--reason needs a value")?.clone());
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => {
                if id.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                id = Some(other.to_string());
                i += 1;
            }
        }
    }
    let id = id.ok_or("missing <id>")?;
    let reason = reason.ok_or("--reason is required")?;
    if reason.trim().is_empty() {
        return Err("--reason cannot be empty".into());
    }
    Ok(KanbanAction::Request(KanbanRequest {
        cmd: "kanban".into(),
        action: "block".into(),
        project,
        id: Some(id),
        reason: Some(reason),
        ..Default::default()
    }))
}

/// `wait <id>` or `wait --any`, `[--timeout SECS] [--project P]` — exactly
/// one of `<id>` / `--any`.
fn parse_kanban_wait(
    args: &[String],
    default_project: Option<String>,
) -> Result<KanbanAction, String> {
    let mut project = default_project;
    let mut id: Option<String> = None;
    let mut any = false;
    let mut timeout: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            "--any" => {
                any = true;
                i += 1;
            }
            "--timeout" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--timeout needs a number of seconds")?;
                timeout = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("--timeout needs a number, got: {v}"))?,
                );
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => {
                if id.is_some() {
                    return Err(format!("unexpected argument: {other}"));
                }
                id = Some(other.to_string());
                i += 1;
            }
        }
    }
    let target = match (id, any) {
        (Some(_), true) => return Err("give either <id> or --any, not both".into()),
        (Some(id), false) => crate::kanban::WaitTarget::Id(id),
        (None, true) => crate::kanban::WaitTarget::Any,
        (None, false) => return Err("wait needs <id> or --any".into()),
    };
    Ok(KanbanAction::Wait {
        project,
        target,
        timeout,
    })
}

const HELP: &str = "\
foreman — a desktop for running fleets of AI-agent terminals

USAGE
  foreman                                   launch the GUI (no arguments)
  foreman open [flags] -- <command...>      spawn a command in a new visible terminal
  foreman chat [flags] [--] <message...>    post to the project chat room
  foreman chat [--project P] --history [N]  read the last N room lines (default 20)
  foreman status [--project P]              list projects + terminals (running/exited)
  foreman close [tN ...] [--project P]      close terminals (no ids: your own pane)
  foreman send [flags] --text TXT / --keys \"K...\"  drive input into a terminal
  foreman snapshot [--project P] [--terminal T] [--tail N]  read viewport or last N buffer lines
  foreman icat <file.png> [--cols N]        print an image into this pane (kitty graphics)
  foreman view <file.png> [--project P]     open a persistent image-viewer window
  foreman kanban <action> ...                add/list/start/done/block/rm/wait a card
  foreman help | --help | -h                this text (also: open --help, chat --help,
                                            status --help, close --help, send --help,
                                            snapshot --help, view --help, kanban --help)

Subcommands talk to the RUNNING foreman instance over its control pipe; they
print a JSON reply on stdout and exit 0 (ok), 1 (foreman refused or is
unreachable), or 2 (bad arguments).

ENVIRONMENT (injected into every foreman-spawned terminal)
  FOREMAN=1            you are inside a foreman terminal
  FOREMAN_EXE          path to this binary — dispatch via & $env:FOREMAN_EXE
  FOREMAN_PROJECT_ID   your project (the default for open/chat)
  FOREMAN_TERMINAL_ID  your terminal id (the chat sender; required to post)";

const HELP_OPEN: &str = "\
foreman open [--project P] [--title T] [--cwd D] -- <command...>

Spawn <command...> in a new visible terminal of project P (default: your
FOREMAN_PROJECT_ID, else the focused project). Everything after -- is passed
per-argument; nothing is shell-interpreted. Reply on stdout:
  {\"ok\":true,\"terminal\":\"tN\",\"project\":\"pN\"}
Ids are assigned by foreman — record \"terminal\", it is how the worker is
addressed in chat. The reply is NOT the command's output or result.
A target that is a .cmd/.bat shim (npm installs) cannot receive newlines
or \" in arguments — foreman refuses such dispatches loudly.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";

const HELP_CHAT: &str = "\
foreman chat [--project P] [--to T]... [--re N] [--] <message...>
foreman chat [--project P] --history [N]

Post <message...> to project P's chat room (default: FOREMAN_PROJECT_ID), or
read the last N lines (default 20). Flags come first; the first non-flag word
starts the message. Posting requires FOREMAN_TERMINAL_ID (be inside a foreman
terminal); --history works for any caller and never joins the room.
  --to tN|you   deliver-interrupt only those members (repeatable); a leading
                @tN/@you run in the message does the same
  --re N        mark the post as a reply to room seq N
  --            end flag parsing (post a message that starts with -)
Replies: a post prints {\"ok\":true,\"seq\":N}; history prints line per line.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";

const HELP_STATUS: &str = "\
foreman status [--project P]

List every project and its terminals, line per line:
  p1  myrepo  C:\\src\\myrepo
    t3  running  chat  agent · parser
    t5  exited(0)  -  cmd
Fields: id, state (running | exited(code)), chat membership (chat | -),
title. Terminal ids are unique only within their project. A worker that
spawned and instantly died shows exited(code) — status asks the live
process, not the pane title. No --project = all projects; \"no projects\"
when there are none. --project pN filters to one project (unknown pN is
an error).
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";

const HELP_CLOSE: &str = "\
foreman close [tN ...] [--project P]

Close terminal panes. With ids: close those terminals in project P
(default: your FOREMAN_PROJECT_ID, else the focused project). ANY unknown
or non-terminal id fails the whole request and nothing closes. With no
ids: close YOUR OWN pane (requires FOREMAN_TERMINAL_ID and
FOREMAN_PROJECT_ID; --project is not allowed) — closing kills the pane's
process tree, you included, so post any done-signal FIRST and do not
expect to see the reply. Reply: {\"ok\":true,\"project\":\"pN\"}.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";

const HELP_SEND: &str = "\
foreman send [--project P] [--terminal T] [--text TXT] [--keys \"K K …\"] [--settle-ms N]

Write input to terminal T (default: your own). --text is raw UTF-8 written
verbatim (\\r = Enter). --keys is a space-separated sequence of named key
presses encoded with the session's live TermMode. --text and --keys are
additive: text first, then keys. --settle-ms N waits for the screen to stay
quiet that long before replying. Reply: {\"ok\":true} or {\"ok\":false,\"error\":\"...\"}.
Key names: F1..F12, Up Down Left Right, Home End PageUp PageDown Insert
Delete, Enter Tab Esc Backspace Space, single letters; Ctrl+/Alt+/Shift+
prefixes combinable. Unknown name exits 2.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";

const HELP_SNAPSHOT: &str = "\
foreman snapshot [--project P] [--terminal T] [--attrs] [--cursor] [--tail N]

Read terminal T (default: your own).
Default: the currently displayed viewport as plain text rows in the history
field, one per visible row, trailing spaces trimmed, printed line per line.
--tail N reads the last N lines of the buffer (scrollback + live screen)
instead — viewport-only inspect silently drops long-build output.
N must be a positive integer; larger than the buffer returns the whole buffer.
Opt-in structured fields:
  --attrs   cells: per-cell fg/bg (RGB) + bold/italic/underline/strikethrough/
            inverse/dim/wide flags (same row span as the text).
  --cursor  cursor: {row, col, shape}.
With --attrs or --cursor the whole reply is printed as one JSON line instead.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";

const HELP_VIEW: &str = "\
foreman view <file.png> [--project P]

Open a persistent Content::Image window in project P (default: your
FOREMAN_PROJECT_ID, else the focused project) showing <file.png>. Unlike
icat (inline, ephemeral, scrolls away with the pane) this is a normal
window: tile/float/tab/close/minimize all work like any other pane, and it
restores across restarts (path only — zoom/pan reset).
PNG only in v1. The path is resolved to an absolute path before the request
is sent — a relative path is relative to THIS process's cwd, not the GUI's.
Reply: {\"ok\":true,\"terminal\":\"tN\",\"project\":\"pN\"}.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments (nonexistent file or
non-.png extension fail here, before touching the pipe).";

const HELP_KANBAN: &str = "\
foreman kanban add <title words...> [--body B] [--project P]
foreman kanban list [--state backlog|in_progress|blocked|done] [--json] [--project P]
foreman kanban start <id> [--project P]
foreman kanban done <id> [--project P]
foreman kanban block <id> --reason R [--project P]
foreman kanban rm <id> [--project P]
foreman kanban wait <id> [--timeout SECS] [--project P]
foreman kanban wait --any [--timeout SECS] [--project P]

Manage cards on project P's board (default: FOREMAN_PROJECT_ID, else the
focused project).
  add     positional words join into the title; --body attaches a longer
          description. Reply: {\"ok\":true,\"id\":\"a3f8k2\"}.
  list    line per card by default (id, state, title, then a context tail —
          claim/blocked-reason/orphan marker). --json emits one JSON card
          object per line instead, each carrying a derived \"orphaned\" flag
          (true when the card's claim points at a Session that is gone —
          this flag is never stored in the card file itself).
  start   self-service claim: requires FOREMAN_TERMINAL_ID (be inside a
          foreman terminal). Errors if another live Session already holds
          the card; succeeds and seizes an orphaned claim.
  done    InProgress -> Done. Errors on any other state or a missing card
          (close-out never resurrects a card).
  block   InProgress -> Blocked; --reason is mandatory and non-empty.
  rm      delete the card's file, from any state.
  wait    poll (client-side, no pipe verb) until the card (or, with --any,
          any card seen InProgress) reaches Done, Blocked, or orphaned, or
          is removed. --timeout SECS bounds the wait.
Exit codes for add/list/start/done/block/rm: 0 ok, 1 refused/unreachable,
2 bad arguments.
Exit codes for wait: 0 the watched card finished (Done), 1 it needs a human
(Blocked, orphaned, or removed), 2 timeout or foreman unreachable — a
different unreachable code than the other verbs, since a stuck wait must be
distinguishable from a stuck board.";

/// Subcommand entry point (no GUI). Returns the process exit code.
pub fn client_main(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("open") => open_main(&args[1..]),
        Some("chat") => chat_main(&args[1..]),
        Some("status") => status_main(&args[1..]),
        Some("close") => close_main(&args[1..]),
        Some("send") => send_main(&args[1..]),
        Some("snapshot") => snapshot_main(&args[1..]),
        Some("icat") => crate::icat::icat_main(&args[1..]),
        Some("view") => view_main(&args[1..]),
        Some("kanban") => kanban_main(&args[1..]),
        Some("help" | "--help" | "-h") => {
            println!("{HELP}");
            0
        }
        _ => {
            eprintln!("usage: foreman open [--project P] [--title T] [--cwd D] -- <command...>");
            eprintln!("       foreman chat [--project P] [--to T]... [--re N] [--] <message...>");
            eprintln!("       foreman chat [--project P] --history [N]");
            eprintln!("       foreman status [--project P]");
            eprintln!("       foreman close [tN ...] [--project P]");
            eprintln!(
                "       foreman send [--project P] [--terminal T] --text TXT [--keys \"K\"] [--settle-ms N]"
            );
            eprintln!("       foreman snapshot [--project P] [--terminal T]");
            eprintln!("       foreman icat <file.png> [--cols N]");
            eprintln!("       foreman view <file.png> [--project P]");
            eprintln!("       foreman kanban <action> ...");
            eprintln!("       foreman help");
            2
        }
    }
}

fn open_main(args: &[String]) -> i32 {
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_OPEN}");
        return 0;
    }
    let req = match parse_open_args(args, std::env::var("FOREMAN_PROJECT_ID").ok()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman open: {e}");
            return 2;
        }
    };
    report("foreman open", request(&client_pipe(), &req))
}

fn chat_main(args: &[String]) -> i32 {
    // before env/parsing: help must work outside a foreman terminal
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_CHAT}");
        return 0;
    }
    let req = match parse_chat_args(
        args,
        std::env::var("FOREMAN_PROJECT_ID").ok(),
        std::env::var("FOREMAN_TERMINAL_ID").ok(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman chat: {e}");
            return 2;
        }
    };
    report("foreman chat", request(&client_pipe(), &req))
}

fn status_main(args: &[String]) -> i32 {
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_STATUS}");
        return 0;
    }
    let req = match parse_status_args(args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman status: {e}");
            return 2;
        }
    };
    report("foreman status", request(&client_pipe(), &req))
}

fn close_main(args: &[String]) -> i32 {
    // before env/parsing: help must work outside a foreman terminal
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_CLOSE}");
        return 0;
    }
    let req = match parse_close_args(
        args,
        std::env::var("FOREMAN_PROJECT_ID").ok(),
        std::env::var("FOREMAN_TERMINAL_ID").ok(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman close: {e}");
            return 2;
        }
    };
    report("foreman close", request(&client_pipe(), &req))
}

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
    report("foreman send", request(&client_pipe(), &req))
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
    report("foreman snapshot", request(&client_pipe(), &req))
}

fn view_main(args: &[String]) -> i32 {
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_VIEW}");
        return 0;
    }
    let req = match parse_view_args(args, std::env::var("FOREMAN_PROJECT_ID").ok()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman view: {e}");
            return 2;
        }
    };
    report("foreman view", request(&client_pipe(), &req))
}

fn kanban_main(args: &[String]) -> i32 {
    // before env/parsing: help must work outside a foreman terminal
    if let Some("--help" | "-h") = args.first().map(String::as_str) {
        println!("{HELP_KANBAN}");
        return 0;
    }
    let action = match parse_kanban_args(
        args,
        std::env::var("FOREMAN_PROJECT_ID").ok(),
        std::env::var("FOREMAN_TERMINAL_ID").ok(),
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("foreman kanban: {e}");
            return 2;
        }
    };
    match action {
        // `id` rides the JSON ok-reply that `report` already prints; `list`
        // output rides `history` and prints line-per-line.
        KanbanAction::Request(req) => {
            let res = request(&client_pipe(), &req).map(|mut r| {
                if let Some(e) = r.error.take() {
                    r.error = Some(kanban_stale_host_hint(e));
                }
                r
            });
            report("foreman kanban", res)
        }
        KanbanAction::Wait {
            project,
            target,
            timeout,
        } => kanban_wait(project, target, timeout),
    }
}

/// An out-of-date foreman host answers the kanban verb with its generic
/// unknown-cmd parse error. The newer client knows what that actually means —
/// the host process predates the verb (e.g. a dev fleet whose GUI was launched
/// before a rebuild) — so it says that instead of letting the operator chase a
/// phantom CLI bug. Client-side only: old hosts cannot be taught new errors.
fn kanban_stale_host_hint(err: String) -> String {
    if err.starts_with("unknown cmd") {
        format!(
            "{err} — the running foreman predates the kanban verb; restart it on the current build"
        )
    } else {
        err
    }
}

/// Client-side poll loop for `foreman kanban wait`: repeated `list --json`
/// requests, verdict decided by the pure [`crate::kanban::wait_verdict`].
/// Never a held pipe connection — the server is serial, so a blocking wait
/// would wedge dispatch for every other agent.
fn kanban_wait(
    project: Option<String>,
    target: crate::kanban::WaitTarget,
    timeout: Option<u64>,
) -> i32 {
    let deadline = timeout.map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));
    let mut watched = std::collections::HashSet::new();
    loop {
        let req = KanbanRequest {
            cmd: "kanban".into(),
            action: "list".into(),
            project: project.clone(),
            json: true,
            ..Default::default()
        };
        match request(&client_pipe(), &req) {
            // Spec exit-code contract: 2 = timeout or foreman unreachable
            // (deliberately different from other verbs' unreachable=1).
            Err(e) => {
                eprintln!("foreman kanban wait: cannot reach foreman ({e})");
                return 2;
            }
            Ok(r) if !r.ok => {
                eprintln!(
                    "foreman kanban wait: {}",
                    kanban_stale_host_hint(r.error.unwrap_or_default())
                );
                return 1;
            }
            Ok(r) => {
                let cards: Vec<crate::kanban::CardLine> = r
                    .history
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                if let Some(code) = crate::kanban::wait_verdict(&target, &mut watched, &cards) {
                    return code;
                }
            }
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            eprintln!("foreman kanban wait: timeout");
            return 2;
        }
        std::thread::sleep(std::time::Duration::from_secs(2)); // client-side poll: the pipe
        // server is serial; a held connection would wedge dispatch for every agent (spec).
    }
}

/// Print the pipe reply (or the connection failure) the way all subcommands do.
/// History replies print line-per-line for agent readability; other ok replies
/// print as JSON (the open reply carries terminal/project ids the caller needs).
fn report(label: &str, res: std::io::Result<OpenReply>) -> i32 {
    match res {
        Ok(r) if r.ok => {
            if r.cells.is_some() || r.cursor.is_some() {
                // Structured snapshot (--attrs/--cursor): emit the whole reply as
                // JSON so the caller gets the cells/cursor payload, not just text.
                println!("{}", serde_json::to_string(&r).unwrap_or_default());
            } else if let Some(lines) = &r.history {
                for l in lines {
                    println!("{l}");
                }
            } else {
                println!("{}", serde_json::to_string(&r).unwrap_or_default());
            }
            0
        }
        Ok(r) => {
            eprintln!("{label}: {}", r.error.unwrap_or_default());
            1
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            eprintln!(
                "{label}: foreman is running but its control pipe stayed busy for {}s — retry, or check for a wedged dispatch",
                CONNECT_TIMEOUT.as_secs()
            );
            1
        }
        Err(e) => {
            eprintln!("{label}: cannot reach foreman ({e}) — is it running?");
            1
        }
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
        let ok = OpenReply {
            ok: true,
            terminal: Some("t4".into()),
            project: Some("p1".into()),
            ..Default::default()
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(!s.contains("error"));
        assert!(!s.contains("seq"));
        assert!(!s.contains("cells"));
        assert!(!s.contains("cursor"));
        assert_eq!(serde_json::from_str::<OpenReply>(&s).unwrap(), ok);
        assert_eq!(OpenReply::err("boom").error.as_deref(), Some("boom"));
    }

    #[test]
    fn open_reply_id_is_wire_compatible_with_v1() {
        // unset id serializes away (byte-identical to v1)…
        let ok = OpenReply {
            ok: true,
            ..Default::default()
        };
        assert!(!serde_json::to_string(&ok).unwrap().contains("\"id\""));
        // …and a v1 reply without the key still parses.
        let r: OpenReply = serde_json::from_str(r#"{"ok":true}"#).unwrap();
        assert_eq!(r.id, None);
    }

    #[test]
    fn kanban_request_wire_roundtrips_and_omits_unset_fields() {
        let req = KanbanRequest {
            cmd: "kanban".into(),
            action: "list".into(),
            ..Default::default()
        };
        let j = serde_json::to_string(&req).unwrap();
        assert!(!j.contains("\"id\"") && !j.contains("\"title\"") && !j.contains("\"json\""));
        let back: KanbanRequest = serde_json::from_str(&j).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn client_pipe_prefers_injected_name_and_falls_back_when_blank_or_unset() {
        assert_eq!(
            pipe_for(Some("foreman-ctl-7-42".into())),
            "foreman-ctl-7-42"
        );
        assert_eq!(pipe_for(None), PIPE);
        assert_eq!(pipe_for(Some("  ".into())), PIPE);
    }

    #[test]
    fn instance_pipe_is_stable_within_the_process_and_never_the_shared_name() {
        assert!(instance_pipe().starts_with("foreman-ctl-"));
        assert_eq!(instance_pipe(), instance_pipe());
        assert_ne!(instance_pipe(), PIPE);
    }

    #[test]
    fn stale_host_unknown_cmd_error_gains_a_restart_hint_others_pass_through() {
        let hinted = kanban_stale_host_hint("unknown cmd: kanban".into());
        assert!(hinted.starts_with("unknown cmd: kanban"));
        assert!(hinted.contains("restart it on the current build"));
        // Any other server error is real and not ours to editorialize.
        assert_eq!(
            kanban_stale_host_hint("no such card: x".into()),
            "no such card: x"
        );
    }

    #[test]
    fn parse_kanban_args_add_joins_title_words_and_takes_body() {
        let req = match parse_kanban_args(
            &s(&["add", "fix", "resize", "flicker", "--body", "detail"]),
            Some("p1".into()),
            None,
        )
        .unwrap()
        {
            KanbanAction::Request(r) => r,
            _ => panic!("expected a request"),
        };
        assert_eq!(req.action, "add");
        assert_eq!(req.title.as_deref(), Some("fix resize flicker"));
        assert_eq!(req.body.as_deref(), Some("detail"));
        assert_eq!(req.project.as_deref(), Some("p1"));
    }

    #[test]
    fn parse_kanban_args_add_rejects_empty_title() {
        assert!(parse_kanban_args(&s(&["add"]), None, None).is_err());
        assert!(parse_kanban_args(&s(&["add", "--body", "x"]), None, None).is_err());
    }

    #[test]
    fn parse_kanban_args_list_happy_path_and_bad_state() {
        let req = match parse_kanban_args(&s(&["list", "--state", "blocked", "--json"]), None, None)
            .unwrap()
        {
            KanbanAction::Request(r) => r,
            _ => panic!("expected a request"),
        };
        assert_eq!(req.action, "list");
        assert_eq!(req.state.as_deref(), Some("blocked"));
        assert!(req.json);
        let e = parse_kanban_args(&s(&["list", "--state", "bogus"]), None, None).unwrap_err();
        assert!(e.contains("bogus"), "{e}");
    }

    #[test]
    fn parse_kanban_args_start_requires_self_terminal() {
        let req =
            match parse_kanban_args(&s(&["start", "a1"]), Some("p1".into()), Some("t4".into()))
                .unwrap()
            {
                KanbanAction::Request(r) => r,
                _ => panic!("expected a request"),
            };
        assert_eq!(req.action, "start");
        assert_eq!(req.id.as_deref(), Some("a1"));
        assert_eq!(req.from.as_deref(), Some("t4"));
        let e = parse_kanban_args(&s(&["start", "a1"]), None, None).unwrap_err();
        assert!(e.contains("FOREMAN_TERMINAL_ID"), "{e}");
    }

    #[test]
    fn parse_kanban_args_done_and_rm_take_one_id() {
        let req = match parse_kanban_args(&s(&["done", "a1"]), None, None).unwrap() {
            KanbanAction::Request(r) => r,
            _ => panic!("expected a request"),
        };
        assert_eq!(req.action, "done");
        assert_eq!(req.id.as_deref(), Some("a1"));

        let req = match parse_kanban_args(&s(&["rm", "a1"]), None, None).unwrap() {
            KanbanAction::Request(r) => r,
            _ => panic!("expected a request"),
        };
        assert_eq!(req.action, "rm");
        assert_eq!(req.id.as_deref(), Some("a1"));

        assert!(parse_kanban_args(&s(&["done"]), None, None).is_err());
    }

    #[test]
    fn parse_kanban_args_block_demands_a_reason() {
        let req = match parse_kanban_args(&s(&["block", "a1", "--reason", "waiting"]), None, None)
            .unwrap()
        {
            KanbanAction::Request(r) => r,
            _ => panic!("expected a request"),
        };
        assert_eq!(req.action, "block");
        assert_eq!(req.reason.as_deref(), Some("waiting"));
        let e = parse_kanban_args(&s(&["block", "a1"]), None, None).unwrap_err();
        assert!(e.contains("--reason"), "{e}");
        let e = parse_kanban_args(&s(&["block", "a1", "--reason", ""]), None, None).unwrap_err();
        assert!(e.contains("--reason"), "{e}");
    }

    #[test]
    fn parse_kanban_args_wait_needs_exactly_one_of_id_or_any() {
        let target =
            match parse_kanban_args(&s(&["wait", "a1", "--timeout", "30"]), None, None).unwrap() {
                KanbanAction::Wait {
                    target, timeout, ..
                } => {
                    assert_eq!(timeout, Some(30));
                    target
                }
                _ => panic!("expected a wait"),
            };
        assert_eq!(target, crate::kanban::WaitTarget::Id("a1".into()));

        let target = match parse_kanban_args(&s(&["wait", "--any"]), None, None).unwrap() {
            KanbanAction::Wait { target, .. } => target,
            _ => panic!("expected a wait"),
        };
        assert_eq!(target, crate::kanban::WaitTarget::Any);

        assert!(parse_kanban_args(&s(&["wait", "a1", "--any"]), None, None).is_err());
        assert!(parse_kanban_args(&s(&["wait"]), None, None).is_err());
    }

    #[test]
    fn parse_kanban_args_rejects_unknown_action_and_flags() {
        let e = parse_kanban_args(&s(&["bogus"]), None, None).unwrap_err();
        assert!(e.contains("bogus"), "{e}");
        let e = parse_kanban_args(&s(&["list", "--nope"]), None, None).unwrap_err();
        assert!(e.contains("--nope"), "{e}");
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn help_prints_and_exits_zero_everywhere() {
        // top level: --help / -h / help
        assert_eq!(client_main(&s(&["--help"])), 0);
        assert_eq!(client_main(&s(&["-h"])), 0);
        assert_eq!(client_main(&s(&["help"])), 0);
        // per verb, first argument — must work OUTSIDE a foreman terminal
        // (no FOREMAN_* env), so the check precedes env reads and parsing
        assert_eq!(client_main(&s(&["open", "--help"])), 0);
        assert_eq!(client_main(&s(&["open", "-h"])), 0);
        assert_eq!(client_main(&s(&["chat", "--help"])), 0);
        assert_eq!(client_main(&s(&["chat", "-h"])), 0);
        assert_eq!(client_main(&s(&["status", "--help"])), 0);
        assert_eq!(client_main(&s(&["status", "-h"])), 0);
        assert_eq!(client_main(&s(&["close", "--help"])), 0);
        assert_eq!(client_main(&s(&["close", "-h"])), 0);
        assert_eq!(client_main(&s(&["send", "--help"])), 0);
        assert_eq!(client_main(&s(&["send", "-h"])), 0);
        assert_eq!(client_main(&s(&["snapshot", "--help"])), 0);
        assert_eq!(client_main(&s(&["snapshot", "-h"])), 0);
        assert_eq!(client_main(&s(&["kanban", "--help"])), 0);
        assert_eq!(client_main(&s(&["kanban", "-h"])), 0);
    }

    #[test]
    fn parse_close_args_resolves_self_from_env() {
        // bare close = self-close: both env vars required
        let req = parse_close_args(&s(&[]), Some("p1".into()), Some("t4".into())).unwrap();
        assert_eq!(req.terminals, vec!["t4"]);
        assert_eq!(req.project.as_deref(), Some("p1"));
        // missing project env: refuse (tN is only unique within its project)
        let e = parse_close_args(&s(&[]), None, Some("t4".into())).unwrap_err();
        assert!(e.contains("FOREMAN_PROJECT_ID"), "{e}");
        // missing terminal env: refuse
        let e = parse_close_args(&s(&[]), Some("p1".into()), None).unwrap_err();
        assert!(e.contains("FOREMAN_TERMINAL_ID"), "{e}");
        // bare close with an explicit --project is a client error
        assert!(
            parse_close_args(
                &s(&["--project", "p2"]),
                Some("p1".into()),
                Some("t4".into())
            )
            .is_err()
        );
    }

    #[test]
    fn parse_close_args_collects_ids_and_project() {
        let req =
            parse_close_args(&s(&["t3", "t5"]), Some("p1".into()), Some("t4".into())).unwrap();
        assert_eq!(req.terminals, vec!["t3", "t5"]);
        assert_eq!(req.project.as_deref(), Some("p1"));
        // explicit --project beats the env default
        let req = parse_close_args(
            &s(&["--project", "p2", "t3"]),
            Some("p1".into()),
            Some("t4".into()),
        )
        .unwrap();
        assert_eq!(req.project.as_deref(), Some("p2"));
        // ids with no env at all: ok, server resolves the focused project
        let req = parse_close_args(&s(&["t3"]), None, None).unwrap();
        assert_eq!(req.project, None);
        assert_eq!(req.terminals, vec!["t3"]);
    }

    #[test]
    fn parse_close_args_rejects_bad_input() {
        let e = parse_close_args(&s(&["bogus"]), None, None).unwrap_err();
        assert!(e.contains("bogus"), "{e}");
        let e = parse_close_args(&s(&["t"]), None, None).unwrap_err();
        assert!(e.contains("bad terminal id: t "), "{e}");
        assert!(parse_close_args(&s(&["--project"]), None, None).is_err());
        let e = parse_close_args(&s(&["--nope", "t3"]), None, None).unwrap_err();
        assert!(e.contains("--nope"), "{e}");
    }

    #[test]
    fn close_request_wire_roundtrips() {
        let req = CloseRequest {
            cmd: "close".into(),
            project: Some("p1".into()),
            terminals: vec!["t3".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""terminals":["t3"]"#), "{json}");
        let back: CloseRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn close_pipe_roundtrip() {
        let pipe = format!("foreman-test-close-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Close(req, reply, _) => {
                    assert_eq!(req.terminals, vec!["t3"]);
                    let _ = reply.send(OpenReply {
                        ok: true,
                        project: Some("p1".into()),
                        ..Default::default()
                    });
                }
                _ => panic!("expected CtrlMsg::Close"),
            }
        });
        let req = CloseRequest {
            cmd: "close".into(),
            project: None,
            terminals: vec!["t3".into()],
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
        let reply = reply.expect("no reply");
        assert!(reply.ok);
        assert_eq!(reply.project.as_deref(), Some("p1"));
    }

    #[test]
    fn kanban_pipe_roundtrip() {
        let pipe = format!("foreman-test-kanban-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Kanban(req, reply, _) => {
                    assert_eq!(req.action, "add");
                    assert_eq!(req.title.as_deref(), Some("fix resize flicker"));
                    let _ = reply.send(OpenReply {
                        ok: true,
                        id: Some("a3f8k2".into()),
                        ..Default::default()
                    });
                }
                _ => panic!("expected CtrlMsg::Kanban"),
            }
        });
        let req = KanbanRequest {
            cmd: "kanban".into(),
            action: "add".into(),
            title: Some("fix resize flicker".into()),
            ..Default::default()
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
        let reply = reply.expect("no reply");
        assert!(reply.ok);
        assert_eq!(reply.id.as_deref(), Some("a3f8k2"));
    }

    #[test]
    fn listen_retry_wins_the_pipe_after_the_holder_exits() {
        let pipe = format!("foreman-test-retry-{}", std::process::id());
        let name = pipe.clone().to_ns_name::<GenericNamespaced>().unwrap();
        let holder = ListenerOptions::new().name(name).create_sync().unwrap();
        let p2 = pipe.clone();
        let t = std::thread::spawn(move || {
            let name = p2.to_ns_name::<GenericNamespaced>().unwrap();
            listen_retry(name, 20, std::time::Duration::from_millis(50)).is_some()
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(holder);
        assert!(
            t.join().unwrap(),
            "retry must acquire the pipe once the holder is gone"
        );
    }

    #[test]
    fn listen_retry_gives_up_when_the_pipe_stays_held() {
        let pipe = format!("foreman-test-retry-held-{}", std::process::id());
        let name = pipe.clone().to_ns_name::<GenericNamespaced>().unwrap();
        let _holder = ListenerOptions::new().name(name).create_sync().unwrap();
        let name2 = pipe.to_ns_name::<GenericNamespaced>().unwrap();
        assert!(listen_retry(name2, 3, std::time::Duration::from_millis(20)).is_none());
    }

    #[test]
    fn parse_status_args_accepts_optional_project() {
        // bare status = ALL projects, no env/focused fallback
        let req = parse_status_args(&s(&[])).unwrap();
        assert_eq!(req.project, None);
        let req = parse_status_args(&s(&["--project", "p2"])).unwrap();
        assert_eq!(req.project.as_deref(), Some("p2"));
        // flag without value
        assert!(parse_status_args(&s(&["--project"])).is_err());
        // stray positional names the token
        let e = parse_status_args(&s(&["p1"])).unwrap_err();
        assert!(e.contains("p1"), "{e}");
        // unknown flag
        assert!(parse_status_args(&s(&["--nope"])).is_err());
    }

    #[test]
    fn status_request_wire_roundtrips() {
        let req: StatusRequest = serde_json::from_str(r#"{"cmd":"status"}"#).unwrap();
        assert_eq!(req.cmd, "status");
        assert_eq!(req.project, None);
        let req = StatusRequest {
            cmd: "status".into(),
            project: Some("p2".into()),
        };
        let back: StatusRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn status_pipe_roundtrip() {
        let pipe = format!("foreman-test-status-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Status(req, reply, _) => {
                    assert_eq!(req.project, None);
                    let _ = reply.send(OpenReply {
                        ok: true,
                        history: Some(vec!["p1  proj  -".into()]),
                        ..Default::default()
                    });
                }
                _ => panic!("expected CtrlMsg::Status"),
            }
        });
        let req = StatusRequest {
            cmd: "status".into(),
            project: None,
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
        let reply = reply.expect("no reply");
        assert!(reply.ok);
        assert_eq!(
            reply.history.as_deref(),
            Some(&["p1  proj  -".to_string()][..])
        );
    }

    #[test]
    fn help_is_not_swallowed_as_message_text() {
        // a message that IS "--help" still posts via the -- escape
        let req = parse_chat_args(&s(&["--", "--help"]), None, Some("t2".into())).unwrap();
        assert_eq!(req.text.as_deref(), Some("--help"));
    }

    #[test]
    fn parse_full_flags() {
        let req = parse_open_args(
            &s(&[
                "--project",
                "p2",
                "--title",
                "agent · t",
                "--cwd",
                "H:\\x",
                "--",
                "claude",
                "-p",
                "task",
            ]),
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
        let req =
            parse_open_args(&s(&["--project", "p1", "--", "cmd.exe"]), Some("p7".into())).unwrap();
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

    #[test]
    fn request_to_missing_pipe_fails_fast() {
        // No pipe with this name exists: the client must error immediately
        // (ERROR_FILE_NOT_FOUND), not sit out the busy-pipe connect deadline.
        let req = OpenRequest {
            cmd: "open".into(),
            project: None,
            cwd: None,
            title: None,
            command: vec!["x".into()],
        };
        let t0 = std::time::Instant::now();
        let r = request(
            &format!("foreman-test-missing-{}", std::process::id()),
            &req,
        );
        assert!(r.is_err());
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(2),
            "missing pipe must fail fast, not wait CONNECT_TIMEOUT"
        );
    }

    #[test]
    fn unknown_verb_is_rejected() {
        let pipe = format!("foreman-test-verb-{}", std::process::id());
        let (tx, _rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        let req = OpenRequest {
            cmd: "frobnicate".into(),
            project: None,
            cwd: None,
            title: None,
            command: vec!["x".into()],
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
        let reply = reply.expect("no reply");
        assert!(!reply.ok);
        assert!(reply.error.unwrap().contains("unknown cmd: frobnicate"));
    }

    #[test]
    fn pipe_roundtrip() {
        // Unique name so parallel test runs / a live foreman don't collide.
        let pipe = format!("foreman-test-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        // Fake GUI thread: answer the first request.
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Open(req, reply, sent) => {
                    assert_eq!(req.command, vec!["cmd.exe", "/c", "echo hi"]);
                    assert!(
                        sent.elapsed() < REPLY_TIMEOUT,
                        "server stamps requests when queued"
                    );
                    let _ = reply.send(OpenReply {
                        ok: true,
                        terminal: Some("t9".into()),
                        project: Some("p1".into()),
                        ..Default::default()
                    });
                }
                _ => panic!("expected CtrlMsg::Open"),
            }
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

    #[test]
    fn chat_request_roundtrips_and_reply_omits_empty_history() {
        let req: ChatRequest = serde_json::from_str(
            r#"{"cmd":"chat","project":"p1","from":"t2","text":"taking the parser"}"#,
        )
        .unwrap();
        assert_eq!(req.from.as_deref(), Some("t2"));
        assert_eq!(req.text.as_deref(), Some("taking the parser"));
        assert_eq!(req.history, None);
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<ChatRequest>(&s).unwrap(), req);

        // ok-reply without history must not serialize the field
        let ok = OpenReply {
            ok: true,
            ..Default::default()
        };
        assert!(!serde_json::to_string(&ok).unwrap().contains("history"));
        // history reply roundtrips
        let h = OpenReply {
            ok: true,
            history: Some(vec!["#1 t2: hi".into()]),
            ..Default::default()
        };
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<OpenReply>(&s).unwrap(), h);
    }

    #[test]
    fn parse_chat_args_builds_post_and_history() {
        // post: trailing words join into one message
        let req = parse_chat_args(
            &s(&["taking", "the", "parser"]),
            Some("p1".into()),
            Some("t2".into()),
        )
        .unwrap();
        assert_eq!(req.project.as_deref(), Some("p1"));
        assert_eq!(req.from.as_deref(), Some("t2"));
        assert_eq!(req.text.as_deref(), Some("taking the parser"));
        assert_eq!(req.history, None);
        // history with explicit N
        let req = parse_chat_args(&s(&["--history", "5"]), None, Some("t2".into())).unwrap();
        assert_eq!(req.history, Some(5));
        assert_eq!(req.text, None);
        // history default N
        let req = parse_chat_args(&s(&["--history"]), None, Some("t2".into())).unwrap();
        assert_eq!(req.history, Some(20));
        // explicit --project beats env default
        let req = parse_chat_args(
            &s(&["--project", "p2", "hi"]),
            Some("p1".into()),
            Some("t2".into()),
        )
        .unwrap();
        assert_eq!(req.project.as_deref(), Some("p2"));
    }

    #[test]
    fn parse_chat_args_treats_message_body_verbatim() {
        // flags inside the message body are message text, not flags
        let req = parse_chat_args(
            &s(&["use", "--project", "p2", "for", "that"]),
            Some("p1".into()),
            Some("t1".into()),
        )
        .unwrap();
        assert_eq!(
            req.project.as_deref(),
            Some("p1"),
            "message content must not reroute the post"
        );
        assert_eq!(req.text.as_deref(), Some("use --project p2 for that"));
        // -- escape hatch for leading-dash messages
        let req = parse_chat_args(
            &s(&["--", "--project", "p2"]),
            Some("p1".into()),
            Some("t1".into()),
        )
        .unwrap();
        assert_eq!(req.text.as_deref(), Some("--project p2"));
        assert_eq!(req.project.as_deref(), Some("p1"));
        // typo'd leading flag errors instead of posting garbage
        assert!(parse_chat_args(&s(&["--histroy", "5"]), None, Some("t1".into())).is_err());
    }

    #[test]
    fn parse_chat_args_rejects_bad_input() {
        // not inside a foreman terminal
        assert!(parse_chat_args(&s(&["hi"]), None, None).is_err());
        // nothing to do
        assert!(parse_chat_args(&s(&[]), None, Some("t2".into())).is_err());
        // both post and history
        assert!(parse_chat_args(&s(&["--history", "5", "hi"]), None, Some("t2".into())).is_err());
        // flag without value
        assert!(parse_chat_args(&s(&["--project"]), None, Some("t2".into())).is_err());
    }

    #[test]
    fn parse_chat_args_history_allows_missing_terminal_id() {
        // HELP_CHAT promises "--history works for any caller" — no terminal id needed
        let req = parse_chat_args(&s(&["--history"]), None, None).unwrap();
        assert_eq!(req.history, Some(20));
        assert_eq!(req.from, None);
        // inside a terminal, history still carries the sender
        let req = parse_chat_args(&s(&["--history"]), None, Some("t2".into())).unwrap();
        assert_eq!(req.from.as_deref(), Some("t2"));
        // posting still requires the terminal id
        assert!(parse_chat_args(&s(&["hi"]), None, None).is_err());
    }

    #[test]
    fn chat_history_request_is_wire_compatible_without_from() {
        // a from-less history request serializes with no "from" key at all
        let req = parse_chat_args(&s(&["--history"]), None, None).unwrap();
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"from\""), "{json}");
        // a v1 post (from always present) still parses with from set
        let v1 = r#"{"cmd":"chat","project":"p1","from":"t2","text":"hi"}"#;
        let req: ChatRequest = serde_json::from_str(v1).unwrap();
        assert_eq!(req.from.as_deref(), Some("t2"));
        // a from-less history request parses with from == None
        let req: ChatRequest = serde_json::from_str(r#"{"cmd":"chat","history":5}"#).unwrap();
        assert_eq!(req.from, None);
        assert_eq!(req.history, Some(5));
    }

    #[test]
    fn chat_pipe_roundtrip() {
        let pipe = format!("foreman-test-chat-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Chat(req, reply, _) => {
                    assert_eq!(req.from.as_deref(), Some("t2"));
                    assert_eq!(req.text.as_deref(), Some("hello"));
                    let _ = reply.send(OpenReply {
                        ok: true,
                        ..Default::default()
                    });
                }
                _ => panic!("expected CtrlMsg::Chat"),
            }
        });
        let req = ChatRequest {
            cmd: "chat".into(),
            project: Some("p1".into()),
            from: Some("t2".into()),
            to: Vec::new(),
            text: Some("hello".into()),
            history: None,
            re: None,
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
    fn parse_chat_args_collects_to_targets() {
        // repeatable, leading @ stripped, you allowed
        let req = parse_chat_args(
            &s(&["--to", "t3", "--to", "@you", "go"]),
            Some("p1".into()),
            Some("t2".into()),
        )
        .unwrap();
        assert_eq!(req.to, vec!["t3", "you"]);
        assert_eq!(req.text.as_deref(), Some("go"));
        // bad format is a client-side error naming the value
        let e = parse_chat_args(&s(&["--to", "bogus", "hi"]), None, Some("t2".into())).unwrap_err();
        assert!(e.contains("bogus"), "{e}");
        let e = parse_chat_args(&s(&["--to", "t", "hi"]), None, Some("t2".into())).unwrap_err();
        assert!(e.contains("bad --to target: t ("), "{e}");
        // --to needs a value
        assert!(parse_chat_args(&s(&["--to"]), None, Some("t2".into())).is_err());
        // mutually exclusive with --history
        let e =
            parse_chat_args(&s(&["--to", "t3", "--history"]), None, Some("t2".into())).unwrap_err();
        assert!(e.contains("mutually exclusive"), "{e}");
        // a plain post carries no targets
        let req = parse_chat_args(&s(&["hi"]), None, Some("t2".into())).unwrap();
        assert!(req.to.is_empty());
    }

    #[test]
    fn chat_request_to_is_wire_compatible_with_v1() {
        // empty to serializes away — untargeted requests are byte-identical to v1
        let req = parse_chat_args(&s(&["hi"]), Some("p1".into()), Some("t2".into())).unwrap();
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"to\""), "{json}");
        // a v1 request (no `to` key) still parses
        let v1 = r#"{"cmd":"chat","project":"p1","from":"t2","text":"hi"}"#;
        let req: ChatRequest = serde_json::from_str(v1).unwrap();
        assert!(req.to.is_empty());
        // targets roundtrip
        let req = parse_chat_args(&s(&["--to", "t3", "go"]), None, Some("t2".into())).unwrap();
        let back: ChatRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back.to, vec!["t3"]);
    }

    #[test]
    fn parse_chat_args_handles_re() {
        // --re N rides with a targeted reply
        let req = parse_chat_args(
            &s(&["--re", "19", "--to", "t6", "on", "it"]),
            Some("p1".into()),
            Some("t7".into()),
        )
        .unwrap();
        assert_eq!(req.re, Some(19));
        assert_eq!(req.to, vec!["t6"]);
        assert_eq!(req.text.as_deref(), Some("on it"));
        // works with an inline leading @mention too (no --to flag)
        let req = parse_chat_args(
            &s(&["--re", "19", "@t6", "on", "it"]),
            None,
            Some("t7".into()),
        )
        .unwrap();
        assert_eq!(req.re, Some(19));
        assert_eq!(req.text.as_deref(), Some("@t6 on it"));
    }

    #[test]
    fn parse_chat_args_rejects_bad_re_input() {
        // --re needs a numeric value
        assert!(parse_chat_args(&s(&["--re"]), None, Some("t2".into())).is_err());
        assert!(parse_chat_args(&s(&["--re", "abc", "hi"]), None, Some("t2".into())).is_err());
        // post-only: not with --history
        assert!(parse_chat_args(&s(&["--re", "5", "--history"]), None, Some("t2".into())).is_err());
    }

    #[test]
    fn chat_request_re_is_wire_compatible() {
        // a plain post serializes away `re` (byte-identical to v1)
        let req = parse_chat_args(&s(&["hi"]), Some("p1".into()), Some("t2".into())).unwrap();
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"re\""), "{json}");
        // a v1 request (no `re` key) still parses
        let v1 = r#"{"cmd":"chat","project":"p1","from":"t2","text":"hi"}"#;
        let req: ChatRequest = serde_json::from_str(v1).unwrap();
        assert_eq!(req.re, None);
        // a set `re` roundtrips
        let req = parse_chat_args(
            &s(&["--re", "9", "--to", "t6", "go"]),
            None,
            Some("t7".into()),
        )
        .unwrap();
        let back: ChatRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back.re, Some(9));
    }

    #[test]
    fn open_reply_seq_omitted_when_none() {
        let r = OpenReply {
            ok: true,
            ..Default::default()
        };
        assert!(!serde_json::to_string(&r).unwrap().contains("seq"));
        let r = OpenReply {
            ok: true,
            seq: Some(42),
            ..Default::default()
        };
        let back: OpenReply = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.seq, Some(42));
    }

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
            attrs: false,
            cursor: false,
            tail: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"project\""), "{json}");
        // false opt-in flags must serialize away (v1 byte-compat)
        assert!(!json.contains("\"attrs\""), "{json}");
        assert!(!json.contains("\"cursor\""), "{json}");
        assert!(!json.contains("\"tail\""), "{json}");
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
    fn parse_send_args_rejects_explicit_project_without_terminal() {
        let e = parse_send_args(
            &s(&["--project", "p2", "--text", "x"]),
            Some("p1".into()),
            Some("t4".into()),
            Some("p1".into()),
        )
        .unwrap_err();
        assert!(e.contains("--project needs an explicit --terminal"), "{e}");
    }

    #[test]
    fn parse_send_args_self_target_uses_own_project() {
        // bare self-target resolves from FOREMAN_PROJECT_ID, never a guess
        let req = parse_send_args(
            &s(&["--text", "x"]),
            Some("stale".into()),
            Some("t4".into()),
            Some("p9".into()),
        )
        .unwrap();
        assert_eq!(req.terminal.as_deref(), Some("t4"));
        assert_eq!(req.project.as_deref(), Some("p9"));
    }

    #[test]
    fn parse_send_args_self_target_requires_both_env_vars() {
        // missing self_terminal
        let e = parse_send_args(
            &s(&["--text", "x"]),
            Some("p1".into()),
            None,
            Some("p1".into()),
        )
        .unwrap_err();
        assert!(e.contains("FOREMAN_TERMINAL_ID"), "{e}");
        // missing self_project
        let e = parse_send_args(
            &s(&["--text", "x"]),
            Some("p1".into()),
            Some("t4".into()),
            None,
        )
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
            &s(&[
                "--project",
                "p1",
                "--terminal",
                "t3",
                "--text",
                "x",
                "--settle-ms",
                "500",
            ]),
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
            &s(&[
                "--project",
                "p1",
                "--terminal",
                "t3",
                "--nope",
                "--text",
                "x",
            ]),
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
                &s(&[
                    "--project",
                    "p1",
                    "--terminal",
                    "t3",
                    "--text",
                    "x",
                    "--settle-ms",
                    "abc"
                ]),
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
    fn parse_snapshot_args_rejects_explicit_project_without_terminal() {
        let e = parse_snapshot_args(
            &s(&["--project", "p2"]),
            Some("p1".into()),
            Some("t4".into()),
        )
        .unwrap_err();
        assert!(e.contains("--project needs an explicit --terminal"), "{e}");
    }

    #[test]
    fn parse_snapshot_args_self_target_requires_project() {
        let e = parse_snapshot_args(&s(&[]), None, Some("t4".into())).unwrap_err();
        assert!(e.contains("FOREMAN_PROJECT_ID"), "{e}");
    }

    #[test]
    fn parse_snapshot_args_requires_terminal() {
        // no flags and no self-target env — pins the fallthrough error, not the
        // explicit-project rejection (that path has its own test above)
        let e = parse_snapshot_args(&s(&[]), None, None).unwrap_err();
        assert!(e.contains("--terminal is required"), "{e}");
    }

    #[test]
    fn parse_snapshot_args_attrs_flag() {
        let req = parse_snapshot_args(
            &s(&["--terminal", "t2", "--attrs"]),
            Some("p1".into()),
            None,
        )
        .unwrap();
        assert!(req.attrs, "expected attrs=true");
        assert!(!req.cursor, "cursor defaults false");
    }

    #[test]
    fn parse_snapshot_args_cursor_flag() {
        let req = parse_snapshot_args(
            &s(&["--terminal", "t2", "--cursor"]),
            Some("p1".into()),
            None,
        )
        .unwrap();
        assert!(req.cursor, "expected cursor=true");
        assert!(!req.attrs, "attrs defaults false");
        assert_eq!(req.tail, None);
    }

    #[test]
    fn parse_snapshot_args_tail_flag() {
        let req = parse_snapshot_args(
            &s(&["--terminal", "t2", "--tail", "80"]),
            Some("p1".into()),
            None,
        )
        .unwrap();
        assert_eq!(req.tail, Some(80));
        assert!(!req.attrs);
        assert!(!req.cursor);
    }

    #[test]
    fn parse_snapshot_args_rejects_tail_zero_and_missing() {
        let e = parse_snapshot_args(
            &s(&["--terminal", "t2", "--tail", "0"]),
            Some("p1".into()),
            None,
        )
        .unwrap_err();
        assert!(e.contains("positive integer"), "{e}");
        let e = parse_snapshot_args(&s(&["--terminal", "t2", "--tail"]), Some("p1".into()), None)
            .unwrap_err();
        assert!(e.contains("positive integer"), "{e}");
        let e = parse_snapshot_args(
            &s(&["--terminal", "t2", "--tail", "nope"]),
            Some("p1".into()),
            None,
        )
        .unwrap_err();
        assert!(e.contains("positive integer"), "{e}");
    }

    #[test]
    fn snapshot_request_without_tail_is_wire_compat_with_v1() {
        // A v1 payload (no tail key) must still parse; a new request with
        // tail=None must omit the key so old GUIs never see it.
        let v1 = r#"{"cmd":"snapshot","terminal":"t3"}"#;
        let req: SnapshotRequest = serde_json::from_str(v1).unwrap();
        assert_eq!(req.tail, None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("\"tail\""), "{json}");
    }

    #[test]
    fn snapshot_reply_without_attrs_cursor_is_wire_compat() {
        // A plain snapshot reply (no --attrs/--cursor) must omit the new keys, so
        // it stays byte-identical to a v1 OpenReply.
        let reply = OpenReply {
            ok: true,
            history: Some(vec!["row0".into()]),
            ..Default::default()
        };
        let json = serde_json::to_string(&reply).unwrap();
        assert!(!json.contains("\"cells\""), "cells must be absent: {json}");
        assert!(
            !json.contains("\"cursor\""),
            "cursor must be absent: {json}"
        );
    }

    // ---- pipe roundtrips -----------------------------------------------------

    #[test]
    fn send_pipe_roundtrip() {
        let pipe = format!("foreman-test-send-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Send(req, reply, _) => {
                    assert_eq!(req.text.as_deref(), Some("hello"));
                    assert_eq!(req.terminal.as_deref(), Some("t3"));
                    let _ = reply.send(OpenReply {
                        ok: true,
                        ..Default::default()
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
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Snapshot(req, reply, _) => {
                    assert_eq!(req.terminal.as_deref(), Some("t3"));
                    let _ = reply.send(OpenReply {
                        ok: true,
                        history: Some(vec!["line one".into(), "line two".into()]),
                        ..Default::default()
                    });
                }
                _ => panic!("expected CtrlMsg::Snapshot"),
            }
        });
        let req = SnapshotRequest {
            cmd: "snapshot".into(),
            project: Some("p1".into()),
            terminal: Some("t3".into()),
            attrs: false,
            cursor: false,
            tail: None,
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

    #[test]
    fn snapshot_pipe_roundtrip_with_attrs_carries_cells() {
        // The structured (--attrs) path: a reply with cells survives the pipe.
        let pipe = format!("foreman-test-snap-attrs-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Snapshot(req, reply, _) => {
                    assert!(req.attrs, "request carries attrs flag");
                    let cell = crate::inspect::CellData {
                        ch: 'X',
                        fg: [10, 20, 30],
                        bg: None,
                        bold: false,
                        italic: false,
                        underline: true,
                        strikethrough: false,
                        inverse: false,
                        dim: false,
                        wide: false,
                    };
                    let _ = reply.send(OpenReply {
                        ok: true,
                        history: Some(vec!["X".into()]),
                        cells: Some(vec![vec![cell]]),
                        ..Default::default()
                    });
                }
                _ => panic!("expected CtrlMsg::Snapshot"),
            }
        });
        let req = SnapshotRequest {
            cmd: "snapshot".into(),
            project: Some("p1".into()),
            terminal: Some("t3".into()),
            attrs: true,
            cursor: false,
            tail: None,
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
        let cells = r.cells.expect("cells present");
        assert_eq!(cells[0][0].ch, 'X');
        assert!(cells[0][0].underline);
    }

    // ---- view ------------------------------------------------------------

    #[test]
    fn parse_view_args_rejects_non_png_extension() {
        let e = parse_view_args(&s(&["photo.jpg"]), None).unwrap_err();
        assert!(e.contains("not a .png file"), "{e}");
    }

    #[test]
    fn parse_view_args_rejects_missing_file() {
        let e = parse_view_args(&s(&["definitely_missing_9f8c.png"]), None).unwrap_err();
        assert!(e.contains("cannot find"), "{e}");
    }

    #[test]
    fn parse_view_args_resolves_absolute_path_and_project() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("foreman-view-test-{}.png", std::process::id()));
        std::fs::write(&path, b"not a real png, just a real file").unwrap();
        let req = parse_view_args(
            &s(&["--project", "p2", path.to_str().unwrap()]),
            Some("p1".into()),
        )
        .unwrap();
        assert_eq!(req.cmd, "view");
        assert_eq!(req.project.as_deref(), Some("p2"));
        assert!(!req.path.starts_with(r"\\?\"), "{}", req.path);
        assert!(std::path::Path::new(&req.path).is_absolute());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_view_args_defaults_project_from_env() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("foreman-view-test-def-{}.png", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let req = parse_view_args(&s(&[path.to_str().unwrap()]), Some("p9".into())).unwrap();
        assert_eq!(req.project.as_deref(), Some("p9"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_view_args_rejects_unknown_flag() {
        let e = parse_view_args(&s(&["--bogus", "a.png"]), None).unwrap_err();
        assert!(e.contains("--bogus"), "{e}");
    }

    #[test]
    fn parse_view_args_rejects_two_positionals() {
        let e = parse_view_args(&s(&["a.png", "b.png"]), None).unwrap_err();
        assert!(e.contains("unexpected argument"), "{e}");
    }

    #[test]
    fn view_request_wire_roundtrips() {
        let req = ViewRequest {
            cmd: "view".into(),
            project: Some("p1".into()),
            path: r"C:\images\armed.png".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ViewRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn view_pipe_roundtrip() {
        let pipe = format!("foreman-test-view-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx, eframe::egui::Context::default()));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::View(req, reply, _) => {
                    assert_eq!(req.path, r"C:\images\armed.png");
                    let _ = reply.send(OpenReply {
                        ok: true,
                        terminal: Some("t4".into()),
                        project: Some("p1".into()),
                        ..Default::default()
                    });
                }
                _ => panic!("expected CtrlMsg::View"),
            }
        });
        let req = ViewRequest {
            cmd: "view".into(),
            project: None,
            path: r"C:\images\armed.png".into(),
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
        assert_eq!(r.terminal.as_deref(), Some("t4"));
    }
}
