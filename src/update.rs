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
}
