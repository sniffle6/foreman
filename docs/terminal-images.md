# Terminal Images (kitty graphics, icat, image paste, alt-key routing)

## What / why

foreman renders the kitty graphics subset Codex pets uses (transmit PNG/raw →
display at cursor → delete by id, chunked transmission), delivers image-paste
keystrokes to agents (Alt+V, Ctrl+V, Ctrl+Alt+V), and no longer double-sends
Alt+letter. Spec with all decisions:
docs/superpowers/specs/2026-07-02-terminal-image-support-design.md

## foreman icat — show an image in your pane

`foreman icat <file.png> [--cols N]` prints an image into the current
terminal by emitting the same kitty APC bytes on stdout (Codex chunk format:
full header + `m=1`, bare continuations, `m=0` final). No pipe, no GUI code —
stdout IS the PTY, so the running foreman's scanner renders it like any other
client, and it also works in kitty/WezTerm. The main use: an agent finishes a
screenshot and runs `foreman icat shot.png` so the human sees it in the pane.

- Sized to the console width (minus a margin), aspect kept via a ~1:2 cell
  ratio; small images are never stretched past their natural span; height is
  capped below the viewport because a placement that scrolls off the top is
  deleted (placement semantics above).
- PNG only in v1 (the image store only decodes PNG).
- icat prints `rows` newlines after the image so the prompt lands below it
  (the renderer deliberately doesn't advance the cursor — v1 limit).
- The image behaves like ordinary output: scrolls with the buffer, gone once
  it scrolls off the top.
- Image id = the icat process id, so repeated icats never collide.
- Pure seams (`encode`, `fit`, `png_dims`) are unit-tested round-trip through
  `graphics::Graphics::feed`/`apply`/`visible` — the tests drive the real
  renderer, not a mock.

## How it works

PTY bytes flow to alacritty exactly as before; `Session::pump` also hands each
chunk to `graphics::Graphics::feed`, which returns "cuts" — offsets where a
graphics command completed. pump advances the parser in segments around cuts so
each placement samples the cursor at the right stream position. `Session::show`
paints the visible images as egui textures after the glyphs. Images never enter
the grid — selection/copy/snapshot stay pure text.

**The ConPTY passthrough problem (the big discovery):** the in-box Windows
conhost strips kitty APC sequences inside ConPTY, so no graphics bytes can ever
reach foreman through the system PTY. Proof: the ignored canary test
`conpty_passes_kitty_apc_through` (run with
`cargo test --release conpty_passes -- --ignored`; needs the pair below beside
the test exe in `target/release/deps`). Fix: `src/conpty_install.rs` embeds the
official post-rearchitecture OpenConsole build (`assets/conpty/`, MIT, from
Microsoft's ConPTY NuGet package) and drops `conpty.dll` + `OpenConsole.exe`
beside foreman.exe at startup — portable-pty prefers a sideloaded pair over
kernel32. The installer requires the exact matched pair (or no sideload at all)
before the GUI starts, refuses to replace a DLL mapped by another Foreman, and
holds both sidecars open for the process lifetime. A failed update disables the
sideloaded DLL and degrades to the in-box ConPTY (images just don't arrive);
startup aborts only when an unverified `conpty.dll` would stay loadable.

**Pin the pair to a good version.** The sideloaded host owns *every* PTY spawn,
so a bad build slows the whole app. The WezTerm-vendored **1.22.2502** pair we
shipped first added a fixed **~3.0s stall on every terminal spawn** (prompt in
~3.3s vs ~0.25s on in-box conhost) — a host bug, not ours. The official
Microsoft redistributable **1.24.2605.12001** restored fast spawn; it was
replaced on 2026-07-09 by **1.25.2605.12002-preview** to pick up ConPTY's
post-resize cursor synchronization (#19535 + #20095) and recovery after unknown
VT sequences such as kitty APC (#20009). Both are official x64, MIT,
Microsoft-signed `Microsoft.Windows.Console.ConPTY` packages. The 1.25 pair
passed the APC canary, completed the post-APC cursor query in 3ms, and showed no
spawn stall. See
`assets/conpty/README.md` for exact hashes and source lineage. Whenever you
touch `assets/conpty/`, run every package, passthrough, cursor-sync, latency, and
spawn gate listed there.

## Gotchas

- The cursor does NOT advance after an image (v1): ratatui apps don't care;
  `kitten icat` in a bare shell overprints. Deliberate — see spec limits.
- KITTY_WINDOW_ID=1 is injected; TERM stays xterm-256color on purpose.
- Graphics replies bypass `resp` — a successful resp flush latches `ready`
  (DSR contract).
- A `clear`/RIS doesn't delete placements; scrolling or `a=d` does. Pets
  deletes its own frames constantly, so this only shows with rogue clients.
- Alt text suppression is `alt && !ctrl` — AltGr (= Ctrl+Alt on Windows) must
  keep typing on intl layouts. Don't "simplify" it to plain `alt`. Same story
  for the Ctrl+Alt encoder: it's scoped to V only (Codex's binding) because an
  unscoped version double-injects on AltGr text.
- WezTerm's *stable* (2024-02) conpty pair does NOT pass APC through — only
  the 2025-02+ build does. If you update `assets/conpty/`, rerun the canary.
- ...but don't ship *just any* passthrough build: the 1.22.2502 pair passes the
  canary yet stalls every spawn ~3s. Vendor a matched pair from Microsoft's
  `Microsoft.Windows.Console.ConPTY` NuGet package (currently
  1.25.260512002-preview) and verify spawn time as well as the canary.
- The canary test fails in `target/release/deps` unless the pair is copied
  there too (the app's auto-install only covers the exe's own directory).
- **Synchronized updates stale the anchor.** Ratatui apps (codex) emit whole
  frames inside `?2026h..?2026l`; vte buffers the block, so a cursor sample at
  a graphics cut would read the PREVIOUS frame's cursor (codex parks it at the
  composer caret — the pet rendered there until this was fixed).
  `advance_scanned` force-flushes the parser (`stop_sync`) at each cut so the
  Term reflects the stream up to the command. Regression test:
  `sync_update_frame_anchors_at_the_pet_cup_not_the_stale_caret`; diagnostics:
  `codex_pet_rx_capture` (headless codex, dumps raw rx, zero usage) +
  `codex_pet_rx_analyze`, and `FOREMAN_RX_DUMP=<file>` on any session.
- The image-only-clipboard Ctrl+V → `0x16` fallback is best-effort: it relies
  on egui delivering the Key::V event alongside an empty Paste event, which
  may vary by egui version/platform. The primary paste paths (Alt+V, handled
  entirely by the agent, and Ctrl+Alt+V → `ESC 0x16`) don't depend on it.

## Performance

Flood benchmark: median of 3 runs of `cmd /c type` on a 200k-line file (120
cols) inside a release foreman pane (`foreman send` dispatch, `foreman
snapshot` readback).

| Point | TotalSeconds (runs) | Median |
|---|---|---|
| Baseline (pre-change, eed13bf, inbox conhost) | 0.782 / 0.793 / 0.766 | **0.782** |
| After (scanner + sideloaded conpty) | 0.786 / 0.765 / 0.722 | **0.765** |

Net: ~2% faster end-to-end (the newer OpenConsole outweighs scanner cost).
Gate (<2% regression): **pass**.

Scanner micro-benchmark (`cargo test --release scanner_overhead -- --ignored
--nocapture`, isolates our code): plain flood — vte 108 MB/s, scanner 2254
MB/s (4.8% relative); ANSI-heavy — vte 180 MB/s, scanner 1726 MB/s (10.5%
relative). The scanner is a second linear pass an order of magnitude faster
than the parse it rides alongside.

## Key files

- src/graphics.rs — scanner, kitty parser, image store, placements (pure)
- src/icat.rs — `foreman icat`: kitty APC encoder + fit math + console-size
  probe (CLI-side; dispatched from control.rs's client_main)
- src/terminal.rs — advance_scanned/term_view, pump segmenting, texture cache,
  overlay painting, clipboard_has_image, conpty canary + perf bench tests
- src/input.rs — alt/ctrl+alt routing, empty-paste fallback
- src/wm.rs — KITTY_WINDOW_ID in term_env
- src/conpty_install.rs + assets/conpty/ — sideloaded passthrough ConPTY host
