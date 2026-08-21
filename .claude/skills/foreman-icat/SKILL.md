---
name: foreman-icat
description: Use when running inside foreman (the FOREMAN env var is 1) and you want to show an image to the human — inline in your pane (icat) or in a persistent viewer window (view): display a screenshot, render a chart, share visual evidence instead of just naming a file path.
---

# Show an image to the human in foreman

**This skill is complete. Do NOT read foreman source or docs to learn icat/view
mechanics — every fact you need is below.**

Precondition: `$env:FOREMAN` is `1`; if not, tell the user this needs to run
inside foreman (icat also renders in kitty/WezTerm, but don't rely on it
elsewhere).

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
- **If your shell output is captured by an agent harness** (e.g. Claude Code
  Bash/PowerShell tool calls — output goes to the model, not the PTY), icat's
  bytes never reach the screen and the human sees nothing. `view` still works:
  it talks to the GUI over the control pipe, not stdout. When in doubt, use
  `view`.

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
  or `foreman close --terminal tN`.
- Exit codes: 0 shown/opened, 2 bad arguments or unreadable/non-PNG file
  (view validates the file client-side before touching the GUI).
- The release foreman.exe is a GUI-subsystem binary: cmd/PowerShell don't
  wait for it or surface its stderr unless output is redirected/piped. A
  silent no-window `view` means the args were bad — rerun with stderr
  captured to see the reason.
- If foreman replies `unexpected argument "icat"`/`"view"` or shows usage,
  the installed foreman predates the verb (icat needs ≥ v0.3.0, view
  ≥ v0.3.2) — tell the user to update foreman, and fall back to naming the
  file path.

## Anti-patterns

- Do NOT `cat`/`Get-Content` an image file into the terminal — that floods
  the pane with binary garbage. icat/view are the only correct ways.
- Don't screenshot-verify silently and describe the result in prose when the
  human asked to see it — show the image.
- Don't icat from a captured-output tool call and claim the human saw it —
  they didn't. Use `view` there.
