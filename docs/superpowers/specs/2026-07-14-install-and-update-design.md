# Install & Update — Design

Date: 2026-07-14
Status: approved; revised 2026-07-15 after grug review + deep-module review +
fact-check (all corrections applied, changelog at bottom)

## Goal

Public distribution of foreman on Windows: a frictionless install for a
developer audience and an in-app update path that never interrupts live agent
sessions. Today the only way to run foreman is `cargo run` from a checkout;
version is frozen at 0.1.0 and no release/CI infra exists.

## Decisions (locked)

| Question | Decision |
|---|---|
| Audience | Public distribution |
| Update UX | Notify chip first (Phase 3); one-click apply added after a real release cycle proves the loop (Phase 4) |
| Artifacts | Portable zip on GitHub Releases; per-user Inno `setup.exe` **deferred** until someone asks (built with the winget phase) |
| Headline install | `irm <raw install.ps1 URL> \| iex` one-liner (uv/bun/scoop pattern) |
| Code signing | Skipped for now; CI keeps a slot to add it later |
| winget / custom domain / Inno | Deferred; asset naming stays stable so they bolt on later |
| Approach | Self-contained Rust updater (vs. script-as-updater, vs. notify-only) |

Rationale highlights:

- The `irm | iex` headline path avoids SmartScreen entirely: neither
  `Invoke-WebRequest` nor `Expand-Archive` applies/propagates Mark-of-the-Web
  (verified empirically on Win11 26200, pwsh 7 and PS 5.1). The SmartScreen
  hurdle remains for browser-downloaded artifacts **including portable zips
  extracted with Explorer** (Explorer does propagate MotW) — not just a
  hypothetical setup.exe.
- WezTerm/Alacritty precedent: zip + installer on GitHub Releases, winget
  manifest, at most a notify-style update check. Foreman starts at that
  standard (notify chip) and adds one-click apply once evidence supports it.
- A Rust updater downloads a hash-verified zip from a release — the trust
  chain is the immutable release, not a mutable remote script.
- The swap dance (rename a running exe) exists in exactly ONE place: the Rust
  updater. `install.ps1` refuses to update a running foreman instead of
  re-implementing it in PowerShell.

## Section 1 — Release pipeline & versioning

**Version source of truth: `Cargo.toml`.** Releasing = bump version, commit,
push tag `vX.Y.Z`. CI fails the release if the tag and `Cargo.toml` disagree.

`.github/workflows/release.yml`, triggered on `v*` tags, runs on
`windows-latest`:

1. Install `stable-gnu` toolchain — matches local builds so release binaries
   don't silently differ from what gets tested. Runner facts as of June 2026:
   `windows-latest` = Windows Server 2025 / VS2026 image, ships mingw-w64
   gcc 15.2.0, MSYS2 at `C:\msys64` (off PATH), InnoSetup 6.7.1, gh CLI
   2.95.0. rustup's gnu toolchain bundles its own `rust-mingw` link
   components. **Phase 1 must still empirically verify the link step** (gcc
   major-version drift has bitten before — the local w64devkit gcc-16
   `libgcc_eh` incident); fallback is the image's MSYS2.
2. `cargo test` — a failing test kills the release.
3. `cargo build --release`.
4. Package the zip per the asset naming convention below (exe + LICENSE +
   third-party notices for embedded assets: Hack font, ConPTY).
5. Generate `SHA256SUMS.txt`.
6. Create the GitHub Release (`gh release create` with `GITHUB_TOKEN`; if an
   action is ever preferred, `softprops/action-gh-release@v3` — v2 is EOL)
   with both assets + auto-generated notes.

**No separate update manifest.** The GitHub Releases API (`releases/latest`)
is the manifest; the updater and install script both read it. `latest`
excludes drafts and prereleases by GitHub's definition, which is exactly the
desired channel semantics.

### Asset naming convention (the single definition — all tooling refers here)

- Zip: `foreman-v{VERSION}-x86_64-windows.zip`
- Checksums: `SHA256SUMS.txt` (`<hex>  <filename>` lines)
- Deferred installer, when it exists: `foreman-v{VERSION}-setup.exe`

CI is the only writer of full names. **Consumers (install.ps1, updater,
later winget) never reconstruct filenames from a version — they select from
the release's asset list by suffix** (e.g. ends-with `-x86_64-windows.zip`).
In the updater this is a pure, unit-tested
`select_asset(&[Asset]) -> Result<&Asset, SelectErr>`.

A PR-triggered dry-run job builds the zip without publishing, so pipeline rot
is caught before tag day. (There is no CI today; this workflow is new.)

**Prerequisite: the repo has no LICENSE file.** Public distribution requires
choosing one (MIT/Apache-2.0 dual is the Rust convention) before the first
release. The exe embeds the Hack font and bundles ConPTY, whose licenses
(`assets/fonts/LICENSE-Hack.md`, `assets/conpty/LICENSE`) must ship in the
zip.

