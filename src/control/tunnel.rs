//! The Tunnel listener: the way back from a Remote Session's agent. One
//! loopback HTTP listener per foreman instance, bound on the first Remote
//! Session's spawn; each Remote Session's `ssh -R` forwards a port on its
//! Remote to it. A per-Session bearer token names the caller, so the listener
//! never trusts anything the request says about who sent it.
//!
//! It is a second front door onto the same GUI channel as the pipe
//! ([`CtrlMsg`]): it builds ordinary `ChatRequest`/`StatusRequest` values and
//! never touches the pipe's wire format. Spec: "The way back" in
//! `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

use std::collections::HashMap;
use std::io::Write;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::{ChatRequest, CtrlMsg, OpenReply, REPLY_TIMEOUT, StatusRequest, read_until};
use crate::remote::RemoteShell;

/// Request line plus headers. curl sends a few hundred bytes.
const MAX_HEAD: usize = 8 * 1024;

/// Largest body (`POST /chat` text). Matches the peer listener's line cap.
const MAX_BODY: usize = 64 * 1024;

/// One deadline for reading a whole request, head and body. A client that
/// drips bytes or stops mid-request is answered 408 when it passes.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

const MAX_INFLIGHT: usize = 16;

/// Most chat lines one `GET /chat?history=N` returns.
const MAX_HISTORY: usize = 1000;

/// Remote ports a Tunnel forwards from. Below both Linux's (32768+) and
/// Windows' (49152+) ephemeral ranges, so an outgoing connection on the
/// Remote rarely holds one; a collision ends `ssh` at once through
/// `ExitOnForwardFailure`, visibly, and the next launch picks again.
const REMOTE_PORTS: std::ops::RangeInclusive<u16> = 20000..=29999;

/// Who a token speaks for: the Project and Member id its Session had at spawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Owner {
    pub project: String,
    pub terminal: String,
}

/// The listener, its token table, and the GUI channel requests go down. Lives
/// in the egui context ([`install`]/[`hub`]) for the app's run; the socket
/// binds on the first [`Hub::lease`].
pub struct Hub {
    tx: mpsc::Sender<CtrlMsg>,
    ctx: eframe::egui::Context,
    owners: Mutex<HashMap<String, Owner>>,
    port: Mutex<Option<u16>>,
    io_timeout: Duration,
}

/// A Remote Session's Tunnel: its token (revoked on drop) and both ports.
pub struct Lease {
    pub token: Token,
    pub local_port: u16,
    pub remote_port: u16,
}

/// A live token. Dropping it revokes it, so it dies with whatever holds it:
/// the Session drops it when its process exits, or when the Session goes.
pub struct Token {
    hub: Arc<Hub>,
    value: String,
}

impl Token {
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        if let Ok(mut owners) = self.hub.owners.lock() {
            owners.remove(&self.value);
        }
    }
}

fn ctx_id() -> eframe::egui::Id {
    eframe::egui::Id::new("foreman::tunnel")
}

/// Make the Tunnel available to the window manager. Binds nothing yet.
pub fn install(ctx: &eframe::egui::Context, tx: mpsc::Sender<CtrlMsg>) {
    let hub = Hub::new(tx, ctx.clone(), IO_TIMEOUT);
    ctx.data_mut(|d| d.insert_temp(ctx_id(), hub));
}

/// The installed Tunnel; `None` before [`install`] (unit tests, mostly), and
/// then a Remote Session launches with no way back.
pub fn hub(ctx: &eframe::egui::Context) -> Option<Arc<Hub>> {
    ctx.data_mut(|d| d.get_temp(ctx_id()))
}

impl Hub {
    fn new(
        tx: mpsc::Sender<CtrlMsg>,
        ctx: eframe::egui::Context,
        io_timeout: Duration,
    ) -> Arc<Hub> {
        Arc::new(Hub {
            tx,
            ctx,
            owners: Mutex::new(HashMap::new()),
            port: Mutex::new(None),
            io_timeout,
        })
    }

