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
  The PowerShell install commands in `remote::INSTALL` have never run on any
  Remote, and neither have the folder list's PowerShell scripts
  (`remote_browse::ps_script`, drive roots included, and the new-folder
  `ps_mkdir_script`). Test on a real Windows Remote before you trust any of
  them. `ps_mkdir_script` alone also runs in the unit tests through a local
  PowerShell, with no SSH.
- **An install Session's argv names the agent.** The Claude and Codex
  install URLs contain `claude` / `codex`. `IconKind::from_argv` returns
  `None` for an `install_of` argv, so the icon, `agent_of`, the badges and the
  workspace snapshot all treat an install tab as a plain Remote shell. A new
  "is this an agent" check must go through `agent_of`. A substring scan of
  its own turns install tabs back into agents, except Grok's, whose URL has
  no agent name in it. Install Sessions get no Tunnel
  (`remote::carries_tunnel`).
- **`start_test` does nothing while a Test runs.** When something changed
  the Remote (an install ended), call `retest`: it owes one more run after
  the one in flight, which probed too early.
- **Test answers live in a process-wide cache** (`remote_probe::test_entry`),
  not in the Remotes pane. The launcher reads it, and the install watch
  re-runs Test into it. The pane copies a found shell into `Remote::shell`
  only when it sees the answer, so a Test that finishes while Settings is
  closed does not update the saved shell.
- **The install commands are copied from vendor pages** (2026-11-22, URLs
  beside each entry). Vendors move them; Codex's docs have already changed
  domain once. When an install fails, check the vendor page before
  changing foreman.
- **A folder listing is cached for the life of its prompt or edit form,
  except a failure to reach the Remote.** A folder made on the Remote by
  anything but the picker's "+ new folder" does not show until the prompt is
  opened again. The
  script's own answers (missing, a file, denied) are kept too. An
  `Unreachable` answer (ssh error, timeout, needs a password) is dropped by
  `Cache::forget` whenever the directory or the Remote changes, or Down or
  the retry row asks again. So a test that seeds an `Unreachable` and then
  moves to that directory sees Loading, not the failure: seed it after
  landing. The cache belongs to the `Browser`, and dropping the `Browser`
  kills its running ssh (the Remotes pane drops it the frame after the form
  closes).
- **"+ new folder" refreshes when the folder was or may have been made**
  (`Mkdir::Made`, `Mkdir::Unsure`). A timeout or a cut-off ssh can land
  after `mkdir` ran, so it is never a plain failure. The create's own
  thread does the refresh (`deliver`), so an Esc'd name field or a dropped
  widget can't skip it. A refusal (`Refused` from the script, `NotSent`
  when ssh never got to a login) changed nothing and keeps the rows. That
  matters because relisting blanks the rows, name field included. An
  unsure answer therefore closes the name field and puts its words on the
  breadcrumb (`Browser::notice`). `settle` keeps a name field without a
  row only while its create runs, since only the answer may close it.
- **A create is never killed with its widget.** Dropping the `Browser`
  kills its listings' ssh, but the create runs to its 15 s deadline: a kill
  mid-create leaves nobody knowing whether the folder exists. One create
  at a time per widget (`Browser::creating`). While it runs, the row reads
  "making …" and opens nothing.
- **The keyboard trigger is Ctrl+N, never a bare letter.** With the list
  focused, a bare N used to open the name field, and what was typed next
  became the name: typing "node" to filter made a folder "ode". Letters
  always go to the field as a filter. The name field also starts empty
  (never the filter: "~/src/proj" + new folder + Enter made "proj"), and
  Enter creates only once the field was drawn and the user typed in it.
- **A held Enter must never create twice, nor create and then open.** The
  name field creates only on a press egui has not marked `repeat`, and the
  field ignores Enter repeats right after a create (`Browser::held_enter`).
  egui marks a press of a key it still holds down as a repeat, so a test
  that presses Enter twice without a release (`pressed: false`) in between
  sees the second as a repeat and creates nothing.
