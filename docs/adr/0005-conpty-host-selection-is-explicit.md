# ADR 0005 — Select ConPTY hosts explicitly in tests and production

- **Status:** accepted
- **Date:** 2026-10-06
- **Source:** card qrmdos; policy confirmed with the user before implementation

## Context

portable-pty 0.9's `win/psuedocon.rs::load_conpty` first loads kernel32,
then prefers a bare-name `conpty.dll` load. Under the standard Windows DLL
search order, that can find Foreman's installed pair on PATH. Local tests
therefore used OpenConsole by accident while clean CI used in-box conhost.
The hosts differ in startup queries and timing. Production's
`InstallOutcome::SideloadDisabled` also left the PATH copy eligible, defeating
the intended in-box fallback.

## Decision

Ordinary `cargo test` Sessions use the exact embedded OpenConsole pair.
`FOREMAN_TEST_CONPTY_HOST=inbox` selects in-box conhost for an entire test
process; `openconsole` explicitly selects the default. Other values fail.
This selector is compiled only into tests. CI runs the suite in separate
jobs for both hosts, with neither failure cancelling the other.

Before the first test Session opens a PTY, a once-only initializer establishes
the DLL search policy. OpenConsole installs and leases the embedded pair beside
the test executable and pins its DLL by absolute path for the process lifetime.
Installation or loading failure fails the test instead of silently substituting
the in-box host. In-box mode searches only System32, ignoring even stale
sidecars beside the executable. An already loaded conflicting ConPTY module
fails initialization.

Production's `ensure_conpty` restricts bare-name DLL searches to the application
directory and System32 before installation. PATH and the current directory are
excluded, so disabling the application sidecar leaves the in-box fallback.
The policy applies to this process's later DLL loads, not child executable
lookup; PATH remains available for launching shells and agents.

## Why

The default exercises the host users normally run. The second CI job exercises
the supported degraded mode. Pinning only in-box conhost would omit the usual
production path; pinning only OpenConsole would omit the fallback. Documentation
alone would leave both the test mismatch and the product bug intact.

Windows' process-wide loader policy and portable-pty's lazy host API make
separate processes necessary. The DLL-lock test also runs in its own process
so its temporary module cannot race another test's host initialization.

## Consequences and validation

CI spends an additional suite run to cover the fallback. The in-box Windows
version still depends on the OS image; this decision pins the host choice,
not the OS build. Tests requiring graphics passthrough remain specific to
OpenConsole.

`conpty_install::tests::host_selection_ignores_path_and_current_directory`
launches isolated test executables with a valid pair on PATH and in the
current directory. It checks the loaded module after a real PTY creation for
OpenConsole, in-box mode (also with stale application sidecars), and a forced
production installation failure. The OpenConsole probe checks the module's
absolute path; fallback probes require no sideloaded ConPTY module.

Key files: `src/conpty_install.rs`, `src/terminal.rs::Session::spawn_with`,
`.github/workflows/test.yml`, `.claude/skills/foreman-build-and-env/SKILL.md`.

Windows loader contract:
[SetDefaultDllDirectories](https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-setdefaultdlldirectories).
