---
name: foreman-proof-and-analysis-toolkit
description: Use when a foreman investigation needs proof instead of eyeballing — tracing raw PTY bytes, splitting "our bug" from a ConPTY/platform bug, extracting GUI- or PTY-entangled logic into a pure testable seam, timeout/deadline nesting (MAX_SETTLE_MS, REPLY_TIMEOUT, CONNECT_TIMEOUT), pixel-to-cell or buffer-vs-viewport coordinate math, root-causing a flaky PTY test, wire-compat for control.rs protocol changes, or exhaustive state-machine test tables.
---

# Foreman Proof & Analysis Toolkit

First-principles analysis recipes, each proven in this repo's history.
House rule: **prove it, don't just observe it.** An observation ("it looks
fixed") is not a result; a result names a mechanism and shows the evidence that
mechanism predicts. The general evidence bar and result lifecycle live in
**foreman-research-methodology** — this skill is the recipe box.

Code is cited by file and symbol, never by line number.

## Picking a recipe

| You are about to… | Recipe |
|---|---|
| Theorize about garbled/misplaced terminal output | 1. Byte-level PTY tracing |
| Decide whether a terminal bug is ours or Windows' | 2. A/B against a reference implementation |
| Test logic that is tangled with egui / live Sessions / the clock | 3. Pure-seam extraction |
| Do any performance work | 4. Predict-then-measure |
| Add or change a timeout, settle, or deadline | 5. Timeout-budget analysis |
| Write pixel↔cell or grid-index math | 6. Coordinate-system proofs |
| "Fix" a flaky test | 7. Race root-causing |
| Add a field to a Control plane message | 8. Wire-compat proof |
| Test a state machine | 9. Transition-table contract testing |

## Domain terms used below (defined once)

- **PTY / ConPTY** — a pseudo-terminal: the OS object that makes a child
  process believe it is talking to a real terminal. ConPTY is Windows'
  implementation. Details: **terminal-emulation-reference**.
- **VT escape sequence** — in-band control bytes like `ESC[15~` (F5) or
  `ESC[23;22H` (move cursor to row 23, col 22) that programs and terminals
  exchange. Details: **terminal-emulation-reference**.
- **DSR** — Device Status Report, `ESC[6n`: the child asks "where is the
  cursor?" and hangs until the terminal answers. A Session is **Ready** once
  its startup DSR has been answered (CONTEXT.md "Ready"; the latch is in
  `Session::pump`, `src/terminal.rs`).
- **Grid / viewport / `display_offset`** — `alacritty_terminal` (the crate
  foreman uses as parser + screen model) keeps the visible screen plus
  scrollback history in one grid. `display_offset` is how many lines the user
  has scrolled back; the grid's `Line` index goes negative into history.

---

## Recipe 1 — Byte-level PTY tracing

**When:** rendered output looks wrong (corrupted prompt, misplaced cursor,
missing text) and you are about to theorize. `foreman snapshot` shows the
*parsed* grid — downstream of the parser. Before blaming foreman's resize
logic, ConPTY, or the shell, read the actual bytes.

**Where the bytes flow** (all in `src/terminal.rs`):

| Direction | Path |
|---|---|
| ConPTY → foreman | the reader thread spawned by `Session::spawn` (chunked reads → mpsc channel) → `Session::pump` drains and feeds the parser |
| foreman → ConPTY | `Session::send`; plus `pump()` flushing emulator replies (DSR answers) back to the child |

**Steps:**

1. Add a *temporary* tee at the seam you suspect: in the reader loop before
   `tx.send` (inbound), and in `send()` (outbound), append each chunk to a
   file with a direction marker and length. Both sides matter — the question
   is always a dialogue ("what did we send right before it emitted that?").
2. Reproduce minimally. One reproduction, small window, known grid size.
3. Decode the trace. PowerShell 7+, from the repo root
   (`H:/claude code/foreman`), to make escapes readable:

   ```powershell
   [System.IO.File]::ReadAllBytes("$env:TEMP\pty-trace.bin") |
     ForEach-Object { if ($_ -eq 27) { '<ESC>' } elseif ($_ -ge 32 -and $_ -le 126) { [char]$_ } else { '<{0:X2}>' -f $_ } } |
     Join-String
   ```

4. Ask the discriminating question: **are the bytes themselves wrong** (child
   or ConPTY bug — foreman is rendering faithfully), or **are correct bytes
   being parsed/painted wrong** (our bug)?
