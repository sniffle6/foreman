# Remote Sessions over SSH — design spec

Direction settled with the user 2026-11-20: Sessions that run on another PC,
opened from this foreman over SSH, living in the same Project as local ones
and marked with a globe. **The other PC needs nothing from foreman**: sshd,
the agent CLI and `curl`. Windows and Linux Remotes are both in scope, and
the whole design ships as one run of work. Decisions tagged *(proposed)*
were written into this spec without being discussed and need a yes or no
before tickets are cut.

Nothing here has been run. The experiments at the end gate the build; a
failure in the first three changes the design.

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
  it is an ordinary Member. Delivery to it is the existing PTY write, and an
  orchestrator reads it with `snapshot` like any local Session. The Bell and
  the OSC title also arrive in-band.
- **Nothing is installed on a Remote.** No foreman, no skills, no hook
  files. What the agent there needs to know or run travels with the launch.
- **The way back is an SSH reverse tunnel, a per-Session token, and plain
  HTTP** *(proposed)*. The client is `curl`, which ships with Windows and
  Linux. It carries the two things that originate on the Remote: the agent
  pushing a chat post, and hook events for the state badge.
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
- *The foreman CLI on the Remote as the way-back client*: the first draft of
  this spec. It forced an install on every PC and ruled out Linux, to send
  one line of text.
- *Tailscale listener for the way back* (`control::serve_remote`): it exists,
  but it identifies a caller by source address, which cannot tell two
  Sessions on one PC apart, and it needs a second network configured per PC.
- *In-band escape sequence for the way back*: no listener at all, but any
  text printed in the pane (a `cat` of a hostile file) could forge a chat
  post into other agents' prompts, and a Windows Remote's ConPTY may drop
  sequences it does not know.
- *No way back at all*: workable, since an orchestrator can pull with
  `snapshot`. Kept in because push reporting and badges are small once the
  tunnel exists, and idle-aware chat delivery depends on the badge.
- *Remote `open` on the other PC's foreman*: exactly what the local-only rule
  forbids, and the pane would be on the wrong monitor.
- *Auto-reconnect*: an uncertain shell input must never be resent on its own.
- *An SSH library*: a new dependency that loses the user's ssh config, agent,
  `ProxyJump` and host-key store.

## Goals and non-goals

Goals: pick a PC by name and get a terminal on it in the Project's directory
there; agents dispatch to a Remote with one flag; a Remote Session is a full
Member that can post and show state; a dropped connection is visible and one
keypress from recovery; none of it needs setup on the Remote beyond sshd.

Non-goals (v1): surviving a dropped connection; kanban card dispatch or
Assignments onto a Remote (worktree, integration queue and process-tree
ownership checks are all local); `icat`, image paste, file drops and
file-path links across PCs; browsing the Remote's directories; syncing the
repo between PCs; automatic Session naming for Remote Sessions; optional
foreman tooling that runs on a Remote (see Follow-ups).

## Vocabulary

Lands in `CONTEXT.md` with the first implementing commit.

- **Remote** — a named PC foreman can open Sessions on over SSH. _Avoid_:
  host (taken by ConPTY host and "the host that spawned it"), peer (that is a
  foreman instance allowlisted for `--host`), server.
- **Remote Session** — a Session whose process is an SSH client into a
  Remote. _Avoid_: SSH tab, remote terminal.
- **Tunnel** — the per-Session reverse forward a Remote Session's agent uses
  to reach the seat PC.

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
| `shell` | `Pwsh`, `WindowsPowerShell` or `Posix` | Enum; Test fills it in |
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

The remote line carries a script as base64, so the line itself is letters,
digits and `+/=` plus a fixed wrapper. Whatever login shell sshd runs first
never sees a quote or a space from the user's data.

| `shell` | Remote line | Script |
|---|---|---|
| `Pwsh` / `WindowsPowerShell` | `pwsh -NoLogo [-NoExit] -EncodedCommand <b64>` (or `powershell`) | PowerShell, UTF-16LE; quoting rule: single quotes, `'` doubled |
| `Posix` | `sh -c "$(printf %s <b64> \| base64 -d)"` | POSIX sh, UTF-8; quoting rule: single quotes, `'` as `'\''` |

Both scripts do the same steps:

1. Change to the mapped directory and stop with the error on screen if it
   does not exist. Never start an agent in the wrong place. A blank mapping
   skips this step (home).
2. Export `FOREMAN=1`, `FOREMAN_TERMINAL_ID`, `FOREMAN_PROJECT_ID`,
   `COLORTERM`, `FOREMAN_URL=http://127.0.0.1:<rport>`, `FOREMAN_TOKEN`.
3. Run the logical argv, each element a quoted literal. A Shell launch has
   no argv and leaves an interactive shell; an agent or Dispatch ends with
   its process, the same as locally.

On `Posix` step 3 runs through the user's own login shell with its login
and interactive startup files, not bare `sh`:
`exec "$SHELL" -lic 'exec "$@"' foreman-launch <argv…>`. An SSH command
line otherwise skips those files, and an agent installed under the user's
home is then not on PATH even though it works when the user logs in by
hand (seen on the test Remote: `codex` was missing from the bare PATH and
found this way). Passing the argv as positional parameters also means no
second quoting layer: each element is quoted once, by the one rule.

Not injected: `FOREMAN_PIPE`, `FOREMAN_TITLE_PIPE`, `FOREMAN_EXE` (seat-PC
names that mean nothing there) and `KITTY_WINDOW_ID` (graphics are a
non-goal).

**Length cap.** If a Windows Remote's login shell is cmd, the whole remote
line must fit cmd's 8191-character limit, and the encoding costs about 2.7
line characters per script character. The builder refuses a Windows launch
over the cap with an error that says to open without the prompt and send
the brief by chat. It never truncates. `Posix` has no practical cap.

