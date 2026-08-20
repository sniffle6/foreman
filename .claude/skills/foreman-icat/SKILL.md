---
name: foreman-icat
description: Use when running inside foreman (the FOREMAN env var is 1) and you want to show an image to the human — display a screenshot, render a PNG in the terminal, share visual evidence (a chart, a UI capture, a diff image) directly in your pane instead of just naming a file path.
---

# Show an image in your foreman pane

**This skill is complete. Do NOT read foreman source or docs to learn icat
mechanics — every fact you need is below.**

Precondition: `$env:FOREMAN` is `1`; if not, tell the user this needs to run
inside foreman (the command also renders in kitty/WezTerm, but don't rely on
it elsewhere).

## The command

    & $env:FOREMAN_EXE icat "C:\path\to\image.png"

bash: `"$FOREMAN_EXE" icat "/c/path/to/image.png"`.

The image renders inside your own pane, where the human can see it. Use it
whenever you produce visual evidence — a window screenshot after a UI change,
a rendered chart, a before/after capture — instead of only mentioning the
file path. The image behaves like ordinary terminal output: it scrolls with
the buffer and is gone once it scrolls off the top — and also disappears on
`clear` or a pane resize — so re-run the command if the human asks to see
it again.

## Facts

- **PNG only.** JPEG/GIF/WebP are rejected with a clear error — convert
  first if needed.
- Sized automatically to your pane's width, aspect ratio kept; very large
  screenshots are downscaled before display, so a 4K capture is fine.
- `--cols N` forces a smaller width in terminal columns (e.g. `--cols 40`
  for a thumbnail next to your text).
- The prompt lands below the image automatically; nothing else to do.
- Exit codes: 0 shown, 2 bad arguments or unreadable/non-PNG file.
- If foreman replies `unexpected argument "icat"` or shows usage, the
  installed foreman predates icat (< v0.3.0) — tell the user to update
  foreman, and fall back to naming the file path.

## Anti-patterns

- Do NOT `cat`/`Get-Content` an image file into the terminal — that floods
  the pane with binary garbage. icat is the only correct way.
- Don't screenshot-verify silently and describe the result in prose when the
  human asked to see it — show the image.
