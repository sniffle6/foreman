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
What a Win holds — a Terminal, a Project, or a Chat viewer.
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

**Inspection**:
Driving a Session with input (**send**) and reading back its rendered screen
(**snapshot**) over the Control plane — so an agent or a test can verify a
terminal headlessly, without the window or the user's keyboard.
_Avoid_: scraping, probing, automation.

**Snapshot**:
A Session's rendered screen captured as data — the grid as text by default, or
per-cell attributes and cursor on request. A read, never a side effect.
_Avoid_: dump, capture, screenshot (a screenshot is pixels; a Snapshot is the grid).

**Chat room**:
A Project's shared, append-only message log that member Sessions post to and
receive from, used to coordinate agents.
_Avoid_: channel, thread, log.

**Member**:
A Session that belongs to a Project's Chat room.
_Avoid_: participant, client.

**Dispatcher / Worker**:
Roles in a Chat room — the Dispatcher hands out work; a Worker is a Session
dispatched to do it.
_Avoid_: master/slave, manager/agent.

**Crew board**:
The Chat viewer's presence panel, showing each Member and how recently it was
heard from.
_Avoid_: roster, sidebar, presence list.

### Seams & patterns

These name deliberate seams — places where behaviour is isolated behind a small
interface so it can be changed or tested in one spot.

**Deferred action**:
A window interaction recorded during the draw pass and applied after it, because
the draw cannot mutate nested Window managers mid-render.
_Avoid_: command, event (overloaded), callback.

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

**Caret gate**:
The step that decides which cell the painted caret rests at, given a stream of
cursor observations and the user's recent typing. It de-jitters a full-screen
program that moves its cursor mid-redraw: it adopts a new resting cell only once
the cursor has stopped moving for a beat (cursor stability, distinct from
Quiescence settle's output stability), follows a single-row step while the user
is actively editing, and holds far jumps and self-running animations. The caret
is what Foreman paints; the cursor is the program's, owned by the grid model.
_Avoid_: cursor (that's the model's), blink, debounce.
