# Remote Sessions over SSH — design spec

Direction settled with the user 2026-11-20: Sessions that run on another PC,
opened from this foreman over SSH, living in the same Project as local ones
and marked with a globe. Decisions tagged *(proposed)* were written into this
spec without being discussed and need a yes or no before tickets are cut.

Nothing here has been run. The experiments at the end gate the build; a
failure in the first two changes the design.

## Problem

One PC is the seat. Agents are dispatched to other PCs so parallel work gets
its own checkout without worktrees on the seat PC. Today that needs a second
foreman running on each PC, a Tailscale pairing in both directions
(`foreman peers add`), and `--host` calls between two separate Chat rooms. A
remote sender is not a Member, so there is no delivery back, no `--re`, no
badge, and the pane itself is on the other PC's monitor.

## Decisions

- **A Remote Session is a local Session whose process is the SSH client.**
  The PTY and emulator stay on the seat PC, so rendering, tiling, tabs, Zoom,
  `send` and `snapshot` need no change. Still `Content::Terminal`: no new
  Content kind, no third window state.
- **One Chat room, on the seat PC.** A Remote Session has a local Session, so
  it is an ordinary Member. Delivery to it is the existing PTY write. Only the
  agent's way *back* (posting, hook events) is new.
- **The way back is an SSH reverse tunnel plus a per-Session token**
  *(proposed; recommended over the alternatives below)*. It rides the
  connection that already exists, so adding a PC is one SSH target.
- **Remotes are structured settings, not command templates.** Foreman builds
  the argv, because it has to inject the directory, the agent and the
  credentials, and one pure builder is testable where a user string is not.
- **Foreman stores no keys and no passwords.** The target is whatever `ssh`
  accepts, including `~/.ssh/config` aliases. Prompts appear in the pane.
- **The system `ssh.exe` is the transport.** No SSH crate.
- **`open`, `send`, `close` stay local-only.** The seat PC dials out; nothing
  on a Remote can open or drive a Session here.

Rejected:

- *Host-authoritative mirror* (`docs/2026-10-16-remote-control-research.md`):
  the other PC's foreman owns the PTY and this one renders a replica. Needs a
  display protocol, late-attach snapshots, a controller lease and a resize
  policy. It is the only design that survives a dropped connection, and it is
  the follow-up if that turns out to matter; it is not the first step.
- *Tailscale listener for the way back* (`control::serve_remote`): it exists,
  but it identifies a caller by source address, which cannot tell two
  Sessions on one PC apart, and it needs a second network configured per PC.
- *In-band escape sequence for the way back*: no listener at all, but any
  text printed in the pane (a `cat` of a hostile file) could forge a chat
  post into other agents' prompts, and the Remote's own ConPTY may drop
  sequences it does not know.
- *Remote `open` on the other PC's foreman*: exactly what the local-only rule
  forbids, and the pane would be on the wrong monitor.
- *Auto-reconnect*: an uncertain shell input must never be resent on its own.
- *An SSH library*: a new dependency that loses the user's ssh config, agent,
  `ProxyJump` and host-key store.

## Goals and non-goals

Goals: pick a PC by name and get a terminal on it in the Project's directory
there; agents dispatch to a Remote with one flag; a Remote Session is a full
Member; a dropped connection is visible and one keypress from recovery.

Non-goals (v1): non-Windows Remotes (the builder leaves room; there is no
foreman CLI to answer on them); surviving a dropped connection; kanban card
dispatch or Assignments onto a Remote (worktree, integration queue and
process-tree ownership checks are all local); `icat`, image paste, file drops
and file-path links across PCs; browsing the Remote's directories; installing
anything on a Remote over SSH; syncing the repo between PCs.

## Vocabulary

Lands in `CONTEXT.md` with the first implementing commit.

- **Remote** — a named PC foreman can open Sessions on over SSH. _Avoid_:
  host (taken by ConPTY host and "the host that spawned it"), peer (that is a
  foreman instance allowlisted for `--host`), server.
- **Remote Session** — a Session whose process is an SSH client into a
  Remote. _Avoid_: SSH tab, remote terminal.
- **Tunnel** — the per-Session reverse forward a Remote Session's agent uses
  to reach the seat PC's Control plane.

`--host` (talk to another PC's foreman) and `--on` (run a Session on a Remote
from this foreman) are different features and stay different words. Remotes
need neither Tailscale nor `Settings::remote_peers`.

## Remote model

`Settings` gains `remotes: Vec<Remote>`, sanitized on load like
`remote_peers` and saved by the existing debounced write.

| Field | Meaning | Validation |
|---|---|---|
| `name` | The label in the launcher, on tabs, and for `--on` | Same character rule as `config::valid_peer`, short cap, unique, not `local` |
| `target` | SSH destination: `user@pc` or an ssh-config alias | Non-empty, no whitespace, must not start with `-` |
| `shell` | `Pwsh` or `WindowsPowerShell`: the PowerShell on that PC | Enum; Test fills it in |
| `dirs` | Seat-PC Project directory → directory on this Remote | Plain strings |

