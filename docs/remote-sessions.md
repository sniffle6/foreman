# Remote Sessions

## What it does

A Remote Session is a Session whose process is `ssh` into another PC (a
Remote). The PTY, the screen and Chat membership stay on this PC. The Remote
needs only sshd, the agent CLI and `curl`. The agent there reaches back through
a per-Session reverse forward (the Tunnel) to a loopback HTTP listener here.
Design and rejected alternatives: `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

Agents learn `--on` from the foreman-dispatch and foreman-chat skills. This doc
is for changing the feature.

## Gotchas

- **Every Windows-Remote claim is untested.** No Windows Remote has existed to
  try. That covers the PowerShell scripts, `curl.exe` in the reply note,
  `WINDOWS_LINE_CAP`, the 5.1 argument escaping (`legacy_native`,
  `legacy_passable`) and resize through two ConPTYs. These were checked only by
  running encoded lines through local cmd → pwsh and cmd → 5.1, with no SSH.
  Test on a real Windows Remote before you trust any of them.
- **Agent-state badges are not available on a Remote Session yet.** Nothing
  posts hook events, so posts to it are delivered Ready-only and `status`
  carries no `agent_state` for it.
- **The token is per connection.** `spawn_remote_session` leases a new one on
  every launch and every reconnect, and the old one dies with its `ssh`. A
  reconnect that relaunches an agent injects the fresh reply note at launch,
  because a resumed agent still has the old `curl` command in its
  conversation and would get 401 until a post came. A shell reconnect waits
  for the first post: a shell would run the note as commands. Input queued on
  the dead connection is carried over without its note (`ReplyNote::strip`),
  or the agent would be taught the dead token too.
- **The forwarded port is open to everyone on the Remote.** Any user or
  process there can connect to it. The token is the only credential, and it
  sits base64-encoded on the `ssh` command line on both PCs. On Linux any user
  can read it with `ps`, and can then post as that Session until it exits.
  This risk was accepted in the spec. Do not add a route that does more than
  post, read or report state.
- **Listener slots are shared by every Remote.** There is one listener per
  foreman instance, so `MAX_INFLIGHT` counts callers from all Remotes
  together. A flood from one Remote (any user on it, see above) answers
  another Remote's agents with 503 busy.
- **A reply sent before the request is fully read must drain first**, or
  Windows resets the connection and the caller loses the reply. See
  foreman-debugging-playbook §16. Do not repeat the fix here; keep
  `close_after_reply` on every path that answers early.
- **Resume drops the original flags.** `relaunch` builds the program plus its
  `RESUME` entry and nothing else. `--model`, permission flags and any
  `--settings` the first launch had are gone, so a reconnected agent can stop
  on permission prompts the original skipped. `--continue` / `resume --last`
  also take the latest conversation in that directory, which is another
  Session's if two share the directory on that Remote.
- **The launch reply note can land in an ssh password prompt.** Ready
  latches on the first output, and a password or host-key prompt is output.
  An agent opened with a prompt then gets the note typed into `ssh`'s prompt
  instead of into the agent. Key login avoids it: Settings > Remotes > Test
  says whether login works without a prompt, and one interactive login
  accepts the host key.
- **File-path links and file drops do not exist yet, so nothing gates them
  on a Remote Session.** Whoever adds either must make it inert when
  `Session::remote()` is `Some`. See `docs/terminal-links.md`.

## Why

- **No auto-reconnect.** A dropped connection may have eaten a shell input,
  and an uncertain input must never be resent by itself. Reconnect is the
  human's Enter on the disconnect bar, and the Dispatch prompt is never sent
  again.
- **The note is written out, not `$FOREMAN_URL`.** A Windows agent's tool shell
  may be PowerShell or Git Bash, and they spell variables differently. The
  variables are exported too, which the procedure below uses.

## Procedures

- **Check the Tunnel by hand.** In a Remote Session's shell (Posix):
  `curl -sS -H "Authorization: Bearer $FOREMAN_TOKEN" "$FOREMAN_URL/status"`.
  On Windows (untested) use `curl.exe` and `$env:FOREMAN_TOKEN`. A 401 means a
  stale or unknown token. Connection refused means there is no forward: the
  Session is outside a Project (it toasts "has no way back"), or `ssh -R`
  failed, which ends the Session at once.
- **Read what a Remote Session ran.** Take the last argument of its `ssh.exe`
  command line
  (`Get-CimInstance Win32_Process -Filter "Name='ssh.exe'" | Select-Object CommandLine`).
  For Posix, decode the base64 inside the `printf`: `printf %s <b64> | base64 -d`.
  For PowerShell, decode the `-EncodedCommand` value:
  `[Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('<b64>'))`.

## Key files

- `src/remote.rs` — `Remote`, `build`, `relaunch` and `RESUME`, `launch_dir`.
- `src/control/tunnel.rs` — `Hub::lease`, `Hub::handle`, `build_call`,
  `reply_note`, `ReplyNote`.
- `src/wm.rs` — `spawn_remote_session`, `add_remote_terminal`,
  `refresh_reconnects`, `inherited`.
- `src/terminal.rs` — `Session::spawn_remote`, `Session::disconnected_remote`,
  `Session::attach_tunnel`, `disconnect_bar_text`.
- `src/remote_probe.rs` — `reach`, `start_test`.
- `src/remotes_pane.rs`, `src/remote_dir_prompt.rs`, `src/launcher.rs`
  `entries_on`.