## Section 2 — Install channels

Both live channels produce the **identical layout** (fact table below).

### install.ps1 (headline)

Lives in the repo; README leads with
`irm https://raw.githubusercontent.com/sniffle6/foreman/main/install.ps1 | iex`.

Deliberately minimal — download, verify, extract, PATH. Nothing else.

- Query `releases/latest` → select zip asset by suffix → download → verify
  SHA256 against `SHA256SUMS.txt` → extract to the layout dir.
- If `foreman.exe` is currently running: print "close foreman first" and
  exit. The script does NOT re-implement the rename-swap.
- Add the dir to the **user** PATH if absent (the exe doubles as the
  `foreman` CLI used by agents).
- No Start-menu shortcut, no uninstall registry entry: the `irm|iex`
  audience is developers; the deferred Inno installer owns the double-click /
  Settings→Apps experience if demand appears.
- Idempotent: re-running the one-liner is the manual update path.

### Portable zip

Just the exe + licenses; put it anywhere. Self-update still works from any
user-writable location (Section 3). Note: extracting with Explorer keeps
MotW, so first launch may hit SmartScreen; `Expand-Archive` doesn't.

### Install layout seam (fact table — every writer must agree)

| Fact | Value | Writers |
|---|---|---|
| Install dir | `%LOCALAPPDATA%\Programs\foreman` | script, (Inno later) |
| Exe name | `foreman.exe` | CI zip, script, (Inno) |
| PATH entry | install dir, **user** PATH, append-if-absent | script, (Inno) |
| Swap staging | `foreman.exe.new` / `foreman.exe.old`, beside the exe | updater ONLY |
| Startup cleanup | delete `foreman.exe.old` if present, ignore failure | exe itself |

The updater deliberately does NOT consume this seam — it keys off
`std::env::current_exe()`, which is why portable installs update too.

When the Inno installer is eventually built it must write these same facts
plus its own rows (shortcut name, HKCU uninstall key/AppId), use
`PrivilegesRequired=lowest` and `{localappdata}\Programs\foreman`, and append
to user PATH via a `[Code]` append-if-absent helper + `ChangesEnvironment=yes`
— Inno has no built-in PATH directive and a naive `[Registry]` entry
*overwrites* the user PATH.

## Section 3 — In-app updater (`src/update.rs`)

Built in two phases behind one design: **Phase 3 ships the check + notify
chip** (no download, no swap, no `zip` dep); **Phase 4 adds one-click apply**
after the v0.2.x pilot proves the release loop.

**Gating:**

- Release builds only: `cfg(debug_assertions)` disables all update behavior,
  so `cargo run` dev sessions never phone home.
- `FOREMAN_NO_UPDATE=1` env var as an escape hatch.

**Pure core, I/O at the edge** (matches the layout/wm/chat convention: pure
model, unit-tested, no GUI):

- `fn decide(current: &str, release: &ReleaseJson) -> Decision` — semver
  compare (upgrades only, never downgrades) + `select_asset` by suffix.
- `fn step(State, Event) -> (State, Vec<Effect>)` — the entire state machine.
  - `Event`: `ReleaseFetched`, `FetchFailed`, `ClickChip`, `Progress`,
    `HashOk`, `HashBad`, `SwapOk`, `SwapFailed`, `ClickRestart`.
  - `Effect`: `FetchLatest`, `OpenReleasesPage`, `Download`, `VerifyAndSwap`,
    `SaveWorkspaceAndRestart`.
- A background thread is a thin adapter: executes `Effect`s (all I/O lives
  here) and feeds `Event`s back over a channel + repaint request — the same
  seam shape as `Session`'s PTY reader thread. The egui side only renders the
  state and forwards clicks. No blocking calls on the UI thread, ever.

**Renderable states (complete enum — the panel chip is the consumer):**

- `Idle` — no chip.
- `UpdateAvailable { version, can_apply }` — chip `↓ v0.2.0`.
  `can_apply = false` when the exe location isn't user-writable or the build
  is Phase-3-only: click opens the Releases page. `can_apply = true`: click
  starts the download.
- `Downloading { progress }` — chip shows progress (Phase 4).
- `ReadyToRestart` — chip "Restart to update"; restart only when the user
  chooses (Phase 4).
- `Error { retryable }` — retryable failures (download, hash) allow retry;
  swap failure links the Releases page as fallback.

**Check:** first check ~10 s after launch (never competes with startup), then
every 6 h. `GET api.github.com/repos/sniffle6/foreman/releases/latest` via
`ureq` (small, blocking, no tokio; explicit User-Agent, short timeout).
Unauthenticated rate limit (60/h/IP) is far above the cadence; the limit is
shared behind corporate NAT, but check failures are silent-skip anyway.

