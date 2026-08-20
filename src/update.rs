//! In-app update: pure state machine + decisions. The worker thread (Task 5)
//! executes Effects and reports Events; nothing in this file does I/O.
//! Spec: docs/superpowers/specs/2026-07-14-install-and-update-design.md section 3.

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
pub struct Offer {
    pub version: String,
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

// Strict X.Y.Z (optional leading v). Anything else - prereleases,
// two-part versions - is None, which the caller treats as "no update"
// (spec: upgrades only, silent-skip on weirdness).
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

// Suffix selection over the release's asset list - consumers never
// reconstruct full filenames (spec 1, asset naming convention).
pub fn select_asset(assets: &[Asset]) -> Option<&Asset> {
    assets.iter().find(|a| a.name.ends_with(ZIP_SUFFIX))
}

// SHA256SUMS.txt sits beside the zip in every release; the name is fixed by
// the release-build script, never derived from the version (spec section 1).
pub fn select_sums(assets: &[Asset]) -> Option<&Asset> {
    assets.iter().find(|a| a.name == "SHA256SUMS.txt")
}

pub fn step(state: State, ev: Event, current: &str) -> (State, Vec<Effect>) {
    use Effect as X;
    use Event as E;
    use State as S;
    match (state, ev) {
        (S::Idle, E::ReleaseFetched { info, writable })
        | (S::UpdateAvailable { .. }, E::ReleaseFetched { info, writable })
        | (S::Error { .. }, E::ReleaseFetched { info, writable }) => {
            match (parse_version(current), parse_version(&info.tag_name)) {
                (Some(cur), Some(new)) if new > cur => {
                    let zip = select_asset(&info.assets).cloned();
                    let sums = select_sums(&info.assets).cloned();
                    let can_apply = writable && zip.is_some() && sums.is_some();
                    (
                        S::UpdateAvailable {
                            offer: Offer {
                                version: info.tag_name,
                                html_url: info.html_url,
                                zip,
                                sums,
                            },
                            can_apply,
                        },
                        vec![],
                    )
                }
                _ => (S::Idle, vec![]),
            }
        }
        // Busy states ignore a fresh fetch outright (spec: don't yank the rug
        // out from under an in-flight download or an armed restart).
        (s @ (S::Downloading { .. } | S::ReadyToRestart { .. }), E::ReleaseFetched { .. }) => {
            (s, vec![])
        }
        (
            S::UpdateAvailable {
                offer,
                can_apply: false,
            },
            E::ClickChip,
        ) => {
            let url = offer.html_url.clone();
            (
                S::UpdateAvailable {
                    offer,
                    can_apply: false,
                },
                vec![X::OpenReleasesPage(url)],
            )
        }
        (
            S::UpdateAvailable {
                offer,
                can_apply: true,
            },
            E::ClickChip,
        ) => match (offer.zip.clone(), offer.sums.clone()) {
            (Some(zip), Some(sums)) => (
                S::Downloading {
                    offer,
                    progress: 0.0,
                },
                vec![X::Download { zip, sums }],
            ),
            // can_apply promises both assets; anything else is defensive —
            // never unwrap here, fall back to the manual-download page.
            _ => {
                let url = offer.html_url.clone();
                (
                    S::UpdateAvailable {
                        offer,
                        can_apply: true,
                    },
                    vec![X::OpenReleasesPage(url)],
                )
            }
        },
        (S::Downloading { offer, .. }, E::Progress(p)) => {
            (S::Downloading { offer, progress: p }, vec![])
        }
        (S::Downloading { offer, .. }, E::FetchFailed) => (
            S::Error {
                offer,
                retryable: true,
            },
            vec![],
        ),
        (S::Downloading { offer, .. }, E::DownloadDone) => (
            S::Downloading {
                offer,
                progress: 1.0,
            },
            vec![X::VerifyAndSwap],
        ),
        (S::Downloading { offer, .. }, E::HashBad) => (
            S::Error {
                offer,
                retryable: true,
            },
            vec![],
        ),
        (S::Downloading { offer, .. }, E::SwapFailed) => (
            S::Error {
                offer,
                retryable: false,
            },
            vec![],
        ),
        (S::Downloading { .. }, E::SwapOk) => (S::ReadyToRestart { armed: false }, vec![]),
        (S::ReadyToRestart { armed: false }, E::ClickChip) => {
            (S::ReadyToRestart { armed: true }, vec![])
        }
        (S::ReadyToRestart { armed: true }, E::ClickChip) => (
            S::ReadyToRestart { armed: true },
            vec![X::SaveWorkspaceAndRestart],
        ),
        (S::ReadyToRestart { armed: true }, E::ArmTimeout) => {
            (S::ReadyToRestart { armed: false }, vec![])
        }
        (
            S::Error {
                offer,
                retryable: true,
            },
            E::ClickChip,
        ) => match (offer.zip.clone(), offer.sums.clone()) {
            (Some(zip), Some(sums)) => (
                S::Downloading {
                    offer,
                    progress: 0.0,
                },
                vec![X::Download { zip, sums }],
            ),
            _ => {
                let url = offer.html_url.clone();
                (
                    S::Error {
                        offer,
                        retryable: true,
                    },
                    vec![X::OpenReleasesPage(url)],
                )
            }
        },
        (
            S::Error {
                offer,
                retryable: false,
            },
            E::ClickChip,
        ) => {
            let url = offer.html_url.clone();
            (
                S::Error {
                    offer,
                    retryable: false,
                },
                vec![X::OpenReleasesPage(url)],
            )
        }
        (s, _) => (s, vec![]),
    }
}

// I/O edge: everything below runs on the worker thread

const API_URL: &str = "https://api.github.com/repos/sniffle6/foreman/releases/latest";
const FIRST_CHECK: std::time::Duration = std::time::Duration::from_secs(10);
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

fn fetch_url(url: &str) -> Result<ReleaseInfo, String> {
    let ua = concat!(
        "foreman/",
        env!("CARGO_PKG_VERSION"),
        " (+https://github.com/sniffle6/foreman)"
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("User-Agent", ua)
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

fn fetch_latest() -> Result<ReleaseInfo, String> {
    fetch_url(API_URL)
}

// %TEMP%\foreman-update -- scratch space for the downloaded zip + sums,
// wiped after a successful swap and best-effort on startup (cleanup_leftovers).
pub fn staging_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("foreman-update")
}

// "foreman.exe" + ".old" -> "foreman.exe.old"; appends to the full filename,
// not the extension, so it survives paths without one.
pub fn sibling(p: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

// Cheap write+delete probe -- the only reliable way to know if the install
// dir is writable without an update-and-see (spec: Program Files needs UAC,
// a per-user install doesn't).
pub fn probe_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!(".foreman-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

pub fn sha256_hex(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

// Parses "<hex>  <name>" lines (SHA256SUMS.txt: sha256sum's two-space format,
// but tolerate one space and CRLF). Name is the last token so paths with
// spaces in the hex slot never happen -- the hex is always token 0.
pub fn expected_hash(sums_text: &str, file_name: &str) -> Option<String> {
    for line in sums_text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 2 {
            continue;
        }
        if tokens[tokens.len() - 1] == file_name {
            return Some(tokens[0].to_lowercase());
        }
    }
    None
}

// Release zips store foreman.exe at the root; match by suffix in case a
// future release nests it (spec doesn't guarantee root, just presence).
pub fn extract_exe(zip_path: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let f = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let mut idx = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.name().ends_with("foreman.exe") {
            idx = Some(i);
            break;
        }
    }
    let idx = idx.ok_or_else(|| "foreman.exe not found in release zip".to_string())?;
    let mut entry = archive.by_index(idx).map_err(|e| e.to_string())?;
    let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    out.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

// Two-rename dance: exe -> exe.old, then exe.new -> exe. A running exe can be
// renamed on Windows (just not deleted/overwritten in place), so this swap
// works even while the old binary is still executing this code. On the
// second rename's failure, best-effort roll the first one back so the app
// is never left without an exe at its expected path.
pub fn swap_exe(exe: &std::path::Path) -> Result<(), String> {
    let old = sibling(exe, ".old");
    let new = sibling(exe, ".new");
    std::fs::rename(exe, &old)
        .map_err(|e| format!("rename {} -> {}: {e}", exe.display(), old.display()))?;
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!(
            "rename {} -> {}: {e}",
            new.display(),
            exe.display()
        ));
    }
    Ok(())
}

// Startup best-effort: a prior run's leftover .old (never cleaned because the
// process using it had already restarted) and any abandoned staging dir.
pub fn cleanup_leftovers() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(sibling(&exe, ".old"));
    }
    let _ = std::fs::remove_dir_all(staging_dir());
}