**Directory.** The first launch on a Remote from a Project with no mapping
asks once in a small text prompt and saves the answer to `Remote::dirs`.
The prompt names what it is about to open and where ("Split right: open a
shell on desk"), and says the answer is changed in Settings > Remotes, whose
edit form lists each Project directory with its Remote directory to edit or
clear. Splits always use the mapping, not wherever the Remote shell has
`cd`'d to (there is no OSC 7 tracking).

**Folder list.** Directly under the prompt's Directory field, and directly
under the edit form's focused Directory row, a list of the Remote's folders
saves typing a path blind. Folders only, never
files. The field stays the source of truth: the list shows the folders in
the directory the text names up to its last separator, filtered by the
case-insensitive prefix after it. Choosing one writes `dir/name/` into the
field and lists the next level; `..` goes up (above home it uses where the
listing found home); home is written `~/…`. The area is a fixed height in
every state (listing, listed, empty, cut, failed, retry), the prompt's list
gives up rows rather than push its buttons off a small window, and the field
and the list never both look focused. It has no fill: a 1 px border that
turns the focus colour while the list has the keyboard. Rows are 22 px,
folder names in the UI font, the breadcrumb's path in monospace; the
selected row is shown by the selection wash alone, and hover washes as the
launcher's rows do. Its breadcrumb names the directory
(in the edit form also whose: "Folders for app on desk in /srv/") and says
"first 500" when cut. Keys: Down from the field enters the list, and while
the directory is still listing it waits there and selects the first folder
on arrival; Up/Down move; Enter or Right goes in; Left, or Backspace with no
filter, goes up; Tab, Esc, or Up from the first row returns to the field,
and typing in the list goes to the field. Enter in the field opens (or
saves) whatever is typed, as before, and never descends; Esc there cancels.
Each listing is one `ssh -T -o BatchMode=yes` run off the UI thread with a
15 s deadline, at most two at once, debounced 300 ms while typing, killed
when the prompt or form closes. The Remote's answer (folders, or missing, a
file, denied) is cached per (target, shell, directory) for the life of the
prompt or form. A failure to reach the Remote (an ssh error, a timeout, a
login that needs a password) is shown with a retry row and asked again when
the directory or Remote changes or Down enters the list. Every failure says
what to do next, with ssh's own words only in a hover. After 3 s a listing
says it is still trying. The typed path rides only inside a base64 script,
quoted once. Posix runs plain `sh`: `cd`, then each entry that is a
directory (dot-folders included, sorted after the rest), at most 500; a
name or resolved path holding a line break is skipped or refused. A name
that is not UTF-8, or holds a control character, is shown dimmed and cannot
be chosen, and control and bidi characters are painted as a placeholder.
PowerShell uses `Get-ChildItem -Directory -Force` and lists the drive roots
beside home, where a bare drive is typed; that script is untested (no
Windows Remote exists), and a line past the Windows command-line cap says so
instead of running. Nothing is copied to or installed on the Remote.

**New folder.** The one thing the folder list changes on the Remote, and only
by an explicit action: the first row after `..` is "+ New folder" (its `+`
in the gutter no folder uses, and its key Ctrl+N shown while the list has the
keyboard, so it reads as an action, not a folder, by more than colour),
offered only over a listing. Enter or a click on it, or Ctrl+N while
the list has the keyboard, turns that row into an empty name field in the same
slot; the list's height never changes. A bare letter never opens it: letters
typed in the list go to the field as a filter. The breadcrumb then says where:
"New folder in ~/Projects on falador". Enter creates only once the field has
been drawn and holds a name the user typed there (and only a fresh press: a
held Enter's repeats never create twice, and never then open or save); Esc
goes back to the list, the next Esc to the field. The name is one path
component: not empty, not only spaces, no space at either end (refused with
the reason, never trimmed), not `.` or `..`, no `/` (nor `\` or `:` on a
Windows Remote, nor `<>"|?*`, a trailing dot, or a device name such as CON,
NUL, COM1 or LPT1 with or without an extension), no control character, at
most 255 bytes; a leading `-` is fine. A bad name is refused at once, in
words, and nothing runs. The create is one `ssh -T -o BatchMode=yes` run off
the UI thread with a 15 s deadline, the parent and name only inside the
base64 script: Posix `cd -- parent` then `mkdir -- name`, one level, never
over anything already there; PowerShell finds the parent with `-LiteralPath`,
pins it as the process's current directory (so it can't vanish), and makes the
folder with .NET `CreateDirectory`, which is literal and, with the parent
pinned, one level (untested on a Remote, like the rest of the Windows path).
One create runs at a time per picker: while it runs the breadcrumb says so,
and if its name field was closed the row says it is busy and opens nothing.
Closing the prompt or form does not stop it. A refusal (already taken,
permission denied, the folder gone, a login that needs a password, a Remote
that could not be reached) shows in the danger colour on the breadcrumb line,
saying what to do, with the name kept for another try. A create whose answer
never came back (timed out, cut off) may have made the folder: the parent is
listed again and the breadcrumb says the folder may have been made. On
success the parent's and the new folder's listings are dropped from the
cache, the field holds the new folder (`~/Projects/name/`) and gets the
keyboard back, and the list shows the new, empty folder, so Enter opens or
saves there. Nothing is created by typing a path that does not exist, opening
a Session, or saving the form.

## The way back

One loopback listener per foreman instance, bound to `127.0.0.1` on an
OS-assigned port, started when the first Remote Session spawns. Each Remote
Session forwards its own remote port (random within a fixed range) to it.
`ExitOnForwardFailure` turns a port collision into an immediate, visible
exit; reconnect picks a new port.

A forwarded loopback port is reachable by every process on the Remote, so
the port is not the credential. At spawn foreman mints a random token from
the OS RNG and records token → (Project id, terminal id). Every request
carries it as `Authorization: Bearer`. A token dies with its Session.

The listener speaks just enough HTTP/1.1 for `curl`: a request line, the
`Authorization`, `Content-Length` and `Expect` headers, a body. Hand-parsed,
no new dependency. It is a separate front door onto the same GUI channel
the pipe uses (`CtrlMsg`); the pipe protocol and its request structs are
untouched, so v1 wire compatibility holds by construction.

| Request | Does |
|---|---|
| `POST /chat` (body is the text; `to`, `re`, `now` as query) | Posts as the token's Session down the normal Member path, not `ChatRoom::post_remote` |
| `GET /chat?history=N` | Last N lines |
| `GET /status` | The Project's Sessions, read-only |
| `POST /hook?agent=…&event=…` | An agent-state event for the token's Session |

Anything else is refused. There is no route for `open`, `send`, `close`,
`snapshot` or kanban. Guards match the peer listener: a body cap, one
per-connection deadline, bounded in-flight handlers.

