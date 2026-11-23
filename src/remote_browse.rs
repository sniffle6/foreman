//! Listing a Remote's folders for the directory picker under a Directory
//! field (the directory prompt and the Remotes pane). The field text is the
//! source of truth: [`split`] reads which directory it names and the prefix
//! that filters it, and [`descend`] / [`up`] write a choice back. Listings
//! run over `ssh` with `BatchMode` on a worker thread, cached per (target,
//! shell, directory) for the life of the widget that owns the cache; an
//! answer that never came back ([`State::Unreachable`]) is shown but not
//! kept. Everything above the probe section is pure and table-tested; the
//! widget is [`crate::remote_browse_ui`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::remote::RemoteShell;

/// Most folders one listing keeps. Past it the breadcrumb says it was cut.
pub const CAP: usize = 500;
/// Listings one widget runs at once; more wait, `Loading`, for a free slot.
pub const MAX_RUNNING: usize = 2;
/// The whole listing: a login plus one directory read.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);
/// Prefix on the scripts' marker lines, so login noise is never a folder.
const MARK: &str = "foreman-browse";

// ---------------------------------------------------------------------------
// Path model
// ---------------------------------------------------------------------------

/// How the Remote spells paths: its shell decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Posix,
    /// `\` and `/` both separate; written with `\`. Drives are roots.
    Windows,
}

impl Style {
    pub fn of(shell: RemoteShell) -> Style {
        match shell {
            RemoteShell::Posix => Style::Posix,
            RemoteShell::Pwsh | RemoteShell::WindowsPowerShell => Style::Windows,
        }
    }

    fn is_sep(self, c: char) -> bool {
        c == '/' || (self == Style::Windows && c == '\\')
    }

    fn sep(self) -> char {
        match self {
            Style::Posix => '/',
            Style::Windows => '\\',
        }
    }
}

/// The field text read as a directory to list and a prefix to filter it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split<'a> {
    /// Up to and including the last separator, as written. Empty (or `~`)
    /// is the Remote's home.
    pub dir: &'a str,
    /// What follows the last separator.
    pub filter: &'a str,
}

pub fn split(text: &str, style: Style) -> Split<'_> {
    if text.trim() == "~" {
        return Split {
            dir: text,
            filter: "",
        };
    }
    match text.rfind(|c| style.is_sep(c)) {
        Some(i) => Split {
            dir: &text[..=i],
            filter: &text[i + 1..],
        },
        None => Split {
            dir: "",
            filter: text,
        },
    }
}

/// Whether a written directory is the Remote's home: blank, `~`, `~/`.
pub fn is_home(dir: &str) -> bool {
    crate::remote::home_relative(dir.trim()).is_some_and(str::is_empty) || dir.trim().is_empty()
}

/// The directory a listing is asked for: home is always `""`, so `~/`, `~`
/// and a blank field share one cache entry.
pub fn listed_dir(dir: &str) -> String {
    if is_home(dir) {
        String::new()
    } else {
        dir.trim_start().to_string()
    }
}

/// The one-line name of the directory being listed.
pub fn breadcrumb(dir: &str) -> String {
    if is_home(dir) {
        "~".into()
    } else {
        dir.trim_start().to_string()
    }
}

/// Hover notes for a name that cannot be chosen.
pub const NOT_UTF8: &str =
    "This name is not valid UTF-8, so it can't be written into the Directory field";
pub const HAS_CONTROL: &str = "This name holds a control character, which a Directory can't hold";

/// What stands in for a character [`Entry::shown`] will not paint.
pub const PLACEHOLDER: char = '\u{FFFD}';

/// One folder of a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The name as the Remote spells it (lossy when it is not UTF-8).
    pub name: String,
    /// A Windows drive root (`C:\`), listed beside home's folders.
    pub drive: bool,
    /// Why this name cannot be chosen, for its hover note; `None` when it
    /// can. A name that is not UTF-8 cannot be typed back, and a control
    /// character is refused by the field's own check.
    pub blocked: Option<&'static str>,
    /// `name` lowercased once, for the prefix filter.
    lower: String,
}

impl Entry {
    pub fn dir(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            drive: false,
            blocked: name.chars().any(char::is_control).then_some(HAS_CONTROL),
            lower: name.to_lowercase(),
        }
    }

    /// A name read from the Remote as bytes.
    pub fn from_bytes(name: &[u8]) -> Entry {
        match std::str::from_utf8(name) {
            Ok(n) => Entry::dir(n),
            Err(_) => Entry {
                blocked: Some(NOT_UTF8),
                ..Entry::dir(&String::from_utf8_lossy(name))
            },
        }
    }

    pub fn drive(root: &str) -> Entry {
        Entry {
            drive: true,
            ..Entry::dir(root)
        }
    }

    /// A case-insensitive prefix match; `filter` already lowercased.
    pub fn matches(&self, lower_filter: &str) -> bool {
        self.lower.starts_with(lower_filter)
    }

    /// The name as it is safe to paint: control and bidi characters become
    /// a visible [`PLACEHOLDER`].
    pub fn shown(&self) -> std::borrow::Cow<'_, str> {
        if self.name.chars().any(unpaintable) {
            self.name
                .chars()
                .map(|c| if unpaintable(c) { PLACEHOLDER } else { c })
                .collect::<String>()
                .into()
        } else {
            self.name.as_str().into()
        }
    }
}