- **Under `cfg(test)` nothing reaches a network unless the test opts in.**
  Every reach probe, Test, listing and create leaves the UI thread through
  `remote_probe::spawn_network`, and every ssh run or connect checks
  `network_allowed`. Both refuse on a test thread without
  `remote_probe::allow_network()` (only the `#[ignore]`d live tests take
  it). So an unseeded listing stays "Listing folders…" and a reach stays
  Checking forever. Seed with `Browser::put` / `put_test_entry`. An ssh
  create stays Creating (`Browser::started` records it). Hand back its
  answer with `Browser::land`, or drive the real create thread with
  `Browser::with_runner`. A new network call must go through the seam, or
  `nothing_reaches_a_network_under_test_without_opting_in` should grow a
  line for it.
  `the_posix_script_runs_against_hostile_names` does run the real Posix
  script through a local `sh` (Git Bash on Windows) and skips when there is
  none. Its denied branch runs only on Unix, because Windows ignores the
  mode bits.
- **Don't `request_focus` a TextEdit that already has focus.** The request
  resets the field's focus-lock filter, so egui then treats ArrowUp/Down as
  focus moves and parks the keyboard on a button for a frame. The directory
  prompt requests focus only when the field lacks it.
- **Agent-state badges: Claude on a POSIX Remote only.** Its hooks ride the
  launch as `--settings` and post to the Tunnel's `/hook`; gotchas are in
  `docs/agent-state.md`. Codex, Grok and Windows Remotes post no events, so
  posts to them are delivered Ready-only and `status` carries no
  `agent_state` for them.
- **Anything typed before Claude's folder-trust dialog is answered quits
  Claude.** The dialog's default is "No, exit", and the Enter that ends any
  injected text picks it. Three things keep input out of it. A first launch
  whose note rides in its prompt types nothing. A Remote Session's first
  launch joins the room at its end (`ChatRoom::join_at_end`), so it gets no
  catch-up. A Claude launched with hooks holds every post and typed note
  (`remote::ChatHold`, `ReadyGate::hold`) until a hook event opens it: Claude
  runs no hook at the dialog (seen 2026-11-23: 40 s at the dialog, nothing,
  then `SessionStart` 0.3 s after the answer). There is no timer. A timer
  would type into the dialog it waited out.
- **`SessionStart` is too early for a launch with a brief.** Claude's REPL is
  still mounting when it fires. A post pasted then was lost once in three
  tries: no `[chat …]` turn in the transcript. In the other two the tty
  echoed it and the REPL read it later. So a launch whose brief rides in
  argv waits for that brief's `UserPromptSubmit`, which only a mounted REPL
  sends. A bare launch and `--continue` have nothing later, so `SessionStart`
  opens theirs. A bare one kept its held post once; that is not proof it
  cannot lose one.
- **A held Claude whose hooks never run holds forever.** Examples are
  `disableAllHooks` in the Remote's settings, a missing `curl`, a subcommand
  (`claude mcp …`), or `--version`. Its posts stay in the room, its typed
  note stays queued, and `snapshot` shows nothing typed. Codex, Grok, a
  shell, a wrapper and a Windows Remote carry no hooks and get no hold.
- **Codex in an untrusted Remote directory could not post.** Seen twice on
  2026-11-23, Codex 0.163 with no `sandbox_mode` set. Its tool shell has
  `FOREMAN_URL` and `FOREMAN_TOKEN` (`env | grep -c ^FOREMAN_TOKEN=` printed
  1), but its `curl` got "Failed to connect" while a plain ssh shell got a
  401 from the same port. So its tool sandbox blocks the call, not the
  Tunnel. foreman would have to loosen Codex's sandbox to change that, so it
  leaves it alone.
- **The prompt rule refuses rather than guesses.** `prompt_index` accepts only
  the bare program and then the brief, with `--model <id>` the one tail it
  allows. The brief must hold whitespace. No subcommand of the three CLIs
  does, so this needs no subcommand list that drifts between CLI versions. A
  one-word brief therefore gets the typed note. On top of that,
  `remote::note_route` carries the note only for `claude` and `codex` on a
  Posix Remote. Those are the only tool shells seen to have the variables.
