//! Remotes: PCs foreman opens Sessions on over SSH, and the pure builder that
//! turns a Remote plus a logical argv into the local `ssh` argv. Nothing here
//! spawns or touches the network. Spec:
//! `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Most entries `Settings::remotes` keeps.
pub const MAX_REMOTES: usize = 32;

/// Longest Remote name. It is a tab label and a `--on` argument.
pub const MAX_NAME: usize = 32;

/// The name the launcher's where-strip uses for this PC.
pub const RESERVED_NAME: &str = "local";

/// Longest remote line a Windows Remote gets. cmd's limit is 8191 for the
/// whole command line, and sshd wraps ours in `cmd.exe /c`; the gap is
/// headroom for that wrapper.
pub const WINDOWS_LINE_CAP: usize = 8000;

/// The login shell sshd runs on the Remote, which decides the script's
/// language. Posix is the default so a half-filled entry still loads; Test
/// fills in the real one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RemoteShell {
    Pwsh,
    WindowsPowerShell,
    #[default]
    Posix,
}

/// A named PC foreman can open Sessions on over SSH.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Remote {
    /// Launcher label, tab tooltip, and the `--on` argument.
    pub name: String,
    /// Whatever `ssh` accepts as a destination: `user@pc` or a config alias.
    pub target: String,
    #[serde(default)]
    pub shell: RemoteShell,
    /// Seat-PC Project directory -> directory on this Remote. A blank value
    /// is a saved answer meaning the Remote's home.
    #[serde(default)]
    pub dirs: BTreeMap<String, String>,
}

/// Name rule: `config::valid_peer` characters, short, and not `local`.
/// Uniqueness is a list property; `sanitize` checks it.
pub fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && crate::config::valid_peer(name)
        && !name.eq_ignore_ascii_case(RESERVED_NAME)
}

/// Target rule. A leading `-` would be read by `ssh` as an option, so it is
/// refused here even though the builder also puts `--` before the target.
pub fn valid_target(target: &str) -> bool {
    !target.is_empty()
        && !target.starts_with('-')
        && !target.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Trim, validate, de-duplicate (case-insensitive) and cap a loaded list.
/// The first entry with a name wins.
pub fn sanitize(remotes: Vec<Remote>) -> Vec<Remote> {
    let mut seen = std::collections::HashSet::new();
    remotes
        .into_iter()
        .map(|mut r| {
            r.name = r.name.trim().to_string();
            r.target = r.target.trim().to_string();
            for dir in r.dirs.values_mut() {
                *dir = dir.trim().to_string();
            }
            r.dirs.retain(|project, _| !project.trim().is_empty());
            r
        })
        .filter(|r| valid_name(&r.name) && valid_target(&r.target))
        .filter(|r| seen.insert(r.name.to_ascii_lowercase()))
        .take(MAX_REMOTES)
        .collect()
}

/// `Settings::remotes` deserializer: an entry that does not parse (a typo'd
/// shell, a missing target) is dropped alone instead of failing the whole
/// settings file.
pub fn deserialize_lenient<'de, D>(d: D) -> Result<Vec<Remote>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// The Remote `--on <name>` names. Case-insensitive, like `sanitize`'s
/// uniqueness rule.
pub fn find<'a>(remotes: &'a [Remote], name: &str) -> Result<&'a Remote, String> {
    if let Some(r) = remotes.iter().find(|r| r.name.eq_ignore_ascii_case(name)) {
        return Ok(r);
    }
    if remotes.is_empty() {
        return Err(format!(
            "no Remote named {name:?}: none are set up (add one in Settings > Remotes)"
        ));
    }
    let known: Vec<&str> = remotes.iter().map(|r| r.name.as_str()).collect();
    Err(format!(
        "no Remote named {name:?} (known: {})",
        known.join(", ")
    ))
}

/// Whether two ssh targets name the same PC: the host part (an ssh-config
/// Host or a host name, both matched without case by ssh) compared without
/// case, the user before an `@` exactly.
pub fn same_target(a: &str, b: &str) -> bool {
    let split = |t: &str| match t.rsplit_once('@') {
        Some((user, host)) => (Some(user.to_string()), host.to_ascii_lowercase()),
        None => (None, t.to_ascii_lowercase()),
    };
    split(a) == split(b)
}

/// The globe badge's tooltip: the Session's copy of the Remote name, plus
/// the target while the Remote is still set up (a deleted one leaves only
/// the name the Session carries). `disconnected` is the down dot's state,
/// said in words so the dot is not colour-only.
pub fn badge_tooltip(remotes: &[Remote], name: &str, disconnected: bool) -> String {
    let base = match find(remotes, name) {
        Ok(r) => format!("Remote: {name} ({})", r.target),
        Err(_) => format!("Remote: {name}"),
    };
    if disconnected {
        format!("{base} · disconnected")
    } else {
        base
    }
}

/// Provider program → the arguments that reopen its latest conversation in
/// the launch directory. The one table reconnect reads; a provider missing
/// here relaunches fresh.
const RESUME: &[(&str, &[&str])] = &[
    ("claude", &["--continue"]),
    ("codex", &["resume", "--last"]),
];

/// A tool an install command needs on the Remote before it can work. Test
/// reports each one; a missing one blocks the install action with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prereq {
    Curl,
    Bash,
}

impl Prereq {
    /// The program name, as Test looks it up and the UI names it.
    pub fn name(self) -> &'static str {
        match self {
            Prereq::Curl => "curl",
            Prereq::Bash => "bash",
        }
    }
}

/// Which install command a Remote's shell takes. Pwsh and Windows
/// PowerShell run the same vendor command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallShell {
    Posix,
    PowerShell,
}

impl From<RemoteShell> for InstallShell {
    fn from(s: RemoteShell) -> Self {
        match s {
            RemoteShell::Posix => InstallShell::Posix,
            RemoteShell::Pwsh | RemoteShell::WindowsPowerShell => InstallShell::PowerShell,
        }
    }
}

/// One vendor install command for an agent CLI.
#[derive(Debug, PartialEq, Eq)]
pub struct Install {
    pub provider: &'static str,
    pub shell: InstallShell,
    /// The vendor's own command, verbatim from its install page.
    pub command: &'static str,
    pub needs: &'static [Prereq],
}

/// (provider, shell kind) → the vendor's official install command. Each
/// command is copied from the vendor page in the comment beside it (looked
/// up 2026-11-22); re-check the page before changing one. A provider or
/// shell missing here has no install action: the UI says to install it by
/// hand. Foreman installs nothing of its own: this is only the vendor's
/// installer, run visibly in a Remote Session. Every PowerShell entry is
/// untested (no Windows Remote has existed).
const INSTALL: &[Install] = &[
    // https://code.claude.com/docs/en/setup (native install, macOS/Linux/WSL)
    Install {
        provider: "claude",
        shell: InstallShell::Posix,
        command: "curl -fsSL https://claude.ai/install.sh | bash",
        needs: &[Prereq::Curl, Prereq::Bash],
    },
    // https://code.claude.com/docs/en/setup (native install, Windows PowerShell)
    Install {
        provider: "claude",
        shell: InstallShell::PowerShell,
        command: "irm https://claude.ai/install.ps1 | iex",
        needs: &[],
    },
    // https://github.com/openai/codex (README, standalone installer, macOS/Linux)
    Install {
        provider: "codex",
        shell: InstallShell::Posix,
        command: "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
        needs: &[Prereq::Curl],
    },
    // https://github.com/openai/codex (README, standalone installer, Windows)
    Install {
        provider: "codex",
        shell: InstallShell::PowerShell,
        command: r#"powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex""#,
        needs: &[],
    },
    // https://docs.x.ai/build/overview (Grok Build, macOS/Linux)
    Install {
        provider: "grok",
        shell: InstallShell::Posix,
        command: "curl -fsSL https://x.ai/cli/install.sh | bash",
        needs: &[Prereq::Curl, Prereq::Bash],
    },
    // https://docs.x.ai/build/overview (Grok Build, Windows PowerShell)
    Install {
        provider: "grok",
        shell: InstallShell::PowerShell,
        command: "irm https://x.ai/cli/install.ps1 | iex",
        needs: &[],
    },
];

/// The install command for `provider` on a Remote whose shell is `shell`.
pub fn install_for(provider: &str, shell: RemoteShell) -> Option<&'static Install> {
    let family = InstallShell::from(shell);
    INSTALL
        .iter()
        .find(|i| i.provider == provider && i.shell == family)
}

/// The logical argv an install Session runs: the vendor command handed to
/// the Remote's own shell, since every command is a pipeline. It goes
/// through [`build`] like any other launch, in the Remote's home.
pub fn install_argv(shell: RemoteShell, command: &str) -> Vec<String> {
    let argv: &[&str] = match shell {
        RemoteShell::Posix => &["sh", "-c"],
        RemoteShell::Pwsh => &["pwsh", "-NoLogo", "-NoProfile", "-Command"],
        RemoteShell::WindowsPowerShell => &["powershell", "-NoLogo", "-NoProfile", "-Command"],
    };
    argv.iter()
        .map(|a| a.to_string())
        .chain(std::iter::once(command.to_string()))
        .collect()
}

/// The provider an install Session's argv installs, when `argv` is exactly
/// one [`install_argv`] from the table. Reconnect reads it so Enter on an
/// ended install opens a shell there and never runs the installer again.
pub fn install_of(argv: &[String]) -> Option<&'static str> {
    const SHELLS: [RemoteShell; 3] = [
        RemoteShell::Posix,
        RemoteShell::Pwsh,
        RemoteShell::WindowsPowerShell,
    ];
    INSTALL.iter().find_map(|i| {
        SHELLS
            .iter()
            .filter(|s| InstallShell::from(**s) == i.shell)
            .any(|s| install_argv(*s, i.command) == argv)
            .then_some(i.provider)
    })
}