struct Staged {
    zip_path: std::path::PathBuf,
    sums_path: std::path::PathBuf,
    zip_name: String,
}

// Byte-download sibling of fetch_url: same agent/UA shape but streams the
// body to a file instead of parsing JSON. `progress` is None for the tiny
// sums file; for the zip it carries the channel to report whole-percent
// Progress events on. Missing Content-Length means no progress events at
// all -- the chip just shows the downloading label (spec: best effort).
fn download_to_file(
    url: &str,
    dest: &std::path::Path,
    progress: Option<(&std::sync::mpsc::Sender<Event>, &eframe::egui::Context)>,
) -> Result<(), String> {
    use std::io::{Read, Write};
    let ua = concat!(
        "foreman/",
        env!("CARGO_PKG_VERSION"),
        " (+https://github.com/sniffle6/foreman)"
    );
    // A 15 MB zip on a slow link can take a while; the 10s global timeout
    // fetch_url uses for the tiny release-info call would kill it.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("User-Agent", ua)
        .call()
        .map_err(|e| e.to_string())?;
    let total = resp.body().content_length();
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut reader = resp.body_mut().as_reader();
    let mut buf = [0u8; 65536];
    let mut done: u64 = 0;
    let mut last_pct: u64 = u64::MAX;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if let (Some((event_tx, ctx)), Some(total)) = (progress, total) {
            if total > 0 {
                let pct = done * 100 / total;
                if pct != last_pct {
                    last_pct = pct;
                    let _ = event_tx.send(Event::Progress(done as f32 / total as f32));
                    ctx.request_repaint();
                }
            }
        }
    }
    Ok(())
}

