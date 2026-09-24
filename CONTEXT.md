# Foreman

Foreman is a fast, native desktop window manager for running and supervising many
AI-agent terminals at once ("tmux built for AI"). This is a glossary only — what
each term means and which near-synonyms to avoid, so code and docs stay
consistent. No implementation details; the epics and `docs/` hold those.

## Language

### Composition

**Window manager**:
The single reusable engine that lays out, focuses, and stacks a set of Wins inside
a rectangular area. The same engine runs at the desktop level and nested inside
every Project.
_Avoid_: compositor (reserve for "recursive compositor"), layout manager.

**Recursive compositor**:
The arrangement where a Window manager is nested inside itself — a Project's
content is another Window manager — so the same interactions work at every level.
_Avoid_: nesting, embedding.

**Desktop**:
The outermost Window manager, hosting Project windows full-bleed.
_Avoid_: root, canvas.

**Win**:
One window the Window manager owns: a stack of one or more tabs with a position,
stacking order, and a Content. A single-tab Win reads as a plain window.
_Avoid_: pane, frame, widget.

**Content**:
What a Win holds — a Terminal, a Project, or one of the PTY-less viewers.
_Avoid_: body, payload.

**Project**:
A top-level Win whose Content is its own nested Window manager — a sandbox that
confines its child windows.
_Avoid_: workspace, group, folder.

### Window state & layout

**Tiled / Floating**:
The two states every Win is in. Tiled Wins are leaves of the Layout tree and fill
their slot; Floating Wins overlap freely with a stacking order.
_Avoid_: docked; snapped (snapping is a transient gesture, not a state).

**Layout tree**:
The tree of recursive horizontal/vertical splits that positions every tiled Win.
_Avoid_: grid, BSP (informal only).

**Tab stack**:
A Win holding more than one tab. Tabbing is allowed only between Wins in the same
Window manager — Projects with Projects, Terminals with Terminals.
_Avoid_: group, notebook.

**Zoom**:
A tmux-style temporary state that renders one Win over the whole area without
changing the Layout tree.
_Avoid_: maximize, fullscreen (maximize is a real rect change; Zoom is an overlay).

### Terminal

**Session**:
Foreman's name for one running terminal — a live shell or agent process with its
emulated screen.
_Avoid_: terminal, tab, console, PTY (a PTY is only one part of a Session).

**Shell / agent**:
The two kinds of program a Session runs — a Shell (cmd, PowerShell, bash) or an
agent CLI (Claude Code, Codex).
_Avoid_: program, command (too generic to disambiguate the two).

**Ready**:
The state a Session reaches once it has answered the program's startup
device-status query; injected input only lands after a Session is Ready.
_Avoid_: started, alive, booted.

**Bell**:
A breathing amber pulse on a Session's chrome and its Sessions-panel row when
the program rings BEL — attention routing to that Session, latched until the
Session gains keyboard focus; not an OS notification. The latch belongs to that
Session (not the Win); a single app-wide preference can silence all Bell
attention (visual now; sound/push later).
_Avoid_: alert, notification, toast (reserve those for later supervision push).

### Input & control

**Leader**:
The prefix chord (default Ctrl+B) that arms Foreman to read the next key as a
window command instead of terminal input.
_Avoid_: prefix, hotkey, modifier.

**Chord**:
A single key combination. A Chord paired with a command is a binding.
_Avoid_: shortcut, accelerator.

**Keymap**:
The data-driven set of Chord→command bindings, with user overrides merged over
defaults.
_Avoid_: config, bindings file.

### Agent coordination

**Dispatch**:
Launching a new visible Session (an agent or command) from inside another Session.
_Avoid_: spawn, run, fork.

**Control plane**:
The local request channel an in-terminal agent uses to drive Foreman — open a
terminal, post to chat, query status, close.
_Avoid_: API, IPC bus, server.

**FOREMAN_PIPE**:
The per-instance Control-plane pipe name injected into every Session, so a CLI
inside that Session talks to the host that spawned it.
_Avoid_: well-known pipe (that is `PIPE` / `\\.\pipe\foreman`, for callers
outside a Session).

