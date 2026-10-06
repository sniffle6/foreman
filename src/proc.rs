//! Process-tree agent detection: "what agent is running under this terminal's
//! shell?" — the robust fallback when the cheap signals (dispatch argv, OSC
//! title) don't resolve. A hand-typed `codex` sets a useless OSC title (the
//! username), so we look at the actual OS process tree instead.
//!
//! Interface: [`agent_for`] — give it the shell's PID, get back the agent
//! running under it (or `None`). Everything else (the throttled `sysinfo`
//! refresh, the per-PID memo, the descendant matching) is hidden.
//!
//! Windows-only blind spot: a WSL (`bash`) pane runs the agent *inside* the WSL
//! VM, which isn't a Windows process, so it won't show here — those rely on the
//! OSC-title path.

use crate::icons::IconKind;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Refresh the process table at most this often. The scan is best-effort and the
/// icon can lag this long after an agent starts/exits — fine for a tab badge.
const REFRESH_EVERY: Duration = Duration::from_millis(1500);

/// One process, flattened to just what detection needs. Plain data so the
/// matching logic is unit-tested with synthetic tables (no real OS). Keep it to
/// these five fields.
struct ProcRow {
    pid: u32,
    /// The PID this process was created by. Windows never rewrites it, so after
    /// that parent dies it can name an unrelated process that was handed the
    /// recycled PID — check it with [`is_child_of`], never compare it bare.
    parent: u32,
    /// Creation time in FILETIME ticks (100 ns), or 0 when the process can't be
    /// opened (SYSTEM, protected, or elevated processes).
    started: u64,
    /// Executable file name, e.g. `claude.exe`, `node.exe`, `powershell.exe`.
    name: String,
    /// Full command line; for an interpreter the script path carries the agent
    /// name (`node …\codex\bin\codex.js`).
    cmd: Vec<String>,
}

/// The agent a single process represents, if any: the executable's file stem
/// (`claude.exe` → claude), else any command-line argument's stem (`…\codex.js`
/// → codex). Matching the *stem* — never the whole path — keeps a folder named
/// "claude code" in a path from false-positiving (same rule as the OSC title).
fn agent_of_row(row: &ProcRow) -> Option<IconKind> {
    IconKind::from_title(&row.name).or_else(|| {
        row.cmd
            .iter()
            .skip(1)
            .find_map(|arg| IconKind::from_title(arg))
    })
}

/// Is `row` really `parent`'s child, not an orphan whose dead parent's PID was
/// recycled to `parent`? A child is never created before its parent. A row with
/// an unreadable creation time (0) never passes under a readable parent, so a
/// SYSTEM orphan like `csrss.exe` can't adopt a fresh shell. The cost: a real
/// child the user can't open (possibly an elevated one) goes unlisted.
fn is_child_of(row: &ProcRow, parent: &ProcRow) -> bool {
    row.parent == parent.pid && row.started >= parent.started
}

/// How many parent hops from `row` up to `root` within `table`; `None` when
/// `row` does not descend from `root`. Every hop is checked with
/// [`is_child_of`], so a recycled PID breaks the chain. Bounded against cycles /
/// a corrupt snapshot.
fn depth_below(table: &[ProcRow], row: &ProcRow, root: u32) -> Option<usize> {
    let mut cur = row;
    for depth in 0..64 {
        if cur.pid == root {
            return Some(depth);
        }
        let parent = table.iter().find(|r| r.pid == cur.parent)?;
        if !is_child_of(cur, parent) {
            return None;
        }
        cur = parent;
    }
    None
}

/// Does `row` descend from `root` within `table`?
fn descends_from(table: &[ProcRow], row: &ProcRow, root: u32) -> bool {
    depth_below(table, row, root).is_some()
}

