# Git windows on a Remote — design spec

Direction settled with the user 2026-11-25: Git Changes and Git History show
the repository where the work is happening. In a Project with Remote
Sessions that is often the Remote's checkout, not this PC's. The first cut
is Git Changes complete on a Remote: view, the index writes, commit and
push. Decisions tagged *(proposed)* were written into this spec before
being discussed; the user agreed to all of them the same day.

Three small checks were run against one Linux Remote while writing this;
they are under Results. Nothing else here has been run.

## Problem

A Remote Session opens in the Project's folder on the Remote
(`remote::derived_dir`), and the agent there edits that checkout. Git Changes
and Git History are built with the Project's local directory
(`HistoryView::new`, `ChangesView::new` in `src/wm.rs`) and run `git` on this
PC only. So in a Project whose work is on a Remote, both windows show a
checkout nothing is touching, and the changes the agent just made are
invisible. A window that showed the Remote's files but could not commit them
would be worse than none: it looks like the local one and half of it is
dead.

## Decisions

- **A Git window shows one Site: this PC, or one Remote and a directory
  there.** It follows the Site of the last focused terminal in its Project,
  unless it is pinned.
- **A header strip says which Site**, in both windows, on Local too. Same
  look as the launcher's.
- **On a Remote, Git Changes is whole**: the file list, Add to VCS, Mark
  Resolved, Rollback, Commit, Commit and Push, Push, and the Diff window
  opened from it.
- **One seam runs git at a Site.** Everything above it (status parsing, the
  write queue, the push dialog, the diff parser) does not know which PC it
  is talking to.
- **The transport is one `ssh` per git command**, the system `ssh.exe`, with
  the flags `remote_browse::ssh_args` already uses. No SSH crate, no agent
  on the Remote, nothing installed there.
- **Foreman holds no credentials and forwards none.** A push from a Remote
  uses whatever git on that Remote is set up with. This PC's credential
  helper never travels.
- **Git never prompts and never hangs.** `GIT_TERMINAL_PROMPT=0` and
  `BatchMode=yes`; a failure shows git's or ssh's own text.
- **The Remote's git name and email are the Settings > Remotes row**
  (`src/remote_git.rs`, already built). The Git windows do not write config.

Written in first, then agreed:

- **The Remote command is a wrapped script, not an argv.** Each git argument
  is quoted with the Posix quoting `remote_git` uses, the script is wrapped
  by `remote_browse::remote_line`, and ssh carries one opaque word. A commit
  message rides inside it as an ordinary `-m` argument. The earlier plan was
  "messages over stdin"; that does not work, because `commit --only` already
  uses stdin for the path list (`commit::over_paths`). *(proposed)*
- **No login shell for git.** The probe uses one to find agent CLIs, and it
  prints rc-file noise that the probe filters by marker lines. Git output is
  NUL-separated and parsed whole; noise would corrupt it. Checked on one
  Remote: git and its credential helper both resolve without it.
  *(proposed)*
- **No timer on a Remote.** There is no file watch across ssh. A Remote
  Changes window re-reads when it becomes active (today's no-watch
  behaviour), after each of its own writes, on a Refresh control in the
  strip, and when a Remote Session of that Remote in its Project goes from
  working to idle. *(proposed)*
- **Delete and Add to .gitignore are not offered on a Remote in the first
  cut.** Delete goes to this PC's Recycle Bin (`commit::recycle`) and has no
  undoable equivalent over ssh; ignore edits the file through the local
  filesystem. Both menu items are disabled there with a reason. Show in
  Explorer likewise. *(proposed)*
- **Posix Remotes only in the first cut.** On a Windows Remote the window
  shows the strip and one line saying Git windows do not work on a Windows
  Remote yet. The Windows half of Remote Sessions has never been run, and a
  second quoting dialect doubles the seam for a path with no test machine.
  *(proposed)*
- **Git History on a Remote is the last step, not the first cut.** Until it
  lands, History stays on Local and its strip says so, including while
  Changes beside it shows a Remote. *(proposed)*
- **The pin is saved with the window.** An unpinned window restores to Local
  and moves on the next terminal focus. *(proposed)*
- **The commit panel warns when the Remote has no git name or email**, from
  the last Test, and points at Settings > Remotes. It does not block the
  commit. *(proposed)*

Rejected:

- *A persistent helper process on the Remote* (one long-lived ssh running a
  small command loop). Removes the per-command connect cost, but it is a
  protocol to design, a process to supervise, and a thing that can be left
  running. A Remote "needs nothing from foreman" is a decision of the Remote
  Sessions spec; this would break it for a half-second saving.
- *SSH connection sharing* (`ControlMaster`). Not available in the Windows
  OpenSSH client, which is the transport.