fn download_release(
    zip: &Asset,
    sums: &Asset,
    event_tx: &std::sync::mpsc::Sender<Event>,
    ctx: &eframe::egui::Context,
) -> Result<Staged, String> {
    let dir = staging_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let sums_path = dir.join("SHA256SUMS.txt");
    download_to_file(&sums.browser_download_url, &sums_path, None)?;
    let zip_path = dir.join(&zip.name);
    download_to_file(&zip.browser_download_url, &zip_path, Some((event_tx, ctx)))?;
    Ok(Staged {
        zip_path,
        sums_path,
        zip_name: zip.name.clone(),
    })
}

fn verify_and_swap(st: &Staged) -> Event {
    let sums_text = match std::fs::read_to_string(&st.sums_path) {
        Ok(t) => t,
        Err(_) => {
            let _ = std::fs::remove_dir_all(staging_dir());
            return Event::HashBad;
        }
    };
    let expected = expected_hash(&sums_text, &st.zip_name);
    let actual = sha256_hex(&st.zip_path).ok();
    if expected.is_none() || expected != actual {
        let _ = std::fs::remove_dir_all(staging_dir());
        return Event::HashBad;
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return Event::SwapFailed,
    };
    if let Err(e) = extract_exe(&st.zip_path, &sibling(&exe, ".new")) {
        eprintln!("update: extract failed: {e}");
        return Event::SwapFailed;
    }
    match swap_exe(&exe) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(staging_dir());
            Event::SwapOk
        }
        Err(e) => {
            eprintln!("update: swap failed: {e}");
            Event::SwapFailed
        }
    }
}