/// Whether a Remote Session launched with `argv` gets a Tunnel (the reverse
/// forward plus `FOREMAN_URL`/`FOREMAN_TOKEN`). An install Session does not:
/// nothing in a vendor's curl-pipe script posts to chat, the token would be
/// handed to third-party code, and `ExitOnForwardFailure` could fail the
/// install over a port clash.
pub fn carries_tunnel(argv: &[String]) -> bool {
    install_of(argv).is_none()
}

/// The agent provider (`claude` / `codex` / `grok`, from `board::AGENTS`)
/// an argv runs, by the same scan the tab icon uses
/// ([`crate::icons::IconKind::from_argv`]). The one "is an agent" predicate
/// for the icon, the at-launch reply note and reconnect: they must agree, or
/// an argv that shows the Claude icon could replay its prompt on reconnect.
pub fn agent_of(argv: &[String]) -> Option<&'static str> {
    crate::icons::IconKind::from_argv(argv).and_then(crate::icons::IconKind::provider)
}

/// How a launch of `argv` on a `shell` Remote teaches its agent the reply
/// command. Reconnect argvs come here too: [`relaunch`] leaves no prompt, so
/// they are never `InPrompt`.
///
/// `InPrompt` only where the note can name `$FOREMAN_URL` and
/// `$FOREMAN_TOKEN` rather than the token itself (which would sit in the
/// agent's world-readable argv for its life), the agent's tool shell was
/// seen to have both (2026-11-23, Debian: Claude 2.1.331's Bash tool,
/// codex-cli 0.163.0's shell), and [`prompt_index`] names the prompt.
/// Grok (never run on a Remote), every Windows Remote (none tried; a tool
/// shell there may be PowerShell or Git Bash) and any other agent with
/// arguments get it typed `AtLaunch`. A bare agent, a shell or a non-agent
/// command waits for the first post: a shell would run the note.
pub fn note_route(argv: &[String], shell: RemoteShell) -> crate::control::tunnel::NoteGoes {
    use crate::control::tunnel::NoteGoes;
    if argv.len() < 2 || agent_of(argv).is_none() {
        return NoteGoes::WithFirstPost;
    }
    let sees_env = shell == RemoteShell::Posix && matches!(argv[0].as_str(), "claude" | "codex");
    if sees_env && prompt_index(argv).is_some() {
        NoteGoes::InPrompt
    } else {
        NoteGoes::AtLaunch
    }
}

/// Which argument of `argv` is its Dispatch prompt, when that is certain;
/// `None` for anything else, which then gets the reply note typed instead of
/// appended to an argument that may not be the prompt.
///
/// Accepted: exactly the shapes foreman dispatches and its skills document,
/// the bare program then the brief (`card_argv`; "prompt immediately after
/// the program"): `<agent> <brief>`, and for Claude and Codex
/// `<agent> <brief> --model <id>`. The brief must hold whitespace and not
/// start with `-`. No subcommand of any of the three CLIs holds whitespace,
/// so this tells a brief from `exec` / `resume` / `mcp` without a list that
/// drifts with CLI versions; a one-word brief falls back.
///
/// Refused: a program path or wrapper (`/usr/bin/claude`, `npx …`,
/// `my-claude`), a flag first (`-p`, `--continue`, `--model x brief`), any
/// subcommand, and any other trailing argument, which could change what the
/// first one means.
pub fn prompt_index(argv: &[String]) -> Option<usize> {
    let (program, rest) = argv.split_first()?;
    let program = program.as_str();
    if !crate::board::AGENTS.contains(&program) {
        return None;
    }
    let (brief, tail) = rest.split_first()?;
    let is_brief = !brief.starts_with('-') && brief.chars().any(char::is_whitespace);
    let tail_ok = match tail {
        [] => true,
        [flag, id] => {
            matches!(program, "claude" | "codex")
                && flag == "--model"
                && !id.is_empty()
                && !id.starts_with('-')
                && !id.chars().any(char::is_whitespace)
        }
        _ => false,
    };
    (is_brief && tail_ok).then_some(1)
}

/// What a reconnect relaunches. See [`relaunch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relaunch {
    pub argv: Vec<String>,
    /// The agent provider, when the original argv was an agent.
    pub agent: Option<&'static str>,
    /// The argv reopens the agent's latest conversation (a resume entry).
    pub resumes: bool,
    /// The original argv was an install of this provider; the relaunch is a
    /// shell, so the installer only ever runs from its named action.
    pub install: Option<&'static str>,
}

/// What a reconnect runs for a Session launched with `original`. An agent
/// whose program IS the provider (`claude`, `C:\bin\claude.exe`) gets the
/// provider's resume arguments from [`RESUME`], or none at all. Anything else
/// that looks like an agent (`npx @anthropic-ai/claude-code "do X"`,
/// `wsl claude …`, a wrapper script) fails safe: the bare provider, fresh,
/// with no argument from the original argv, since any of them may be the
/// Dispatch prompt. A shell stays a shell, and any other command runs again
/// as it was. An install Session (see [`install_of`]) comes back as a shell,
/// checked first because the claude and codex install URLs name the agent.
pub fn relaunch(original: &[String]) -> Relaunch {
    if let Some(provider) = install_of(original) {
        return Relaunch {
            argv: Vec::new(),
            agent: None,
            resumes: false,
            install: Some(provider),
        };
    }
    let Some(agent) = agent_of(original) else {
        return Relaunch {
            argv: original.to_vec(),
            agent: None,
            resumes: false,
            install: None,
        };
    };
    let program = &original[0];
    let stem = std::path::Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase();
    if stem != agent {
        return Relaunch {
            argv: vec![agent.to_string()],
            agent: Some(agent),
            resumes: false,
            install: None,
        };
    }
    let resume = RESUME
        .iter()
        .find(|(p, _)| *p == agent)
        .map_or(&[][..], |(_, args)| *args);
    Relaunch {
        argv: std::iter::once(program.clone())
            .chain(resume.iter().map(|a| a.to_string()))
            .collect(),
        agent: Some(agent),
        resumes: !resume.is_empty(),
        install: None,
    }
}

/// [`relaunch`]'s argv.
pub fn reconnect_argv(original: &[String]) -> Vec<String> {
    relaunch(original).argv
}

/// Windows path equality for `Remote::dirs` keys: case, slash direction, a
/// trailing separator and a `\\?\` verbatim prefix (what `canonicalize`
/// returns) do not make a different Project directory.
pub fn same_dir(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = plain_dir(&s.replace('/', "\\"));
        s.trim_end_matches('\\').to_string()
    };
    norm(a).eq_ignore_ascii_case(&norm(b))
}

/// `dir` without a `\\?\` verbatim prefix (`\\?\UNC\x` becomes `\\x`): the
/// spelling a `Remote::dirs` key is written in, so settings.json and the
/// Remotes pane show the path the user knows. Old verbatim keys still match
/// through [`same_dir`].
pub fn plain_dir(dir: &str) -> String {
    match dir.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => dir.strip_prefix(r"\\?\").unwrap_or(dir).to_string(),
    }
}

/// The directory on `remote` a launch from a Project at `project_dir` runs
/// in. An explicit `cwd` wins and is not saved; otherwise the Project's
/// mapping (blank = the Remote's home). Neither is an error naming both fixes,
/// because guessing would start an agent in the wrong place.
pub fn launch_dir<'a>(
    remote: &'a Remote,
    cwd: Option<&'a str>,
    project_dir: Option<&str>,
) -> Result<&'a str, String> {
    if let Some(dir) = cwd {
        return Ok(dir);
    }
    let name = &remote.name;
    let Some(project_dir) = project_dir else {
        return Err(format!(
            "this Project has no directory to map to one on {name}; pass --cwd <directory on {name}>"
        ));
    };
    remote
        .dirs
        .iter()
        .find(|(seat, _)| same_dir(seat, project_dir))
        .map(|(_, there)| there.as_str())
        .ok_or_else(|| {
            format!(
                "{name} has no directory for this Project ({project_dir}); pass --cwd <directory on {name}>, or open a Session on {name} from this Project's launcher once and answer its directory prompt"
            )
        })
}

/// The per-Session reverse forward back to the seat PC's listener.
#[derive(Clone, Copy, Debug)]
pub struct Tunnel<'a> {
    /// Port the Remote's agent dials on its own loopback.
    pub remote_port: u16,
    /// The seat PC's loopback listener port.
    pub local_port: u16,
    /// Bearer token naming this Session to the listener.
    pub token: &'a str,
}

/// Everything one Remote Session launch needs.
pub struct Launch<'a> {
    pub remote: &'a Remote,
    /// Directory on the Remote. Blank = the Remote's home.
    pub dir: &'a str,
    /// `t{id}`, as `FOREMAN_TERMINAL_ID` is locally.
    pub terminal_id: &'a str,
    /// `p{id}`; `None` outside a Project, as locally.
    pub project_id: Option<&'a str>,
    /// `None` = no way back: no forward, no URL/token.
    pub tunnel: Option<Tunnel<'a>>,
    /// What the user asked to run (`claude …`). Empty = an interactive shell.
    pub argv: &'a [String],
}

