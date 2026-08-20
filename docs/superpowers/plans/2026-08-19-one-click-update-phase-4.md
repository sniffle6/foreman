# One-Click Update (Phase 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Flip foreman's updater from notify-only to one-click apply: chip click downloads the release zip, verifies SHA-256, swaps the running exe via the two-rename dance, and restarts on an armed second click.

**Architecture:** The pure state machine in `src/update.rs` grows the Phase 4 transitions (already-stubbed effects become real); the worker thread implements download/verify/swap; App (`src/main.rs`) handles the restart effect (workspace flush → spawn new exe → exit) and the arm timer; the panel chip grows state variants. All I/O stays on the worker or in App — `step` remains pure and table-tested.

**Tech Stack:** Rust, egui/eframe 0.34, ureq 3 (existing), `zip` + `sha2` (new deps), `windows-sys` (existing dep, `Win32_System_Threading` already enabled).

**Spec:** `docs/superpowers/specs/2026-08-19-one-click-update-phase-4-design.md` (extends `docs/superpowers/specs/2026-07-14-install-and-update-design.md`)

## Global Constraints

- Windows only; GNU toolchain (`stable-gnu`); build loop per CLAUDE.md — kill the running app **by exe path** first, never by name; if `$env:FOREMAN` is `1`, build with `cargo build --target-dir target/agent` instead of killing.
- New deps limited to: `zip = { version = "5", default-features = false, features = ["deflate"] }` and `sha2 = "0.10"`. Nothing else.
- Asset names are never reconstructed from a version: zip selected by `-x86_64-windows.zip` suffix, checksums by exact name `SHA256SUMS.txt`.
- Swap staging names (parent spec fact table): `foreman.exe.new` / `foreman.exe.old`, beside the exe. Download staging: `%TEMP%\foreman-update\`.
- Debug builds never update (`cfg(debug_assertions)`); `FOREMAN_NO_UPDATE=1` disables in release. `FOREMAN_UPDATE_TEST` is debug-only.
- Never install unverified bytes: hash mismatch deletes the download.
- The swap is never left half-done: rename-2 failure renames `.old` back.
- Commit messages: `type(scope): subject` + blank line + body, ending with the `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` trailer. Run `cargo test` before every commit.
- egui 0.34: `App::ui(&mut Ui, ...)`, painter-based text layout (see CLAUDE.md gotchas).

---

### Task 1: State machine rework (pure core, `src/update.rs`)

Replace the Phase-3 state shapes with the full Phase-4 machine. Everything in this task is pure and table-tested; no I/O changes.

**Files:**
- Modify: `src/update.rs` (types at top, `step`, tests at bottom)
- Modify: `src/main.rs:127-137` (FOREMAN_UPDATE_TEST initial state — new struct shape), `src/main.rs:554-557` (chip mapping reads `offer.version`)

**Interfaces:**
- Produces (later tasks consume these exact shapes):

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub version: String,  // release tag, e.g. "v0.3.0"
    pub html_url: String,
    pub zip: Option<Asset>,
    pub sums: Option<Asset>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    UpdateAvailable { offer: Offer, can_apply: bool },
    Downloading { offer: Offer, progress: f32 },
    ReadyToRestart { armed: bool },
    Error { offer: Offer, retryable: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    ReleaseFetched { info: ReleaseInfo, writable: bool },
    FetchFailed,
    ClickChip,
    Progress(f32),
    DownloadDone,
    HashBad,
    SwapOk,
    SwapFailed,
    ArmTimeout,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    FetchLatest,
    OpenReleasesPage(String),
    Download { zip: Asset, sums: Asset },
    VerifyAndSwap,
    SaveWorkspaceAndRestart,
}

pub fn select_sums(assets: &[Asset]) -> Option<&Asset>;  // exact name "SHA256SUMS.txt"
```

  Deleted: `Event::HashOk`, `Event::ClickRestart` (the chip is one widget; `ClickChip` serves every state), the `CAN_APPLY` const.

**Transition table** (implement exactly; everything not listed is `(s, _) => (s, vec![])`):

| State | Event | New state | Effects |
|---|---|---|---|
| `Idle` / `UpdateAvailable` / `Error` | `ReleaseFetched` (strictly newer, parseable) | `UpdateAvailable { offer, can_apply: writable && zip.is_some() && sums.is_some() }` | — |
| `Idle` / `UpdateAvailable` / `Error` | `ReleaseFetched` (equal/older/unparseable) | `Idle` | — |
| `Downloading` / `ReadyToRestart` | `ReleaseFetched` | unchanged (ignore-newer decision) | — |
| `Idle` / `UpdateAvailable` | `FetchFailed` | unchanged (silent skip) | — |
| `UpdateAvailable { can_apply: false }` | `ClickChip` | unchanged | `OpenReleasesPage(offer.html_url)` |
| `UpdateAvailable { can_apply: true }` | `ClickChip` | `Downloading { offer, progress: 0.0 }` | `Download { zip, sums }` (both are `Some` — `can_apply` guarantees it) |
| `Downloading` | `Progress(p)` | `Downloading { progress: p, .. }` | — |
| `Downloading` | `FetchFailed` (download error) | `Error { offer, retryable: true }` | — |
| `Downloading` | `DownloadDone` | `Downloading { progress: 1.0, .. }` | `VerifyAndSwap` |
| `Downloading` | `HashBad` | `Error { offer, retryable: true }` | — |
| `Downloading` | `SwapFailed` | `Error { offer, retryable: false }` | — |
| `Downloading` | `SwapOk` | `ReadyToRestart { armed: false }` | — |
| `ReadyToRestart { armed: false }` | `ClickChip` | `ReadyToRestart { armed: true }` | — |
| `ReadyToRestart { armed: true }` | `ClickChip` | unchanged | `SaveWorkspaceAndRestart` |
| `ReadyToRestart { armed: true }` | `ArmTimeout` | `ReadyToRestart { armed: false }` | — |
| `Error { retryable: true }` | `ClickChip` | if `zip`&`sums` are `Some`: `Downloading { offer, 0.0 }`, else unchanged | `Download {..}`, else `OpenReleasesPage` |
| `Error { retryable: false }` | `ClickChip` | unchanged | `OpenReleasesPage(offer.html_url)` |

