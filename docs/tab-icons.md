# Tab icons + browser-style tabs

Window/tab headers show a per-tab icon (the app/agent logo or a shell/folder
glyph) and the tab chips are styled like classic browser tabs.

## What it does

- Every tab (and single-window header) gets a leading icon:
  - A terminal running a known agent → that agent's **official logo** (Claude,
    Codex), tinted in its brand color.
  - A plain shell → a terminal-prompt glyph tinted by shell (PowerShell blue,
    CMD gray, bash green).
  - A project tab → a folder.
  - The chat viewer → no icon.
- Tab chips are **classic browser tabs**: rounded on top, flat on the bottom so
  the active tab joins the content area below it. The close `×` shows only on the
  active or hovered tab.
- Overflowing tab names fade at the right edge before the close button. Hover
  the tab to read its complete name in a tooltip. Short names remain fully
  visible without a fade or tooltip. Chip packing is unchanged. Single-window titles use the
  same fade before their header controls; see `docs/window-chrome.md`.

## Why it exists

Pure polish — at a glance you can tell which pane is Claude, which is Codex, and
which is a plain shell, the way browser favicons tell tabs apart.

## How it works

- **Logos are SVG → texture.** Official marks live as embedded SVGs in
  `assets/icons/` (white silhouettes). `src/icons.rs` rasterizes one with `resvg`
  the first time a given `(IconKind, pixel-size)` is needed, caches the egui
  texture in the context, and re-rasterizes crisply if the DPI/zoom changes. The
  white silhouette is **tinted** to the brand color at paint time.
- **Detection** happens in `Session::icon_kind`, in priority order:
  1. **Dispatch argv** — a dispatched terminal remembers its argv
     (`Session.dispatch_argv`, set in `spawn_argv`); `IconKind::from_argv` scans
     it for `claude`/`codex`. Covers `foreman open claude …`.
  2. **OSC title** — for an agent launched *by hand* (you typed `claude` at a
     shell prompt), the program sets its terminal title via an OSC escape;
     foreman captures it (`Listener` → `Session.osc_title`) and
     `IconKind::from_title` matches it. Works when the program sets a *useful*
     title — Claude sets `claude`; **Codex sets your username**, so this misses it.
  3. **Process scan** — `proc::agent_for(root_pid)` walks the OS process tree
     under the terminal's shell PID (`Session.root_pid`) and finds an agent
     process there. This catches a hand-typed `codex` (it runs as `node …
     codex.js`) that the title path can't. Throttled (~1.5 s) and off the render
     path; the matching core (`detect_agent`) is pure and unit-tested with
     synthetic process tables.
  4. **Shell type** — a plain shell falls back to its `Shell`'s glyph.

  Each layer reverts cleanly: when the agent exits, its argv/title/process all
  go away and the icon falls back to the shell glyph. The icon also gates the
  Sessions-panel agent-state badge: a badge shows only while the detected icon
  matches the hook event's provider (`docs/agent-state.md`).
- **Drawing.** The tab paint in `src/wm.rs` asks each tab's `Content` for an
  `IconKind`, gets the texture from `icons::texture`, and paints it left of the
  label via `painter.image(..)`.

## Gotchas

- **Logos must be a white silhouette.** The embedded SVGs have their fill forced
  to `#ffffff`; tinting multiplies the brand color onto white. A colored SVG
  would tint wrong. Simple Icons / LobeHub marks are single-color, so this holds.
- **resvg is a real dependency.** Added for SVG rasterization. If a logo ever
  fails to parse, `rasterize` logs and returns a blank icon rather than panicking
  across the egui callback.
- **OSC title is matched on the file *stem*, not the whole string.** Observed
  titles are the running program's path/name (`…\claude.EXE`, `claude`; a shell
  sets its own exe path). Matching the stem avoids a false positive when the user
  works in a folder literally named `claude code` (which shows up in the prompt,
  but not in the OSC title). See `from_title`. The process scan reuses the same
  stem rule on each process's exe name and command-line args, so a script under a
  `claude`-named folder can't false-positive either.
- **Process scan is Windows-only and WSL-blind.** It enumerates *Windows*
  processes (`sysinfo`), so an agent running inside a WSL (`bash`) pane — a
  process in the WSL VM, not Windows — isn't visible; those rely on the OSC title.
- **Process scan is throttled (~1.5 s) and off the render path.** A `proc-scan`
  thread owns the `sysinfo::System` and publishes each scan as a shared
  snapshot. `agent_for` reads the latest one, memoizes the per-PID answer
  against it, and when it is older than ~1.5 s kicks the thread for a rescan
  instead of waiting — so calling `icon_kind` per-tab per-frame costs
  microseconds. The icon can lag up to that interval (plus one scan) after an
  agent starts/exits, and shows the shell glyph until the first scan lands
  (within ~70 ms of launch).
  Measured 2026-10-06 (release, Ryzen 9 5950X, ~375–480 processes): the scan
  used to run on the UI thread at ~19–21 ms every 1.5 s, a dropped frame each
  time; now the UI thread pays ~1–3 µs a call (≤0.15 ms on the first call
  after a new table), and the background scan takes ~12–15 ms. Most of that is
  the kernel's process snapshot (`NtQuerySystemInformation`, ~7 ms alone with a
  right-sized buffer), so no sysinfo setting brings it under a frame budget.
  Don't move it back onto the UI thread.
- **A recycled PID can't badge a shell.** An orphaned agent (its launcher dead)
  could otherwise read as the child of a new shell that was handed the
  launcher's old PID. The walk checks creation times on every hop. The
  mechanism is in `docs/close-confirm.md` (Gotchas, "Recycled parent PIDs").
- **Trademarks.** The Claude/Codex marks are their owners' trademarks; foreman
  uses them descriptively to label what a terminal is running.
- **The restyle is shared.** The same header code draws project tabs and terminal
  tabs, so both get the browser look. The `PS·CMD·SH` launcher in project headers
  is unchanged.

## Key files

- `src/icons.rs` — `IconKind`, the resvg rasterizer, the per-context texture
  cache, brand tints, and argv/shell → icon mapping.
- `assets/icons/` — `claude.svg`, `codex.svg` (official marks), `terminal.svg`
  (shell prompt glyph), `folder.svg` (projects).
- `src/terminal.rs` — `Session.dispatch_argv`, `Session.osc_title`,
  `Session.root_pid`, the `Listener` title capture, and `Session::icon_kind` (the
  4-layer resolver).
- `src/proc.rs` — `agent_for` (reads the background scanner's latest snapshot)
  and the pure `detect_agent` core + its tests.
- `src/wm.rs` — `Content::icon_kind` and the restyled tab-chip / single-window
  header painting.
- `Cargo.toml` — the `resvg` (SVG raster) and `sysinfo` (process scan)
  dependencies.
