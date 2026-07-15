# Teaching notes

## Learner profile
- Andy: experienced Rust dev, wrote foreman (terminal emulator, wm, egui).
  High ZPD for code; the *new* territory is distribution/ops concepts
  (MotW, manifests, swap mechanics, release pipelines).
- No flattery, no hype (global CLAUDE.md rule). Direct, technical, plain
  language — grug-brain docs preference carries over to teaching.
- Quality-obsessed; push back rather than agree.

## Preferences observed
- 2026-07-15: chose "own it as maintainer" mission over broad-domain
  learning — keep every lesson anchored to foreman's actual spec and files.
- Workspace in `teaching/` subdir to keep repo root clean.

## Course plan (revise as records accumulate)
1. 0001 — Anatomy of a release: the end-to-end journey + three invariants. DONE
2. 0002 candidate — The updater state machine: pure core, Events/Effects,
   why testability forced the shape (interactive state-machine explorer?).
3. 0003 candidate — MotW & SmartScreen mechanics: Zone.Identifier streams,
   who writes them, who strips them (hands-on: inspect streams in pwsh).
4. 0004 candidate — Debugging a broken release: symptom → stage table drill.

## Format notes
- Quiz answers must keep equal word counts (no length tells).
- Lessons link: ../reference/*.html, ../RESOURCES.md, and the spec via
  relative path ../../docs/superpowers/specs/.
