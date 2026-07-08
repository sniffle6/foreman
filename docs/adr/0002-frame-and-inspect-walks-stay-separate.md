# ADR 0002 — frame and inspect keep separate grid walks; snapshot cursor stays raw (`visible_cells` seam rejected)

- **Status:** accepted (rejection recorded)
- **Date:** 2026-07-07
- **Source:** architecture-review candidate 02, killed by a 3-skeptic adversarial
  verification pass (2× *harmful*, 1× *flawed*; all high confidence)

## Context

An architecture review read `frame::text_rows`, `inspect::snapshot_text`, and
`inspect::snapshot_cells` as three drifted copies of one grid walk and proposed
a shared `visible_cells()` seam, citing three "divergences": frame doesn't skip
`WIDE_CHAR_SPACER`, frame keeps trailing whitespace, and `snapshot --cursor`
reports the raw cursor while the painted caret goes through `CaretGate`.

## Decision

Rejected. The walks stay separate; `inspect::cursor_info` keeps reading the raw
model cursor. (The only accepted residue: the byte-identical Region clamp
duplicated *within* inspect.rs was folded into a private helper — internal to
that one file.)

## Why (the load-bearing facts)

1. **Every "divergence" is documented, test-pinned intent, not drift.**
   - Cursor: `docs/cursor-rendering.md` states outright that `snapshot
     --cursor` reports the raw model cursor, "not the gated draw — it wants
     ground truth." Routing it through `CaretGate` would make headless reads
     stale and wall-clock-nondeterministic, and `CaretGate::observe` is `&mut`
     state-committing — a read would mutate what the screen paints.
   - Spacer: frame must emit one char per grid cell so galley x-positions stay
     column-aligned with metrics-derived caret/selection rects; inspect skips
     the spacer deliberately (commented + two dedicated tests).
   - Trim: frame must keep trailing styled cells because that is how a
     full-width status bar's background gets painted; inspect's trim is its
     documented snapshot-ergonomics contract.
2. **The truly shared code across the frame/inspect boundary is ~1–2 lines**
   (the `Line(row - off)` offset map and the NUL→space ternary). A shared seam
   would need parameters for clamp source, spacer policy, trim, receiver type,
   and per-cell payload — every axis the callers legitimately differ on. That
   is a loop skeleton with mode flags, not a deep module.
3. **Both sides are themselves deliberate, recent seams** (frame.rs as the pure
   half of the paint path; inspect.rs's "the interface is the test surface"
   contract). Merging their guts reopens settled module boundaries.

## Consequences / revisit when

- Fixing any wide-char/trim rule means checking BOTH walks on purpose — that is
  the accepted cost, and the inspect-side tests pin each contract.
- Revisit only if a *fourth* consumer of the grid appears whose needs match an
  existing walk exactly (two-adapter rule), or if paint and snapshot are ever
  required to render identically by a product decision — which would be a
  design reversal (user-only), not a refactor.
