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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<String>>, // chat --history results
}

impl OpenReply {
    pub fn err(msg: impl Into<String>) -> Self {
        OpenReply {
            ok: false,
            terminal: None,
            project: None,
            error: Some(msg.into()),
            history: None,
        }
    }
}

/// Project chat post or history read (spec: agent-group-chat §1). Exactly one
/// of `text` (post) / `history` (read last N) must be set — the client
/// enforces this; the server treats `history` as the discriminator. `from` is
/// the sender's own terminal id from its env. As with `open`, this is a
/// guardrail against confused agents, NOT a security boundary — any local
/// process can speak to the pipe and claim any `from`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChatRequest {
    pub cmd: String, // always "chat"
    #[serde(default)]
    pub project: Option<String>, // "p1"; None = focused project
    pub from: String, // "t2"
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub history: Option<usize>,
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
use std::sync::mpsc;

/// One control request, the channel the GUI thread answers on, and when the
/// server queued it. The GUI must NOT execute requests older than
/// [`REPLY_TIMEOUT`]: the server has already told that client "foreman did not
/// respond", so spawning would open a terminal the dispatcher believes failed —
/// and a retrying dispatcher would then create a duplicate.
pub enum CtrlMsg {
    Open(OpenRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Chat(ChatRequest, mpsc::Sender<OpenReply>, std::time::Instant),
}

/// Pipe server. Runs on a background thread for the GUI's whole lifetime; the
/// GUI drains `tx`'s receiver each frame. One JSON line in, one JSON line out,
/// per connection.
pub fn serve(pipe: &str, tx: mpsc::Sender<CtrlMsg>) {
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
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let mut conn = BufReader::new(conn);
        let mut line = String::new();
        // No read timeout: a client that connects and never sends a newline
        // parks this loop, serializing the (single-threaded) pipe. The GUI is
        // unaffected — only dispatch stalls. Accepted for v1; revisit if a
        // wedged client ever shows up in practice.
        if conn.read_line(&mut line).is_err() {
            continue;
        }
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
                other => Err(format!("unknown cmd: {other}")),
            },
        };
        let reply = match msg {
            Err(e) => OpenReply::err(e),
            Ok(m) => {
                if tx.send(m).is_err() {
                    return; // GUI gone; stop serving
                }
                rrx.recv_timeout(REPLY_TIMEOUT)
                    .unwrap_or_else(|_| OpenReply::err("foreman did not respond"))
            }
        };
        let mut out = serde_json::to_string(&reply).expect("OpenReply is always serializable");
        out.push('\n');
        let _ = conn.get_mut().write_all(out.as_bytes());
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
    let from = self_terminal.ok_or("not inside a foreman terminal (FOREMAN_TERMINAL_ID unset)")?;
    let mut project = default_project;
    let mut history: Option<usize> = None;
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
        (false, None) => Ok(ChatRequest {
            cmd: "chat".into(),
            project,
            from,
            text: Some(words.join(" ")),
            history: None,
        }),
        (true, Some(n)) => Ok(ChatRequest {
            cmd: "chat".into(),
            project,
            from,
            text: None,
            history: Some(n),
        }),
        (true, None) => Err("nothing to do: give a message or --history".into()),
        (false, Some(_)) => Err("--history and a message are mutually exclusive".into()),
    }
}

/// Subcommand entry point (no GUI). Returns the process exit code.
pub fn client_main(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("open") => open_main(&args[1..]),
        Some("chat") => chat_main(&args[1..]),
        _ => {
            eprintln!("usage: foreman open [--project P] [--title T] [--cwd D] -- <command...>");
            eprintln!("       foreman chat [--project P] <message...>");
            eprintln!("       foreman chat [--project P] --history [N]");
            2
        }
    }
}

fn open_main(args: &[String]) -> i32 {
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
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(!s.contains("error"));
        assert_eq!(serde_json::from_str::<OpenReply>(&s).unwrap(), ok);
        assert_eq!(OpenReply::err("boom").error.as_deref(), Some("boom"));
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
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
        std::thread::spawn(move || serve(&p2, tx));
        let req = OpenRequest {
            cmd: "status".into(),
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
        assert!(reply.error.unwrap().contains("unknown cmd: status"));
    }

    #[test]
    fn pipe_roundtrip() {
        // Unique name so parallel test runs / a live foreman don't collide.
        let pipe = format!("foreman-test-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx));
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
        assert_eq!(req.from, "t2");
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
        };
        assert!(!serde_json::to_string(&ok).unwrap().contains("history"));
        // history reply roundtrips
        let h = OpenReply {
            ok: true,
            terminal: None,
            project: None,
            error: None,
            history: Some(vec!["#1 t2: hi".into()]),
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
        assert_eq!(req.from, "t2");
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
    fn chat_pipe_roundtrip() {
        let pipe = format!("foreman-test-chat-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx));
        std::thread::spawn(move || {
            match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
                CtrlMsg::Chat(req, reply, _) => {
                    assert_eq!(req.from, "t2");
                    assert_eq!(req.text.as_deref(), Some("hello"));
                    let _ = reply.send(OpenReply {
                        ok: true,
                        terminal: None,
                        project: None,
                        error: None,
                        history: None,
                    });
                }
                _ => panic!("expected CtrlMsg::Chat"),
            }
        });
        let req = ChatRequest {
            cmd: "chat".into(),
            project: Some("p1".into()),
            from: "t2".into(),
            text: Some("hello".into()),
            history: None,
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
}
