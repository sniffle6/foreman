# Project `+` launcher menu — design

Status: approved design, not built. Date: 2026-09-23.

## Problem

Foreman has grown per-project windows — Chat, Board, Plan, Git history — that
can only be opened by leader chord (G / K / L / H). No mouse surface reaches
them:

- the project titlebar `+` is a hover menu with New project + PS/CMD/SH
  terminals, and its **click is inert** (the interact response is never read);
- the panel's project-row `+` click spawns a default-shell terminal, no menu;
- the landing picks an agent only for a *new* project.

The number of windows is not the problem; discoverability is. A separate
"desktop" icon view was considered and rejected: in a tiling WM it is always
covered by the work, and it would be a third surface kind next to tiled and
floating.

## Behavior

Both `+` buttons (project titlebar and panel project row) behave the same:

- **Click** — new terminal in that project, `Settings::default_shell`.
  (Titlebar: new behavior. Panel: unchanged.)
- **Hover** — opens the launcher menu below (same `hover_menu` open/close
  rules as today). Clicking the `+` while its menu is open spawns the terminal
  *and* closes the menu.

```
 [+]
 ┌──────────────────────────────┐
 │ AGENTS                       │
 │   Claude                     │
 │   Codex                      │
 │   Grok                       │
 │ SHELLS                       │
 │   PowerShell       (default) │
 │   CMD                        │
 │   SH                         │
 │ PROJECT                      │
 │   Chat          ●  Ctrl+B G  │
 │   Board            Ctrl+B K  │
 │   Plan             Ctrl+B L  │
 │   Git history      Ctrl+B H  │
 │ ──────────────────────────── │
 │   New project…               │
 └──────────────────────────────┘
```

- **Agents** (v1): a new terminal in the default shell with the agent's
  command injected — the exact recipe `WindowManager::add_project_with_command`
  uses (`spawn` + `Session::inject_input(cmd)`), so quitting the agent drops
  back to a prompt. Commands come from landing's `SessionKind::launch_command`.
  All three are always listed; clicking one that is not on PATH queues an error
  toast `"<Agent> isn't installed"` (`crate::notify::queue`), same as landing.
  Do **not** call `SessionKind::installed()` while painting — it stats PATH.
- **Shells**: one row per shell; the default one carries a dim `(default)`.
- **Project tools**: call the existing `open_*_window` functions, which are
  already per-project singletons that resurface a buried/minimized window. A
  dim `●` marks a tool whose window is already open. The right-aligned hint is
  `leader.pretty() + " " + chord.pretty()` from the live keymap
  (`keymap::live(ctx)`, `Keymap::chord_for`); no hint when unbound. Never
  hardcode chords — they are rebindable.
- **New project…** below a divider: desktop-scope, the existing
  `Act::OpenProjectPicker`.

Every launch surfaces and focuses the project (same as the panel `+` today via
`surface_target`). Placement follows `add_terminal` (`tile_new` +
`new_windows_float`).

## Shape

### One list — `src/launcher.rs` (new)

```rust
pub enum Launch { Agent(SessionKind), Shell(Shell), Tool(Tool), NewProject }
pub enum Tool { Chat, Board, Plan, GitHistory }   // Tool::command() -> keymap::Command
```

Plus a pure builder that returns the ordered menu entries (headers, items,
divider) given the live keymap, the default shell, and "which tools are open".
Both `+` surfaces build from it; the future command palette reads the same
list. Unit-test the builder: order, groups, hint text with bound/unbound
chords, default-shell mark.

### Grouped `hover_menu`

Today `hover_menu(ui, id, anchor, area, &[(&str, Act)], align_right)` is a flat
list private to `src/wm.rs`. Generalize it (move to its own module so
`panel.rs` can call it) to:

- items of `Header(label) | Item { label, hint, mark, act } | Divider`,
  generic over the act type (`Act` for `⋯`, `Launch` for `+`);
- build items lazily (closure called only when the menu is open) — header
  chrome runs every frame for every project, so no per-frame `Vec` when closed;
- a `dismiss: bool` input so an anchor click closes it;
- width fits label + hint columns.

The `⋯` menu keeps working unchanged (all `Item`s, no hints).

### Wiring

- Replace `Act::AddTerm(WinId, Shell)` and `Act::AddTermPath(TargetPath)` with
  one `Act::LaunchPath(TargetPath, Launch)`; `apply_add_term_path` becomes
  `apply_launch_path`, which resolves the project tab exactly as today, then
  dispatches on `Launch`. The titlebar builds
  `TargetPath { project: id, ptab: None, window: None, tab: None }`.
- Panel: `PanelBtn::AddTerm` stays the click; add a `launch:
  Option<(TargetPath, Launch)>` field drained in `drain_panel_acts`. Drop the
  `"New terminal"` hover tooltip — the menu replaces it. For the `●` marks,
  `ProjectEntry` gains the open-tools set, filled in `panel_model()`.
- Panel placement: pass the screen rect (not the panel rect) as the clamp
  `area`, so a right-docked panel's menu opens leftward onto the desktop; the
  existing vertical flip covers bottom docks. Strip/rail modes still have no
  `+`.

## Out of scope

- Command palette (separate card; reuses `launcher.rs`).
- Hover-open delay — add only if the taller menu proves annoying in use.
- Settings / Task manager / Image viewer in the menu (desktop-scope or
  path-requiring).

## Done means

- `cargo test` green, including new `launcher` builder tests and updated wm
  tests that used `Act::AddTermPath`.
- Screenshot evidence (build-screenshot, user-run) of the menu open from the
  titlebar and from a right-docked panel.
- Manual: click `+` → terminal; hover → each group launches; missing agent →
  toast; open tool shows `●` and click focuses the existing window.
- `docs/window-chrome.md` and `docs/task-manager-panel.md` updated (the `+`
  sections).
