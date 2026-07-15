# Foreman Install & Update — Resources

All Knowledge entries were verified against primary sources during the
2026-07-15 design fact-check (8/8 claims confirmed, 2 empirically on this
machine).

## Knowledge

- [Spec: 2026-07-14 Install & Update design](../docs/superpowers/specs/2026-07-14-install-and-update-design.md)
  The authoritative design this course teaches. Use for: every decision and
  its rationale; the changelog records what review changed and why.
- [self-replace crate docs](https://docs.rs/self-replace)
  How rustup-style self-updating works on Windows: a running exe can be
  renamed but not unlinked. Use for: the Phase-4 swap mechanics.
- [Outflank: "Mark-of-the-Web from a red team's perspective"](https://www.outflank.nl/blog/2020/03/30/mark-of-the-web-from-a-red-teams-perspective/)
  The definitive MotW explainer: Zone.Identifier streams, which tools apply
  them, why System.Net downloads don't. Use for: the SmartScreen story.
- [Archiver MotW propagation comparison](https://github.com/nmantani/archiver-MOTW-support-comparison)
  Table of which extractors propagate MotW (Explorer: yes; Expand-Archive:
  no). Use for: why irm|iex installs dodge SmartScreen but Explorer-extracted
  zips don't.
- [GitHub REST: rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)
  60 req/h/IP unauthenticated, per-IP. Use for: update-check cadence math.
- [GitHub REST: Releases API](https://docs.github.com/en/rest/releases/releases)
  `releases/latest` = newest non-prerelease, non-draft. Use for: why the
  Releases API is the manifest and what "latest" means exactly.
- [actions/runner-images — Windows 2025 readme](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md)
  What's on windows-latest (gcc 15.2, InnoSetup 6.7.1, gh 2.95.0, MSYS2).
  Use for: release.yml assumptions; re-check before blaming the pipeline.
- [Inno Setup: PrivilegesRequired](https://jrsoftware.org/ishelp/topic_setup_privilegesrequired.htm)
  Per-user no-UAC installs. Use for: the deferred installer phase; note the
  no-built-in-PATH-directive gotcha recorded in the spec.
- [ureq docs](https://docs.rs/ureq/latest/ureq/)
  Default TLS = rustls + webpki-roots, NOT the Windows cert store. Use for:
  the corporate-proxy silent-skip caveat and `platform-verifier`.
- Book: *A Philosophy of Software Design* — John Ousterhout
  Deep modules, shallow modules, interface vs implementation. Use for: the
  design vocabulary (seam, depth, deletion test) the spec review used.

## Wisdom (Communities)

- [r/rust](https://reddit.com/r/rust)
  High-signal; release/distribution threads recur. Use for: "how do you ship
  your Rust desktop app" sanity checks.
- [Rust Community Discord — #os-windows](https://discord.gg/rust-lang-community)
  Use for: Windows-specific linking/toolchain trouble (gnu vs msvc, mingw).
- [WezTerm discussions](https://github.com/wezterm/wezterm/discussions)
  The closest peer project (Rust terminal, GitHub-Releases distribution).
  Use for: precedent checks — how wez handled the same problem.

## Gaps

- No single great source on "designing self-updaters" as a discipline; the
  knowledge is scattered across rustup/self-replace/Sparkle/Squirrel
  implementations. Lessons synthesize from the spec's fact-check instead.
