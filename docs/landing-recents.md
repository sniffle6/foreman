# Landing: Recent Projects

The landing page (shown when `FOREMAN_LANDING` is set and no project is
*visible* — none open, or every project minimized) keeps a short list of the
projects you actually opened, so getting back into one is a single click —
or Tab, arrows, Enter. The Sessions panel stays docked at its normal size
beside/above the landing; click a minimized row there to restore it.

## What it does

- Under the icon row, a `Recent` list shows up to 5 projects, newest first.
- Each entry remembers **how** it was opened: Claude, Codex, or plain
  terminal. Opening it again launches the same thing in the same directory.
- Recorded automatically whenever you deliberately open a project: Enter or
  an icon on the landing, or the leader-key project picker.

## How to use it

- **Mouse:** click a row. Hover highlights it.
- **Keyboard:** the landing is three zones — path field, agent buttons,
  recents. `Tab` cycles them forward, `Shift+Tab` backward (recents skipped
  while empty). `↑`/`↓` walk the same order as one column: `↓` in the field
  opens the directory popup, `↓` past the popup's last row lands on the
  buttons, `↓` again enters recents. In the buttons zone `←`/`→` pick
  Claude/Codex/Terminal and `Enter` launches the field's path with that
  kind; in recents `↑`/`↓` move the `>` marker and `Enter` reopens the
  entry. `Esc` (or just typing) returns to the field from anywhere.

## Where it lives on disk

`%APPDATA%\foreman\recents.json` — a flat JSON list of `{path, kind}`
entries, written atomically on every recorded open. Delete the file to clear
the list; a missing or corrupt file just means an empty list.

## Gotchas

- **Kind is a plain string** ("claude" / "codex" / "terminal"), not an enum.
  A kind this build doesn't recognize (written by a newer foreman) opens as a
  plain terminal instead of breaking the file.
- **Missing directories are hidden, not deleted.** Unplug a drive and its
  projects vanish from the list; plug it back in and they return. The check
  runs when the landing (re)appears, not every frame — a dead network path
  can't stall the GUI, but a drive plugged in mid-landing shows up next visit.
- **What is NOT recorded:** the flag-off startup auto-project (implicit, not
  a choice) and `foreman open` from the CLI (it spawns terminals inside an
  existing project — it never creates one).
- **Dedup is case-insensitive** (`H:\Foo` and `h:\foo` are the same project);
  re-opening moves an entry to the top and adopts the new kind.
- The list caps at 5 — one number, no hidden extras on disk.

## How recording works (one paragraph)

`WindowManager::add_project` / `add_project_with_command` push
`(cwd, command)` onto a small drain; `App` empties it once per frame with
`take_opened()` and records each open into `Recents` (mapping the command's
program stem to a kind string). The window engine never learns what a
"recent" is — it only reports opens.

## Key files

- `src/recents.rs` — MRU model (`push` is pure and unit-tested) +
  `recents.json` persistence via `config.rs` helpers; `kind_of_command`.
- `src/landing.rs` — the list render, Tab focus zone (`Zone`, pure `step`
  state machine), kind-string → `SessionKind` mapping.
- `src/wm.rs` — the `opened` drain + `take_opened()`.
- `src/main.rs` — `App.recents`, per-frame drain recording, startup discard.
- Spec: `docs/superpowers/specs/2026-07-08-landing-recent-projects-design.md`
