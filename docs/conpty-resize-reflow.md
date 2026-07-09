# Known limitation: ConPTY resize/recall cursor corruption (Windows)

**Status:** Cursor desync is mitigated by the bundled ConPTY
1.25.2605.12002-preview; redraw/content divergence remains. `Ctrl+L` is still
the full-redraw workaround.
**Upstream:** [microsoft/terminal #18725](https://github.com/microsoft/terminal/issues/18725)
was closed by [#19535](https://github.com/microsoft/terminal/pull/19535).

Three manifestations are documented here: width-shrink + history recall
(partially mitigated by upstream cursor synchronization), height-grow + plain
typing (fixed in Foreman by `resize_anchored`), and width drag through wrap
overflow (cursor mitigated; lost ConPTY content is not reconstructed).

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
conhost's algorithm, which differs from `alacritty_terminal`'s. Older builds
then returned a cursor through `GetConsoleScreenBufferInfo(Ex)` that was
inconsistent with the hosting terminal's VT grid. PSReadLine used that position
for history recall and rendered into the old prompt.

Windows Terminal avoids the full class because it also replicates conhost's
reflow math byte-for-byte
([microsoft/terminal PR #4741](https://github.com/microsoft/terminal/pull/4741)).
Foreman's bundled ConPTY now provides the narrower upstream mitigation: after a
successful resize it marks its cursor suspect, then lazily asks the host for a
cursor report before the next screen-buffer-info call returns. This aligns the
cursor but does not make the two buffers contain the same rows.

## Evidence (all reproduced)

- The in-box ConPTY's resize repaint is itself **clean** (places the prompt input
  at e.g. row 24); the divergence is in the **recall**, which ConPTY emits at
  `ESC[23;22H` (row 23). foreman renders both faithfully — the bytes disagree.
- With the older ConPTY, Foreman sent nothing but the Up key around recall; no
  DSR query was involved. The child reported the correct narrow width. Plain
  typing after a single **width-only shrink** was correct — history recall was
  the reliable trigger.
  (Typing after a **height grow** is NOT safe — see the second manifestation
  below. Typing after a **width drag through wrap overflow** is NOT safe either
  — see the third manifestation.)
- The offset equals the prompt's wrapped-row count, consistent across all widths
  and heights where the prompt wraps.
- **Windows Terminal renders the identical PowerShell scenario cleanly.**
- win32-input-mode (`ESC[?9001h`) is unrelated to the original reflow cause,
  but #19620 is required so the new CPR is captured instead of leaking as input.

## Second manifestation: height-grow + scrollback (typed echo lands mid-screen)

**Symptom (2026-07-08):** make a pane **taller** while there is scrollback, then
type. The typed characters and the caret appear rows *above* the prompt, in the
middle of old output. No recall needed — plain typing misfires. `Ctrl+L` heals.

**Root cause (byte-level, reproduced):** on a height grow ConPTY emits **zero
bytes** — no repaint. Its internal layout keeps content anchored where it was
(prompt row unchanged, blank rows appear below). `alacritty_terminal` instead
**pulls lines back from scrollback** to fill the new rows, moving the prompt to
the bottom. The layouts silently diverge by exactly the number of pulled lines.
The next PSReadLine render addresses the cursor **absolutely** in ConPTY's
layout (e.g. `ESC[30;20H`), which in our grid is mid-scrollback — the echo and
caret land there.

**Evidence:** the ignored diagnostic test `resize_typing_probe` in
`src/terminal.rs` reproduces this headless (spawn PowerShell → 40 lines of
output → resize 100x30 → 80x45 → type `sdfs`) and dumps grid, cursor, and the
raw ConPTY byte stream per phase. Run:
`cargo test --release resize_typing_probe -- --ignored --nocapture`.
Observed: resize-repaint phase = empty byte stream; typed-echo phase =
`ESC[30;20H … sdfs` while our prompt sits at row 45 — offset 15 = exactly the
height delta (all 15 new rows were filled from history).

**Fixed (2026-07-08):** unlike the wrapped-line reflow divergence above, this
one does not require conhost's reflow math — it is a pure policy difference
("pull from history on grow" vs "anchor and extend below"). `resize_anchored`
(`src/terminal.rs`, called from `Session::resize`) cancels alacritty's history
pull after a height grow: it measures the pull as the cursor-line delta across
the height step (the column step runs separately first so rewrap can't pollute
the measurement), scrolls the pulled lines back into history via
`Grid::scroll_up` on the full screen, rotates any live selection back alongside
the content (`Term::resize` had rotated it to track the pull), and restores the
cursor + saved cursor. Height **shrink** was probed clean (both sides push into
history the same way) and is untouched, as are width-only changes.
Regression tests: `height_grow_anchors_content_instead_of_pulling_scrollback`,
`height_grow_keeps_selection_on_its_content` (pure) and
`typed_echo_lands_on_the_prompt_after_a_height_grow` (live ConPTY,
ignored/diagnostic).

With the bundled 1.25 pair, the first later screen-buffer query also produces a
DSR/CPR exchange. The 2026-07-09 trace answered `ESC[30;20R`; typed echo then
landed on the anchored prompt at row 30. `resize_anchored` remains necessary for
programs that do not make the API call which triggers #19535.

**TODO (tracked follow-up, not fixed):** the compensation only reaches the
*active* grid — alacritty exposes no public accessor for the inactive one. A
height grow while an alt-screen app runs (vim/less/htop, agent TUIs) leaves the
*primary* grid pulled and uncompensated; quitting the app then reproduces the
mis-anchored prompt. A full fix needs the inactive grid compensated too
(e.g. swap, compensate, swap back — invasive) or an upstream accessor.

## Third manifestation: width drag through wrap overflow (typed echo lands mid-screen)

**Symptom (2026-07-08, reproduced):** drag a pane's width smaller then bigger
while on-screen rows are wide enough to wrap when narrow and the wrapped total
exceeds the pane height. Afterwards, typed input and the caret land rows above
the prompt (a torn wrap fragment is usually visible nearby). `Ctrl+L` heals.

**Root cause:** the scrollback asymmetry again, through the width axis. ConPTY's
buffer is viewport-sized — when narrowing wraps content past the pane height,
the overflow scrolls out ConPTY's top and is **gone**; alacritty parks the same
rows in scrollback. On widening, alacritty's rewrap re-joins wrapped lines
across the history boundary and the content comes back; ConPTY unwraps only
what it kept, so its cursor settles rows higher than ours. The next child
repaint addresses ConPTY's layout. Unlike the height-grow case, the re-join is
intrinsic to reflow — there is no separable "pull" to cancel — so this is NOT
cheaply fixable; it needs the conhost-parity reflow (or the newer-ConPTY
partial mitigation) described below.

**Evidence:** `resize_drag_probe` (`src/terminal.rs`) steps the width
100→60→100 at frame cadence over wrapping rows, then types. The original dated
run had echo at row 15 and the prompt at row 29. A 2026-07-09 control run also
captured the inverse visible split (old prompt row 15, new echo row 29), while
isolated repeats landed cleanly; the exact artifact is timing-sensitive.
The 1.25 pair aligned prompt and echo in four of four captured runs. That proves
cursor mitigation, not content retention: CPR cannot restore rows ConPTY already
dropped. Height never changes, so `resize_anchored` is not involved.
`Session::resize` also writes
`<<RESIZE …>>` markers (old→new size, measured pull, cursor, PTY result) into
the `FOREMAN_RX_DUMP` stream for attributing live repros.

## Upstream and bundled-pair verification (2026-07-09)

The version number alone was misleading. The stable
`Microsoft.Windows.Console.ConPTY` **1.24.260512001** pair previously in
`assets/conpty/` was published after #19535 merged, but its official PDB maps to
the 1.24 servicing commit `b4e69c6`. That source contains none of #19535's dirty
cursor, `WriteDSRCPR`, or synchronization code.

The bundled **1.25.260512002-preview** pair maps through Microsoft's symbol
server to microsoft/terminal commit
[`8214f66`](https://github.com/microsoft/terminal/commit/8214f66a61e17cc49025b58629e649225b73abc5).
It contains #19535 plus the later active-buffer and Win32-input fixes
([#20095](https://github.com/microsoft/terminal/pull/20095),
[#19620](https://github.com/microsoft/terminal/pull/19620)), and cursor recovery
after unknown VT sequences such as kitty APC
([#20009](https://github.com/microsoft/terminal/pull/20009)). Both files match
the official NuGet package and have valid Microsoft Authenticode signatures;
exact hashes are in `assets/conpty/README.md`.

The mechanism is **demand-triggered**, not an unconditional resize repaint:

1. A successful ConPTY buffer resize marks its cursor position suspect.
2. When the child next calls `GetConsoleScreenBufferInfo(Ex)`, ConPTY emits
   `ESC[6n` and waits (without holding the console lock).
3. Alacritty parses the query against Foreman's post-resize grid and raises
   `Event::PtyWrite`. `Session::pump` writes that CPR through the same PTY
   writer; the captured M1 reply was `ESC[20;48R`, exactly matching the grid's
   one-based cursor. Replies flush after the RX chunk that completed the query,
   and minimized panes continue pumping headlessly, keeping both paths below
   ConPTY's 500ms wait.
4. ConPTY adopts the reported cursor and lets the blocked API call continue.

No query immediately after `Session::resize` is expected. The ignored
`resize_recall_probe` forces the later PowerShell API call with Up-history and
records both sides of the exchange. The Ready latch is unchanged: startup still
requires its first DSR reply plus first child paint; later CPRs only reuse the
already-live response path.

| Scenario | Old 1.24 pair | Bundled 1.25 pair | Verdict |
|---|---|---|---|
| M1: shrink wrapped input, then Up | No DSR; recall one row below the host cursor; aggressive case raised PSReadLine `ArgumentOutOfRangeException` | One DSR + matching CPR; recall moved to the host cursor row, but stale pending text remained | Cursor synchronized; redraw gap remains |
| M2: grow 100x30→80x45, then type | Clean with `resize_anchored` | DSR/CPR observed; echo stayed on row-30 prompt | Pass |
| M3: 100→60→100 wrap overflow, then type | Captured prompt/echo split; isolated repeats were timing-sensitive | Prompt/echo aligned in 4/4 captures | Cursor mitigated; content-loss mechanism remains |

The candidate also passed the kitty APC passthrough canary, emitted a DSR after
that unknown sequence, received Foreman's exact CPR, and completed the blocked
PowerShell screen-buffer query in 3ms rather than the affected host's 500ms
timeout. It did not reintroduce the old ~3-second spawn stall. It is an official
Microsoft package, but it is still a broad preview build rather than an isolated
backport; keep the version pinned and rerun these probes before every future
bump.

## What was tried and rejected ("let ConPTY own the redraw")

Vendored `portable-pty` with `PSEUDOCONSOLE_RESIZE_QUIRK` dropped + an
experimental grid-reset on resize + sideloading ConPTY 1.24.260512001. We now
know that stable servicing build lacked #19535, so these are historical tests of
redraw ownership, not tests of cursor resynchronization. All four combinations
failed:

1. ConPTY 1.24 stable + quirk on → no resize repaint; alacritty reflow diverges → gap.
2. ConPTY 1.24 stable + quirk off → still no resize repaint → same gap.
3. in-box ConPTY + quirk off + reflow → clean repaint but recall `ESC[23;22H` → mashup.
4. in-box ConPTY + quirk off + grid-reset (ConPTY sole layout) → recall `ESC[23;22H` → mashup.

Because the cursor returned through `GetConsoleScreenBufferInfo(Ex)` disagreed
with the hosting grid, no frontend redraw-ownership strategy fixed the old
build. The current DSR/CPR path addresses that coordinate only; it still does not
make the two buffers reflow identically.

## Current fix boundary

- **Full fix (large):** replicate conhost's `ResizeWithReflow` (wrapped-line +
  viewport-top math) in/around `alacritty_terminal`, plus bundle a newer ConPTY.
  This is what Windows Terminal does. It remains rejected on cost/benefit:
  fragile, large, and ongoing maintenance.
- **Adopted partial mitigation:** bundle matched ConPTY/OpenConsole
  1.25.260512002-preview. It lazily synchronizes the cursor on the next
  screen-buffer query and substantially reduces cursor-placement failures. It
  does not clear stale PSReadLine text or reconstruct overflow rows.
- **Operational fallback:** `Ctrl+L` asks the child for a full redraw and remains
  the only cheap repair for residual content artifacts.