/// The agent running under `root_pid` in this process table, if any. Pure: the
/// unit-test surface. Finds agent-named processes that descend from the shell
/// — so an agent under a *different* terminal never leaks in — and picks the
/// one closest to the shell. An agent launched by another agent's tool
/// (`codex exec` from Claude's Bash) is deeper, so it never flips the icon; the
/// table comes from a HashMap, so first-found would be arbitrary.
fn detect_agent(table: &[ProcRow], root_pid: u32) -> Option<IconKind> {
    table
        .iter()
        .filter_map(|row| {
            let kind = agent_of_row(row)?;
            let depth = depth_below(table, row, root_pid)?;
            Some((depth, row.pid, kind))
        })
        .min_by_key(|(depth, pid, _)| (*depth, *pid))
        .map(|(_, _, kind)| kind)
}

/// One top-level process the shell launched, for the close-confirm list. Plain
/// data so the selection logic is unit-tested with synthetic tables.
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    /// How many further processes sit under this one (its own subtree, minus
    /// console-host plumbing). Rendered as "(+n)"; closing kills them too.
    pub background: usize,
}

/// Console-host plumbing ConPTY spawns around a shell — never real user work.
const HOST_PLUMBING: &[&str] = &["openconsole.exe", "conhost.exe"];

fn is_plumbing(name: &str) -> bool {
    HOST_PLUMBING.contains(&name.to_ascii_lowercase().as_str())
}

/// Count every process under `root` (any depth), minus `root` and console-host
/// plumbing. The rollup shown as "(+n)" next to a top-level process.
fn count_descendants(table: &[ProcRow], root: u32) -> usize {
    table
        .iter()
        .filter(|r| r.pid != root)
        .filter(|r| !is_plumbing(&r.name))
        .filter(|r| descends_from(table, r, root))
        .count()
}

/// Pure: the shell's direct children worth warning about — what the user
/// actually launched (an agent, a build, a REPL, a server) — each carrying a
/// count of its own subtree. Console-host plumbing is skipped. The test surface.
/// An agent that spawns a fleet of MCP servers shows as one row with a big
/// rollup, not one row per helper. Empty when the shell isn't in the table.
fn collect_top_level(table: &[ProcRow], root: u32) -> Vec<ProcInfo> {
    let Some(shell) = table.iter().find(|r| r.pid == root) else {
        return Vec::new();
    };
    table
        .iter()
        .filter(|r| is_child_of(r, shell))
        .filter(|r| !is_plumbing(&r.name))
        .map(|r| ProcInfo {
            pid: r.pid,
            name: r.name.clone(),
            background: count_descendants(table, r.pid),
        })
        .collect()
}

/// The shell's live top-level children (with subtree rollups). Throttled through
/// the same scanner as `agent_for`; empty for an idle shell.
pub fn top_children(root_pid: u32) -> Vec<ProcInfo> {
    SCANNER.with(|s| {
        let mut s = s.borrow_mut();
        let stale = s.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        if stale {
            s.refresh();
        }
        collect_top_level(&s.table, root_pid)
    })
}

/// Force an immediate table refresh, ignoring the throttle, and reset it so the
/// following `top_children` calls this frame reuse the fresh scan. Called once at
/// the instant a close/quit is requested: without it, a child spawned inside the
/// last throttle window (<`REFRESH_EVERY`) is invisible and the pane closes with
/// no warning — the exact silent-kill the confirm exists to prevent. One
/// synchronous scan on the caller's thread, on the closing click (not the modal);
/// the icon detector already runs the same scan every `REFRESH_EVERY` anyway.
pub fn refresh_now() {
    SCANNER.with(|s| s.borrow_mut().refresh());
}

struct Scanner {
    sys: sysinfo::System,
    table: Vec<ProcRow>,
    last_refresh: Option<Instant>,
    /// Memoized answer per shell PID, valid until the next refresh.
    memo: HashMap<u32, Option<IconKind>>,
}

impl Scanner {
    fn new() -> Self {
        Self {
            sys: sysinfo::System::new(),
            table: Vec::new(),
            last_refresh: None,
            memo: HashMap::new(),
        }
    }

