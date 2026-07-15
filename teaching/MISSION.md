# Mission: Foreman's install & update system

## Why
Andy is shipping foreman publicly and will own its distribution forever: every
broken release, bricked update, and "it won't install" report lands on him.
Agents will write much of the implementation, so he must understand the
architecture deeply enough to review their work, veto bad changes, and debug a
failed release pipeline or a half-swapped exe without re-deriving the design
from scratch.

## Success looks like
- Trace a release end-to-end (tag push → CI → GitHub Release → install script
  / updater → swapped exe) and name the invariant each stage protects.
- Diagnose a broken release from symptoms: SmartScreen complaint, hash
  mismatch, chip never appears, swap rollback, PATH missing.
- Review an agent's PR against `src/update.rs` or `release.yml` and spot a
  violation of the seams (asset naming, layout fact table, pure-core state
  machine) on sight.
- Explain to another dev *why* GitHub Releases is the manifest and why the
  swap lives only in Rust — the reasoning, not just the rule.

## Constraints
- Experienced Rust dev (built foreman's terminal emulator) — skip language
  basics, go straight at distribution concepts.
- Short lessons; learning happens between real work sessions.
- Grounded in foreman's actual spec, not generic theory.

## Out of scope (for now)
- Code signing / Azure Trusted Signing (deferred in the spec).
- winget manifest authoring, Inno Setup internals (deferred phases).
- macOS/Linux distribution.
