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
  install URLs contain `claude` / `codex`. `IconKind::from_argv` and
  `remote::program_agent` both return `None` for an `install_of` argv, so
  the icon, the badges, reconnect and the workspace snapshot all treat an
  install tab as a plain Remote shell. A new check that decides what runs or
  what is typed must go through `program_agent`. A substring scan of its own
  turns install tabs back into agents, except Grok's, whose URL has no agent
  name in it. Install Sessions get no Tunnel
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
- **A folder listing is cached for the life of its edit form, except a
  failure to reach the Remote.** A folder made on the Remote by anything
  but the picker's "+ new folder" does not show until the form is opened
  again. The
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
  focus moves and parks the keyboard on a button for a frame.
- **Only a directory equal to the derived one is created, and only its
  last folder.** The launch script makes the directory when it equals
  `<Projects folder>/<Project folder name>` for the Project it launches
  from (`spawn_remote_session`), whoever chose it. It never makes the
  parent, so a mistyped Projects folder ends the Session with `mkdir`'s
  error. Any other directory (a Project's own row in Settings, `--cwd`)
  must exist, or the `cd` fails and the Session ends.
- **The derived directory is never saved.** `Remote::dirs` holds only the
  Projects given a directory of their own. Changing the Projects folder
  moves every other Project's next launch, and leaves the folders already
  made under the old one behind.
- **A blank Project row is no mapping; the home is `~`.** Before the
  Projects folder a blank value was the directory prompt's answer for
  home, so `sanitize` loads a blank as `~`. Saving a blank row removes it.
- **The edit form lists a row per open Project**, mapped or not
  (`RemotesView::set_projects`), since nothing else creates a row. A
  closed Project with no mapping has none: open it to give it one.
- **The Windows-Remote folder create has never run.** The PowerShell line
  that checks the parent and creates the folder is only matched as text in
  the unit tests.
- **A Remote's git line is from the last Test, global config only.** Test
  reads `git --version` and `git config --global user.name/email` through
  the login shell. A name set in one repo, or in the system config, shows
  as "not set". Nothing is read until T is pressed.
- **"Use this PC's" writes only what the Remote lacks**, to its global git
  config, after a confirm that shows the values (`remote_git::fix`). A
  value the Remote has is never replaced. The values ride as environment
  variables, because the login shell that finds `git` would otherwise parse
  them. A value holding `"` is refused: a Windows Remote cannot carry it.
- **This PC's git name is read on the G key or the button**, by running
  `git` twice on the UI thread. Never call `remote_git::local_identity`
  per frame. Under test it is not run: `RemotesView::local_git` stands in.
- **The Windows-Remote git lines and write have never run**, like the rest
  of the Windows-Remote scripts.
- **Agent-state badges: Claude on a POSIX Remote only.** Its hooks ride the
  launch as `--settings` and post to the Tunnel's `/hook`; gotchas are in
  `docs/agent-state.md`. Codex, Grok and Windows Remotes post no events, so
  posts to them are delivered Ready-only and `status` carries no
  `agent_state` for them.
- **Anything typed before Claude's folder-trust dialog is answered quits
  Claude.** The dialog's default is "No, exit", and the Enter that ends any
  injected text picks it. Four things keep input out of it. A first launch
  whose note rides in its prompt types nothing. A Claude has its note typed
  only behind a hold (`remote::typed_route`); a hookless Claude gets it with
  its first post. A Remote Session's first launch joins the room at its end
  (`ChatRoom::join_at_end`), so it gets no catch-up. A Claude launched with
  hooks holds every post and typed note (`remote::ChatHold`,
  `ReadyGate::hold`) until a hook event opens it: Claude runs no hook at the
  dialog (seen 2026-11-23: 40 s at the dialog, nothing, then `SessionStart`
  0.3 s after the answer). Nothing opens the hold before an event: a timer
  alone would type into the dialog it waited out.
- **A keystroke never opens a hold.** The first keys a human presses in a
  held pane are the ones that answer the trust dialog, so releasing then
  would type into it. Only the cue's "deliver now" click and
  `foreman deliver tN` open it by hand (`Session::release_chat_hold`). The
  action's sense has no focus (`HELD_CUE_SENSE`), so Tab, Enter and Space
  cannot reach it even if focus handling changes.
- **The held cue waits `HELD_CUE_AFTER` before it shows.** It takes a row
  from the grid, and every hooked Claude launch holds for 2-4 s, so showing
  it at once resized the PTY twice while Claude drew its startup screen
  (seen on every launch, 2026-11-24). Its strip is not grid: a press or a
  drag that starts there sends no mouse bytes and selects nothing.
- **`SessionStart` comes before the REPL can read a long line.** After the
  dialog the tty is in cooked mode until the REPL mounts, and Linux drops a
  line over 4095 bytes there. Seen 2026-11-24 (Claude Code 2.1.331): a held
  post of 1150 emoji (4.6 KB on one line) released at `SessionStart` was
  lost 5 times in 5, and the same post sent to a mounted REPL arrived 3 in 3.
  Held across a reconnect (`claude --continue`) it was lost 1 in 5. Short input is buffered and read later, so 50 ordinary runs lost nothing.
  So `SessionStart` opens a hold only `remote::START_SETTLE` (2 s) later.
  Measured with a pty on the Remote over 53 launches, the REPL turned
  bracketed paste back on 0.10-0.19 s after `SessionStart` and painted at
  0.18-0.27 s. Any other event comes from a mounted REPL and opens it at
  once. A launch whose first argument is a brief, one word or many, bare or
  by path, waits for that brief's `UserPromptSubmit` instead
  (`remote::hooked_claude`). A slash command may never send one (`/cost`
  and `/context` did not, `/init` did), so it settles like a bare launch.
- **A held Claude whose hooks never run holds until a human opens it.**
  Examples are `disableAllHooks` in the Remote's settings, a missing `curl`
  or a hook that cannot reach the Tunnel (seen 2026-11-24), a dropped hook
  event (the lane is a bounded `try_send`) with nothing after it, and a
  subcommand that stays open (`claude agents`: a first argument counts as a
  brief, and it fired `SessionStart` but never `UserPromptSubmit`). Its posts
  stay in the room and its typed note stays queued. `status --json`
  carries `chat_held: true` at once. Once the hold has lasted
  `remote::HELD_CUE_AFTER` (8 s) the pane shows a cue on its bottom line
  ("Chat waiting until claude is ready — deliver now"), and the cue's click
  or `foreman deliver tN` sends it all in order. Any later hook event still
  opens it: every event but `SessionStart` comes from a mounted REPL.
- **Known limit: a hookless Remote agent's chat goes at Ready.** Codex,
  Grok, Claude with the user's own `--settings`, a wrapper and anything on a
  Windows Remote report no event, so there is no hold. Their posts are
  pasted once the pane is Ready, which a trust dialog already is. A post
  sent while one shows answers it ("No, exit" for Claude). Holding them
  with no signal would stall unattended dispatch into trusted directories
  forever, and a hold only a human can open is worse than the dialog. A
  hookless Claude gets no note typed at launch, so a Dispatch alone cannot
  quit it, and it cannot post until it is posted to. Codex and Grok get the
  note typed at Ready, since a Dispatch may never get a post to carry it.
- **Only an agent PROGRAM is typed into or resumed.** `remote::program_agent`
  decides every Remote choice of what runs or what is typed (typed note,
  hooks, hold, what reconnect runs, the agent the workspace saves).
  argv[0]'s stem is the agent with no extension or an executable's
  (`claude.cmd` yes, `claude.sh` no), or `npx`/`wsl` runs it in a form whose
  grammar the code states and cites (`npx_agent`, `wsl_agent`). An unknown
  flag refuses. The tab icon's scan (`IconKind::from_argv`) also matches a
  mention and decides nothing. On it `vi codex.md` got the note and its
  Enter typed into vi, and its reconnect started `codex resume --last`
  (2026-11-24).
- **`npx claude` and `npx codex` are not agents.** Both bare names are
  other npm packages (2026-11-24: `claude` 0.1.2 says it is not the official
  package, `codex` is a static site generator). Only the scoped packages
  count, optionally `@<version>` or `@<tag>`.
- **The npx grammar is npm 11's; the Debian Remote runs npm 9.2.0.** There
  `npx --no @anthropic-ai/claude-code --version` printed `9.2.0`, npm's own
  version, so an option after the package may still reach npm on that
  version. `npx_agent` reads nothing after the package, so its verdict
  holds; check this before it ever does. No npx form was run live: whether
  one would download is unproved.
- **A reconnect never replays an agent's original prompt.** `remote::reconnect_class`
  has three classes. An agent program resumes (`claude --continue`,
  `codex resume --last`). Through a wrapper it runs through that wrapper
  again, its options kept and the agent's arguments dropped
  (`remote::wrapper_command`): `wsl -d U -u me -- claude --continue` resumes
  inside the distro, while npx starts the same package and version fresh,
  since npm 9.2.0 took an option after the package as its own (below).
  A command that names no agent (`vi notes.md`, `htop`, a
  shell) reruns as it was. A command that names one but is no agent program
  (`env X=1 codex "brief"`, `nice claude "brief"`, `bash -c 'claude …'`, and
  also `vi codex.md`) gets a login shell in the same directory and runs
  nothing: foreman cannot tell a brief from a file name there. Before
  009024d these relaunched bare; 009024d reran them, brief and all, so a
  dropped Dispatch redid its work. The bar says "Enter to reconnect to a
  shell". The banner's header is one row like every note, and the command
  follows it whole, quoted to retype (`Session::inject_note_full`): a note
  is elided to one row, and that cut the command mid-brief (2026-11-24).
- **That shell leaves the room for good** (`ChatRoom::leave`): no Tunnel,
  no reply note, none of the dead connection's queue, no later post, and
  every later reconnect of the pane keeps it out (`has_left`). A post
  addressed to it fails as to any exited Member. Before this, `rejoin` put
  it back with its cursor, and a broadcast made while it was down ran in
  bash, with the note's curl and its token after it (2026-11-25,
  `echo CHATRAN > chat-ran.txt`). The banner's last row says chat stopped.
