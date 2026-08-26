# Project Directories

## What it does

Each project has a working directory. Every terminal you open in that project —
the first one and any you add later — starts in that directory. When you make a
new project you choose its directory in an **address-bar picker**: an editable
path field with inline completion and a dropdown of matching subfolders.

## Why it exists

Before this, every shell started wherever Foreman itself was launched, so a
second terminal in a project did not land next to the first one. Projects are
meant to be per-repo sandboxes, so they need their own directory.

The picker started life as a highlight-and-drill list. It was rewritten around
the path field because that field is the thing you actually want when you
already know where you are going — you can paste or type a full path instead of
walking to it one folder at a time. The tree navigation survived on the arrow
keys for when you don't.

## How to use it

Click the "+" on a project titlebar to get the picker as a centered modal. The
same widget is embedded in the landing screen (see `docs/landing-recents.md`).

**The path field is the source of truth.** Whatever it resolves to is what
Enter opens. Type or paste a path directly, or navigate:

- `↑` / `↓` — move the highlight in the dropdown. `↑` on the top row collapses
  the dropdown, same as Esc.
- `←` — go up one directory.
- `→` — go into the highlighted directory. It never goes up; use `←` for that.
- `Enter` — open a project at the path in the field. If the path is not an
  existing directory, the field flags itself invalid and keeps focus.
- `Esc` — collapse the dropdown. In the "+" modal that also drops the picker.
- `Backspace` / `Home` / `End` / click — ordinary text editing on the field.

As you type a partial name, the remainder of the highlighted match appears
after your text as gray **ghost text**. Matching is case-insensitive prefix
matching against the directories in the base path, not substring or fuzzy.

## Gotchas

- **Arrows drive the tree, not the text caret.** `←` / `→` are consumed by the
  picker whenever the dropdown is open, so they move between directories rather
  than moving the cursor through the path string. Edit the text with Backspace,
  Home/End, or a click instead.
- **Tab does nothing here.** It is deliberately eaten so keyboard focus cannot
  escape the field to a sibling widget while the dropdown is open.
- **Esc collapses, it does not cancel.** The picker reports `Cancelled` and each
  caller decides: the "+" modal drops the picker, the landing just hides the
  dropdown and lets its icons show.
- **Enter accepts the field, not the highlight.** Highlighting a row and
  pressing Enter opens the *field's* path — which is usually the parent. Press
  `→` first to descend into the highlighted folder, then Enter.
- **Only directories are listed, and dotfile folders are hidden** (`.git`,
  `.serena`). Rows are sorted case-insensitively by name.
- **The directory is set once, at creation.** Terminals spawn there, but the
  project has no live "follow the shell's cwd" tracking (that would need OSC 7).

## Key files

- `src/dirpicker.rs` — the pure navigation seam (`split`, `base_dir`,
  `completions`, `ghost`), all unit-tested, plus `DirPicker` and its two egui
  renders: `show` (embedded, used by the landing) and `show_modal` (the "+"
  overlay). `list_dirs` is the filesystem lister the seam is parameterized over.
  Navigation lives in `go_parent` / `go_child` / `current_dir`; `Outcome` is
  what a caller acts on.
- `src/terminal.rs` — `Session::spawn` takes the cwd and sets it on the PTY
  command.
- `src/wm.rs` — `WindowManager.cwd` (per-project dir), `add_project` (creates a
  project at a dir), the `picker` field and the `OpenProjectPicker` act.
- Spec: `docs/superpowers/specs/2026-07-08-directory-picker-redesign-design.md`
  — the rewrite's rejected alternatives, including why accept is not fuzzy.
