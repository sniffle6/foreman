# ADR 0004 — The history graph streams with a bounded lookahead (full-graph preload rejected)

- **Status:** accepted
- **Date:** 2026-09-26
- **Source:** Git History branch-selector design
  (`docs/superpowers/specs/2026-09-26-history-branch-selector-design.md`, §3)

## Context

The Git History timeline collapses long edges (a child far above its parent)
into two short arrow stubs, so the lane between them is free for other
branches. To draw the top stub, the layout must already know the edge is
long, which means knowing the parent's row. `src/git_history.rs` streams
`git log` in demand-driven pages (`BATCH`), so the parent may not have been
read yet.

JetBrains' Git log solves this by loading the whole commit graph first:
every edge knows both row indices, and arrow placement is arithmetic
(`PrintElementGeneratorImpl`, `LONG_EDGE_SIZE = 30`, in `intellij-community`).

## Decision

The layout runs a fixed `LONG` rows (30) behind the reader. When a row is laid
out, the next 30 commits are already buffered, so "is this parent more than 30
rows away?" is answered locally. A long edge draws its down-stub and parks
the parent's frontier slot with no lane. The slot gets a lane back, with an
up-arrow, when the parent comes within the stub length. Rows already emitted
never change.

## Why

1. **First paint stays immediate.** A full preload walks every commit
   reachable from the scope before the first row can be drawn, so its cost
   grows with the repo. The lookahead adds 30 buffered commits to the first
   page and nothing after it.
2. **Memory stays proportional to what is loaded.** A preload holds every
   hash and parent list of the repo for the window's lifetime, even when the
   human only reads the first screen.
3. **Pages stay immutable.** The alternative that keeps streaming without a
   window — re-lay-out earlier rows when a parent lands — moves rows under
   the reader and breaks the viewport-only paint assumptions.
4. **The drawing is identical to JetBrains'.** The same 30-row threshold and
   1-row stubs; the only visible difference is what a down-arrow click does
   (below).

## Consequences

- **A down-arrow knows its target's hash, not its row.** Clicking one opens
  the parent in the details pane without scrolling the list. With a preload
  it could scroll straight to the parent's row. This is the one place the
  choice is felt.
- The threshold must equal the lookahead: an edge can only be classified as
  long if the window is at least `LONG` rows deep. Raising `LONG` raises the
  buffer with it.
- Layout output must not depend on where page boundaries fall. The
  page-split-independence test pins this and is the guard for any future
  change to the allocator.

## Revisit when

- A feature needs whole-graph knowledge anyway (for example graph-wide
  search by ancestry, or a "show long edges" mode with thresholds far beyond
  the window). At that point the preload's cost is paid regardless, and the
  arrow jump should become a plain scroll.
