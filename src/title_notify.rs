//! Passive hook-to-GUI notification lane: one message per provider lifecycle
//! hook, carrying the event name and routing IDs. A `UserPromptSubmit` from a
//! main agent also carries the prompt, which starts Session naming.

use crate::agent_state::HookEvent;
use crate::terminal_titles::SourceAgent;
use interprocess::ConnectWaitMode;
use interprocess::local_socket::{ConnectOptions, GenericNamespaced, ListenerOptions, prelude::*};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// Helper stdin cap. `PostToolUse` payloads include the tool's full output.
const HELPER_INPUT_BYTES: u64 = 16 * 1024 * 1024;
/// Pipe-server read cap. The forwarded message is the event and IDs only.
const MESSAGE_BYTES: u64 = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
const READ_TIMEOUT: Duration = Duration::from_secs(1);
/// Tool-heavy turns across several panes, doubled by Grok replaying Claude's
/// hooks, must not get a `Stop` turned away.
const MAX_INFLIGHT: usize = 32;

static PIPE: OnceLock<String> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TitlePromptEvent {
    pub source_agent: SourceAgent,
    pub hook_event: HookEvent,
    pub vendor_session_id: String,
    pub transcript_path: Option<String>,
    pub project_id: Option<String>,
    pub terminal_id: String,
    /// Present only for a main agent's `UserPromptSubmit` with a prompt worth
    /// naming from. State needs nothing but the event and the IDs.
    pub prompt: Option<String>,
}

pub fn new_pipe_name() -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("foreman-title-{}-{nonce}", std::process::id())
}

pub fn set_pipe_name(pipe: String) -> Result<(), String> {
    PIPE.set(pipe)
        .map_err(|_| "title notification pipe was already initialized".to_string())
}

pub fn pipe_name() -> Option<&'static str> {
    PIPE.get().map(String::as_str)
}

/// Internal hook command. A hook must never interfere with the agent's prompt:
/// malformed input, a busy/missing Foreman, and transport failure are all quiet
/// success. The GUI is the only process that may start title generation.
fn parse_args(args: &[String]) -> Option<(SourceAgent, HookEvent)> {
    let value_of = |flag: &str| {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
    };
    let source = SourceAgent::parse(value_of("--agent")?)?;
    // A missing `--event` is a handler written by an older Foreman; it keeps
    // naming until the installer rewrites it.
    let event = match value_of("--event") {
        Some(name) => HookEvent::parse(name)?,
        None => HookEvent::UserPromptSubmit,
    };
    Some((source, event))
}

pub fn client_main(args: &[String]) -> i32 {
    let Some((source, event)) = parse_args(args) else {
        return 0;
    };
    let mut input = Vec::new();
    if std::io::stdin()
        .take(HELPER_INPUT_BYTES + 1)
        .read_to_end(&mut input)
        .is_err()
        || input.len() as u64 > HELPER_INPUT_BYTES
    {
        return 0;
    }
    let Some(event) = normalize_event(source, event, &input, |name| std::env::var(name).ok())
    else {
        return 0;
    };
    let Some(pipe) = std::env::var("FOREMAN_TITLE_PIPE").ok() else {
        return 0;
    };
    let _ = send_event(&pipe, &event);
    0
}