/// Worker thread: executes Effects, reports Events (channel + repaint --
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
        let mut staged: Option<Staged> = None;
        loop {
            let wait = next_check.saturating_duration_since(std::time::Instant::now());
            match effect_rx.recv_timeout(wait) {
                Ok(Effect::FetchLatest) => {}
                Ok(Effect::OpenReleasesPage(url)) => {
                    ctx.open_url(eframe::egui::OpenUrl::new_tab(url));
                    ctx.request_repaint();
                    continue;
                }
                Ok(Effect::Download { zip, sums }) => {
                    let ev = match download_release(&zip, &sums, &event_tx, &ctx) {
                        Ok(st) => {
                            staged = Some(st);
                            Event::DownloadDone
                        }
                        Err(e) => {
                            eprintln!("update: download failed: {e}");
                            Event::FetchFailed
                        }
                    };
                    if event_tx.send(ev).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    continue;
                }
                Ok(Effect::VerifyAndSwap) => {
                    let ev = match staged.take() {
                        Some(st) => verify_and_swap(&st),
                        None => Event::SwapFailed, // effect without a download: defensive
                    };
                    if event_tx.send(ev).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    continue;
                }
                // App intercepts the click before it ever reaches the worker
                // (it saves the workspace and restarts the process itself).
                Ok(Effect::SaveWorkspaceAndRestart) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let ev = match fetch_latest() {
                Ok(r) => {
                    let writable = std::env::current_exe()
                        .ok()
                        .and_then(|e| e.parent().map(probe_writable))
                        .unwrap_or(false);
                    Event::ReleaseFetched { info: r, writable }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, assets: &[&str]) -> ReleaseInfo {
        ReleaseInfo {
            tag_name: tag.into(),
            html_url: "https://github.com/sniffle6/foreman/releases/tag/TEST".into(),
            assets: assets
                .iter()
                .map(|n| Asset {
                    name: (*n).into(),
                    browser_download_url: format!("https://x/{n}"),
                })
                .collect(),
        }
    }

    // parse_version
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

    // select_asset
    #[test]
    fn selects_zip_by_suffix_never_by_full_name() {
        let r = rel(
            "v0.2.1",
            &["SHA256SUMS.txt", "foreman-v0.2.1-x86_64-windows.zip"],
        );
        assert_eq!(
            select_asset(&r.assets).unwrap().name,
            "foreman-v0.2.1-x86_64-windows.zip"
        );
        let renamed = rel("v0.2.1", &["totally-different-x86_64-windows.zip"]);
        assert!(select_asset(&renamed.assets).is_some());
        let none = rel("v0.2.1", &["SHA256SUMS.txt", "foreman-setup.exe"]);
        assert!(select_asset(&none.assets).is_none());
    }

    fn offer(zip: bool, sums: bool) -> Offer {
        Offer {
            version: "v0.3.0".into(),
            html_url: "https://gh/rel".into(),
            zip: zip.then(|| Asset {
                name: "foreman-v0.3.0-x86_64-windows.zip".into(),
                browser_download_url: "https://x/z".into(),
            }),
            sums: sums.then(|| Asset {
                name: "SHA256SUMS.txt".into(),
                browser_download_url: "https://x/s".into(),
            }),
        }
    }

    // step: fetch outcomes
    #[test]
    fn newer_release_shows_chip_with_can_apply_false() {
        let (s, fx) = step(
            State::Idle,
            Event::ReleaseFetched {
                info: rel("v0.2.1", &[]),
                writable: false,
            },
            "0.2.0",
        );
        assert_eq!(
            s,
            State::UpdateAvailable {
                offer: Offer {
                    version: "v0.2.1".into(),
                    html_url: "https://github.com/sniffle6/foreman/releases/tag/TEST".into(),
                    zip: None,
                    sums: None,
                },
                can_apply: false,
            }
        );
        assert!(fx.is_empty());
    }

    #[test]
    fn writable_fetch_with_both_assets_offers_apply() {
        let info = rel(
            "v0.3.0",
            &["SHA256SUMS.txt", "foreman-v0.3.0-x86_64-windows.zip"],
        );
        let (s, _) = step(
            State::Idle,
            Event::ReleaseFetched {
                info,
                writable: true,
            },
            "0.2.10",
        );
        assert!(matches!(
            s,
            State::UpdateAvailable {
                can_apply: true,
                ..
            }
        ));
    }

    #[test]
    fn unwritable_or_missing_assets_fall_back_to_notify() {
        let full = rel(
            "v0.3.0",
            &["SHA256SUMS.txt", "foreman-v0.3.0-x86_64-windows.zip"],
        );
        let (s, _) = step(
            State::Idle,
            Event::ReleaseFetched {
                info: full.clone(),
                writable: false,
            },
            "0.2.10",
        );
        assert!(matches!(
            s,
            State::UpdateAvailable {
                can_apply: false,
                ..
            }
        ));
        let no_sums = rel("v0.3.0", &["foreman-v0.3.0-x86_64-windows.zip"]);
        let (s, _) = step(
            State::Idle,
            Event::ReleaseFetched {
                info: no_sums,
                writable: true,
            },
            "0.2.10",
        );
        assert!(matches!(
            s,
            State::UpdateAvailable {
                can_apply: false,
                ..
            }
        ));
    }

    #[test]
    fn equal_older_or_unparseable_release_stays_idle() {
        for tag in ["v0.2.0", "v0.1.9", "v0.2.0-rc1", "junk"] {
            let (s, fx) = step(
                State::Idle,
                Event::ReleaseFetched {
                    info: rel(tag, &[]),
                    writable: true,
                },
                "0.2.0",
            );
            assert_eq!(s, State::Idle, "tag {tag} must not offer an update");
            assert!(fx.is_empty());
        }
    }

    #[test]
    fn refetch_replaces_an_existing_offer() {
        let showing = State::UpdateAvailable {
            offer: Offer {
                version: "v0.2.1".into(),
                html_url: "u".into(),
                zip: None,
                sums: None,
            },
            can_apply: false,
        };
        let (s, _) = step(
            showing,
            Event::ReleaseFetched {
                info: rel("v0.3.0", &[]),
                writable: true,
            },
            "0.2.0",
        );
        match s {
            State::UpdateAvailable { offer, .. } => assert_eq!(offer.version, "v0.3.0"),
            other => panic!("expected UpdateAvailable, got {other:?}"),
        }
    }

    #[test]
    fn fetch_failure_is_silent_skip_in_any_state() {
        let showing = State::UpdateAvailable {
            offer: Offer {
                version: "v0.2.1".into(),
                html_url: "u".into(),
                zip: None,
                sums: None,
            },
            can_apply: false,
        };
        let (s, fx) = step(showing.clone(), Event::FetchFailed, "0.2.0");
        assert_eq!(s, showing);
        assert!(fx.is_empty());
        let (s, fx) = step(State::Idle, Event::FetchFailed, "0.2.0");
        assert_eq!(s, State::Idle);
        assert!(fx.is_empty());
    }

    // step: chip click
    #[test]
    fn click_without_can_apply_opens_releases_page() {
        let showing = State::UpdateAvailable {
            offer: Offer {
                version: "v0.2.1".into(),
                html_url: "https://gh/rel".into(),
                zip: None,
                sums: None,
            },
            can_apply: false,
        };
        let (s, fx) = step(showing.clone(), Event::ClickChip, "0.2.0");
        assert_eq!(s, showing);
        assert_eq!(fx, vec![Effect::OpenReleasesPage("https://gh/rel".into())]);
    }

    #[test]
    fn apply_click_starts_download() {
        let s = State::UpdateAvailable {
            offer: offer(true, true),
            can_apply: true,
        };
        let (s, fx) = step(s, Event::ClickChip, "0.2.10");
        assert!(matches!(s, State::Downloading { progress, .. } if progress == 0.0));
        assert!(matches!(fx.as_slice(), [Effect::Download { .. }]));
    }

    #[test]
    fn can_apply_true_with_missing_asset_defensively_opens_releases_page() {
        // can_apply is supposed to guarantee both assets; if it's ever wrong,
        // step must never unwrap -- it falls back to the manual page instead.
        let s = State::UpdateAvailable {
            offer: offer(false, true),
            can_apply: true,
        };
        let (s2, fx) = step(s.clone(), Event::ClickChip, "0.2.10");
        assert_eq!(s2, s);
        assert_eq!(fx, vec![Effect::OpenReleasesPage("https://gh/rel".into())]);
    }

    #[test]
    fn download_completes_then_verifies_then_restart_offer() {
        let s = State::Downloading {
            offer: offer(true, true),
            progress: 0.0,
        };
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
        for (ev, retryable) in [
            (Event::HashBad, true),
            (Event::FetchFailed, true),
            (Event::SwapFailed, false),
        ] {
            let s = State::Downloading {
                offer: offer(true, true),
                progress: 0.5,
            };
            let (s, _) = step(s, ev, "0.2.10");
            assert_eq!(
                s,
                State::Error {
                    offer: offer(true, true),
                    retryable
                }
            );
        }
    }

    #[test]
    fn retryable_error_click_redownloads_nonretryable_opens_page() {
        let s = State::Error {
            offer: offer(true, true),
            retryable: true,
        };
        let (s, fx) = step(s, Event::ClickChip, "0.2.10");
        assert!(matches!(s, State::Downloading { .. }));
        assert!(matches!(fx.as_slice(), [Effect::Download { .. }]));
        let s = State::Error {
            offer: offer(true, true),
            retryable: false,
        };
        let (_, fx) = step(s, Event::ClickChip, "0.2.10");
        assert_eq!(fx, vec![Effect::OpenReleasesPage("https://gh/rel".into())]);
    }

    #[test]
    fn restart_requires_arm_then_confirm_and_timeout_disarms() {
        let (s, fx) = step(
            State::ReadyToRestart { armed: false },
            Event::ClickChip,
            "0.2.10",
        );
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
        let info = rel(
            "v9.9.9",
            &["SHA256SUMS.txt", "foreman-v9.9.9-x86_64-windows.zip"],
        );
        for s in [
            State::Downloading {
                offer: offer(true, true),
                progress: 0.5,
            },
            State::ReadyToRestart { armed: false },
        ] {
            let (s2, fx) = step(
                s.clone(),
                Event::ReleaseFetched {
                    info: info.clone(),
                    writable: true,
                },
                "0.2.10",
            );
            assert_eq!(s2, s);
            assert!(fx.is_empty());
        }
    }

    #[test]
    fn error_state_accepts_a_fresh_offer() {
        let info = rel(
            "v0.4.0",
            &["SHA256SUMS.txt", "foreman-v0.4.0-x86_64-windows.zip"],
        );
        let s = State::Error {
            offer: offer(true, true),
            retryable: true,
        };
        let (s, _) = step(
            s,
            Event::ReleaseFetched {
                info,
                writable: true,
            },
            "0.2.10",
        );
        assert!(matches!(s, State::UpdateAvailable { offer, .. } if offer.version == "v0.4.0"));
    }

    #[test]
    fn selects_sums_by_exact_name() {
        let r = rel(
            "v0.3.0",
            &["SHA256SUMS.txt", "foreman-v0.3.0-x86_64-windows.zip"],
        );
        assert_eq!(select_sums(&r.assets).unwrap().name, "SHA256SUMS.txt");
        let r = rel("v0.3.0", &["foreman-v0.3.0-x86_64-windows.zip"]);
        assert!(select_sums(&r.assets).is_none());
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

    // GitHub JSON parsing (fixture from the real API shape)
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
        assert_eq!(
            select_asset(&r.assets).unwrap().browser_download_url,
            "https://x/z"
        );
    }

    #[test]
    fn fetch_error_maps_to_fetch_failed_event() {
        // fetch_latest against an unroutable host must produce Err, which the
        // worker maps to Event::FetchFailed (never a panic).
        let err = fetch_url("http://127.0.0.1:9/releases/latest");
        assert!(err.is_err());
    }

    // verify/swap helpers — each test gets a fresh temp_dir()/foreman-test-<label>-<pid>,
    // mirroring control.rs's unique-per-test pipe names.
    fn test_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("foreman-test-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        let dir = test_dir("sha");
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
        assert_eq!(
            expected_hash(sums, "foreman-v0.3.0-x86_64-windows.zip"),
            Some("bbbb".into())
        );
        assert_eq!(expected_hash(sums, "missing.zip"), None);
        // tolerate single-space and CRLF variants
        assert_eq!(
            expected_hash("cccc foreman.zip\r\n", "foreman.zip"),
            Some("cccc".into())
        );
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
        assert!(!probe_writable(std::path::Path::new(
            r"C:\nonexistent-foreman-probe-dir"
        )));
    }

    #[test]
    fn sibling_appends_to_the_full_filename() {
        assert_eq!(
            sibling(std::path::Path::new(r"C:\x\foreman.exe"), ".old"),
            std::path::PathBuf::from(r"C:\x\foreman.exe.old")
        );
    }
}