    /// Mint a token for the Session `terminal` of `project`, starting the
    /// listener if this is the first.
    pub fn lease(self: &Arc<Self>, project: &str, terminal: &str) -> std::io::Result<Lease> {
        let local_port = self.port()?;
        let mut raw = [0u8; 34];
        os_random(&mut raw)?;
        let value: String = raw[..32].iter().map(|b| format!("{b:02x}")).collect();
        let span = u32::from(*REMOTE_PORTS.end() - *REMOTE_PORTS.start()) + 1;
        let pick = u32::from(u16::from_le_bytes([raw[32], raw[33]])) % span;
        let remote_port = *REMOTE_PORTS.start() + pick as u16;
        self.owners.lock().map_err(|_| poisoned())?.insert(
            value.clone(),
            Owner {
                project: project.into(),
                terminal: terminal.into(),
            },
        );
        Ok(Lease {
            token: Token {
                hub: self.clone(),
                value,
            },
            local_port,
            remote_port,
        })
    }

    /// The listener's port once bound.
    #[cfg(test)]
    pub fn bound_port(&self) -> Option<u16> {
        *self.port.lock().unwrap()
    }

    fn owner(&self, token: &str) -> Option<Owner> {
        self.owners.lock().ok()?.get(token).cloned()
    }

    /// The listener's port, binding `127.0.0.1:0` the first time. Loopback
    /// only: `ssh -R` delivers to it from the seat PC's side, and nothing on
    /// the LAN should ever reach it.
    fn port(self: &Arc<Self>) -> std::io::Result<u16> {
        let mut port = self.port.lock().map_err(|_| poisoned())?;
        if let Some(p) = *port {
            return Ok(p);
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let p = listener.local_addr()?.port();
        let hub = self.clone();
        std::thread::Builder::new()
            .name("tunnel".into())
            .spawn(move || hub.serve(listener))?;
        *port = Some(p);
        Ok(p)
    }

    fn serve(self: Arc<Self>, listener: TcpListener) {
        let inflight = Arc::new(AtomicUsize::new(0));
        /// Gives the slot back even if the handler panics.
        struct Slot(Arc<AtomicUsize>);
        impl Drop for Slot {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }
        for conn in listener.incoming() {
            let Ok(conn) = conn else {
                // No hot loop on a persistent accept error.
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            if inflight.load(Ordering::Relaxed) >= MAX_INFLIGHT {
                let _ = (&conn).write_all(&response(503, "foreman: tunnel busy\n"));
                continue;
            }
            inflight.fetch_add(1, Ordering::Relaxed);
            let (hub, slot) = (self.clone(), Slot(inflight.clone()));
            std::thread::spawn(move || {
                let _slot = slot;
                hub.handle(conn);
            });
        }
    }

    fn handle(&self, conn: TcpStream) {
        let deadline = Instant::now() + self.io_timeout;
        if conn.set_nonblocking(false).is_err()
            || conn.set_write_timeout(Some(self.io_timeout)).is_err()
        {
            return;
        }
        let Some(out) = self.exchange(&conn, deadline) else {
            return;
        };
        let _ = (&conn).write_all(&out);
        // Same as the peer listener: closing with unread request bytes resets
        // the connection on Windows and the client loses the reply. FIN, then
        // swallow a bounded remainder.
        let _ = conn.shutdown(std::net::Shutdown::Write);
        let linger = Instant::now() + Duration::from_secs(1);
        read_until(&conn, linger, |b| b.len() >= 1 << 20);
    }

    /// Read one request and produce the bytes to answer it with. `None` = the
    /// client connected and left without sending anything.
    fn exchange(&self, conn: &TcpStream, deadline: Instant) -> Option<Vec<u8>> {
        let mut buf = read_until(conn, deadline, |b| {
            head_end(b).is_some() || b.len() >= MAX_HEAD
        });
        if buf.is_empty() {
            return None;
        }
        let refuse = |r: Refusal| Some(response(r.status, &format!("{}\n", r.msg)));
        let (head, body_at) = match parse_head(&buf) {
            Ok(Some(h)) => h,
            Ok(None) => return refuse(timeout()),
            Err(r) => return refuse(r),
        };
        // Token before routing: without one, the listener says nothing about
        // which routes exist (only a malformed head is refused ahead of it).
        let Some(owner) = head.bearer.as_deref().and_then(|t| self.owner(t)) else {
            return refuse(Refusal::new(
                401,
                "missing or unknown token (Authorization: Bearer $FOREMAN_TOKEN)",
            ));
        };
        let route = match route(&head.method, &head.path) {
            Ok(r) => r,
            Err(r) => return refuse(r),
        };
        let len = match (route, head.content_length) {
            (_, Some(n)) if n > MAX_BODY => {
                return refuse(Refusal::new(413, format!("body over {MAX_BODY} bytes")));
            }
            (Route::Post, None) => return refuse(Refusal::new(411, "POST needs Content-Length")),
            (_, n) => n.unwrap_or(0),
        };
        if head.expect_continue && len > 0 {
            let _ = (&*conn).write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        }
        let mut body = buf.split_off(body_at);
        if body.len() < len {
            let need = len - body.len();
            body.extend(read_until(conn, deadline, |b| b.len() >= need));
            if body.len() < len {
                return refuse(timeout());
            }
        }
        body.truncate(len);
        let call = match build_call(route, &head.query, &body, &owner) {
            Ok(c) => c,
            Err(r) => return refuse(r),
        };
        Some(self.dispatch(call))
    }

    /// Hand the request to the GUI exactly as the pipe does, and wait for it.
    fn dispatch(&self, call: Call) -> Vec<u8> {
        let (rtx, rrx) = mpsc::channel();
        let now = Instant::now();
        let msg = match call {
            Call::Chat(r) => CtrlMsg::Chat(r, rtx, now),
            Call::Status(r) => CtrlMsg::Status(r, rtx, now),
        };
        if self.tx.send(msg).is_err() {
            return response(503, "foreman is not accepting requests\n");
        }
        self.ctx.request_repaint();
        match rrx.recv_timeout(REPLY_TIMEOUT) {
            Ok(reply) => reply_response(&reply),
            Err(_) => response(503, "foreman did not respond\n"),
        }
    }
}

fn poisoned() -> std::io::Error {
    std::io::Error::other("tunnel token table poisoned")
}

fn timeout() -> Refusal {
    Refusal::new(
        408,
        format!("incomplete request after {}s", IO_TIMEOUT.as_secs()),
    )
}

/// The OS CSPRNG (BCryptGenRandom, system-preferred RNG).
fn os_random(buf: &mut [u8]) -> std::io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    let len = u32::try_from(buf.len()).map_err(std::io::Error::other)?;
    // SAFETY: the pointer and length describe `buf`, which outlives the call;
    // a null algorithm handle is what the system-preferred-RNG flag requires.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            len,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "BCryptGenRandom failed (NTSTATUS {status:#x})"
        )))
    }
}