No port field: a non-default port belongs in the target's ssh-config entry.
A `target` starting with `-` would be read by `ssh` as an option, so it is
refused at the settings boundary and the builder also puts `--` before it.

## Launch

A new `src/remote.rs` holds a pure builder: a `Remote`, the remote
directory, the Session ids, the Tunnel details and the logical argv in; the
local `ssh` argv out. `Session::spawn_argv` runs it unchanged. Its
`dispatched` argv is the *logical* one (`claude …`), so
`IconKind::from_argv` still sees the agent name; the wrapped line never
contains it. `agent_command::codex_in_process` applies to the logical argv
before wrapping.

Local side:

```
ssh -t -o ExitOnForwardFailure=yes -o ServerAliveInterval=15
    -o ServerAliveCountMax=3 -R <rport>:127.0.0.1:<lport> -- <target> <remote line>
```

Remote line: `pwsh -NoLogo [-NoExit] -EncodedCommand <base64>` (or
`powershell` per `shell`). The script is base64 of UTF-16LE, so the line is
letters, digits and `+/=` only. That survives whichever login shell sshd
runs first (cmd or PowerShell) with no per-shell quoting. Inside the script
there is one quoting rule: PowerShell single quotes, `'` doubled.

The script, in order:

1. `Set-Location -LiteralPath <dir>` with errors terminating. A missing
   directory stops the launch with the error on screen; it never starts an
   agent in the wrong place. A blank mapping skips this step (home).
2. Set `FOREMAN=1`, `FOREMAN_TERMINAL_ID`, `FOREMAN_PROJECT_ID`,
   `COLORTERM`, `FOREMAN_REMOTE=127.0.0.1:<rport>`, `FOREMAN_TOKEN`.
3. Set `FOREMAN_EXE` to the Remote's own CLI: the installed
   `%LOCALAPPDATA%\Programs\foreman\foreman.com` if present, else PATH, else
   unset.
4. Run the logical argv, each element a quoted literal. A Shell launch has
   no argv and uses `-NoExit`; an agent or Dispatch ends with its process,
   the same as locally.

Not injected: `FOREMAN_PIPE`, `FOREMAN_TITLE_PIPE` (seat-PC pipe names that
mean nothing there) and `KITTY_WINDOW_ID` (graphics are a non-goal).

**Length cap.** If the Remote's login shell is cmd, the whole remote line
must fit cmd's 8191-character limit, and the encoding costs about 2.7 line
characters per script character. The builder refuses a launch over the cap
with an error that says to open without the prompt and send the brief by
chat. It never truncates.

**Directory.** The first launch on a Remote from a Project with no mapping
asks once in a small text prompt, prefilled with the seat-PC path, and
saves the answer to `Remote::dirs`. Splits always use the mapping, not
wherever the Remote shell has `cd`'d to (there is no OSC 7 tracking).

## The way back

One loopback listener per foreman instance, bound to `127.0.0.1` on an
OS-assigned port, started when the first Remote Session spawns. Each Remote
Session forwards its own remote port (random within a fixed range) to it.
`ExitOnForwardFailure` turns a port collision into an immediate, visible
exit; reconnect picks a new port.

A forwarded loopback port is reachable by every process on the Remote, so
the port is not the credential. At spawn foreman mints a random token from
the OS RNG and records token → (Project id, terminal id). A connection's
first line is the token; the request line follows. Request structs are
untouched, so v1 wire compatibility holds by construction.

`control::Origin` gains a Tunnel variant carrying those ids, and
`dispatch_line` gains a third allow-list next to `REMOTE_VERBS`:

- `chat` — post and history. The sender is stamped from the token; a `from`
  in the request is ignored. The post goes down the normal Member path, not
  `ChatRoom::post_remote`.
- `status` — read-only.
- the hook event (below).

The Tunnel listener keeps the peer listener's guards: the request-line cap,
one per-connection deadline, bounded in-flight handlers. A token dies with
its Session.

On the Remote, the CLI checks `FOREMAN_REMOTE` first. When it is set the
CLI uses the Tunnel and **never** falls back to a pipe: the Remote may be
running its own foreman, and the well-known pipe there is the wrong
instance.

Accepted risk: the token sits on a command line (base64, not secret) on
both PCs, so another process running as the same user can read it. It
buys posting chat as that one Session until it exits. The local pipe
already trusts same-user processes with more than that.

## Agent state and naming

The managed hook helper (`src/title_notify.rs`) reports over
`FOREMAN_TITLE_PIPE`. When `FOREMAN_REMOTE` is set it sends the same small
message through the Tunnel instead. `WindowManager::apply_hook_event`
already matches on Project id, terminal id and icon; the token supplies the
first two and the logical argv supplies the icon.

The hooks must exist in the provider config **on the Remote**. v1 does not
install them over SSH: the Remote's own foreman installs them when its
settings ask for them. Test (below) reports whether the helper is
reachable, so a missing badge has an explanation. A hand-typed agent in a
Remote shell gets its icon from the OSC title only; the process-tree scan
sees `ssh.exe`.