- [ ] **Step 1: Rewrite the test module first** — update the existing 8 tests to the new shapes and add the new transition tests. Key new tests (write all of these; the `rel` fixture helper stays):

```rust
fn offer(zip: bool, sums: bool) -> Offer {
    Offer {
        version: "v0.3.0".into(),
        html_url: "https://gh/rel".into(),
        zip: zip.then(|| Asset { name: "foreman-v0.3.0-x86_64-windows.zip".into(), browser_download_url: "https://x/z".into() }),
        sums: sums.then(|| Asset { name: "SHA256SUMS.txt".into(), browser_download_url: "https://x/s".into() }),
    }
}

#[test]
fn writable_fetch_with_both_assets_offers_apply() {
    let info = rel("v0.3.0", &["SHA256SUMS.txt", "foreman-v0.3.0-x86_64-windows.zip"]);
    let (s, _) = step(State::Idle, Event::ReleaseFetched { info, writable: true }, "0.2.10");
    assert!(matches!(s, State::UpdateAvailable { can_apply: true, .. }));
}

#[test]
fn unwritable_or_missing_assets_fall_back_to_notify() {
    let full = rel("v0.3.0", &["SHA256SUMS.txt", "foreman-v0.3.0-x86_64-windows.zip"]);
    let (s, _) = step(State::Idle, Event::ReleaseFetched { info: full.clone(), writable: false }, "0.2.10");
    assert!(matches!(s, State::UpdateAvailable { can_apply: false, .. }));
    let no_sums = rel("v0.3.0", &["foreman-v0.3.0-x86_64-windows.zip"]);
    let (s, _) = step(State::Idle, Event::ReleaseFetched { info: no_sums, writable: true }, "0.2.10");
    assert!(matches!(s, State::UpdateAvailable { can_apply: false, .. }));
}

#[test]
fn apply_click_starts_download() {
    let s = State::UpdateAvailable { offer: offer(true, true), can_apply: true };
    let (s, fx) = step(s, Event::ClickChip, "0.2.10");
    assert!(matches!(s, State::Downloading { progress, .. } if progress == 0.0));
    assert!(matches!(fx.as_slice(), [Effect::Download { .. }]));
}

#[test]
fn download_completes_then_verifies_then_restart_offer() {
    let s = State::Downloading { offer: offer(true, true), progress: 0.0 };
    let (s, _) = step(s, Event::Progress(0.43), "0.2.10");
    assert!(matches!(s, State::Downloading { progress, .. } if (progress - 0.43).abs() < 1e-6));
    let (s, fx) = step(s, Event::DownloadDone, "0.2.10");
    assert_eq!(fx, vec![Effect::VerifyAndSwap]);
    let (s, fx) = step(s, Event::SwapOk, "0.2.10");
    assert_eq!(s, State::ReadyToRestart { armed: false });
    assert!(fx.is_empty());
}

#[test]
fn hash_bad_and_download_failure_are_retryable_swap_failure_is_not() {
    for (ev, retryable) in [(Event::HashBad, true), (Event::FetchFailed, true), (Event::SwapFailed, false)] {
        let s = State::Downloading { offer: offer(true, true), progress: 0.5 };
        let (s, _) = step(s, ev, "0.2.10");
        assert_eq!(s, State::Error { offer: offer(true, true), retryable });
    }
}

#[test]
fn retryable_error_click_redownloads_nonretryable_opens_page() {
    let s = State::Error { offer: offer(true, true), retryable: true };
    let (s, fx) = step(s, Event::ClickChip, "0.2.10");
    assert!(matches!(s, State::Downloading { .. }));
    assert!(matches!(fx.as_slice(), [Effect::Download { .. }]));
    let s = State::Error { offer: offer(true, true), retryable: false };
    let (_, fx) = step(s, Event::ClickChip, "0.2.10");
    assert_eq!(fx, vec![Effect::OpenReleasesPage("https://gh/rel".into())]);
}

#[test]
fn restart_requires_arm_then_confirm_and_timeout_disarms() {
    let (s, fx) = step(State::ReadyToRestart { armed: false }, Event::ClickChip, "0.2.10");
    assert_eq!(s, State::ReadyToRestart { armed: true });
    assert!(fx.is_empty());
    let (s2, fx) = step(s.clone(), Event::ArmTimeout, "0.2.10");
    assert_eq!(s2, State::ReadyToRestart { armed: false });
    assert!(fx.is_empty());
    let (_, fx) = step(s, Event::ClickChip, "0.2.10");
    assert_eq!(fx, vec![Effect::SaveWorkspaceAndRestart]);
}

#[test]
fn newer_release_while_busy_is_ignored() {
    let info = rel("v9.9.9", &["SHA256SUMS.txt", "foreman-v9.9.9-x86_64-windows.zip"]);
    for s in [State::Downloading { offer: offer(true, true), progress: 0.5 },
              State::ReadyToRestart { armed: false }] {
        let (s2, fx) = step(s.clone(), Event::ReleaseFetched { info: info.clone(), writable: true }, "0.2.10");
        assert_eq!(s2, s);
        assert!(fx.is_empty());
    }
}

#[test]
fn error_state_accepts_a_fresh_offer() {
    let info = rel("v0.4.0", &["SHA256SUMS.txt", "foreman-v0.4.0-x86_64-windows.zip"]);
    let s = State::Error { offer: offer(true, true), retryable: true };
    let (s, _) = step(s, Event::ReleaseFetched { info, writable: true }, "0.2.10");
    assert!(matches!(s, State::UpdateAvailable { offer, .. } if offer.version == "v0.4.0"));
}

#[test]
fn selects_sums_by_exact_name() {
    let r = rel("v0.3.0", &["SHA256SUMS.txt", "foreman-v0.3.0-x86_64-windows.zip"]);
    assert_eq!(select_sums(&r.assets).unwrap().name, "SHA256SUMS.txt");
    let r = rel("v0.3.0", &["foreman-v0.3.0-x86_64-windows.zip"]);
    assert!(select_sums(&r.assets).is_none());
}
```

  Existing tests to adapt: `newer_release_shows_chip_with_can_apply_false` (pass `writable: false`), `click_without_can_apply_opens_releases_page`, `refetch_replaces_an_existing_offer`, `fetch_failure_is_silent_skip_in_any_state`, `irrelevant_events_are_ignored` (SwapOk in Idle still ignored), `equal_older_or_unparseable_release_stays_idle`.

