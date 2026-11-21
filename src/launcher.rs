//! Shared project launcher choices for titlebar and task-manager panel.

use crate::keymap::{Command, Keymap};
use crate::landing::SessionKind;
use crate::remote_probe::Reach;
use crate::terminal::Shell;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Launch {
    /// On this PC.
    Agent(SessionKind),
    /// On this PC.
    Shell(Shell),
    /// On the Remote the Project's where-strip names, looked up when applied.
    RemoteAgent(SessionKind),
    /// The Remote's login shell, on the Remote the where-strip names.
    RemoteShell,
    /// The "+" itself: the default shell wherever the where-strip points.
    Default,
    /// Point the Project's where-strip at this PC (`None`) or at
    /// `Settings::remotes[i]` as listed this frame. Opens nothing.
    Where(Option<usize>),
    Tool(Tool),
    NewProject,
}

/// A Remote as the where-strip shows it.
pub struct Spot<'a> {
    pub name: &'a str,
    pub target: &'a str,
    pub shell: crate::remote::RemoteShell,
    pub reach: Reach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Chat,
    Board,
    Plan,
    GitHistory,
}

impl Tool {
    pub fn command(self) -> Command {
        match self {
            Self::Chat => Command::OpenChat,
            Self::Board => Command::OpenBoard,
            Self::Plan => Command::OpenPlan,
            Self::GitHistory => Command::OpenGitHistory,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Board => "Board",
            Self::Plan => "Plan",
            Self::GitHistory => "Git history",
        }
    }
}

/// The launcher for a Project whose where-strip names `launch_on`, from this
/// frame's settings, keymap and reachability. Build only after the hover
/// menu opens: that is what keeps the probes to while it is on screen.
pub fn menu(
    ctx: &eframe::egui::Context,
    open_tools: &HashSet<Tool>,
    launch_on: Option<&str>,
) -> Vec<crate::hover_menu::Entry<Launch>> {
    let live = crate::config::live(ctx);
    let spots: Vec<Spot> = live
        .remotes
        .iter()
        .map(|r| Spot {
            name: &r.name,
            target: &r.target,
            shell: r.shell,
            reach: crate::remote_probe::reach(ctx, &r.target),
        })
        .collect();
    let on = launch_on.and_then(|n| {
        live.remotes
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case(n))
    });
    entries_on(
        &crate::keymap::live(ctx),
        live.default_shell.to_shell(),
        open_tools,
        &spots,
        on,
    )
}

/// Pure ordered menu model with no Remotes. Build only after the hover menu
/// opens.
pub fn entries(
    keymap: &Keymap,
    default_shell: Shell,
    open_tools: &HashSet<Tool>,
) -> Vec<crate::hover_menu::Entry<Launch>> {
    entries_on(keymap, default_shell, open_tools, &[], None)
}