**Inspection**:
Driving a Session with input (**send**) and reading back its rendered screen
(**snapshot**) over the Control plane — so an agent or a test can verify a
terminal headlessly, without the window or the user's keyboard.
_Avoid_: scraping, probing, automation.

**Snapshot**:
A Session's grid captured as data — the displayed viewport as text by default,
or the last N buffer lines with `--tail N`; per-cell attributes and cursor on
request. A read, never a side effect.
_Avoid_: dump, capture, screenshot (a screenshot is pixels; a Snapshot is the grid).

**Chat room**:
A Project's shared, append-only message log that member Sessions post to and
receive from, used to coordinate agents.
_Avoid_: channel, thread, log.

**Member**:
A Session that belongs to a Project's Chat room.
_Avoid_: participant, client.

**Member id**:
The stable identity a Member carries for its whole life — assigned when its
Session spawns and unchanged by tabbing, untabbing, focusing, or moving. The same
identity the agent reads as its own terminal id, so the room's view of "who" and
the agent's view of "me" never disagree.
_Avoid_: window id, tab id (a Win id can change; a Member id cannot).

**Dispatcher / Worker**:
Roles in a Chat room — the Dispatcher hands out work; a Worker is a Session
dispatched to do it.
_Avoid_: master/slave, manager/agent.

**Crew board**:
The Chat viewer's presence panel, showing each Member and how recently it was
heard from.
_Avoid_: roster, sidebar, presence list.

