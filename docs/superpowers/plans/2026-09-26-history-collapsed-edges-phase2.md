# Git History Collapsed Edges (Phase 2) Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to run these checkbox steps in order. The Graph task has an explicit review gate. Do not dispatch subagents unless the active card asks for them.

**Goal:** Keep Local and All Git History timelines narrow when many card branches fork from distant points on `main`, by drawing long edges as colored arrow stubs and freeing their lanes between the stubs.

**Architecture:** `Graph::feed` owns the 30-commit lookahead, edge classification, parked parent slots, and immutable `Row` output; `Graph::finish` drains its window at EOF. `stream_history` only packs emitted rows into `BATCH` pages. Rows carry paint-only arrow geometry, with no jump targets or pointer behavior. The width proof uses generated Git-shaped histories, entirely inside `Graph`.

**Tech Stack:** Rust 2024; `VecDeque` and `HashMap` from `std`; egui/eframe 0.34.3; existing `src/git_history.rs` and its module-local tests. No new dependency.

**Spec:** `docs/superpowers/specs/2026-09-26-history-branch-selector-design.md` §3, with the lean cuts in `C:\Users\sniff\AppData\Local\Temp\foreman-handoff-history-phase2-planning.md`. **Decision:** `docs/adr/0004-history-graph-streams-with-bounded-lookahead.md`.

## Global Constraints

- Build and test with `--target-dir target/agent`; this work runs inside Foreman. Never stop a process by the name `foreman`.
- Keep `BATCH = 512`, demand-driven reads, immutable pages, and viewport-only painting. The first full page may read at most `BATCH + LONG` commits. The lookahead belongs to `Graph`, not the worker's page loop.
- Production `LONG = 30`, `STUB = 1`; tests construct `Graph` with smaller values. An edge is long only when parent distance is **greater than** `LONG`, or the parent never arrives. Distance exactly `LONG` is short.
- Short edges preserve today's lane, color, `incoming`, and `outgoing` behavior. The existing `Graph::push` tests stay unchanged. A parked parent retains one color and no lane; duplicate children do not create duplicate parked slots. A live short child cancels the future up-stub.
- `Row::width` covers the node, visible live segments, and visible stubs only. It never counts a parked slot. Phase 1's `ease_lanes` needs no change.
- Arrowheads are painted only. No row numbers, target hash in `Arrow`, hover cursor, tooltip, arrow click, selection, or scrolling code. The existing whole-row click still selects its commit.
- Stage only named task files, never `.foreman/` or `git add -A`. End **each** implementation commit message with `Card: <implementation-card-id>`. Do not reuse this planning card's id. Run `cargo fmt` before code commits.
- The spec is the permanent decision record; add a dated “Phase 2 shipped lean: no arrow jumps” note **when the implementation ships**, preserving the original §3 design. Update `docs/git-history.md` before removing this plan under `docs/superpowers/README.md`.

## Review Focus

1. **A formerly live long parent still occupying a lane.** `Graph` must remove it as soon as the child row's down-stub is committed. The generated-history width test in Task 1 is the gate; review the before/after frontier transition, not only the arrow count.
2. **A parked parent that gains a short child.** Unpark into the short child's ordinary live edge, keep the parked color, and suppress ▲. Pinned by `parked_parent_joined_by_short_child_has_no_up_arrow`.
3. **Multiple long children of one parent.** Keep one parked slot and draw one ▲, regardless of child count. Pinned by `long_children_share_one_up_stub`.
4. **EOF and page boundaries.** `finish` drains exactly once; `stream_history` must not drop or duplicate its tail, request an extra page, or change rows when the page cut moves. Pinned by `lookahead_is_independent_of_page_cuts` and the two existing stream tests named below.
5. **Drawing and width agreement.** Every painted segment and triangle lies within `Row::width`; a missing parent produces only ▼. Verify arrow direction and color in a native screenshot before claiming the visual result.

## Task 1: Graph-owned lookahead, parked slots, and the width proof

**Files:** Modify `src/git_history.rs` (`Row`, `Graph`, the `tests` module). No Git process or egui context in the new Graph tests.