- **foreman cannot see an agent behind an alias, a variable, a wrapper
  script or a provider it does not know**, so such a command (`cc "brief"`,
  `$AGENT "brief"`, `./run-claude.sh "brief"`) reconnects by rerunning it,
  arguments and all. A user-written `claude.bat`/`.cmd`/`.ps1` that embeds a
  brief is taken for the agent itself and resumes as one.
- **Known limit: a pane that is no agent still gets typed chat.** Seen
  2026-11-25: `open --on R -- bash` ran a broadcast's lines and the note's
  curl; `open --on R -- vi notes.md` took the post and note as text, and
  again after its reconnect reran vi; a local `open -- pwsh` got the post
  and its catch-up typed, and ran nothing only because PowerShell refused to
  parse the `[chat pN #M]` frame. A shell opened from the launcher is not a
  Member (code, not run live). Whether non-agent panes should be in the
  room is a decision nobody has made yet.
- **"needs login" is the Windows host's.** An agent run through `wsl` lives
  in the distro, with its own login, so it clears nothing
  (`remote_install::agent_opened`); one through npx clears it.
- **A workspace saved before `program` restores every Remote pane as a
  shell.** The old `agent` field came from the icon's scan, so a saved
  `vi codex.md` and a real Codex look the same; neither is trusted
  (`ContentSnap::Terminal`). Builds still write `agent` too, so an older
  build restores a newer file's agents. A wrapped agent also saves
  `wrapper`, the npx/wsl command without the agent's arguments; an older
  build ignores it and restores the bare agent natively. Restore trusts a
  `wrapper` only if it is exactly one for that `program`, and otherwise
  opens a shell rather than the bare agent on the host.