#[derive(Debug, PartialEq)]
pub struct Built {
    /// The local process: `ssh … -- <target> <remote line>`.
    pub ssh: Vec<String>,
    /// The logical argv, unchanged, for `IconKind::from_argv`. The wrapped
    /// line never contains the agent's name in readable form. It never holds
    /// the carried note either: reconnect and the snapshot read it, and the
    /// note's token dies with this connection.
    pub dispatched: Vec<String>,
    /// How this connection's reply note reaches the agent ([`note_route`]).
    /// `InPrompt` only when the note was appended to the prompt; a launch
    /// that would not take it is `AtLaunch`, typed by the caller.
    pub note: crate::control::tunnel::NoteGoes,
    /// Claude's hooks ride this launch, so the caller holds all chat input
    /// until the hook event [`ChatHold`] names. `None`: no hooks, no hold.
    pub hold: Option<ChatHold>,
}

/// Which hook event opens a hooked Remote Claude's chat hold. Claude runs no
/// hook while its folder-trust dialog waits, and any Enter typed there quits
/// it, so no input goes before an event. `SessionStart` is the first one
/// past the dialog, but its REPL is still mounting then: a post pasted at it
/// was lost once in three tries (2026-11-23, Claude Code 2.1.331; the other
/// two were echoed by the tty and then read). A launch whose brief rides in
/// argv also fires `UserPromptSubmit` for it, and only a mounted REPL does,
/// so it waits for that. A bare launch or `--continue` has nothing later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatHold {
    /// Any event opens it, `SessionStart` included.
    UntilStart,
    /// Any event but `SessionStart` opens it.
    UntilPrompt,
}

impl ChatHold {
    /// Whether `event` opens this hold.
    pub fn opened_by(self, event: crate::agent_state::HookEvent) -> bool {
        self == Self::UntilStart || event != crate::agent_state::HookEvent::SessionStart
    }
}

/// [`Built::hold`] for `argv`: only Claude's launch hooks report anything on
/// a Remote ([`claude_with_hooks`]).
fn chat_hold(argv: &[String], shell: RemoteShell) -> Option<ChatHold> {
    claude_with_hooks(argv, shell)?;
    Some(if prompt_index(argv).is_some() {
        ChatHold::UntilPrompt
    } else {
        ChatHold::UntilStart
    })
}

/// Build the local `ssh` argv for a launch. Refuses (never truncates) a
/// Windows launch whose remote line would not fit cmd.
pub fn build(l: &Launch) -> Result<Built, String> {
    use crate::control::tunnel::NoteGoes;
    let dispatched = l.argv.to_vec();
    let hold = l.tunnel.and_then(|_| chat_hold(l.argv, l.remote.shell));
    let note = note_route(l.argv, l.remote.shell);
    if note == NoteGoes::InPrompt
        && l.tunnel.is_some()
        && let Some(i) = prompt_index(l.argv)
    {
        let mut carried = dispatched.clone();
        carried[i] = format!(
            "{}\n\n{}",
            carried[i],
            crate::control::tunnel::prompt_note()
        );
        if let Ok(ssh) = ssh_argv(l, &carried) {
            return Ok(Built {
                ssh,
                dispatched,
                note,
                hold,
            });
        }
    }
    Ok(Built {
        ssh: ssh_argv(l, l.argv)?,
        dispatched,
        note: match note {
            NoteGoes::InPrompt => NoteGoes::AtLaunch,
            other => other,
        },
        hold,
    })
}

/// [`build`]'s `ssh` argv running `argv` for launch `l`.
fn ssh_argv(l: &Launch, argv: &[String]) -> Result<Vec<String>, String> {
    // Same rule as a local spawn, applied before the argv is buried in base64.
    let run = crate::agent_command::codex_in_process(argv).unwrap_or_else(|| argv.to_vec());
    // Nothing is installed on a Remote, so Claude's state hooks ride the
    // launch. Without a Tunnel there is nowhere for them to go.
    let run = match l.tunnel {
        Some(_) => claude_with_hooks(&run, l.remote.shell).unwrap_or(run),
        None => run,
    };

    let mut env: Vec<(&str, String)> = vec![
        ("FOREMAN", "1".into()),
        ("FOREMAN_TERMINAL_ID", l.terminal_id.into()),
    ];
    if let Some(p) = l.project_id {
        env.push(("FOREMAN_PROJECT_ID", p.into()));
    }
    env.push(("COLORTERM", "truecolor".into()));
    if let Some(t) = l.tunnel {
        env.push(("FOREMAN_URL", format!("http://127.0.0.1:{}", t.remote_port)));
        env.push(("FOREMAN_TOKEN", t.token.into()));
    }

    let line = match l.remote.shell {
        RemoteShell::Posix => posix_line(&posix_script(l.dir, &env, &run)),
        shell @ (RemoteShell::Pwsh | RemoteShell::WindowsPowerShell) => {
            let exe = if shell == RemoteShell::Pwsh {
                "pwsh"
            } else {
                "powershell"
            };
            if shell == RemoteShell::WindowsPowerShell
                && let Some(bad) = run.iter().skip(1).find(|a| !legacy_passable(a))
            {
                let preview: String = bad.chars().take(40).collect();
                return Err(format!(
                    "Windows PowerShell 5.1 on {} cannot pass the argument {preview:?}… to a program intact (a quote comes before every space); set the Remote's shell to pwsh, or open without it and send it by chat",
                    l.remote.name
                ));
            }
            let no_exit = if run.is_empty() { " -NoExit" } else { "" };
            let line = format!(
                "{exe} -NoLogo{no_exit} -EncodedCommand {}",
                utf16le_b64(&ps_script(
                    l.dir,
                    &env,
                    &run,
                    shell == RemoteShell::WindowsPowerShell
                ))
            );
            if line.len() > WINDOWS_LINE_CAP {
                return Err(format!(
                    "this launch is {} characters on {}'s command line, over the {WINDOWS_LINE_CAP} a Windows Remote allows; open the Session without the prompt and send the brief by chat",
                    line.len(),
                    l.remote.name
                ));
            }
            line
        }
    };

    let mut ssh: Vec<String> = ["ssh", "-t"].map(String::from).to_vec();
    if l.tunnel.is_some() {
        ssh.extend(["-o", "ExitOnForwardFailure=yes"].map(String::from));
    }
    ssh.extend(
        [
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]
        .map(String::from),
    );
    if let Some(t) = l.tunnel {
        ssh.push("-R".into());
        ssh.push(format!("{}:127.0.0.1:{}", t.remote_port, t.local_port));
    }
    ssh.push("--".into());
    ssh.push(l.remote.target.clone());
    ssh.push(line);
    Ok(ssh)
}

/// The event a Remote Claude fires first once it is past its folder-trust
/// dialog: launched with a prompt, bare, and on `--continue` (seen
/// 2026-11-23, Claude Code 2.1.331: nothing in 40 s at the dialog, then this
/// within 0.3 s of the answer). It only opens the chat hold
/// ([`ChatHold`]) and maps to no state.
const REMOTE_GATE_EVENT: &str = "SessionStart";

/// Events whose hook sends its stdin payload, so the `/hook` route can drop a
/// subagent's the way the local helper does. The others count for a subagent
/// anyway, and their payload carries tool input or output of any size.
const PAYLOAD_EVENTS: &[&str] = &["UserPromptSubmit", "Stop", "StopFailure"];

/// `argv` with Claude's state hooks as `--settings` JSON after the program:
/// one `curl` per event to the Tunnel's `/hook`, the token read from the
/// launch environment (never written into argv, which every user on the
/// Remote can read). `None` when the program is not Claude itself (a wrapper
/// gets no hooks, as with [`crate::agent_command::codex_in_process`]), the
/// user passed their own `--settings`, or the Remote is Windows: unproved
/// there, and the encoded hooks would take ~3 KB of the brief's room under
/// [`WINDOWS_LINE_CAP`].
fn claude_with_hooks(argv: &[String], shell: RemoteShell) -> Option<Vec<String>> {
    let stem = std::path::Path::new(argv.first()?).file_stem()?.to_str()?;
    if shell != RemoteShell::Posix
        || !stem.eq_ignore_ascii_case("claude")
        || argv[1..]
            .iter()
            .any(|a| a == "--settings" || a.starts_with("--settings="))
    {
        return None;
    }
    let mut hooks = serde_json::Map::new();
    for &event in crate::agent_hooks::state_events("claude")
        .iter()
        .chain(&[REMOTE_GATE_EVENT])
    {
        hooks.insert(
            event.into(),
            serde_json::json!([{ "hooks": [posix_hook(event)] }]),
        );
    }
    let settings = serde_json::json!({ "hooks": hooks }).to_string();
    let mut out = argv.to_vec();
    out.splice(1..1, ["--settings".to_string(), settings]);
    Some(out)
}

/// One hook handler: a quiet `curl` whose output and failure never reach the
/// agent (a `UserPromptSubmit` hook's stdout lands in its context). Async, as
/// the local handlers are, so a slow Tunnel never delays a turn.
fn posix_hook(event: &str) -> serde_json::Value {
    let body = if PAYLOAD_EVENTS.contains(&event) {
        "--data-binary @-"
    } else {
        "-d ''"
    };
    serde_json::json!({
        "type": "command",
        "command": format!(
            "curl -s -m 3 -o /dev/null -H \"Authorization: Bearer $FOREMAN_TOKEN\" {body} \"$FOREMAN_URL/hook?agent=claude&event={event}\" 2>/dev/null || true"
        ),
        "timeout": 5,
        "async": true,
    })
}