**Interfaces:** Keep `Graph::push(Commit) -> Row` as the existing immediate-layout helper so its three current graph tests remain byte-for-byte unchanged and can also supply the baseline width. Add `Graph::feed(Commit) -> Option<Row>` and `Graph::finish() -> Vec<Row>`. `Graph::default()` uses `(LONG, STUB) = (30, 1)`; a private `Graph::with_lengths(long, stub)` serves tests. Add `Row::arrows: Vec<Arrow>` where `Arrow { lane, color, direction: Up | Down }` has no target. Give `Graph` a `VecDeque<Commit>` plus a hash-to-future-row map for that queue; store parked parents separately from live `lanes`.

- [ ] **Step 1: Write the failing Graph tests.** Keep the existing `commit(hash, parents)` helper. Add a `collect` helper that calls `feed` for every commit, appends `finish`, and asserts each input commit appears exactly once in order. Use `long = 3`, `stub = 1` for the table tests. Add these named tests with exact assertions:

  | Test | Input shape and required assertion |
  |---|---|
  | `edge_at_threshold_stays_short` | `a→d`, with two intervening rows (distance exactly three); `a` has no ▼, the parent lane persists and `d` has no ▲. |
  | `long_edge_frees_lane_between_colored_stubs` | `a→far`, at least five intervening rows; `a` has one ▼, `far` has one ▲ one row before it, both colors equal, middle rows have no lane for `far`, and their width equals a linear-main baseline. |
  | `parked_parent_joined_by_short_child_has_no_up_arrow` | `a→far` is long and `near→far` is short; the latter restores the parked color as a live edge and neither its row nor the row above `far` has ▲. |
  | `long_children_share_one_up_stub` | Two far-apart children point to `far`; their ▼ colors match the single ▲ color, and only one parked entry and one ▲ exist. |
  | `missing_parent_has_only_down_stub` | Child names an absent parent; `finish` leaves no parked entries and emits no ▲. |
  | `merge_with_long_second_parent_keeps_first_parent_short` | Merge has a near first parent and distant second parent; first parent has its ordinary edge, second gets ▼/▲, and no wide lane spans the gap. |
  | `lookahead_is_independent_of_page_cuts` | For the same commits, collect `feed` output into pages at capacities 1, 2, 4, and 7 (without restarting `Graph`); flattened `Row` geometry, colors, widths, and arrows match one unpaged run. |

  The test helper must compare `Row` geometry rather than the `Commit`'s author/date strings. Derive `Debug, PartialEq` on `Arrow` and its direction; compare a compact tuple of `(hash, lane, color, incoming, outgoing, width, arrows)` or derive `PartialEq` on the row types if that does not alter production semantics.

  Write the central edge test with explicit row positions; reuse its `collect` helper in the other six cases:

  ```rust
  fn collect(g: &mut Graph, commits: Vec<Commit>) -> Vec<Row> {
      let hashes: Vec<_> = commits.iter().map(|c| c.hash.clone()).collect();
      let mut rows: Vec<_> = commits.into_iter().filter_map(|c| g.feed(c)).collect();
      rows.extend(g.finish());
      assert_eq!(rows.iter().map(|r| &r.commit.hash).collect::<Vec<_>>(),
                 hashes.iter().collect::<Vec<_>>());
      rows
  }

  #[test]
  fn long_edge_frees_lane_between_colored_stubs() {
      let rows = collect(&mut Graph::with_lengths(3, 1), vec![
          commit("card", &["fork"]),
          commit("m0", &["m1"]), commit("m1", &["m2"]),
          commit("m2", &["m3"]), commit("m3", &["m4"]),
          commit("m4", &[]), commit("fork", &[]),
      ]);
      let down = rows[0].arrows.iter().find(|a| a.direction == Direction::Down).unwrap();
      let up = rows[5].arrows.iter().find(|a| a.direction == Direction::Up).unwrap();
      assert_eq!(down.color, up.color);
      assert!(rows[1..5].iter().all(|r| r.width == 1));
      assert!(rows[1..5].iter().all(|r| r.arrows.is_empty()));
  }
  ```

  Use the same `Direction` enum name in `Arrow`, production, and the paint task. If the cell convention places ▲ on the parent row rather than the row immediately above it, change the implementation: `STUB = 1` requires row 5 here.