**Card**:
One unit of work-in-flight on a project's board; a file in `.foreman/tasks/`
owned by the app. A Done Card may be grouped into a Version by a Cut.
_Avoid_: task (taken), ticket, issue (GitHub's word).

**Claim**:
The card↔Session link recorded at dispatch or `start`: terminal id, app run
nonce, agent, timestamp. Dead claims are derived (orphan), never stored.
_Avoid_: assignment, lock.

**Worktree**:
A Card's private checkout under `.foreman/worktrees/<id>`, on branch
`card/<id>`, created at dispatch and recorded on the Card.
_Avoid_: sandbox, checkout (ambiguous with the main checkout).

**Base**:
The branch the main checkout had at dispatch; the Worktree's integration
target.
_Avoid_: main, trunk (it may be a feature branch).

**Integrate**:
Landing a Card's branch on Base: rebase onto Base, run the project's checks,
fast-forward Base. Done by Foreman's Integration queue, not by the Worker.
_Avoid_: merge (implies a merge commit).

**Integration queue**:
The per-repository, durable, ordered list of Submissions Foreman integrates
one Turn at a time; files under the clone's git common dir.
_Avoid_: merge queue (GitHub's remote thing), lock, pipeline.

**Submission**:
One request to integrate a Card's committed Worktree at a specific commit;
the same Card and commit submitted twice is one Submission.
_Avoid_: PR, job, ticket.

**Turn**:
The exclusive right to integrate for one repository, held by one owner process
for as long as it works and released by the OS when it dies.
_Avoid_: lease, lock (the lock is the mechanism; the Turn is what it grants),
slot.

**Prepared commit**:
The Worktree's HEAD after a clean rebase onto the target: what the checks ran
on and what the fast-forward advances Base to; persisted before the merge.
_Avoid_: candidate, staged commit.

**Needs resolution**:
The Submission substate after Foreman hands a Card back — conflict, failed
check, moved source — with the reason and next action; the Worker resolves
and resubmits. A substate of In Progress, not a column.
_Avoid_: failed, rejected, blocked (that is a column).

**Held**:
A queued Submission the destination refused (dirty, wrong branch, busy); the
whole queue waits and retries, nothing is touched.
_Avoid_: paused, stuck.

**Teardown**:
The non-forcing remove-and-delete queued by `done`, Release, and `rm`; keeps
a dirty tree or an unmerged branch.
_Avoid_: cleanup, delete.

**Discard**:
The human-only forcing Teardown offered on a Done, Blocked, or orphaned Card.
_Avoid_: force-delete, purge.

**Board**:
The per-project kanban surface (a window content variant) — shows Cards in
four fixed columns (Backlog / In Progress / Blocked / Done) and dispatches a
Worker straight from a Card.
_Avoid_: kanban window, task board, panel (taken).

**Cut**:
The explicit action (Board button, `foreman kanban cut`) that stamps every
ungrouped Done Card as shipped in a named Version. Grouping, not a state
change.
_Avoid_: release (taken — that is In Progress/Blocked → Backlog), archive,
milestone, sprint.

**Version**:
The named group a Cut creates; the Done column can show Current or any one
Version. Exists only as the distinct `shipped.name` values on Cards.
_Avoid_: changelog, milestone, ship (the event, not the group).

**Plan**:
An ordered grouping of Cards, derived from their `planned.name` the way a
Version is derived from `shipped.name` — there is no plan file and no plan
store. A Card belongs to at most one Plan, and Plan names fold case and outer
whitespace like Version names.
_Avoid_: epic, milestone, project (taken), sprint, roadmap.

**Wave**:
A Plan's ordering rank, carried per Card as `planned.wave`; lower runs
earlier and numbers need not be contiguous. The **current wave** is the
lowest one still holding a non-Done Card.
_Avoid_: phase, stage, step, batch, tier.

**Plan view**:
The per-project read-only window (leader `L`) that groups Cards by Plan then
by Wave. It reads; it never dispatches — that stays on the Board.
_Avoid_: plan window (the abandoned scheduler design used that name),
timeline, roadmap.

**Card trailer**:
The `Card: <id>` line a Worker ends each commit message with; how a Cut
finds a Card's commits after rebase or squash.
_Avoid_: tag, footer, reference.

### Seams & patterns

These name deliberate seams — places where behaviour is isolated behind a small
interface so it can be changed or tested in one spot.

**Deferred action**:
A window interaction recorded during the draw pass and applied after it, because
the draw cannot mutate nested Window managers mid-render.
_Avoid_: command, event (overloaded), callback.

**Project launcher**:
The ordered Agents, Shells, Project tools, and New project choices shared by
project titlebars and task-manager project rows. `src/launcher.rs` builds the
list from the live keymap, default shell, and open project tools; `LaunchPath`
resolves the target project tab, performs the action, and surfaces that project.

**Panel order**:
Presentation-only per-tab rank (`Tab::panel_order`) driving sessions-panel row
order. Written only by `Act::ReorderPanel` (dense per scope), projected by
`panel_model()`, persisted additively in `TabSnap`. Never touches the tab
strip, Layout tree, z-order, or focus.

**Tab uid**:
Runtime-only per-tab identity (`Tab::tab_uid`), stamped at construction from a
process-global counter; how a panel drag ref finds its row at drop time.
Separate system from Panel order (rank = position, uid = identity); never
persisted — uids regenerate on workspace restore.
_Avoid_: tab order (that's the real strip), z-order, sort index.

**Input-encoding seam**:
The pure step that turns a Session's keyboard and mouse events into the exact
bytes a terminal program expects, with no dependency on the GUI — so it can be
tested without a window.
_Avoid_: input handler, key router.

**Quiescence settle**:
Waiting until a Session has produced no new output for a short window before
reading it — the default way `send` returns a settled screen instead of a
mid-update race.
_Avoid_: sleep, debounce, delay.

**Title lane**:
The bounded, one-way path from an agent's first meaningful prompt hook to the
single background naming worker and back to its owning Session tab. It is not
part of the Control plane: hooks never wait for a reply, and the GUI never waits
for a provider.
_Avoid_: title API, hook server, naming pool.

**Caret**:
What Foreman paints for a Session's cursor: the grid model's cursor cell, drawn
every frame exactly where the model says it is — `?25l` (hide) honored, no
blink, no position debouncing. Focused pane: filled rect; unfocused panes: a
hollow full-cell outline. The caret is what Foreman paints; the cursor is the
program's, owned by the grid model. (The old "Caret gate" — a settle/grace
debounce against mid-redraw cursor teleports — was retired 2026-07-15 after
measurement showed modern TUIs bracket redraws in synchronized output;
evidence in docs/cursor-rendering.md.)
_Avoid_: cursor (that's the model's), Caret gate (retired), blink, debounce.

**Ready gate**:
The step that decides when a Session may accept injected chat input, and what
bytes that injection becomes. It latches Ready only after the startup
device-status reply has been flushed *and* the child has painted first visible
output; holds posts that arrive earlier; and turns a post into bracketed-paste
bytes plus a deferred submit. Pure decisions only — it does not write the PTY.
_Avoid_: ready flag, inject queue (those are pieces; the gate is the whole
contract), DSR (one half of the latch, not the gate).

**Cell metrics**:
One frame's pixel↔cell geometry for a Session's pane — where the pane starts,
how big a cell is, and how many cells fit. Every pointer→cell and cell→rect
conversion goes through it, so out-of-bounds clamping is decided (and tested)
in one place instead of at each paint or input site.
_Avoid_: geometry (too broad), layout (that word belongs to the Layout tree),
hit-testing.

**Frame plan**:
One frame's paint geometry and content for a Session's pane — the styled text
runs, the selection-highlight rects, the caret rect, and the scrollback-thumb
rect — computed purely from the grid, its Cell metrics, the selection, and the
caret draw decision. `show()` only decides visibility (focus for the caret,
hover for the thumb) and the paint style (colors, corner radii), then replays the
plan. The grid walk is clamped to the grid's real bounds first, so a stale index
can't panic and abort the process.
_Avoid_: display list, render list, draw commands.

**Scrollbar geometry**:
The pure, axis-generic mapping between a scrollable viewport, its content
offset, and the thumb's paint and interaction rects. Terminal scrollback and
the Sessions panel share it so sizing, edge clearance, and dragging stay in
sync.
_Avoid_: scroll state, scroll view, thumb math.

**Selection cull**:
The step that re-projects the selection onto the visible viewport for painting.
The selection itself lives in the grid model (`term.selection`, buffer
coordinates — one source feeding both the copied text and the highlight); the
cull maps it to the rows actually on screen. An edge row that is scrolled out
of view takes the full-row boundary (a start above the viewport becomes the
origin cell, an end below it the bottom-right cell); a fully off-screen
selection paints nothing.
_Avoid_: clip (pixel-flavored), conversion (any coord math), re-anchoring.

**Outbox**:
The Chat room's per-frame delivery decision: given which Members are Ready, it
returns exactly the framed lines each one still needs and advances their delivery
cursors — a pure step, so the delivery guarantee can be tested without a terminal.
The engine only injects what the Outbox hands back; it never decides what to send.
_Avoid_: queue, buffer, broadcast (broadcast is a targeting mode, not the delivery
step).

**Theme tokens**:
Every named color lives as a const in `src/theme.rs`, glob-imported by its
consumers — one place to restyle the app, and the future seam for real themes.
Deliberately static: the consts become fields on a Theme struct only when a
second theme actually lands; a switchable system today would be interface with
nothing behind it.
_Avoid_: palette (that's the ANSI 16-color table, one token among many),
stylesheet, skin.

**Agent command resolution**:
Converts a known npm Codex launcher into a shell-free Node argv while preserving
the original command identity and prompt. Owned by `src/agent_command.rs`;
Session spawning keeps native executables first and guards other batch shims.

**Drop proposal**:
An accepted window drop and its predicted layout rectangle, resolved from the
current manager state for both preview and release.
_Avoid_: snap zone.

**Commit details**:
The selected Git History commit's message, identity, containing branches, and
changed-file tree. Owned by `src/git_history/details.rs`; each selection has a
cancellable background request and a private reply channel so stale results
cannot replace the current selection. Merge changes use the first parent.

**Diff window**:
The per-Project, reused side-by-side view of one changed file at one commit,
opened from Commit details. Owned by `src/git_history/diff_view.rs`; rows are
padded so both sides align, and git's single whole-file hunk is the only
accepted input.