- [ ] **Step 2: Run tests to verify they fail to compile** — `cargo test --lib update` — expected: compile errors (new shapes don't exist yet).

- [ ] **Step 3: Implement the new types, `select_sums`, and the transition table** in `src/update.rs`. `select_sums` mirrors `select_asset`:

```rust
pub fn select_sums(assets: &[Asset]) -> Option<&Asset> {
    assets.iter().find(|a| a.name == "SHA256SUMS.txt")
}
```

  Delete `CAN_APPLY`. In the `ReleaseFetched` arm, build the offer with `zip: select_asset(&r.assets).cloned()`, `sums: select_sums(&r.assets).cloned()`.

- [ ] **Step 4: Fix the two `src/main.rs` call sites** so the app still compiles:
  - `main.rs:127-137` FOREMAN_UPDATE_TEST init: build `State::UpdateAvailable { offer: update::Offer { version: "v9.9.9".into(), html_url: update::RELEASES_URL.into(), zip: None, sums: None }, can_apply: false }` (Task 4 extends this to stages).
  - `main.rs:554-557` chip mapping: `update::State::UpdateAvailable { offer, .. } => Some(offer.version.clone())` (Task 4 replaces this mapping wholesale).
  - The worker's `Ok(_) => continue` arm in `spawn` still absorbs new effects; the fetch site changes to `Event::ReleaseFetched { info: r, writable: false }` for now (Task 3 adds the probe).

- [ ] **Step 5: Run the full test suite** — `cargo test` — expected: all green.

- [ ] **Step 6: Commit**

```bash
git add src/update.rs src/main.rs
git commit -m "feat(update): phase 4 state machine — download/verify/swap/restart transitions

Pure transitions only; effects are still dropped by the worker.
Two-stage restart arm with timeout, retryable vs terminal errors,
newer-release-while-busy ignored. CAN_APPLY const replaced by a
writability flag carried on the fetch event.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Verify/swap helpers (pure fs, `src/update.rs`) + new deps

All the file mechanics as free functions, each testable in a temp dir. This task adds the `sha2` and `zip` deps.

**Files:**
- Modify: `Cargo.toml` (two deps), `src/update.rs` (helpers + tests below the I/O-edge marker)

**Interfaces:**
- Produces (Task 3 worker and Task 5 startup consume):

```rust
pub fn probe_writable(dir: &std::path::Path) -> bool;
pub fn sha256_hex(path: &std::path::Path) -> std::io::Result<String>;         // lowercase hex
pub fn expected_hash(sums_text: &str, file_name: &str) -> Option<String>;     // parses "<hex>  <name>" lines
pub fn extract_exe(zip_path: &std::path::Path, dest: &std::path::Path) -> Result<(), String>; // pulls foreman.exe out of the zip
pub fn swap_exe(exe: &std::path::Path) -> Result<(), String>;                 // two-rename dance; expects <exe>.new beside it
pub fn sibling(p: &std::path::Path, suffix: &str) -> std::path::PathBuf;      // "foreman.exe" + ".old" -> "foreman.exe.old"
pub fn staging_dir() -> std::path::PathBuf;                                   // %TEMP%\foreman-update
pub fn cleanup_leftovers();                                                   // best-effort: <current_exe>.old + staging_dir()
```

- [ ] **Step 1: Add deps to `Cargo.toml`** (versions checked against crates.io at implementation time; `zip` major may differ — pin whatever `cargo add` resolves):

```toml
sha2 = "0.10"
zip = { version = "5", default-features = false, features = ["deflate"] }
```

- [ ] **Step 2: Write the failing tests** (in `update.rs`'s test module; use `std::env::temp_dir()` + unique-per-test subdirs seeded with `std::process::id()`, cleaned at test end — matches how `control.rs` tests build unique pipe names):

```rust
#[test]
fn sha256_hex_matches_known_vector() {
    let dir = test_dir("sha");                       // helper: temp_dir()/foreman-test-sha-<pid>, created fresh
    let f = dir.join("abc.txt");
    std::fs::write(&f, b"abc").unwrap();
    assert_eq!(
        sha256_hex(&f).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn expected_hash_finds_the_matching_line() {
    let sums = "aaaa  other.zip\nbbbb  foreman-v0.3.0-x86_64-windows.zip\n";
    assert_eq!(expected_hash(sums, "foreman-v0.3.0-x86_64-windows.zip"), Some("bbbb".into()));
    assert_eq!(expected_hash(sums, "missing.zip"), None);
    // tolerate single-space and CRLF variants
    assert_eq!(expected_hash("cccc foreman.zip\r\n", "foreman.zip"), Some("cccc".into()));
}

#[test]
fn extract_exe_pulls_the_exe_out_of_a_zip() {
    let dir = test_dir("zip");
    let zip_path = dir.join("rel.zip");
    // build a zip containing foreman.exe + a license, with the zip crate itself
    let f = std::fs::File::create(&zip_path).unwrap();
    let mut w = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default();
    use std::io::Write as _;
    w.start_file("foreman.exe", opts).unwrap();
    w.write_all(b"NEW-EXE-BYTES").unwrap();
    w.start_file("LICENSE", opts).unwrap();
    w.write_all(b"license").unwrap();
    w.finish().unwrap();
    let dest = dir.join("foreman.exe.new");
    extract_exe(&zip_path, &dest).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"NEW-EXE-BYTES");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn swap_replaces_exe_and_keeps_old() {
    let dir = test_dir("swap");
    let exe = dir.join("foreman.exe");
    std::fs::write(&exe, b"OLD").unwrap();
    std::fs::write(sibling(&exe, ".new"), b"NEW").unwrap();
    swap_exe(&exe).unwrap();
    assert_eq!(std::fs::read(&exe).unwrap(), b"NEW");
    assert_eq!(std::fs::read(sibling(&exe, ".old")).unwrap(), b"OLD");
    assert!(!sibling(&exe, ".new").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn swap_rolls_back_when_new_is_missing() {
    // rename 1 succeeds, rename 2 fails (no .new) -> .old must be renamed back
    let dir = test_dir("rollback");
    let exe = dir.join("foreman.exe");
    std::fs::write(&exe, b"OLD").unwrap();
    assert!(swap_exe(&exe).is_err());
    assert_eq!(std::fs::read(&exe).unwrap(), b"OLD", "exe must be restored");
    assert!(!sibling(&exe, ".old").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn probe_writable_true_in_temp_false_in_missing_dir() {
    assert!(probe_writable(&std::env::temp_dir()));
    assert!(!probe_writable(std::path::Path::new(r"C:\nonexistent-foreman-probe-dir")));
}

#[test]
fn sibling_appends_to_the_full_filename() {
    assert_eq!(
        sibling(std::path::Path::new(r"C:\x\foreman.exe"), ".old"),
        std::path::PathBuf::from(r"C:\x\foreman.exe.old")
    );
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test --lib update` — expected: compile errors (functions missing).

- [ ] **Step 4: Implement the helpers.** Notes that matter:
  - `sibling`: `let mut s = p.as_os_str().to_owned(); s.push(suffix); s.into()`.
  - `probe_writable`: create `dir.join(format!(".foreman-probe-{}", std::process::id()))`, write 1 byte, delete; any failure → `false`.
  - `sha256_hex`: stream in 64 KiB chunks through `sha2::Sha256` (never read the whole zip into memory), format with `{:02x}`.
  - `expected_hash`: per line, `split_whitespace()` → first token is the hex, last token is the name; compare name case-sensitively; return lowercase hex.
  - `extract_exe`: open with `zip::ZipArchive`, find the entry whose name ends with `foreman.exe` (release zips store it at the root), `std::io::copy` to `dest`, then `File::sync_all` (the spec's "fully written and flushed before any rename").
  - `swap_exe`: `rename(exe, exe.old)`; then `rename(exe.new, exe)` — on failure `rename(exe.old, exe)` (best effort) and return `Err`. Map both errors to strings with context.
  - `cleanup_leftovers`: `current_exe()` → remove `sibling(&exe, ".old")` and `remove_dir_all(staging_dir())`, both `let _ =`.

- [ ] **Step 5: Run the full suite** — `cargo test` — expected: green.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/update.rs
git commit -m "feat(update): verify/swap helpers — sha256, SHA256SUMS parse, zip extract, two-rename swap

New deps: sha2, zip (no default features, deflate only). Swap tested
in a temp dir including the rollback path; hash against the NIST abc
vector.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Worker executes the Phase-4 effects (`src/update.rs::spawn`)

The worker's `Ok(_) => continue` arm becomes real: download with progress, then verify+swap using Task 2 helpers. Pure decision logic is already tested; this task is I/O glue whose end-to-end proof is the live pilot (Task 7).

**Files:**
- Modify: `src/update.rs` (`spawn`, plus a `fetch_url` sibling for byte downloads)

**Interfaces:**
- Consumes: `Effect::Download { zip, sums }`, `Effect::VerifyAndSwap`, helpers from Task 2, `Event::{Progress, DownloadDone, HashBad, SwapOk, SwapFailed, FetchFailed}` from Task 1.
- Produces: no new public API. Worker-internal state: `let mut staged: Option<Staged>` where `struct Staged { zip_path: PathBuf, sums_path: PathBuf, zip_name: String }`.

- [ ] **Step 1: Implement `Effect::Download { zip, sums }` in the worker loop.**

```rust
Ok(Effect::Download { zip, sums }) => {
    let ev = match download_release(&zip, &sums, &event_tx, &ctx) {
        Ok(st) => { staged = Some(st); Event::DownloadDone }
        Err(e) => { eprintln!("update: download failed: {e}"); Event::FetchFailed }
    };
    if event_tx.send(ev).is_err() { break; }
    ctx.request_repaint();
    continue;
}
```

  `download_release` (private fn beside `fetch_url`):
  - `std::fs::create_dir_all(staging_dir())`, download `sums` first (tiny, no progress), then the zip.
  - Zip download: `ureq` GET with the same agent config as `fetch_url` but `timeout_global` raised to 300 s (a 15 MB zip on slow links; the 10 s global would kill it). Read `Content-Length` from the response headers; stream `resp.body_mut().as_reader()` in 64 KiB chunks to the staging file; after each chunk, if the whole-percent value changed, `event_tx.send(Event::Progress(done as f32 / total as f32))` + `ctx.request_repaint()`. Missing `Content-Length` → send no Progress events (chip just shows the downloading label; state machine is fine with that).
  - Returns `Staged { zip_path, sums_path, zip_name: zip.name.clone() }`.

- [ ] **Step 2: Implement `Effect::VerifyAndSwap`.**

```rust
Ok(Effect::VerifyAndSwap) => {
    let ev = match staged.take() {
        Some(st) => verify_and_swap(&st),
        None => Event::SwapFailed, // effect without a download: defensive
    };
    if event_tx.send(ev).is_err() { break; }
    ctx.request_repaint();
    continue;
}
```

  `verify_and_swap(st: &Staged) -> Event`:
  1. `sums_text = std::fs::read_to_string(&st.sums_path)`; `expected = expected_hash(&sums_text, &st.zip_name)`; `actual = sha256_hex(&st.zip_path)`. Any `Err`/`None` or `expected != actual` → `let _ = std::fs::remove_dir_all(staging_dir());` then `Event::HashBad`.
  2. `exe = std::env::current_exe()` (error → `SwapFailed`); `extract_exe(&st.zip_path, &sibling(&exe, ".new"))` (error → `SwapFailed`).
  3. `swap_exe(&exe)` → `Ok` ⇒ `let _ = std::fs::remove_dir_all(staging_dir());` + `Event::SwapOk`; `Err(e)` ⇒ log to stderr + `Event::SwapFailed`.

- [ ] **Step 3: Wire the writability probe into the fetch.** In the worker where `fetch_latest()` succeeds:

```rust
let ev = match fetch_latest() {
    Ok(r) => {
        let writable = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| probe_writable(d)))
            .unwrap_or(false);
        Event::ReleaseFetched { info: r, writable }
    }
    Err(e) => { eprintln!("update: check failed (will retry): {e}"); Event::FetchFailed }
};
```

  Remove the `Ok(_) => continue` catch-all arm — every effect now has a real arm except `SaveWorkspaceAndRestart`, which keeps a `continue` arm with a comment that App intercepts it before it ever reaches the worker (Task 5).

- [ ] **Step 4: Build and test** — `cargo build && cargo test` — expected: green (no new unit tests here; the pure seams were tested in Tasks 1–2).

- [ ] **Step 5: Commit**

```bash
git add src/update.rs
git commit -m "feat(update): worker downloads, verifies, and swaps

Download streams to %TEMP%\\foreman-update with whole-percent Progress
events; verify+swap runs the task-2 helpers; the writability probe
rides in on every fetch. SaveWorkspaceAndRestart is intercepted by App
and never reaches the worker.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Chip UI variants (`src/panel.rs`, `src/wm.rs`, `src/main.rs`)

The chip stops being a version string and becomes a small display enum; every Phase-4 state gets a label. FOREMAN_UPDATE_TEST grows stages so each variant can be screenshot-verified.

**Files:**
- Modify: `src/panel.rs:98-101` (`PanelModel.update` type), `src/panel.rs:286-317` (`paint_update_chip`), `src/panel.rs:319-355` (`paint_rail_update_glyph`), call sites at `panel.rs:172-180`, `panel.rs:431`, `panel.rs:713`, `panel.rs:821`
- Modify: `src/wm.rs:471` (`update_chip` field type), `src/wm.rs:1888` (model build), `src/wm.rs:1893` (`set_update_chip` signature)
- Modify: `src/main.rs:127-137` (FOREMAN_UPDATE_TEST stages), `src/main.rs:554-557` (state→chip mapping)

**Interfaces:**
- Produces (defined in `src/panel.rs`, used by wm + main):

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateChip {
    Notify { version: String },                      // Phase 3 look: click for release notes
    Apply { version: String },                       // click to update
    Downloading { version: String, progress: f32 },  // progress in 0..=1
    Restart { armed: bool },
    Failed { retryable: bool },
}
```

- Consumes: `update::State` (Task 1 shapes) for the mapping in main.

- [ ] **Step 1: Change the types.** `PanelModel.update: Option<UpdateChip>`; `wm.rs` field `update_chip: Option<UpdateChip>` and `pub fn set_update_chip(&mut self, v: Option<UpdateChip>)`; model build at `wm.rs:1888` clones it through unchanged.

- [ ] **Step 2: Map state → chip in `src/main.rs`** (replaces lines 554–557):

```rust
self.desktop.set_update_chip(match &self.update_state {
    update::State::UpdateAvailable { offer, can_apply: false } =>
        Some(panel::UpdateChip::Notify { version: offer.version.clone() }),
    update::State::UpdateAvailable { offer, can_apply: true } =>
        Some(panel::UpdateChip::Apply { version: offer.version.clone() }),
    update::State::Downloading { offer, progress } =>
        Some(panel::UpdateChip::Downloading { version: offer.version.clone(), progress: *progress }),
    update::State::ReadyToRestart { armed } =>
        Some(panel::UpdateChip::Restart { armed: *armed }),
    update::State::Error { retryable, .. } =>
        Some(panel::UpdateChip::Failed { retryable: *retryable }),
    update::State::Idle => None,
});
```

- [ ] **Step 3: Rewrite `paint_update_chip`** to take `chip: &UpdateChip`. Label and color per variant (`th = crate::theme::live(...)`; dim/hover behavior stays exactly as today for the non-warn variants):

| Variant | Text | Color |
|---|---|---|
| `Notify` | `↓ {version} — click for release notes` | `th.dim` / hover `th.snap_stroke` (unchanged) |
| `Apply` | `↓ {version} — click to update` | same as Notify |
| `Downloading` | `↓ {version} — {pct}%` (`pct = (progress * 100.0) as u32`) | same |
| `Restart { armed: false }` | `↻ Restart to update` | same |
| `Restart { armed: true }` | `Restart? {n} sessions close` | `th.danger`, hover `th.danger` |
| `Failed { retryable: true }` | `Update failed — retry` | `th.danger` |
| `Failed { retryable: false }` | `Update failed — release notes` | `th.danger` |

  Session count `n` is computed in the panel from its own model: `self.model.projects.iter().map(|p| p.tabs.len()).sum::<usize>()`. Singular form when `n == 1` (`session closes`). Click latch (`self.update_click = true`) is identical for every variant — the state machine decides meaning.

- [ ] **Step 4: Rewrite `paint_rail_update_glyph`** the same way: glyph `↓` for Notify/Apply/Downloading, `↻` for Restart, `!` for Failed; hover text = the same string the expanded chip shows; `th.danger` for armed/failed. No pulse animation — Progress events already repaint per percent, and a rail user gets the tooltip. (Deviation from the spec's "pulsing" cell: a steady glyph, chosen to avoid a per-frame animation loop for a rarely-visible state. Note it in the doc task.)

- [ ] **Step 5: Extend FOREMAN_UPDATE_TEST (debug-only) with stages** at `main.rs:127-137`. The env var's *value* picks the initial state; any unknown non-empty value behaves like `avail` (backward compatible with `=1`):

```rust
let update_state = if cfg!(debug_assertions) {
    match std::env::var("FOREMAN_UPDATE_TEST").ok().as_deref() {
        None | Some("") => update::State::Idle,
        Some("apply") => update::State::UpdateAvailable { offer: fake_offer(), can_apply: true },
        Some("down") => update::State::Downloading { offer: fake_offer(), progress: 0.43 },
        Some("ready") => update::State::ReadyToRestart { armed: false },
        Some("armed") => update::State::ReadyToRestart { armed: true },
        Some("err") => update::State::Error { offer: fake_offer(), retryable: true },
        Some("errswap") => update::State::Error { offer: fake_offer(), retryable: false },
        Some(_) => update::State::UpdateAvailable { offer: fake_offer(), can_apply: false },
    }
} else {
    update::State::Idle
};
```

  `fake_offer()` is a tiny local fn returning an `Offer` with version `v9.9.9`, `RELEASES_URL`, and `None` assets.

- [ ] **Step 6: Build, run `cargo test`, then screenshot-verify each stage.** Kill any running dev instance by exe path (CLAUDE.md), then for each stage in `apply down ready armed err errswap`: launch `$env:FOREMAN_UPDATE_TEST="<stage>"; cargo run` via the screenshot script in `docs/HANDOFF.md` § 3, screenshot, `Read` the PNG, confirm the chip text/color matches the table, close. Also screenshot one collapsed-rail stage (`armed`) to check the glyph + danger color.

- [ ] **Step 7: Commit**

```bash
git add src/panel.rs src/wm.rs src/main.rs
git commit -m "feat(panel): update chip variants for one-click apply

Notify/Apply/Downloading/Restart(armed)/Failed chip states with danger
coloring for armed-restart and failures; session count in the armed
label from the panel model; FOREMAN_UPDATE_TEST grows per-state stages
for screenshot verification.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Restart lifecycle (`src/main.rs`)

App intercepts `SaveWorkspaceAndRestart`, runs the arm timer, cleans up leftovers at startup, and the freshly-spawned child waits for its parent to die.

**Files:**
- Modify: `src/main.rs` — effect drains at `545-552` and `601-608`, App fields near `62-67`, startup in `main()` (before eframe runs, near the `update::spawn` gating at `996-1009`), plus the top of `fn main()` for the child-side wait.

**Interfaces:**
- Consumes: `Effect::SaveWorkspaceAndRestart` (Task 1), `cleanup_leftovers` (Task 2), existing `flush_workspace` (`main.rs:175`) and `force_quit` (`main.rs:93`).
- Produces: env contract `FOREMAN_WAIT_PID=<pid>` set on the restart-spawned child, removed from that child's own env before it spawns terminals.

- [ ] **Step 1: Factor the two effect-drain loops into one helper** on App (the duplicate at `601-608` currently repeats `545-552`):

```rust
/// Feed one event through the state machine and dispatch its effects.
/// SaveWorkspaceAndRestart is App's own (needs the live tree + viewport);
/// everything else goes to the worker.
fn drive_update(&mut self, ev: update::Event, ctx: &egui::Context) {
    let state = std::mem::replace(&mut self.update_state, update::State::Idle);
    let (state, effects) = update::step(state, ev, env!("CARGO_PKG_VERSION"));
    self.update_state = state;
    for fx in effects {
        match fx {
            update::Effect::SaveWorkspaceAndRestart => self.restart_for_update(ctx),
            other => { let _ = self.update_fx.send(other); }
        }
    }
}
```

  Both call sites become `self.drive_update(ev, &ctx)` / `self.drive_update(update::Event::ClickChip, &ctx)`.

- [ ] **Step 2: Implement `restart_for_update`** (mirrors the quit-confirmed path at `main.rs:654-658`):

```rust
fn restart_for_update(&mut self, ctx: &egui::Context) {
    self.flush_workspace();
    let Ok(exe) = std::env::current_exe() else { return };
    match std::process::Command::new(&exe)
        .env("FOREMAN_WAIT_PID", std::process::id().to_string())
        .spawn()
    {
        Ok(_) => {
            self.force_quit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        Err(e) => {
            self.notify.push(
                notify::Level::Error,
                format!("update restart failed: {e}"),
                std::time::Instant::now(),
            );
        }
    }
}
```

  (`current_exe()` on disk is the new image after the swap; the running process keeps its old image. Spawn failure leaves the app running — the swap already happened, so a manual restart still updates.)

- [ ] **Step 3: Arm timer.** New App field `arm_deadline: Option<std::time::Instant>` (init `None`). Each frame, immediately after the `update_rx` drain:

```rust
match (&self.update_state, self.arm_deadline) {
    (update::State::ReadyToRestart { armed: true }, None) => {
        let d = std::time::Instant::now() + std::time::Duration::from_secs(5);
        self.arm_deadline = Some(d);
        ctx.request_repaint_after(std::time::Duration::from_secs(5));
    }
    (update::State::ReadyToRestart { armed: true }, Some(d)) if std::time::Instant::now() >= d => {
        self.arm_deadline = None;
        self.drive_update(update::Event::ArmTimeout, &ctx);
    }
    (update::State::ReadyToRestart { armed: true }, Some(_)) => {}
    _ => self.arm_deadline = None,
}
```

- [ ] **Step 4: Child-side wait + startup cleanup** at the very top of `fn main()` (before eframe/native options are built):

```rust
// Update-restart handshake: the old instance spawned us with its pid.
// Wait (bounded) for it to fully exit so we don't race its control pipe
// or paint two desktops at once, then keep the var out of terminals' env.
if let Some(pid) = std::env::var("FOREMAN_WAIT_PID").ok().and_then(|s| s.parse::<u32>().ok()) {
    unsafe { std::env::remove_var("FOREMAN_WAIT_PID") };
    wait_for_exit(pid, std::time::Duration::from_secs(10));
}
update::cleanup_leftovers();
```

  `wait_for_exit` uses the already-enabled windows-sys features (`Win32_System_Threading` + `Win32_Foundation` are in Cargo.toml):

```rust
/// Block until `pid` exits or `timeout` passes. Best effort: open failure
/// (already gone, or access denied) returns immediately.
fn wait_for_exit(pid: u32, timeout: std::time::Duration) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, SYNCHRONIZE};
    unsafe {
        let h = OpenProcess(SYNCHRONIZE, 0, pid);
        if h.is_null() { return; }
        WaitForSingleObject(h, timeout.as_millis() as u32);
        CloseHandle(h);
    }
}
```

  (`cleanup_leftovers` runs unconditionally — debug builds too — so a dev instance beside a previously-updated install still tidies `.old`. It is `let _ =` best-effort throughout; AV holding `.old` is retried at the next launch by construction.)

- [ ] **Step 5: Build + full test suite** — `cargo test` — expected: green.

- [ ] **Step 6: Manual smoke of the armed flow** (no real release needed): `$env:FOREMAN_UPDATE_TEST="ready"; cargo run` — click the chip once (arms, warn label), wait 6 s (disarms), click twice (App calls `restart_for_update`: workspace flushes, a second foreman appears, the first closes). Debug builds run the same restart path; the child was built from the same exe so this exercises spawn + wait + cleanup end-to-end. Screenshot evidence of the armed label before/after timeout.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat(update): restart lifecycle — workspace flush, respawn, parent wait, leftover cleanup

App intercepts SaveWorkspaceAndRestart (flush + spawn new exe with
FOREMAN_WAIT_PID + close); a 5s arm timer disarms the restart chip;
startup waits for the parent pid, then deletes foreman.exe.old and the
staging dir.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Control-pipe creation retry (`src/control.rs`)

One-shot pipe creation becomes a short retry loop so the restarted instance wins the pipe even if the old process lingers past the PID wait.

**Files:**
- Modify: `src/control.rs:239-247` (`serve`), test module at the bottom

**Interfaces:**
- Produces: `fn listen_retry(name: interprocess::local_socket::Name, attempts: u32, delay: std::time::Duration) -> Option<Listener>` (private; `serve` calls it with `attempts: 8, delay: 250ms`).

- [ ] **Step 1: Write the failing test** (same pattern as `close_pipe_roundtrip` at `control.rs:1100` — unique per-test pipe name):

```rust
#[test]
fn listen_retry_wins_the_pipe_after_the_holder_exits() {
    let pipe = format!("foreman-test-retry-{}", std::process::id());
    let name = pipe.clone().to_ns_name::<GenericNamespaced>().unwrap();
    let holder = ListenerOptions::new().name(name).create_sync().unwrap();
    let p2 = pipe.clone();
    let t = std::thread::spawn(move || {
        let name = p2.to_ns_name::<GenericNamespaced>().unwrap();
        listen_retry(name, 20, std::time::Duration::from_millis(50)).is_some()
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(holder);
    assert!(t.join().unwrap(), "retry must acquire the pipe once the holder is gone");
}

#[test]
fn listen_retry_gives_up_when_the_pipe_stays_held() {
    let pipe = format!("foreman-test-retry-held-{}", std::process::id());
    let name = pipe.clone().to_ns_name::<GenericNamespaced>().unwrap();
    let _holder = ListenerOptions::new().name(name).create_sync().unwrap();
    let name2 = pipe.to_ns_name::<GenericNamespaced>().unwrap();
    assert!(listen_retry(name2, 3, std::time::Duration::from_millis(20)).is_none());
}
```

  Note: `to_ns_name` takes ownership on `String`/borrows on `&str` — match whatever the existing test at `control.rs:1100` does; `Name` is not `Clone`, hence building it twice from the pipe string.

- [ ] **Step 2: Run to verify failure** — `cargo test --lib control::tests::listen_retry` — expected: compile error (`listen_retry` missing).

- [ ] **Step 3: Implement** and use in `serve`:

```rust
/// Create the pipe listener, retrying briefly: after an update-restart the
/// old instance can hold the pipe a beat past its window closing, and two
/// instances launched fast race it. First success wins; None = give up
/// (dispatch disabled), same behavior as the old one-shot failure.
fn listen_retry(
    name: interprocess::local_socket::Name<'_>,
    attempts: u32,
    delay: std::time::Duration,
) -> Option<interprocess::local_socket::Listener> {
    for i in 0..attempts {
        match ListenerOptions::new().name(name.clone()).create_sync() {
            Ok(l) => return Some(l),
            Err(e) if i + 1 == attempts => {
                eprintln!("control: pipe unavailable after {attempts} attempts ({e}); agent dispatch disabled");
            }
            Err(_) => std::thread::sleep(delay),
        }
    }
    None
}
```

  If `Name` turns out not to implement `Clone`, take the pipe `&str` instead and build the name per attempt — decide at implementation time; the test compiles either way since it passes what `serve` passes. `serve` becomes:

```rust
let Some(listener) = listen_retry(name, 8, std::time::Duration::from_millis(250)) else {
    return;
};
```

  (8 × 250 ms ≈ the "~2 s" in the spec.)

- [ ] **Step 4: Run tests** — `cargo test --lib control` — expected: green (including the two new ones).

- [ ] **Step 5: Commit**

```bash
git add src/control.rs
git commit -m "feat(control): retry pipe creation for ~2s at startup

Covers the update-restart window where the old instance still holds
\\\\.\\pipe\\foreman, and the two-instances-launched-fast race. Failure
after the retries degrades exactly like before: dispatch disabled.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Docs + release acceptance

**Files:**
- Modify: `docs/installing-and-updating.md` (Phase 4 section, gotchas, key files)
- Modify: `docs/superpowers/specs/2026-08-19-one-click-update-phase-4-design.md` (status header)

- [ ] **Step 1: Update `docs/installing-and-updating.md`:**
  - Replace the "One-click apply (Phase 4): not built yet" bullet with a "One-click apply (Phase 4, current)" section: chip click downloads + verifies + swaps, armed two-click restart (5 s disarm), staged-but-never-restarted is fine, failure behaviors (retryable vs release-page fallback).
  - Gotchas additions: `FOREMAN_UPDATE_TEST` now takes stages (`apply`/`down`/`ready`/`armed`/`err`/`errswap`); `FOREMAN_WAIT_PID` is internal to the restart handshake — never set it by hand; the rail glyph is steady (no pulse — deliberate deviation from the spec table); swap updates the exe only, install.ps1 owns licenses/layout.
  - Key files: add `src/control.rs` (pipe retry) and note `src/update.rs` now contains the full download/verify/swap worker.

- [ ] **Step 2: Flip the spec's Status line** to `implemented (vX.Y.Z pilot pending)`.

- [ ] **Step 3: Commit**

```bash
git add docs/installing-and-updating.md docs/superpowers/specs/2026-08-19-one-click-update-phase-4-design.md
git commit -m "docs(update): phase 4 one-click apply — behavior, env stages, gotchas

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

- [ ] **Step 4: Live acceptance (user-driven, after merge to main):** bump `Cargo.toml` to `0.3.0`, tag `v0.3.0`, let CI release. Then on the installed copy (`%LOCALAPPDATA%\Programs\foreman`), cut a trivial `v0.3.1` and one-click apply it: chip → download % → restart-armed → confirm → new version running, workspace restored, `foreman.exe.old` gone on the following launch, `foreman status` works (pipe acquired). This is the spec's end-to-end acceptance; nothing in-repo can substitute for it.

---

## Self-review notes (already applied)

- Spec coverage: every spec section maps to a task — state machine (1), download/verify/swap (2–3), restart + pipe race + cleanup (5–6), chip UI + gating (4), error table (1–3), testing (each task) and live acceptance (7). The rail "pulsing" cell is deliberately downgraded to a steady glyph + tooltip; recorded in the doc task.
- Type consistency: `Offer`/`UpdateChip`/`listen_retry`/helper signatures are stated once in Interfaces blocks and reused verbatim in later tasks.
- Known implementation-time flex points (called out in their tasks, not placeholders): the `zip` crate major version, and whether `interprocess::Name` is `Clone`.
