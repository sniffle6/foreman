//! Off-thread probes of a Remote. Reachability is `ssh -G` (where the target
//! really goes) then a TCP connect; Test is one `BatchMode` login that reports
//! the shell, `curl` and the agent CLIs. Parsers are pure and tested; the
//! workers run on their own threads and wake the UI with `request_repaint`.
//! Nothing here blocks the frame path: a caller only reads a cache or a
//! channel.

use std::collections::HashMap;
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::remote::RemoteShell;

/// How long a reachability answer is reused before the next caller re-probes.
pub const REACH_TTL: Duration = Duration::from_secs(10);
/// Per-address TCP connect budget. Short: a LAN or tailnet PC answers in ms.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// `ssh -G` reads local config only; this only guards a wedged ssh.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Each Test stage: a login plus, on Posix, the user's login startup files.
const TEST_TIMEOUT: Duration = Duration::from_secs(20);
/// The agent CLIs Test looks for, in report order.
pub const AGENTS: [&str; 3] = ["claude", "codex", "grok"];
/// Prefix on every line the Test scripts print, so motd and startup-file
/// noise on the Remote is never mistaken for a result.
const MARK: &str = "foreman-probe";

// ---------------------------------------------------------------------------
// Reachability
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The first probe is in flight; nothing is known yet.
    Checking,
    Reachable,
    Offline,
    /// The probe cannot tell; the string says why.
    Unknown(String),
}

/// Where `ssh -G` says a target goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Direct {
        host: String,
        port: u16,
    },
    /// A `ProxyJump` or `ProxyCommand` hides the real hop from a TCP probe.
    Proxied,
}

/// Parse `ssh -G` output (`keyword value` per line, keywords lowercase).
/// `None` when it names no host or an unreadable port.
pub fn parse_ssh_g(out: &str) -> Option<Endpoint> {
    let mut host = None;
    let mut port = None;
    let mut proxied = false;
    for line in out.lines() {
        let Some((key, value)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "hostname" => host = Some(value.to_string()),
            "port" => port = Some(value.parse::<u16>().ok()?),
            "proxyjump" | "proxycommand" if !value.eq_ignore_ascii_case("none") => proxied = true,
            _ => {}
        }
    }
    if proxied {
        return Some(Endpoint::Proxied);
    }
    let host = host.filter(|h| !h.is_empty())?;
    Some(Endpoint::Direct {
        host,
        port: port.unwrap_or(22),
    })
}

#[derive(Debug, Default)]
struct Entry {
    /// Resolved once per target; a failed resolve is retried on the next probe.
    endpoint: Option<Endpoint>,
    reach: Option<Reach>,
    checked: Option<Instant>,
    in_flight: bool,
}

/// A probe the caller must run off-thread, then hand back to `finish`.
#[derive(Debug, PartialEq)]
pub struct Job {
    pub target: String,
    /// The cached `ssh -G` answer; `None` means resolve first.
    pub endpoint: Option<Endpoint>,
}

/// Per-target reachability answers. Pure: the caller supplies the clock and
/// runs the jobs.
#[derive(Debug, Default)]
pub struct ReachCache {
    entries: HashMap<String, Entry>,
}

impl ReachCache {
    /// The current answer for `target`, and a job when it is stale and no
    /// probe is already running. A stale answer keeps showing while the next
    /// one runs, so a row never flickers back to Checking.
    pub fn get(&mut self, target: &str, now: Instant) -> (Reach, Option<Job>) {
        let e = self.entries.entry(target.to_string()).or_default();
        let stale = e
            .checked
            .is_none_or(|at| now.saturating_duration_since(at) >= REACH_TTL);
        let job = (stale && !e.in_flight).then(|| {
            e.in_flight = true;
            Job {
                target: target.to_string(),
                endpoint: e.endpoint.clone(),
            }
        });
        (e.reach.clone().unwrap_or(Reach::Checking), job)
    }