- **Typed input at Ready can answer Codex's update prompt.** Seen
  2026-11-24: a reconnect's `codex resume --last` showed "update available"
  and the carried posts' paste and Enter picked its default. Codex ran
  `npm install -g @openai/codex` on the Remote (0.163.0 to 0.172.0) and
  exited. A typed note at Ready does the same whenever an update is
  pending. Nothing reports that prompt, so foreman cannot wait it out.
- **Claude keeps only the last `--settings`.** With two, only the last
  one's hooks ran, and hooks then `{}` ran none (2026-11-24, Claude Code
  2.1.331, `claude -p`). So foreman cannot add its hooks beside the user's
  own `--settings`, and `remote::hooked_claude` treats that launch as
  hookless. `--plugin-dir` can carry hooks too, but needs files on the
  Remote.
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
  one-word brief therefore gets the note another way. On top of that,
  `remote::note_route` carries the note only for `claude` and `codex` on a
  Posix Remote, never in a brief that starts with `/`: the note became
  `/context`'s arguments (2026-11-24). Those are the only tool shells seen
  to have the variables.
- **The token is per connection.** `spawn_remote_session` leases a new one on
  every launch and every reconnect, and the old one dies with its `ssh`. A
  reconnect that relaunches a held Claude types the fresh reply note once
  the hold opens (`remote::connection_note`). The resume argv has no prompt
  to carry the note (`relaunch` drops the original prompt), and a resumed
  agent still has the old `curl` command in its conversation, so it would
  get 401 until a post came. A resumed Codex (`codex resume --last`) has it
  typed at Ready. A resumed hookless Claude and a shell wait for the first
  post: nothing proves Claude is past its dialog, and a shell would run the
  note as commands. Input queued on the dead connection is carried over
  without its note (`ReplyNote::strip`), after the fresh typed note or
  carrying the fresh note itself (`Session::carry_injects`). Ahead of it, a
  resumed Claude answered the carried posts with the dead token and port
  (2026-11-24, 3 of 3 runs).