Gotchas the listener must handle:

- **`Expect: 100-continue`.** `curl` sends it for larger bodies and stalls
  for a second if the server stays silent. Answer it.
- **`curl` on Windows PowerShell 5.1 is an alias**, not the program. Every
  command foreman hands a Windows Remote says `curl.exe`.

**Teaching the agent.** The Remote's agent has no foreman skills. The first
chat delivery to a Remote Session, and the first after a reconnect, carries
a short note with the exact reply command for that Remote's shell. A
Dispatch brief built for `--on` carries the same note. It is sent once per
connection, not on every message.

A first launch of `claude` or `codex` on a Posix Remote whose prompt
argument foreman can name for certain (`remote::note_route`, over
`remote::prompt_index`: the bare program, then the brief) carries the note
at the end of that argument and types nothing. Keystrokes at launch can
answer a dialog the agent shows first, and Claude's folder-trust dialog
defaults to "No, exit". That note names `$FOREMAN_URL` and `$FOREMAN_TOKEN`,
which the launch exports. Both agents' tool shells were seen to have them.
A brief that starts with `/` never carries it: it would become the slash
command's arguments. A Claude launched with hooks that foreman cannot
carry the note for (a slash brief, a one-word brief, `-p`, a path) and a
reconnect of one (its resume argv has no prompt, and its token is new) get
it typed behind the chat hold below, with the values written out. Every
other agent (Grok, Codex in another shape, Claude with the user's own
`--settings`, every Windows Remote, a bare agent, a resumed hookless one)
gets it with its first chat delivery. Nothing is typed into them at
launch: no signal says a startup dialog is gone.

A Remote Session's first launch joins the room at its end, so it gets no
catch-up of earlier posts. A reconnect keeps its Member and cursor. A
Claude launched with hooks also holds every post and typed note until a
hook event says it is past the dialog: its brief's `UserPromptSubmit` (one
word or many, bare or by path), or for a bare launch, `--continue` and a
slash command, a fixed settle after `SessionStart` so the REPL has mounted
(Results, 2026-11-24). Nothing opens the hold before an event, and no
keystroke opens it: the human's first keys answer the dialog. While it
holds, `status --json` carries `chat_held: true`. Once the hold has lasted
`remote::HELD_CUE_AFTER`, the pane shows a cue on its bottom line with one
action, "deliver now", which opens it. So does the local-only
`foreman deliver tN`, for an agent whose events never come.
A hookless agent has no hold: its posts go at Ready, so a post sent while
its own startup dialog shows can answer it. Holding it with no signal would
stall unattended dispatch into trusted directories.

Accepted risk: the token sits on a command line (base64, not secret) on
both PCs, so another process running as the same user can read it, and on
Linux any user can read a process's arguments while the launch shells run.
It buys posting chat as that one Session, and feeding its badge, until the
Session exits. The local pipe already trusts same-user processes with more
than that. The agent's own argv never holds the token: an argv lasts the
agent's life, where the launch line lasts only while it runs.

## Agent state

Locally the badge comes from hooks foreman installs in each provider's
config. On a Remote nothing is installed, so the hooks travel with the
launch: where a provider accepts hook settings on its command line, the
builder adds them, and each hook is one `curl` to `/hook` with the event
name. `WindowManager::apply_hook_event` already matches on Project id,
terminal id and icon; the token supplies the first two and the logical argv
supplies the icon.

Per provider (see Results):

- **Claude** takes hook settings as JSON on `--settings`. Confirmed to fire
  through the Tunnel. The launch adds `SessionStart`, which maps to no
  state and only opens the chat hold. Claude keeps only the last
  `--settings` it is given, so a launch with the user's own gets no hooks
  and no hold (Results, 2026-11-24, the hold for every hookable Claude).
- **Codex** reads hooks from a file and only runs ones the user has trusted.
  The one launch-time route is its trust-bypass flag. Foreman does not pass
  that flag on its own; Codex gets no badge on a bare Remote in v1.
- **Grok** is unverified.

A provider with no per-launch hooks gets no badge on a Remote Session, and
its chat delivery behaves as it does for any Session with no state.

`/hook` answers with an empty body, and the hook's `curl` discards output
anyway. A `UserPromptSubmit` hook's stdout is fed into the agent's context,
so a reply line there would land in every prompt. A hand-typed
agent in a Remote shell has no hooks either, and gets its icon from the OSC
title only; the process-tree scan sees `ssh.exe`.

The local helper filters subagent events using the hook's stdin payload
(`title_notify::normalize_event`). Whether the remote hook forwards enough
of that payload, within the body cap, to keep the same filtering is part of
experiment 3. If it cannot, a subagent finishing may mark its parent idle
early on a Remote Session; that is recorded as a gotcha, not papered over.

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
Past the width that fits, the strip scrolls sideways (Shift+wheel or drag),
fades at an edge with more past it, and long names are cut with "…" (full
name in the tooltip). Built that way instead of a dropdown: a dropdown
inside a hover menu is a second popup that closes the first. One dim line
under the strip says the selection's state in words ("desk: offline — no
answer on port 22"), and under a Remote the headers read "AGENTS ON DESK" /
"SHELLS ON DESK".

**Reachability.** `ssh -G <target>` resolves the real host and port once per
target; a TCP connect with a short timeout answers reachable or offline.
Both run off the UI thread, only while the launcher or the Remotes pane is
open, cached for a few seconds. An offline name is dimmed and stays
clickable, because the probe cannot see through `ProxyJump`.

**Globe.** A small globe overlays the corner of the tab icon and the
Sessions-panel row icon; the agent logo stays. Tooltip: name and target.
One white-silhouette SVG in `assets/icons/`, tinted like the rest.

**Splits inherit, but never redirect.** A split from a Session whose Remote
was deleted, or whose name now points at another target, opens Local with a
toast saying so.

**Splits inherit.** Splitting or adding a terminal while a Remote Session is
focused opens on that Remote. Anything else opens Local. A Remote is never
chosen by a default. The launcher's own "+" (titlebar and Sessions panel)
follows the where-strip instead, not the focused Session: the strip is on
screen under the pointer when it is clicked, so that is what the user sees
choosing.

