//! Bounded, read-only provider compaction recovery over the instance control pipe.

use crate::control::{KanbanRequest, OpenReply};
use std::io::Read;

const INPUT_LIMIT: u64 = 64 * 1024;
const HELPER_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Bounds workers even when a client times out before the server finishes.
pub(crate) struct RequestPermit<'a>(&'a std::sync::atomic::AtomicUsize);

/// Separate capacity for compaction context: polling waits and other
/// assignment requests must not starve the time-bounded recovery hook.
#[derive(Default)]
struct Pools {
    general: std::sync::atomic::AtomicUsize,
    context: std::sync::atomic::AtomicUsize,
}

static POOLS: Pools = Pools {
    general: std::sync::atomic::AtomicUsize::new(0),
    context: std::sync::atomic::AtomicUsize::new(0),
};

impl RequestPermit<'static> {
    pub fn acquire(operation: &str) -> Result<Self, String> {
        Self::acquire_in(&POOLS, operation)
    }
}

impl<'a> RequestPermit<'a> {
    fn acquire_in(pools: &'a Pools, operation: &str) -> Result<Self, String> {
        if operation == "context" {
            return Self::acquire_from(&pools.context);
        }
        Self::acquire_from(&pools.general)
    }

    fn acquire_from(counter: &'a std::sync::atomic::AtomicUsize) -> Result<Self, String> {
        use std::sync::atomic::Ordering;
        counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < 4).then_some(n + 1)
            })
            .map_err(|_| "assignment service busy; retry the same request id".to_string())?;
        Ok(Self(counter))
    }
}

impl Drop for RequestPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn request_for(
    provider: &str,
    input: &serde_json::Value,
    env: impl Fn(&str) -> Option<String>,
) -> Option<(String, KanbanRequest)> {
    if !matches!(provider, "codex" | "claude")
        || input["hook_event_name"] != "SessionStart"
        || input["source"] != "compact"
        || input.get("agent_id").is_some_and(|v| !v.is_null())
    {
        return None;
    }
    let session = input["session_id"].as_str()?.trim();
    if session.is_empty() || session.len() > 256 {
        return None;
    }
    let pipe = env("FOREMAN_PIPE")?.trim().to_string();
    if pipe.is_empty() || pipe == crate::control::PIPE {
        return None;
    }
    Some((
        pipe,
        KanbanRequest {
            cmd: "kanban".into(),
            action: "assignment".into(),
            project: Some(env("FOREMAN_PROJECT_ID")?),
            from: Some(env("FOREMAN_TERMINAL_ID")?),
            assignment: Some(crate::assignments::Command {
                operation: "context".into(),
                provider: provider.into(),
                session: session.into(),
                caller_pid: std::process::id(),
                ..Default::default()
            }),
            ..Default::default()
        },
    ))
}

fn hook_output(reply: OpenReply) -> Option<serde_json::Value> {
    if !reply.ok {
        return None;
    }
    let context = reply.history?.join("\n");
    if context.trim().is_empty() {
        return None;
    }
    Some(
        serde_json::json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": context}}),
    )
}

/// Failed, missing, malformed, duplicate and timed-out hooks cannot mutate an
/// assignment or block the provider. Only the bounded main thread prints context.
pub(crate) fn client_main(args: &[String]) -> i32 {
    let Some(provider) = args
        .windows(2)
        .find(|p| p[0] == "--agent")
        .map(|p| p[1].clone())
    else {
        return 0;
    };
    let output = context_output(
        provider,
        std::io::stdin(),
        |key| std::env::var(key).ok(),
        |pipe, request| crate::control::request(pipe, request),
        HELPER_TIMEOUT,
    );
    if let Some(output) = output {
        println!("{output}");
    }
    0
}