    /// The SSH port the last resolve found for `target`; `None` before the
    /// first resolve or behind a proxy hop.
    pub fn port(&self, target: &str) -> Option<u16> {
        match self.entries.get(target)?.endpoint.as_ref()? {
            Endpoint::Direct { port, .. } => Some(*port),
            Endpoint::Proxied => None,
        }
    }

    pub fn finish(&mut self, target: &str, endpoint: Option<Endpoint>, reach: Reach, now: Instant) {
        let e = self.entries.entry(target.to_string()).or_default();
        if endpoint.is_some() {
            e.endpoint = endpoint;
        }
        e.reach = Some(reach);
        e.checked = Some(now);
        e.in_flight = false;
    }
}

fn cache() -> &'static Mutex<ReachCache> {
    static CACHE: OnceLock<Mutex<ReachCache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// The cached reachability of `target`, starting a background probe when it
/// is stale. Cheap enough to call per frame for every visible Remote: a lock
/// and a map lookup. Probes only happen while something on screen asks, and
/// a repaint is scheduled so an open pane refreshes as answers expire.
pub fn reach(ctx: &egui::Context, target: &str) -> Reach {
    let (reach, job) = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(target, Instant::now());
    if let Some(job) = job {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let (endpoint, reach) = run_reach(&job);
            cache().lock().unwrap_or_else(|e| e.into_inner()).finish(
                &job.target,
                endpoint,
                reach,
                Instant::now(),
            );
            ctx.request_repaint();
        });
    }
    ctx.request_repaint_after(REACH_TTL);
    reach
}

/// The SSH port [`reach`] probes for `target`, once it has resolved it.
pub fn ssh_port(target: &str) -> Option<u16> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .port(target)
}

fn run_reach(job: &Job) -> (Option<Endpoint>, Reach) {
    let endpoint = match &job.endpoint {
        Some(e) => e.clone(),
        None => match run(&["-G", "--", &job.target], RESOLVE_TIMEOUT) {
            Ok(out) if out.code == Some(0) => match parse_ssh_g(&out.stdout) {
                Some(e) => e,
                None => return (None, Reach::Unknown("ssh -G named no host".into())),
            },
            Ok(out) => {
                return (
                    None,
                    Reach::Unknown(last_line(&out.stderr, "ssh -G failed")),
                );
            }
            Err(e) => return (None, Reach::Unknown(e)),
        },
    };
    let reach = match &endpoint {
        Endpoint::Proxied => Reach::Unknown("through a proxy hop the probe cannot see past".into()),
        Endpoint::Direct { host, port } => tcp_reach(host, *port),
    };
    (Some(endpoint), reach)
}

fn tcp_reach(host: &str, port: u16) -> Reach {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return Reach::Offline; // the name does not resolve from here
    };
    for addr in addrs {
        if TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).is_ok() {
            return Reach::Reachable;
        }
    }
    Reach::Offline
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct TestReport {
    pub shell: RemoteShell,
    /// `$SHELL` on a Posix Remote.
    pub login_shell: Option<String>,
    pub curl: bool,
    /// Agent CLIs from `AGENTS` found on the Remote's PATH.
    pub agents: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TestOutcome {
    Ok(TestReport),
    /// `BatchMode` refused: a password, a key passphrase, or an unknown host
    /// key. Only an interactive Remote Session can answer those.
    NeedsLogin,
    Failed(String),
}

impl TestOutcome {
    /// One plain line for the pane.
    pub fn summary(&self) -> String {
        match self {
            TestOutcome::NeedsLogin => "Needs an interactive login (a password, passphrase or new host key): open a Remote Session on it once and log in there".into(),
            TestOutcome::Failed(e) => format!("Test failed: {e}"),
            TestOutcome::Ok(r) => {
                let shell = match (&r.shell, &r.login_shell) {
                    (RemoteShell::Posix, Some(path)) => format!("SH ({path})"),
                    (shell, _) => crate::remotes_pane::shell_label(*shell).to_string(),
                };
                let curl = if r.curl {
                    "curl found"
                } else {
                    "no curl (chat and badges need it)"
                };
                let missing: Vec<&str> = AGENTS
                    .iter()
                    .copied()
                    .filter(|a| !r.agents.iter().any(|f| f == a))
                    .collect();
                let agents = match (r.agents.is_empty(), missing.is_empty()) {
                    (true, _) => "no agent CLIs on PATH".to_string(),
                    (false, true) => format!("agents: {}", r.agents.join(", ")),
                    (false, false) => format!(
                        "agents: {} ({} not found)",
                        r.agents.join(", "),
                        missing.join(", ")
                    ),
                };
                format!("Login works · {shell} · {curl} · {agents}")
            }
        }
    }
}

/// Which family the login shell is, from the stage-1 echo. `None` when the
/// marker never came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Windows,
    Posix,
}