/// POSIX single-quote: `'` becomes `'\''`.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// PowerShell single-quote: `'` doubled. PowerShell also treats the curly
/// quotes U+2018..U+201B as single quotes, so they are doubled too.
pub(crate) fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// `~` and `~/rest` mean the Remote's home; anything else is literal.
pub(crate) fn home_relative(dir: &str) -> Option<&str> {
    if dir == "~" {
        return Some("");
    }
    dir.strip_prefix("~/").or_else(|| dir.strip_prefix("~\\"))
}

fn posix_script(dir: &str, env: &[(&str, String)], argv: &[String]) -> String {
    let mut s = String::new();
    if !dir.is_empty() {
        let target = match home_relative(dir) {
            Some("") => "\"$HOME\"".to_string(),
            Some(rest) => format!("\"$HOME\"/{}", sh_quote(rest)),
            None => sh_quote(dir),
        };
        s.push_str(&format!("cd -- {target} || exit 1\n"));
    }
    for (k, v) in env {
        s.push_str(&format!("export {k}={}\n", sh_quote(v)));
    }
    // The user's own login shell, so PATH from their startup files applies;
    // the argv rides as positional parameters so it is quoted exactly once.
    if argv.is_empty() {
        s.push_str("exec \"${SHELL:-sh}\" -li\n");
    } else {
        s.push_str("exec \"${SHELL:-sh}\" -lic 'exec \"$@\"' foreman-launch");
        for a in argv {
            s.push(' ');
            s.push_str(&sh_quote(a));
        }
        s.push('\n');
    }
    s
}

pub(crate) fn posix_line(script: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    format!("sh -c \"$(printf %s {b64} | base64 -d)\"")
}

fn ps_script(dir: &str, env: &[(&str, String)], argv: &[String], legacy: bool) -> String {
    let mut s = String::new();
    if !dir.is_empty() {
        let target = match home_relative(dir) {
            Some("") => "$HOME".to_string(),
            Some(rest) => format!("(Join-Path $HOME {})", ps_quote(rest)),
            None => ps_quote(dir),
        };
        s.push_str(&format!(
            "try {{ Set-Location -LiteralPath {target} -ErrorAction Stop }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}\n"
        ));
    }
    for (k, v) in env {
        s.push_str(&format!("$env:{k} = {}\n", ps_quote(v)));
    }
    if !argv.is_empty() {
        s.push('&');
        for (i, a) in argv.iter().enumerate() {
            s.push(' ');
            if legacy && i > 0 {
                s.push_str(&ps_quote(&legacy_native(a)));
            } else {
                s.push_str(&ps_quote(a));
            }
        }
        s.push_str("\nexit $LASTEXITCODE\n");
    }
    s
}

/// Windows PowerShell 5.1 hands a native program its arguments without
/// escaping embedded `"` (it only wraps an argument holding whitespace in
/// quotes), so `say "hi"` arrives as `say hi`. Pre-escape for the receiving
/// program's command-line parser: `"` becomes `\"`, backslashes before a `"`
/// double, and trailing backslashes double when 5.1 will add the closing
/// quote. pwsh 7.3+ escapes correctly itself and must not get this.
/// Whether 5.1 can pass `a` at all. It wraps an argument in quotes only when
/// it sees whitespace outside quotes, and it counts escaped `\"` as quotes
/// too. If every space follows an odd number of `"`, the escaped form is
/// passed bare and splits, and no escaping fixes that.
fn legacy_passable(a: &str) -> bool {
    if !a.chars().any(char::is_whitespace) {
        return true;
    }
    let mut quotes = 0;
    legacy_native(a).chars().any(|c| {
        if c == '"' {
            quotes += 1;
        }
        c.is_whitespace() && quotes % 2 == 0
    })
}