- [ ] **Step 2: Add the generated-history proof test.** Use this fixture and assertion. It is newest-to-oldest topological order: each even card is merged through the preceding `main` commit; odd cards remain open. Every card has a distinct, distant fork point on the later main spine. The values 5, 20, and 50 intentionally exercise growth, and the test needs no Git process.

  ```rust
  fn staggered_cards(n: usize) -> Vec<Commit> {
      let mut history = Vec::new();
      for i in 0..n {
          let next = format!("main/{}", i + 1);
          let card = format!("card/{i}");
          let parents = if i % 2 == 0 {
              vec![next.as_str(), card.as_str()]
          } else {
              vec![next.as_str()]
          };
          history.push(commit(&format!("main/{i}"), &parents));
          let fork = format!("main/{}", n + 5 + i * 5);
          history.push(commit(&card, &[&fork]));
      }
      for i in n..=6 * n + 10 {
          let parents = if i == 6 * n + 10 {
              Vec::new()
          } else {
              vec![format!("main/{}", i + 1)]
          };
          history.push(commit(
              &format!("main/{i}"),
              &parents.iter().map(String::as_str).collect::<Vec<_>>(),
          ));
      }
      history
  }

  #[test]
  fn staggered_card_branches_have_bounded_visible_width() {
      for n in [5, 20, 50] {
          let history = staggered_cards(n);
          let mut collapsed = Graph::with_lengths(3, 1);
          let mut collapsed_rows = Vec::new();
          for c in &history {
              if let Some(row) = collapsed.feed(commit(
                  &c.hash,
                  &c.parents.iter().map(String::as_str).collect::<Vec<_>>(),
              )) {
                  collapsed_rows.push(row);
              }
          }
          collapsed_rows.extend(collapsed.finish());
          assert_eq!(collapsed_rows.len(), history.len());
          let collapsed_max = collapsed_rows.iter().map(|r| r.width).max().unwrap();
          let mut old = Graph::default();
          let uncollapsed_max = history.into_iter().map(|c| old.push(c).width).max().unwrap();
          assert!(collapsed_max <= 4, "N={n}, collapsed={collapsed_max}, old={uncollapsed_max}");
          assert!(uncollapsed_max >= n / 2, "N={n}, collapsed={collapsed_max}, old={uncollapsed_max}");
      }
  }
  ```

  This is the width proof; do **not** benchmark today's repositories or require a pixel measurement. Keep the fixture shape if the allocator needs adjustment; do not raise the width bound without explaining the visible short edges that require it.

- [ ] **Step 3: Run the focused tests red.** Run `cargo test --target-dir target/agent git_history::tests::long_edge_frees_lane_between_colored_stubs`. Expected: compile failure for `feed`, `finish`, or `Arrow` until the types and engine exist. Keep the new test names; do not relax the width bound to make an incorrect allocator pass.

- [ ] **Step 4: Implement the Graph state machine.** Use this order for each `feed`/layout transition:

  1. Push the read commit into `VecDeque` and record its hash at the global read index. Once there are more than `long` buffered commits, pop the oldest and lay it out using the remaining queue as the future window. `feed` returns at most one `Row`. `finish` lays out all remaining commits in order, then clears parked state. Keep only the current lookahead hashes in the future map.
  2. For every parent of the row being laid out, classify it as short when the hash is in the future window at distance `1..=long`; otherwise classify it long. Do not infer long from page position. Existing live lanes for a parent are shared; short child edges must revive a parked slot before normal allocation.
  3. Lay out short parents through the current frontier rules in `push`, retaining its colors and joins. Put each long parent in one parked hash entry carrying its chosen color; the first parent's color follows the child's color, and later parents take the existing next-color path. Draw the child's down edge for `stub` rows and a ▼ at its end, then remove that edge's lane from the continuing frontier. Do not allocate a continuing lane for a parked parent.
  4. When a parked parent is within `stub` rows of arrival, allocate one live lane and draw one ▲ at the beginning of its up-stub, unless a short child has already made that parent live. A second or later long child reuses the parked color. The parent's arrival consumes the slot through the ordinary frontier transition.
  5. Compute `Row::width` from the before/after live frontiers, node lane, and visible stub/arrow lanes after compaction. `Row` output is final; never mutate an emitted row. Use a small helper to avoid duplicating the existing short-edge layout rules; keep `Graph::push` observable behavior unchanged for the old tests.

  `long` and `stub` are fields, with `assert!(long > 0 && stub > 0 && stub < long)` in `with_lengths`. `Default` calls `with_lengths(30, 1)`. Name the parked struct for its role (`Parked { color, ... }`); do not store a jump target row/hash in `Arrow`.