**Settings → Remotes.** A custom-body pane like Keybindings, in the
settings rows' grammar (row height 46, left pad 18, label over a dim
description, controls right-aligned). One item per Remote: name over
`target · shell`, its reachability and Test / edit / delete on the right,
then its status line and agents line, each a fixed line; the selection wash
covers all three. Editing expands the item in place into ordinary rows on
the pane background, marked only by a 2 px focus bar down its left edge (the
rail's idiom), never a filled block: a header (the name, or "New Remote",
with save and cancel where delete and keep sit), the reserved error line,
Name, Target (monospace) with the reserved ssh-config suggestion line under
it (outline chips), Shell as an outline choice chip, then under a "PROJECT
DIRECTORIES" section header one row per mapping: the Project folder's name
over its seat-PC path (cut from the front, whole on hover), the Directory
field ("(home)" when blank) and clear. Enter saves and Esc cancels from any
field. The pane has no title or hint of its own: the settings title band
names it, and the settings footer carries the keys for whatever has the
keyboard (browsing, a field, the folder list, the new-folder name, a
confirm), supplied by the pane. **Test** runs one
`ssh -o BatchMode=yes` probe off-thread and reports: login works without a
prompt (or "needs an interactive login: open a Remote Session on it once
and log in there"), which
shell it found, whether `curl` is there, which agent CLIs are on PATH.
Deleting a Remote does not touch live Sessions; each carries its own copy
of the name.

**Installing an agent CLI.** Each row has a second fixed line for the agent
CLIs, there before the first Test so a result never moves a row: the words
on the left, groups split by "; " ("has claude, codex; grok needs bash"),
and each installable agent's "install grok" button (or one "retry" after
"did not install") in the line's right-hand cluster. "grok needs bash" means the installer needs a tool Test did not
find; foreman installs no OS packages. The other words are "grok: install
by hand" (no command for that shell), "grok installing…", "grok checking…"
and "grok installed, needs login". `I` or the button opens a confirm in the
row, shaped like delete's: "Install grok on desk?" with install and cancel,
and the exact command in the room of the two lines under it. ←/→ picks
another missing agent. Enter opens the vendor command as an ordinary Remote
Session in the Remote's home, in the focused Project. Settings is a Win of
its own, so that is the visible Project focused last; with none open it
says one must be. When the Session's `ssh` ends, or its Win or Project
closes, Test runs again. The disconnect bar says how the install ended, and
Enter there opens a shell on the Remote to log in, never the installer
again. Nothing installs on Test, launch or connect.

The commands are one table, `remote::INSTALL`: one per (provider, Posix or
PowerShell), the vendor's own native installer, with its page beside it.
The Posix ones fetch with `curl`, and Claude's and Grok's pipe to `bash`, so
Test also reports `bash`. None uses npm, so nothing needs Node. Every
PowerShell entry is untested.

In the launcher, under a Remote whose last Test this run misses an agent,
that agent's row is dimmed and says "not installed" (or "installing…").
Choosing it opens a desktop prompt with the same question, note and
command, or the same plain message when there is nothing to run. A Remote
not tested this run keeps its rows as they were. Test answers and installs
are not saved across runs.

**Disconnect.** When a Remote Session's `ssh` exits, the Win stays, shows a
one-line "Disconnected from desk — Enter to reconnect" bar, and writes
nothing to the PTY. A danger dot on the top corner of the tab and row icon
says the same thing where the title is cut. Selection, copy, scrollback and
search stay live, so
the ssh error can be copied. A fresh Enter (not key repeat) or a click on
the bar relaunches the same Remote and directory; the tab title and the
Sessions-panel row read "disconnected" until then. An agent Session
relaunches with that provider's resume argv from one small table; a
provider with no entry, or an agent launched through `npx` or `wsl`,
starts the bare provider fresh. "Is an agent" is the program itself
(`remote::program_agent`), not the tab icon's scan, so a command that only
names an agent (`vi codex.md`) runs again as it was, like any other
command. An agent's Dispatch prompt is never sent again.

**Inert on a Remote Session:** file-path links (URLs still open), file
drops and image paste (refused with a toast). The close confirm names the
Remote instead of listing a process tree it cannot see.

## Control plane

- `OpenRequest` gains `on: Option<String>`, absent on the wire when `None`,
  with a compat test beside the existing ones. `--cwd` with `--on` is a
  directory on the Remote and is not saved as the Project's mapping (the
  Session's own snapshot keeps it). No mapping and no `--cwd` is an
  error that says which one to supply.
- `foreman remotes` lists name, target and last known reachability in
  `OpenReply::history`. Local callers only.
- `status` marks Remote Sessions with the Remote's name, in text and
  `--json`.
- `foreman reconnect tN` is a new request kind (`ReconnectRequest`) that
  does what Enter on the disconnect bar does, and is refused for anything
  but a disconnected Remote Session. Local pipe only: not a peer verb, and
  the Tunnel has no route that builds it.
- The dispatch and chat skills gain a short "another PC" section using
  `--on`; both are embedded, so the Codex twins and a rebuild go with it.

## Persistence

`ContentSnap::Terminal` gains an optional Remote name, and for a Remote
Session the agent's provider name (never its prompt or other arguments)
and the Remote directory, so a restored agent resumes and an explicit
`--cwd` survives a restart. All three are absent when local, so a local
Terminal saves byte-identically. Restore creates the Win **in the
disconnected state** and connects on Enter. Restoring must not open a batch of SSH connections at launch, some
to PCs that are off and some asking for a password. A name that no longer
exists restores as a local Shell with a toast.

## Experiments that gate the build

1. `ssh -R` from the seat PC to a Windows Remote (inbox OpenSSH) and to a
   Linux one, and a `curl` back through each. Failure reopens the way back.
2. The encoded launch line: through a cmd login shell and a PowerShell one
   on Windows, and through bash and zsh on Linux; into a directory with
   spaces; starting each agent CLI, including one installed through a
   version manager. Failure reopens the builder.
3. Per-launch hook settings for each agent CLI: does the flag exist, do the
   hooks fire, and how much of the payload the subagent filter needs.
   Decides which providers get a badge on a Remote.
4. Resize and the resize-then-Up-arrow case, Windows Remote (two ConPTYs)
   and Linux Remote (one). Record it; `docs/conpty-resize-reflow.md` is
   settled and this is not a reason to reopen it.
