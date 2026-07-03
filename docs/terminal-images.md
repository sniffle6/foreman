# Terminal Images (kitty graphics, image paste, alt-key routing)

## What / why

foreman renders the kitty graphics subset Codex pets uses (transmit PNG/raw →
display at cursor → delete by id, chunked transmission), delivers image-paste
keystrokes to agents (Alt+V, Ctrl+V, Ctrl+Alt+V), and no longer double-sends
Alt+letter. Spec with all decisions:
docs/superpowers/specs/2026-07-02-terminal-image-support-design.md

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
post-rearchitecture OpenConsole build (`assets/conpty/`, MIT, vendored from
wezterm 2025-02) and drops `conpty.dll` + `OpenConsole.exe` beside foreman.exe
at startup — portable-pty prefers a sideloaded pair over kernel32. Best-effort:
if the install fails, foreman still runs, images just don't arrive.

## Gotchas

- The cursor does NOT advance after an image (v1): ratatui apps don't care;
  `kitten icat` in a bare shell overprints. Deliberate — see spec limits.
- KITTY_WINDOW_ID=1 is injected; TERM stays xterm-256color on purpose.
- Graphics replies bypass `resp` — resp's flush latches `ready` (DSR contract).
- A `clear`/RIS doesn't delete placements; scrolling or `a=d` does. Pets
  deletes its own frames constantly, so this only shows with rogue clients.
- Alt text suppression is `alt && !ctrl` — AltGr (= Ctrl+Alt on Windows) must
  keep typing on intl layouts. Don't "simplify" it to plain `alt`. Same story
  for the Ctrl+Alt encoder: it's scoped to V only (Codex's binding) because an
  unscoped version double-injects on AltGr text.
- WezTerm's *stable* (2024-02) conpty pair does NOT pass APC through — only
  the 2025-02+ build does. If you update `assets/conpty/`, rerun the canary.
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
- src/terminal.rs — advance_scanned/term_view, pump segmenting, texture cache,
  overlay painting, clipboard_has_image, conpty canary + perf bench tests
- src/input.rs — alt/ctrl+alt routing, empty-paste fallback
- src/wm.rs — KITTY_WINDOW_ID in term_env
- src/conpty_install.rs + assets/conpty/ — sideloaded passthrough ConPTY host