    fn refresh(&mut self) {
        self.sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing().with_cmd(sysinfo::UpdateKind::Always),
        );
        self.table = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| ProcRow {
                pid: pid.as_u32(),
                parent: p.parent().map(|pp| pp.as_u32()).unwrap_or(0),
                started: creation_time(pid.as_u32()),
                name: p.name().to_string_lossy().into_owned(),
                cmd: p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect(),
            })
            .collect();
        self.memo.clear();
        self.last_refresh = Some(Instant::now());
    }
}

/// `pid`'s creation time in FILETIME ticks, or 0 when it can't be opened.
/// sysinfo's `start_time` is whole seconds, which can't order an orphan and a
/// process that took its dead parent's PID within the same second — exactly the
/// churn case. One open + query per process per refresh (~1 ms for ~430
/// processes, measured 2026-10-06 in a debug build, against ~20 ms for the
/// sysinfo refresh itself).
fn creation_time(pid: u32) -> u64 {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return 0;
        }
        let [mut created, mut exited, mut kernel, mut user] = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let ok = GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(h);
        if ok == 0 {
            return 0;
        }
        (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)
    }
}

thread_local! {
    static SCANNER: RefCell<Scanner> = RefCell::new(Scanner::new());
}

/// The agent running under `root_pid` (a terminal's shell PID), or `None`.
/// Throttled: refreshes the OS process table at most every [`REFRESH_EVERY`] and
/// memoizes per PID between refreshes, so calling it per-tab per-frame is cheap.
pub fn agent_for(root_pid: u32) -> Option<IconKind> {
    SCANNER.with(|s| {
        let mut s = s.borrow_mut();
        let stale = s.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        if stale {
            s.refresh();
        }
        if let Some(&cached) = s.memo.get(&root_pid) {
            return cached;
        }
        let result = detect_agent(&s.table, root_pid);
        s.memo.insert(root_pid, result);
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture row born in pid order (`started = pid`), so a parent with a
    /// lower pid is older — a real parent/child pair. Recycled-PID cases set
    /// `started` explicitly with [`born`].
    fn row(pid: u32, parent: u32, name: &str, cmd: &[&str]) -> ProcRow {
        ProcRow {
            pid,
            parent,
            started: u64::from(pid),
            name: name.to_string(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn born(started: u64, pid: u32, parent: u32, name: &str) -> ProcRow {
        ProcRow {
            started,
            ..row(pid, parent, name, &[name])
        }
    }

    #[test]
    fn direct_child_claude_is_detected() {
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(200, 100, "claude.exe", &["claude"]),
        ];
        assert_eq!(detect_agent(&t, 100), Some(IconKind::Claude));
    }

    #[test]
    fn codex_via_node_script_is_detected() {
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(
                200,
                100,
                "node.exe",
                &["node", r"C:\npm\node_modules\@openai\codex\bin\codex.js"],
            ),
        ];
        assert_eq!(detect_agent(&t, 100), Some(IconKind::Codex));
    }

    #[test]
    fn direct_child_grok_is_detected() {
        // Real install is a native `grok.exe` (e.g. `%USERPROFILE%\.grok\bin\grok.exe`).
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(200, 100, "grok.exe", &["grok"]),
        ];
        assert_eq!(detect_agent(&t, 100), Some(IconKind::Grok));
    }

    #[test]
    fn plain_shell_has_no_agent() {
        let t = vec![row(100, 1, "powershell.exe", &["powershell"])];
        assert_eq!(detect_agent(&t, 100), None);
    }

    #[test]
    fn tool_the_agent_spawns_does_not_change_the_match() {
        // shell -> claude -> bash (a tool). Still Claude; bash is not an agent.
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(200, 100, "claude.exe", &["claude"]),
            row(300, 200, "bash.exe", &["bash", "-c", "ls"]),
        ];
        assert_eq!(detect_agent(&t, 100), Some(IconKind::Claude));
    }

    #[test]
    fn folder_named_claude_in_a_script_path_does_not_false_positive() {
        // A plain build script that happens to live under "H:\claude code\…".
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(
                200,
                100,
                "node.exe",
                &["node", r"H:\claude code\foreman\build.js"],
            ),
        ];
        assert_eq!(detect_agent(&t, 100), None);
    }

    #[test]
    fn agent_under_another_terminal_does_not_leak() {
        // Two shells; claude runs under 100, but we ask about shell 500.
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(200, 100, "claude.exe", &["claude"]),
            row(500, 1, "powershell.exe", &["powershell"]),
        ];
        assert_eq!(detect_agent(&t, 500), None);
    }

    #[test]
    fn closest_agent_wins_over_a_deeper_one_regardless_of_table_order() {
        // Claude's Bash tool runs `codex exec`: shell -> claude -> bash -> codex.
        // sysinfo's table is a HashMap, so the deeper agent may be listed first.
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(
                400,
                300,
                "node.exe",
                &["node", r"C:\npm\node_modules\@openai\codex\bin\codex.js"],
            ),
            row(
                300,
                200,
                "bash.exe",
                &["bash", "-c", "codex exec 'echo hi'"],
            ),
            row(200, 100, "claude.exe", &["claude"]),
        ];
        assert_eq!(detect_agent(&t, 100), Some(IconKind::Claude));
        let mut reversed = t;
        reversed.reverse();
        assert_eq!(detect_agent(&reversed, 100), Some(IconKind::Claude));
    }

    #[test]
    fn agent_nested_one_wrapper_deep_is_found() {
        // shell -> cmd -> claude (dispatched-style wrapping).
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(150, 100, "cmd.exe", &["cmd", "/c", "claude"]),
            row(200, 150, "claude.exe", &["claude"]),
        ];
        assert_eq!(detect_agent(&t, 100), Some(IconKind::Claude));
    }

    #[test]
    fn top_level_lists_the_direct_child() {
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(200, 100, "claude.exe", &["claude"]),
        ];
        let d = collect_top_level(&t, 100);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].pid, 200);
        assert_eq!(d[0].name, "claude.exe");
        assert_eq!(d[0].background, 0, "a leaf child has no subtree");
    }

    #[test]
    fn top_level_rolls_grandchildren_into_the_child() {
        // shell -> claude -> {rg, node}: one top-level row (claude), rollup +2.
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(200, 100, "claude.exe", &["claude"]),
            row(300, 200, "rg.exe", &["rg", "foo"]),
            row(400, 200, "node.exe", &["node"]),
        ];
        let d = collect_top_level(&t, 100);
        assert_eq!(d.len(), 1, "only the direct child is a top-level row");
        assert_eq!(d[0].name, "claude.exe");
        assert_eq!(d[0].background, 2, "both grandchildren rolled up");
    }

    #[test]
    fn top_level_excludes_console_host_plumbing() {
        // plumbing is skipped as a row and left out of the rollup count.
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(200, 100, "OpenConsole.exe", &["OpenConsole"]),
            row(210, 100, "conhost.exe", &["conhost"]),
            row(300, 100, "node.exe", &["node"]),
            row(310, 300, "conhost.exe", &["conhost"]),
        ];
        let d = collect_top_level(&t, 100);
        let names: Vec<&str> = d.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["node.exe"],
            "host plumbing leaked into the list"
        );
        assert_eq!(d[0].background, 0, "plumbing under node was not counted");
    }

    #[test]
    fn top_level_is_empty_for_an_idle_shell() {
        let t = vec![row(100, 1, "powershell.exe", &["powershell"])];
        assert!(
            collect_top_level(&t, 100).is_empty(),
            "an idle shell has no children to warn about"
        );
    }

    #[test]
    fn top_level_does_not_leak_across_shells() {
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(200, 100, "claude.exe", &["claude"]),
            row(500, 1, "powershell.exe", &["powershell"]),
        ];
        assert!(
            collect_top_level(&t, 500).is_empty(),
            "another shell's child leaked in"
        );
    }

    // Windows recycles PIDs and never rewrites a child's parent PID, so an
    // orphan reads as the child of whichever process later gets its dead
    // parent's PID. Seen live 2026-10-06: an idle cmd.exe handed PID 7992 listed
    // nextcloud.exe (whose launcher had been PID 7992) as its child — the
    // idle_terminals_produce_no_groups flake.

    #[test]
    fn orphan_does_not_become_the_child_of_a_shell_that_took_its_dead_parents_pid() {
        // nextcloud (born t=100) was launched by PID 7992, long dead; a new
        // cmd.exe (born t=900, under foreman) now holds PID 7992.
        let t = vec![
            born(50, 40, 1, "foreman.exe"),
            born(100, 2516, 7992, "nextcloud.exe"),
            born(900, 7992, 40, "cmd.exe"),
        ];
        assert!(
            collect_top_level(&t, 7992).is_empty(),
            "an orphan older than the shell was listed as its child"
        );
    }

    #[test]
    fn orphaned_agent_does_not_badge_the_shell_that_took_its_parents_pid() {
        // The tab-icon path: a codex.exe whose launcher died must not mark an
        // unrelated new shell as running Codex.
        let t = vec![
            born(100, 3000, 4000, "codex.exe"),
            born(900, 4000, 40, "powershell.exe"),
        ];
        assert_eq!(detect_agent(&t, 4000), None);
    }

    #[test]
    fn recycled_pid_deep_in_the_tree_cuts_only_the_orphan() {
        // shell -> node (PID 300, recycled from an old launcher). The orphan
        // claude.exe the old launcher left behind is neither counted in node's
        // rollup nor found as the shell's agent; node itself still lists.
        let t = vec![
            born(10, 100, 1, "powershell.exe"),
            born(5, 400, 300, "claude.exe"),
            born(20, 300, 100, "node.exe"),
            born(30, 500, 300, "rg.exe"),
        ];
        let d = collect_top_level(&t, 100);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "node.exe");
        assert_eq!(d[0].background, 1, "only rg is node's real child");
        assert_eq!(detect_agent(&t, 100), None);
    }

    #[test]
    fn unreadable_orphan_does_not_become_the_child_of_a_readable_shell() {
        // csrss.exe / winlogon.exe can't be opened by a normal user, so their
        // creation time reads 0; their dead parent's recycled PID must not
        // adopt them.
        let t = vec![
            born(0, 1800, 1784, "csrss.exe"),
            born(0, 1944, 1784, "winlogon.exe"),
            born(900, 1784, 40, "cmd.exe"),
        ];
        assert!(collect_top_level(&t, 1784).is_empty());
    }

    #[test]
    fn a_child_born_in_the_same_tick_as_its_shell_still_counts() {
        // Ties can't come from recycling (the orphan predates the new parent),
        // so equal creation times keep the link.
        let t = vec![
            born(500, 100, 1, "powershell.exe"),
            born(500, 200, 100, "claude.exe"),
        ];
        assert_eq!(collect_top_level(&t, 100).len(), 1);
        assert_eq!(detect_agent(&t, 100), Some(IconKind::Claude));
    }

    #[test]
    fn top_level_is_empty_when_the_shell_is_not_in_the_table() {
        // Without the shell's own row its children's links can't be checked.
        let t = vec![row(200, 100, "claude.exe", &["claude"])];
        assert!(collect_top_level(&t, 100).is_empty());
    }

    #[test]
    fn a_live_child_passes_the_creation_time_check() {
        // End to end against the real OS: a process this test spawns is still
        // listed under the test process once real creation times are compared.
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn ping");
        refresh_now();
        let listed = top_children(std::process::id())
            .iter()
            .any(|p| p.pid == child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            listed,
            "a real child was dropped by the creation-time check"
        );
    }

    #[test]
    fn refresh_now_scans_the_live_table() {
        // Forces a real scan (ignoring the throttle) and reads back the table via
        // the current process's PID — proving the close-time refresh actually
        // repopulates, so a just-spawned child would be visible.
        refresh_now();
        let me = std::process::id();
        let seen = SCANNER.with(|s| s.borrow().table.iter().any(|r| r.pid == me));
        assert!(
            seen,
            "a forced refresh should include the running test process"
        );
    }
}
