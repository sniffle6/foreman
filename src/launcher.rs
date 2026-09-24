//! Shared project launcher choices for titlebar and task-manager panel.

use crate::keymap::{Command, Keymap};
use crate::landing::SessionKind;
use crate::terminal::Shell;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Launch {
    Agent(SessionKind),
    Shell(Shell),
    Tool(Tool),
    NewProject,
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

/// Pure ordered menu model. Build only after the hover menu opens.
pub fn entries(
    keymap: &Keymap,
    default_shell: Shell,
    open_tools: &HashSet<Tool>,
) -> Vec<crate::hover_menu::Entry<Launch>> {
    use crate::hover_menu::Entry;
    let mut rows = vec![Entry::Header("AGENTS")];
    for agent in [SessionKind::Claude, SessionKind::Codex, SessionKind::Grok] {
        rows.push(Entry::Item {
            label: agent.label(),
            hint: None,
            mark: false,
            act: Launch::Agent(agent),
        });
    }
    rows.push(Entry::Header("SHELLS"));
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
}
