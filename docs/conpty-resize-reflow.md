# Known limitation: ConPTY resize/recall cursor corruption (Windows)

**Status:** Known upstream bug, not fixed in foreman. Workaround: `Ctrl+L`.
**Upstream:** [microsoft/terminal #18725](https://github.com/microsoft/terminal/issues/18725) (open, unshipped).

## Symptom

Resize a terminal pane **narrower** while a **wrapped** prompt/input line is on
screen and there is command output above it, then trigger a PSReadLine re-render
(most reliably **Up-arrow history recall**). The recalled line renders one row per
wrapped prompt-row too high, overwriting/stranding the prompt (e.g. a
`…\foreman>` fragment left mid-line, cursor misplaced). It persists until a full
redraw — `Ctrl+L` heals it; an ordinary keystroke does not. Triggers on any
width-shrinking reshape: window-edge drag, a split/divider that narrows a pane,
or up-arrow after a reshape.

## Root cause (this is NOT a foreman double-reflow)

An earlier diagnosis blamed a "double reflow" in `Session::resize`
(`term.resize()` + `master.resize()`). **That is wrong** — disproven by
byte-level tracing of the ConPTY↔foreman stream and an A/B against Windows
Terminal.

The real cause is **ConPTY's reflow diverging from the hosting terminal**
(microsoft/terminal #18725). On resize, ConPTY reflows its internal buffer with
conhost's algorithm, which differs from `alacritty_terminal`'s. ConPTY then
reports a cursor position via `GetConsoleCursorInfo` that is inconsistent with its
own VT output — off by exactly the prompt's wrapped-row count. PSReadLine reads
that wrong cursor for history-recall and renders into the old prompt.

Windows Terminal avoids this **only** because it (a) bundles a newer
quirk-honoring ConPTY/OpenConsole and (b) replicates conhost's reflow math
byte-for-byte ([microsoft/terminal PR #4741](https://github.com/microsoft/terminal/pull/4741)).
`portable-pty` 0.9.0 already sets `PSEUDOCONSOLE_RESIZE_QUIRK`; the gap is the
reflow algorithm, which we cannot cheaply match without forking
`alacritty_terminal`.

## Evidence (all reproduced)

- The in-box ConPTY's resize repaint is itself **clean** (places the prompt input
  at e.g. row 24); the divergence is in the **recall**, which ConPTY emits at
  `ESC[23;22H` (row 23). foreman renders both faithfully — the bytes disagree.
- foreman sends ConPTY nothing but the Up key around the recall; no DSR query is
  involved. The child reports the correct (narrow) width. Plain typing after a
  resize is correct — only history-recall misfires.
- The offset equals the prompt's wrapped-row count, consistent across all widths
  and heights where the prompt wraps.
- **Windows Terminal renders the identical PowerShell scenario cleanly.**
- win32-input-mode (`ESC[?9001h`) is unrelated (input fidelity only).

## What was tried and rejected ("let ConPTY own the redraw")

Vendored `portable-pty` with `PSEUDOCONSOLE_RESIZE_QUIRK` dropped + an experimental
grid-reset on resize + sideloading the newer ConPTY (NuGet
`Microsoft.Windows.Console.ConPTY` 1.24.260512001). All four combinations failed:

1. newer ConPTY + quirk on → no resize repaint; alacritty reflow diverges → gap.
2. newer ConPTY + quirk off → still no resize repaint → same gap.
3. in-box ConPTY + quirk off + reflow → clean repaint but recall `ESC[23;22H` → mashup.
4. in-box ConPTY + quirk off + grid-reset (ConPTY sole layout) → recall `ESC[23;22H` → mashup.

Because ConPTY's `GetConsoleCursorInfo` is internally inconsistent with its own VT
repaint, no frontend redraw-ownership strategy fixes it.

## Paths to a real fix (not pursued — cost/benefit)

- **Full fix (large):** replicate conhost's `ResizeWithReflow` (wrapped-line +
  viewport-top math) in/around `alacritty_terminal`, plus bundle a newer ConPTY.
  This is what Windows Terminal does. Fragile; ongoing maintenance.
- **Partial mitigation:** bundle the newer ConPTY (sideload `conpty.dll` +
  `OpenConsole.exe`). Downgrades "prompt destroyed" → "prompt preserved + minor
  recall gap". Adds ~1 MB to distribution; still not clean.
- **Upstream:** ConPTY-side DSR re-sync after resize (#18725) — unshipped.
