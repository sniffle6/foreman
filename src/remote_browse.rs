//! Listing a Remote's folders for the directory picker under a Directory
//! field (the directory prompt and the Remotes pane). The field text is the
//! source of truth: [`split`] reads which directory it names and the prefix
//! that filters it, and [`descend`] / [`up`] write a choice back. Listings
//! run over `ssh` with `BatchMode` on a worker thread, cached per (target,
//! shell, directory) for the life of the widget that owns the cache.
//! Everything above the probe section is pure and table-tested; the widget
//! is [`crate::remote_browse_ui`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;

use crate::remote::RemoteShell;

/// Most folders one listing keeps. Past it the list says it was cut.
pub const CAP: usize = 500;
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

/// One folder of a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// A Windows drive root (`C:\`), listed beside home's folders.
    pub drive: bool,
    /// `name` lowercased once, for the prefix filter.
    lower: String,
}

impl Entry {
    pub fn dir(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            drive: false,
            lower: name.to_lowercase(),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Asked, or about to be: nothing to show yet.
    Loading,
    Listed(Listed),
    /// A plain sentence for the list area.
    Failed(String),
}

/// What the list area says when `BatchMode` was refused.
pub const NEEDS_LOGIN: &str = "Can't list folders: logging in to this Remote needs a password or passphrase. Type the path instead.";

/// The script for a Posix Remote, run by plain `sh`. Prints the marker
/// lines [`parse`] reads: the resolved directory, `d <name>` per folder (at
/// most `CAP + 1`, so a cut shows), then an end line. A name holding a line
/// break is skipped: it cannot be told apart from two names.
pub fn posix_script(dir: &str) -> String {
    let target = match crate::remote::home_relative(dir) {
        Some("") => "\"$HOME\"".to_string(),
        Some(rest) => format!("\"$HOME\"/{}", crate::remote::sh_quote(rest)),
        None if dir.is_empty() => "\"$HOME\"".to_string(),
        None => crate::remote::sh_quote(dir),
    };
    format!(
        "unset CDPATH
t={target}
if ! cd -- \"$t\" 2>/dev/null; then
  u=${{t%/}}; [ -n \"$u\" ] || u=/
  if [ ! -e \"$u\" ]; then echo '{MARK} fail missing'
  elif [ ! -d \"$u\" ]; then echo '{MARK} fail notdir'
  else echo '{MARK} fail denied'; fi
  exit 0
fi
[ -r . ] || {{ echo '{MARK} fail denied'; exit 0; }}
printf '{MARK} dir %s\\n' \"$(pwd)\"
n=0
nl='
'
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
/// listing home, where a bare drive is typed. Untested: no Windows Remote
/// has existed.
pub fn ps_script(dir: &str) -> String {
    let target = match crate::remote::home_relative(dir) {
        Some("") => "$HOME".to_string(),
        Some(rest) => format!("(Join-Path $HOME {})", crate::remote::ps_quote(rest)),
        None if dir.is_empty() => "$HOME".to_string(),
        None => crate::remote::ps_quote(dir),
    };
    let drives = if dir.is_empty() || is_home(dir) {
        "Get-PSDrive -PSProvider FileSystem | ForEach-Object { 'v ' + $_.Root }\n"
    } else {
        ""
    };
    format!(
        "$ErrorActionPreference = 'Stop'
try {{ [Console]::OutputEncoding = [Text.Encoding]::UTF8 }} catch {{}}
$t = {target}
if (-not (Test-Path -LiteralPath $t)) {{ '{MARK} fail missing'; exit 0 }}
if (-not (Test-Path -LiteralPath $t -PathType Container)) {{ '{MARK} fail notdir'; exit 0 }}
try {{
  $p = (Get-Item -LiteralPath $t -Force).FullName
  $d = @(Get-ChildItem -LiteralPath $p -Directory -Force | Select-Object -First {more})
}} catch [System.UnauthorizedAccessException] {{ '{MARK} fail denied'; exit 0 }}
catch {{ '{MARK} fail other ' + $_.Exception.Message; exit 0 }}
'{MARK} dir ' + $p
foreach ($e in $d) {{ 'd ' + $e.Name }}
{drives}'{MARK} end'
",
        more = CAP + 1
    )
}

/// The remote line for listing `dir`: the script as base64, so the login
/// shell never sees a quote or a space from the typed path.
pub fn list_line(shell: RemoteShell, dir: &str) -> String {
    match shell {
        RemoteShell::Posix => crate::remote::posix_line(&posix_script(dir)),
        RemoteShell::Pwsh | RemoteShell::WindowsPowerShell => {
            let exe = if shell == RemoteShell::Pwsh {
                "pwsh"
            } else {
                "powershell"
            };
            format!(
                "{exe} -NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
                crate::remote::utf16le_b64(&ps_script(dir))
            )
        }
    }
}

/// The `ssh` arguments (after `ssh`) one listing runs: never a prompt, a
/// connect deadline, no PTY.
pub fn list_args(target: &str, shell: RemoteShell, dir: &str) -> Vec<String> {
    [
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
    .chain([list_line(shell, dir)])
    .collect()
}

/// Read a listing's stdout. `None` when no marker came back: the login
/// itself failed, and stderr says why.
pub fn parse(stdout: &str) -> Option<State> {
    let mut lines = stdout
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l));
    let head = lines.find_map(|l| l.strip_prefix(MARK)?.strip_prefix(' '))?;
    if let Some(why) = head.strip_prefix("fail") {
        let why = why.trim();
        return Some(State::Failed(match why {
            "missing" => "No such directory on this Remote".into(),
            "notdir" => "That is a file, not a directory".into(),
            "denied" => "Permission denied: this directory cannot be listed".into(),
            other => {
                let msg = other.strip_prefix("other").unwrap_or(other).trim();
                if msg.is_empty() {
                    "The Remote could not list this directory".into()
                } else {
                    format!("Can't list: {msg}")
                }
            }
        }));
    }
    let resolved = head.strip_prefix("dir ")?.to_string();
    let mut entries = Vec::new();
    let mut ended = false;
    let mut last_cut = false;
    for line in lines {
        if line.strip_prefix(MARK).is_some_and(|r| r.trim() == "end") {
            ended = true;
            break;
        }
        last_cut = false;
        if let Some(name) = line.strip_prefix("d ") {
            if !name.is_empty() {
                entries.push(Entry::dir(name));
                last_cut = true;
            }
        } else if let Some(root) = line.strip_prefix("v ")
            && !root.is_empty()
        {
            entries.push(Entry::drive(root));
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
pub fn outcome(stdout: &str, stderr: &str) -> State {
    if let Some(s) = parse(stdout) {
        return s;
    }
    match crate::remote_probe::classify_failure(stderr) {
        crate::remote_probe::TestOutcome::NeedsLogin => State::Failed(NEEDS_LOGIN.into()),
        crate::remote_probe::TestOutcome::Failed(e) => State::Failed(format!("Can't list: {e}")),
        crate::remote_probe::TestOutcome::Ok(_) => State::Failed("Can't list".into()),
    }
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

/// Listings by key. A key is listed once; its first ask stores `Loading`,
/// which is also what says a listing is running.
#[derive(Debug, Default)]
pub struct Cache {
    entries: HashMap<Key, Arc<State>>,
}

impl Cache {
    /// The state for `key`, and whether the caller must start its listing:
    /// true only on the first ask that `may_start`.
    pub fn get(&mut self, key: &Key, may_start: bool) -> (Arc<State>, bool) {
        if let Some(s) = self.entries.get(key) {
            return (s.clone(), false);
        }
        let loading = Arc::new(State::Loading);
        if may_start {
            self.entries.insert(key.clone(), loading.clone());
        }
        (loading, may_start)
    }

    /// The state for `key` without asking for it.
    pub fn peek(&self, key: &Key) -> Option<Arc<State>> {
        self.entries.get(key).cloned()
    }

    pub fn finish(&mut self, key: Key, state: State) {
        self.entries.insert(key, Arc::new(state));
    }
}

pub type Shared = Arc<Mutex<Cache>>;

/// The listing of `key`, starting it on a worker thread on the first ask
/// that `may_start`. The shape of [`crate::remote_probe::reach`]: a lock and
/// a map lookup on the frame path, `ssh` only off it.
pub fn list(ctx: &egui::Context, cache: &Shared, key: &Key, may_start: bool) -> Arc<State> {
    let (state, start) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key, may_start);
    if start {
        spawn(ctx, cache, key);
    }
    state
}

/// Unit tests never reach a network: the entry stays `Loading`.
#[cfg(test)]
fn spawn(_ctx: &egui::Context, _cache: &Shared, _key: &Key) {}

#[cfg(not(test))]
fn spawn(ctx: &egui::Context, cache: &Shared, key: &Key) {
    let (ctx, cache, key) = (ctx.clone(), cache.clone(), key.clone());
    std::thread::spawn(move || {
        let state = run_listing(&key);
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finish(key, state);
        ctx.request_repaint();
    });
}

/// Run one listing now. Blocks up to [`LIST_TIMEOUT`]: worker threads only.
pub fn run_listing(key: &Key) -> State {
    let args = list_args(&key.target, key.shell, &key.dir);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match crate::remote_probe::run(&args, LIST_TIMEOUT) {
        Ok(out) => outcome(&out.stdout, &out.stderr),
        Err(e) => State::Failed(format!("Can't list: {e}")),
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
        let l = listed(parse(out));
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
    }

    #[test]
    fn an_empty_directory_lists_nothing_and_crlf_is_read() {
        let l = listed(parse(
            "foreman-browse dir /srv/empty\r\nforeman-browse end\r\n",
        ));
        assert!(l.entries.is_empty() && !l.truncated);
        assert_eq!(l.resolved, "/srv/empty");
        // No trailing newline at all.
        let l = listed(parse("foreman-browse dir /x\nd a\nforeman-browse end"));
        assert_eq!(names(&l), ["a"]);
    }

    #[test]
    fn windows_drives_come_after_folders() {
        let out = "foreman-browse dir C:\\Users\\andy\r\nd Desktop\r\nd .vscode\r\nv C:\\\r\nv D:\\\r\nforeman-browse end\r\n";
        let l = listed(parse(out));
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
        let l = listed(parse(&out));
        assert_eq!(l.entries.len(), CAP);
        assert!(l.truncated);
        // The pipe was cut: no end line, and the last name may be half one.
        let l = listed(parse("foreman-browse dir /x\nd alpha\nd bet"));
        assert_eq!(names(&l), ["alpha"]);
        assert!(l.truncated);
    }

    #[test]
    fn script_failures_become_plain_messages() {
        for (out, want) in [
            ("foreman-browse fail missing\n", "No such directory"),
            ("foreman-browse fail notdir\n", "a file, not a directory"),
            ("foreman-browse fail denied\n", "Permission denied"),
            (
                "foreman-browse fail other Access to the path is denied.\r\n",
                "Can't list: Access to the path is denied.",
            ),
        ] {
            match parse(out) {
                Some(State::Failed(m)) => assert!(m.contains(want), "{m}"),
                other => panic!("{out}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_failed_login_is_said_plainly() {
        assert_eq!(parse("motd only\n"), None);
        assert_eq!(
            outcome("", "andy@desk: Permission denied (publickey,password).\r\n"),
            State::Failed(NEEDS_LOGIN.into())
        );
        assert_eq!(
            outcome(
                "",
                "ssh: connect to host desk port 22: Connection refused\n"
            ),
            State::Failed(
                "Can't list: ssh: connect to host desk port 22: Connection refused".into()
            )
        );
    }

    #[test]
    fn the_ssh_argv_never_prompts_and_carries_the_path_only_as_base64() {
        let path = "~/it's a \"dir\"; rm -rf $HOME `x`";
        let args = list_args(" andy@desk ", RemoteShell::Posix, path);
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
            let args = list_args("desk", shell, r"C:\it's");
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

    #[test]
    fn scripts_quote_every_path_form() {
        assert!(posix_script("").contains("t=\"$HOME\"\n"));
        assert!(posix_script("~/").contains("t=\"$HOME\"\n"));
        assert!(posix_script("/srv/a b/").contains("t='/srv/a b/'\n"));
        assert!(ps_script("").contains("$t = $HOME\n"));
        assert!(ps_script(r"~\src\").contains("$t = (Join-Path $HOME 'src\\')\n"));
        assert!(ps_script(r"C:\it's\").contains("$t = 'C:\\it''s\\'\n"));
        // Drives only beside home.
        assert!(ps_script("").contains("Get-PSDrive"));
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
        assert!(c.get(&Key::new("desk", RemoteShell::Posix, "/"), true).1);
        let done = State::Failed("x".into());
        c.finish(k.clone(), done.clone());
        assert_eq!(*c.get(&k, true).0, done);
        assert!(!c.get(&k, true).1, "listed once for the life of the cache");
        // Another target, or the same one under another shell, is its own.
        assert!(c.get(&Key::new("pi", RemoteShell::Posix, "~/"), true).1);
        assert!(c.get(&Key::new("desk", RemoteShell::Pwsh, "~/"), true).1);
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
        for dir in ["", "/", "~/", "/root/", "/nope-foreman/", "/etc/hostname/"] {
            let state = run_listing(&Key::new(&target, RemoteShell::Posix, dir));
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