- *Reusing the Session's own ssh connection or Tunnel.* The Session's channel
  is the terminal; multiplexing git over it means parsing the PTY stream. The
  Tunnel points the wrong way and exists per Session.
- *Mounting the Remote's folder* (sshfs, SMB) and running local git on it.
  Needs a driver or a share per Remote, and git over a network filesystem is
  slow and gets file modes and line endings wrong.
- *Showing Local and Remote side by side in one window.* Two file lists, two
  commit panels, and every action needs a "which one" answer.
- *A separate "Remote Git" window kind.* A new Content kind for the same
  screen; the Site is a property of the view, not a different view.
- *Read-only on a Remote.* The user's call: a Changes window that cannot
  commit or push confuses people.
- *Pushing with this PC's credentials*, by forwarding a helper or a token.
  It sends a secret to a machine that did not have it.

## Goals and non-goals

Goals:

- In a Project with Remote Sessions, Git Changes shows what the agent on the
  Remote changed, without the user doing anything.
- Commit and push from that window land on the Remote's checkout and its
  origin.
- Local behaviour is unchanged: same speed, same watch, same tests.

Non-goals:

- Moving commits between this PC's checkout and the Remote's. Each has its
  own origin; `git push` and `git pull` are how they meet.
- Fixing a Remote's credentials. If push fails there, the text git printed
  is shown and that is all.
- Any git operation the windows do not already have (pull, fetch, branch
  switch, stash).

## Vocabulary

One new glossary term for `CONTEXT.md`, added with the first card:

**Site**:
Where a Git window runs git: this PC and a directory, or a Remote and a
directory there.
_Avoid_: place (`remote::Place` is where a launch puts its Win; Placement is
taken too), location, target (the ssh address), host.

## The Site

- `Site::Local(PathBuf)` or `Site::Remote { remote, dir }`. `remote` is the
  Remote's name, resolved against Settings at the moment a command runs, so
  an edited ssh target takes effect and a deleted Remote is an error shown in
  the window, not a stale address.
- `dir` on a Remote is a string in that Remote's own path form (`~/app`,
  `/srv/app`). It is never joined, normalised or tested with this PC's path
  rules.
- A terminal's Site is its Session's `remote_dir()` on its Remote for a
  Remote Session, and the Project directory on Local otherwise.

## Following and pinning

- The Project's `WindowManager` already knows its focused terminal. When
  focus lands on a terminal, every unpinned Git window in that Project takes
  that terminal's Site. Focus on a Git window, the Diff window, or anything
  that is not a terminal changes nothing.
- A change of Site cancels the window's in-flight reads the same way a
  directory change would, and starts a fresh read. The checkbox selection
  and the commit message are per Site: switching away and back does not lose
  a half-written message. *(proposed)*
- The pin is a toggle in the strip. Pinned, the window keeps its Site
  whatever is focused.
- The Diff window opened from Changes carries the Site it was opened with and
  keeps it; it does not follow.
- A queued or running write belongs to the Site it was started on. A Site
  change while a commit runs does not redirect or cancel it; its result is
  reported against that Site.

## The seam

`src/git_history/git.rs` `fn command` is the one place a `git` process is
built: `spawn` and `output` for reads, `write` for writes, all taking a local
`cwd`. Those three take a Site instead.

- Local: unchanged.
- Remote: `ssh` with `remote_browse::ssh_args`, and one wrapped script:
  change to `dir`, export `GIT_TERMINAL_PROMPT=0` (and
  `GIT_OPTIONAL_LOCKS=0` for reads), run `git --no-pager` with each argument
  quoted.
- Stdin passes through ssh untouched, so `--pathspec-from-file=-` with
  `--pathspec-file-nul` works as it does locally.
- The script builder is a pure function from (Site, args, read or write) to
  the ssh argv, and is what the tests cover. Under `cfg(test)` the seam
  refuses to start a network process, as `remote_probe::spawn_network` does.
- Cancel and timeout kill the local `ssh`. For reads that is enough: the
  remote `git` dies on its closed pipe. For a push it is an experiment (see
  below).
- A commit is never killed locally because that would leave `index.lock`
  (`git::write`). Over ssh a dropped connection can do the same thing and
  nothing here can prevent it. Accepted: the next command shows git's own
  "index.lock exists" text. No automatic lock removal.
- ssh's own failures (unreachable, host key, auth) come back on stderr with
  exit 255 and are shown as they are. `GitError::Spawn` keeps meaning "could
  not start the process on this PC".

Callers that bypass the seam and must move onto it or be gated by Site:

- `commit::recycle`, `commit::ignore`, `commit::inside` — local filesystem.
- The Explorer launch in `src/git_history/changes.rs`.
- `git_history::watch` — `ReadDirectoryChangesW` on a local root. A Remote
  Site gets what `Follow::off` gives a view today, never a watch on a path that
  happens to parse.
