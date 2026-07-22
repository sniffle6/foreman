# Epic — Keyboard-Driven Control (tmux-style)

**Status:** designed, not started. Phases are independent sessions; do them in order
(2 depends on 1, 3 depends on 2).

**Read first:** `docs/HANDOFF.md` (architecture + gotchas), then this file. Each phase
section below is written to be picked up cold in a fresh session.

---

## 1. Why (the bet)

Foreman's bet is **C: the fastest, leanest native multiplexer** — fast to load, fast
to use, fast to develop. We are *not* chasing the agent-orchestration crowd
(worktree isolation, diff review, agent-state dashboards). What makes a multiplexer
feel like tmux is **keyboard-driven control**: move focus, tile, switch projects
without touching the mouse.

Decision history (settled with the user):
- Considered putting model/token status in window headers → **rejected**: the CLIs
  already render that in-pane; repeating it is noise + a sync problem.
- Strategic direction = **C** (fast multiplexer), not attention-routing or
  orchestration.
- Interaction model = **A**: a tmux-style **leader/prefix** key, then a command key.
  Rejected direct chords (collide with the AI CLIs running inside the terminals) and
  hybrid (falls out of the rebinding system later for free).
- A `/grug-review` cut the original scope: the data-driven keymap + serde + in-app
  rebinding editor are **deferred**. Phase 1 ships 100% of the daily *usage* value
  with a hardcoded `match` and zero new dependencies. The editor is earned, not
  assumed.

---

## 2. The binding table (final)

Leader (default): **`Ctrl+b`** — a single constant; the one thing worth making easy to
change early. (`Ctrl+Space` is the recommended alt if `Ctrl+b` collisions annoy.)
After the leader is pressed-and-released, command mode is *armed* and the next chord
is a command (consumed, never sent to the PTY). An unbound chord cancels and is
swallowed.

**Movement & snap matrix** — orthogonal modifiers: **plain = terminal focus**,
**Shift = snap**, **Ctrl = project**. Direction = arrow keys.

| | Focus | Snap |
|---|---|---|
| **Terminal** (inner) | `←↓↑→` | `Shift+←↓↑→` |
| **Project** (outer) | `Ctrl+←↓↑→` | *(deferred — long tail, nobody snaps whole projects)* |

`h/j/k/l` may also be bound to terminal-focus for vi users (Phase 1 optional).

**Action keys** (after leader):

| Binding | Action |
|---|---|
| `c` / `P` | new terminal / new project (dirpicker) |
| `x` / `Ctrl+x` | close terminal / close project |
| `z` / `Ctrl+z` | maximize (zoom) toggle — terminal / project |
| `,` | rename focused window |
| `Tab` / `Ctrl+Tab` | toggle to last-focused terminal / project |
| `[` | scrollback / copy mode (scrollback already exists) |
| `?` | bindings overlay (read-only cheat sheet) |

`Ctrl+z`/`Ctrl+x` are safe despite meaning suspend/cut in a shell, because command
mode consumes them before they reach the PTY.

---

## 3. Shared architecture context (all phases)

Foreman is a **recursive compositor**: one `WindowManager` runs the desktop; each
project window's content is *another* `WindowManager` of terminals
(`Content::Project(Box<WindowManager>)`). Focus cascades — `show(.., active, ..)`
ANDs down the tree so exactly one leaf terminal reads the keyboard. Window rects are
**local** to each manager's `area`.

Code anchors (line numbers drift — search if stale):

- **`src/main.rs:23` `App::ui`** — calls `self.desktop.show(ui, area, true, …)` once
  per frame. This is the outermost point; leader interception is cleanest at the top
  of the desktop `show` (or here) **before** the recursion reaches terminals.
- **`src/wm.rs:216` `WindowManager`** fields: `windows: Vec<Win>`, `focused:
  Option<WinId>`, `z`, `split`, `cwd`, `picker: Option<DirPicker>` (modal pattern),
  `renaming`. New state for this epic goes here (`armed`, `last_focused`, overlay
  flags).