5. Kill the local `ssh.exe` and check the Remote for orphaned agent
   processes.
6. Sleep the seat PC mid-run. Time how long the keepalive takes to end the
   Session, and confirm the resume argv recovers the conversation.
7. Password and host-key prompts against the Ready gate: typed input must
   reach `ssh` before the remote shell has ever answered a DSR.

## Results, 2026-11-20, one Linux Remote

Run by hand from the seat PC (Windows, inbox OpenSSH client) against a
Debian machine with bash as the login shell. No Windows Remote was
available; every Windows half of the experiments above is still open.

- **1, Linux half: passed.** `ssh -R` plus `curl` with a bearer header
  reached a listener on the seat PC, for a short post, a 2000-byte post and
  a GET. That `curl` sent no `Expect` header at 2000 bytes; versions differ,
  so the listener still handles it.
- **2, bash only: passed.** The base64 line changed into a directory
  containing spaces and a `'`, exported the variables, kept a tty, and
  delivered argv elements containing spaces, both quote kinds and a `$`
  unchanged. A missing directory stopped the launch with exit status 1.
  zsh was not tried.
- **2, PowerShell quoting, seat PC only (no SSH).** The builder's encoded
  lines run through local cmd → pwsh 7 and cmd → Windows PowerShell 5.1 into
  a program printing its argv. pwsh passed everything. 5.1 drops embedded
  `"` and empty arguments when calling a native program, so the
  `WindowsPowerShell` script pre-escapes argv for the program's own parser.
  An argument where every space follows an odd number of `"` (`"a b"`)
  cannot get through 5.1 at all; the builder refuses it and says to use
  pwsh or chat.
- **3, Claude: partly passed.** `--settings` with inline hook JSON fired
  `UserPromptSubmit` through the Tunnel with the token from the launch
  environment. `Stop` and `PostToolUse` were not observed because Claude on
  that machine was not logged in, so the turn never ran.
- **3, Codex: not run.** It needs the trust-bypass flag; see Agent state.
- **3, Grok: not run.** Not installed there.
- **Real pane, existing build.** `foreman open -- ssh -t …` to the same
  machine: the Session latched Ready, `send` and `snapshot` worked, and a
  targeted chat post was delivered into the pane. An interactive Codex
  started through the base64 line drew its full TUI. This is the core claim
  of the design, shown with no new code.
- **5, Linux half: passed.** Closing the pane left no process behind on the
  Remote, for a plain `sleep` and for Codex.
- **7, key login only: passed.** Ready latched with no prompt in the way.
  Password and host-key prompts were not exercised.
- **4 and 6: not run.** Resize needs a human at the window; sleep needs the
  seat PC to sleep.

## Results, 2026-11-23, Claude hooks through the Tunnel

A release build of the `/hook` work ran as its own instance (own `APPDATA`,
agent environment stripped), driven through its control pipe against the
same Debian Remote, Claude Code 2.1.331 (logged in), in a throwaway
directory. State read from `status --json` and the screen from `snapshot`.

- **3, Claude: passed.** Prompt submitted → `working` (20:18:11.7), held
  across a `Bash` call, → `idle` 2.4 s later on `Stop`. A `Write` in
  default permission mode → `needs_you` on `PermissionRequest`
  (20:19:57.7); approving → `working` (20:20:26.05), which only
  `PostToolUse` can cause there, → `idle` 0.7 s later on `Stop`.
- **No state where none belongs.** A plain Remote `bash` and a `claude
  --settings <own file>` never carried `agent_state`. The folder-trust
  dialog fires no hook, so it shows no state, not Needs you.
- **Experiment 3's subagent question: answered.** Captured raw payloads (a
  second Claude with its own capture `--settings`): a subagent's
  `PermissionRequest` and `PostToolUse` carry `agent_id` and `agent_type`;
  its end is `SubagentStop`, never `Stop`. Main-thread `UserPromptSubmit`
  and `Stop` carry no agent fields. So a subagent cannot mark its parent
  idle. A backgrounded subagent finishing makes Claude start a main turn
  with a `<task-notification>` prompt (`UserPromptSubmit`, then `Stop`), so
  the row goes working → idle again on its own.
- **Nothing installed on the Remote.** `~/.claude/settings.json` kept its
  mtime and sha256; no foreman file appeared.
- **Not shown live: reconnect.** `send` cannot press Enter on a disconnect
  bar, and driving the GUI was out of bounds. Unit tests cover the hooks
  and the new token on a reconnect.
- **Found: the at-launch reply note answers the trust dialog.** A Claude
  dispatched with a prompt into an untrusted directory exits: the note's
  Enter picks "No, exit". Bare `claude` gets no note at launch and was used
  instead. Not fixed here; see `docs/remote-sessions.md`.
- **Codex, Grok, Windows Remotes: not run.** No hooks are built for them.

## Results, 2026-11-23, the reply note inside the prompt

A release build ran as its own instance (own `APPDATA` holding only the
Remotes settings plus one empty Project, agent environment stripped), driven
through its control pipe against the same Debian Remote. It used Claude Code
2.1.331 and codex-cli 0.163.0, each in a new `/tmp` directory neither had
seen, removed after.

- **(a) Nothing typed at the trust dialog: passed.** `claude "<brief>"`
  opened at 22:23:23.6. At 22:23:36.9 and again at 22:24:08.0 (44 s) it sat
  at "Accessing workspace" with "❯ No, exit" selected, `state: running`.
  The brief and the note sat in its argv (`/proc/<pid>/cmdline`), the note
  after a blank line.
- **(b) Answered by hand, it ran the brief: passed.** `send --keys Down`,
  then `Enter`, chose "Yes, I trust this folder". Claude ran one `curl` from
  its prompt, and the room showed `#2 t2: s2hp4s-ok`. `agent_state` went
  idle.
- **(c) No second note: passed.** A post to it arrived as the bare
  `[chat p1 #4] t1→t2: …` line. The only `[foreman]` text on screen was the
  prompt's. It answered with `#5 t2 (re #4): s2hp4s-second`.
- **Found: room catch-up still answers the dialog.** A second Claude,
  opened the same way into a room that by then held broadcasts #2 and #5,
  got them typed on its first Ready frame (a new Member's cursor starts at
  0). It exited within 6 s: "Claude Code didn't start because this folder
  isn't trusted." Not fixed here. The note no longer causes it, but any
  earlier post does.
