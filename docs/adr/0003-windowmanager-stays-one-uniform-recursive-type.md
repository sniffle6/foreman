# ADR 0003 — WindowManager stays one uniform recursive type (Desktop/Engine split rejected)

- **Status:** accepted (rejection recorded)
- **Date:** 2026-07-07
- **Source:** architecture-review candidate 06, killed by a 3-skeptic adversarial
  verification pass (all three: solution *harmful*, high confidence)

## Context

An architecture review proposed splitting `WindowManager` into a `Desktop` that
owns an `Engine`, on the grounds that one type fuses two roles: children carry
desktop-only fields dead (`keymap`, `picker`, `settings`, `armed`, `show_help`,
`desktop`) and pay an `if self.desktop` tax.

## Decision

Rejected. One `WindowManager` type keeps serving both the desktop level and the
nested project level (`Content::Project(Box<WindowManager>)`), with role
differences expressed as documented no-op/None fields.

## Why (the load-bearing facts)

1. **The tax is exactly 3 `if self.desktop` branches** in ~7,000 lines, plus
   one `Keymap::default()` per project *creation*. Nothing per-frame, nothing
   on the paint hot path.
2. **The fusion is symmetric.** `cwd`, `tag`, and `chat` are *project-only*
   fields carried dead by the desktop, each with a doc comment declaring the
   no-op default — the exact mirror of `keymap`'s "child managers carry a
   default and never read it." An honest role split needs three types, not two.
3. **`picker`/`settings` are not dead in children** — they are uniform
   always-None gates read every frame by shared paths (`is_focus`, `deserted`,
   `overlay_blocks_close`). The uniform type is what lets ONE close funnel and
   ONE focus gate serve both levels.
4. **Desktop state is interleaved mid-`show()`** at 4+ points, so a split must
   thread host-modal state back into Engine (reintroducing the fields) or
   duplicate the show pipeline — plus ~9 forwarded main.rs entry points.
5. **One adapter.** Exactly one desktop instance will ever exist (a single
   `as_desktop()` call site). A seam with one consumer varying nothing is
   hypothetical.
6. The one-engine-recursed design is the recorded architecture ("one
   `WindowManager` engine runs at the desktop level and nested inside each
   project" — CLAUDE.md / HANDOFF.md). Reversing it is a user-only decision.

## Consequences / revisit when

- New desktop-only or project-only state keeps following the existing pattern:
  a field on the shared type with a doc comment stating which role reads it and
  what the other role's harmless default is.
- If the role blur ever genuinely hurts, the grug-sized first step is grouping
  the desktop-only fields under a labeled comment block (or a
  `desktop: Option<DesktopState>` sub-struct) on the SAME type — not a split.
- Revisit only if a second desktop-like consumer of the engine appears
  (two-adapter rule).