## UI

**Launcher.** `launcher::entries` gains a where-strip above AGENTS, shown
only when at least one Remote exists (no Remotes: the menu is exactly as it
is now):

```
 [ Local | desk | laptop | garage ]
 AGENTS   Claude · Codex · Grok
 SHELLS   PowerShell · CMD · SH        (a Remote shows one row: Shell)
 PROJECT  …
```

The selection is per Project, lasts for the app run, and starts at Local.
Past the width that fits, the strip becomes a dropdown instead of wrapping.

**Reachability.** `ssh -G <target>` resolves the real host and port once per
target; a TCP connect with a short timeout answers reachable or offline.
Both run off the UI thread, only while the launcher or the Remotes pane is
open, cached for a few seconds. An offline name is dimmed and stays
clickable, because the probe cannot see through `ProxyJump`.

**Globe.** A small globe overlays the corner of the tab icon and the
Sessions-panel row icon; the agent logo stays. Tooltip: name and target.
One white-silhouette SVG in `assets/icons/`, tinted like the rest.

**Splits inherit.** Splitting or adding a terminal while a Remote Session is
focused opens on that Remote. Anything else opens Local. A Remote is never
chosen by a default.

**Settings → Remotes.** A custom-body pane like Keybindings: one row per
Remote (name, target, shell, reachability, Test, delete), inline add and
edit, ssh-config `Host` names offered as suggestions. **Test** runs one
`ssh -o BatchMode=yes` probe off-thread and reports: login works without a
prompt (or "needs an interactive login, which only a pane can do"), which
PowerShell exists, whether the foreman CLI is installed and its version,
which agent CLIs are on PATH. Deleting a Remote does not touch live
Sessions; each carries its own copy of the name.

**Disconnect.** When a Remote Session's `ssh` exits, the Win stays, shows a
one-line "Disconnected from desk — Enter to reconnect" bar, and takes no
other input. Enter relaunches the same Remote and directory. An agent
Session relaunches with that provider's resume argv from one small table; a
provider with no entry starts fresh. The original Dispatch prompt is never
sent again.

**Inert on a Remote Session:** file-path links (URLs still open), file
drops and image paste (refused with a toast). The close confirm names the
Remote instead of listing a process tree it cannot see.

## Control plane

- `OpenRequest` gains `on: Option<String>`, absent on the wire when `None`,
  with a compat test beside the existing ones. `--cwd` with `--on` is a
  directory on the Remote and is not saved. No mapping and no `--cwd` is an
  error that says which one to supply.
- `foreman remotes` lists name, target and last known reachability in
  `OpenReply::history`. Local callers only.
- `status` marks Remote Sessions with the Remote's name, in text and
  `--json`.
- The dispatch and chat skills gain a short "another PC" section using
  `--on`; both are embedded, so the Codex twins and a rebuild go with it.

## Persistence

`ContentSnap::Terminal` gains an optional Remote name, absent when local.
Restore creates the Win **in the disconnected state** and connects on
Enter. Restoring must not open a batch of SSH connections at launch, some
to PCs that are off and some asking for a password. A name that no longer
exists restores as a local Shell with a toast.

## Experiments that gate the build

1. `ssh -R` between two Windows PCs with the inbox OpenSSH, and a line sent
   back through it. Failure reopens the way-back decision.
2. The encoded launch line through a cmd login shell and a PowerShell one,
   into a directory with spaces, starting each agent CLI. Failure reopens
   the builder.
3. Resize and the resize-then-Up-arrow case through two ConPTYs. Record it;
   `docs/conpty-resize-reflow.md` is settled and this is not a reason to
   reopen it.
4. Kill the local `ssh.exe` and check the Remote for orphaned agent
   processes.
5. Sleep the seat PC mid-run. Time how long the keepalive takes to end the
   Session, and confirm the resume argv recovers the conversation.
6. Password and host-key prompts against the Ready gate: typed input must
   reach `ssh` before the remote shell has ever answered a DSR.
7. A hook fired by an agent on the Remote sees `FOREMAN_REMOTE` and
   `FOREMAN_TOKEN`.

## Order of work

1. Remote model, builder, `--on`, where-strip, globe, Remotes pane,
   disconnect bar, persistence. Nothing on a Remote talks back; no wire
   change beyond `on`.
2. Tunnel listener, token, CLI transport, chat.
3. Hook helper over the Tunnel: badges and naming.

Each slice is usable alone. Slice 1 needs experiments 2–6; slice 2 needs
experiment 1; slice 3 needs experiment 7.

## Tests

Pure and table-driven: the builder (quoting, spaces, `'`, `-`-leading
target, over-cap refusal, logical argv kept for the icon); `sanitize` for
`remotes`; Tunnel verb gating and sender stamping in `dispatch_line`;
`OpenRequest` wire compat for `on`; `launcher::entries` with and without
Remotes; `ContentSnap` round-trip. Nothing in the unit suite opens a
socket to another PC; the experiments cover that by hand.
