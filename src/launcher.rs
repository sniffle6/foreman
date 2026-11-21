//! Shared project launcher choices for titlebar and task-manager panel.

use crate::keymap::{Command, Keymap};
use crate::landing::SessionKind;
use crate::remote_probe::Reach;
use crate::terminal::Shell;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
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
    /// Point the Project's where-strip at this PC (`None`) or at the Remote
    /// of that name. A name, not an index: the act is applied a frame after
    /// the click, against a list that may have changed. Opens nothing.
    Where(Option<String>),
    Tool(Tool),
    NewProject,
}

/// A Remote as the where-strip shows it.
pub struct Spot<'a> {
    pub name: &'a str,
    pub target: &'a str,
    pub shell: crate::remote::RemoteShell,
    pub reach: Reach,
    /// The SSH port the probe resolved, for the state line.
    pub port: Option<u16>,
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
            port: crate::remote_probe::ssh_port(&r.target),
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
        rows.push(Entry::Note(state_line(spots, on)));
    }
    // Under a Remote the headers say where a launch goes, which is also why
    // the local shells are gone.
    let header = |label: &'static str| match on {
        None => Entry::Header(label),
        Some(i) => Entry::HeaderText(format!("{label} ON {}", spots[i].name.to_uppercase())),
    };
    rows.push(header("AGENTS"));
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
    rows.push(header("SHELLS"));
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

/// The dim line under the strip: the selected Remote and its state in words,
/// so the state is not hover-only. Always present while the strip is, so
/// moving the selection never changes the menu's height. Short enough to
/// fit the narrowest menu: a reason too long for it lives in the segment's
/// tooltip.
fn state_line(spots: &[Spot], on: Option<usize>) -> String {
    let Some(s) = on.and_then(|i| spots.get(i)) else {
        return "Local: opens on this PC".into();
    };
    let name = s.name;
    match &s.reach {
        Reach::Reachable => format!("{name}: reachable"),
        Reach::Checking => format!("{name}: checking…"),
        Reach::Offline => match s.port {
            Some(port) => format!("{name}: offline (port {port})"),
            None => format!("{name}: offline"),
        },
        Reach::Unknown(_) => format!("{name}: cannot check"),
    }
}