fn legacy_native(a: &str) -> String {
    const BS: char = '\\';
    if a.is_empty() {
        return "\"\"".into(); // 5.1 drops an empty argument outright
    }
    let mut out = String::with_capacity(a.len());
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            BS => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n(BS, slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n(BS, slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    let wrapped = a.chars().any(char::is_whitespace);
    out.extend(std::iter::repeat_n(
        BS,
        if wrapped { slashes * 2 } else { slashes },
    ));
    out
}

pub(crate) fn utf16le_b64(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_badge_tooltip_names_the_remote_and_its_target_while_it_exists() {
        let desk = super::Remote {
            name: "desk".into(),
            target: "me@desk.lan".into(),
            shell: super::RemoteShell::default(),
            dirs: Default::default(),
        };
        assert_eq!(
            super::badge_tooltip(std::slice::from_ref(&desk), "desk", false),
            "Remote: desk (me@desk.lan)"
        );
        assert_eq!(
            super::badge_tooltip(std::slice::from_ref(&desk), "desk", true),
            "Remote: desk (me@desk.lan) · disconnected"
        );
        assert_eq!(
            super::badge_tooltip(&[desk], "garage", false),
            "Remote: garage"
        );
    }

    #[test]
    fn a_target_retyped_in_another_case_is_the_same_pc() {
        assert!(super::same_target("andy@desk", "andy@DESK"));
        assert!(super::same_target("Desk", "desk"));
        assert!(super::same_target("andy@Desk.LAN", "andy@desk.lan"));
        // The user is not a host name.
        assert!(!super::same_target("Andy@desk", "andy@desk"));
        assert!(!super::same_target("andy@desk", "andy@old-desk"));
        assert!(!super::same_target("desk", "andy@desk"));
    }

    use super::*;

    #[test]
    fn reconnect_resumes_agents_without_their_prompt() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for (original, want) in [
            (
                v(&["claude", "--model", "opus", "do the card"]),
                v(&["claude", "--continue"]),
            ),
            (
                v(&["codex", "do the card"]),
                v(&["codex", "resume", "--last"]),
            ),
            (
                v(&[r"C:\bin\Claude.exe", "brief"]),
                v(&[r"C:\bin\Claude.exe", "--continue"]),
            ),
            // A prompt naming another provider does not change the agent.
            (
                v(&["codex", "exec", "compare with claude"]),
                v(&["codex", "resume", "--last"]),
            ),
            // No table entry: fresh, and still no prompt.
            (v(&["grok", "do the card"]), v(&["grok"])),
            // A shell stays a shell; anything else runs again unchanged.
            (v(&[]), v(&[])),
            (v(&["htop", "-d", "5"]), v(&["htop", "-d", "5"])),
        ] {
            assert_eq!(reconnect_argv(&original), want, "{original:?}");
        }
    }

    /// Anything the tab icon calls an agent, but whose program is not the
    /// provider itself, relaunches the bare provider: no word of the
    /// original argv survives, so the prompt cannot ride along.
    #[test]
    fn a_wrapped_agent_relaunches_bare_and_never_carries_the_prompt() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let prompt = "do X";
        for (original, want) in [
            (v(&["npx", "@anthropic-ai/claude-code", prompt]), "claude"),
            (v(&["wsl", "claude", prompt]), "claude"),
            (v(&["wsl", "--", "codex", prompt]), "codex"),
            (v(&["my-claude-wrapper.cmd", prompt]), "claude"),
            (v(&["python", "claude_tool.py", prompt]), "claude"),
            (v(&["bash", "-lc", &format!("grok {prompt}")]), "grok"),
            // The prompt names another provider: the launched agent wins.
            (v(&["wsl", "codex", "compare with claude"]), "codex"),
        ] {
            let r = relaunch(&original);
            assert_eq!(r.argv, [want], "{original:?}");
            assert_eq!(r.agent, Some(want));
            assert!(!r.resumes, "no clean resume entry: fresh");
            assert!(
                r.argv.iter().all(|a| !a.contains(prompt)),
                "{original:?} -> {:?}",
                r.argv
            );
        }
    }

    /// One predicate: whatever the icon calls an agent, reconnect treats as
    /// one, and the providers are the board's list.
    #[test]
    fn the_agent_predicate_is_the_icon_s() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for argv in [
            v(&["claude"]),
            v(&["npx", "@anthropic-ai/claude-code", "x"]),
            v(&["codex", "exec", "y"]),
            v(&["grok"]),
            v(&["python", "claude_tool.py"]),
            v(&["htop"]),
            v(&[]),
        ] {
            let icon = crate::icons::IconKind::from_argv(&argv);
            assert_eq!(
                agent_of(&argv),
                icon.and_then(crate::icons::IconKind::provider)
            );
            assert_eq!(relaunch(&argv).agent.is_some(), icon.is_some(), "{argv:?}");
            if let Some(a) = agent_of(&argv) {
                assert!(crate::board::AGENTS.contains(&a));
            }
        }
    }

    fn remote(shell: RemoteShell) -> Remote {
        Remote {
            name: "desk".into(),
            target: "andy@desk".into(),
            shell,
            dirs: BTreeMap::new(),
        }
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn launch<'a>(r: &'a Remote, dir: &'a str, argv: &'a [String]) -> Launch<'a> {
        Launch {
            remote: r,
            dir,
            terminal_id: "t7",
            project_id: Some("p3"),
            tunnel: Some(Tunnel {
                remote_port: 41234,
                local_port: 50001,
                token: "tok",
            }),
            argv,
        }
    }

    /// The script a built line carries, decoded.
    fn script(b: &Built, shell: RemoteShell) -> String {
        let line = b.ssh.last().unwrap();
        let engine = base64::engine::general_purpose::STANDARD;
        match shell {
            RemoteShell::Posix => {
                let b64 = line
                    .strip_prefix("sh -c \"$(printf %s ")
                    .and_then(|r| r.strip_suffix(" | base64 -d)\""))
                    .expect("posix wrapper");
                String::from_utf8(engine.decode(b64).unwrap()).unwrap()
            }
            _ => {
                let b64 = line.rsplit(' ').next().unwrap();
                let bytes = engine.decode(b64).unwrap();
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16(&units).unwrap()
            }
        }
    }

    #[test]
    fn remote_posix_launch_matches_the_hand_proved_form() {
        let r = remote(RemoteShell::Posix);
        let a = argv(&["claude", "--model", "x"]);
        let b = build(&launch(&r, "/home/andy/src", &a)).unwrap();
        assert_eq!(
            b.ssh[..b.ssh.len() - 1],
            argv(&[
                "ssh",
                "-t",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-R",
                "41234:127.0.0.1:50001",
                "--",
                "andy@desk",
            ])[..]
        );
        let line = b.ssh.last().unwrap();
        // The line sshd's login shell parses: letters, digits, +/= and a fixed wrapper.
        let b64 = &line["sh -c \"$(printf %s ".len()..line.len() - " | base64 -d)\"".len()];
        assert!(
            b64.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
        );
        let settings = &claude_with_hooks(&a, RemoteShell::Posix).unwrap()[2];
        assert_eq!(
            script(&b, RemoteShell::Posix),
            format!(
                "cd -- '/home/andy/src' || exit 1\n\
                 export FOREMAN='1'\n\
                 export FOREMAN_TERMINAL_ID='t7'\n\
                 export FOREMAN_PROJECT_ID='p3'\n\
                 export COLORTERM='truecolor'\n\
                 export FOREMAN_URL='http://127.0.0.1:41234'\n\
                 export FOREMAN_TOKEN='tok'\n\
                 exec \"${{SHELL:-sh}}\" -lic 'exec \"$@\"' foreman-launch 'claude' '--settings' {} '--model' 'x'\n",
                sh_quote(settings)
            )
        );
    }

    #[test]
    fn remote_scripts_quote_hostile_text_per_shell() {
        // (input, Posix literal, pwsh literal, Windows PowerShell argv literal).
        // Each was run through the real shell into a program printing its argv.
        let cases = [
            ("two words", "'two words'", "'two words'", "'two words'"),
            ("it's", r"'it'\''s'", "'it''s'", "'it''s'"),
            (
                r#"say "hi""#,
                r#"'say "hi"'"#,
                r#"'say "hi"'"#,
                r#"'say \"hi\"'"#,
            ),
            (r"x y\", r"'x y\'", r"'x y\'", r"'x y\\'"),
            (r#"a\"b"#, r#"'a\"b'"#, r#"'a\"b'"#, r#"'a\\\"b'"#),
            ("", "''", "''", r#"'""'"#),
            (
                "$HOME and $(id)",
                "'$HOME and $(id)'",
                "'$HOME and $(id)'",
                "'$HOME and $(id)'",
            ),
            (
                "a\u{2019}b",
                "'a\u{2019}b'",
                "'a\u{2019}\u{2019}b'",
                "'a\u{2019}\u{2019}b'",
            ),
        ];
        for (input, posix, pwsh, legacy) in cases {
            let a = argv(&["claude", input]);
            let dir = format!("/x/{input}");
            for (shell, want_arg, want_dir) in [
                (RemoteShell::Posix, posix, posix),
                (RemoteShell::Pwsh, pwsh, pwsh),
                // Set-Location is a cmdlet: only native argv gets the 5.1 escape.
                (RemoteShell::WindowsPowerShell, legacy, pwsh),
            ] {
                let r = remote(shell);
                // No Tunnel: a carried reply note would end the argument.
                let l = Launch {
                    tunnel: None,
                    ..launch(&r, &dir, &a)
                };
                let s = script(&build(&l).unwrap(), shell);
                assert!(
                    s.contains(&format!(" {want_arg}\n")),
                    "{shell:?} argv {input:?}:\n{s}"
                );
                // dir is "/x/" + input, quoted by the same rule.
                assert!(
                    s.contains(&format!("'/x/{}", &want_dir[1..])),
                    "{shell:?} dir {input:?}:\n{s}"
                );
            }
        }
        // The command name itself is never 5.1-escaped.
        let r = remote(RemoteShell::WindowsPowerShell);
        let a = argv(&[r#"C:\tools\"x"\bin.exe"#]);
        assert!(
            script(&build(&launch(&r, "", &a)).unwrap(), r.shell)
                .contains(r#"& 'C:\tools\"x"\bin.exe'"#)
        );
    }

    #[test]
    fn remote_windows_powershell_refuses_an_argument_it_would_split() {
        // 5.1 sees the space as inside quotes, passes the arg bare, and it splits.
        let a = argv(&["claude", r#""quoted all""#]);
        let r = remote(RemoteShell::WindowsPowerShell);
        let e = build(&launch(&r, "", &a)).unwrap_err();
        assert!(e.contains("pwsh") && e.contains("chat"), "{e}");
        // pwsh passes it fine, and a space before the quote is fine on 5.1.
        assert!(build(&launch(&remote(RemoteShell::Pwsh), "", &a)).is_ok());
        let a = argv(&["claude", r#"so "quoted all""#]);
        assert!(build(&launch(&r, "", &a)).is_ok());
    }

    #[test]
    fn remote_windows_scripts_and_lines() {
        let a = argv(&["claude"]);
        for (shell, exe) in [
            (RemoteShell::Pwsh, "pwsh"),
            (RemoteShell::WindowsPowerShell, "powershell"),
        ] {
            let r = remote(shell);
            let b = build(&launch(&r, r"C:\src\my repo", &a)).unwrap();
            assert!(
                b.ssh
                    .last()
                    .unwrap()
                    .starts_with(&format!("{exe} -NoLogo -EncodedCommand ")),
                "{}",
                b.ssh.last().unwrap()
            );
            assert_eq!(
                script(&b, shell),
                "try { Set-Location -LiteralPath 'C:\\src\\my repo' -ErrorAction Stop } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }\n\
                 $env:FOREMAN = '1'\n\
                 $env:FOREMAN_TERMINAL_ID = 't7'\n\
                 $env:FOREMAN_PROJECT_ID = 'p3'\n\
                 $env:COLORTERM = 'truecolor'\n\
                 $env:FOREMAN_URL = 'http://127.0.0.1:41234'\n\
                 $env:FOREMAN_TOKEN = 'tok'\n\
                 & 'claude'\n\
                 exit $LASTEXITCODE\n"
            );
        }
    }

    #[test]
    fn remote_blank_dir_skips_the_cd_and_tilde_means_home() {
        let a = argv(&["claude"]);
        for shell in [RemoteShell::Posix, RemoteShell::Pwsh] {
            let r = remote(shell);
            let s = script(&build(&launch(&r, "", &a)).unwrap(), shell);
            assert!(!s.contains("cd --") && !s.contains("Set-Location"), "{s}");
        }
        let r = remote(RemoteShell::Posix);
        let s = script(
            &build(&launch(&r, "~/my src", &a)).unwrap(),
            RemoteShell::Posix,
        );
        assert!(s.starts_with("cd -- \"$HOME\"/'my src' || exit 1\n"), "{s}");
        let r = remote(RemoteShell::Pwsh);
        let s = script(&build(&launch(&r, "~", &a)).unwrap(), RemoteShell::Pwsh);
        assert!(
            s.starts_with("try { Set-Location -LiteralPath $HOME "),
            "{s}"
        );
    }

    #[test]
    fn remote_shell_launch_leaves_an_interactive_shell() {
        let r = remote(RemoteShell::Posix);
        let s = script(&build(&launch(&r, "", &[])).unwrap(), RemoteShell::Posix);
        assert!(s.ends_with("exec \"${SHELL:-sh}\" -li\n"), "{s}");
        let r = remote(RemoteShell::Pwsh);
        let b = build(&launch(&r, "", &[])).unwrap();
        assert!(
            b.ssh
                .last()
                .unwrap()
                .starts_with("pwsh -NoLogo -NoExit -EncodedCommand ")
        );
        assert!(!script(&b, RemoteShell::Pwsh).contains('&'));
        assert!(b.dispatched.is_empty());
    }

    #[test]
    fn remote_windows_over_cap_is_refused_not_truncated() {
        let brief = "x".repeat(4000); // ~2.7 line chars per script char
        let a = argv(&["claude", &brief]);
        for shell in [RemoteShell::Pwsh, RemoteShell::WindowsPowerShell] {
            let r = remote(shell);
            let e = build(&launch(&r, "", &a)).unwrap_err();
            assert!(e.contains("desk") && e.contains("chat"), "{e}");
        }
        // Posix has no practical cap.
        let r = remote(RemoteShell::Posix);
        assert!(build(&launch(&r, "", &a)).is_ok());
    }

    #[test]
    fn remote_keeps_the_logical_argv_and_applies_the_codex_rule_inside() {
        let a = argv(&["codex", "do the thing"]);
        let r = remote(RemoteShell::Posix);
        // No Tunnel, so no reply note rides in the prompt.
        let b = build(&Launch {
            tunnel: None,
            ..launch(&r, "", &a)
        })
        .unwrap();
        assert_eq!(b.dispatched, a);
        assert!(b.ssh.iter().all(|w| !w.contains("codex")));
        assert!(
            script(&b, RemoteShell::Posix)
                .contains("foreman-launch 'codex' '--no-daemon' 'do the thing'\n")
        );
    }

    #[test]
    fn remote_claude_launch_carries_the_installer_s_state_hooks() {
        let a = argv(&["claude", "--model", "x"]);
        let run = claude_with_hooks(&a, RemoteShell::Posix).unwrap();
        assert_eq!(run[..2], argv(&["claude", "--settings"])[..]);
        assert_eq!(run[3..], a[1..]);
        let settings: serde_json::Value = serde_json::from_str(&run[2]).unwrap();
        let hooks = settings["hooks"].as_object().unwrap();
        let mut want = crate::agent_hooks::state_events("claude").to_vec();
        want.push(REMOTE_GATE_EVENT);
        let mut got: Vec<&str> = hooks.keys().map(String::as_str).collect();
        want.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, want, "the local installer's events, and the gate's");
        for (event, groups) in hooks {
            let h = &groups[0]["hooks"][0];
            let cmd = h["command"].as_str().unwrap();
            assert!(
                cmd.starts_with("curl -s ")
                    && cmd.contains("\"Authorization: Bearer $FOREMAN_TOKEN\"")
                    && cmd.contains(&format!("\"$FOREMAN_URL/hook?agent=claude&event={event}\""))
                    && cmd.contains("-o /dev/null")
                    && cmd.ends_with("|| true"),
                "{cmd}"
            );
            assert_eq!(
                cmd.contains("--data-binary @-"),
                PAYLOAD_EVENTS.contains(&event.as_str()),
                "only the events the subagent filter reads send stdin: {cmd}"
            );
            assert_eq!(h["async"], true);
        }
        // The token is read from the environment, never written into argv.
        assert!(!run[2].contains("tok"));
        let r = remote(RemoteShell::Posix);
        let s = script(&build(&launch(&r, "", &a)).unwrap(), RemoteShell::Posix);
        assert!(s.contains(&format!("'claude' '--settings' {} ", sh_quote(&run[2]))));
    }

    #[test]
    fn remote_hooks_are_left_out_of_every_other_launch() {
        for words in [
            vec!["codex", "task"],
            vec!["grok", "task"],
            vec!["npx", "@anthropic-ai/claude-code", "task"],
            vec!["claude", "--settings", "/mine.json"],
            vec!["claude", "--settings={}"],
        ] {
            assert_eq!(
                claude_with_hooks(&argv(&words), RemoteShell::Posix),
                None,
                "{words:?}"
            );
        }
        let r = remote(RemoteShell::Posix);
        let s = script(
            &build(&launch(&r, "", &argv(&["grok", "task"]))).unwrap(),
            r.shell,
        );
        assert!(s.ends_with("foreman-launch 'grok' 'task'\n"), "{s}");
        for shell in [RemoteShell::Pwsh, RemoteShell::WindowsPowerShell] {
            let r = remote(shell);
            let s = script(&build(&launch(&r, "", &argv(&["claude"]))).unwrap(), shell);
            assert!(!s.contains("--settings"), "{s}");
        }
        let a = argv(&["claude"]);
        let mut l = launch(&r, "", &a);
        l.tunnel = None;
        assert!(!script(&build(&l).unwrap(), r.shell).contains("--settings"));
        // A plain shell and every install Session, even handed a Tunnel
        // (the window manager never gives one), stay as they were.
        let r = remote(RemoteShell::Posix);
        let shell: Vec<String> = Vec::new();
        assert!(!script(&build(&launch(&r, "", &shell)).unwrap(), r.shell).contains("--settings"));
        for i in INSTALL.iter().filter(|i| i.shell == InstallShell::Posix) {
            let a = install_argv(RemoteShell::Posix, i.command);
            assert_eq!(install_of(&a), Some(i.provider));
            let s = script(&build(&launch(&r, "", &a)).unwrap(), r.shell);
            assert!(!s.contains("--settings"), "{s}");
        }
    }

    #[test]
    fn a_reconnected_claude_gets_the_hooks_again_with_the_new_token() {
        let r = remote(RemoteShell::Posix);
        // The bare program resumes; a wrapper relaunches as bare `claude`.
        for original in [
            argv(&["claude", "do the card"]),
            argv(&["npx", "@anthropic-ai/claude-code", "do the card"]),
        ] {
            let re = relaunch(&original);
            assert_eq!(re.agent, Some("claude"), "{original:?}");
            let mut l = launch(&r, "", &re.argv);
            l.tunnel = Some(Tunnel {
                remote_port: 41999,
                local_port: 50002,
                token: "fresh-token",
            });
            let s = script(&build(&l).unwrap(), r.shell);
            let hooked = claude_with_hooks(&re.argv, r.shell).expect("bare claude");
            assert!(
                s.contains(&format!("'claude' '--settings' {}", sh_quote(&hooked[2]))),
                "{s}"
            );
            // The hooks read the token from the launch environment, which
            // carries the new one.
            assert!(s.contains("export FOREMAN_TOKEN='fresh-token'\n"), "{s}");
            assert!(!hooked[2].contains("fresh-token") && !hooked[2].contains("tok"));
        }
        // A non-agent command relaunches as it was, and an install as a shell.
        for original in [
            argv(&["htop"]),
            install_argv(
                RemoteShell::Posix,
                "curl -fsSL https://claude.ai/install.sh | bash",
            ),
        ] {
            let re = relaunch(&original);
            let s = script(&build(&launch(&r, "", &re.argv)).unwrap(), r.shell);
            assert!(!s.contains("--settings"), "{s}");
        }
    }

    #[test]
    fn remote_without_a_tunnel_has_no_forward_and_no_url() {
        let r = remote(RemoteShell::Posix);
        let a = argv(&["claude"]);
        let mut l = launch(&r, "", &a);
        l.tunnel = None;
        l.project_id = None;
        let b = build(&l).unwrap();
        assert!(
            !b.ssh
                .iter()
                .any(|w| w == "-R" || w.contains("ExitOnForwardFailure"))
        );
        let s = script(&b, RemoteShell::Posix);
        assert!(!s.contains("FOREMAN_URL") && !s.contains("FOREMAN_TOKEN"));
        assert!(!s.contains("FOREMAN_PROJECT_ID"));
        // The target always follows `--`.
        let at = b.ssh.iter().position(|w| w == "--").unwrap();
        assert_eq!(b.ssh[at + 1], "andy@desk");
    }

    #[test]
    fn remote_sanitize_drops_bad_dup_and_reserved_entries() {
        let mk = |name: &str, target: &str| Remote {
            name: name.into(),
            target: target.into(),
            shell: RemoteShell::Posix,
            dirs: BTreeMap::new(),
        };
        let got = sanitize(vec![
            mk(" desk ", " andy@desk "),
            mk("DESK", "other"),               // duplicate, case-insensitive
            mk("local", "x"),                  // reserved
            mk("Local", "x"),                  // reserved
            mk("evil", "-oProxyCommand=calc"), // would be an ssh option
            mk("spaced", "a b"),
            mk("", "x"),
            mk("a/b", "x"),
            mk(&"n".repeat(MAX_NAME + 1), "x"),
            mk("laptop", "laptop-alias"),
        ]);
        let names: Vec<_> = got
            .iter()
            .map(|r| (r.name.as_str(), r.target.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![("desk", "andy@desk"), ("laptop", "laptop-alias")]
        );
    }

    #[test]
    fn remote_find_is_case_insensitive_and_names_the_known_ones() {
        let list = vec![remote(RemoteShell::Posix), {
            let mut r = remote(RemoteShell::Pwsh);
            r.name = "laptop".into();
            r
        }];
        assert_eq!(find(&list, "DESK").unwrap().name, "desk");
        let e = find(&list, "garage").unwrap_err();
        assert!(
            e.contains("\"garage\"") && e.contains("desk, laptop"),
            "{e}"
        );
        let e = find(&[], "desk").unwrap_err();
        assert!(e.contains("none are set up"), "{e}");
    }

    #[test]
    fn remote_launch_dir_prefers_cwd_then_mapping_and_names_both_fixes() {
        let mut r = remote(RemoteShell::Posix);
        r.dirs.insert(
            r"H:\claude code\foreman".into(),
            "/home/andy/foreman".into(),
        );
        r.dirs.insert(r"H:\scratch".into(), String::new());
        // --cwd wins over a mapping.
        assert_eq!(
            launch_dir(&r, Some("/tmp"), Some(r"H:\claude code\foreman")),
            Ok("/tmp")
        );
        // Case, slash direction and a trailing separator still match.
        assert_eq!(
            launch_dir(&r, None, Some("h:/Claude Code/foreman/")),
            Ok("/home/andy/foreman")
        );
        // A blank mapping is a saved answer: the Remote's home.
        assert_eq!(launch_dir(&r, None, Some(r"H:\scratch")), Ok(""));
        let e = launch_dir(&r, None, Some(r"H:\other")).unwrap_err();
        assert!(
            e.contains("--cwd") && e.contains("directory prompt") && e.contains(r"H:\other"),
            "{e}"
        );
        let e = launch_dir(&r, None, None).unwrap_err();
        assert!(e.contains("--cwd"), "{e}");
    }

    #[test]
    fn a_dirs_key_is_written_without_the_verbatim_prefix() {
        assert_eq!(plain_dir(r"\\?\H:\src\app"), r"H:\src\app");
        assert_eq!(plain_dir(r"\\?\UNC\nas\share"), r"\\nas\share");
        assert_eq!(plain_dir(r"H:\src\app"), r"H:\src\app");
        assert!(same_dir(&plain_dir(r"\\?\H:\src\app"), r"\\?\H:\src\app\"));
    }

    #[test]
    fn remote_launch_dir_matches_through_a_verbatim_prefix() {
        let mut r = remote(RemoteShell::Posix);
        r.dirs
            .insert(r"H:\claude code\foreman".into(), "/srv/foreman".into());
        r.dirs
            .insert(r"\\?\UNC\nas\share\proj".into(), "/srv/proj".into());
        // A canonicalized Project directory still finds its mapping...
        assert_eq!(
            launch_dir(&r, None, Some(r"\\?\H:\claude code\foreman\")),
            Ok("/srv/foreman")
        );
        // ...in both directions, and through the UNC form.
        assert_eq!(
            launch_dir(&r, None, Some(r"\\nas\share\proj")),
            Ok("/srv/proj")
        );
        assert!(launch_dir(&r, None, Some(r"\\?\H:\claude code")).is_err());
    }

    /// Every agent Test looks for has a command for every shell kind, and the
    /// command is the one its own shell kind runs.
    #[test]
    fn every_agent_has_an_install_for_every_shell_kind() {
        for provider in crate::remote_probe::AGENTS {
            for shell in [
                RemoteShell::Posix,
                RemoteShell::Pwsh,
                RemoteShell::WindowsPowerShell,
            ] {
                let i = install_for(provider, shell)
                    .unwrap_or_else(|| panic!("{provider} on {shell:?} has no install"));
                assert_eq!(i.provider, provider);
                assert_eq!(i.shell, InstallShell::from(shell));
                assert!(!i.command.trim().is_empty());
                // A POSIX command fetches with curl and a PowerShell one
                // with irm, so the curl gate matches the command.
                match i.shell {
                    InstallShell::Posix => {
                        assert!(i.command.starts_with("curl "), "{}", i.command);
                        assert!(i.needs.contains(&Prereq::Curl));
                        assert_eq!(
                            i.needs.contains(&Prereq::Bash),
                            i.command.ends_with("| bash"),
                            "{}",
                            i.command
                        );
                    }
                    InstallShell::PowerShell => {
                        assert!(i.command.contains("irm "), "{}", i.command);
                        assert!(i.needs.is_empty());
                    }
                }
            }
        }
        // No entry, no action.
        assert_eq!(install_for("aider", RemoteShell::Posix), None);
        assert_eq!(install_for("", RemoteShell::Pwsh), None);
        // One entry per (provider, shell kind).
        for (a, x) in INSTALL.iter().enumerate() {
            for y in &INSTALL[a + 1..] {
                assert!(
                    !(x.provider == y.provider && x.shell == y.shell),
                    "{} twice",
                    x.provider
                );
            }
        }
    }

    /// The install Session goes through `build` with the install as its
    /// logical argv: the tab and reconnect see that argv, and the script
    /// carries the vendor command as one quoted literal, run in the home.
    #[test]
    fn an_install_is_built_like_any_launch_with_the_command_as_its_argv() {
        for shell in [
            RemoteShell::Posix,
            RemoteShell::Pwsh,
            RemoteShell::WindowsPowerShell,
        ] {
            let r = remote(shell);
            let i = install_for("claude", shell).unwrap();
            let argv = install_argv(shell, i.command);
            assert_eq!(argv.last().map(String::as_str), Some(i.command));
            let b = build(&launch(&r, "", &argv)).expect("an install fits");
            assert_eq!(b.dispatched, argv);
            let s = script(&b, shell);
            assert!(!s.contains("cd --") && !s.contains("Set-Location"), "{s}");
            match shell {
                RemoteShell::Posix => assert!(
                    s.contains(&format!("foreman-launch 'sh' '-c' '{}'", i.command)),
                    "{s}"
                ),
                _ => assert!(s.contains(&format!("'-Command' '{}'", i.command)), "{s}"),
            }
        }
        // Codex's Windows command carries quotes; 5.1 can still pass it.
        let r = remote(RemoteShell::WindowsPowerShell);
        let i = install_for("codex", RemoteShell::WindowsPowerShell).unwrap();
        let argv = install_argv(RemoteShell::WindowsPowerShell, i.command);
        assert!(build(&launch(&r, "", &argv)).is_ok());
    }

    /// Enter on an ended install opens a shell there: the installer never
    /// runs again from the disconnect bar, even though two of the URLs name
    /// the agent and would otherwise read as one.
    #[test]
    fn an_install_session_reconnects_as_a_shell() {
        for i in INSTALL {
            let shell = match i.shell {
                InstallShell::Posix => RemoteShell::Posix,
                InstallShell::PowerShell => RemoteShell::Pwsh,
            };
            let argv = install_argv(shell, i.command);
            assert_eq!(install_of(&argv), Some(i.provider));
            let re = relaunch(&argv);
            assert_eq!(re.argv, Vec::<String>::new(), "{argv:?}");
            assert_eq!((re.agent, re.install), (None, Some(i.provider)));
        }
        // Only the exact argv: the same words typed by hand are a command.
        assert_eq!(install_of(&argv(&["sh", "-c", "curl x | bash"])), None);
        assert_eq!(install_of(&argv(&["claude"])), None);
        assert_eq!(relaunch(&argv(&["claude"])).install, None);
    }

    /// An install Session is a shell running a vendor script, for every
    /// provider on every shell kind: no agent icon, no agent predicate, and
    /// no Tunnel, though the claude and codex URLs name the agent (grok's
    /// does not, which is why the scan alone was inconsistent).
    #[test]
    fn an_install_session_is_never_an_agent_and_carries_no_tunnel() {
        for provider in crate::remote_probe::AGENTS {
            for shell in [
                RemoteShell::Posix,
                RemoteShell::Pwsh,
                RemoteShell::WindowsPowerShell,
            ] {
                let i = install_for(provider, shell).unwrap();
                let argv = install_argv(shell, i.command);
                assert_eq!(crate::icons::IconKind::from_argv(&argv), None, "{argv:?}");
                assert_eq!(agent_of(&argv), None, "{argv:?}");
                assert!(!carries_tunnel(&argv), "{argv:?}");
            }
        }
        // Everything else keeps its Tunnel.
        assert!(carries_tunnel(&argv(&["claude"])));
        assert!(carries_tunnel(&[]));
        assert!(carries_tunnel(&argv(&["sh", "-c", "curl x | bash"])));
    }

    /// The prompt argument is named only for the shapes foreman dispatches
    /// and documents; everything else gets the note typed.
    #[test]
    fn the_prompt_argument_is_named_only_when_certain() {
        let brief = "do the card";
        let accepted: &[&[&str]] = &[
            &["claude", brief],
            &["codex", brief],
            &["grok", brief],
            // `card_argv` with a model.
            &["claude", brief, "--model", "claude-opus-5-5"],
            &["codex", brief, "--model", "gpt-6.1-sol"],
            &["claude", "line one\nline two"],
        ];
        for a in accepted {
            assert_eq!(prompt_index(&argv(a)), Some(1), "{a:?}");
        }
        let refused: &[&[&str]] = &[
            // No prompt: a shell, a bare agent, an empty brief.
            &[],
            &["claude"],
            &["claude", ""],
            // One word could be a subcommand (or is one).
            &["claude", "hi"],
            &["claude", "mcp"],
            &["codex", "exec", brief],
            &["codex", "resume", "--last"],
            &["codex", "e", brief],
            // A flag first: print mode, resume forms, a flag's value.
            &["claude", "-p", brief],
            &["claude", "--print", brief],
            &["claude", "--continue"],
            &["claude", "--resume", "abc"],
            &["claude", "--model", "x", brief],
            &["codex", "--no-daemon", brief],
            &["claude", "- do the card"],
            // Anything after the brief but `--model <id>`.
            &["claude", brief, "-p"],
            &["claude", brief, "--allowedTools", "Bash"],
            &["claude", brief, "--model"],
            &["claude", brief, "--model", "-x"],
            &["claude", brief, "--model", "two words"],
            &["claude", brief, "--model", "x", "more"],
            &["claude", brief, "another prompt"],
            &["grok", brief, "--model", "x"],
            // Not the bare program: paths, extensions, case, wrappers.
            &["/usr/local/bin/claude", brief],
            &["./claude", brief],
            &[r"C:\bin\claude.exe", brief],
            &["claude.exe", brief],
            &["Claude", brief],
            &["my-claude", brief],
            &["claude-wrapper", brief],
            &["npx", "@anthropic-ai/claude-code", brief],
            &["wsl", "claude", brief],
            &["bash", "-lc", "claude 'do the card'"],
            &["htop", "-d", "5"],
        ];
        for a in refused {
            assert_eq!(prompt_index(&argv(a)), None, "{a:?}");
        }
        // Every reconnect argv and every install Session is refused.
        for a in accepted {
            assert_eq!(prompt_index(&relaunch(&argv(a)).argv), None, "{a:?}");
        }
        for i in INSTALL {
            for shell in [
                RemoteShell::Posix,
                RemoteShell::Pwsh,
                RemoteShell::WindowsPowerShell,
            ] {
                assert_eq!(prompt_index(&install_argv(shell, i.command)), None);
            }
        }
    }

    /// Who gets the reply note in the prompt (naming the variables), who
    /// gets it typed, and who waits for a post. One row per agent and shell.
    #[test]
    fn the_reply_note_s_route_per_agent_and_shell() {
        use crate::control::tunnel::NoteGoes::{AtLaunch, InPrompt, WithFirstPost};
        use RemoteShell::{Posix, Pwsh, WindowsPowerShell};
        let brief = "do the card";
        let table: &[(&[&str], RemoteShell, crate::control::tunnel::NoteGoes)] = &[
            // Seen on a Posix Remote with both variables in the tool shell.
            (&["claude", brief], Posix, InPrompt),
            (&["claude", brief, "--model", "x"], Posix, InPrompt),
            (&["codex", brief], Posix, InPrompt),
            (&["codex", brief, "--model", "x"], Posix, InPrompt),
            // Never run on a Remote: typed.
            (&["grok", brief], Posix, AtLaunch),
            // Windows: no tool shell seen, and it may not be Posix.
            (&["claude", brief], Pwsh, AtLaunch),
            (&["codex", brief], WindowsPowerShell, AtLaunch),
            (&["grok", brief], Pwsh, AtLaunch),
            // An agent with arguments foreman cannot place: typed.
            (&["claude", "hi"], Posix, AtLaunch),
            (&["claude", "-p", brief], Posix, AtLaunch),
            (&["codex", "exec", brief], Posix, AtLaunch),
            (
                &["npx", "@anthropic-ai/claude-code", brief],
                Posix,
                AtLaunch,
            ),
            (&["/usr/bin/claude", brief], Posix, AtLaunch),
            // A reconnect's resume argv: typed (it has no prompt).
            (&["claude", "--continue"], Posix, AtLaunch),
            (&["codex", "resume", "--last"], Posix, AtLaunch),
            (&["grok", "--continue"], Pwsh, AtLaunch),
            // Bare agent, shell, non-agent command: the first post.
            (&["claude"], Posix, WithFirstPost),
            (&["codex"], Pwsh, WithFirstPost),
            (&[], Posix, WithFirstPost),
            (&["htop", "-d", "5"], Posix, WithFirstPost),
        ];
        for (words, shell, want) in table {
            assert_eq!(
                note_route(&argv(words), *shell),
                *want,
                "{words:?} on {shell:?}"
            );
        }
        // No reconnect argv carries it (the caller types it for an agent).
        for (words, shell, _) in table {
            let re = relaunch(&argv(words));
            assert_ne!(note_route(&re.argv, *shell), InPrompt, "{words:?}");
        }
    }

    #[test]
    fn a_first_launch_carries_the_variable_note_inside_its_prompt() {
        use crate::control::tunnel::NoteGoes;
        let r = remote(RemoteShell::Posix);
        let carried = format!("do the card\n\n{}", crate::control::tunnel::prompt_note());
        let a = argv(&["codex", "do the card"]);
        let b = build(&launch(&r, "", &a)).unwrap();
        assert_eq!(b.note, NoteGoes::InPrompt);
        assert_eq!(b.dispatched, a, "the logical argv never holds the note");
        let s = script(&b, r.shell);
        assert!(
            s.ends_with(&format!("'codex' '--no-daemon' {}\n", sh_quote(&carried))),
            "{s}"
        );
        // Claude: after its hooks, and before `--model`.
        let a = argv(&["claude", "do the card", "--model", "x"]);
        let b = build(&launch(&r, "", &a)).unwrap();
        assert_eq!(b.note, NoteGoes::InPrompt);
        let s = script(&b, r.shell);
        let settings = &claude_with_hooks(&a, r.shell).unwrap()[2];
        assert!(
            s.ends_with(&format!(
                "'claude' '--settings' {} {} '--model' 'x'\n",
                sh_quote(settings),
                sh_quote(&carried)
            )),
            "{s}"
        );
    }

    #[test]
    fn a_launch_whose_note_is_not_carried_is_built_as_it_was() {
        use crate::control::tunnel::NoteGoes;
        for (shell, words) in [
            (RemoteShell::Posix, vec!["claude", "-p", "do the card"]),
            (RemoteShell::Posix, vec!["codex", "exec", "do the card"]),
            (RemoteShell::Posix, vec!["grok", "do the card"]),
            (RemoteShell::Posix, vec!["claude"]),
            (RemoteShell::Posix, vec![]),
            (RemoteShell::Pwsh, vec!["claude", "do the card"]),
            (RemoteShell::WindowsPowerShell, vec!["codex", "do the card"]),
        ] {
            let r = remote(shell);
            let a = argv(&words);
            let l = launch(&r, "", &a);
            let b = build(&l).unwrap();
            assert_ne!(b.note, NoteGoes::InPrompt, "{words:?}");
            assert_eq!(b.ssh, ssh_argv(&l, &a).unwrap(), "{words:?}");
        }
        // No Tunnel: nowhere for the note's command to go.
        let r = remote(RemoteShell::Posix);
        let a = argv(&["claude", "do the card"]);
        let mut l = launch(&r, "", &a);
        l.tunnel = None;
        let b = build(&l).unwrap();
        assert_eq!(b.ssh, ssh_argv(&l, &a).unwrap());
        assert_eq!(b.hold, None);
    }

    /// The token may appear once in a built launch: the script's own export,
    /// which lives only while the launch shells run (each `exec`s the next).
    /// Anywhere else (the agent's argv, the hooks, the carried note, the ssh
    /// options) every user on the Remote reads it in `ps` for the agent's
    /// life.
    #[test]
    fn no_remote_launch_carries_the_token_outside_its_export() {
        const TOKEN: &str = "a3qTOKENliteral9f8e7d";
        let brief = "do the card";
        let firsts: &[&[&str]] = &[
            &["claude", brief],
            &["claude", brief, "--model", "claude-opus-5-5"],
            &["codex", brief],
            &["codex", brief, "--model", "x"],
            &["grok", brief],
            &["claude", "hi"],
            &["claude", "-p", brief],
            &["codex", "exec", brief],
            &["npx", "@anthropic-ai/claude-code", brief],
            &["/usr/bin/claude", brief],
            &["claude"],
            &["codex"],
            &["grok"],
            &[],
            &["htop", "-d", "5"],
        ];
        let mut built = 0;
        for shell in [
            RemoteShell::Posix,
            RemoteShell::Pwsh,
            RemoteShell::WindowsPowerShell,
        ] {
            let r = remote(shell);
            for words in firsts {
                let first = argv(words);
                for a in [first.clone(), relaunch(&first).argv] {
                    let l = Launch {
                        tunnel: Some(Tunnel {
                            remote_port: 41234,
                            local_port: 50001,
                            token: TOKEN,
                        }),
                        ..launch(&r, "~/src/proj", &a)
                    };
                    let Ok(b) = build(&l) else { continue };
                    built += 1;
                    let (line, opts) = b.ssh.split_last().unwrap();
                    for o in opts {
                        assert!(!o.contains(TOKEN), "{a:?} {shell:?}: ssh option {o}");
                    }
                    assert!(!line.contains(TOKEN), "{a:?} {shell:?}: the line in clear");
                    assert!(!b.dispatched.iter().any(|d| d.contains(TOKEN)));
                    let export = match shell {
                        RemoteShell::Posix => format!("export FOREMAN_TOKEN='{TOKEN}'"),
                        _ => format!("$env:FOREMAN_TOKEN = '{TOKEN}'"),
                    };
                    let s = script(&b, shell);
                    let holding: Vec<&str> = s.lines().filter(|l| l.contains(TOKEN)).collect();
                    assert_eq!(holding, [export.as_str()], "{a:?} on {shell:?}:\n{s}");
                    assert_eq!(s.matches(TOKEN).count(), 1, "{a:?} on {shell:?}:\n{s}");
                }
            }
        }
        assert!(built > 60, "the table must build: {built}");
    }

    /// Only a launch carrying Claude's hooks holds chat, a launch with its
    /// brief in argv until the brief is submitted, and its hooks include the
    /// one that fires first past the trust dialog.
    #[test]
    fn only_a_hooked_claude_launch_holds_chat_and_a_briefed_one_waits_for_its_prompt() {
        use crate::agent_state::HookEvent;
        use ChatHold::{UntilPrompt, UntilStart};
        let posix = remote(RemoteShell::Posix);
        for (words, hold) in [
            (&["claude", "do the card"][..], Some(UntilPrompt)),
            (
                &["claude", "do the card", "--model", "x"],
                Some(UntilPrompt),
            ),
            (&["claude"], Some(UntilStart)),
            (&["claude", "--continue"], Some(UntilStart)),
            // A prompt foreman cannot place: the first event, as for bare.
            (&["claude", "hi"], Some(UntilStart)),
            (&["claude", "--settings", "mine.json"], None),
            (&["npx", "@anthropic-ai/claude-code"], None),
            (&["codex", "do the card"], None),
            (&["grok", "do the card"], None),
            (&[], None),
        ] {
            let a = argv(words);
            assert_eq!(
                build(&launch(&posix, "", &a)).unwrap().hold,
                hold,
                "{words:?}"
            );
        }
        for shell in [RemoteShell::Pwsh, RemoteShell::WindowsPowerShell] {
            let a = argv(&["claude"]);
            assert_eq!(build(&launch(&remote(shell), "", &a)).unwrap().hold, None);
        }
        let every = [
            HookEvent::SessionStart,
            HookEvent::UserPromptSubmit,
            HookEvent::PermissionRequest,
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::StopFailure,
        ];
        for e in every {
            assert!(UntilStart.opened_by(e), "{e:?}");
            assert_eq!(
                UntilPrompt.opened_by(e),
                e != HookEvent::SessionStart,
                "{e:?}"
            );
        }
        let with = claude_with_hooks(&argv(&["claude"]), RemoteShell::Posix).unwrap();
        let settings: serde_json::Value = serde_json::from_str(&with[2]).unwrap();
        let start = settings["hooks"][REMOTE_GATE_EVENT][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            start.contains(&format!("/hook?agent=claude&event={REMOTE_GATE_EVENT}")),
            "{start}"
        );
        assert!(crate::agent_state::HookEvent::parse(REMOTE_GATE_EVENT).is_some());
    }
}