- [ ] **Step 5: Run the Graph tests green.** Run `cargo fmt`, then `cargo test --target-dir target/agent git_history::tests::`. Expected: the old linear/merge/octopus tests and the new Graph tests pass; the generated proof reports a bound of at most 4 with `N = 5, 20, 50`. Inspect one failure with its generated hashes/rows; do not substitute an empirical repo count.

- [ ] **Step 6: Review the Graph task before committing.** Review the exact `long` threshold, short-child unpark, duplicate-parent join, color stability, EOF cleanup, and max-width test. Resolve review findings, rerun the focused tests, then commit only `src/git_history.rs` with `git add src/git_history.rs` and a message ending `Card: <implementation-card-id>`.

## Task 2: Pack Graph output into demand-driven pages

**Files:** Modify `src/git_history.rs` (`stream_history` and its module-local worker tests).

**Interface:** The worker reads commits only to obtain `graph.feed` rows; at EOF it calls `graph.finish` once and drains those rows. A small `VecDeque<Row>` may hold the ≤`LONG` drain tail when it crosses a `BATCH` boundary. The worker never owns a commit lookahead or performs edge classification.

- [ ] **Step 1: Add a failing worker-boundary test.** Extend `demand_batches_keep_graph_continuity_and_stop_when_view_closes` with a history of `BATCH + 20` commits whose card head at the top forks more than 30 rows into the next page. Assert the first request yields exactly `BATCH` rows, the second yields exactly 20 and `end = true`, the cross-page long edge has the same stub color on both pages, no page arrives without a request, and cancellation still sets the flag. Also add a pure `Graph` page-cut check for cuts at `BATCH - 1`, `BATCH`, and `BATCH + 1` if Task 1's small-capacity test does not already cover those offsets.

- [ ] **Step 2: Run it red.** Run `cargo test --target-dir target/agent git_history::tests::demand_batches_keep_graph_continuity_and_stop_when_view_closes`. Expected: the new assertions fail while `stream_history` still calls `graph.push`.

- [ ] **Step 3: Change only the page loop.** In `stream_history`, keep `requests.recv`, cancel, `Page`, error, and `resolved.take()` behavior. On each request fill `rows` from any pending `finish` tail first, then read `read_commit` and append the optional `graph.feed` row. On the first `None`, call `finish` once and queue its result. Mark `end` only when EOF is known and the tail is empty. Do not read another commit once `rows.len() == BATCH`, and do not send pages without a request. Preserve the existing `exit.finish()` error mapping on the final page.

- [ ] **Step 4: Run worker and viewport checks.** Run `cargo fmt`, then `cargo test --target-dir target/agent git_history`. Expected: all history tests pass, including `demand_batches_keep_graph_continuity_and_stop_when_view_closes` and `large_history_paints_only_viewport_rows_and_scrolls_to_old_commits`. Build with `cargo build --target-dir target/agent`; expected: no new warning from `src/git_history.rs`.

- [ ] **Step 5: Commit.** Stage only `src/git_history.rs` and commit with a message ending `Card: <implementation-card-id>`.

## Task 3: Paint colored ▲/▼ stubs without interaction

**Files:** Modify `src/git_history.rs` (the `HistoryView::show` row painter and a module-local paint test if it can inspect egui shapes without pixel assumptions).

- [ ] **Step 1: Add a failing paint-shape test.** Construct rows through `Graph::with_lengths(3, 1)` with one long edge, render them in a headless `egui::Context`, and inspect recorded shapes: the down/up arrows use `COLORS[arrow.color % COLORS.len()]`, point in the correct vertical direction, and fit inside the graph width. Keep the row-click test unchanged; do not add arrow-click tests.