/// An HTTP refusal: status code and the one-line reason sent as the body.
#[derive(Debug, PartialEq)]
pub struct Refusal {
    pub status: u16,
    pub msg: String,
}

impl Refusal {
    fn new(status: u16, msg: impl Into<String>) -> Self {
        Refusal {
            status,
            msg: msg.into(),
        }
    }
}

/// The parts of a request head the listener reads; every other header is
/// ignored.
#[derive(Debug, PartialEq, Default)]
pub struct Head {
    pub method: String,
    pub path: String,
    /// Percent-decoded `key=value` pairs, in order (keys may repeat).
    pub query: Vec<(String, String)>,
    pub bearer: Option<String>,
    pub content_length: Option<usize>,
    pub expect_continue: bool,
}

fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Parse a request head. `Ok(None)` = not complete yet; `Ok(Some((head, n)))`
/// = complete, the body starts at `buf[n]`.
pub fn parse_head(buf: &[u8]) -> Result<Option<(Head, usize)>, Refusal> {
    let Some(end) = head_end(buf) else {
        return if buf.len() >= MAX_HEAD {
            Err(Refusal::new(
                431,
                format!("request head over {MAX_HEAD} bytes"),
            ))
        } else {
            Ok(None)
        };
    };
    if end > MAX_HEAD {
        return Err(Refusal::new(
            431,
            format!("request head over {MAX_HEAD} bytes"),
        ));
    }
    let bad = |m: &str| Refusal::new(400, m.to_string());
    let text =
        std::str::from_utf8(&buf[..end - 4]).map_err(|_| bad("request head is not UTF-8"))?;
    let mut lines = text.split("\r\n");
    let line = lines.next().unwrap_or_default();
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad("malformed request line"));
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() || !target.starts_with('/') {
        return Err(bad("malformed request line"));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut head = Head {
        method: method.into(),
        path: path.into(),
        query: parse_query(query),
        ..Default::default()
    };
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| bad("malformed header"))?;
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "authorization" => {
                let (scheme, token) = value.split_once(' ').unwrap_or((value, ""));
                if scheme.eq_ignore_ascii_case("bearer") {
                    head.bearer = Some(token.trim().to_string());
                }
            }
            "content-length" => {
                let n: usize = value.parse().map_err(|_| bad("bad Content-Length"))?;
                if head.content_length.is_some_and(|m| m != n) {
                    return Err(bad("conflicting Content-Length"));
                }
                head.content_length = Some(n);
            }
            "expect" if value.eq_ignore_ascii_case("100-continue") => head.expect_continue = true,
            "expect" => return Err(Refusal::new(417, format!("unsupported Expect: {value}"))),
            "transfer-encoding" => {
                return Err(Refusal::new(
                    501,
                    "chunked bodies are not supported; send Content-Length",
                ));
            }
            _ => {}
        }
    }
    Ok(Some((head, end)))
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

