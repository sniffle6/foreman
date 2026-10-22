---
name: foreman-icat
description: Use when running inside Foreman (the FOREMAN env var is 1) and a Codex agent wants to show an image to the human — inline in your pane (icat) or in a persistent viewer window (view): display a screenshot, render a chart, share visual evidence instead of just naming a file path.
---

# Show an image to the human in foreman

Everything needed to show images is below. Foreman's source and docs are for
changing foreman; read them only to diagnose behavior this skill does not
explain.

Precondition: `$env:FOREMAN` is `1`; if not, tell the user this needs to run
inside foreman (icat also renders in kitty/WezTerm, but don't rely on it
elsewhere).

Codex sandbox: `view` talks to Foreman over a named pipe the sandbox blocks
("Access is denied (os error 5)"); run it with escalated/approved execution
from the start. `icat` only writes to stdout and needs no escalation.

## Two verbs — pick the right one

    & $env:FOREMAN_EXE icat "C:\path\to\image.png"   # inline, in your pane
    & $env:FOREMAN_EXE view "C:\path\to\image.png"   # persistent window

bash: `"$FOREMAN_EXE" icat "/c/path/to/image.png"` (same for `view`).

- **`icat`** prints the image into your own pane's output flow. Quick glance
  alongside your text. Ephemeral: it scrolls with the buffer and disappears
  on `clear` or a pane resize — re-run it if the human asks again.
- **`view`** opens the image in its own window in your project: it tiles,
  floats, and tabs like a terminal, rescales on resize, zooms (Ctrl+Scroll)
  and pans (drag), shows in the sessions panel, and survives an app restart.
  Use it for anything the human should keep, compare, or resize.
- **If your shell output is captured by an agent harness** (output goes to
  the model, not the PTY), icat's bytes never reach the screen and the human
  sees nothing. `view` still works: it talks to the GUI over the control
  pipe, not stdout. When in doubt, use `view`.

## Facts

- **PNG only** (both verbs). JPEG/GIF/WebP are rejected with a clear error —
  convert first if needed.
- icat sizes to your pane's width, aspect kept; very large screenshots are
  downscaled before display, so a 4K capture is fine. `--cols N` forces a
  smaller width (e.g. `--cols 40` for a thumbnail). The prompt lands below
  the image automatically.
- view takes `--project P` to open in another project (default: your own,
  from `FOREMAN_PROJECT_ID`). Success prints
  `{"ok":true,"terminal":"tN","project":"pN"}`; close the window with its ✕
  or `foreman close tN` (`close` takes bare `tN` ids — it has no
  `--terminal` flag, and any unknown `--` flag exits 2).
- Exit codes: 0 shown/opened; 2 bad arguments or unreadable/non-PNG file
  (view validates the file client-side before touching the GUI). Exit 1
  means different things per verb: for `view`, foreman was unreachable or
  refused the request; for `icat` — which never touches the control pipe,
  it just writes kitty-graphics bytes to stdout — it means **stdout could
  not be written**, i.e. the harness-capture / redirect failure above, not
  a dead GUI.

## Showing it well

- Show image files with icat or view only; printing one with
  `cat`/`Get-Content` floods the pane with binary.
- When the human asks to see something, show the image itself rather than
  describing a screenshot in prose.