- **Codex: the note arrived, the post did not.** No trust dialog showed. The
  carried note was in its prompt, and nothing but the room catch-up was
  typed. Its `curl` failed ("Failed to connect"), while a plain ssh shell on
  the same port got 401. With no `sandbox_mode` in its config, its tool
  sandbox blocked the call. So (b) is not shown for Codex.
- **Token exposure measured.** The carried note's token is in the agent's
  world-readable cmdline for its life. A Claude on the same Remote launched
  without a carried note showed only `$FOREMAN_TOKEN` there.
- **Not run: reconnect** (it still types the note; unit tests pin it),
  **Grok** (not installed on the Remote) and **Windows Remotes** (none
  exist). Their prompt carrying and the line-cap fallback are unit-tested
  only.

## Results, 2026-11-23, no catch-up, the chat hold, no token in argv

A release build ran as its own instance (own `APPDATA`, agent environment
stripped), driven through its control pipe against the same Debian Remote:
Claude Code 2.1.331, codex-cli 0.163.0. Every agent ran in a new `/tmp`
directory, removed after along with its trust entry. Posts came from the
instance's local probe pane.

- **Before building: no hook fires at the trust dialog.** This used a
  hand-made `--settings` that logged every event to a file. With a prompt
  and bare, the log stayed empty for 40 s at the dialog. `SessionStart`
  came 0.3 s after the answer, then the prompt's `UserPromptSubmit` 0.4 s
  later. `claude --continue` in a trusted directory sent `SessionStart`
  0.7 s after launch with no prompt.
- **Found, and fixed before (a)–(c): `SessionStart` is too early for a
  brief.** With the hold opening on `SessionStart`, a post held through the
  dialog was pasted while the REPL mounted and lost: no `[chat …]` turn in
  the transcript, and Claude only answered it from `GET /chat?history`. Two
  more runs (one briefed, one bare) kept theirs, after the tty had echoed
  it. A launch with a brief now waits for that brief's `UserPromptSubmit`.
- **(a) No catch-up, nothing typed: passed.** Broadcasts #4 and #5 were in
  the room. `claude "<brief>"` opened at 23:19:55.7. At 23:20:25.9 (30 s) it
  sat at the dialog with "❯ No, exit" selected, `state: running`,
  `ready: true`, nothing typed.
- **(b) A post during the dialog is held: passed.** #7 was posted at
  23:20:32.3. At 23:21:03.4 Claude was still at the dialog, alive, nothing
  typed.
- **(c) Held, then delivered in order: passed.** `send --keys Down`, then
  `Enter` at 23:21:09.6. The transcript shows the brief submitted at
  :10.7, its `curl` with `$FOREMAN_TOKEN` and `$FOREMAN_URL` at :13.2
  (room `#8 t4: a3q-done`), then `[chat p1 #7] t2: …` typed at :15.6 and
  answered `#9 t4→t2 (re #7): a3q-heard`. #4 and #5 never reached it. A
  bare `claude` held a post through 30 s of dialog. After the answer, the
  post arrived with the reply note, and it replied `#12 t5→t2 (re #11)`.
- **(d) No token in argv: passed.** The token came from decoding the
  instance's own `ssh.exe` launch line, where it is held once, in its
  `export`. On the Remote the agent's `/proc/<pid>/cmdline` (mode 444) held
  0 copies, and `ps -eo args` held 0. Its environment held 1. The prompt's
  note read `-H "Authorization: Bearer $FOREMAN_TOKEN" … "$FOREMAN_URL/chat?to=tN"`.
- **(e) Codex: the note rides in its prompt, as variables.** Before
  choosing, a Codex in a Remote shell printed both variables from its tool
  shell. Opened as `codex "<brief>"`, nothing was typed: no note and no
  catch-up. Its `env | grep -c ^FOREMAN_TOKEN=` printed 1, and its
  cmdline and every process in its directory held 0 copies of the token.
  Its `curl` still failed ("Failed to connect"): its own sandbox, as
  before. Codex's post is not shown.
- **Not run: reconnect live** (`send` cannot press Enter on a disconnect
  bar; unit tests pin the fresh hold and its `SessionStart`), **Grok** (not
  installed; typed note, unverified) and **Windows Remotes** (none exist;
  typed note, unverified).

## Results, 2026-11-24, the settle after SessionStart and `foreman reconnect`

Release builds ran as their own instances (own `APPDATA`, agent environment
stripped), driven through their pipes against the Debian Remote, Claude
Code 2.1.331. Every run used a new `/tmp` directory. A post counts as
delivered when the transcript holds it as a typed prompt or as a prompt
queued while Claude was busy. BEFORE is main plus the new verb; AFTER adds
the settle and `remote::hooked_claude`.

- **R1** bare `claude`, a broadcast posted while the trust dialog shows,
  the dialog answered with `send`. **R2** the same with `claude "<brief>"`.
  **R3** a one-word brief. **R4** launched by path
  (`/home/andy/.npm-global/bin/claude "<brief>"`). **R5** a bare Claude that
  had posted once on its typed note, its `ssh.exe` stopped by PID, then
  `foreman reconnect`, then asked in the room to post back.
- **R1e / R5e**: R1 and R5 with a held post of 1150 emoji, 4.6 KB on one
  line. For R5e it was posted while the Session was disconnected.

| Run | BEFORE runs | lost | AFTER runs | lost |
|---|---|---|---|---|
| R1 | 10 | 0 | 20 | 0 |
| R2 | 10 | 0 | 10 | 0 |
| R3 | 10 | 0 | 20 | 0 |
| R4 | 10 | 0 | 10 | 0 |
| R5 | 10 | 0 | 20 | 0 |
| R1e | 5 | 5 | 20 | 0 |
| R5e | 5 | 1 | 10 | 0 |

- **What was found.** The short loss the card described did not reproduce
  through foreman: 50 of 50 held posts and notes arrived. In every R5 run
  the fresh note arrived and the agent posted back on the new token. The
  same emoji post sent to a mounted REPL arrived 3 of 3. So the loss is
  timing plus length: a held line over 4095 bytes released at
  `SessionStart` lands in the cooked tty before the REPL mounts, and Linux
  drops it.