- **The token is per connection.** `spawn_remote_session` leases a new one on
  every launch and every reconnect, and the old one dies with its `ssh`. A
  reconnect that relaunches an agent types the fresh reply note at launch.
  The resume argv has no prompt to carry the note (`relaunch` drops the
  original prompt), and a resumed agent still has the old `curl` command in
  its conversation, so it would get 401 until a post came. A shell reconnect
  waits for the first post: a shell would run the note as commands. Input
  queued on the dead connection is carried over without its note
  (`ReplyNote::strip`), or the agent would be taught the dead token too.
- **The forwarded port is open to everyone on the Remote.** Any user or
  process there can connect to it. The token is the only credential, and it
  sits base64-encoded on the `ssh` command line on both PCs. On Linux any user
  can read it with `ps`, and can then post as that Session until it exits.
  This risk was accepted in the spec. Do not add a route that does more than
  post, read or report state.
- **The token must never be in a Remote agent's argv.** `/proc/<pid>/cmdline`
  is mode 444, so every user on the Remote can read an agent's arguments for
  its whole life. The base64 launch line holds the token only in its own
  `export`, and lives only while the launch shells run, since each one
  `exec`s the next. The hooks' `--settings` and the in-prompt note name
  `$FOREMAN_TOKEN`, which sits in the environment, readable only by the same
  user. Measured 2026-11-23: zero copies in a Remote Claude's or Codex's
  cmdline and in `ps -eo args`, one in each one's environment.
  `no_remote_launch_carries_the_token_outside_its_export` decodes every
  built line to keep it that way. The typed note still writes the token out,
  on screen and in the agent's conversation, never in argv.
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
- **A typed launch note can land in an ssh password prompt.** Ready latches
  on the first output, and a password or host-key prompt is output. A
  reconnected agent, or a first launch whose note is typed, then gets the
  note typed into `ssh`'s prompt instead of into the agent. A carried note
  cannot land there, and neither can anything held for a hooked Claude. Key
  login avoids it: Settings > Remotes > Test says whether login works
  without a prompt, and one interactive login accepts the host key.
- **File-path links and file drops do not exist yet, so nothing gates them
  on a Remote Session.** Whoever adds either must make it inert when
  `Session::remote()` is `Some`. See `docs/terminal-links.md`.

## Why

- **No auto-reconnect.** A dropped connection may have eaten a shell input,
  and an uncertain input must never be resent by itself. Reconnect is the
  human's Enter on the disconnect bar, and the Dispatch prompt is never sent
  again.
- **The typed note writes URL and token out; the in-prompt one names
  `$FOREMAN_URL` and `$FOREMAN_TOKEN`.** The prompt is argv, which every user
  on the Remote can read. A typed note is not, and it also serves Grok and
  Windows Remotes, whose tool shells were never seen with the variables. A
  Windows tool shell may be PowerShell or Git Bash, and each spells
  variables differently.
- **Only a Remote Session skips catch-up.** A local agent has the foreman
  skills to read the room, and its catch-up has never raced a dialog
  foreman cannot see through ssh.

## Procedures

- **Check the folder-list script against a real Remote** (read-only):
  `FOREMAN_BROWSE_TARGET=user@pc cargo test --target-dir target/agent remote_browse::tests::live -- --ignored --nocapture`.
  It lists home, `/`, and three dirs that must fail (no permission, missing,
  a file), and prints each result. It runs Posix only.
- **Check the new-folder create against a real Remote** (writes, Posix only):
  `FOREMAN_BROWSE_TARGET=user@pc cargo test --target-dir target/agent remote_browse::tests::live_posix_mkdir -- --ignored --nocapture`.
  It makes `/tmp/foreman-mkdir-test` and hostile names inside it, so that
  directory must not exist first. Remove it after:
  `ssh user@pc 'rm -rf -- /tmp/foreman-mkdir-test'`.

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
- `src/remote_probe.rs` — `reach`, `start_test`, `test_entry`.
- `src/remote_install.rs` — agent states, the prerequisite gate, the
  confirm's words; `wm.rs` `run_install`, `open_install`, `refresh_installs`.
- `src/remotes_pane.rs`, `src/remote_dir_prompt.rs`, `src/launcher.rs`
  `entries_on`.
- `src/remote_browse.rs` (path model, listing scripts, cache) and
  `src/remote_browse_ui.rs` (the folder list widget).