- **`src/wm.rs:323` `focus(id)`** — raises z + sets `self.focused`. Reuse for all
  focus moves.
- **`src/wm.rs:334` `show(ui, area, active, base) -> bool`** — the engine loop;
  returns whether any window was interacted with (parent uses it to bubble focus up).
  Handle commands at the **top of this fn, before the render loop**, so you mutate
  state directly and skip the deferred `Act` dance.
- **`src/wm.rs:200` `enum Act`** — deferred actions drained after render
  (`Focus/Close/Min/Max/Restore/AddTerm/OpenProjectPicker`). The Max/Restore handlers
  (~`wm.rs:871`) are the zoom toggle. `add_terminal` (`wm.rs:283`) /
  `add_project` (`wm.rs:297`) spawn windows.
- **`src/wm.rs:40` `enum Zone`** + **`zone_rect` (`wm.rs:87`)** — snapping. Snap a
  window by setting `w.snap = Some(zone)` and `w.prev = Some(current rect)`; the show
  loop refits snapped windows to `zone_rect` each frame. Arrow → `Zone::Left/Right/
  Top/Bottom`; `z` zoom → `Zone::Max` toggle.
- **`src/terminal.rs:292` `read_input`** — the focused terminal forwards **every**
  keystroke to the PTY via `ui.input`. Command mode MUST stop keys here. Preferred
  mechanism: at the top of the frame, when armed, drain matching key events from egui
  input (`ui.input_mut(|i| …)` / `consume_key`) so terminals never see them. **Verify
  the egui 0.34 input-mutation API** — see HANDOFF gotchas (Ctrl+C/V may arrive as
  `Event::Copy`/`Paste`, not key events).
- **`src/dirpicker.rs`** — the keyboard-driven modal overlay pattern. Mirror it for
  the `?` overlay (Phase 1) and the settings editor (Phase 3).

**Routing commands to the right level** (the one real subtlety): terminal-level
commands operate on the *focused project's child* `WindowManager`
(`self.focused` → `Content::Project(wm)` → act on `wm`); project-level commands
operate on `self` (the desktop). Resolve the focused child once, then dispatch.

**Directional focus algorithm:** from the focused window's `rect` center, pick the
nearest window whose center lies in the requested direction (dominant-axis distance,
tie-break by cross-axis). Pure geometry on local rects — no new state.

**Build/verify (from HANDOFF):** GNU toolchain; kill the running app before building
(`Access is denied (os error 5)`); `cargo build 2>&1 | Select-Object -Last 20`;
verify visually by running the exe + screenshot (GUI can't be seen from the
terminal).

---

## Phase 1 — Leader + commands + `?` overlay  *(no deps, no persistence)*

**Goal:** full keyboard control with hardcoded bindings. Ships the entire usage value.

**Scope:**
1. **Leader state machine.** Add `armed: bool` (+ a short visual cue when armed, e.g.
   a corner pill, optional) to the desktop `WindowManager`. Top of frame: if not armed
   and the leader chord arrives → arm + consume. If armed → look up the next chord,
   run the command, disarm + consume. Armed + unknown → disarm + consume (swallow).
   No timeout, no multi-key sequences — keep it dumb on purpose.
2. **Key interception** so armed commands never reach the PTY (`terminal.rs:292`).
3. **Command dispatch** as a hardcoded `match (key, mods) -> action`, binding and
   behavior in the same arm (locality). Implement the table in §2 minus deferred cells.
4. **Terminal-level ops:** directional focus (geometry), directional snap
   (`Zone` + `w.snap`), zoom `z`, close `x`, rename `,` (reuse `renaming`), new term
   `c`, last-terminal `Tab` (needs a `last_focused: Option<WinId>` per manager).
5. **Project-level ops:** directional focus among desktop windows (`Ctrl+arrows`),
   `Ctrl+z` maximize project, `Ctrl+x` close project, `Ctrl+Tab` last project,
   `P` new project (reuse `OpenProjectPicker`).