- **Measured with a pty on the Remote, no foreman** (53 launches: bare,
  one-word brief, multi-word brief, `--continue`). After the answer, the
  dialog turned bracketed paste, focus reporting and `?2031` off about 0.2 s
  before `SessionStart`. The REPL turned them back on 0.098-0.185 s after it
  and painted its footer at 0.180-0.267 s. A short paste sent 0-700 ms after
  `SessionStart` arrived 50 of 50. A 6000-byte one-line paste was lost at
  +0 and arrived at +400 ms; 3000 bytes arrived at +0. Hence
  `START_SETTLE` = 2 s, 7.5x the slowest paint. Bracketed paste is already
  on at the dialog, so that mode is no signal of a mounted REPL.
- **Slash briefs**: `/cost` and `/context` fired no `UserPromptSubmit`,
  `/init` did. A slash brief therefore settles like a bare launch.
- **Undercounted, then fixed**: six AFTER R3 runs first read as
  lost. Their posts were in the transcript as `queued_command` attachments,
  queued while Claude worked its brief. Every run in both tables was
  recounted with both forms. Each input arrived exactly once, except
  BEFORE R1e and R5e's losses.
- **Not run**: Grok (not installed), Windows Remotes (none exist). The
  leading-space slash brief (`"  /cost"`) was classified after the AFTER
  build and is unit-tested only.

## Results, 2026-11-24, the hold for every hookable Claude, shown and released

Release builds ran as their own instances (own `APPDATA`, agent environment
stripped), driven through their pipes against the Debian Remote, Claude
Code 2.1.331. Every run used a new `/tmp` directory, its trust dialog
answered with `send --keys "Down Enter"`. Posts came from the instance's
local probe pane. A post counts as delivered when the transcript holds it
as a typed prompt or a `queued_command` attachment. BEFORE is main at
afc3660; AFTER adds this change.

- **P1** `claude --settings '{}' "<multi-word brief>"`, a Claude with its own
  settings and so no hooks. **P2a** a bare Claude by path through a wrapper
  that points `$FOREMAN_URL` (which the hooks read) at a closed port, so no
  hook reaches the Tunnel; the typed note writes the real URL out, so the
  agent can still reply. **P2n** the same with a `PATH` holding no `curl`,
  for that launch only. **P2b** `claude agents`, which keeps a live input
  box, sent `SessionStart` and never `UserPromptSubmit` in a probe, and so
  holds as a brief forever. Each P2 run posted after the answer and waited
  60 s; AFTER then ran `foreman deliver`. **S** `claude "/context  now"`, a
  slash brief with whitespace.

| Run | BEFORE runs | failed | AFTER runs | failed |
|---|---|---|---|---|
| P1 | 5 | 5 | 5 | 0 |
| P2a | 2 | 2 | 3 | 0 |
| P2n | 1 | 1 | 1 | 0 |
| P2b | 1 | 1 | 1 | 0 |
| S | 1 | 1 | 1 | 0 |

- **BEFORE P1**: the typed note's Enter chose "No, exit" within 12 s in
  every run: `state: exited`, "Claude Code didn't start because this
  folder isn't trusted."
- **BEFORE P2a, P2n, P2b**: 60 s after the post the input box was empty,
  no transcript existed (nothing was ever submitted), and `status --json`
  showed nothing but `ready: true`. Nothing said chat was waiting.
- **BEFORE S**: the decoded launch argument was `/context  now`, a blank
  line, then the note: the note rode inside the slash command, whose
  `<command-args>` held it. Claude still read it there and answered a
  later post, so the harm is shown for the argument, not for `/context`.
- **AFTER P1**: at 30 s each sat at the dialog, alive, nothing typed. After
  the answer a post carried the note and each replied.
- **AFTER P2**: `chat_held: true` from the dialog through 60 s, with no
  transcript. `foreman deliver` answered ok and the field was gone. P2a and
  P2n: both posts arrived once each, in order, the first typed with the
  note and the second queued, and Claude answered both. P2b: the note and
  the post each became one background task, once each; their order is not
  observable there, since each runs in its own process. A second `deliver`
  was refused, "holds no chat", every time.
- **AFTER S**: the argument was `/context  now` alone. `/context` ran with
  `now`, the note came 3 s later as its own prompt (after `SessionStart`
  and the settle), then the post, and Claude replied.
- **Found before building: Claude keeps only the last `--settings`.** In
  `claude -p` runs with hooks logging to a file, two `--settings` ran only
  the last one's hooks, and hooks followed by `{}` ran none. So foreman's
  hooks cannot ride beside the user's own, and that launch is hookless.
- **Not shown**: the held cue in the GUI (a click and its layout are
  unit-tested; how it looks needs a human), Grok (not installed), Windows
  Remotes (none exist), a dropped hook event followed by a later one
  (unit-tested: every event but `SessionStart` opens either hold).

## Results, 2026-11-24, the note before carried posts, the hookless launch note, the cue

Release builds ran as their own instances (own `APPDATA`, agent environment
stripped), driven through their pipes against the Debian Remote (Claude Code
2.1.332, Codex 0.163.0 then 0.172.0, see below). Every run used a new
`/tmp` directory. BEFORE is 4934798; AFTER is this change. A post counts as
delivered when the transcript holds it as a typed prompt or a queued one.
Agent-state badges were off (the default): the room then hands a Session
its whole backlog in one tick, and posts 2..n wait 500 ms each in the
Session's own queue, which is what a drop carries over.

- **H**: Claude by path with a brief (held, note typed), 6 posts held
  through the trust dialog, the dialog answered with `send`, its `ssh.exe`
  stopped by PID about 1 s after the hold opened, then `foreman reconnect`.
  **C**: `codex -c model_reasoning_effort=low "<brief>"` (a shape
  `prompt_index` refuses), its brief asking it to report READY at once, 60 s
  left alone, then 6 posts, `ssh.exe` stopped 0.9 s after the first, then
  `foreman reconnect`.

| Item | Run | BEFORE | AFTER |
|---|---|---|---|
| 1 | H, carried posts answered with the dead token first | 3 of 3 | 0 of 3 |
| 1 | H, fresh note ahead of every carried post | 0 of 3 | 3 of 3 |
| 1 | C, fresh note ahead of every carried post | 0 of 1 | 2 of 2 |
| 2 | C, can report before anyone posts to it | 0 of 2 | 2 of 2 |
| 4 | normal launch (trusted directory), pane resized by the cue | 9 of 9 | 0 of 8 |
| 4 | held at the dialog past the threshold, failures | not run | 0 of 10 |