/// Stage 1 runs through whatever login shell sshd starts. cmd expands
/// `%OS%`, PowerShell expands `$env:OS`, and a POSIX shell expands neither to
/// `Windows_NT`.
const STAGE1: &str = "echo foreman-probe %OS% $env:OS";

pub fn parse_family(stdout: &str) -> Option<Family> {
    if !stdout.contains(MARK) {
        return None;
    }
    Some(if stdout.contains("Windows_NT") {
        Family::Windows
    } else {
        Family::Posix
    })
}

/// The stage-2 script for a family, already wrapped as a remote line. Both
/// print `foreman-probe have|miss <tool>`; Posix also prints its `$SHELL`
/// and checks through a login shell, because that is the PATH a launch gets.
pub fn stage2_line(family: Family) -> String {
    match family {
        Family::Windows => {
            let mut script = String::from("$ErrorActionPreference = 'SilentlyContinue'\n");
            script.push_str("foreach ($c in 'pwsh','curl.exe'");
            for a in AGENTS {
                script.push_str(&format!(",'{a}'"));
            }
            script.push_str(&format!(
                ") {{ if (Get-Command $c) {{ \"{MARK} have $c\" }} else {{ \"{MARK} miss $c\" }} }}\n"
            ));
            // 5.1 is on every Windows; it is the one that can look for pwsh.
            format!(
                "powershell -NoLogo -NonInteractive -EncodedCommand {}",
                crate::remote::utf16le_b64(&script)
            )
        }
        Family::Posix => {
            let script = format!(
                "echo \"{MARK} shell ${{SHELL:-sh}}\"\nexec \"${{SHELL:-sh}}\" -lic 'for c in curl {}; do if command -v \"$c\" >/dev/null 2>&1; then echo \"{MARK} have $c\"; else echo \"{MARK} miss $c\"; fi; done'\n",
                AGENTS.join(" ")
            );
            crate::remote::posix_line(&script)
        }
    }
}

/// Read stage 2's marked lines into a report. `None` when none came back.
pub fn parse_tools(family: Family, stdout: &str) -> Option<TestReport> {
    let mut have = Vec::new();
    let mut login_shell = None;
    let mut any = false;
    for line in stdout.lines() {
        let Some(rest) = line.trim().strip_prefix(MARK) else {
            continue;
        };
        let rest = rest.trim();
        if let Some(tool) = rest.strip_prefix("have ") {
            any = true;
            have.push(tool.trim().to_ascii_lowercase());
        } else if rest.starts_with("miss ") {
            any = true;
        } else if let Some(path) = rest.strip_prefix("shell ") {
            login_shell = Some(path.trim().to_string());
        }
    }
    if !any {
        return None;
    }
    let has = |t: &str| have.iter().any(|h| h == t);
    let shell = match family {
        Family::Posix => RemoteShell::Posix,
        Family::Windows if has("pwsh") => RemoteShell::Pwsh,
        Family::Windows => RemoteShell::WindowsPowerShell,
    };
    Some(TestReport {
        shell,
        login_shell: login_shell.filter(|_| family == Family::Posix),
        curl: has("curl") || has("curl.exe"),
        agents: AGENTS
            .iter()
            .filter(|a| has(a))
            .map(|a| a.to_string())
            .collect(),
    })
}