/// [`entries`] with the where-strip: shown only when there is a Remote,
/// with `on` the selected index into `spots` (`None` = this PC). Under a
/// Remote, agents open there and SHELLS is the Remote's one login shell.
pub fn entries_on(
    keymap: &Keymap,
    default_shell: Shell,
    open_tools: &HashSet<Tool>,
    spots: &[Spot],
    on: Option<usize>,
) -> Vec<crate::hover_menu::Entry<Launch>> {
    use crate::hover_menu::Entry;
    let on = on.filter(|i| *i < spots.len());
    let mut rows = Vec::new();
    if !spots.is_empty() {
        rows.push(Entry::Strip(strip(spots, on)));
    }
    rows.push(Entry::Header("AGENTS"));
    for agent in [SessionKind::Claude, SessionKind::Codex, SessionKind::Grok] {
        rows.push(Entry::Item {
            label: agent.label(),
            hint: None,
            mark: false,
            act: match on {
                None => Launch::Agent(agent),
                Some(_) => Launch::RemoteAgent(agent),
            },
        });
    }
    rows.push(Entry::Header("SHELLS"));
    if let Some(i) = on {
        rows.push(Entry::Item {
            label: "Shell",
            hint: Some(format!(
                "({})",
                crate::remotes_pane::shell_label(spots[i].shell)
            )),
            mark: false,
            act: Launch::RemoteShell,
        });
    } else {
        for (label, shell) in [
            ("PowerShell", Shell::PowerShell),
            ("CMD", Shell::Cmd),
            ("SH", Shell::Bash),
        ] {
            rows.push(Entry::Item {
                label,
                hint: (shell == default_shell).then(|| "(default)".into()),
                mark: false,
                act: Launch::Shell(shell),
            });
        }
    }
    rows.push(Entry::Header("PROJECT"));
    for tool in [Tool::Chat, Tool::Board, Tool::Plan, Tool::GitHistory] {
        rows.push(Entry::Item {
            label: tool.label(),
            hint: keymap
                .chord_for(tool.command())
                .map(|chord| format!("{} {}", keymap.leader.pretty(), chord.pretty())),
            mark: open_tools.contains(&tool),
            act: Launch::Tool(tool),
        });
    }
    rows.push(Entry::Divider);
    rows.push(Entry::Item {
        label: "New project…",
        hint: None,
        mark: false,
        act: Launch::NewProject,
    });
    rows
}