fn normalize_event(
    source: SourceAgent,
    event: HookEvent,
    input: &[u8],
    getenv: impl Fn(&str) -> Option<String>,
) -> Option<TitlePromptEvent> {
    if getenv("FOREMAN").as_deref() != Some("1") {
        return None;
    }
    // Grok currently imports Claude-compatible hooks. Its dedicated hook carries
    // better session identity, so suppress that duplicated Claude invocation.
    if source == SourceAgent::Claude && getenv("GROK_SESSION_ID").is_some() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(input).ok()?;
    let nonempty = |name: &str| {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    let subagent = [
        "agent_id",
        "agentId",
        "agent_type",
        "agentType",
        "subagent_type",
        "subagentType",
    ]
    .into_iter()
    .any(nonempty);
    // A subagent's turn is not the Session's turn, but its permission prompt
    // waits on the human and its tool finishing is what leaves Needs you.
    if subagent && !matches!(event, HookEvent::PermissionRequest | HookEvent::PostToolUse) {
        return None;
    }
    let prompt = if event == HookEvent::UserPromptSubmit && !subagent {
        value
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::terminal_titles::meaningful_prompt)
    } else {
        None
    };
    let vendor_session_id = match source {
        SourceAgent::Grok => getenv("GROK_SESSION_ID").or_else(|| {
            value
                .get("sessionId")
                .or_else(|| value.get("session_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        }),
        SourceAgent::Claude | SourceAgent::Codex => value
            .get("session_id")
            .or_else(|| value.get("sessionId"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }?
    .trim()
    .to_string();
    if vendor_session_id.is_empty() {
        return None;
    }
    let transcript_path = value
        .get("transcript_path")
        .or_else(|| value.get("transcriptPath"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let terminal_id = getenv("FOREMAN_TERMINAL_ID")?.trim().to_string();
    if terminal_id.is_empty() {
        return None;
    }
    Some(TitlePromptEvent {
        source_agent: source,
        hook_event: event,
        vendor_session_id,
        transcript_path,
        project_id: getenv("FOREMAN_PROJECT_ID").filter(|value| !value.trim().is_empty()),
        terminal_id,
        prompt,
    })
}

fn send_event(pipe: &str, event: &TitlePromptEvent) -> std::io::Result<()> {
    let name = pipe.to_ns_name::<GenericNamespaced>()?;
    let mut connection = ConnectOptions::new()
        .name(name)
        .wait_mode(ConnectWaitMode::Timeout(CONNECT_TIMEOUT))
        .connect_sync()?;
    serde_json::to_writer(&mut connection, event).map_err(std::io::Error::other)?;
    connection.flush()
}

fn read_event_bytes(reader: &mut impl Read, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        let remaining = (MESSAGE_BYTES as usize + 1).saturating_sub(bytes.len());
        if remaining == 0 {
            return None;
        }
        let chunk = remaining.min(buffer.len());
        match reader.read(&mut buffer[..chunk]) {
            // Windows PIPE_NOWAIT maps an empty, still-connected pipe to
            // Ok(0), just like EOF. Only a complete event ends this read.
            Ok(0) => std::thread::sleep(Duration::from_millis(2)),
            Ok(read) => {
                bytes.extend_from_slice(&buffer[..read]);
                if bytes.len() as u64 > MESSAGE_BYTES {
                    return None;
                }
                if serde_json::from_slice::<TitlePromptEvent>(&bytes).is_ok() {
                    return Some(bytes);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// Run the process-local one-way listener. Connections and the GUI queue are
/// both bounded; each client also has a read deadline. Overload or a stalled
/// client drops a naming attempt instead of delaying input.
pub fn serve(pipe: &str, tx: mpsc::SyncSender<TitlePromptEvent>, ctx: eframe::egui::Context) {
    let Ok(name) = pipe.to_ns_name::<GenericNamespaced>() else {
        return;
    };
    let Ok(listener) = ListenerOptions::new().name(name).create_sync() else {
        eprintln!("foreman: Session naming hook listener unavailable");
        return;
    };
    let inflight = Arc::new(AtomicUsize::new(0));
    for connection in listener.incoming() {
        let Ok(connection) = connection else { continue };
        if inflight.load(Ordering::Relaxed) >= MAX_INFLIGHT {
            continue;
        }
        inflight.fetch_add(1, Ordering::Relaxed);
        let inflight = inflight.clone();
        let tx = tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut connection = connection;
            let bytes = connection
                .set_nonblocking(true)
                .ok()
                .and_then(|_| read_event_bytes(&mut connection, READ_TIMEOUT));
            if let Some(bytes) = bytes
                && let Ok(event) = serde_json::from_slice(&bytes)
                && tx.try_send(event).is_ok()
            {
                ctx.request_repaint();
            }
            inflight.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_state::HookEvent;
    use std::collections::HashMap;

    struct WouldBlockForever;

    impl Read for WouldBlockForever {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        }
    }

    struct SlowDrip {
        reads: usize,
    }

    impl Read for SlowDrip {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(5));
            if self.reads >= 10 {
                return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
            }
            self.reads += 1;
            buffer[0] = b'x';
            Ok(1)
        }
    }

    fn env(values: &[(&str, &str)]) -> HashMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn claude_hook_payload_becomes_a_scoped_event() {
        let vars = env(&[
            ("FOREMAN", "1"),
            ("FOREMAN_TERMINAL_ID", "t4"),
            ("FOREMAN_PROJECT_ID", "p2"),
        ]);
        let event = normalize_event(
            SourceAgent::Claude,
            HookEvent::UserPromptSubmit,
            br#"{"session_id":"claude-7","transcript_path":"C:\\Users\\me\\.claude\\projects\\s.jsonl","prompt":" fix the auth race "}"#,
            |name| vars.get(name).cloned(),
        )
        .expect("valid event");
        assert_eq!(event.vendor_session_id, "claude-7");
        assert_eq!(
            event.transcript_path.as_deref(),
            Some(r"C:\Users\me\.claude\projects\s.jsonl")
        );
        assert_eq!(event.terminal_id, "t4");
        assert_eq!(event.project_id.as_deref(), Some("p2"));
        assert_eq!(event.prompt.as_deref(), Some("fix the auth race"));
        assert_eq!(event.hook_event, HookEvent::UserPromptSubmit);
    }

    fn env_fn(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map = env(pairs);
        move |name| map.get(name).cloned()
    }

    const IN_FOREMAN: &[(&str, &str)] = &[
        ("FOREMAN", "1"),
        ("FOREMAN_PROJECT_ID", "p9"),
        ("FOREMAN_TERMINAL_ID", "t7"),
    ];

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn event_arg_defaults_to_prompt_submit_and_rejects_unknown_names() {
        assert_eq!(
            parse_args(&args(&["--agent", "claude"])),
            Some((SourceAgent::Claude, HookEvent::UserPromptSubmit))
        );
        assert_eq!(
            parse_args(&args(&["--agent", "codex", "--event", "Interrupt"])),
            Some((SourceAgent::Codex, HookEvent::Interrupt))
        );
        assert_eq!(
            parse_args(&args(&["--agent", "grok", "--event", "SessionStart"])),
            None
        );
        assert_eq!(parse_args(&args(&["--event", "Stop"])), None);
    }

    #[test]
    fn state_events_carry_the_event_and_ids_but_no_payload_text() {
        let input = br#"{"session_id":"s1","prompt":"secret","last_assistant_message":"secret","tool_response":"secret"}"#;
        let event = normalize_event(
            SourceAgent::Claude,
            HookEvent::Stop,
            input,
            env_fn(IN_FOREMAN),
        )
        .expect("state event");
        assert_eq!(event.hook_event, HookEvent::Stop);
        assert_eq!(event.prompt, None, "prompt rides only on UserPromptSubmit");
        assert_eq!(event.vendor_session_id, "s1");
        assert_eq!(event.project_id.as_deref(), Some("p9"));
        assert_eq!(event.terminal_id, "t7");
        let wire = serde_json::to_string(&event).unwrap();
        assert!(!wire.contains("secret"), "{wire}");
    }

    #[test]
    fn a_slash_command_prompt_is_a_state_event_without_naming() {
        let input = br#"{"session_id":"s1","prompt":"/help"}"#;
        let event = normalize_event(
            SourceAgent::Claude,
            HookEvent::UserPromptSubmit,
            input,
            env_fn(IN_FOREMAN),
        )
        .expect("the turn still starts");
        assert_eq!(event.hook_event, HookEvent::UserPromptSubmit);
        assert_eq!(event.prompt, None, "nothing worth naming from");
    }

    #[test]
    fn subagent_events_are_dropped_except_permission_and_tool_done() {
        let payload = |event: &str| {
            format!(
                r#"{{"session_id":"s1","agent_id":"a1","hook_event_name":"{event}","prompt":"child work"}}"#
            )
        };
        let drop = [
            HookEvent::UserPromptSubmit,
            HookEvent::Stop,
            HookEvent::StopFailure,
            HookEvent::Interrupt,
        ];
        for event in drop {
            assert!(
                normalize_event(
                    SourceAgent::Claude,
                    event,
                    payload(event.name()).as_bytes(),
                    env_fn(IN_FOREMAN)
                )
                .is_none(),
                "{event:?}"
            );
        }
        for event in [HookEvent::PermissionRequest, HookEvent::PostToolUse] {
            let got = normalize_event(
                SourceAgent::Claude,
                event,
                payload(event.name()).as_bytes(),
                env_fn(IN_FOREMAN),
            )
            .unwrap_or_else(|| panic!("{event:?}"));
            assert_eq!(got.hook_event, event);
            assert_eq!(got.prompt, None);
        }
    }

    #[test]
    fn grok_duplicate_of_a_claude_state_hook_is_suppressed() {
        let mut pairs = IN_FOREMAN.to_vec();
        pairs.push(("GROK_SESSION_ID", "g1"));
        let input = br#"{"sessionId":"g1"}"#;
        assert!(
            normalize_event(SourceAgent::Claude, HookEvent::Stop, input, env_fn(&pairs)).is_none()
        );
        let event =
            normalize_event(SourceAgent::Grok, HookEvent::Stop, input, env_fn(&pairs)).unwrap();
        assert_eq!(event.vendor_session_id, "g1");
    }

    #[test]
    fn helper_accepts_a_tool_payload_larger_than_one_message() {
        let big = "x".repeat(MESSAGE_BYTES as usize * 4);
        let input = format!(r#"{{"session_id":"s1","tool_response":"{big}"}}"#);
        assert!(input.len() as u64 > MESSAGE_BYTES);
        assert!(input.len() as u64 <= HELPER_INPUT_BYTES);
        let event = normalize_event(
            SourceAgent::Claude,
            HookEvent::PostToolUse,
            input.as_bytes(),
            env_fn(IN_FOREMAN),
        )
        .expect("large tool output must not drop the event");
        assert!(
            serde_json::to_vec(&event).unwrap().len() < 1024,
            "the forwarded message stays small"
        );
    }

    #[test]
    fn subagents_and_grok_replaying_claude_hooks_are_ignored() {
        let mut vars = env(&[("FOREMAN", "1"), ("FOREMAN_TERMINAL_ID", "t4")]);
        assert!(
            normalize_event(
                SourceAgent::Codex,
                HookEvent::UserPromptSubmit,
                br#"{"session_id":"s1","agent_id":"child","prompt":"do work"}"#,
                |name| vars.get(name).cloned(),
            )
            .is_none()
        );
        vars.insert("GROK_SESSION_ID".into(), "g1".into());
        assert!(
            normalize_event(
                SourceAgent::Claude,
                HookEvent::UserPromptSubmit,
                br#"{"session_id":"s1","prompt":"do work"}"#,
                |name| vars.get(name).cloned(),
            )
            .is_none()
        );

        let grok_vars = env(&[
            ("FOREMAN", "1"),
            ("FOREMAN_TERMINAL_ID", "t4"),
            ("GROK_SESSION_ID", "g1"),
        ]);
        assert!(
            normalize_event(
                SourceAgent::Grok,
                HookEvent::UserPromptSubmit,
                br#"{"sessionId":"g1","subagentType":"explore","prompt":"do child work"}"#,
                |name| grok_vars.get(name).cloned(),
            )
            .is_none()
        );
    }

    #[test]
    fn connected_client_can_delay_its_first_write() {
        let pipe = new_pipe_name();
        let listener = ListenerOptions::new()
            .name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap())
            .create_sync()
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut connection = listener.accept().unwrap();
            connection.set_nonblocking(true).unwrap();
            started_tx.send(()).unwrap();
            read_event_bytes(&mut connection, READ_TIMEOUT)
        });
        let mut client = ConnectOptions::new()
            .name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap())
            .connect_sync()
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let event = TitlePromptEvent {
            source_agent: SourceAgent::Codex,
            hook_event: HookEvent::UserPromptSubmit,
            vendor_session_id: "delayed-session".into(),
            transcript_path: None,
            project_id: Some("p3".into()),
            terminal_id: "t8".into(),
            prompt: Some("fix delayed delivery".into()),
        };
        let payload = serde_json::to_vec(&event).unwrap();
        // The broken reader may already have closed the connection.
        let _ = client.write_all(&payload);
        assert_eq!(server.join().unwrap(), Some(payload));
    }

    #[test]
    fn one_way_pipe_round_trip_preserves_routing_identity() {
        let pipe = new_pipe_name();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let server_pipe = pipe.clone();
        std::thread::spawn(move || serve(&server_pipe, tx, eframe::egui::Context::default()));
        let event = TitlePromptEvent {
            source_agent: SourceAgent::Grok,
            hook_event: HookEvent::UserPromptSubmit,
            vendor_session_id: "grok-session".into(),
            transcript_path: None,
            project_id: Some("p3".into()),
            terminal_id: "t8".into(),
            prompt: Some("trace the rendering regression".into()),
        };
        // Retry only connection establishment while the listener binds. Once
        // connected, send exactly one event: ordinary delivery must not race.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut connection = loop {
            if let Ok(connection) = ConnectOptions::new()
                .name(pipe.as_str().to_ns_name::<GenericNamespaced>().unwrap())
                .connect_sync()
            {
                break connection;
            }
            assert!(Instant::now() < deadline, "listener never bound");
            std::thread::sleep(Duration::from_millis(2));
        };
        serde_json::to_writer(&mut connection, &event).unwrap();
        connection.flush().unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), event);
    }

    #[test]
    fn partial_event_survives_empty_reads_and_incomplete_eof_times_out() {
        struct PausedChunks {
            chunks: std::collections::VecDeque<Vec<u8>>,
        }
        impl Read for PausedChunks {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let chunk = self.chunks.pop_front().unwrap_or_default();
                buffer[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
        }
        let payload = br#"{"source_agent":"codex","hook_event":"UserPromptSubmit","vendor_session_id":"s1","terminal_id":"t1","prompt":"fix it"}"#;
        let mut reader = PausedChunks {
            chunks: [payload[..30].to_vec(), vec![], payload[30..].to_vec()].into(),
        };
        assert_eq!(
            read_event_bytes(&mut reader, READ_TIMEOUT),
            Some(payload.to_vec())
        );
        assert!(read_event_bytes(&mut &payload[..30], Duration::from_millis(20)).is_none());
    }

    #[test]
    fn stalled_listener_read_is_bounded() {
        let started = std::time::Instant::now();
        assert!(read_event_bytes(&mut WouldBlockForever, Duration::from_millis(20)).is_none());
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "stalled reader outlived its deadline"
        );
    }

    #[test]
    fn slow_drip_listener_read_obeys_the_wall_clock_deadline() {
        let mut reader = SlowDrip { reads: 0 };
        assert!(read_event_bytes(&mut reader, Duration::from_millis(12)).is_none());
        assert!(
            reader.reads <= 3,
            "readable bytes must not extend the wall-clock deadline"
        );
    }

    /// Manual release-gate benchmark. It launches only Foreman's early hook
    /// helper and never starts a provider CLI. Run with:
    /// `FOREMAN_TITLE_BENCH_EXE=target/agent/release/foreman.exe cargo test
    /// --release --target-dir target/agent title_hook_helper_p95 -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore = "manual release helper benchmark"]
    fn title_hook_helper_p95_is_below_100_ms() {
        use std::process::{Command, Stdio};

        let exe = std::env::var_os("FOREMAN_TITLE_BENCH_EXE")
            .expect("set FOREMAN_TITLE_BENCH_EXE to the release Foreman executable");
        let mut elapsed = Vec::with_capacity(100);
        for index in 0..100 {
            let pipe = format!("foreman-title-bench-{}-{index}", std::process::id());
            let name = pipe
                .as_str()
                .to_ns_name::<GenericNamespaced>()
                .expect("valid benchmark pipe name");
            let listener = ListenerOptions::new()
                .name(name)
                .create_sync()
                .expect("bind benchmark listener");
            let started = std::time::Instant::now();
            let mut command = Command::new(&exe);
            command
                .args(["title-event", "--agent", "claude"])
                .env("FOREMAN", "1")
                .env("FOREMAN_TERMINAL_ID", "t1")
                .env("FOREMAN_TITLE_PIPE", &pipe)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            let mut child = command.spawn().expect("launch hook helper");
            child
                .stdin
                .take()
                .expect("helper stdin")
                .write_all(
                    br#"{"session_id":"bench-session","prompt":"benchmark the local hook helper"}"#,
                )
                .expect("write hook event");
            let mut connection = listener.accept().expect("accept helper connection");
            let mut bytes = Vec::new();
            connection.read_to_end(&mut bytes).expect("read hook event");
            assert!(child.wait().expect("wait for helper").success());
            let event: TitlePromptEvent = serde_json::from_slice(&bytes).expect("valid event");
            assert_eq!(event.terminal_id, "t1");
            elapsed.push(started.elapsed());
        }
        elapsed.sort_unstable();
        let p95 = elapsed[94];
        println!(
            "title-event 100 launches: median={:.2}ms p95={:.2}ms max={:.2}ms",
            elapsed[49].as_secs_f64() * 1000.0,
            p95.as_secs_f64() * 1000.0,
            elapsed[99].as_secs_f64() * 1000.0,
        );
        assert!(p95 < Duration::from_millis(100), "p95 was {p95:?}");
    }
}