- **H BEFORE.** The resumed Claude got the carried posts, then the fresh
  note last. It answered with the dead token and port
  (`curl: (7) Failed to connect`), and posted again once the late note came.
  Two more BEFORE runs carried nothing: the stop landed after every post
  was written. **H AFTER**: all 13 carried posts were answered in the room
  on the live token.
- **C.** Codex's own sandbox refused every `curl` in these directories
  (exit 7, the new port listening), as recorded above, so its commands are
  the evidence. BEFORE C2 carried two posts with no note. The fresh note
  came only with a later post from the room, and Codex tried the dead token
  too. AFTER, the note came first and only the live token was used. Item 2
  BEFORE: Codex searched for a chat command and wrote "The project chat
  command hasn't been provided". AFTER: the note was typed once after its
  brief, and it ran the report `curl` with it.
- **BEFORE C1 updated Codex on the Remote.** On reconnect,
  `codex resume --last` showed its update prompt. The carried posts' paste
  and Enter picked "Update now": `npm install -g @openai/codex` ran
  (0.163.0 to 0.172.0) and Codex exited. The typed note at Ready that item
  2 restores does the same whenever an update is pending.
- **Item 4 timings** (open request to hold open, trusted directory): bare
  4.05-4.12 s, a brief 2.1-3.2 s, `--continue` 3.51-4.15 s, 17 launches over
  both builds. `HELD_CUE_AFTER` = 8 s. BEFORE every one of them drew the cue,
  then dropped it as the hold opened (40 to 41 rows). AFTER the pane kept one
  size (47 rows) from its first frame.
- **Item 4 at the dialog** (6 with a brief, 4 bare; 3 with a held post of
  1150 emoji, 4.6 KB on one line). At 3 s the dialog was drawn whole at 47
  rows, and at 10.5 s with the cue up it was whole at 45. After the answer
  the REPL was drawn at 47 rows. Each held post arrived exactly once, the
  long ones intact. Two bare runs did not reply: there the post is Claude's
  first prompt, which it read as pasted content.
- **Found, not fixed.** Posts already pasted into a busy agent sit in its
  own memory and die with it on a drop. foreman counts them delivered. Up to
  4 per run were lost this way, in both builds.
- Unverified: how the cue looks (needs a human), Grok (not installed),
  Windows Remotes (none exist).

## Results, 2026-11-24, the reply note only into an agent program

Same setup as above, one run each. BEFORE is 308401b, AFTER is this change.
The Remote has no `vim`, only `vi` (vim.tiny). The argv `vi codex.md`
matches the tab icon's substring scan just as `vim codex.md` does, so
`foreman open --on falador --cwd /tmp/<new> -- vi codex.md` was run.

| Run | BEFORE | AFTER |
|---|---|---|
| note typed into vi at launch (snapshot at 17 s) | yes | no |
| first chat post to the pane carries the note | not run | yes |

- BEFORE: rows 1-9 of the pane were the reply note, typed into vi's buffer
  above its `"codex.md" [New File]` line.
- AFTER: the pane showed only vi's status line until a post. The post
  `[chat p1 #3] t1→t4` came first, then the note.
- Both runs quit with `:q!`. Nothing was saved, and both directories were
  removed.
- Found, not fixed: the BEFORE instance's saved workspace held the vi pane
  as a Remote `agent: "codex"` (persisted through `agent_of`), and
  `relaunch` turns it into a bare `codex`. Reconnect and restore still use
  the loose predicate.

## Results, 2026-11-24, reconnect and restore by the real program

Same setup, one run each. BEFORE is bbb26be; AFTER is this change. Each
reconnect: the pane's `ssh.exe` stopped by PID, then `foreman reconnect`.

| Run | BEFORE | AFTER |
|---|---|---|
| `vi codex.md`, reconnected | Codex v0.172.0 started (`codex --no-daemon`) | vi on `codex.md` again |
| what the workspace saved for that pane | `agent: "codex"` | no agent |
| BEFORE's saved file restored, then reconnected | not run | a login shell in its directory |
| Claude after one turn, reconnected | not run | `claude --continue`, posted on the new token |

- BEFORE: Codex's start screen in the throwaway directory. Nothing was
  typed into it, and the pane was closed.
- Restore was dead at startup on both builds: the restore read
  `config::live` before the frame seeded it, so every saved Remote tab came
  back as a local shell. AFTER seeds the settings first. The BEFORE file
  then restored as a disconnected Remote with no agent.
- Claude: one turn posted `ALPHA`. After the reconnect the Remote ran
  `claude --settings <hooks> --continue`, the old turn was on screen, the
  fresh note came after the hold, and a post asking for `BRAVO` got
  `BRAVO` back through the room.
- Cleanup: vi quit with `:q!`, nothing saved. Directories and the Claude
  transcript removed. Instances stopped by PID, sandbox workspace restored.
  Claude's trust entry for the deleted directory stays in the Remote's
  `~/.claude.json`.

## Order of work

One run, in this order, each step usable when it lands:

1. Remote model, builder (all three shells), `--on`, where-strip, globe,
   Remotes pane, disconnect bar, persistence.
2. Tunnel listener, token, the chat and status routes, the reply note.
3. Per-launch hooks and the `/hook` route.

## Follow-ups

- Optional foreman tooling on a Remote, for things that need a presence
  there: hooks for providers with no per-launch flag, repo sync helpers,
  images across PCs. Always an addition; a bare Remote keeps working.
- Kanban card dispatch onto a Remote.
- Surviving a dropped connection (the mirror design above).

## Tests

Pure and table-driven: the builder per shell (quoting, spaces, `'`,
`-`-leading target, over-cap refusal on Windows, logical argv kept for the
icon); `sanitize` for `remotes`; the HTTP parser (caps, missing or wrong
token, `Expect`, unknown route); sender stamping; `OpenRequest` wire compat
for `on`; `launcher::entries` with and without Remotes; `ContentSnap`
round-trip. Nothing in the unit suite opens a socket to another PC; the
experiments cover that by hand.