/// `Local | desk | laptop …`. Every Remote carries a status glyph (in a slot
/// that exists before the first answer, so nothing moves when it lands) and
/// a tooltip that says the state in words. Offline is dimmed, not disabled:
/// the probe cannot see through a `ProxyJump`.
fn strip(spots: &[Spot], on: Option<usize>) -> Vec<crate::hover_menu::Seg<Launch>> {
    use crate::hover_menu::{Seg, SegMark};
    let mut segs = vec![Seg {
        label: "Local".into(),
        selected: on.is_none(),
        mark: SegMark::None,
        dim: false,
        tip: "Open on this PC".into(),
        act: Launch::Where(None),
    }];
    for (i, s) in spots.iter().enumerate() {
        let (mark, state) = match &s.reach {
            Reach::Reachable => (SegMark::Up, "reachable".to_string()),
            Reach::Offline => (
                SegMark::Down,
                format!("offline. {}", crate::remotes_pane::OFFLINE_WHY),
            ),
            Reach::Checking => (SegMark::Pending, "checking whether it answers…".into()),
            Reach::Unknown(why) => (SegMark::Unsure, format!("reachability unknown: {why}")),
        };
        segs.push(Seg {
            label: s.name.to_string(),
            selected: on == Some(i),
            mark,
            dim: s.reach == Reach::Offline,
            tip: format!("Open on {} ({}): {state}", s.name, s.target),
            act: Launch::Where(Some(i)),
        });
    }
    segs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hover_menu::Entry;

    #[test]
    fn groups_order_default_mark_and_live_hints() {
        let mut keymap = Keymap::default();
        let mut open = HashSet::new();
        open.insert(Tool::Chat);
        keymap.rebind(
            Command::OpenBoard,
            crate::keymap::Chord::new(eframe::egui::Key::Y, false, false, false),
        );
        keymap.rebind(
            Command::OpenPlan,
            crate::keymap::Chord::new(eframe::egui::Key::Y, false, false, false),
        );
        let rows = entries(&keymap, Shell::Cmd, &open);
        assert!(matches!(rows[0], Entry::Header("AGENTS")));
        assert!(matches!(rows[4], Entry::Header("SHELLS")));
        assert!(matches!(rows[8], Entry::Header("PROJECT")));
        assert!(matches!(rows[13], Entry::Divider));
        assert!(matches!(
            rows[14],
            Entry::Item {
                act: Launch::NewProject,
                ..
            }
        ));
        assert!(matches!(&rows[6], Entry::Item { hint: Some(h), .. } if h == "(default)"));
        assert!(
            matches!(&rows[9], Entry::Item { mark: true, hint: Some(h), .. } if h == "Ctrl+B G")
        );
        assert!(matches!(&rows[10], Entry::Item { hint: None, .. }));
        assert!(matches!(&rows[11], Entry::Item { hint: Some(h), .. } if h == "Ctrl+B Y"));
    }

    fn spots() -> Vec<Spot<'static>> {
        use crate::remote::RemoteShell;
        vec![
            Spot {
                name: "desk",
                target: "andy@desk",
                shell: RemoteShell::Posix,
                reach: Reach::Reachable,
            },
            Spot {
                name: "garage",
                target: "garage",
                shell: RemoteShell::Pwsh,
                reach: Reach::Offline,
            },
            Spot {
                name: "laptop",
                target: "laptop",
                shell: RemoteShell::Posix,
                reach: Reach::Checking,
            },
        ]
    }

    #[test]
    fn with_remotes_the_strip_leads_and_starts_at_local() {
        use crate::hover_menu::SegMark;
        let keymap = Keymap::default();
        let rows = entries_on(&keymap, Shell::Cmd, &HashSet::new(), &spots(), None);
        let Entry::Strip(segs) = &rows[0] else {
            panic!("the strip comes first")
        };
        let names: Vec<_> = segs.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(names, ["Local", "desk", "garage", "laptop"]);
        assert!(segs[0].selected && segs.iter().skip(1).all(|s| !s.selected));
        assert_eq!(segs[0].act, Launch::Where(None));
        assert_eq!(segs[2].act, Launch::Where(Some(1)));
        // Each state has its own glyph and says itself in words.
        let marks: Vec<_> = segs.iter().map(|s| s.mark).collect();
        assert_eq!(
            marks,
            [SegMark::None, SegMark::Up, SegMark::Down, SegMark::Pending]
        );
        // Offline is dimmed, not disabled: it still has its act.
        assert!(segs[2].dim && !segs[1].dim && !segs[3].dim);
        assert!(segs[2].tip.contains("offline"), "{}", segs[2].tip);
        assert!(segs[3].tip.contains("checking"), "{}", segs[3].tip);
        // Under Local, the rest of the menu is today's, one row down.
        let plain = entries(&keymap, Shell::Cmd, &HashSet::new());
        assert_eq!(rows.len(), plain.len() + 1);
        assert!(matches!(rows[1], Entry::Header("AGENTS")));
        assert!(matches!(
            rows[2],
            Entry::Item {
                act: Launch::Agent(SessionKind::Claude),
                ..
            }
        ));
        assert!(matches!(
            rows[6],
            Entry::Item {
                act: Launch::Shell(Shell::PowerShell),
                ..
            }
        ));
    }

    #[test]
    fn under_a_remote_agents_open_there_and_there_is_one_shell_row() {
        let rows = entries_on(
            &Keymap::default(),
            Shell::Cmd,
            &HashSet::new(),
            &spots(),
            Some(1),
        );
        let Entry::Strip(segs) = &rows[0] else {
            panic!("the strip comes first")
        };
        assert!(segs[2].selected && !segs[0].selected, "garage, offline, still selectable");
        for (i, agent) in [SessionKind::Claude, SessionKind::Codex, SessionKind::Grok]
            .into_iter()
            .enumerate()
        {
            assert!(
                matches!(rows[2 + i], Entry::Item { act, .. } if act == Launch::RemoteAgent(agent))
            );
        }
        assert!(matches!(rows[5], Entry::Header("SHELLS")));
        assert!(matches!(
            &rows[6],
            Entry::Item { label: "Shell", act: Launch::RemoteShell, hint: Some(h), .. }
                if h == "(PowerShell)"
        ));
        assert!(matches!(rows[7], Entry::Header("PROJECT")));
        // A stale index reads as Local.
        let rows = entries_on(
            &Keymap::default(),
            Shell::Cmd,
            &HashSet::new(),
            &spots(),
            Some(9),
        );
        assert!(matches!(&rows[0], Entry::Strip(s) if s[0].selected));
        assert!(matches!(
            rows[2],
            Entry::Item {
                act: Launch::Agent(_),
                ..
            }
        ));
    }
}
