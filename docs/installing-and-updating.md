# Installing and updating foreman

How foreman gets onto a machine and how a running foreman finds out a newer
version exists. Spec with the full decision history:
`docs/superpowers/specs/2026-07-14-install-and-update-design.md`.

## What it does

- **Install**: one PowerShell line downloads the latest GitHub Release,
  verifies its SHA-256, extracts to `%LOCALAPPDATA%\Programs\foreman`,
  appends that dir to the user PATH, and drops a Start-menu shortcut
  (Windows Search only surfaces apps that have one):

  ```powershell
  irm https://raw.githubusercontent.com/sniffle6/foreman/main/install.ps1 | iex
  ```

- **Release**: pushing a tag `vX.Y.Z` makes CI test, build, zip, checksum,
  and publish a GitHub Release. That release IS the update manifest — there
  is no update server, no manifest file.

- **Update notify (Phase 3, current)**: a release build of foreman checks
  `releases/latest` 10 s after launch and every 6 h. If a strictly newer
  `X.Y.Z` exists, a quiet chip appears in the Sessions panel footer
  (`↓ v0.2.1 — click for release notes`); collapsed rails show a lone `↓`
  glyph instead. Clicking opens the releases page in the browser. Nothing
  downloads, nothing installs.

- **One-click apply (Phase 4, current)**: clicking the chip on an applicable
  release downloads the zip and checksums, verifies the SHA-256, and swaps the
  running exe for the new one (`foreman.exe` → `.old`, `.new` → `foreman.exe`).
  The chip then reads "Restart to update"; a first click arms it ("Restart? N
  sessions close"), a second click within 5 s actually restarts (spawns the
  new exe, which waits out the old process, then the old one exits), and
  letting the 5 s pass disarms it back to a plain restart prompt. It's fine to
  stage a swap and never restart — the new exe just sits there until you do.
  Failures split in two: a bad hash or a failed download are retryable
  (clicking the chip re-downloads); a failed swap is not (clicking opens the
  releases page so you can grab the zip by hand).

## Why it exists this way

- The GitHub Release object already stores version, notes, and assets — a
  custom manifest would fail the deletion test, so it was never built.
- The `irm | iex` exe carries no Mark-of-the-Web, so unsigned installs don't
  hit SmartScreen. (A browser-downloaded zip extracted by Explorer still
  does — use the one-liner.)
- Notify-only shipped first on purpose: the self-swap is the most dangerous
  code in the app, so it waits behind a proven release loop.

## How to cut a release

1. Edit `version` in `Cargo.toml` (strict `X.Y.Z`), commit, push main.
2. `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. CI refuses if the tag and Cargo.toml disagree. Otherwise ~12 min later the
   release is live and every running foreman ≥0.2.0 will chip within 6 h.

Dry-run: PRs touching the workflow/installer upload the zip as an artifact
instead of publishing.

## Gotchas

- **Only CI writes asset names** (`foreman-vX.Y.Z-x86_64-windows.zip`).
  Consumers (install.ps1, `select_asset()`) match the `-x86_64-windows.zip`
  suffix — never rebuild the name from a version.
- Prereleases, drafts, and non-`X.Y.Z` tags are silently ignored by the
  updater (`parse_version` returns None → no chip).
- Debug builds never check for updates. `FOREMAN_NO_UPDATE=1` disables the
  check in release builds. `FOREMAN_UPDATE_TEST` (debug only) fakes an update
  and picks which chip state to preview: unset/empty = no chip, `apply` =
  offer, `down` = downloading, `ready`/`armed` = restart prompt (unarmed/
  armed), `err`/`errswap` = retryable/non-retryable failure; any other
  non-empty value (including the old `=1`) falls back to a plain notify chip.
  The fake offer's assets point at an unroutable URL, so clicking `apply` in a
  debug build kicks off a real download that fails fast into `err` — useful
  for screenshotting the error chip, but don't expect it to actually swap
  anything.
- `FOREMAN_WAIT_PID` is set internally by the restart handshake (the old
  instance passes its own pid to the freshly-spawned new one so it can wait
  the old process out) — never set this by hand.
- The collapsed-rail glyph (`↓`/`↻`/`!`) is steady, not pulsing — a deliberate
  simplification from the original spec's animated cell.
- The swap only replaces the exe (two-rename dance: `foreman.exe` → `.old`,
  `.new` → `foreman.exe`, staged in `%TEMP%\foreman-update`). It does not
  touch licenses, the Start-menu shortcut, or PATH — those are install.ps1's
  job, untouched by an in-place update. Only the GUI process cleans up a
  leftover `.old` at startup, never the CLI verbs (`foreman open`/`status`/...),
  so a leftover can't race a concurrent update download from a dispatching
  agent.
- The updater uses rustls + webpki-roots, not the Windows cert store —
  corporate MITM proxies make the check fail, which is a silent skip by
  design (stderr gets one line).
- install.ps1 refuses to run while foreman.exe is running, and must never
  call `exit` (it runs under `iex` in the user's shell — it uses `return`).
- Unauthenticated GitHub API is limited to 60 requests/h/IP; the 6 h cadence
  keeps foreman far under it.
- Release builds are GUI-subsystem (no console window on double-click; the
  CLI verbs adopt the parent console via `attach_parent_console`). Side
  effect: bare `foreman status` in PowerShell doesn't set `$LASTEXITCODE`
  because pwsh doesn't wait for GUI-subsystem exes. Piped/captured output
  works everywhere, and Git Bash (what agents use) waits and gets exit codes
  correctly. Debug builds stay console-subsystem.

## Key files

- `.github/workflows/release.yml` — tag-driven pipeline; sole writer of asset
  names; tag==Cargo.toml check.
- `install.ps1` — the one-liner install: download, verify, extract, PATH.
- `src/update.rs` — pure state machine (`step`/`parse_version`/
  `select_asset`), the full download/verify/swap worker (`spawn`), the
  two-rename swap and startup `cleanup_leftovers`, all gating constants.
- `src/panel.rs` — `paint_update_chip` (expanded footer) and
  `paint_rail_update_glyph` (collapsed rails, steady glyph).
- `src/main.rs` — App wiring: event drain, chip state hand-off, release-only
  spawn gating, the restart handshake (`FOREMAN_WAIT_PID`, `restart_for_update`).
- `src/control.rs` — pipe-creation retry (`listen_retry`) so a restarted
  instance wins `\\.\pipe\foreman` even if the old one lingers a beat past
  the restart handshake's wait.