/// The helper's whole job behind its deadline: a stalled stdin or host
/// yields nothing in time rather than holding up the provider.
fn context_output(
    provider: String,
    input: impl Read + Send + 'static,
    env: impl Fn(&str) -> Option<String> + Send + 'static,
    request: impl Fn(&str, &KanbanRequest) -> std::io::Result<OpenReply> + Send + 'static,
    deadline: std::time::Duration,
) -> Option<serde_json::Value> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            input.take(INPUT_LIMIT + 1).read_to_end(&mut bytes).ok()?;
            if bytes.len() as u64 > INPUT_LIMIT {
                return None;
            }
            let input = serde_json::from_slice(&bytes).ok()?;
            let (pipe, kanban) = request_for(&provider, &input, env)?;
            hook_output(request(&pipe, &kanban).ok()?)
        })();
        let _ = tx.send(result);
    });
    rx.recv_timeout(deadline).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routed(key: &str) -> Option<String> {
        match key {
            "FOREMAN_PIPE" => Some("foreman-instance".into()),
            "FOREMAN_PROJECT_ID" => Some("p1".into()),
            "FOREMAN_TERMINAL_ID" => Some("t2".into()),
            _ => None,
        }
    }

    fn compact_event() -> std::io::Cursor<Vec<u8>> {
        std::io::Cursor::new(
            br#"{"hook_event_name":"SessionStart","source":"compact","session_id":"root"}"#
                .to_vec(),
        )
    }

    /// Never returns data, like a provider that keeps stdin open.
    struct Stalled;
    impl Read for Stalled {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(std::time::Duration::from_secs(30));
            Ok(0)
        }
    }

    const DEADLINE: std::time::Duration = std::time::Duration::from_millis(200);

    fn brief(_: &str, _: &KanbanRequest) -> std::io::Result<OpenReply> {
        Ok(OpenReply {
            ok: true,
            history: Some(vec!["Recovered brief".into()]),
            ..Default::default()
        })
    }

    #[test]
    fn a_stalled_stdin_or_host_yields_nothing_within_the_deadline() {
        let started = std::time::Instant::now();
        assert!(context_output("claude".into(), Stalled, routed, brief, DEADLINE).is_none());
        let hung = |_: &str, _: &KanbanRequest| {
            std::thread::sleep(std::time::Duration::from_secs(30));
            brief("", &KanbanRequest::default())
        };
        assert!(context_output("claude".into(), compact_event(), routed, hung, DEADLINE).is_none());
        assert!(started.elapsed() < DEADLINE * 2 + std::time::Duration::from_secs(1));
    }

    #[test]
    fn malformed_or_oversized_input_never_reaches_the_host() {
        let refuse = |_: &str, _: &KanbanRequest| -> std::io::Result<OpenReply> {
            panic!("no request for unusable input")
        };
        for input in [b"{not json".to_vec(), vec![b' '; INPUT_LIMIT as usize + 1]] {
            let input = std::io::Cursor::new(input);
            assert!(context_output("claude".into(), input, routed, refuse, DEADLINE).is_none());
        }
        let unavailable = |_: &str, _: &KanbanRequest| -> std::io::Result<OpenReply> {
            Err(std::io::ErrorKind::NotFound.into())
        };
        let event = compact_event();
        assert!(context_output("claude".into(), event, routed, unavailable, DEADLINE).is_none());
    }

    #[test]
    fn duplicate_invocations_only_ever_ask_for_read_only_context() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        for _ in 0..2 {
            let log = seen.clone();
            let record = move |pipe: &str, request: &KanbanRequest| {
                let operation = request.assignment.as_ref().unwrap().operation.clone();
                log.lock().unwrap().push((pipe.to_string(), operation));
                brief(pipe, request)
            };
            let output =
                context_output("claude".into(), compact_event(), routed, record, DEADLINE).unwrap();
            assert_eq!(
                output["hookSpecificOutput"]["additionalContext"],
                "Recovered brief"
            );
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(
            seen.iter()
                .all(|(pipe, op)| pipe == "foreman-instance" && op == "context")
        );
    }

    #[test]
    fn compaction_context_keeps_capacity_while_waits_fill_the_general_pool() {
        let pools = Pools::default();
        let held: Vec<_> = (0..4)
            .map(|_| RequestPermit::acquire_in(&pools, "status").unwrap())
            .collect();
        assert!(RequestPermit::acquire_in(&pools, "status").is_err());
        assert!(RequestPermit::acquire_in(&pools, "context").is_ok());
        drop(held);
        assert!(RequestPermit::acquire_in(&pools, "status").is_ok());
    }

    #[test]
    fn request_capacity_is_released_even_when_a_worker_fails() {
        let capacity = std::sync::atomic::AtomicUsize::new(0);
        let mut permits = Vec::new();
        for _ in 0..4 {
            permits.push(RequestPermit::acquire_from(&capacity).unwrap());
        }
        assert!(RequestPermit::acquire_from(&capacity).is_err());
        permits.pop();
        assert!(RequestPermit::acquire_from(&capacity).is_ok());
        drop(permits);
        assert_eq!(capacity.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn only_root_compaction_with_explicit_instance_routing_requests_context() {
        let env = |key: &str| match key {
            "FOREMAN_PIPE" => Some("foreman-instance".into()),
            "FOREMAN_PROJECT_ID" => Some("p1".into()),
            "FOREMAN_TERMINAL_ID" => Some("t2".into()),
            _ => None,
        };
        let input = serde_json::json!({"hook_event_name":"SessionStart", "source":"compact", "session_id":"root-session"});
        let (pipe, request) = request_for("codex", &input, env).unwrap();
        assert_eq!(pipe, "foreman-instance");
        assert_eq!(request.assignment.unwrap().session, "root-session");
        assert!(request_for("codex", &input, |_| None).is_none());
        let mut child = input.clone();
        child["agent_id"] = "child".into();
        assert!(request_for("codex", &child, env).is_none());
        let mut fork = input.clone();
        fork["source"] = "fork".into();
        assert!(request_for("codex", &fork, env).is_none());
        assert!(hook_output(crate::control::OpenReply::err("unavailable")).is_none());
        let output = hook_output(crate::control::OpenReply {
            ok: true,
            history: Some(vec!["Recovered brief".into()]),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            output["hookSpecificOutput"]["additionalContext"],
            "Recovered brief"
        );
    }
}