5. Remove the instrumentation before committing — same throwaway-harness rule
   as the latency work (`docs/followups-latency-and-control.md` § How to
   verify). Permanent measurement tooling belongs to
   **foreman-diagnostics-and-tooling**.

**Worked example — the ConPTY resize/recall investigation
(`docs/conpty-resize-reflow.md`, commit `5332757`).** An earlier diagnosis
blamed a "double reflow" in `Session::resize`. Byte tracing disproved it: the
resize repaint ConPTY emitted was itself clean (prompt input at row 24), but
the history-recall arrived as `ESC[23;22H` — one row too high — and foreman
had sent nothing but the Up key in between. The bytes disagreed *with each
other*, so no foreman-side reflow fix could matter; root cause is ConPTY's
reflow diverging from its own reported cursor (microsoft/terminal #18725).
Full dead-end chronicle: **foreman-failure-archaeology**.

**Done when:** you can quote the exact wrong sequence, say which side emitted
it, and say what foreman sent immediately before it.

**Evidence bar:** raw sequences quoted in the writeup, not paraphrased; the
reproduction recipe recorded. General bar: **foreman-research-methodology**.

---

## Recipe 2 — A/B against a reference implementation

**When:** Recipe 1 shows suspicious bytes from the child/ConPTY side and you
need to split "our frontend" from "the platform". The reference is Windows
Terminal — it drives the same kind of ConPTY with a mature frontend.

**Steps:**

1. Script the identical scenario: same shell, same starting size, same resize
   deltas, same keystrokes, in both foreman and Windows Terminal.
2. Compare outcomes. Divergence localizes the bug; identical corruption in
   both means platform.
3. **Interpret with the version caveat:** Windows Terminal bundles its own
   newer ConPTY/OpenConsole and replicates conhost's reflow math
   byte-for-byte (microsoft/terminal PR #4741, per
   `docs/conpty-resize-reflow.md`). "Clean in Windows Terminal" can mean
   "different platform code ships inside it", not "foreman's logic is wrong".
   Pair the A/B with the byte trace before assigning blame.

**Worked example — same investigation.** Windows Terminal rendered the
identical PowerShell resize+recall scenario cleanly
(`docs/conpty-resize-reflow.md` § Evidence). Combined with the byte trace
(foreman renders ConPTY's bytes faithfully; the bytes are self-inconsistent),
this proved the bug upstream. Four "let ConPTY own the redraw" variants were
then tried and all failed — recorded so they are never re-tried
(**foreman-change-control** holds the do-not-re-litigate list).

**Done when:** the same scripted reproduction has a documented result in both
stacks, and you can name what differs between them (frontend logic vs bundled
platform component).

**Evidence bar:** the scenario script + both outcomes recorded; a claim of
"platform bug" additionally needs the byte-level evidence from Recipe 1.

---

## Recipe 3 — Pure-seam extraction (the house move)

**When:** a decision is entangled with egui frames, live ConPTY Sessions, or
the wall clock, so every test of it is a flaky integration test. This is
foreman's standard answer: carve the *decision* into a pure module with
injected inputs; the GUI/PTY shell shrinks to an adapter that supplies live
state and applies side effects.

**Steps:**

1. **Identify the decision** — the "given these facts, do what?" kernel inside
   the entangled code.
2. **Define its inputs and outputs as plain data.** Time enters as an
   `Instant` parameter — the pure code *never reads the clock*
   (`settle_tick` in `src/wm.rs` takes `now` as a parameter).
3. **Extract** into a pure fn/struct with unit tests (value/byte equality, no
   sleeps, no GUI, no Session).
4. **Thin the shell**: `show()`/`pump()` supply live state, call the seam,
   apply the returned side effects in order.
5. **Name the seam in CONTEXT.md** — the glossary is the registry of
   deliberate seams (house style: **foreman-docs-and-writing**; the full seam
   map: **foreman-architecture-contract**).

**Worked examples (all live):**

| Seam (CONTEXT.md name) | Pure code | Signature shape |
|---|---|---|
| Input-encoding seam | `src/input.rs` `process_input` | egui events + term mode + has-selection → `InputOutcome` (bytes + side-effect flags) |
| Caret (gate retired 2026-07-15) | `src/caret.rs` `draw` | model cursor (line, col, shape) → what to paint |
| Outbox | `src/chat.rs` `ChatRoom::tick` | project tag + live Member presence → `Vec<Delivery>` |
| Quiescence settle | `src/wm.rs` `settle_tick` | (gen, quiet_since, deadline, window, now) → (gen, since, done) |
| Cell metrics | `src/geom.rs` `CellMetrics` | pane rect + cell size → all pixel↔cell conversions |
| Paint plan | `src/frame.rs` `plan_paint` (with `text_rows`, `overlays`) | grid + Cell metrics + colors → paint plan |
| Ready gate | `src/ready.rs` `ReadyGate` | injected bytes + Ready state + clock → queue or flush |

The pattern's payoff is visible in the test census: `rg -c '#\[test\]' src/ |
sort -t: -k2 -rn` — the pure modules carry the tests, and they run without a
window or a PTY.

**Done when:** the decision has GUI-free, Session-free, sleep-free unit tests,
and the shell contains no branching logic of its own beyond visibility/paint
policy.

**Evidence bar:** the seam's tests enumerate its contract (see Recipe 9), and
CONTEXT.md names it.

---

## Recipe 4 — Predict-then-measure performance analysis

**When:** any performance work. The rule: **state the mechanism and the
predicted numbers before measuring.** A measurement without a prior prediction
can rationalize anything.

**Steps:**

1. Write down the mechanism hypothesis and what number it predicts.
2. Instrument (throwaway harness patterns and how-to-measure live in
   **foreman-diagnostics-and-tooling**).
3. Measure; compare to the prediction; explain any delta.
4. **Record negative results.** A refuted hypothesis is a decision — it goes
   in the do-not-re-litigate record (**foreman-change-control**), with the
   measurement attached.

**Worked example — the latency investigation (2026-06-18,
`docs/followups-latency-and-control.md`).**

- **Prediction:** the app repainted on a fixed 16 ms
  `request_repaint_after` metronome, and Windows' ~15.6 ms default timer
  granularity floors any shorter request — so every keystroke echo should
  carry ~16–32 ms of added latency, independent of GPU/vsync.
- **Confirmed:** the fix (adaptive cadence, commit `1accc46` — hot tick for
  250 ms after input/PTY output/Dispatch, 100 ms idle, with the PTY reader
  threads' immediate `ctx.request_repaint()` ~0.2 ms as the true fast path)
  removed the floor.
- **Negative result, measured:** vsync off vs on made typing feel identical —
  vsync was *not* the mechanism, and it stays at default (on) to avoid
  tearing/GPU-spin risk. Recorded as a don't-re-litigate decision.
- **Measured capacity numbers (dated 2026-06-18):** idle ~0.13 ms/frame; one
  max-rate flood ~0.8 ms; 12 simultaneous floods ~8 ms avg / 11 ms max —
  render cost is parse-bound (per actively-outputting Session), not
  draw-bound (per cell).

**Done when:** the writeup shows prediction → measurement → verdict in that
order, with the setup described well enough to re-run.

**Evidence bar:** numbers with dates and the measurement setup; quote prior
measured numbers as dated facts rather than re-measuring casually.

---

## Recipe 5 — Timeout-budget analysis

**When:** adding or changing any timeout, settle window, retry, or deadline on
a request path. Timeouts nest; an inner wait that can outlive its outer
deadline makes the outer layer lie to its caller.

**Steps:**

1. Enumerate **every** timer on the path end to end (client → pipe server →
   GUI frame loop → reply).
2. Write the nesting inequality and prove each link: the inner operation must
   complete (or give up) strictly before the outer layer abandons it.
3. Check drop-stale logic: who discards work the other side already gave up
   on?
4. Document the ripple for the *next* timer someone adds.

**Ground truth:**

| Timer | Value | Where | Role |
|---|---|---|---|
| `Settings::send_settle_ms` | 120 ms default, **user-editable**, clamped ≤ 2000 by `Settings::sanitize` | `src/config.rs` | default Quiescence settle window for `foreman send` when the caller omits `--settle-ms` |
| `MAX_SETTLE_MS` | 4000 ms | `src/wm.rs` | hard cap on the total settle wait, applied whatever the caller or the setting asked for |
| `REPLY_TIMEOUT` | 5 s | `src/control.rs` | pipe server waits this long for the GUI; the GUI drops any queued request older than this (`WindowManager::handle_ctrl`) so it never executes work the client was already told failed |
| `CONNECT_TIMEOUT` | 10 s | `src/control.rs` | client-side deadline for connecting to a busy pipe |

**The invariant:** `send_settle_ms (≤ 2000) < MAX_SETTLE_MS (4000) <
REPLY_TIMEOUT (5 s) < CONNECT_TIMEOUT (10 s)`.

Link by link: the clamp keeps a hand-edited setting from reaching the cap; the
cap stays under `REPLY_TIMEOUT` so the pipe server's `recv_timeout` never fires
before a settle reply lands; and a client waiting to connect must outwait a
server that may spend up to `REPLY_TIMEOUT` on the request ahead of it.

**Read the first row again — that is this recipe's own worked example.** The
settle default used to be a compile-time `DEFAULT_SETTLE_MS`. It became a
persisted, user-editable field, which is exactly the "next timer someone adds"
this recipe exists to catch, and the thing that makes it safe is not the
default value but `Settings::sanitize`'s clamp: without it a user typing
`"send_settle_ms": 9999999` into settings.json would push an inner wait past
three outer deadlines. **A knob that a human can type into is a timer.** When
you promote a constant to a setting, the clamp is not polish, it is the proof.

**The documented ripple (open — designed, not built):** Phase 4's
`snapshot --wait-for PATTERN --timeout-ms N` is deliberately deferred across
frames and carries its own deadline, so it **must be exempted from the
`REPLY_TIMEOUT` stale-drop** (and `serve()` must `recv_timeout` on the
inspection's own ceiling) — otherwise every wait longer than `REPLY_TIMEOUT`
silently vanishes (`docs/epics/terminal-inspection-epic.md`). Anyone building
`--wait-for` re-runs this recipe first.

**Legacy-doc warning:** anything describing the pipe server as handling "one
connection at a time" predates the thread-per-connection rewrite (`15f675f`,
2026-06-18): `serve()` now spawns a thread per connection with a `MAX_INFLIGHT`
cap. The terminal-inspection epic's "Serial-pipe blocking" constraint predates
it too. `CONNECT_TIMEOUT`'s rationale (bounded wait when the server is wedged)
still holds.

**Done when:** the inequality chain is written down with each timer's file +
symbol and one sentence per link proving why inner < outer.

**Evidence bar:** the chain plus a test per boundary where practical — the
settle tests in `src/wm.rs` prove it fires at the `MAX_SETTLE_MS` deadline and
not before the configured `send_settle_ms`; the stale-drop tests prove
abandoned requests are never executed; `sanitize_clamps_hand_edited_values`
(`src/config.rs`) proves the outermost user-facing link.

---

## Recipe 6 — Coordinate-system proofs

**When:** writing any pixel↔cell, grid-index, or mouse-protocol math. Before
the first line of arithmetic, write down the *space* of every input and
output: origin, 0- or 1-based, axis order, and the clamp story.

**The spaces in this codebase:**

| Space | Convention | Where defined |
|---|---|---|
| Viewport cells | 0-based `(row, col)`, row 0 = top of visible area | `CellMetrics::cell_at`, `src/geom.rs` |
| Mouse protocol | **1-based `(col, row)` — column first** (SGR/X10 wire order) | `CellMetrics::mouse_cell`, `src/geom.rs` |
| Grid/buffer | `Line` goes negative into scrollback; viewport row → grid line is `Line(row - display_offset)` | `src/frame.rs`, `src/terminal.rs` |

**Steps:**

1. Name each function's input/output space in its doc comment.
2. Write the clamp story: what happens out of bounds. In this repo the clamp
   is load-bearing twice over — `CellMetrics` clamps pointer positions into
   the grid so a drag leaving the pane still resolves (`src/geom.rs`), and
   `frame::plan_paint` clamps its grid walk to the grid's *real* dims because a
   stale `grid[Line][Column]` index panics, and a panic across the winit
   callback aborts the whole process (`src/frame.rs` module docs).
3. If two functions read the same pointer, add a **cross-agreement test** so
   the two readings can never drift apart.

**Worked example A — the `cell_at` / `mouse_cell` asymmetry (live).** Both
convert pointer → cell, but `mouse_cell` is 1-based and column-first because
the mouse *protocols* speak that order — the asymmetry is protocol-driven,
not accidental. The cross-agreement test
`mouse_cell_is_cell_at_plus_one_in_col_row_order` (`src/geom.rs`) pins them
together, clamps included.

**Worked example B — the selection-v1 failure (resolved 2026-07-02).**
`docs/terminal-selection.md` recounts v1's failure mode: selection endpoints
stored as screen-space `(row, col)` broke under scroll — the highlight stayed
pinned to the screen while text moved, and the copy path read through the
*current* scroll offset so highlight and copy disagreed. That failure story
is the lesson: state the space first. The buffer-coordinate rewrite the doc
described was fiction for three weeks (no committed revision had it) until
`b581240` (2026-07-02) landed it: selection now lives in
`alacritty_terminal`'s buffer-space `Selection` (`Selection::new`/`update`,
`to_range`), `sel_anchor`/`sel_head`/`selection_text` are deleted, and the
seam boundary demonstrates the recipe — `sel_point` converts
viewport→buffer on the way in, `sel_viewport_range` converts buffer→viewport
on the way out, so `frame::SelRange` stays deliberately viewport-space while
storage is buffer-space. Every crossing names its space; that is the "state the
space first" rule, enforced at a seam.

**Done when:** every function involved names its space in its doc comment,
the clamp behavior is unit-tested, and paired readers have a cross-agreement
test.

**Evidence bar:** the space table written before the math; tests at the
boundaries (first pixel of a cell, last cell, both out-of-range corners — see
`src/geom.rs`'s test module).

---

## Recipe 7 — Race root-causing (no fix-by-retry)

**When:** a test is flaky, or a live behavior fails intermittently. The rule:
no fix-by-retry, no test serialization, no sleep-tuning. A real root cause is
**one mechanism that explains both the failure and every pass** — if your
theory can't explain why it ever passed, it's not the mechanism.

**Steps:**

1. Characterize the pass/fail pattern precisely (isolation vs full suite,
   load, timing).
2. Find the mechanism in code — the specific lossy or ordered step.
3. Check the mechanism predicts the pass pattern too.
4. Fix **at the mechanism**, not at the schedule.
5. Explicitly record the rejected hide-the-race fixes and why.

**Worked example — the flaky chat broadcast test
(`docs/plans/2026-06-11-fix-flaky-chat-broadcast-test.md`).**

- **Pattern:** `human_post_appends_with_reserved_id_and_broadcasts_to_all_members`
  failed nearly every full parallel run, passed in isolation.
- **Mechanism:** bytes injected into a Session before it is **Ready** are
  eaten by the startup DSR scan. The test injected once, immediately after
  spawn. A deferred submit `\r` fired on a fixed 150 ms timer regardless of
  readiness.
- **It explains every pass:** in isolation the DSR resolves well under
  150 ms, so the deferred `\r` landed post-Ready and rescued the test. Under
  full-suite load (dozens of concurrent conhost spawns) the DSR resolved
  late, *both* writes were eaten, and nothing was ever re-sent.
- **Fix at the mechanism, in layers:** (a) the test re-sends until the
  member's stdin has seen it (the 2026-06-11 plan); (b) the product fix —
  `inject_input` queues bytes until Ready latches, `pump()` flushes
  (commit `6ad7f64`, `src/terminal.rs`; the gate now lives in `src/ready.rs`);
  (c) the Outbox's per-Member delivery cursors only advance on delivery to a
  Ready Session, after which the test needs no re-send at all — it just pumps
  and `chat_tick()`s until both members exit, and a sibling test in `src/wm.rs`
  notes "the cursor + ready-gating make a re-send unnecessary".
- **Explicitly rejected:** serializing the suite — it would merely keep DSR
  latency under the 150 ms timer, hiding the race, and the same swallow
  existed in production (a chat post to a just-Dispatched Member).

**Done when:** you can write "fails under X because M; passes under Y because
M" with the same M, and the fix changes M's behavior, not the schedule.

**Evidence bar:** the mechanism cited to file + symbol; pre-fix failure
reproduced; post-fix consecutive full-suite green runs recorded as the flake
evidence. Flaky-test policy: **foreman-validation-and-qa**.

---

## Recipe 8 — Wire-compat proof

**When:** adding or changing any field in a Control plane request/reply
(`src/control.rs`). The CLI client and the GUI can be different builds
talking over the same pipe, so the protocol evolves only by *addition that is
invisible when unused*.

**The contract:** a new optional field must (a) vanish from the wire when
unset — serde `skip_serializing_if` — so an untargeted/plain message stays
**byte-identical** to the v1 form, and (b) default cleanly when absent —
serde `default` — so old JSON still parses.

**The proof is three asserts in one test:**

1. Serialize the unset form; assert the new key is **absent** from the JSON
   string (byte-identity to v1).
2. Parse a *literal* v1 JSON string (no new key); assert the default.
3. Round-trip the set form.

**Worked examples (all in `src/control.rs` tests;
`rg -n "wire_compat|omits_none" src/control.rs` lists the current set):**

| Test | Guards |
|---|---|
| `chat_request_to_is_wire_compatible_with_v1` | `--to` targets field |
| `chat_request_re_is_wire_compatible` | `--re` handshake back-pointer |
| `chat_history_request_is_wire_compatible_without_from` | optional `from` on history reads |
| `send_request_omits_none_and_empty_fields` | send verb's optional fields |
| `snapshot_request_without_tail_is_wire_compat_with_v1` | `--tail N` added without moving the v1 request |
| `snapshot_reply_without_attrs_cursor_is_wire_compat` | reply stays a v1 `OpenReply` without opt-ins |

The field-level doc comments in `src/control.rs` carry the same rule ("skipped
on the wire when None so v1 replies stay byte-identical").

**Done when:** all three asserts exist for every new field, with the v1 form
as a literal string in the test (not re-serialized by current code — that
would prove nothing).

**Evidence bar:** byte-identity asserted on the serialized string, not
struct equality. Operating the verbs themselves: **foreman-run-and-operate**
(developers) / **foreman-dispatch**, **foreman-chat** (agents inside foreman).

---

## Recipe 9 — Transition-table contract testing

**When:** a behavior is a small state machine — state × input → state. Don't
scatter one test per interesting case; enumerate the **full cross-product in
one test**, as a literal table, one assert per cell, labeled so a regression
names its cell.

**Steps:**

1. Make the machine pure first (Recipe 3).
2. Write the table as data: rows = current states, columns = inputs, cells =
   expected results.
3. Loop-assert with a message carrying `"{state:?} + {input}"`.
4. If a cell is impossible, say so explicitly in the test rather than
   omitting it silently.

**Worked example — historical: `compose_zone`.** The keyboard edge/corner
snap machine (a horizontal pin × vertical pin per axis) was tested by
`compose_zone_matches_full_transition_table`, whose asserted cells mirrored the
design table cell for cell — every state × every direction, none omitted
(`git show e438a83:src/wm.rs`). The code
and test were deleted in `f3c76f0` (2026-06-11) when zone snapping was
replaced by the Layout tree — **the code died; the method survives.**

**Worked example — retired but exemplary: the Caret gate** (deleted
2026-07-15 when the caret moved to direct model-cursor tracking; see
docs/cursor-rendering.md). Its `cursor_to_draw` was a pure policy table with
each row asserted directly, timeline tests driving the time-derived half
through injected `Instant`s, and boundary tests pinning its settle/grace
constants at their exact thresholds — **the code died; the method survives**
(like compose_zone above). `settle_tick`'s tests in `src/wm.rs` enumerate cases
the same way and are still live.

**Done when:** asserted cells = |states| × |inputs| (or exclusions are
explicit), and every assert message names its cell.

**Evidence bar:** the table in the test mirrors the table in the design doc
or module comment — a reviewer can diff them by eye.

---

## When NOT to use this skill

- **A known symptom needs triage now** → **foreman-debugging-playbook** (this
  skill is how you prove a *new* mechanism; that one is the dictionary of
  mechanisms already proven).
- **Any fix a recipe motivates** still goes through **foreman-change-control**
  — these recipes produce evidence, never authorization.

## Provenance and maintenance

The recipes are method and do not rot. The facts they lean on do. Re-check
these before repeating them:

| Claim | Re-verify with |
|---|---|
| The timeout chain (Recipe 5), including the settle clamp | the timeout-chain runs in the block below |
| Stale-drop guard still drops abandoned requests | `rg -n "sent.elapsed" src/wm.rs` |
| `--wait-for` ripple still open (unbuilt) | the `wait_for` run below — any hit means it shipped and this section is stale |
| Wire-compat test inventory (Recipe 8) | the `wire_compat` / `omits_none` runs below |
| Measured latency numbers (2026-06-18) and ConPTY facts | read `docs/followups-latency-and-control.md`, `docs/conpty-resize-reflow.md` |

Run these as written. **`rg` uses Rust regex, not BRE — an alternation is a
bare `|`.** Writing `\|` (as `git grep` needs) makes `rg` match a *literal*
pipe, which matches nothing and reports a silent false pass:

```
# timeout chain
rg -n "send_settle_ms" src/config.rs src/wm.rs
rg -n "MAX_SETTLE_MS" src/wm.rs
rg -n "REPLY_TIMEOUT" src/control.rs
rg -n "CONNECT_TIMEOUT" src/control.rs

# --wait-for ripple: no output = still unbuilt
rg -n "wait_for|wait-for" src/control.rs

# wire-compat test inventory
rg -n "wire_compat" src/control.rs
rg -n "omits_none" src/control.rs
```