- [ ] **Step 2: Run the paint test red.** Run `cargo test --target-dir target/agent git_history::tests::long_stub_arrows_paint_in_lane_color`. Expected: failure because the row painter ignores `arrows`.

- [ ] **Step 3: Paint the two shapes.** In the existing row painter after incoming/outgoing lines and before the node, iterate `row.arrows`. Use `x(arrow.lane)`, `r.top()/center().y/bottom()`, scaled stem stroke `1.7 * s`, and a small filled triangle in the same `COLORS` entry. ▲ points toward `r.top()`, ▼ toward `r.bottom()`; for `STUB = 1` the arrow is at the free end of its one-row stem. Clip through the existing `p` painter. Keep the row's one `allocate_exact_size(..., Sense::click())`; add no `interact`, hover, tooltip, `HistoryAct`, or selected-hash path for an arrow.

- [ ] **Step 4: Run tests and inspect a native screenshot.** Run `cargo fmt`, `cargo test --target-dir target/agent git_history`, and `cargo build --target-dir target/agent`. Use `.codex/skills/build-screenshot` for the native capture (it is user-triggered in the Claude copy, so follow the Codex copy in this Session). Use a generated Git fixture with at least one long card fork, one merge, and one near child joining a parked parent; inspect ▼/▲ direction, theme color, stub length, and the narrowed subject column. No claim of visual correctness from terminal output alone.

- [ ] **Step 5: Commit.** Stage only `src/git_history.rs` and commit with a message ending `Card: <implementation-card-id>`.

## Task 4: Record the shipped lean behavior and retire the plan

**Files:** Modify `docs/git-history.md` and `docs/superpowers/specs/2026-09-26-history-branch-selector-design.md`; delete this plan **only after** Tasks 1–3 have shipped and the feature doc holds the how-to.

- [ ] **Step 1: Update the feature doc.** In the timeline section of `docs/git-history.md`, explain the 30-row threshold, the one-row colored ▼/▲ stubs, why the middle lane is free, the parent-already-live case, and that arrows are visual only. In Validation, record the date, generated-history N values and observed max widths from the pure Graph proof, history-test result, and exact screenshot fixture/capture checked. Cite `src/git_history.rs` by `Graph::feed`, `Graph::finish`, and `HistoryView::show`, without line numbers.

- [ ] **Step 2: Append a dated scope note to the spec.** At the end of §3, add a short subsection headed `Phase 2 shipped lean (YYYY-MM-DD)` stating that long edges, parked colors, one up-stub, and drawn ▲/▼ shipped; row-index/hash jump targets, arrow tooltips/clicks, and arrow-click UI tests were deferred. Leave the original design and rejection rationale intact. If the implementation date differs from this plan's date, use the actual ship date.

- [ ] **Step 3: Validate the docs and code.** Run `pwsh -NoProfile -File .claude/hooks/cite-guard.ps1 -All`, `cargo test --target-dir target/agent git_history`, and `cargo build --target-dir target/agent`. Expected: citation guard clean; all history tests and build pass. Record any screenshot limitation honestly if native evidence cannot be obtained.

- [ ] **Step 4: Commit the docs, then retire this plan.** Stage the two doc files by name and commit with `Card: <implementation-card-id>` as the final line. Per `docs/superpowers/README.md`, remove `docs/superpowers/plans/2026-09-26-history-collapsed-edges-phase2.md` in the same or a follow-up implementation commit once the code and feature doc have landed. Do not delete the spec.

## Hybrid batching

| Batch | Work | Review gate | Commit |
|---|---|---|---|
| 1 | Task 1: Graph lookahead, parking, generated-history proof | **Required** before commit: inspect frontier transitions and the three N results | One Graph commit |
| 2 | Task 2: worker page packing | Tests and build; no separate review | One stream commit |
| 3 | Task 3: arrow painting | Native screenshot plus tests; no separate code review | One paint commit |
| 4 | Task 4: feature doc, spec note, plan retirement | Citation guard and final checks; no separate review | One docs commit, plus removal when shipped |

The implementation executor should finish each batch before starting the next. The pure generated-history proof is the acceptance criterion for width; repository snapshots are visual evidence for shape and color only.