/// Control characters, and the formatting characters that reorder text
/// (bidi marks, embeddings, overrides and isolates).
fn unpaintable(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// The field text after choosing `entry` in the directory `text` names: one
/// level deeper, ending in a separator so the next level lists. Under home
/// it is written `~/name/`.
pub fn descend(text: &str, style: Style, entry: &Entry) -> String {
    if entry.drive {
        return entry.name.clone();
    }
    let dir = split(text, style).dir;
    let sep = style.sep();
    let base = if is_home(dir) {
        format!("~{sep}")
    } else {
        dir.trim_start().to_string()
    };
    format!("{base}{}{sep}", entry.name)
}

/// The field text one level above the directory `text` names, and the name
/// of the folder left (to select it again). `resolved` is where the last
/// listing of that directory found itself; only home and `.`/`..` need it.
/// `None` at a root.
pub fn up(text: &str, style: Style, resolved: Option<&str>) -> Option<(String, String)> {
    let dir = split(text, style).dir.trim_start();
    if !is_home(dir) {
        match parent_of(dir, style) {
            Some((parent, child)) if child != "." && child != ".." => {
                return Some((parent, child));
            }
            Some(_) => {} // "./" or "../": only the listing knows where it is
            None => return None,
        }
    }
    parent_of(resolved?, style)
}

/// `dir` minus its last component, written with its separator kept, and that
/// component. A Windows drive root's parent is `""`, where drives are listed.
fn parent_of(dir: &str, style: Style) -> Option<(String, String)> {
    let body = dir.trim_end_matches(|c| style.is_sep(c));
    if body.is_empty() {
        return None; // "/" or "\"
    }
    if style == Style::Windows && body.len() == 2 && body.ends_with(':') {
        return Some((String::new(), format!("{body}\\")));
    }
    match body.rfind(|c| style.is_sep(c)) {
        Some(i) => {
            let unc_root = style == Style::Windows
                && body.starts_with(r"\\")
                && body[2..].matches(|c| style.is_sep(c)).count() < 2;
            if unc_root {
                return None; // \\server\share has no listable parent
            }
            Some((body[..=i].to_string(), body[i + 1..].to_string()))
        }
        // A relative "src/": its parent is where ssh starts, home.
        None => Some((String::new(), body.to_string())),
    }
}

// ---------------------------------------------------------------------------
// The listing scripts and their output
// ---------------------------------------------------------------------------

/// A finished listing of one directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Where the Remote says the directory is (`pwd`), for going up past home.
    pub resolved: String,
    /// Folders, then dot-folders, then drives; each case-insensitively sorted.
    pub entries: Vec<Entry>,
    /// More than [`CAP`] were there, or the output was cut.
    pub truncated: bool,
}

/// Why a listing has no folders to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// One plain sentence that ends by saying what to do.
    pub why: String,
    /// What ssh or the Remote said, for a hover; never the whole message.
    pub detail: Option<String>,
}