6. **`?` overlay:** read-only cheat sheet of the current bindings, dismissed by any
   key. Mirror `dirpicker.rs`. This is the discoverability surface.

**Out of scope:** persistence, config files, serde, the rebinding editor, project-snap,
numeric jump (`0–9`). Do not add dependencies.

**Acceptance:**
- With a project containing ≥2 terminals: `Ctrl+b` then arrows moves terminal focus;
  `Shift+arrows` snaps; `z` zooms; `c` adds a terminal; `x` closes; `,` renames.
- With ≥2 projects: `Ctrl+b Ctrl+arrows` moves project focus; `Ctrl+z`/`Ctrl+x` zoom/
  close project; `P` opens the picker.
- Typing in a terminal is unaffected when command mode is *not* armed (verify a shell
  command + an interactive CLI still get Ctrl/arrow keys).
- `Ctrl+b ?` shows the overlay; any key dismisses it.
- Verify by running the exe + screenshots; show evidence.

---

## Phase 2 — JSON persistence  *(depends on Phase 1)*

**Goal:** make bindings editable by hand without recompiling. Small, additive.

**Scope:**
1. Add `serde` + `serde_json`. Introduce a `Command` enum (all Phase 1 actions) and a
   serializable `Chord { key, ctrl, shift, alt }`. Replace the hardcoded `match` with
   a `Keymap { leader: Chord, table: Map<Chord, Command> }`. **Defaults still defined
   in code**; user file is loaded and merged *over* defaults so future commands always
   get a default.
2. Load `%APPDATA%\foreman\keybindings.json` at startup (read `APPDATA` directly —
   Windows-only, no `dirs` dep). Missing/corrupt → fall back to defaults without
   crashing (log it). No write path yet (hand-edited file).

**Acceptance:** editing the JSON changes bindings on next launch; a malformed file
falls back to defaults and the app still runs; deleting the file restores defaults.

**Note:** the `Keymap` data model + serde exist *only because* of persistence/editing.
Do not introduce them in Phase 1.

---

## Phase 3 — In-app rebinding editor  *(depends on Phase 2; build only if wanted)*

> **STATUS (2026-07): SHIPPED, and no longer a modal.** The rebinding editor was
> built in `src/settings.rs` (`SettingsView`) and, after the settings-menu window
> conversion, folded into the settings window's **Keybindings pane** — it renders
> inline, not as a desktop modal. Chord capture briefly grabs all input and the
> leader is suppressed via `WindowManager::settings_capturing()`. The "modal
> overlay" scope below is the original design and is superseded; see
> `docs/settings-menu.md` and `docs/superpowers/specs/2026-07-21-keybindings-pane.md`.
> The flow (leader row, grouped list, capture → conflict) is as described and still accurate.

**Goal:** edit bindings inside Foreman. Earn this — the JSON file may be enough.

**Scope (original design — now inline, see status note above):** a desktop-level
**modal overlay** (settings are global, not project-confined — mirror `dirpicker.rs`,
not a `Content` window). Opened via the `?` overlay's *Edit* affordance and/or an
`OpenSettings` command.
- Top: editable **Leader** chord.
- Grouped list (**Projects / Terminals / Actions**): each row = label · current chord
  (pretty `Ctrl+Shift+→`) · Rebind · reset-one.
- **Rebind flow:** activate row → "press keys…" capture → next chord captured →
  conflict check (chord already bound → inline "conflicts with *X* — replace?") →
  assign + write to disk.
- Footer: **Reset all to defaults**, Close.
- Keyboard-driven (`j/k`/arrows select, `Enter` rebind, `Esc` close); mouse works too.
- **Conflict rule:** one chord → one command; overwriting unbinds the old. Leader
  can't double as a command chord.

**Acceptance:** rebind a command in-app, see it persist across restart; conflicts are
caught and resolved; reset-all restores defaults.
