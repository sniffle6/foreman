//! Agent dispatch control channel: a named pipe (`\\.\pipe\foreman`) any local
//! process can use to open a terminal inside the running foreman. See
//! docs/epics/agent-dispatch-epic.md.

/// Pipe name; `GenericNamespaced` maps it to `\\.\pipe\foreman` on Windows.
pub const PIPE: &str = "foreman";

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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
}

impl OpenReply {
    pub fn err(msg: impl Into<String>) -> Self {
        OpenReply {
            ok: false,
            terminal: None,
            project: None,
            error: Some(msg.into()),
            history: None,
            seq: None,
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
}

/// Pipe server. Runs on a background thread for the GUI's whole lifetime; the
/// GUI drains `tx`'s receiver each frame. One JSON line in, one JSON line out,
/// per connection.
pub fn serve(pipe: &str, tx: mpsc::Sender<CtrlMsg>, ctx: eframe::egui::Context) {
    let Ok(name) = pipe.to_ns_name::<GenericNamespaced>() else {
        return;
    };
    let listener = match ListenerOptions::new().name(name).create_sync() {
        Ok(l) => l,
        // Another foreman owns the pipe (or it's blocked): GUI still works,
        // dispatch is just unavailable in this instance.
        Err(e) => {
            eprintln!("control: pipe unavailable ({e}); agent dispatch disabled");
            return;
        }
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

const HELP: &str = "\
foreman — a desktop for running fleets of AI-agent terminals

USAGE
  foreman                                   launch the GUI (no arguments)
  foreman open [flags] -- <command...>      spawn a command in a new visible terminal
  foreman chat [flags] [--] <message...>    post to the project chat room
  foreman chat [--project P] --history [N]  read the last N room lines (default 20)
  foreman status [--project P]              list projects + terminals (running/exited)
  foreman close [tN ...] [--project P]      close terminals (no ids: your own pane)
  foreman help | --help | -h                this text (also: open --help, chat --help,
                                            status --help, close --help)

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

/// Subcommand entry point (no GUI). Returns the process exit code.
pub fn client_main(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("open") => open_main(&args[1..]),
        Some("chat") => chat_main(&args[1..]),
        Some("status") => status_main(&args[1..]),
        Some("close") => close_main(&args[1..]),
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
    report("foreman open", request(PIPE, &req))
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
    report("foreman chat", request(PIPE, &req))
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
    report("foreman status", request(PIPE, &req))
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
    report("foreman close", request(PIPE, &req))
}

/// Print the pipe reply (or the connection failure) the way all subcommands do.
/// History replies print line-per-line for agent readability; other ok replies
/// print as JSON (the open reply carries terminal/project ids the caller needs).
fn report(label: &str, res: std::io::Result<OpenReply>) -> i32 {
    match res {
        Ok(r) if r.ok => {
            if let Some(lines) = &r.history {
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
            error: None,
            history: None,
            seq: None,
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(!s.contains("error"));
        assert!(!s.contains("seq"));
        assert_eq!(serde_json::from_str::<OpenReply>(&s).unwrap(), ok);
        assert_eq!(OpenReply::err("boom").error.as_deref(), Some("boom"));
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
                        terminal: None,
                        project: Some("p1".into()),
                        error: None,
                        history: None,
                        seq: None,
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
                        terminal: None,
                        project: None,
                        error: None,
                        history: Some(vec!["p1  proj  -".into()]),
                        seq: None,
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
                        error: None,
                        history: None,
                        seq: None,
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
            terminal: None,
            project: None,
            error: None,
            history: None,
            seq: None,
        };
        assert!(!serde_json::to_string(&ok).unwrap().contains("history"));
        // history reply roundtrips
        let h = OpenReply {
            ok: true,
            terminal: None,
            project: None,
            error: None,
            history: Some(vec!["#1 t2: hi".into()]),
            seq: None,
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
                        terminal: None,
                        project: None,
                        error: None,
                        history: None,
                        seq: None,
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
            terminal: None,
            project: None,
            error: None,
            history: None,
            seq: None,
        };
        assert!(!serde_json::to_string(&r).unwrap().contains("seq"));
        let r = OpenReply {
            ok: true,
            terminal: None,
            project: None,
            error: None,
            history: None,
            seq: Some(42),
        };
        let back: OpenReply = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.seq, Some(42));
    }
}