impl Failure {
    pub fn new(why: &str) -> Failure {
        Failure {
            why: why.into(),
            detail: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Asked, or about to be: nothing to show yet.
    Loading,
    Listed(Listed),
    /// The Remote answered that this directory cannot be listed (missing, a
    /// file, denied). Kept: asking again says the same.
    Failed(Failure),
    /// The answer never came back: no route, refused, timed out, or a login
    /// that needs a password. Shown with a retry row, never kept: the next
    /// look asks again ([`Cache::forget`]).
    Unreachable(Failure),
}

/// What the list area says when `BatchMode` was refused.
pub const NEEDS_LOGIN: &str = "Can't list folders: logging in to this Remote needs a password or passphrase. Type the path instead, or retry once a key is loaded.";
/// What it says when a Windows Remote's command line would be too long.
pub const TOO_LONG: &str =
    "This path is too long to list on a Windows Remote. Type the path instead.";

/// The script for a Posix Remote, run by plain `sh`. Prints the marker
/// lines [`parse`] reads: the resolved directory, `d <name>` per folder (at
/// most `CAP + 1`, so a cut shows), then an end line. A name holding a line
/// break is skipped and a resolved path holding one is refused: either
/// could forge a line.
pub fn posix_script(dir: &str) -> String {
    let target = match crate::remote::home_relative(dir) {
        Some("") => "\"$HOME\"".to_string(),
        Some(rest) => format!("\"$HOME\"/{}", crate::remote::sh_quote(rest)),
        None if dir.is_empty() => "\"$HOME\"".to_string(),
        None => crate::remote::sh_quote(dir),
    };
    format!(
        "unset CDPATH
nl='
'
t={target}
if ! cd -- \"$t\" 2>/dev/null; then
  u=${{t%/}}; [ -n \"$u\" ] || u=/
  if [ ! -e \"$u\" ]; then echo '{MARK} fail missing'
  elif [ ! -d \"$u\" ]; then echo '{MARK} fail notdir'
  else echo '{MARK} fail denied'; fi
  exit 0
fi
[ -r . ] || {{ echo '{MARK} fail denied'; exit 0; }}
p=$(pwd)
case $p in *\"$nl\"*) echo '{MARK} fail badpath'; exit 0;; esac
printf '{MARK} dir %s\\n' \"$p\"
n=0
for f in * .[!.]* ..?*; do
  [ -d \"$f\" ] || continue
  case $f in *\"$nl\"*) continue;; esac
  printf 'd %s\\n' \"$f\"
  n=$((n+1))
  [ \"$n\" -gt {CAP} ] && break
done
echo '{MARK} end'
"
    )
}

/// The PowerShell script (pwsh and 5.1): the same marker lines, folders by
/// `Get-ChildItem -Directory -Force`, and the drive roots (`v C:\`) when
/// listing home, where a bare drive is typed. Everything that can throw is
/// inside the `try`, so the script always ends in a marker. Untested: no
/// Windows Remote has existed.
pub fn ps_script(dir: &str) -> String {
    let target = match crate::remote::home_relative(dir) {
        Some("") => "$HOME".to_string(),
        Some(rest) => format!("(Join-Path $HOME {})", crate::remote::ps_quote(rest)),
        None if dir.is_empty() => "$HOME".to_string(),
        None => crate::remote::ps_quote(dir),
    };
    let drives = if dir.is_empty() || is_home(dir) {
        "@(Get-PSDrive -PSProvider FileSystem | ForEach-Object { $_.Root })"
    } else {
        "@()"
    };
    format!(
        "$ErrorActionPreference = 'Stop'
try {{ [Console]::OutputEncoding = [Text.Encoding]::UTF8 }} catch {{}}
try {{
  $t = {target}
  if (-not (Test-Path -LiteralPath $t)) {{ '{MARK} fail missing'; exit 0 }}
  if (-not (Test-Path -LiteralPath $t -PathType Container)) {{ '{MARK} fail notdir'; exit 0 }}
  $p = (Get-Item -LiteralPath $t -Force).FullName
  $d = @(Get-ChildItem -LiteralPath $p -Directory -Force | Select-Object -First {more})
  $v = {drives}
}} catch [System.UnauthorizedAccessException] {{ '{MARK} fail denied'; exit 0 }}
catch {{ '{MARK} fail other ' + $_.Exception.Message; exit 0 }}
'{MARK} dir ' + $p
foreach ($e in $d) {{ 'd ' + $e.Name }}
foreach ($r in $v) {{ 'v ' + $r }}
'{MARK} end'
",
        more = CAP + 1
    )
}

/// The remote line for listing `dir`: the script as base64, so the login
/// shell never sees a quote or a space from the typed path. A Windows line
/// past [`crate::remote::WINDOWS_LINE_CAP`] is refused with [`TOO_LONG`].
pub fn list_line(shell: RemoteShell, dir: &str) -> Result<String, Failure> {
    match shell {
        RemoteShell::Posix => Ok(crate::remote::posix_line(&posix_script(dir))),
        RemoteShell::Pwsh | RemoteShell::WindowsPowerShell => {
            let exe = if shell == RemoteShell::Pwsh {
                "pwsh"
            } else {
                "powershell"
            };
            let line = format!(
                "{exe} -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
                crate::remote::utf16le_b64(&ps_script(dir))
            );
            if line.len() > crate::remote::WINDOWS_LINE_CAP {
                return Err(Failure::new(TOO_LONG));
            }
            Ok(line)
        }
    }
}

/// The `ssh` arguments (after `ssh`) one listing runs: never a prompt, a
/// connect deadline, no PTY.
pub fn list_args(target: &str, shell: RemoteShell, dir: &str) -> Result<Vec<String>, Failure> {
    let line = list_line(shell, dir)?;
    Ok([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=8",
        "--",
        target,
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([line])
    .collect())
}

/// Read a listing's stdout. `None` when no marker came back: the login
/// itself failed, and stderr says why. Only PowerShell ends its lines with
/// CR; on Posix a CR is part of a name.
pub fn parse(stdout: &[u8], style: Style) -> Option<State> {
    let crlf = style == Style::Windows;
    let mut lines = stdout.split(|&b| b == b'\n').map(|l| match l {
        [rest @ .., b'\r'] if crlf => rest,
        l => l,
    });
    let mark = MARK.as_bytes();
    let head = lines.find_map(|l| l.strip_prefix(mark)?.strip_prefix(b" "))?;
    if let Some(why) = head.strip_prefix(b"fail") {
        let why = String::from_utf8_lossy(why);
        return Some(State::Failed(match why.trim() {
            "missing" => {
                Failure::new("No such directory on this Remote. Check the path, or type another.")
            }
            "notdir" => {
                Failure::new("That is a file, not a directory. Type a directory path instead.")
            }
            "denied" => Failure::new(
                "Permission denied: this directory can't be listed. Type the path instead.",
            ),
            "badpath" => Failure::new(
                "This directory's path holds a line break, so it can't be listed. Type the path instead.",
            ),
            other => {
                let msg = other.strip_prefix("other").unwrap_or(other).trim();
                Failure {
                    why: "The Remote could not list this directory. Type the path instead.".into(),
                    detail: (!msg.is_empty()).then(|| msg.to_string()),
                }
            }
        }));
    }
    let resolved = String::from_utf8_lossy(head.strip_prefix(b"dir ")?).into_owned();
    let mut entries = Vec::new();
    let mut ended = false;
    let mut last_cut = false;
    for line in lines {
        if line
            .strip_prefix(mark)
            .is_some_and(|r| r.trim_ascii() == b"end")
        {
            ended = true;
            break;
        }
        last_cut = false;
        if let Some(name) = line.strip_prefix(b"d ") {
            if !name.is_empty() {
                entries.push(Entry::from_bytes(name));
                last_cut = true;
            }
        } else if let Some(root) = line.strip_prefix(b"v ")
            && !root.is_empty()
        {
            entries.push(Entry::drive(&String::from_utf8_lossy(root)));
        }
    }
    // Output cut mid-stream: the last name may be half a name.
    if !ended && last_cut {
        entries.pop();
    }
    let (mut folders, drives): (Vec<Entry>, Vec<Entry>) =
        entries.into_iter().partition(|e| !e.drive);
    folders.sort_by(|a, b| {
        (a.name.starts_with('.'), &a.lower, &a.name).cmp(&(
            b.name.starts_with('.'),
            &b.lower,
            &b.name,
        ))
    });
    let truncated = !ended || folders.len() > CAP;
    folders.truncate(CAP);
    folders.extend(drives);
    Some(State::Listed(Listed {
        resolved,
        entries: folders,
        truncated,
    }))
}

/// A finished `ssh` turned into a state: the script's answer, or why the
/// login failed in plain words.
pub fn outcome(stdout: &[u8], stderr: &str, style: Style) -> State {
    if let Some(s) = parse(stdout, style) {
        return s;
    }
    match crate::remote_probe::classify_failure(stderr) {
        crate::remote_probe::TestOutcome::NeedsLogin => State::Unreachable(Failure {
            why: NEEDS_LOGIN.into(),
            detail: Some(crate::remote_probe::last_line(stderr, "")).filter(|d| !d.is_empty()),
        }),
        crate::remote_probe::TestOutcome::Failed(e) => unreachable(&e),
        crate::remote_probe::TestOutcome::Ok(_) => unreachable(""),
    }
}

/// ssh's own words (`raw`) as a plain sentence that says what to do; the
/// words themselves go to the hover.
pub fn unreachable(raw: &str) -> State {
    let l = raw.to_lowercase();
    let what = if l.contains("could not run ssh") {
        "ssh could not be started on this PC. Type the path instead."
    } else if l.contains("did not finish") || l.contains("timed out") {
        "the Remote did not answer in time. Retry, or type the path instead."
    } else if l.contains("connection refused") {
        "the Remote refused the connection. Retry, or type the path instead."
    } else if l.contains("could not resolve") || l.contains("name or service not known") {
        "this Target's host name was not found. Check the Target, or type the path instead."
    } else if l.contains("no route") || l.contains("unreachable") {
        "there is no network route to the Remote. Retry, or type the path instead."
    } else {
        "ssh could not reach the Remote. Retry, or type the path instead."
    };
    State::Unreachable(Failure {
        why: format!("Can't list folders: {what}"),
        detail: Some(raw.trim().to_string()).filter(|d| !d.is_empty()),
    })
}

// ---------------------------------------------------------------------------
// The cache and the probe
// ---------------------------------------------------------------------------

/// What one listing is for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub target: String,
    pub shell: RemoteShell,
    /// [`listed_dir`]: home is `""`.
    pub dir: String,
}

impl Key {
    pub fn new(target: &str, shell: RemoteShell, dir: &str) -> Key {
        Key {
            target: target.trim().to_string(),
            shell,
            dir: listed_dir(dir),
        }
    }
}

#[derive(Debug)]
struct Slot {
    state: Arc<State>,
    /// When its listing started; read while `Loading`.
    since: Instant,
}

/// Listings by key. A key is listed once; its first ask stores `Loading`,
/// which is also what says a listing is running. At most [`MAX_RUNNING`]
/// run at once. An `Unreachable` answer lasts until [`Cache::forget`].
#[derive(Debug, Default)]
pub struct Cache {
    entries: HashMap<Key, Slot>,
    running: usize,
}

impl Cache {
    /// The state for `key`, and whether the caller must start its listing:
    /// true only on the first ask that `may_start` while a slot is free.
    pub fn get(&mut self, key: &Key, may_start: bool) -> (Arc<State>, bool) {
        if let Some(s) = self.entries.get(key) {
            return (s.state.clone(), false);
        }
        let loading = Arc::new(State::Loading);
        let start = may_start && self.running < MAX_RUNNING;
        if start {
            self.running += 1;
            self.entries.insert(
                key.clone(),
                Slot {
                    state: loading.clone(),
                    since: Instant::now(),
                },
            );
        }
        (loading, start)
    }

    /// The state for `key` without asking for it.
    pub fn peek(&self, key: &Key) -> Option<Arc<State>> {
        self.entries.get(key).map(|s| s.state.clone())
    }

    /// How long `key`'s listing has been running, while it is.
    pub fn running_for(&self, key: &Key) -> Option<Duration> {
        self.entries
            .get(key)
            .filter(|s| matches!(*s.state, State::Loading))
            .map(|s| s.since.elapsed())
    }

    pub fn finish(&mut self, key: Key, state: State) {
        let was_running = self
            .entries
            .get(&key)
            .is_some_and(|s| matches!(*s.state, State::Loading));
        if was_running {
            self.running = self.running.saturating_sub(1);
        }
        self.entries.insert(
            key,
            Slot {
                state: Arc::new(state),
                since: Instant::now(),
            },
        );
    }

    /// Drop every `Unreachable` answer, so the next look asks again.
    pub fn forget(&mut self) {
        self.entries
            .retain(|_, s| !matches!(*s.state, State::Unreachable(_)));
    }

    /// Test hook: pretend `key`'s listing started `ago`.
    #[cfg(test)]
    pub fn age(&mut self, key: &Key, ago: Duration) {
        if let Some(s) = self.entries.get_mut(key) {
            s.since = Instant::now() - ago;
        }
    }
}

pub type Shared = Arc<Mutex<Cache>>;

/// The listing of `key`, starting it on a worker thread on the first ask
/// that `may_start`. The shape of [`crate::remote_probe::reach`]: a lock and
/// a map lookup on the frame path, `ssh` only off it. Setting `cancel`
/// kills every listing started with it.
pub fn list(
    ctx: &egui::Context,
    cache: &Shared,
    key: &Key,
    may_start: bool,
    cancel: &Arc<AtomicBool>,
) -> Arc<State> {
    let (state, start) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key, may_start);
    if start {
        spawn(ctx, cache, key, cancel);
    }
    state
}

/// Unit tests never reach a network: the entry stays `Loading`.
#[cfg(test)]
fn spawn(_ctx: &egui::Context, _cache: &Shared, _key: &Key, _cancel: &Arc<AtomicBool>) {}

#[cfg(not(test))]
fn spawn(ctx: &egui::Context, cache: &Shared, key: &Key, cancel: &Arc<AtomicBool>) {
    let (ctx, cache, key, cancel) = (ctx.clone(), cache.clone(), key.clone(), cancel.clone());
    std::thread::spawn(move || {
        let state = run_listing(&key, &cancel);
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finish(key, state);
        if !cancel.load(Ordering::Relaxed) {
            ctx.request_repaint();
        }
    });
}

/// Run one listing now. Blocks up to [`LIST_TIMEOUT`], or until `cancel` is
/// set: worker threads only.
pub fn run_listing(key: &Key, cancel: &AtomicBool) -> State {
    let args = match list_args(&key.target, key.shell, &key.dir) {
        Ok(a) => a,
        Err(f) => return State::Failed(f),
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match crate::remote_probe::run_raw(&args, LIST_TIMEOUT, Some(cancel)) {
        Ok(out) => outcome(
            &out.stdout,
            &String::from_utf8_lossy(&out.stderr),
            Style::of(key.shell),
        ),
        Err(e) => unreachable(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Style::{Posix, Windows};

    #[test]
    fn split_reads_the_directory_and_the_filter() {
        for (text, style, dir, filter) in [
            ("", Posix, "", ""),
            ("src", Posix, "", "src"),
            ("~", Posix, "~", ""),
            ("~/", Posix, "~/", ""),
            ("~/sr", Posix, "~/", "sr"),
            ("~/src/fo", Posix, "~/src/", "fo"),
            ("/", Posix, "/", ""),
            ("/ho", Posix, "/", "ho"),
            ("/home/andy/", Posix, "/home/andy/", ""),
            ("a b/c d", Posix, "a b/", "c d"),
            // A backslash is a name character on Posix.
            (r"C:\src", Posix, "", r"C:\src"),
            ("", Windows, "", ""),
            ("C:", Windows, "", "C:"),
            (r"C:\", Windows, r"C:\", ""),
            (r"C:\Us", Windows, r"C:\", "Us"),
            ("C:/src/fo", Windows, "C:/src/", "fo"),
            (r"~\src\", Windows, r"~\src\", ""),
            (r"\\srv\share\x", Windows, r"\\srv\share\", "x"),
            ("~/é/ü", Posix, "~/é/", "ü"),
        ] {
            assert_eq!(
                split(text, style),
                Split { dir, filter },
                "{text:?} {style:?}"
            );
        }
    }

    #[test]
    fn home_is_blank_tilde_or_tilde_slash() {
        for d in ["", " ", "~", "~/", r"~\", " ~/"] {
            assert!(is_home(d), "{d:?}");
            assert_eq!(listed_dir(d), "");
            assert_eq!(breadcrumb(d), "~");
        }
        for d in ["/", "~/src/", "src/", "~x/"] {
            assert!(!is_home(d), "{d:?}");
        }
        assert_eq!(listed_dir(" /srv/"), "/srv/");
    }

    #[test]
    fn descend_writes_the_choice_and_keeps_home_as_tilde() {
        let e = Entry::dir;
        for (text, style, entry, want) in [
            ("", Posix, e("src"), "~/src/"),
            ("s", Posix, e("src"), "~/src/"),
            ("~", Posix, e("src"), "~/src/"),
            ("~/", Posix, e("src"), "~/src/"),
            ("~/src/f", Posix, e("foreman"), "~/src/foreman/"),
            ("/", Posix, e("home"), "/home/"),
            ("/home/an", Posix, e("andy"), "/home/andy/"),
            ("rel/", Posix, e("x"), "rel/x/"),
            ("~/", Posix, e("it's \"q\" $x"), "~/it's \"q\" $x/"),
            ("", Windows, e("src"), r"~\src\"),
            (r"C:\", Windows, e("Users"), r"C:\Users\"),
            ("C:/a/", Windows, e("b"), r"C:/a/b\"),
            ("C", Windows, Entry::drive(r"C:\"), r"C:\"),
            (r"C:\x\", Windows, Entry::drive(r"D:\"), r"D:\"),
        ] {
            assert_eq!(descend(text, style, &entry), want, "{text:?} {entry:?}");
        }
    }

    #[test]
    fn up_goes_one_level_and_names_the_folder_left() {
        let s = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
        for (text, style, resolved, want) in [
            ("~/src/foreman/", Posix, None, s("~/src/", "foreman")),
            ("~/src/foreman/fil", Posix, None, s("~/src/", "foreman")),
            ("~/src/", Posix, None, s("~/", "src")),
            // Above home needs where home is.
            ("~/", Posix, Some("/home/andy"), s("/home/", "andy")),
            ("", Posix, Some("/home/andy"), s("/home/", "andy")),
            ("~", Posix, Some("/home/andy"), s("/home/", "andy")),
            ("~/", Posix, None, None),
            ("/home/", Posix, None, s("/", "home")),
            ("/", Posix, Some("/"), None),
            ("rel/", Posix, None, s("", "rel")),
            ("a/../", Posix, Some("/home/andy"), s("/home/", "andy")),
            (r"C:\Users\andy\", Windows, None, s(r"C:\Users\", "andy")),
            (r"C:\Users\", Windows, None, s(r"C:\", "Users")),
            // A drive root goes to home, where the drives are listed.
            (r"C:\", Windows, None, s("", r"C:\")),
            ("", Windows, Some(r"C:\Users\andy"), s(r"C:\Users\", "andy")),
            (r"\\srv\share\x\", Windows, None, s(r"\\srv\share\", "x")),
            (r"\\srv\share\", Windows, None, None),
        ] {
            assert_eq!(up(text, style, resolved), want, "{text:?} {resolved:?}");
        }
    }

    #[test]
    fn the_filter_is_a_case_insensitive_prefix() {
        let e = Entry::dir("Documents");
        assert!(e.matches("doc") && e.matches("") && !e.matches("ocu"));
        assert!(Entry::dir("Über").matches(&"üb".to_lowercase()));
    }

    fn listed(s: Option<State>) -> Listed {
        match s {
            Some(State::Listed(l)) => l,
            other => panic!("{other:?}"),
        }
    }

    fn names(l: &Listed) -> Vec<&str> {
        l.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn a_listing_sorts_folders_then_dot_folders_and_keeps_odd_names() {
        let out = "Welcome to Debian\nforeman-browse dir /home/andy\nd src\nd .cache\nd My Files\nd it's \"quoted\"\nd Ünïcode\nd apps\nd .config\nforeman-browse end\n";
        let l = listed(parse(out.as_bytes(), Posix));
        assert_eq!(l.resolved, "/home/andy");
        assert!(!l.truncated);
        assert_eq!(
            names(&l),
            [
                "apps",
                "it's \"quoted\"",
                "My Files",
                "src",
                "Ünïcode",
                ".cache",
                ".config"
            ]
        );
        assert!(l.entries.iter().all(|e| e.blocked.is_none()));
    }

    #[test]
    fn an_empty_directory_lists_nothing_and_powershell_crlf_is_read() {
        let l = listed(parse(
            b"foreman-browse dir C:\\empty\r\nforeman-browse end\r\n",
            Windows,
        ));
        assert!(l.entries.is_empty() && !l.truncated);
        assert_eq!(l.resolved, r"C:\empty");
        // No trailing newline at all.
        let l = listed(parse(
            b"foreman-browse dir /x\nd a\nforeman-browse end",
            Posix,
        ));
        assert_eq!(names(&l), ["a"]);
    }

    /// A CR is a name character on Posix: "x\r" stays "x\r" (and cannot be
    /// chosen, since a Directory holds no control character), never "x".
    #[test]
    fn a_posix_cr_is_kept_in_the_name() {
        let l = listed(parse(
            b"foreman-browse dir /srv\nd x\r\nd y\nforeman-browse end\n",
            Posix,
        ));
        assert_eq!(names(&l), ["x\r", "y"]);
        assert_eq!(l.entries[0].blocked, Some(HAS_CONTROL));
        assert_eq!(l.entries[0].shown(), format!("x{PLACEHOLDER}"));
        assert_eq!(l.entries[1].blocked, None);
        let l = listed(parse(
            b"foreman-browse dir C:\\a\r\nd x\r\nforeman-browse end\r\n",
            Windows,
        ));
        assert_eq!(names(&l), ["x"]);
    }

    /// A name that is not UTF-8 is listed, dimmed and not choosable, and a
    /// bidi override is never painted.
    #[test]
    fn odd_bytes_and_bidi_names_are_listed_but_never_painted_raw() {
        let l = listed(parse(
            b"foreman-browse dir /srv\nd bad\xff\xfename\nd evil\xe2\x80\xaetxt\nforeman-browse end\n",
            Posix,
        ));
        let bad = l
            .entries
            .iter()
            .find(|e| e.name.starts_with("bad"))
            .unwrap();
        assert_eq!(bad.blocked, Some(NOT_UTF8));
        let evil = l
            .entries
            .iter()
            .find(|e| e.name.starts_with("evil"))
            .unwrap();
        assert_eq!(evil.blocked, None, "a valid name stays choosable");
        assert_eq!(evil.shown(), format!("evil{PLACEHOLDER}txt"));
        assert!(!evil.shown().contains('\u{202E}'));
    }

    #[test]
    fn windows_drives_come_after_folders() {
        let out = "foreman-browse dir C:\\Users\\andy\r\nd Desktop\r\nd .vscode\r\nv C:\\\r\nv D:\\\r\nforeman-browse end\r\n";
        let l = listed(parse(out.as_bytes(), Windows));
        assert_eq!(names(&l), ["Desktop", ".vscode", r"C:\", r"D:\"]);
        assert!(l.entries[2].drive && !l.entries[0].drive);
    }

    #[test]
    fn past_the_cap_or_cut_short_says_so() {
        let mut out = String::from("foreman-browse dir /big\n");
        for i in 0..=CAP {
            out.push_str(&format!("d dir{i:04}\n"));
        }
        out.push_str("foreman-browse end\n");
        let l = listed(parse(out.as_bytes(), Posix));
        assert_eq!(l.entries.len(), CAP);
        assert!(l.truncated);
        // The pipe was cut: no end line, and the last name may be half one.
        let l = listed(parse(b"foreman-browse dir /x\nd alpha\nd bet", Posix));
        assert_eq!(names(&l), ["alpha"]);
        assert!(l.truncated);
    }

    #[test]
    fn script_failures_become_plain_messages_that_say_what_to_do() {
        for (out, want) in [
            ("foreman-browse fail missing\n", "No such directory"),
            ("foreman-browse fail notdir\n", "a file, not a directory"),
            ("foreman-browse fail denied\n", "Permission denied"),
            ("foreman-browse fail badpath\n", "line break"),
            (
                "foreman-browse fail other Access to the path is denied.\r\n",
                "could not list this directory",
            ),
        ] {
            match parse(out.as_bytes(), Windows) {
                Some(State::Failed(f)) => {
                    assert!(f.why.contains(want), "{}", f.why);
                    assert!(
                        f.why.contains("Type") || f.why.contains("type"),
                        "{}",
                        f.why
                    );
                }
                other => panic!("{out}: {other:?}"),
            }
        }
        // The Remote's own words go to the hover, not the message.
        let Some(State::Failed(f)) = parse(b"foreman-browse fail other Boom.\r\n", Windows) else {
            panic!()
        };
        assert_eq!(f.detail.as_deref(), Some("Boom."));
        assert!(!f.why.contains("Boom"));
    }

    /// Any answer that never came back is `Unreachable`, in plain words
    /// that say what to do; ssh's own line is only the detail.
    #[test]
    fn a_failed_login_or_transport_is_unreachable_and_plain() {
        assert_eq!(parse(b"motd only\n", Posix), None);
        let State::Unreachable(f) = outcome(
            b"",
            "andy@desk: Permission denied (publickey,password).\r\n",
            Posix,
        ) else {
            panic!()
        };
        assert_eq!(f.why, NEEDS_LOGIN);
        for (stderr, want) in [
            (
                "ssh: connect to host desk port 22: Connection refused\n",
                "refused the connection",
            ),
            (
                "ssh: Could not resolve hostname desk: No such host is known.\n",
                "host name was not found",
            ),
            (
                "ssh: connect to host desk port 22: Connection timed out\n",
                "in time",
            ),
            (
                "kex_exchange_identification: read: Connection reset\n",
                "could not reach",
            ),
        ] {
            let State::Unreachable(f) = outcome(b"", stderr, Posix) else {
                panic!("{stderr}")
            };
            assert!(f.why.contains(want), "{stderr}: {}", f.why);
            assert!(f.why.ends_with("instead."), "{}", f.why);
            assert!(!f.why.contains("ssh: "), "raw stderr in {}", f.why);
            assert_eq!(f.detail.as_deref(), Some(stderr.trim()));
        }
        let State::Unreachable(f) = unreachable("ssh did not finish in 15s") else {
            panic!()
        };
        assert!(f.why.contains("in time"), "{}", f.why);
    }

    #[test]
    fn the_ssh_argv_never_prompts_and_carries_the_path_only_as_base64() {
        let path = "~/it's a \"dir\"; rm -rf $HOME `x`";
        let args = list_args(" andy@desk ", RemoteShell::Posix, path).unwrap();
        assert_eq!(
            &args[..7],
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=8",
                "--",
                " andy@desk "
            ]
        );
        assert_eq!(args.len(), 8);
        let line = &args[7];
        let b64 = line
            .strip_prefix("sh -c \"$(printf %s ")
            .and_then(|r| r.strip_suffix(" | base64 -d)\""))
            .expect(line);
        assert!(
            b64.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))
        );
        use base64::Engine;
        let script = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap(),
        )
        .unwrap();
        // The path is one quoted literal under $HOME.
        assert!(
            script.contains(r#"t="$HOME"/'it'\''s a "dir"; rm -rf $HOME `x`'"#),
            "{script}"
        );

        for (shell, exe) in [
            (RemoteShell::Pwsh, "pwsh"),
            (RemoteShell::WindowsPowerShell, "powershell"),
        ] {
            let args = list_args("desk", shell, r"C:\it's").unwrap();
            let b64 = args[7]
                .strip_prefix(&format!(
                    "{exe} -NoLogo -NoProfile -NonInteractive -EncodedCommand "
                ))
                .expect(&args[7]);
            assert!(
                b64.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))
            );
        }
    }

    /// A Windows Remote's command line has a cap; a path that would pass it
    /// is a plain, kept failure, never an ssh run that the Remote cuts.
    #[test]
    fn a_windows_line_past_the_cap_is_refused_plainly() {
        let long = format!(r"C:\{}\", "x".repeat(crate::remote::WINDOWS_LINE_CAP));
        for shell in [RemoteShell::Pwsh, RemoteShell::WindowsPowerShell] {
            assert_eq!(list_line(shell, &long), Err(Failure::new(TOO_LONG)));
            assert!(list_line(shell, r"C:\src\").is_ok());
        }
        // Posix has no such cap.
        assert!(list_line(RemoteShell::Posix, &long).is_ok());
    }

    #[test]
    fn scripts_quote_every_path_form() {
        assert!(posix_script("").contains("t=\"$HOME\"\n"));
        assert!(posix_script("~/").contains("t=\"$HOME\"\n"));
        assert!(posix_script("/srv/a b/").contains("t='/srv/a b/'\n"));
        assert!(ps_script("").contains("$t = $HOME\n"));
        assert!(ps_script(r"~\src\").contains("$t = (Join-Path $HOME 'src\\')\n"));
        assert!(ps_script(r"C:\it's\").contains("$t = 'C:\\it''s\\'\n"));
        // Drives only beside home, and inside the try.
        let home = ps_script("");
        let (inside, after) = home
            .split_once("} catch [System.UnauthorizedAccessException]")
            .unwrap();
        assert!(inside.contains("Get-PSDrive") && !after.contains("Get-PSDrive"));
        assert!(!ps_script(r"C:\").contains("Get-PSDrive"));
    }

    #[test]
    fn the_cache_starts_one_listing_per_key() {
        let mut c = Cache::default();
        let k = Key::new("desk", RemoteShell::Posix, "~/");
        // Not allowed to start yet (debounce): Loading, nothing stored.
        assert_eq!(c.get(&k, false), (Arc::new(State::Loading), false));
        assert_eq!(c.peek(&k), None);
        assert!(c.get(&k, true).1, "first ask starts it");
        assert!(!c.get(&k, true).1, "running: never a second");
        // Home in any spelling is the same listing.
        assert!(!c.get(&Key::new("desk", RemoteShell::Posix, ""), true).1);
        let done = State::Failed(Failure::new("x"));
        c.finish(k.clone(), done.clone());
        assert_eq!(*c.get(&k, true).0, done);
        assert!(!c.get(&k, true).1, "listed once for the life of the cache");
        // Another target, or the same one under another shell, is its own.
        assert!(c.get(&Key::new("pi", RemoteShell::Posix, "~/"), true).1);
        assert!(c.get(&Key::new("desk", RemoteShell::Pwsh, "~/"), true).1);
    }

    /// Two listings at once at most; a third waits as Loading, unstored,
    /// and starts on a later ask once one finishes.
    #[test]
    fn the_cache_runs_two_listings_at_most() {
        let mut c = Cache::default();
        let k = |d: &str| Key::new("desk", RemoteShell::Posix, d);
        assert!(c.get(&k("/a/"), true).1);
        assert!(c.get(&k("/b/"), true).1);
        assert_eq!(c.get(&k("/c/"), true), (Arc::new(State::Loading), false));
        assert_eq!(c.peek(&k("/c/")), None, "waiting, not running");
        assert!(c.running_for(&k("/a/")).is_some());
        c.finish(k("/a/"), State::Failed(Failure::new("x")));
        assert_eq!(c.running_for(&k("/a/")), None);
        assert!(c.get(&k("/c/"), true).1, "a slot came free");
        // A test's put (never started) frees nothing it did not take.
        c.finish(k("/z/"), State::Failed(Failure::new("x")));
        assert!(!c.get(&k("/d/"), true).1);
    }

    /// A script's answer is kept; a transport failure is forgotten, so the
    /// next ask lists again.
    #[test]
    fn unreachable_is_forgotten_and_failed_is_kept() {
        let mut c = Cache::default();
        let gone = Key::new("desk", RemoteShell::Posix, "/a/");
        let missing = Key::new("desk", RemoteShell::Posix, "/b/");
        assert!(c.get(&gone, true).1 && c.get(&missing, true).1);
        c.finish(gone.clone(), unreachable("Connection refused"));
        c.finish(missing.clone(), State::Failed(Failure::new("No such")));
        c.forget();
        assert_eq!(c.peek(&gone), None);
        assert!(c.get(&gone, true).1, "asked again");
        assert!(matches!(*c.peek(&missing).unwrap(), State::Failed(_)));
        assert!(!c.get(&missing, true).1, "never asked again");
    }

    // -----------------------------------------------------------------------
    // The real Posix script, run by a real `sh`
    // -----------------------------------------------------------------------

    /// Whether `sh` runs here (Git Bash on a Windows dev box).
    fn have_sh() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "true"])
            .status()
            .is_ok_and(|s| s.success())
    }

    /// `sh -c` with `script` (`$1` = `arg`) in `home`; ok when it ran.
    fn sh(home: &std::path::Path, script: &str, arg: &str) -> bool {
        std::process::Command::new("sh")
            .args(["-c", script, "sh", arg])
            .current_dir(home)
            .status()
            .is_ok_and(|s| s.success())
    }

    /// `home` as `sh` spells it: forward slashes even on Windows.
    fn sh_path(p: &std::path::Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    /// The generated script for `dir`, run with `$HOME` = `home`, parsed.
    fn run_script(home: &std::path::Path, dir: &str) -> State {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(posix_script(dir))
            .env("HOME", sh_path(home))
            .output()
            .expect("sh ran");
        assert!(out.status.success(), "{out:?}");
        parse(&out.stdout, Posix).unwrap_or_else(|| panic!("no marker: {out:?}"))
    }

    /// The generated Posix script, executed by `sh` against hostile names:
    /// every folder comes back as itself (a symlink to a folder included),
    /// a plain file and a name with a line break never do, past the cap is
    /// cut, and each fail branch says so. Skips when `sh` is not on PATH.
    #[test]
    fn the_posix_script_runs_against_hostile_names() {
        if !have_sh() {
            eprintln!("skipped: no sh on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let h = home.join("hostile");
        std::fs::create_dir(&h).unwrap();
        for name in ["-x", "it's", "$(x)", ".hidden", "sub"] {
            std::fs::create_dir(h.join(name)).unwrap();
        }
        std::fs::write(h.join("plain.txt"), "x").unwrap();
        // A line break and a symlink: through sh, which can make both on
        // every platform that has it (Git Bash maps the newline).
        let newline = sh(&h, "mkdir \"$(printf 'a\\nb')\"", "");
        let link = sh(&h, "ln -s sub link && [ -d link ]", "");
        let State::Listed(l) = run_script(home, "~/hostile/") else {
            panic!()
        };
        let mut want = vec!["$(x)", "-x", "it's"];
        if link {
            want.push("link");
        }
        want.extend(["sub", ".hidden"]);
        assert_eq!(names(&l), want, "newline dir made: {newline}");
        assert!(!l.truncated);
        assert!(l.resolved.ends_with("/hostile"), "{}", l.resolved);
        assert!(l.entries.iter().all(|e| e.blocked.is_none()));

        // Home, by every spelling.
        for dir in ["", "~", "~/"] {
            let State::Listed(l) = run_script(home, dir) else {
                panic!("{dir:?}")
            };
            assert_eq!(names(&l), ["hostile"], "{dir:?}");
        }

        // The cap: CAP + 1 folders list CAP and say so.
        let big = home.join("big");
        std::fs::create_dir(&big).unwrap();
        for i in 0..=CAP {
            std::fs::create_dir(big.join(format!("d{i:04}"))).unwrap();
        }
        let State::Listed(l) = run_script(home, "~/big/") else {
            panic!()
        };
        assert_eq!(l.entries.len(), CAP);
        assert!(l.truncated);
        std::fs::remove_dir_all(&big).unwrap();

        // Each fail branch.
        let failed = |dir: &str, want: &str| match run_script(home, dir) {
            State::Failed(f) => assert!(f.why.contains(want), "{dir}: {}", f.why),
            other => panic!("{dir}: {other:?}"),
        };
        failed("~/nope/", "No such directory");
        failed("~/hostile/plain.txt/", "not a directory");
        failed("~/hostile/plain.txt", "not a directory");
        if newline {
            // A resolved path with a line break would forge a line.
            failed("~/hostile/a\nb/", "line break");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = home.join("locked");
            std::fs::create_dir(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Root reads through any mode: nothing to prove there.
            if std::fs::read_dir(&locked).is_err() {
                failed("~/locked/", "Permission denied");
            }
            let shut = home.join("shut");
            std::fs::create_dir(&shut).unwrap();
            std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o300)).unwrap();
            if std::fs::read_dir(&shut).is_err() {
                failed("~/shut/", "Permission denied");
            }
            for p in [&locked, &shut] {
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
    }

    /// The Posix script against a real Remote, read-only. Run by hand:
    /// `FOREMAN_BROWSE_TARGET=user@pc cargo test --target-dir target/agent
    /// remote_browse::tests::live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_posix_listing() {
        let Ok(target) = std::env::var("FOREMAN_BROWSE_TARGET") else {
            return;
        };
        let never = AtomicBool::new(false);
        for dir in ["", "/", "~/", "/root/", "/nope-foreman/", "/etc/hostname/"] {
            let state = run_listing(&Key::new(&target, RemoteShell::Posix, dir), &never);
            match &state {
                State::Listed(l) => println!(
                    "{dir:?} -> {} in {} (cut: {}), last: {:?}",
                    l.entries.len(),
                    l.resolved,
                    l.truncated,
                    l.entries
                        .iter()
                        .rev()
                        .take(3)
                        .map(|e| &e.name)
                        .collect::<Vec<_>>()
                ),
                other => println!("{dir:?} -> {other:?}"),
            }
        }
    }
}