/// A stage that did not print its marker: tell a refused login from any
/// other failure. `BatchMode` turns every prompt into one of these errors.
pub fn classify_failure(stderr: &str) -> TestOutcome {
    const PROMPTS: [&str; 4] = [
        "Permission denied",
        "Host key verification failed",
        "passphrase",
        "Too many authentication failures",
    ];
    if PROMPTS.iter().any(|p| stderr.contains(p)) {
        TestOutcome::NeedsLogin
    } else {
        TestOutcome::Failed(last_line(stderr, "ssh printed nothing"))
    }
}

/// Run Test for `target` on its own thread; the answer arrives on the
/// returned channel and wakes the UI.
pub fn start_test(ctx: &egui::Context, target: &str) -> mpsc::Receiver<TestOutcome> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    let target = target.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(run_test(&target));
        ctx.request_repaint();
    });
    rx
}

fn run_test(target: &str) -> TestOutcome {
    let stage = |line: &str| {
        run(
            &[
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=8",
                "--",
                target,
                line,
            ],
            TEST_TIMEOUT,
        )
    };
    let first = match stage(STAGE1) {
        Ok(out) => out,
        Err(e) => return TestOutcome::Failed(e),
    };
    let Some(family) = parse_family(&first.stdout) else {
        return classify_failure(&first.stderr);
    };
    let second = match stage(&stage2_line(family)) {
        Ok(out) => out,
        Err(e) => return TestOutcome::Failed(e),
    };
    match parse_tools(family, &second.stdout) {
        Some(report) => TestOutcome::Ok(report),
        None => TestOutcome::Failed(format!(
            "login works, but listing tools failed: {}",
            last_line(&second.stderr, "no output")
        )),
    }
}

// ---------------------------------------------------------------------------
// ssh-config Host names
// ---------------------------------------------------------------------------

/// `Host` names from an ssh config, in file order, without patterns
/// (`*`, `?`, `!`). `Include` is not followed.
pub fn parse_config_hosts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else {
            continue;
        };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for name in rest.trim_start_matches(['=', ' ', '\t']).split_whitespace() {
            if name.starts_with('#') {
                break;
            }
            if name.contains(['*', '?', '!']) || out.iter().any(|h| h == name) {
                continue;
            }
            out.push(name.to_string());
        }
    }
    out
}

/// The user's `~/.ssh/config` Host names; empty when there is none. A small
/// file read, done once when the add form opens.
pub fn config_hosts() -> Vec<String> {
    let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) else {
        return Vec::new();
    };
    let path = std::path::Path::new(&home).join(".ssh").join("config");
    std::fs::read_to_string(path)
        .map(|t| parse_config_hosts(&t))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Running ssh
