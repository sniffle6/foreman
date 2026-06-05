# Project Directories

## What it does

Each project has a working directory. Every terminal you open in that project —
the first one and any you add later — starts in that directory. When you make a
new project you pick its directory through a keyboard-driven navigator.

## Why it exists

Before this, every shell started wherever Foreman itself was launched, so a
second terminal in a project did not land next to the first one. Projects are
meant to be per-repo sandboxes, so they need their own directory.

## How to use it

- Click the "+" on a project titlebar. A picker opens at the focused project's
  directory (or the process directory if none).
- Navigate:
  - Up / Down — move the highlight
  - Right / Tab — go into the highlighted folder
  - Left — go up to the parent
  - Type — filter the current folder's subfolders
  - Enter — open a project here (the folder shown at the top)
  - Esc — cancel
- To choose a folder as the project directory, go *into* it, then press Enter.

## Gotchas

- Only directories are shown; files are hidden. Dotfile folders (`.git`,
  `.serena`) are hidden too.
- "Enter accepts the current location" — not the highlighted row. The highlight
  is only for drilling in. This is deliberate: you navigate *to* the directory
  you want, then accept it.
- The directory is set once, at creation. Terminals spawn there but the project
  has no live "follow the shell's cwd" tracking yet (that would need OSC 7).

## Key files

- `src/dirpicker.rs` — the picker: navigation logic (`DirPicker`, unit-tested)
  plus the egui modal (`show`).
- `src/terminal.rs` — `Session::spawn` takes the cwd and sets it on the PTY
  command.
- `src/wm.rs` — `WindowManager.cwd` (per-project dir), `add_project` (creates a
  project at a dir), `picker` field + `OpenProjectPicker` act (opens the modal,
  creates the project on accept).
