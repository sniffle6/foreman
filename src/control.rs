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
        // No read timeout: a client that connects and never sends a newline
        // parks this loop, serializing the (single-threaded) pipe. The GUI is
        // unaffected — only dispatch stalls. Accepted for v1; revisit if a
        // wedged client ever shows up in practice.
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
        let mut out =
            serde_json::to_string(&reply).expect("OpenReply is always serializable");
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

    #[test]
    fn pipe_roundtrip() {
        // Unique name so parallel test runs / a live foreman don't collide.
        let pipe = format!("foreman-test-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pipe.clone();
        std::thread::spawn(move || serve(&p2, tx));
        // Fake GUI thread: answer the first request.
        std::thread::spawn(move || {
            let CtrlMsg::Open(req, reply) =
                rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
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
}