**Apply (Phase 4, on chip click):** download zip to `%TEMP%` → verify SHA256
→ extract (`zip` crate) → write `foreman.exe.new` beside the current exe →
rename running `foreman.exe` → `foreman.exe.old` → rename `.new` into place
(renaming a running exe is legal on Windows — verified empirically, and it's
the rustup/self-replace mechanism). Chip becomes "Restart to update".
Restart = force a workspace save, spawn the new exe, exit; cold restore
rebuilds the workspace. Next launch deletes any leftover `foreman.exe.old`
(ignore failure — AV can hold it briefly).

**Control-plane note:** `foreman` CLI invocations inside terminals are
separate short-lived processes of the same exe; rename-swap does not disturb
them, and injected `FOREMAN_EXE` paths stay valid because the installed path
never changes.

## Section 4 — Error handling

- Background check fails (offline, rate-limited, corporate TLS interception):
  silent skip, log line, retry next interval. Never a visible error for a
  background check.
- Hash mismatch: delete the download, chip shows a retryable error. Never
  install unverified bytes.
- Rename-swap fails (AV lock, read-only dir): rename `.old` back into place,
  chip links the Releases page as fallback. The swap is two renames, so a
  failure of the second rename is undone by reversing the first — the
  install is never left half-swapped.
- Update staged but user never restarts: fine indefinitely. The running
  process keeps its renamed image; the chip stays at "Restart to update".
- install.ps1 while foreman runs: refuse with a clear message; no swap in
  PowerShell.

## Section 5 — Testing

- Unit (pure, no GUI — matches existing test layout): `decide`,
  `select_asset`, and every `step` transition — these test the *shipping*
  state machine, not a shadow copy, because the thread only executes
  Effects.
- Swap mechanics (Phase 4): integration test performing the two-rename dance
  on dummy files in a temp dir, including the rollback path.
- Pipeline: the PR dry-run job (Section 1).
- Pilot: `v0.2.0` is the shakedown release — install via the one-liner on a
  clean machine/sandbox. `v0.2.1` proves the notify chip. Phase 4 lands
  after that; the next release proves one-click apply end-to-end.

## Phasing (each independently shippable)

1. **Release pipeline** — tag → zip + checksums + GitHub Release. Verify the
   GNU link step on the runner here. Prerequisite: LICENSE.
2. **install.ps1** + README headline.
3. **Updater, notify chip** — `src/update.rs` pure core + check thread +
   panel chip with `can_apply = false` everywhere. New dep: `ureq`.
4. **Updater, one-click apply** — download/verify/swap/restart. New dep:
   `zip`. Gated on the v0.2.x pilot having proven the release loop.
5. *Deferred until asked for:* Inno installer, winget manifest, code-signing
   slot, custom install domain, Explorer "Open in foreman" context menu
   (legacy-menu version is cheap: HKCU `Directory\shell` + `Directory\
   Background\shell` keys written by install.ps1 and a new `--open-project`
   launch path — pipe-forward to a running instance via `\\.\pipe\foreman`,
   else boot with that dir; the top-level Win11 menu needs a packaged/signed
   `IExplorerCommand`, so it pairs with the signing work).

## Dependencies

- `ureq` (Phase 3): HTTP for check + download. Default TLS is rustls +
  webpki-roots, **not** the Windows cert store — corporate MITM proxies will
  fail the check (absorbed by silent-skip). If that ever matters, enable
  ureq's `platform-verifier` feature.
- `zip` (Phase 4 only): `default-features = false, features = ["deflate"]`.
  Shelling out to `Expand-Archive` was considered and rejected — it adds a
  PowerShell dependency at the most failure-sensitive moment.

## New files

- `.github/workflows/release.yml` — tag-driven release pipeline.
- `.github/workflows/release-dryrun.yml` — PR dry-run.
- `install.ps1` — one-liner install script.
- `src/update.rs` — pure state machine + thread adapter.
- Panel chip wiring in `src/panel.rs`; restart/save hooks in `src/main.rs` /
  `src/wm.rs`.
- Deferred: `installer/foreman.iss`.

## Review changelog (2026-07-15)

Grug review: Inno installer deferred to Phase 5; install.ps1 slimmed to
download/verify/extract/PATH (no shortcut/registry); updater split into
notify-first (P3) / apply-later (P4); swap logic exists only in Rust.
Deep-module review: asset naming defined once + suffix selection; pure
`step`/`decide` core so unit tests hit the real interface; complete
renderable-state enum; layout seam fact table.
Fact-check (all 8 claims confirmed, 2 empirically): MotW wording broadened
(Explorer-extracted zips DO hit SmartScreen); Inno PATH `[Code]` nuance
recorded; ureq webpki-roots vs Windows cert store noted; runner-image facts
pinned; `action-gh-release@v3` if an action is used.
