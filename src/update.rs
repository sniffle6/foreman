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
pub enum State {
    Idle,
    UpdateAvailable {
        version: String,
        html_url: String,
        can_apply: bool,
    },
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

// Phase 3 is notify-only; Phase 4 flips this to a real writability probe.
const CAN_APPLY: bool = false;

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
            (
                S::UpdateAvailable {
                    version,
                    html_url,
                    can_apply: false,
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
        let r = rel("v0.2.1", &["SHA256SUMS.txt", "foreman-v0.2.1-x86_64-windows.zip"]);
        assert_eq!(
            select_asset(&r.assets).unwrap().name,
            "foreman-v0.2.1-x86_64-windows.zip"
        );
        let renamed = rel("v0.2.1", &["totally-different-x86_64-windows.zip"]);
        assert!(select_asset(&renamed.assets).is_some());
        let none = rel("v0.2.1", &["SHA256SUMS.txt", "foreman-setup.exe"]);
        assert!(select_asset(&none.assets).is_none());
    }

    // step: fetch outcomes
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
        let showing = State::UpdateAvailable {
            version: "v0.2.1".into(),
            html_url: "u".into(),
            can_apply: false,
        };
        let (s, _) = step(showing, Event::ReleaseFetched(rel("v0.3.0", &[])), "0.2.0");
        match s {
            State::UpdateAvailable { version, .. } => assert_eq!(version, "v0.3.0"),
            other => panic!("expected UpdateAvailable, got {other:?}"),
        }
    }

    #[test]
    fn fetch_failure_is_silent_skip_in_any_state() {
        let showing = State::UpdateAvailable {
            version: "v0.2.1".into(),
            html_url: "u".into(),
            can_apply: false,
        };
        let (s, fx) = step(showing.clone(), Event::FetchFailed, "0.2.0");
        assert_eq!(s, showing);
        assert!(fx.is_empty());
        let (s, fx) = step(State::Idle, Event::FetchFailed, "0.2.0");
        assert_eq!(s, State::Idle);
        assert!(fx.is_empty());
    }

    // step: chip click (Phase 3 = notify-only)
    #[test]
    fn click_without_can_apply_opens_releases_page() {
        let showing = State::UpdateAvailable {
            version: "v0.2.1".into(),
            html_url: "https://gh/rel".into(),
            can_apply: false,
        };
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
}
