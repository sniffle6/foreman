# One-Click Update (Phase 4) — Design

Date: 2026-08-19
Status: implemented; live pilot PASSED 2026-08-20 (v0.3.0 one-clicked to v0.3.1,
installed exe hash-verified against the release asset, leftovers cleaned). Extends
`2026-07-14-install-and-update-design.md` (section 3 "Apply (Phase 4)"),
which stays authoritative for everything not restated here.

## Goal

Flip the updater from notify-only to one-click apply: chip click downloads
the release zip, verifies it, swaps the running exe via the two-rename dance,
and restarts on a second (armed) click. The Phase 4 gate in the parent spec —
"the v0.2.x pilot proves the release loop" — is satisfied: v0.2.0 through
v0.2.10 all shipped through the tag→CI→Release pipeline and the notify chip
works. Workspace save/restore (`src/workspace.rs`) exists, so
`SaveWorkspaceAndRestart` has something real to call.

## Decisions

| Question | Decision |
|---|---|
| Restart consent | Two-stage chip: first click arms (`Restart? N sessions close`), second click within ~5 s restarts; timeout disarms. No modal, no agent-state detection. |
| Download timing | On chip click (parent spec, unchanged). No background auto-download. |
| Newer release appears while `Downloading`/`ReadyToRestart` | Ignore it. Finish/keep the staged version; the next launch's first check offers the newer one. Never restart into an unstaged version. |
| What gets swapped | The exe only. install.ps1 owns the full layout (licenses etc.); the updater's trust boundary is the verified zip. |
| Verify step | SHA-256 of the zip against `SHA256SUMS.txt` from the same release. No separate hash of the extracted exe. |
| Writability gate | `CAN_APPLY` const is deleted; the worker probes writability (create+delete a temp file beside `current_exe()`) at fetch time and the fetch event carries the result into `step`. Probe fails → Phase 3 behavior (chip opens Releases page). |
| New deps | `zip` (`default-features = false, features = ["deflate"]`), `sha2`. |

## Section 1 — UX flow and state machine (pure, `src/update.rs`)

User-visible flow:

1. Chip `↓ v0.3.0` appears as today, but with `can_apply: true` when the
   writability probe passed. Click starts the download (not the browser).
2. `↓ v0.3.0 — 43%` while downloading. Verify + swap run automatically at
   100%; no user action.
3. Chip becomes `↻ Restart to update`. First click arms it:
   `Restart? N sessions close` (warn color). Second click within ~5 s
   restarts. No second click → disarms back to `↻ Restart to update`.
4. Failures: download/hash error → `Update failed — retry` (click restarts
   the download); swap failure (after rollback) → chip degrades to notify-only
   (click opens the Releases page).

State machine changes (`step` stays pure, tested as a transition table):

- `ReadyToRestart` gains `armed: bool`. `ClickRestart` arms; a new event
  `ArmTimeout` disarms. The 5 s timer lives on the egui side (an `Instant`
  stored next to the chip state; the app already repaints) — no clock inside
  `step`.
- `Error { retryable }` becomes reachable: `HashBad` or a download failure →
  `retryable: true` (click → `Download` again — the state retains the asset
  so the retry needs no refetch); `SwapFailed` → `retryable: false` (click →
  `OpenReleasesPage`, so the state retains the release URL).
- `ReleaseFetched` while `Downloading` or `ReadyToRestart`: state unchanged,
  no effects (the "ignore newer release" decision).
- The probe result rides in with the fetch: the worker probes writability
  once per fetch and the event carries it, so `can_apply` =
  `probe_ok && select_asset(..).is_some()`. A read-only install (e.g.
  Program Files) cleanly stays notify-only.

## Section 2 — Worker: download, verify, swap (I/O edge, `src/update.rs`)

The worker thread today accepts and drops the Phase 4 effects; they get real
implementations:

**`Download(asset)`** — `ureq` GET of `browser_download_url` streamed to
`%TEMP%\foreman-update\<asset name>` (dedicated subdir so cleanup is one
`remove_dir_all`). Chunked reads against `Content-Length`; `Event::Progress`
throttled to whole-percent changes. Also downloads `SHA256SUMS.txt`, selected
from the same release's asset list by exact name (a sibling of
`select_asset`; consumers still never reconstruct filenames). Any network
error → the retryable-error path.

**`VerifyAndSwap`** —
1. SHA-256 the zip (`sha2`) and compare against the matching line in
   `SHA256SUMS.txt` (`<hex>  <filename>` format, per the parent spec's asset
   convention). Mismatch → delete the download dir, `Event::HashBad`. Never
   install unverified bytes.
2. Extract `foreman.exe` from the zip (`zip` crate) to `foreman.exe.new`
   beside `current_exe()` — fully written and flushed before any rename.
3. Two-rename dance: `foreman.exe` → `foreman.exe.old`, then
   `foreman.exe.new` → `foreman.exe`. Rename 2 fails → rename `.old` back,
   `Event::SwapFailed` (never half-swapped). Rename 1 fails (AV lock) →
   `SwapFailed`. Success → `Event::SwapOk` + delete the %TEMP% dir.