// ---------------------------------------------------------------------------

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// The last non-blank line of `text`, or `fallback`.
fn last_line(text: &str, fallback: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Run the system `ssh` with `args`, no window, no stdin, killed at `timeout`.
/// Blocks: call it from a worker thread only.
fn run(args: &[&str], timeout: Duration) -> Result<Output, String> {
    let mut cmd = Command::new("ssh");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("could not run ssh: {e}"))?;
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let mut buf = Vec::new();
                let _ = r.by_ref().take(64 * 1024).read_to_end(&mut buf);
                s = String::from_utf8_lossy(&buf).into_owned();
            }
            let _ = tx.send(s);
        });
        rx
    };
    let out_rx = pipe(child.stdout.take().map(|r| Box::new(r) as _));
    let err_rx = pipe(child.stderr.take().map(|r| Box::new(r) as _));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("ssh did not finish in {}s", timeout.as_secs()));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("ssh: {e}"));
            }
        }
    };
    Ok(Output {
        code: status.code(),
        stdout: out_rx.recv().unwrap_or_default(),
        stderr: err_rx.recv().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_g_direct_reads_host_and_port() {
        let out = "user andy\nhostname 192.168.1.20\nport 2222\nproxyjump none\n";
        assert_eq!(
            parse_ssh_g(out),
            Some(Endpoint::Direct {
                host: "192.168.1.20".into(),
                port: 2222
            })
        );
    }

    #[test]
    fn ssh_g_proxy_hop_is_proxied() {
        assert_eq!(
            parse_ssh_g("hostname desk\nport 22\nproxyjump bastion\n"),
            Some(Endpoint::Proxied)
        );
        assert_eq!(
            parse_ssh_g("hostname desk\nport 22\nproxycommand nc %h %p\n"),
            Some(Endpoint::Proxied)
        );
    }

    #[test]
    fn ssh_g_without_host_or_with_bad_port_is_none() {
        assert_eq!(parse_ssh_g("port 22\n"), None);
        assert_eq!(parse_ssh_g("hostname desk\nport banana\n"), None);
        assert_eq!(parse_ssh_g(""), None);
    }

    #[test]
    fn ssh_g_tolerates_crlf_and_default_port() {
        assert_eq!(
            parse_ssh_g("hostname desk\r\n"),
            Some(Endpoint::Direct {
                host: "desk".into(),
                port: 22
            })
        );
    }

    #[test]
    fn reach_cache_probes_once_then_reuses_until_stale() {
        let mut c = ReachCache::default();
        let t0 = Instant::now();
        let (r, job) = c.get("desk", t0);
        assert_eq!(r, Reach::Checking);
        assert_eq!(
            job,
            Some(Job {
                target: "desk".into(),
                endpoint: None
            })
        );
        // In flight: a second caller does not start another probe.
        assert!(c.get("desk", t0).1.is_none());

        let ep = Endpoint::Direct {
            host: "10.0.0.2".into(),
            port: 22,
        };
        c.finish("desk", Some(ep.clone()), Reach::Reachable, t0);
        let (r, job) = c.get("desk", t0 + Duration::from_secs(1));
        assert_eq!((r, job), (Reach::Reachable, None));

        // Stale: the old answer still shows, and the job reuses the endpoint.
        let (r, job) = c.get("desk", t0 + REACH_TTL);
        assert_eq!(r, Reach::Reachable);
        assert_eq!(job.unwrap().endpoint, Some(ep));
    }

    #[test]
    fn reach_cache_keeps_a_resolved_endpoint_when_a_later_resolve_is_absent() {
        let mut c = ReachCache::default();
        let t0 = Instant::now();
        c.get("desk", t0);
        c.finish("desk", Some(Endpoint::Proxied), Reach::Offline, t0);
        c.finish("desk", None, Reach::Offline, t0);
        let (_, job) = c.get("desk", t0 + REACH_TTL);
        assert_eq!(job.unwrap().endpoint, Some(Endpoint::Proxied));
    }

    #[test]
    fn family_from_each_login_shell() {
        // cmd: %OS% expands, $env:OS is literal.
        assert_eq!(
            parse_family("foreman-probe Windows_NT $env:OS\r\n"),
            Some(Family::Windows)
        );
        // PowerShell: echo prints each argument on its own line.
        assert_eq!(
            parse_family("foreman-probe\r\n%OS%\r\nWindows_NT\r\n"),
            Some(Family::Windows)
        );
        // bash: neither expands to Windows_NT.
        assert_eq!(
            parse_family("Welcome to Debian\nforeman-probe %OS% :OS\n"),
            Some(Family::Posix)
        );
        assert_eq!(parse_family(""), None);
    }

    #[test]
    fn windows_tools_pick_pwsh_when_present() {
        let out = "foreman-probe have pwsh\r\nforeman-probe have curl.exe\r\nforeman-probe have claude\r\nforeman-probe miss codex\r\nforeman-probe miss grok\r\n";
        let r = parse_tools(Family::Windows, out).unwrap();
        assert_eq!(r.shell, RemoteShell::Pwsh);
        assert!(r.curl);
        assert_eq!(r.agents, vec!["claude".to_string()]);
        assert_eq!(r.login_shell, None);

        let out = "foreman-probe miss pwsh\r\nforeman-probe miss curl.exe\r\n";
        let r = parse_tools(Family::Windows, out).unwrap();
        assert_eq!(r.shell, RemoteShell::WindowsPowerShell);
        assert!(!r.curl);
    }

    #[test]
    fn posix_tools_ignore_startup_noise_and_read_the_login_shell() {
        let out = "foreman-probe shell /bin/bash\nbash: no job control in this shell\nLast login: Tue\nforeman-probe have curl\nforeman-probe miss claude\nforeman-probe have codex\nforeman-probe miss grok\n";
        let r = parse_tools(Family::Posix, out).unwrap();
        assert_eq!(r.shell, RemoteShell::Posix);
        assert_eq!(r.login_shell.as_deref(), Some("/bin/bash"));
        assert!(r.curl);
        assert_eq!(r.agents, vec!["codex".to_string()]);
    }

    #[test]
    fn tools_without_any_marker_are_none() {
        assert_eq!(
            parse_tools(Family::Posix, "foreman-probe shell /bin/sh\n"),
            None
        );
        assert_eq!(parse_tools(Family::Windows, "garbage"), None);
    }

    #[test]
    fn batchmode_refusals_need_a_login_and_others_fail_plainly() {
        for err in [
            "andy@desk: Permission denied (publickey,password).",
            "Host key verification failed.",
            "Load key \"id\": incorrect passphrase supplied",
        ] {
            assert_eq!(classify_failure(err), TestOutcome::NeedsLogin, "{err}");
        }
        assert_eq!(
            classify_failure("ssh: connect to host desk port 22: Connection timed out\r\n"),
            TestOutcome::Failed("ssh: connect to host desk port 22: Connection timed out".into())
        );
        assert_eq!(
            classify_failure(""),
            TestOutcome::Failed("ssh printed nothing".into())
        );
    }

    #[test]
    fn stage2_lines_carry_no_quote_or_space_from_the_script() {
        let posix = stage2_line(Family::Posix);
        assert!(posix.starts_with("sh -c \"$(printf %s "), "{posix}");
        let win = stage2_line(Family::Windows);
        let b64 = win
            .strip_prefix("powershell -NoLogo -NonInteractive -EncodedCommand ")
            .unwrap();
        assert!(
            b64.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))
        );
    }

    #[test]
    fn summary_names_shell_curl_and_agents() {
        let r = TestOutcome::Ok(TestReport {
            shell: RemoteShell::Posix,
            login_shell: Some("/bin/zsh".into()),
            curl: false,
            agents: vec!["claude".into()],
        });
        assert_eq!(
            r.summary(),
            "Login works · SH (/bin/zsh) · no curl (chat and badges need it) · agents: claude (codex, grok not found)"
        );
        let r = TestOutcome::Ok(TestReport {
            shell: RemoteShell::Pwsh,
            login_shell: None,
            curl: true,
            agents: vec![],
        });
        assert_eq!(
            r.summary(),
            "Login works · PowerShell · curl found · no agent CLIs on PATH"
        );
        assert!(
            TestOutcome::NeedsLogin
                .summary()
                .contains("open a Remote Session on it once and log in there")
        );
    }

    #[test]
    fn config_hosts_skip_patterns_and_duplicates() {
        let cfg = "# comment\nHost desk laptop\n  HostName 10.0.0.2\nHost *.lan !nope\nhost=garage\nHost desk\nMatch host foo\nHost   pi # trailing\nHostName notahost\n";
        assert_eq!(
            parse_config_hosts(cfg),
            vec!["desk", "laptop", "garage", "pi"]
        );
    }
}
