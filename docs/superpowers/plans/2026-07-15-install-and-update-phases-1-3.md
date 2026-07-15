# Install & Update (Phases 1–3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship foreman's public distribution: tag-driven GitHub release pipeline, `irm | iex` install script, and the in-app update-notify chip (Phases 1–3 of the spec; Phase 4 one-click apply gets its own plan after the v0.2.x pilot).

**Architecture:** A GitHub Actions workflow turns a `vX.Y.Z` tag into a Release carrying a zip + `SHA256SUMS.txt` — the Releases API is the update manifest, there is no other. `install.ps1` and the in-app checker are pure consumers that select assets by suffix. The updater is a pure state machine (`step`/`decide` in `src/update.rs`, unit-tested) with a worker thread that only executes Effects and reports Events over `std::sync::mpsc` + `ctx.request_repaint()` — the same seam shape as `Session`'s PTY reader thread and `control::serve`.

**Tech Stack:** Rust (stable-gnu) + egui 0.34, GitHub Actions `windows-latest`, PowerShell 5.1+ for install.ps1, new dep `ureq = "3"` (default features: rustls). No tokio, no `zip` crate (that's Phase 4).

**Spec:** `docs/superpowers/specs/2026-07-14-install-and-update-design.md` — read it before deviating from anything here.

## Global Constraints

- Toolchain is **GNU**, never MSVC: `stable-x86_64-pc-windows-gnu` locally and in CI.
- Local builds: kill the app first or linking fails with `Access is denied (os error 5)` — `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`. **EXCEPTION: if `$env:FOREMAN` is `1` you are running INSIDE foreman — do NOT Stop-Process; build with `cargo build --target-dir target/agent` instead.**
- egui 0.34: the app impl is `fn ui(&mut self, ui: &mut egui::Ui, ...)`, not `update`.
- Channels are `std::sync::mpsc` (codebase convention — no crossbeam, no tokio).
- Colors come only from `src/theme.rs` consts (glob-imported); do not hardcode Color32 values in panel code.
- Asset naming (verbatim, single definition): zip `foreman-v{VERSION}-x86_64-windows.zip`, checksums `SHA256SUMS.txt` with `<lowercase-hex>  <filename>` lines (two spaces). Consumers match the suffix `-x86_64-windows.zip`, never rebuild full names.
- Install layout: `%LOCALAPPDATA%\Programs\foreman\foreman.exe`; **user** PATH, append-if-absent.
- License: `MIT OR Apache-2.0`, copyright "the foreman contributors".
- Update gating: no update behavior in `cfg(debug_assertions)` builds; `FOREMAN_NO_UPDATE=1` disables in release builds.
- `install.ps1` runs under `iex` in the USER'S shell: never call `exit` (it would kill their terminal) — use `return`/`throw`.
- Unit tests are in-module `#[cfg(test)] mod tests { use super::*; ... }` (see `src/layout.rs:600`).
- Commit per task with conventional prefixes (`ci:`, `feat:`, `docs:`, `chore:`).

**One deliberate deviation from the spec:** the spec lists `release.yml` + `release-dryrun.yml` as two workflows. This plan uses ONE `release.yml` with three triggers (tag push, PR-on-paths, manual dispatch) and a publish step gated on the tag ref. Same dry-run capability, zero duplicated build steps. The spec's intent (PR-triggered dry-run exists) is satisfied.

---

### Task 1: LICENSE + Cargo package metadata

**Files:**
- Create: `LICENSE-MIT`
- Create: `LICENSE-APACHE`
- Modify: `Cargo.toml:1-4` (package section)
- Modify: `README.md` (append License section at end)

**Interfaces:**
- Produces: `LICENSE-MIT` and `LICENSE-APACHE` at repo root — Task 2's packaging step copies these exact filenames into the zip.

- [ ] **Step 1: Write LICENSE-MIT**

Create `LICENSE-MIT` with exactly:

```text
MIT License

Copyright (c) 2026 the foreman contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

- [ ] **Step 2: Fetch the canonical Apache-2.0 text**

Run: `curl -sSf -o LICENSE-APACHE https://www.apache.org/licenses/LICENSE-2.0.txt`
Verify: `Get-Content LICENSE-APACHE -TotalCount 2` prints the "Apache License / Version 2.0, January 2004" header, and `(Get-Item LICENSE-APACHE).Length` is ~11,300 bytes.

- [ ] **Step 3: Add package metadata to Cargo.toml**

In `Cargo.toml`, extend the `[package]` section (currently name/version/edition) to:

```toml
[package]
name = "foreman"
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
repository = "https://github.com/sniffle6/foreman"
description = "Fast native desktop for running many AI-agent terminal sessions"
```

- [ ] **Step 4: Append a License section to README.md**

At the end of `README.md` add:

```markdown
## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Bundled third-party components
keep their own licenses (Hack font: `assets/fonts/LICENSE-Hack.md`; ConPTY:
`assets/conpty/LICENSE`).
```

- [ ] **Step 5: Verify the build still passes metadata parsing**

Run: `cargo metadata --format-version 1 --no-deps | Select-String '"license":"MIT OR Apache-2.0"'`
Expected: one matching line.

- [ ] **Step 6: Commit**

```bash
git add LICENSE-MIT LICENSE-APACHE Cargo.toml README.md
git commit -m "chore: dual-license MIT OR Apache-2.0"
```

---

### Task 2: Release workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: `LICENSE-MIT`, `LICENSE-APACHE` from Task 1; `assets/fonts/LICENSE-Hack.md`, `assets/conpty/LICENSE` (already in repo).
- Produces: on tag `vX.Y.Z` — a GitHub Release with assets `foreman-vX.Y.Z-x86_64-windows.zip` (zip root: `foreman.exe`, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-Hack-font.md`, `THIRD-PARTY-ConPTY.txt`) and `SHA256SUMS.txt`. Tasks 3 and 5 consume these via the Releases API.

- [ ] **Step 1: Write the workflow**

Create `.github/workflows/release.yml`:

```yaml
name: release

on:
  push:
    tags: ["v*"]
  pull_request:
    paths:
      - ".github/workflows/release.yml"
      - "install.ps1"
  workflow_dispatch:

permissions:
  contents: write

jobs:
  release:
    runs-on: windows-latest
    defaults:
      run:
        shell: pwsh
    steps:
      - uses: actions/checkout@v4

      # Match local builds (GNU, not MSVC) so release binaries don't
      # silently differ from what gets tested. Spec §1 requires empirically
      # verifying this link step on the runner.
      - name: Use GNU toolchain
        run: |
          rustup toolchain install stable-x86_64-pc-windows-gnu
          rustup default stable-x86_64-pc-windows-gnu
          rustc -vV

      - name: Read version from Cargo.toml
        id: ver
        run: |
          $m = Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1
          $v = $m.Matches[0].Groups[1].Value
          "version=$v" >> $env:GITHUB_OUTPUT

      - name: Tag must match Cargo.toml
        if: startsWith(github.ref, 'refs/tags/')
        run: |
          if ('v${{ steps.ver.outputs.version }}' -ne '${{ github.ref_name }}') {
            throw "tag ${{ github.ref_name }} != Cargo.toml v${{ steps.ver.outputs.version }}"
          }

      - name: Test
        run: cargo test

      - name: Build release
        run: cargo build --release

      - name: Package
        run: |
          $v = '${{ steps.ver.outputs.version }}'
          $zip = "foreman-v$v-x86_64-windows.zip"
          New-Item -ItemType Directory staging | Out-Null
          Copy-Item target/release/foreman.exe staging/
          Copy-Item LICENSE-MIT, LICENSE-APACHE staging/
          Copy-Item assets/fonts/LICENSE-Hack.md staging/THIRD-PARTY-Hack-font.md
          Copy-Item assets/conpty/LICENSE staging/THIRD-PARTY-ConPTY.txt
          Compress-Archive -Path staging/* -DestinationPath $zip
          $h = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
          "$h  $zip" | Set-Content -Encoding ascii SHA256SUMS.txt
          Get-Content SHA256SUMS.txt

      - name: Upload dry-run artifacts
        if: "!startsWith(github.ref, 'refs/tags/')"
        uses: actions/upload-artifact@v4
        with:
          name: release-dry-run
          path: |
            foreman-v*-x86_64-windows.zip
            SHA256SUMS.txt

      - name: Publish release
        if: startsWith(github.ref, 'refs/tags/')
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          gh release create '${{ github.ref_name }}' `
            "foreman-v${{ steps.ver.outputs.version }}-x86_64-windows.zip" `
            SHA256SUMS.txt `
            --generate-notes
```

- [ ] **Step 2: Commit and push a branch to dry-run it**

```bash
git checkout -b ci/release-pipeline
git add .github/workflows/release.yml
git commit -m "ci: tag-driven release pipeline with PR dry-run"
git push -u origin ci/release-pipeline
```

- [ ] **Step 3: Trigger a manual dry-run on the branch**

Run: `gh workflow run release.yml --ref ci/release-pipeline` then `gh run watch` (or `gh run list --workflow=release.yml` and `gh run view <id> --log-failed`).
Expected: green run; `release-dry-run` artifact contains `foreman-v0.1.0-x86_64-windows.zip` + `SHA256SUMS.txt`. **This is the spec's "empirically verify GNU on the runner" gate.** If the link step fails, add a MSYS2 mingw-w64 install step (`msys2/setup-msys2@v2` with `mingw-w64-x86_64-gcc`, prepend its bin to PATH) and re-run — record whichever worked in the workflow comments.

- [ ] **Step 4: Download and inspect the dry-run artifact**

Run: `gh run download <run-id> -n release-dry-run -D /tmp/dryrun` then expand and list the zip contents.
Expected: zip root holds exactly `foreman.exe`, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-Hack-font.md`, `THIRD-PARTY-ConPTY.txt`. Verify the checksum line matches `Get-FileHash` of the zip.

- [ ] **Step 5: Merge to main**

```bash
git checkout main && git merge --no-ff ci/release-pipeline -m "ci: release pipeline" && git push
```

(Or open a PR if preferred — the PR itself dry-runs the workflow via the `paths` trigger.)

---

### Task 3: install.ps1 + README install section

**Files:**
- Create: `install.ps1` (repo root)
- Modify: `README.md:9-11` (insert Install section between the intro and `## Run`)

**Interfaces:**
- Consumes: Release assets from Task 2 (zip selected by `-x86_64-windows.zip` suffix; `SHA256SUMS.txt` by exact name).
- Produces: the installed layout `%LOCALAPPDATA%\Programs\foreman\foreman.exe` + user PATH entry. Env override `FOREMAN_INSTALL_REPO` (default `sniffle6/foreman`) exists solely for testing against a fork.

- [ ] **Step 1: Write install.ps1**

```powershell
#Requires -Version 5.1
# Foreman installer / manual updater.
#   irm https://raw.githubusercontent.com/sniffle6/foreman/main/install.ps1 | iex
# Does exactly four things: download latest release zip, verify SHA256,
# extract to %LOCALAPPDATA%\Programs\foreman, add that dir to the USER PATH.
# No shortcuts, no registry. Re-run any time to update.
# NOTE: runs inside the user's shell via iex - never call `exit` here.

$ErrorActionPreference = 'Stop'

$repo = if ($env:FOREMAN_INSTALL_REPO) { $env:FOREMAN_INSTALL_REPO } else { 'sniffle6/foreman' }
$dest = Join-Path $env:LOCALAPPDATA 'Programs\foreman'
$zipSuffix = '-x86_64-windows.zip'

if (Get-Process -Name foreman -ErrorAction SilentlyContinue) {
    Write-Host 'foreman is running - close it first, then re-run this installer.' -ForegroundColor Yellow
    return
}

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$headers = @{ 'User-Agent' = 'foreman-install' }
$rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -Headers $headers

$asset = $rel.assets | Where-Object { $_.name.EndsWith($zipSuffix) } | Select-Object -First 1
if (-not $asset) { throw "release $($rel.tag_name) has no asset ending in $zipSuffix" }
$sums = $rel.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' } | Select-Object -First 1
if (-not $sums) { throw "release $($rel.tag_name) has no SHA256SUMS.txt" }

$tmp = Join-Path ([IO.Path]::GetTempPath()) "foreman-install-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zipPath = Join-Path $tmp $asset.name
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zipPath -Headers $headers
    $sumsPath = Join-Path $tmp 'SHA256SUMS.txt'
    Invoke-WebRequest -Uri $sums.browser_download_url -OutFile $sumsPath -Headers $headers

    $line = Get-Content $sumsPath | Where-Object { $_.EndsWith("  $($asset.name)") } | Select-Object -First 1
    if (-not $line) { throw "SHA256SUMS.txt has no entry for $($asset.name)" }
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $zipPath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "hash mismatch: expected $expected, got $actual - aborting install" }

    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Expand-Archive -Path $zipPath -DestinationPath $dest -Force

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($userPath -split ';') -notcontains $dest) {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$dest", 'User')
        Write-Host "added $dest to your user PATH (new terminals will pick it up)"
    }
    Write-Host "foreman $($rel.tag_name) installed to $dest" -ForegroundColor Green
    Write-Host "run it: `"$dest\foreman.exe`" (or 'foreman' from a new terminal)"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
```

- [ ] **Step 2: Parse-check the script**

Run: `pwsh -NoProfile -Command "[void][ScriptBlock]::Create((Get-Content -Raw install.ps1)); 'parse ok'"`
Expected: `parse ok` (a parse error throws instead).

- [ ] **Step 3: Insert the Install section into README.md**

Between the intro paragraph (`**Start here:** ...` line) and `## Run`, insert:

```markdown
## Install (Windows)

​```powershell
irm https://raw.githubusercontent.com/sniffle6/foreman/main/install.ps1 | iex
​```

Installs the [latest release](https://github.com/sniffle6/foreman/releases/latest)
to `%LOCALAPPDATA%\Programs\foreman` and adds it to your user PATH — no admin,
no installer wizard. Re-run the same line to update. Prefer manual? Grab the
zip from the releases page (note: extracting with Explorer may trigger
SmartScreen; the one-liner doesn't). Foreman will show a quiet chip in the
Sessions panel when a new version is available.
```

(Remove the zero-width characters around the inner code fence — they exist only to nest fences in this plan.)

- [ ] **Step 4: End-to-end test — deferred, by design**

Full E2E needs a published release and happens in Task 7 (pilot). Nothing to run here beyond Step 2's parse check.

- [ ] **Step 5: Commit**

```bash
git add install.ps1 README.md
git commit -m "feat: irm|iex install script + README install section"
```

---

### Task 4: `src/update.rs` pure core (state machine + decisions)

**Files:**
- Create: `src/update.rs`
- Modify: `src/main.rs` (add `mod update;` beside the other `mod` lines near the top)

**Interfaces:**
- Produces (consumed by Tasks 5–6):
  - `update::State` — `Idle | UpdateAvailable { version: String, html_url: String, can_apply: bool } | Downloading { progress: f32 } | ReadyToRestart | Error { retryable: bool }` (full enum per spec — the interface is frozen now; `Downloading`/`ReadyToRestart`/`Error` transitions land in the Phase-4 plan).
  - `update::Event` — `ReleaseFetched(ReleaseInfo) | FetchFailed | ClickChip | Progress(f32) | HashOk | HashBad | SwapOk | SwapFailed | ClickRestart`.
  - `update::Effect` — `FetchLatest | OpenReleasesPage(String) | Download(Asset) | VerifyAndSwap | SaveWorkspaceAndRestart`.
  - `pub fn step(state: State, ev: Event, current: &str) -> (State, Vec<Effect>)` (the spec's `step` with the current-version string threaded in so tests control it).
  - `pub fn parse_version(s: &str) -> Option<(u64, u64, u64)>`, `pub fn select_asset(assets: &[Asset]) -> Option<&Asset>`.
  - `pub struct ReleaseInfo { pub tag_name: String, pub html_url: String, pub assets: Vec<Asset> }`, `pub struct Asset { pub name: String, pub browser_download_url: String }` (both `#[derive(Debug, Clone, PartialEq, serde::Deserialize)]`).
  - Consts: `pub const ZIP_SUFFIX: &str = "-x86_64-windows.zip";`, `pub const RELEASES_URL: &str = "https://github.com/sniffle6/foreman/releases/latest";`

- [ ] **Step 1: Write the failing tests + type skeleton**

Create `src/update.rs` with the types above, all fns stubbed `todo!()`, and this test module (in-module convention, `src/layout.rs:600` style):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, assets: &[&str]) -> ReleaseInfo {
        ReleaseInfo {
            tag_name: tag.into(),
            html_url: "https://github.com/sniffle6/foreman/releases/tag/TEST".into(),
            assets: assets
                .iter()
                .map(|n| Asset { name: (*n).into(), browser_download_url: format!("https://x/{n}") })
                .collect(),
        }
    }

    // ── parse_version ────────────────────────────────────────────────
    #[test]
    fn parses_plain_and_v_prefixed_versions() {
        assert_eq!(parse_version("0.2.1"), Some((0, 2, 1)));
        assert_eq!(parse_version("v0.2.1"), Some((0, 2, 1)));
        assert_eq!(parse_version("v10.20.30"), Some((10, 20, 30)));
    }

    #[test]
    fn rejects_prereleases_and_garbage() {
        assert_eq!(parse_version("v0.2.1-rc1"), None);
        assert_eq!(parse_version("v0.2"), None);
        assert_eq!(parse_version("v0.2.1.4"), None);
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version(""), None);
    }

    // ── select_asset ─────────────────────────────────────────────────
    #[test]
    fn selects_zip_by_suffix_never_by_full_name() {
        let r = rel("v0.2.1", &["SHA256SUMS.txt", "foreman-v0.2.1-x86_64-windows.zip"]);
        assert_eq!(select_asset(&r.assets).unwrap().name, "foreman-v0.2.1-x86_64-windows.zip");
        let renamed = rel("v0.2.1", &["totally-different-x86_64-windows.zip"]);
        assert!(select_asset(&renamed.assets).is_some()); // suffix match survives renames
        let none = rel("v0.2.1", &["SHA256SUMS.txt", "foreman-setup.exe"]);
        assert!(select_asset(&none.assets).is_none());
    }

    // ── step: fetch outcomes ─────────────────────────────────────────
    #[test]
    fn newer_release_shows_chip_with_can_apply_false() {
        let (s, fx) = step(State::Idle, Event::ReleaseFetched(rel("v0.2.1", &[])), "0.2.0");
        assert_eq!(
            s,
            State::UpdateAvailable {
                version: "v0.2.1".into(),
                html_url: "https://github.com/sniffle6/foreman/releases/tag/TEST".into(),
                can_apply: false,
            }
        );
        assert!(fx.is_empty());
    }

    #[test]
    fn equal_older_or_unparseable_release_stays_idle() {
        for tag in ["v0.2.0", "v0.1.9", "v0.2.0-rc1", "junk"] {
            let (s, fx) = step(State::Idle, Event::ReleaseFetched(rel(tag, &[])), "0.2.0");
            assert_eq!(s, State::Idle, "tag {tag} must not offer an update");
            assert!(fx.is_empty());
        }
    }

    #[test]
    fn refetch_replaces_an_existing_offer() {
        let showing = State::UpdateAvailable { version: "v0.2.1".into(), html_url: "u".into(), can_apply: false };
        let (s, _) = step(showing, Event::ReleaseFetched(rel("v0.3.0", &[])), "0.2.0");
        match s {
            State::UpdateAvailable { version, .. } => assert_eq!(version, "v0.3.0"),
            other => panic!("expected UpdateAvailable, got {other:?}"),
        }
    }

    #[test]
    fn fetch_failure_is_silent_skip_in_any_state() {
        let showing = State::UpdateAvailable { version: "v0.2.1".into(), html_url: "u".into(), can_apply: false };
        let (s, fx) = step(showing.clone(), Event::FetchFailed, "0.2.0");
        assert_eq!(s, showing); // never downgrades the chip
        assert!(fx.is_empty());
        let (s, fx) = step(State::Idle, Event::FetchFailed, "0.2.0");
        assert_eq!(s, State::Idle);
        assert!(fx.is_empty());
    }

    // ── step: chip click (Phase 3 = notify-only) ────────────────────
    #[test]
    fn click_without_can_apply_opens_releases_page() {
        let showing = State::UpdateAvailable { version: "v0.2.1".into(), html_url: "https://gh/rel".into(), can_apply: false };
        let (s, fx) = step(showing.clone(), Event::ClickChip, "0.2.0");
        assert_eq!(s, showing);
        assert_eq!(fx, vec![Effect::OpenReleasesPage("https://gh/rel".into())]);
    }

    #[test]
    fn irrelevant_events_are_ignored() {
        let (s, fx) = step(State::Idle, Event::ClickChip, "0.2.0");
        assert_eq!(s, State::Idle);
        assert!(fx.is_empty());
        let (s, fx) = step(State::Idle, Event::SwapOk, "0.2.0");
        assert_eq!(s, State::Idle);
        assert!(fx.is_empty());
    }

    // ── GitHub JSON parsing (fixture from the real API shape) ───────
    #[test]
    fn parses_release_json_ignoring_unknown_fields() {
        let json = r#"{
            "tag_name": "v0.2.1",
            "html_url": "https://github.com/sniffle6/foreman/releases/tag/v0.2.1",
            "draft": false,
            "prerelease": false,
            "assets": [
                {"name": "SHA256SUMS.txt", "browser_download_url": "https://x/s", "size": 100},
                {"name": "foreman-v0.2.1-x86_64-windows.zip", "browser_download_url": "https://x/z", "size": 9999}
            ]
        }"#;
        let r: ReleaseInfo = serde_json::from_str(json).unwrap();
        assert_eq!(r.tag_name, "v0.2.1");
        assert_eq!(select_asset(&r.assets).unwrap().browser_download_url, "https://x/z");
    }
}
```

Add `mod update;` in `src/main.rs` next to the existing `mod` declarations.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test update` (add `--target-dir target/agent` if `$env:FOREMAN` is `1`)
Expected: FAIL — panics on `todo!()` (or compile errors until the skeleton is complete; fix skeleton until it compiles, then the `todo!()` panics are the expected failures).

- [ ] **Step 3: Implement the pure core**

```rust
//! In-app update: pure state machine + decisions. The worker thread (Task 5)
//! executes Effects and reports Events; nothing in this file does I/O.
//! Spec: docs/superpowers/specs/2026-07-14-install-and-update-design.md §3.

pub const ZIP_SUFFIX: &str = "-x86_64-windows.zip";
pub const RELEASES_URL: &str = "https://github.com/sniffle6/foreman/releases/latest";

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub html_url: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    UpdateAvailable { version: String, html_url: String, can_apply: bool },
    Downloading { progress: f32 },
    ReadyToRestart,
    Error { retryable: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    ReleaseFetched(ReleaseInfo),
    FetchFailed,
    ClickChip,
    Progress(f32),
    HashOk,
    HashBad,
    SwapOk,
    SwapFailed,
    ClickRestart,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    FetchLatest,
    OpenReleasesPage(String),
    Download(Asset),
    VerifyAndSwap,
    SaveWorkspaceAndRestart,
}

/// Phase 3 is notify-only; Phase 4 flips this to a real writability probe.
const CAN_APPLY: bool = false;

/// Strict `X.Y.Z` (optional leading `v`). Anything else — prereleases,
/// two-part versions — is None, which the caller treats as "no update"
/// (spec: upgrades only, silent-skip on weirdness).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut it = s.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next()?.parse().ok()?;
    let pat = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((maj, min, pat))
}

/// Suffix selection over the release's asset list — consumers never
/// reconstruct full filenames (spec §1, asset naming convention).
pub fn select_asset(assets: &[Asset]) -> Option<&Asset> {
    assets.iter().find(|a| a.name.ends_with(ZIP_SUFFIX))
}

pub fn step(state: State, ev: Event, current: &str) -> (State, Vec<Effect>) {
    use Effect as X;
    use Event as E;
    use State as S;
    match (state, ev) {
        (S::Idle, E::ReleaseFetched(r)) | (S::UpdateAvailable { .. }, E::ReleaseFetched(r)) => {
            match (parse_version(current), parse_version(&r.tag_name)) {
                (Some(cur), Some(new)) if new > cur => (
                    S::UpdateAvailable {
                        version: r.tag_name,
                        html_url: r.html_url,
                        can_apply: CAN_APPLY && select_asset(&r.assets).is_some(),
                    },
                    vec![],
                ),
                _ => (S::Idle, vec![]),
            }
        }
        (S::UpdateAvailable { version, html_url, can_apply: false }, E::ClickChip) => {
            let url = html_url.clone();
            (S::UpdateAvailable { version, html_url, can_apply: false }, vec![X::OpenReleasesPage(url)])
        }
        // FetchFailed is a silent skip everywhere; Phase-4 events
        // (Progress/HashOk/HashBad/SwapOk/SwapFailed/ClickRestart and
        // can_apply:true clicks) are wired in the Phase-4 plan.
        (s, _) => (s, vec![]),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test update`
Expected: all 10 tests PASS. Also run the full suite once: `cargo test` — no regressions.

- [ ] **Step 5: Commit**

```bash
git add src/update.rs src/main.rs
git commit -m "feat(update): pure state machine, version/asset decisions"
```

---

### Task 5: Worker thread + App wiring + gating (ureq)

**Files:**
- Modify: `Cargo.toml` (add `ureq = "3"` to `[dependencies]`)
- Modify: `src/update.rs` (append fetch + spawn below the pure core)
- Modify: `src/main.rs` (App fields ~`:55`, `App::new` ~`:97`, per-frame drain near `:477`, spawn near `:785`)

**Interfaces:**
- Consumes: `update::{State, Event, Effect, step, ReleaseInfo}` from Task 4; the thread-spawn pattern of `src/main.rs:785-787` (`cc.egui_ctx.clone()` into `std::thread::spawn`); the drain pattern of `src/main.rs:477` (`while let Ok(..) = rx.try_recv()`).
- Produces (consumed by Task 6):
  - `update::spawn(ctx: egui::Context, event_tx: mpsc::Sender<Event>, effect_rx: mpsc::Receiver<Effect>)` — worker: self-scheduled checks (10 s after launch, then every 6 h), executes `Effect::FetchLatest` and `Effect::OpenReleasesPage` (via `ctx.open_url`), ignores Phase-4 effects.
  - `App.update_state: update::State` field, kept current by the per-frame drain; `App.update_fx: mpsc::Sender<update::Effect>` for sending click-driven effects.
  - `App::new(ctrl: Receiver<control::CtrlMsg>, update_rx: Receiver<update::Event>, update_fx: Sender<update::Effect>) -> Self` (extended signature).

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` `[dependencies]` (alphabetical, after `unicode-width`):

```toml
# Update check only: blocking HTTP on a plain thread, rustls default.
# NOTE: default TLS roots are webpki, NOT the Windows cert store — corporate
# MITM proxies fail the check; that is absorbed by silent-skip (spec §Deps).
ureq = "3"
```

Run: `cargo build` (or `--target-dir target/agent` inside foreman). Expected: compiles; `Cargo.lock` picks ureq 3.x.

- [ ] **Step 2: Write the failing fetch-parse test**

`parses_release_json_ignoring_unknown_fields` from Task 4 already covers JSON→`ReleaseInfo`. Add one test for the fetch wrapper's error path (in `mod tests`):

```rust
    #[test]
    fn fetch_error_maps_to_fetch_failed_event() {
        // fetch_latest against an unroutable host must produce Err, which the
        // worker maps to Event::FetchFailed (never a panic).
        let err = fetch_url("http://127.0.0.1:9/releases/latest");
        assert!(err.is_err());
    }
```

Run: `cargo test update` — FAILS (`fetch_url` undefined).

- [ ] **Step 3: Implement fetch + worker**

Append to `src/update.rs` (below the pure core, above `mod tests`):

```rust
// ─── I/O edge: everything below runs on the worker thread ───────────────

const API_URL: &str = "https://api.github.com/repos/sniffle6/foreman/releases/latest";
const FIRST_CHECK: std::time::Duration = std::time::Duration::from_secs(10);
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

fn fetch_url(url: &str) -> Result<ReleaseInfo, String> {
    let ua = concat!("foreman/", env!("CARGO_PKG_VERSION"), " (+https://github.com/sniffle6/foreman)");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("User-Agent", ua)
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

fn fetch_latest() -> Result<ReleaseInfo, String> {
    fetch_url(API_URL)
}

/// Worker thread: executes Effects, reports Events (channel + repaint —
/// the same seam shape as Session's PTY reader thread, terminal.rs:824).
/// Self-schedules the periodic check; Phase-4 effects are accepted and
/// dropped so the GUI never needs to know which phase is compiled in.
pub fn spawn(
    ctx: eframe::egui::Context,
    event_tx: std::sync::mpsc::Sender<Event>,
    effect_rx: std::sync::mpsc::Receiver<Effect>,
) {
    std::thread::spawn(move || {
        let mut next_check = std::time::Instant::now() + FIRST_CHECK;
        loop {
            let wait = next_check.saturating_duration_since(std::time::Instant::now());
            match effect_rx.recv_timeout(wait) {
                Ok(Effect::FetchLatest) => {}
                Ok(Effect::OpenReleasesPage(url)) => {
                    ctx.open_url(eframe::egui::OpenUrl::new_tab(url));
                    ctx.request_repaint();
                    continue;
                }
                Ok(_) => continue, // Phase-4 effects: not executed yet
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let ev = match fetch_latest() {
                Ok(r) => Event::ReleaseFetched(r),
                Err(e) => {
                    eprintln!("update: check failed (will retry): {e}");
                    Event::FetchFailed
                }
            };
            if event_tx.send(ev).is_err() {
                break;
            }
            ctx.request_repaint();
            next_check = std::time::Instant::now() + CHECK_EVERY;
        }
    });
}
```

(If `ureq::Agent::config_builder()` doesn't exist under the resolved 3.x minor, the equivalent is `ureq::config::Config::builder()...build().new_agent()` — check `docs.rs/ureq` for the locked version; the required behavior is fixed: 10 s global timeout + the exact User-Agent above.)

- [ ] **Step 4: Wire into main.rs**

Three edits, mirroring the control-pipe wiring exactly:

1. App fields (near `ctrl` at `src/main.rs:55`):

```rust
    /// Update-check events from the worker (update::spawn); drained per-frame.
    update_rx: std::sync::mpsc::Receiver<update::Event>,
    /// Effects for the worker to execute (fetch now / open releases page).
    update_fx: std::sync::mpsc::Sender<update::Effect>,
    /// Current updater state; rendered by the panel chip (Task 6).
    update_state: update::State,
```

2. `App::new` (at `:97`) becomes:

```rust
    fn new(
        ctrl: std::sync::mpsc::Receiver<control::CtrlMsg>,
        update_rx: std::sync::mpsc::Receiver<update::Event>,
        update_fx: std::sync::mpsc::Sender<update::Effect>,
    ) -> Self {
```

with field inits `update_rx, update_fx, update_state: update::State::Idle,`. Debug preview hook (so the chip is screenshotable despite the release-only gate) — in `new`, after the struct init line for `update_state`:

```rust
        // Debug-only preview: FOREMAN_UPDATE_TEST=1 fakes an available update
        // so the chip can be seen/screenshotted without a real newer release.
        let update_state = if cfg!(debug_assertions) && std::env::var_os("FOREMAN_UPDATE_TEST").is_some() {
            update::State::UpdateAvailable {
                version: "v9.9.9".into(),
                html_url: update::RELEASES_URL.into(),
                can_apply: false,
            }
        } else {
            update::State::Idle
        };
```

3. Per-frame drain (immediately after the ctrl drain at `src/main.rs:477-482`):

```rust
        while let Ok(ev) = self.update_rx.try_recv() {
            let state = std::mem::replace(&mut self.update_state, update::State::Idle);
            let (state, effects) = update::step(state, ev, env!("CARGO_PKG_VERSION"));
            self.update_state = state;
            for fx in effects {
                let _ = self.update_fx.send(fx);
            }
        }
```

4. Spawn (in the `run_native` closure, beside `control::serve` at `:785-787`):

```rust
            let (upd_event_tx, upd_event_rx) = std::sync::mpsc::channel();
            let (upd_effect_tx, upd_effect_rx) = std::sync::mpsc::channel();
            // Release builds only; FOREMAN_NO_UPDATE=1 is the escape hatch
            // (spec §3 gating). Debug builds never phone home.
            if !cfg!(debug_assertions) && std::env::var_os("FOREMAN_NO_UPDATE").is_none() {
                update::spawn(cc.egui_ctx.clone(), upd_event_tx, upd_effect_rx);
            }
            Ok(Box::new(App::new(rx, upd_event_rx, upd_effect_tx)))
```

(The channels are created unconditionally so `App` always holds live-typed ends; when the worker isn't spawned, `update_rx` just never yields and sends into `update_fx` return `Err`, which is ignored.)

- [ ] **Step 5: Build + test**

Run: `cargo test update` — expected: all tests pass (including `fetch_error_maps_to_fetch_failed_event`). Then `cargo build` — compiles clean. Then full `cargo test` — no regressions.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/update.rs src/main.rs
git commit -m "feat(update): background check worker, App wiring, release-only gating"
```

---

### Task 6: Panel chip (render + click → open releases page)

**Files:**
- Modify: `src/panel.rs` (`PanelModel:89`, `PanelView:103`, `show:139`, new `paint_update_chip`)
- Modify: `src/wm.rs` (`panel_model:1658`, panel drain `:1884-1898`, new field + two methods)
- Modify: `src/main.rs` (set chip before `desktop` draws; consume click after)

**Interfaces:**
- Consumes: `App.update_state` / `App.update_fx` from Task 5; `update::{State, Event, step}`; theme consts `SEL_BG`, `SNAP_STROKE`, `DIM`, `BG`; the deferred-Act click pattern (view records → wm drains, `wm.rs:1884`).
- Produces:
  - `PanelModel.update: Option<String>` (version string, e.g. `"v0.2.1"`; `None` = no chip).
  - `PanelView.update_click: bool` (recorded during paint, drained by wm).
  - `WindowManager::set_update_chip(&mut self, v: Option<String>)` and `WindowManager::take_update_click(&mut self) -> bool`.

- [ ] **Step 1: PanelModel + PanelView fields**

In `src/panel.rs`: add `pub update: Option<String>,` to `PanelModel` (`:89`) and `pub update_click: bool,` to `PanelView` (`:103`, init `false` in `new`/`with_dock`). Fix `PanelModel` construction in `wm.rs:1658` (`panel_model`) to set `update: self.update_chip.clone(),`.

In `src/wm.rs`: add to `WindowManager`:

```rust
    /// Version string to show as the panel's update chip (None = hidden).
    update_chip: Option<String>,
    /// Latched when the user clicks the chip; App drains it each frame.
    update_clicked: bool,
```

(init both in the constructor: `update_chip: None, update_clicked: false,`) plus:

```rust
    pub fn set_update_chip(&mut self, v: Option<String>) {
        self.update_chip = v;
    }

    pub fn take_update_click(&mut self) -> bool {
        std::mem::take(&mut self.update_clicked)
    }
```

- [ ] **Step 2: Footer chip in PanelView::show**

In `show` (`panel.rs:139`), right after the background fill (`p.rect_filled(rect, 0.0, BG)` at `:141`), carve a footer band and shrink the body rect the row layout uses (the expanded, vertical layout only — rail/collapsed and horizontal modes skip the chip; YAGNI until someone misses it):

```rust
        const UPDATE_CHIP_H: f32 = 26.0;
        let mut body = rect;
        if !self.collapsed && self.model.update.is_some() {
            let footer = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.max.y - UPDATE_CHIP_H),
                rect.max,
            );
            body.max.y -= UPDATE_CHIP_H;
            let ver = self.model.update.clone().unwrap();
            self.paint_update_chip(ui, footer, base, &ver);
        }
```

then use `body` (not `rect`) as the bounds for the existing row-layout region (the `y` loop starting near `:176` and the scroll clamp — every later use of `rect.max.y` in the row path becomes `body.max.y`).

New method on `PanelView`, mirroring the `paint_strip` chip idiom (`panel.rs:401`: measured galley, `ui.interact` + `Sense::click()`, `SEL_BG` hover fill, rounded 5):

```rust
    /// Quiet update-available chip pinned to the panel's bottom edge.
    /// Click is recorded (deferred-Act, like `self.click`) and drained by wm.
    fn paint_update_chip(&mut self, ui: &mut egui::Ui, footer: egui::Rect, base: egui::Id, ver: &str) {
        let p = ui.painter();
        let chip = footer.shrink2(egui::vec2(6.0, 3.0));
        let id = base.with("update-chip");
        let resp = ui.interact(chip, id, egui::Sense::click());
        if resp.hovered() {
            p.rect_filled(chip, egui::CornerRadius::same(5), SEL_BG);
        }
        let text = format!("↓ {ver} — click for release notes");
        let galley = p.layout_no_wrap(
            text,
            egui::FontId::proportional(12.0),
            if resp.hovered() { SNAP_STROKE } else { DIM },
        );
        let pos = egui::pos2(
            chip.min.x + 6.0,
            chip.center().y - galley.size().y / 2.0,
        );
        p.galley(pos, galley, SNAP_STROKE);
        if resp.clicked() {
            self.update_click = true;
        }
    }
```

(`SEL_BG`, `SNAP_STROKE`, `DIM` are already glob-imported in panel.rs via the theme import at the top of the file — verify the `use` line includes them, extend it if not. If the galley overflows the collapsed-width chip, `TextWrapping::truncate_at_width` per `panel.rs:421-429` is the fix — copy that idiom.)

- [ ] **Step 3: Drain the click in wm, surface to App**

In the existing panel drain loop (`wm.rs:1884-1898`), alongside `v.click.take()` / `v.toggle_collapse`:

```rust
                    if v.update_click {
                        v.update_click = false;
                        self.update_clicked = true;
                    }
```

In `src/main.rs` `ui()`: before the desktop draws (right after the update-event drain added in Task 5):

```rust
        self.desktop.set_update_chip(match &self.update_state {
            update::State::UpdateAvailable { version, .. } => Some(version.clone()),
            _ => None,
        });
```

and after the desktop's draw/act pass in the same frame (immediately after the call that draws `self.desktop`):

```rust
        if self.desktop.take_update_click() {
            let state = std::mem::replace(&mut self.update_state, update::State::Idle);
            let (state, effects) = update::step(state, update::Event::ClickChip, env!("CARGO_PKG_VERSION"));
            self.update_state = state;
            for fx in effects {
                let _ = self.update_fx.send(fx);
            }
        }
```

**Note the effect path:** click → `Effect::OpenReleasesPage` → worker thread → `ctx.open_url` → default browser. In debug builds the worker was never spawned, so the debug preview chip's click sends into a dead channel — the click is a visual no-op there; that's fine and expected (verify the browser open in the Task 7 pilot on a release build).

- [ ] **Step 4: Build + unit tests**

Run: `cargo test` then `cargo build`. Expected: clean; no panel/wm test regressions (`cargo test wm`, `cargo test layout` still green).

- [ ] **Step 5: Visual verification (screenshot — working agreement)**

Run: `$env:FOREMAN_UPDATE_TEST='1'; cargo run` (use `--target-dir target/agent` + ask the user to run it if inside foreman). Screenshot the window (script in `docs/HANDOFF.md` §3) and `Read` the PNG.
Expected: Sessions panel shows a quiet `↓ v9.9.9 — click for release notes` chip pinned to the panel bottom, amber on hover, and the session rows still lay out correctly above it (no overlap with the last row). Clean up: `Remove-Item Env:FOREMAN_UPDATE_TEST`.

- [ ] **Step 6: Commit**

```bash
git add src/panel.rs src/wm.rs src/main.rs
git commit -m "feat(update): panel footer chip, click opens releases page"
```

---

### Task 7: Pilot releases + feature doc

**Files:**
- Modify: `Cargo.toml:3` (version bumps)
- Create: `docs/installing-and-updating.md`

**Interfaces:**
- Consumes: everything above, live on `main`, pushed.
- Produces: real releases `v0.2.0` and `v0.2.1`; the evidence that opens the spec's Phase-4 gate.

- [ ] **Step 1: Ship v0.2.0 (the shakedown release)**

```bash
# on main, everything merged and green
# edit Cargo.toml: version = "0.2.0"
cargo build   # refreshes Cargo.lock's own-version entry
git add Cargo.toml Cargo.lock
git commit -m "chore: v0.2.0"
git tag v0.2.0
git push && git push origin v0.2.0
gh run watch
```

Expected: workflow green; `gh release view v0.2.0` shows `foreman-v0.2.0-x86_64-windows.zip` + `SHA256SUMS.txt` + generated notes.

- [ ] **Step 2: Prove the one-liner**

On this machine (foreman CLOSED — the script refuses otherwise; that refusal is itself a test assertion to observe once with foreman open), in a fresh PowerShell:

```powershell
irm https://raw.githubusercontent.com/sniffle6/foreman/main/install.ps1 | iex
```

Expected: green "foreman v0.2.0 installed to C:\Users\...\AppData\Local\Programs\foreman"; `%LOCALAPPDATA%\Programs\foreman` contains the exe + 4 license/notice files; a NEW terminal resolves `(Get-Command foreman).Source` to that path; the installed exe launches and passes a quick smoke (open a project, type in a terminal). Also verify no SmartScreen dialog appeared at any point.

- [ ] **Step 3: Ship v0.2.1 and watch the chip**

Repeat Step 1 with `version = "0.2.1"` and tag `v0.2.1` (any trivial change; the version bump itself is enough). Then launch the INSTALLED v0.2.0 exe and wait ~15 s.
Expected: the `↓ v0.2.1` chip appears in the Sessions panel (first check fires 10 s after launch); clicking it opens the GitHub release page in the default browser; `FOREMAN_NO_UPDATE=1` relaunch shows no chip. Update the installed copy by re-running the one-liner (foreman closed) — it lands v0.2.1 and the chip is gone on next launch.

- [ ] **Step 4: Write the feature doc (per user's global docs rule — grug-simple)**

Create `docs/installing-and-updating.md`:

```markdown
# Installing and updating foreman

## What it does
Users install foreman with one PowerShell line. Foreman quietly checks GitHub
a couple times a day and shows a small chip in the Sessions panel when a newer
release exists; clicking it opens the release page. Updating = re-run the
install line with foreman closed. (One-click in-app updating is planned —
"Phase 4" in the spec — after this loop proves itself.)

## Why it exists
Public distribution without an installer wizard, an update server, or code
signing. The GitHub Release IS the update manifest; the irm|iex path dodges
SmartScreen because PowerShell downloads don't carry Mark-of-the-Web.

## How to use it
- Install/update: `irm https://raw.githubusercontent.com/sniffle6/foreman/main/install.ps1 | iex`
- Release a new version: bump `Cargo.toml`, commit, `git tag vX.Y.Z`, push the
  tag. CI does the rest (refuses if tag != Cargo.toml).
- Kill the update check: `FOREMAN_NO_UPDATE=1`. Debug builds never check.
- Preview the chip in a debug build: `FOREMAN_UPDATE_TEST=1`.

## Gotchas
- Asset names are written ONLY by `.github/workflows/release.yml`; the script
  and the app find the zip by the `-x86_64-windows.zip` SUFFIX. Rename things
  in one place only.
- `install.ps1` runs under `iex` in the user's own shell — `exit` there would
  kill their terminal. It uses `return`/`throw` and must stay that way.
- The update state machine is pure (`update::step`) — new behavior goes in
  `step` + a unit test, never inline in the worker thread or the GUI.
- ureq uses webpki roots, not the Windows cert store: corporate MITM proxies
  make the check fail silently. That's accepted (spec §Dependencies).

## Key files
- `.github/workflows/release.yml` — tag → test → build → zip + SHA256SUMS → Release.
- `install.ps1` — download, verify hash, extract, user PATH. Nothing else.
- `src/update.rs` — pure state machine + worker thread (check/effects).
- `src/panel.rs` (`paint_update_chip`) / `src/wm.rs` (chip state + click drain)
  / `src/main.rs` (wiring, gating) — the chip path.
- Spec: `docs/superpowers/specs/2026-07-14-install-and-update-design.md`.
```

- [ ] **Step 5: Commit**

```bash
git add docs/installing-and-updating.md
git commit -m "docs: installing-and-updating feature doc"
git push
```

- [ ] **Step 6: Record the Phase-4 gate as open**

Tell the user: the v0.2.x pilot is complete — the spec's Phase-4 gate (one-click apply) is now open and needs its own plan when they want it.

---

## Self-review notes (done at authoring time)

- **Spec coverage:** §1 pipeline → Task 2 (incl. GNU-verify + LICENSE prerequisite in Task 1); §2 script/zip/layout → Tasks 2–3; §3 gating/check/states/pure-core/threading → Tasks 4–6 (Phase-4 states frozen in the enum, transitions deferred per spec phasing); §4 error handling → silent-skip in `step` tests + worker eprintln; §5 testing → unit tests Tasks 4–5, pilot Task 7. Deferred by spec: Inno/winget/signing/domain/`.old` cleanup (the `.old` artifact only exists once Phase 4 swaps).
- **Two-workflow deviation** documented in Global Constraints.
- **Type consistency:** `State::UpdateAvailable { version, html_url, can_apply }`, `PanelModel.update: Option<String>`, `PanelView.update_click: bool`, `set_update_chip`/`take_update_click`, `spawn(ctx, event_tx, effect_rx)`, extended `App::new(ctrl, update_rx, update_fx)` — names match across Tasks 4/5/6.
