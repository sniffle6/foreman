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

**Folder list.** Under the prompt's Directory field, and under the edit
form's Directory rows (one reserved area for whichever row is focused), a
list of the Remote's folders saves typing a path blind. Folders only, never
files. The field stays the source of truth: the list shows the folders in
the directory the text names up to its last separator, filtered by the
case-insensitive prefix after it. Choosing one writes `dir/name/` into the
field and lists the next level; `..` goes up (above home it uses where the
listing found home); home is written `~/…`. The area is a fixed height in
every state (listing, listed, empty, cut, failed, retry), the prompt's list
gives up rows rather than push its buttons off a small window, and the field
and the list never both look focused. Its breadcrumb names the directory
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

Accepted risk: the token sits on a command line (base64, not secret) on
both PCs, so another process running as the same user can read it, and on
Linux any user can read a process's arguments. It buys posting chat as that
one Session, and feeding its badge, until the Session exits. The local pipe
already trusts same-user processes with more than that.

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
  through the Tunnel.
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

**Settings → Remotes.** A custom-body pane like Keybindings: one row per
Remote (name, target, shell, reachability, Test, delete), inline add and
edit, ssh-config `Host` names offered as suggestions. **Test** runs one
`ssh -o BatchMode=yes` probe off-thread and reports: login works without a
prompt (or "needs an interactive login: open a Remote Session on it once
and log in there"), which
shell it found, whether `curl` is there, which agent CLIs are on PATH.
Deleting a Remote does not touch live Sessions; each carries its own copy
of the name.

**Installing an agent CLI.** Each row has a second fixed line for the agent
CLIs, there before the first Test so a result never moves a row: "has
claude, codex", then each missing agent as an "install grok" row button or
in words. "grok needs bash" means the installer needs a tool Test did not
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
provider with no entry, or an agent launched through a wrapper (`npx`,
`wsl`, a script), starts the bare provider fresh. "Is an agent" is the tab
icon's test, so the icon and reconnect never disagree. The original
Dispatch prompt is never sent again.

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