- Any place that reads a working-tree file directly instead of through git,
  in `diff_view.rs` and the AI draft in `commit.rs`. To be found by reading,
  not assumed absent.

## Cost

One ssh connection per git command. Measured on a LAN Remote: about 0.45 s
per command, nearly all of it the connect. So:

- A refresh must stay one command. If the Changes read is more than one
  `git` call today, they are joined into one script on a Remote.
- The push dialog's reads are joined the same way.
- Nothing on a Remote runs per frame, per keystroke or on hover.
- The window shows that a read is in flight; half a second with no sign of
  life reads as broken.

## UI

- **Strip**, top of both windows: the Site as `Local · <dir>` or
  `<Remote name> · <dir>`, the pin toggle, and Refresh. One line; the
  directory is cut from the left when it does not fit.
- **Unreachable Remote**: the strip stays, the body shows ssh's text and a
  Retry control. The previous file list is not kept on screen under an
  error.
- **Not a repository there**: today's not-a-repository state, with the
  Remote's directory in it.
- **Disabled actions** on a Remote keep their place in the menu and say why.
- **Push dialog**: unchanged, read through the seam. A failed push shows
  what git on the Remote printed, in the same notice a local failure uses.
- **AI draft** of the commit message: the diff is read through the seam and
  the drafting agent runs on this PC, as now.

## Persistence

The window snapshot (`ContentSnap::GitChanges`, `ContentSnap::GitHistory`)
gains the pinned Site, optional and absent by default so older saved layouts
load unchanged. An unpinned window saves nothing new. A pinned Site whose
Remote no longer exists restores as pinned and shows the error; it does not
silently fall back to Local.

## Results, 2026-11-25, one Linux Remote

Run from this PC with Git Bash's `ssh`, not the Windows OpenSSH client
foreman uses. Read-only.

- `git status --porcelain=v2 -z --branch` in a directory with spaces in its
  name returned clean NUL-separated output with no extra bytes. 474 ms; a
  bare connect was 406 ms.
- Without a login shell, `git` and the credential helper were both found
  (the helper is configured by absolute path) and `git ls-remote origin`
  succeeded with `GIT_TERMINAL_PROMPT=0`.
- A NUL-separated path list written to ssh's stdin arrived byte for byte.
- Earlier the same day, a real `git push` of a throwaway branch from that
  Remote succeeded non-interactively and was cleaned up.

## Experiments that gate the build

1. The same three checks through `C:\Windows\System32\OpenSSH\ssh.exe`,
   which is what foreman spawns. Its stdin handling is the one that matters.
   A failure of the NUL list changes how paths travel.
2. Kill the local `ssh` during a slow `git push` on the Remote and check
   what is left running there. If the remote git survives, cancel on a
   Remote push is relabelled as "stop waiting", not hidden.
3. A commit message with quotes, newlines, a leading dash and non-ASCII
   text, through the wrapped script, compared byte for byte with
   `git log -1 --format=%B` on the Remote.
4. A Remote where git is not on the non-login PATH (Homebrew on macOS is the
   likely case). If one exists, Test records the login shell's PATH and the
   script exports it. Not built unless this fails.

## Order of work

1. `Site`, the pure script builder and its tests, the seam taking a Site.
   Local callers pass `Site::Local`; nothing visible changes.
2. Experiments 1 to 3.
3. Changes reads on a Remote, the strip, follow and pin, the refresh
   triggers, the unreachable and Windows-Remote states.
4. Changes writes on a Remote: Add to VCS, Mark Resolved, Rollback, Commit;
   the three local-only actions disabled; the identity warning.
5. Push and the push dialog on a Remote.
6. The Diff window from a Remote Changes window.
7. Git History on a Remote.

Steps 3 to 6 are the first cut and ship together. Step 1 can land alone.

## Follow-ups

- Windows Remotes.
- Delete and Add to .gitignore on a Remote.
- Pull or fetch, which is how a Remote's commits reach this PC's checkout
  and is the first thing a user will want after pushing from one.

## Tests

- The script builder, table-tested: quoting of every argument shape in use
  (paths with spaces and quotes, a multi-line message, a leading dash), the
  directory forms, read versus write environment, and that a Local Site
  produces today's command exactly.
- Follow and pin as pure state: focus moves an unpinned window's Site, not a
  pinned one; non-terminal focus moves nothing; a running write keeps its
  Site.
- A layout test for the strip at the window's minimum width, both Sites, and
  that the strip does not move the tree or the commit panel between Sites.
- Snapshot round trip with and without a pinned Site, and an old snapshot
  with no Site field.
- Every existing `git_history` test passes unchanged against `Site::Local`.
- No automated test reaches a real Remote. The experiments above are run by
  hand and their output is added to this spec under a dated Results heading.