**Startup cleanup** (spec'd in the parent doc, built now): every launch
best-effort deletes `foreman.exe.old` beside `current_exe()` and any stale
`%TEMP%\foreman-update` dir. Failures ignored — AV can hold `.old` briefly;
next launch gets it.

## Section 3 — Restart: workspace save, respawn, pipe race

`SaveWorkspaceAndRestart` cannot run on the worker thread (it needs the live
desktop tree), so App handles it in the frame that drains it:

1. `flush_workspace()` — the immediate-save path (`src/main.rs`), not the
   debounced one, so the saved layout is what the user sees.
2. Spawn the new exe: `Command::new(current_exe())` — on disk that is now the
   new image; the running process keeps executing its old renamed image
   (the Windows rename-swap property the parent spec verified). Detached, no
   inherited console.
3. Exit via eframe's normal close path so no racing write is truncated.

**Pipe race:** the old instance holds `\\.\pipe\foreman` until it fully
exits, and the child must not restore the workspace while the old window is
still up. Two cheap guards, no IPC handshake:

- The child is spawned with `FOREMAN_WAIT_PID=<old pid>`. At startup, if set,
  it waits (bounded ~10 s; `OpenProcess` + `WaitForSingleObject`) for that
  PID to exit before doing anything visible, then clears the var so spawned
  terminals don't inherit it. Timeout → proceed anyway.
- The control server's pipe creation gets a short retry loop (a few attempts
  over ~2 s) instead of one-shot failure — also hardens the unrelated
  "two instances launched fast" case. (`src/control.rs`)

**Honesty about sessions:** every PTY dies with the old process. Workspace
restore rebuilds projects, tabs, and layout with fresh shells — not the old
processes. The armed chip label is the only warning, by decision.

**Non-restart path is free:** if the user never clicks restart, the swap
already happened on disk; a normal quit + next launch runs the new version
and cleans up `.old`. The chip never nags.

## Section 4 — Chip UI, gating, testing, rollout

**Chip variants** (`paint_update_chip` + rail glyph in `src/panel.rs`, quiet
chrome like today):

| State | Expanded footer | Collapsed rail |
|---|---|---|
| `UpdateAvailable{can_apply:true}` | `↓ v0.3.0 — click to update` | `↓` |
| `Downloading{progress}` | `↓ v0.3.0 — 43%` | `↓` pulsing |
| `ReadyToRestart{armed:false}` | `↻ Restart to update` | `↻` |
| `ReadyToRestart{armed:true}` | `Restart? N sessions close` (warn) | `↻` warn |
| `Error{retryable:true}` | `Update failed — retry` | `!` |
| `Error{retryable:false}` | `Update failed — release notes` | `!` |

The session count in the armed label is read from the desktop
`WindowManager` at paint time; the state machine never knows about it.

**Gating:** unchanged — debug builds never update, `FOREMAN_NO_UPDATE=1`
kills everything. `FOREMAN_UPDATE_TEST=1` (debug-only) grows to fake the full
Phase 4 sequence so every chip state can be screenshot-verified without a
real release.

**Testing:**
- Transition-table unit tests for every new `step` edge: arm/disarm/timeout,
  both error paths and their retry clicks, newer-release-while-busy ignored,
  probe-false → notify-only.
- Integration test of the two-rename dance on dummy files in a temp dir,
  including the forced-rollback path (no GUI; runs in `cargo test`).
- SHA-256 verify against a known vector; `SHA256SUMS.txt` line parsing.
- Visual: screenshot each chip state via `FOREMAN_UPDATE_TEST=1`.
- End-to-end acceptance is a live release: ship this as v0.3.0, then cut a
  trivial v0.3.1 and one-click apply it on an installed copy — the same way
  Phase 3 was proven.

## Error handling summary

| Failure | Behavior |
|---|---|
| Download network error | Delete partial file, `Error{retryable:true}`, click retries |
| Hash mismatch | Delete download dir, `Error{retryable:true}` |
| Rename 1 fails (AV lock) | `Error{retryable:false}`, chip links Releases page |
| Rename 2 fails | `.old` renamed back into place, `Error{retryable:false}` |
| Staged but never restarted | Fine indefinitely; next normal launch runs the new exe |
| Child never sees parent exit | Bounded wait (~10 s), then proceed; pipe retry loop absorbs the tail |
| Writability probe fails | `can_apply: false` → Phase 3 notify-only behavior |

## Files touched

- `src/update.rs` — bulk: state machine extensions, download/verify/swap in
  the worker, writability probe.
- `src/panel.rs` — chip variants + rail glyph states.
- `src/main.rs` — effect drain for `SaveWorkspaceAndRestart`, startup cleanup
  (`.old`, stale %TEMP% dir), `FOREMAN_WAIT_PID` wait.
- `src/control.rs` — pipe-creation retry loop.
- `Cargo.toml` — add `zip` (no default features, deflate) and `sha2`.
- `docs/installing-and-updating.md` — update the Phase 4 section when built.
