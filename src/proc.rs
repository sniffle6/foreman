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
/// these four fields.
struct ProcRow {
    pid: u32,
    parent: u32,
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
    IconKind::from_title(&row.name)
        .or_else(|| row.cmd.iter().skip(1).find_map(|arg| IconKind::from_title(arg)))
}

/// Does `pid` descend from `root` within `table`? Walks the parent chain up,
/// bounded against cycles / a corrupt snapshot.
fn descends_from(table: &[ProcRow], pid: u32, root: u32) -> bool {
    let mut cur = pid;
    for _ in 0..64 {
        if cur == root {
            return true;
        }
        match table.iter().find(|r| r.pid == cur) {
            Some(r) => cur = r.parent,
            None => return false,
        }
    }
    false
}

/// The agent running under `root_pid` in this process table, if any. Pure: the
/// unit-test surface. Finds an agent-named process and confirms it descends from
/// the shell — so a tool the agent itself spawns (a `bash` for a command) never
/// counts, and an agent under a *different* terminal never leaks in.
fn detect_agent(table: &[ProcRow], root_pid: u32) -> Option<IconKind> {
    table.iter().find_map(|row| {
        let kind = agent_of_row(row)?;
        descends_from(table, row.pid, root_pid).then_some(kind)
    })
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

    fn row(pid: u32, parent: u32, name: &str, cmd: &[&str]) -> ProcRow {
        ProcRow {
            pid,
            parent,
            name: name.to_string(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
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
            row(200, 100, "node.exe", &["node", r"H:\claude code\foreman\build.js"]),
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
    fn agent_nested_one_wrapper_deep_is_found() {
        // shell -> cmd -> claude (dispatched-style wrapping).
        let t = vec![
            row(100, 1, "powershell.exe", &["powershell"]),
            row(150, 100, "cmd.exe", &["cmd", "/c", "claude"]),
            row(200, 150, "claude.exe", &["claude"]),
        ];
        assert_eq!(detect_agent(&t, 100), Some(IconKind::Claude));
    }
}