- **What a reconnect carries is the Session's own queue, not the room.** The
  room hands a post over only when the Session can take it. Without
  agent-state badges (the default) it hands over the whole backlog in one
  tick, and posts 2..n wait `ready::SUBMIT_DELAY` (500 ms) each in the
  Session's queue. A drop then carries them. Posts already pasted into a
  busy agent wait in its own memory and die with it on a drop: foreman
  counts them delivered (seen 2026-11-24, up to 4 posts in a run).
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
- **A post to a hookless Remote agent can land in an ssh password prompt.**
  Ready latches on the first output, and a password or host-key prompt is
  output, so the first post to it is typed into `ssh`'s prompt instead of
  into the agent. A carried note cannot land there, and neither can
  anything held for a hooked Claude. Key login avoids it: Settings > Remotes
  > Test says whether login works without a prompt, and one interactive
  login accepts the host key.
- **File-path links and file drops do not exist yet, so nothing gates them
  on a Remote Session.** Whoever adds either must make it inert when
  `Session::remote()` is `Some`. See `docs/terminal-links.md`.

## Why

- **No directory prompt.** A first launch used to ask which directory on
  the Remote to use. It now takes `<Projects folder>/<Project folder
  name>` without asking (2026-11-25): a Remote Session should follow its
  Project like a local one does. The spec's Launch > Directory section
  describes the removed prompt.
- **The Projects folder is never created, only the Project's folder in
  it.** Creating the whole path would turn a typo (`~/scr`) into a tree of
  empty folders with agents running in them.
- **No auto-reconnect.** A dropped connection may have eaten a shell input,
  and an uncertain input must never be resent by itself. Reconnect is the
  human's Enter on the disconnect bar, or `foreman reconnect tN`, which does
  the same through this PC's pipe only: a peer and the Tunnel cannot send
  it, so a Remote agent cannot reconnect a pane. A reconnect never replays
  an agent's original prompt (the three classes are under Gotchas).
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

- **Open a stuck hold by hand.** `foreman status --json` shows
  `chat_held: true` on the Session; after the dialog is answered (check with
  `foreman snapshot`), `foreman deliver tN --project pN`. It refuses a
  Session that holds nothing. Like `reconnect`, only this PC's pipe takes it.

- **Drive a reconnect** (tests, or Enter without the GUI). Find the
  Session's `ssh.exe` by its parent, the foreman PID
  (`Get-CimInstance Win32_Process -Filter "Name='ssh.exe' and ParentProcessId=<pid>"`),
  stop that PID, wait for `disconnected: true` in `status --json`, then
  `foreman reconnect tN --project pN`. It refuses a local Session and a
  connected one.

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

- `src/remote.rs` — `Remote`, `build`, `relaunch` and `RESUME`, `launch_dir`,
  `derived_dir`.
- `src/control/tunnel.rs` — `Hub::lease`, `Hub::handle`, `build_call`,
  `reply_note`, `ReplyNote`.
- `src/wm.rs` — `spawn_remote_session`, `add_remote_terminal`, `remote_dir`,
  `refresh_reconnects`, `inherited`.
- `src/terminal.rs` — `Session::spawn_remote`, `Session::disconnected_remote`,
  `Session::attach_tunnel`, `disconnect_bar_text`.
- `src/remote_probe.rs` — `reach`, `start_test`, `test_entry`.
- `src/remote_install.rs` — agent states, the prerequisite gate, the
  confirm's words; `wm.rs` `run_install`, `open_install`, `refresh_installs`.
- `src/remotes_pane.rs`, `src/launcher.rs` `entries_on`.
- `src/remote_browse.rs` (path model, listing scripts, cache) and
  `src/remote_browse_ui.rs` (the folder list widget).