/// `Local | desk | laptop …`. Every Remote carries a status glyph (in a slot
/// that exists before the first answer, so nothing moves when it lands) and
/// a short tooltip; the state line under the strip says the rest. Offline is
/// dimmed, not disabled: the probe cannot see through a `ProxyJump`.
fn strip(spots: &[Spot], on: Option<usize>) -> Vec<crate::hover_menu::Seg<Launch>> {
    use crate::hover_menu::{Seg, SegMark};
    let mut segs = vec![Seg {
        label: "Local".into(),
        selected: on.is_none(),
        mark: SegMark::None,
        dim: false,
        tip: "This PC".into(),
        act: Launch::Where(None),
    }];
    for (i, s) in spots.iter().enumerate() {
        let (mark, state) = match &s.reach {
            Reach::Reachable => (SegMark::Up, "reachable".into()),
            // The Remotes pane's words, so the two never disagree.
            Reach::Offline => (
                SegMark::Down,
                format!("offline. {}", crate::remotes_pane::OFFLINE_WHY),
            ),
            Reach::Checking => (SegMark::Pending, "checking…".into()),
            Reach::Unknown(why) => (SegMark::Unsure, format!("unknown — {why}")),
        };
        // The label is the name; the tip adds the target only when it says
        // something the name does not.
        let tip = if s.target.eq_ignore_ascii_case(s.name) {
            format!("{}: {state}", s.name)
        } else {
            format!("{} ({}): {state}", s.name, s.target)
        };
        segs.push(Seg {
            label: s.name.to_string(),
            selected: on == Some(i),
            mark,
            dim: s.reach == Reach::Offline,
            tip,
            act: Launch::Where(Some(s.name.to_string())),
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
                port: Some(22),
            },
            Spot {
                name: "garage",
                target: "garage",
                shell: RemoteShell::Pwsh,
                reach: Reach::Offline,
                port: Some(2222),
            },
            Spot {
                name: "laptop",
                target: "laptop",
                shell: RemoteShell::Posix,
                reach: Reach::Checking,
                port: None,
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
        assert_eq!(segs[2].act, Launch::Where(Some("garage".into())));
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
        // Short tips: the target only where it differs from the name.
        assert_eq!(segs[1].tip, "desk (andy@desk): reachable");
        assert_eq!(segs[3].tip, "laptop: checking…");
        // The state line, under the strip, names where a launch goes.
        assert!(matches!(&rows[1], Entry::Note(n) if n == "Local: opens on this PC"));
        // Under Local, the rest of the menu is today's, two rows down.
        let plain = entries(&keymap, Shell::Cmd, &HashSet::new());
        assert_eq!(rows.len(), plain.len() + 2);
        assert!(matches!(rows[2], Entry::Header("AGENTS")));
        assert!(matches!(
            rows[3],
            Entry::Item {
                act: Launch::Agent(SessionKind::Claude),
                ..
            }
        ));
        assert!(matches!(
            rows[7],
            Entry::Item {
                act: Launch::Shell(Shell::PowerShell),
                ..
            }
        ));
    }

    #[test]
    fn the_state_line_says_the_selected_remote_and_its_state() {
        let s = spots();
        assert_eq!(state_line(&s, Some(0)), "desk: reachable");
        assert_eq!(state_line(&s, Some(1)), "garage: offline (port 2222)");
        assert_eq!(state_line(&s, Some(2)), "laptop: checking…");
        let mut s = spots();
        s[1].port = None;
        assert_eq!(state_line(&s, Some(1)), "garage: offline");
    }

    /// Every state's line fits the narrowest launcher (one Remote, an SH
    /// login shell) uncut, for a name of ordinary length.
    #[test]
    fn every_state_line_fits_the_narrowest_menu() {
        use crate::remote::RemoteShell;
        let proxied = "through a proxy hop the probe cannot see past";
        for reach in [
            Reach::Reachable,
            Reach::Checking,
            Reach::Offline,
            Reach::Unknown(proxied.into()),
            Reach::Unknown("ssh: Could not resolve hostname workshop: no such host".into()),
        ] {
            for port in [Some(65535), None] {
                for on in [Some(0), None] {
                    let spot = [Spot {
                        name: "workshop",
                        target: "workshop",
                        shell: RemoteShell::Posix,
                        reach: reach.clone(),
                        port,
                    }];
                    let notes = crate::hover_menu::notes_fit(|| {
                        entries_on(&Keymap::default(), Shell::Cmd, &HashSet::new(), &spot, on)
                    });
                    assert_eq!(notes.len(), 1);
                    for (text, fits) in notes {
                        assert!(fits, "{reach:?}: {text:?} is cut");
                    }
                }
            }
        }
    }

    #[test]
    fn the_tooltip_keeps_the_reason_the_state_line_leaves_out() {
        let mut s = spots();
        let why = "through a proxy hop the probe cannot see past";
        s[2].reach = Reach::Unknown(why.into());
        let segs = strip(&s, None);
        assert_eq!(segs[3].tip, format!("laptop: unknown — {why}"));
        assert!(!state_line(&s, Some(2)).contains(why));
        // Offline says the Remotes pane's reason, word for word.
        assert!(
            segs[2].tip.ends_with(crate::remotes_pane::OFFLINE_WHY),
            "{}",
            segs[2].tip
        );
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
        assert!(
            segs[2].selected && !segs[0].selected,
            "garage, offline, still selectable"
        );
        // The headers say where a launch goes.
        assert!(matches!(&rows[2], Entry::HeaderText(h) if h == "AGENTS ON GARAGE"));
        for (i, agent) in [SessionKind::Claude, SessionKind::Codex, SessionKind::Grok]
            .into_iter()
            .enumerate()
        {
            assert!(
                matches!(&rows[3 + i], Entry::Item { act, .. } if *act == Launch::RemoteAgent(agent))
            );
        }
        assert!(matches!(&rows[6], Entry::HeaderText(h) if h == "SHELLS ON GARAGE"));
        assert!(matches!(
            &rows[7],
            Entry::Item { label: "Shell", act: Launch::RemoteShell, hint: Some(h), .. }
                if h == "(PowerShell)"
        ));
        assert!(matches!(rows[8], Entry::Header("PROJECT")));
        // A stale index reads as Local.
        let rows = entries_on(
            &Keymap::default(),
            Shell::Cmd,
            &HashSet::new(),
            &spots(),
            Some(9),
        );
        assert!(matches!(&rows[0], Entry::Strip(s) if s[0].selected));
        assert!(matches!(rows[2], Entry::Header("AGENTS")));
        assert!(matches!(
            rows[3],
            Entry::Item {
                act: Launch::Agent(_),
                ..
            }
        ));
    }
}