/// `%XX` decoding (`+` stays literal). A malformed escape is kept as typed.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|&c| hex(c)),
                b.get(i + 2).and_then(|&c| hex(c)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The three routes. Nothing else exists: no open, send, close, snapshot or
/// kanban, so a token can post and read, never drive or spawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Route {
    /// `POST /chat`
    Post,
    /// `GET /chat`
    History,
    /// `GET /status`
    Status,
}

pub fn route(method: &str, path: &str) -> Result<Route, Refusal> {
    match (method, path) {
        ("POST", "/chat") => Ok(Route::Post),
        ("GET", "/chat") => Ok(Route::History),
        ("GET", "/status") => Ok(Route::Status),
        (_, "/chat" | "/status") => {
            Err(Refusal::new(405, format!("{method} {path} is not a route")))
        }
        _ => Err(Refusal::new(
            404,
            format!("no route {path} (only POST /chat, GET /chat, GET /status)"),
        )),
    }
}

/// A request ready for the GUI channel.
#[derive(Debug, PartialEq)]
pub enum Call {
    Chat(ChatRequest),
    Status(StatusRequest),
}

/// Turn a routed request into the GUI's own request type, stamped with the
/// token's Session. Project and sender come only from `owner`: a `from`,
/// `project` or `peer` in the query is never read, and `peer` stays `None` so
/// the post takes the normal Member path, not `post_remote`.
pub fn build_call(
    route: Route,
    query: &[(String, String)],
    body: &[u8],
    owner: &Owner,
) -> Result<Call, Refusal> {
    let bad = |m: String| Refusal::new(400, m);
    let get = |k: &str| {
        query
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    let chat = |text, history| ChatRequest {
        cmd: "chat".into(),
        project: Some(owner.project.clone()),
        from: Some(owner.terminal.clone()),
        to: Vec::new(),
        text,
        history,
        re: None,
        now: None,
        peer: None,
    };
    match route {
        Route::Post => {
            let text = std::str::from_utf8(body).map_err(|_| bad("body is not UTF-8".into()))?;
            let text = text.trim_end();
            if text.trim().is_empty() {
                return Err(bad("empty post".into()));
            }
            let mut req = chat(Some(text.to_string()), None);
            req.to = query
                .iter()
                .filter(|(k, _)| k == "to")
                .flat_map(|(_, v)| v.split(','))
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect();
            req.re = get("re")
                .map(|v| v.parse().map_err(|_| bad(format!("bad re: {v:?}"))))
                .transpose()?;
            req.now = get("now").and_then(|v| match v {
                "" | "1" | "true" => Some(true),
                _ => None,
            });
            Ok(Call::Chat(req))
        }
        Route::History => {
            let n = match get("history") {
                None => 20,
                Some(v) => v
                    .parse::<usize>()
                    .map_err(|_| bad(format!("bad history: {v:?}")))?,
            };
            Ok(Call::Chat(chat(None, Some(n.min(MAX_HISTORY)))))
        }
        Route::Status => Ok(Call::Status(StatusRequest {
            cmd: "status".into(),
            project: Some(owner.project.clone()),
            json: matches!(get("json"), Some("" | "1" | "true")),
        })),
    }
}

/// The GUI's reply as HTTP: 200 with the reply's lines (or `posted #N`), or
/// 400 with its error. A timeout inside the GUI is a 503.
fn reply_response(reply: &OpenReply) -> Vec<u8> {
    if !reply.ok {
        let e = reply.error.as_deref().unwrap_or("refused");
        let status = if e == "foreman did not respond" {
            503
        } else {
            400
        };
        return response(status, &format!("{e}\n"));
    }
    let mut body = String::new();
    if let Some(seq) = reply.seq {
        body.push_str(&format!("posted #{seq}\n"));
    }
    for line in reply.history.iter().flatten() {
        body.push_str(line);
        body.push('\n');
    }
    response(200, &body)
}

fn response(status: u16, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        417 => "Expectation Failed",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        _ => "Service Unavailable",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// What a Remote Session's agent is told, once per connection, with its first
/// chat delivery: the exact command to post back. URL and token are written
/// out rather than named as `$FOREMAN_URL`, because a Windows agent's tool
/// shell may be PowerShell or Git Bash and each spells variables differently.
/// Windows says `curl.exe`: in Windows PowerShell 5.1 `curl` is an alias for
/// `Invoke-WebRequest`.
pub fn reply_note(shell: RemoteShell, remote_port: u16, token: &str) -> String {
    let curl = match shell {
        RemoteShell::Posix => "curl",
        RemoteShell::Pwsh | RemoteShell::WindowsPowerShell => "curl.exe",
    };
    let url = format!("http://127.0.0.1:{remote_port}");
    format!(
        "[foreman] You run on another PC, so the foreman CLI is not here. Post to this chat with:\n\
         {curl} -sS -H \"Authorization: Bearer {token}\" --data-binary 'your message' \"{url}/chat?to=tN\"\n\
         Query: to=tN names the member (repeat or comma-separate; leave it out to post to everyone), re=N answers post #N, now=1 is urgent. \
         Same header, GET {url}/chat?history=20 reads the room and GET {url}/status lists its Sessions."
    )
}

/// `line` with the one-time note appended, taking it so the next delivery on
/// this connection goes out bare.
pub fn with_reply_note<'a>(line: &'a str, note: &mut Option<String>) -> std::borrow::Cow<'a, str> {
    match note.take() {
        Some(n) => format!("{line}\n\n{n}").into(),
        None => line.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn owner() -> Owner {
        Owner {
            project: "p2".into(),
            terminal: "t7".into(),
        }
    }

    fn q(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_head_reads_route_auth_length_and_expect() {
        let raw = b"POST /chat?to=t3&to=t4%2Ct5&re=9 HTTP/1.1\r\nHost: x\r\nauthorization: bearer abc\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\nhello";
        let (head, at) = parse_head(raw).unwrap().unwrap();
        assert_eq!(&raw[at..], b"hello");
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/chat");
        assert_eq!(head.query, q(&[("to", "t3"), ("to", "t4,t5"), ("re", "9")]));
        assert_eq!(head.bearer.as_deref(), Some("abc"));
        assert_eq!(head.content_length, Some(5));
        assert!(head.expect_continue);
    }

    #[test]
    fn parse_head_waits_for_the_blank_line_and_caps_it() {
        assert_eq!(parse_head(b"GET /status HTTP/1.1\r\nHost: x\r\n"), Ok(None));
        let huge = vec![b'a'; MAX_HEAD];
        assert_eq!(parse_head(&huge).unwrap_err().status, 431);
    }

    #[test]
    fn parse_head_refuses_malformed_and_unsupported_framing() {
        let status = |raw: &[u8]| parse_head(raw).unwrap_err().status;
        assert_eq!(status(b"GET\r\n\r\n"), 400);
        assert_eq!(status(b"GET status HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status(b"GET /status SPDY/3\r\n\r\n"), 400);
        assert_eq!(status(b"GET /status HTTP/1.1\r\nno colon\r\n\r\n"), 400);
        assert_eq!(
            status(b"POST /chat HTTP/1.1\r\nContent-Length: x\r\n\r\n"),
            400
        );
        assert_eq!(
            status(b"POST /chat HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n"),
            400
        );
        assert_eq!(
            status(b"POST /chat HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            501
        );
        assert_eq!(status(b"POST /chat HTTP/1.1\r\nExpect: tea\r\n\r\n"), 417);
        // A Basic credential is not a bearer token.
        let (h, _) = parse_head(b"GET /status HTTP/1.1\r\nAuthorization: Basic abc\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(h.bearer, None);
    }

    #[test]
    fn only_chat_and_status_are_routes() {
        assert_eq!(route("POST", "/chat"), Ok(Route::Post));
        assert_eq!(route("GET", "/chat"), Ok(Route::History));
        assert_eq!(route("GET", "/status"), Ok(Route::Status));
        assert_eq!(route("DELETE", "/chat").unwrap_err().status, 405);
        for path in [
            "/open",
            "/send",
            "/close",
            "/snapshot",
            "/kanban",
            "/hook",
            "/",
        ] {
            assert_eq!(route("POST", path).unwrap_err().status, 404, "{path}");
            assert_eq!(route("GET", path).unwrap_err().status, 404, "{path}");
        }
    }

    #[test]
    fn a_post_is_stamped_with_the_token_s_session_and_ignores_a_claimed_sender() {
        let query = q(&[
            ("from", "t1"),
            ("project", "p9"),
            ("peer", "evil"),
            ("to", "t3,t4"),
            ("to", "t5"),
            ("re", "12"),
            ("now", "1"),
        ]);
        let Call::Chat(req) =
            build_call(Route::Post, &query, b"done: tests green\n", &owner()).unwrap()
        else {
            panic!("a post is a chat request");
        };
        assert_eq!(req.from.as_deref(), Some("t7"));
        assert_eq!(req.project.as_deref(), Some("p2"));
        assert_eq!(req.peer, None, "the normal Member path, not post_remote");
        assert_eq!(req.text.as_deref(), Some("done: tests green"));
        assert_eq!(req.history, None);
        assert_eq!(req.to, ["t3", "t4", "t5"]);
        assert_eq!(req.re, Some(12));
        assert_eq!(req.now, Some(true));
    }

    #[test]
    fn a_post_needs_utf8_text_and_a_numeric_re() {
        let status = |query: &[(&str, &str)], body: &[u8]| {
            build_call(Route::Post, &q(query), body, &owner())
                .unwrap_err()
                .status
        };
        assert_eq!(status(&[], b"  \n"), 400);
        assert_eq!(status(&[], &[0xff, 0xfe]), 400);
        assert_eq!(status(&[("re", "x")], b"hi"), 400);
    }

    #[test]
    fn history_and_status_read_the_token_s_project_only() {
        let Call::Chat(h) = build_call(
            Route::History,
            &q(&[("history", "5"), ("project", "p9")]),
            b"",
            &owner(),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            (h.project.as_deref(), h.history, h.text),
            (Some("p2"), Some(5), None)
        );
        let Call::Chat(h) =
            build_call(Route::History, &q(&[("history", "999999")]), b"", &owner()).unwrap()
        else {
            panic!()
        };
        assert_eq!(h.history, Some(MAX_HISTORY));
        assert_eq!(
            build_call(Route::History, &q(&[("history", "-1")]), b"", &owner())
                .unwrap_err()
                .status,
            400
        );
        let Call::Status(s) =
            build_call(Route::Status, &q(&[("project", "p9")]), b"", &owner()).unwrap()
        else {
            panic!()
        };
        assert_eq!((s.project.as_deref(), s.json), (Some("p2"), false));
    }

    #[test]
    fn reply_note_spells_curl_for_the_remote_s_shell_and_goes_once() {
        let posix = reply_note(RemoteShell::Posix, 21000, "tok");
        assert!(
            posix.contains("\ncurl -sS -H \"Authorization: Bearer tok\""),
            "{posix}"
        );
        assert!(
            posix.contains("http://127.0.0.1:21000/chat?to=tN"),
            "{posix}"
        );
        for shell in [RemoteShell::Pwsh, RemoteShell::WindowsPowerShell] {
            assert!(reply_note(shell, 21000, "tok").contains("\ncurl.exe -sS"));
        }
        let mut note = Some("NOTE".to_string());
        assert_eq!(
            with_reply_note("[chat p1 #1] t1→t2: hi", &mut note),
            "[chat p1 #1] t1→t2: hi\n\nNOTE"
        );
        assert_eq!(
            with_reply_note("[chat p1 #2] t1→t2: again", &mut note),
            "[chat p1 #2] t1→t2: again"
        );
    }

    #[test]
    fn os_random_tokens_differ_and_ports_stay_in_range() {
        let (tx, _rx) = mpsc::channel();
        let hub = Hub::new(
            tx,
            eframe::egui::Context::default(),
            Duration::from_millis(300),
        );
        let a = hub.lease("p1", "t1").unwrap();
        let b = hub.lease("p1", "t2").unwrap();
        assert_eq!(a.token.value().len(), 64);
        assert_ne!(a.token.value(), b.token.value());
        assert_eq!(a.local_port, b.local_port, "one listener per instance");
        assert!(REMOTE_PORTS.contains(&a.remote_port));
        assert_eq!(
            hub.owner(a.token.value()),
            Some(Owner {
                project: "p1".into(),
                terminal: "t1".into()
            })
        );
        let value = a.token.value().to_string();
        drop(a);
        assert_eq!(hub.owner(&value), None, "a dropped token is revoked");
    }

    // ---- over a real loopback socket, with a stand-in GUI ----

    /// A Hub whose GUI side answers every request and reports what it got.
    fn live_hub() -> (Arc<Hub>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<CtrlMsg>();
        let (seen_tx, seen_rx) = mpsc::channel();
        std::thread::spawn(move || {
            for msg in rx {
                match msg {
                    CtrlMsg::Chat(r, reply, _) => {
                        let _ = seen_tx.send(format!(
                            "chat {:?} {:?} {:?} {:?}",
                            r.from, r.project, r.text, r.history
                        ));
                        let _ = reply.send(match r.history {
                            Some(_) => OpenReply {
                                ok: true,
                                history: Some(vec!["a".into(), "b".into()]),
                                ..Default::default()
                            },
                            None => OpenReply {
                                ok: true,
                                seq: Some(4),
                                ..Default::default()
                            },
                        });
                    }
                    CtrlMsg::Status(r, reply, _) => {
                        let _ = seen_tx.send(format!("status {:?}", r.project));
                        let _ = reply.send(OpenReply::err("no such project: p2"));
                    }
                    _ => {
                        let _ = seen_tx.send("other".into());
                    }
                }
            }
        });
        (
            Hub::new(
                tx,
                eframe::egui::Context::default(),
                Duration::from_millis(400),
            ),
            seen_rx,
        )
    }

    fn connect(port: u16) -> TcpStream {
        let c = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c
    }

    fn roundtrip(port: u16, raw: &[u8]) -> String {
        let mut c = connect(port);
        c.write_all(raw).unwrap();
        let mut out = String::new();
        let _ = c.read_to_string(&mut out);
        out
    }

    #[test]
    fn listener_binds_loopback_on_an_os_assigned_port() {
        let (hub, _seen) = live_hub();
        assert_eq!(
            *hub.port.lock().unwrap(),
            None,
            "nothing binds before the first lease"
        );
        let lease = hub.lease("p2", "t7").unwrap();
        assert_ne!(lease.local_port, 0);
        // A loopback-only bind refuses the same port on this PC's other addresses.
        if let Some(ip) = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .and_then(|s| s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).map(|_| s))
            .and_then(|s| s.local_addr())
            .ok()
            .map(|a| a.ip())
            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        {
            assert!(
                TcpStream::connect_timeout(
                    &(ip, lease.local_port).into(),
                    Duration::from_millis(500)
                )
                .is_err()
            );
        }
    }

    #[test]
    fn listener_posts_reads_and_refuses_over_http() {
        let (hub, seen) = live_hub();
        let lease = hub.lease("p2", "t7").unwrap();
        let (port, tok) = (lease.local_port, lease.token.value().to_string());

        let out = roundtrip(
            port,
            format!("POST /chat?from=t1 HTTP/1.1\r\nAuthorization: Bearer {tok}\r\nContent-Length: 2\r\n\r\nhi").as_bytes(),
        );
        assert!(
            out.starts_with("HTTP/1.1 200 OK\r\n") && out.ends_with("posted #4\n"),
            "{out}"
        );
        assert_eq!(
            seen.recv().unwrap(),
            r#"chat Some("t7") Some("p2") Some("hi") None"#
        );

        let out = roundtrip(
            port,
            format!("GET /chat?history=2 HTTP/1.1\r\nAuthorization: Bearer {tok}\r\n\r\n")
                .as_bytes(),
        );
        assert!(out.ends_with("\r\n\r\na\nb\n"), "{out}");
        assert_eq!(
            seen.recv().unwrap(),
            r#"chat Some("t7") Some("p2") None Some(2)"#
        );

        let out = roundtrip(
            port,
            format!("GET /status HTTP/1.1\r\nAuthorization: Bearer {tok}\r\n\r\n").as_bytes(),
        );
        assert!(
            out.starts_with("HTTP/1.1 400 ") && out.ends_with("no such project: p2\n"),
            "{out}"
        );
        assert_eq!(seen.recv().unwrap(), r#"status Some("p2")"#);

        let refused = |raw: String, status: &str| {
            let out = roundtrip(port, raw.as_bytes());
            assert!(
                out.starts_with(&format!("HTTP/1.1 {status} ")),
                "{raw:?} -> {out}"
            );
        };
        refused("GET /status HTTP/1.1\r\n\r\n".into(), "401");
        refused(
            "GET /status HTTP/1.1\r\nAuthorization: Bearer nope\r\n\r\n".into(),
            "401",
        );
        refused(
            format!(
                "POST /send HTTP/1.1\r\nAuthorization: Bearer {tok}\r\nContent-Length: 0\r\n\r\n"
            ),
            "404",
        );
        refused(
            format!(
                "POST /chat HTTP/1.1\r\nAuthorization: Bearer {tok}\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY + 1
            ),
            "413",
        );
        refused(
            format!("POST /chat HTTP/1.1\r\nAuthorization: Bearer {tok}\r\n\r\n"),
            "411",
        );
        // None of the refusals reached the GUI.
        assert!(seen.try_recv().is_err());

        drop(lease);
        refused(
            format!("GET /status HTTP/1.1\r\nAuthorization: Bearer {tok}\r\n\r\n"),
            "401",
        );
    }

    #[test]
    fn listener_answers_expect_100_continue_before_the_body() {
        let (hub, seen) = live_hub();
        let lease = hub.lease("p2", "t7").unwrap();
        let mut c = connect(lease.local_port);
        c.write_all(
            format!(
                "POST /chat HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\n",
                lease.token.value()
            )
            .as_bytes(),
        )
        .unwrap();
        let mut interim = [0u8; 25];
        c.read_exact(&mut interim).unwrap();
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        c.write_all(b"hello").unwrap();
        let mut out = String::new();
        let _ = c.read_to_string(&mut out);
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{out}");
        assert_eq!(
            seen.recv().unwrap(),
            r#"chat Some("t7") Some("p2") Some("hello") None"#
        );
    }

    #[test]
    fn listener_refuses_a_stalled_client_at_the_deadline() {
        let (hub, seen) = live_hub();
        let lease = hub.lease("p2", "t7").unwrap();
        let tok = lease.token.value().to_string();
        let t0 = Instant::now();
        // Stalls in the head…
        let out = roundtrip(lease.local_port, b"GET /status HTTP/1.1\r\nHost: x\r\n");
        assert!(out.starts_with("HTTP/1.1 408 "), "{out}");
        // …and in the body.
        let out = roundtrip(
            lease.local_port,
            format!("POST /chat HTTP/1.1\r\nAuthorization: Bearer {tok}\r\nContent-Length: 10\r\n\r\nabc").as_bytes(),
        );
        assert!(out.starts_with("HTTP/1.1 408 "), "{out}");
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        assert!(seen.try_recv().is_err());
    }

    #[test]
    fn a_session_s_token_dies_when_its_process_exits() {
        let (tx, _rx) = mpsc::channel();
        let ctx = eframe::egui::Context::default();
        let hub = Hub::new(tx, ctx.clone(), IO_TIMEOUT);
        let lease = hub.lease("p1", "t1").unwrap();
        let value = lease.token.value().to_string();
        let argv: Vec<String> = ["cmd.exe", "/c", "exit 0"].map(String::from).to_vec();
        let mut s = crate::terminal::Session::spawn_argv(&argv, None, &[], ctx).unwrap();
        s.attach_tunnel(lease.token, "note".into());
        assert!(hub.owner(&value).is_some());
        let t0 = Instant::now();
        while s.exited().is_none() {
            assert!(t0.elapsed() < Duration::from_secs(10), "cmd never exited");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            hub.owner(&value),
            None,
            "revoked the frame the exit is seen"
        );
    }
}
