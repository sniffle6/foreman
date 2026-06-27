# Terminal Inspection — Epic

**Status:** **Phases 1–3 built & green (2026-06-26), 249 tests passing, reviewed
clean.** The core feedback loop works: `foreman send` (with cross-frame quiescence
settle) + `foreman snapshot` (text). Remaining: Phase 4 opt-ins
(`--attrs`/`--cursor`/`--region`/`--wait-for`/`--since-seq`, plus the
`REPLY_TIMEOUT` exemption that long `--wait-for` needs) and Phase 5 (dogfood).
Spec produced via codebase-design *design-it-twice* (three parallel interface
explorations → the hybrid below). Built on the `src/input.rs` encoder seam from
the terminal-completeness epic (Session A).

## Why it exists

Foreman can't currently be *driven and read back* without the GUI. To verify a
terminal change you must screenshot the window and look — and to exercise input
(press F1 in vim) you must send real keystrokes, which hijacks the user's
keyboard. So an agent (or an automated test, or the user's CI) has **no closed
feedback loop**: it can't say "send these keys, now show me what's on screen."

This layer adds that loop over the existing control plane: **`foreman send`**
drives input into a terminal, **`foreman snapshot`** reads its rendered screen as
data. Foreman is uniquely placed to offer this — it's a terminal *with a local
control plane already*. It is also a real product feature: a scriptable,
inspectable terminal that agents running *inside* foreman use to self-verify, and
a stepping stone to the daemon/headless-core split on the roadmap. Crucially, it
makes the parts that are hard to unit-test today (end-to-end key delivery, the
egui render path) finally assertable.

**Read first:** `docs/epics/agent-dispatch-epic.md` (the control plane this
extends), `CONTEXT.md` (domain glossary), and `src/control.rs` / `src/input.rs`.

---

## The design — two verbs: `send` + `snapshot`

The design-it-twice explorations landed on one combined verb (drive+read folded),
three verbs (drive / read / wait split apart), and a common-caller verb (trivial
self-target default). The hybrid keeps the strongest idea from each:

- **Two verbs, not one or three.** `send` (drive) and `snapshot` (read) are
  genuinely distinct capabilities — folding them into one verb makes "just send"
  or "just read" an awkward sub-case (you must suppress a snapshot you don't
  want). Splitting `wait` into its own third verb is the opposite error: waiting
  is "snapshot, but block until a condition," i.e. a flag on `snapshot`, not a
  verb. Two deep, single-purpose verbs; the common loop is two clean one-liners.
- **`send` settles by default (quiescence).** After writing input the screen is
  stale. `send` blocks until the child has gone quiet (~120 ms of no new PTY
  bytes, capped) before replying, so a following `snapshot` reads settled state.
  The common caller never thinks about timing. `--settle-ms 0` disables it,
  `--settle-ms N` extends it.
- **`snapshot` returns plain text by default**, in the `history` field the agent
  already knows from `status` — greppable, small, zero new wire surface for the
  90% case. Structured detail (`--attrs`, `--cursor`, `--region`) is opt-in.
- **Self-target via env**, exactly like bare `foreman close`: `--terminal` /
  `--project` default to `FOREMAN_TERMINAL_ID` / `FOREMAN_PROJECT_ID`, so an agent
  inspects its own terminal with zero addressing.

### Interface

**`foreman send [--project P] [--terminal T] [--text TXT] [--keys "K K …"] [--settle-ms N]`**

Writes input to terminal T (default: your own). `--text` is raw UTF-8 written
verbatim (`\r` = Enter). `--keys` is a space-separated sequence of named key
presses, encoded through `crate::input::encode_key` with the session's live
`TermMode` — the *same* path the GUI uses, so encoding never diverges. Text and
keys are additive: text first, then keys (`--text "ls" --keys "Tab Enter"`).
Replies `{ok:true}` once settled, or `{ok:false,error:…}`.

```
SendRequest {
  cmd: "send",
  project: Option<String>,   // pN; None → FOREMAN_PROJECT_ID
  terminal: Option<String>,  // tN; None → FOREMAN_TERMINAL_ID (self)
  text: Option<String>,
  keys: Vec<String>,         // skip_serializing_if empty
  settle_ms: Option<u64>,    // None → DEFAULT_SETTLE (~120ms); 0 → no wait; cap 5000
}
```

**`foreman snapshot [--project P] [--terminal T] [--rows N] [--attrs] [--cursor] [--region R] [--wait-for PATTERN --timeout-ms N] [--since-seq S]`**

Reads terminal T's rendered viewport. Default reply: plain text rows in
`OpenReply.history`, one string per visible row, trailing spaces trimmed — the
agent prints/greps it like `status`. Opt-in additions are new optional
`OpenReply` fields (wire-compat via `skip_serializing_if`):

- `--attrs` → `cells: Vec<Vec<CellData>>` (per-cell fg/bg/bold/dim/italic/
  underline/strikethrough/inverse/wide). Pay for it only when asked.
- `--cursor` → `cursor: {row, col, shape}` from `renderable_content().cursor`.
- `--region '{row,col,rows,cols}'` / `--rows N` bound what's serialized (an 80×24
  full-attr grid is ~28 KB of JSON; bound it).
- `--wait-for PATTERN [--timeout-ms N]` blocks until PATTERN (substring; `--regex`
  variant later) appears in the grid or the deadline passes; reply carries
  `matched: bool`. For the "I know what success looks like" case.
- `--since-seq S` → if the grid generation hasn't advanced past S, reply
  immediately with no snapshot (the escape hatch for non-blocking client-side
  polling — see the pipe-blocking constraint below). The reply stamps the current
  generation in `seq`.

```
SnapshotRequest {
  cmd: "snapshot",
  project: Option<String>, terminal: Option<String>,   // same self-target rule
  rows: Option<u32>, region: Option<Region>,
  attrs: bool, cursor: bool,                            // skip_serializing_if false
  wait_for: Option<String>, timeout_ms: Option<u64>,
  since_seq: Option<u64>,
}
```

**Key-name grammar** (`--keys`): readable, `Ctrl+`/`Alt+`/`Shift+` prefixes
(combinable), space-separated. Names: `F1`..`F12`, `Up Down Left Right`,
`Home End PageUp PageDown Insert Delete`, `Enter Tab Esc Backspace`, and single
letters/digits. Example: `--keys "Escape F1 Alt+b Ctrl+C"`. Unknown name → exit 2.

**Errors / exit codes** (matching the existing verbs): unknown terminal →
`ok:false`, exit 1; bad flag / unknown key → exit 2; `--wait-for` timeout →
`ok:true, matched:false` with the (stale) snapshot still attached.

---

## The seam — `src/inspect.rs` (the deep, GUI-free module)

All three explorations converged here, and it's right. One new pure module holds
the logic; `control.rs` and the GUI are thin adapters over it. **The interface is
the test surface** — tested in-process with no pipe and no GUI.

```
// pure: walk the grid, no egui, no PTY, no control plane
pub fn snapshot_text(term: &Term<L>, region: Option<Region>) -> Vec<String>
pub fn snapshot_cells(term: &Term<L>, region: Option<Region>) -> Vec<Vec<CellData>>
pub fn cursor_info(term: &Term<L>) -> CursorInfo
pub fn grid_contains(term: &Term<L>, pattern: &str) -> bool

// pure: key-name strings → PTY bytes, via the input.rs encoder
pub fn parse_keys(names: &[String], mode: TermMode) -> Result<Vec<u8>, String>
```

**Behind the seam:** grid-walking with wide-char handling (skip
`WIDE_CHAR_SPACER`, advance two columns for `WIDE_CHAR`) and bounds clamping —
mirror `terminal.rs::selection_text`, which already solved the alt-screen/resize
shrink hazard (do not re-open it). Attribute extraction reuses `glyph_style`'s
flag reads. `parse_keys` maps each name → `(egui::Key, egui::Modifiers)` via a
lookup table, then calls `input::encode_key(key, mods, mode)` — so `send --keys`
and the live keyboard share one encoder.

**Testing (in-process, dependency category = in-process per DEEPENING.md):**
construct a bare `Term` and drive it with VTE bytes through a `Processor`, then
assert on the snapshot — no `Session`, no PTY, no window.
- `snapshot_text` returns exact rows for known content; region clamps, never panics.
- `snapshot_cells` reports underline/inverse/etc. for styled bytes.
- `parse_keys`: `"Ctrl+C"` → `[0x03]`, `"Up"` → `ESC[A`, `"F5"` → `ESC[15~`,
  `"Alt+b"` → `[0x1b,b'b']`; unknown name → `Err`. (Byte-equality, like the
  `input.rs` tests.)
- > Gotcha: the project rule "**never use `VoidListener`**" is about *live* PTY
  > sessions (the DSR trap — a real shell hangs without the reply pump). It does
  > NOT apply to these test fixtures, which advance a parser over fixed bytes and
  > only read the grid — no PTY, no DSR. Use a trivial test listener there.

The `control.rs` adapter is shallow: two request structs, two `CtrlMsg` variants,
parse arms in `serve()`, `parse_send_args`/`parse_snapshot_args`, `send_main`/
`snapshot_main`, HELP text. Its pipe-roundtrip test pattern (already used for
open/chat/status/close) extends directly.

---

## The crux — settle/wait must run ACROSS frames, not block

This is the implementation insight the raw explorations skated past. The GUI is
single-threaded egui: it drains `CtrlMsg` and `pump()`s every session each frame
(~16 ms). A `send` that blocks the frame loop for 120 ms of quiescence, or a
`snapshot --wait-for` that blocks for seconds, would **freeze the UI** — and you
can't move the `Session` to a worker thread (it's owned by the `WindowManager`).

So settle/wait is a **stateful, multi-frame operation**, in the spirit of the
existing deferred-action (`Act`) pattern:

- The App holds a **pending-inspections** list: each entry is
  `(request, reply_sender, deadline, last_change_seen)`.
- An inspection that needs to settle/wait is pushed to this list instead of
  replying immediately; one that needs neither (a plain `snapshot`, or `send
  --settle-ms 0`) replies same-frame.
- **Each frame**, after the normal `pump()`, advance every pending entry: did the
  target session produce new output this frame? If yes, reset its silence timer
  (a per-session grid-generation counter, bumped in `pump()` when new bytes
  arrive, is the cheap signal). When silence ≥ threshold (for `send`), or the
  pattern matches (for `snapshot --wait-for`), or the deadline passes — run the
  snapshot via `inspect.rs` and fire the `OpenReply`.

**Ripple — the `REPLY_TIMEOUT` stale-drop must exempt inspections.** Today the
GUI drops any `CtrlMsg` older than 5 s (`control.rs` `REPLY_TIMEOUT`) because the
server already told that client "foreman did not respond." Inspections are
*intentionally* deferred across frames and carry their own `--timeout-ms`, so they
must be exempt from the 5 s drop and honored up to their own ceiling (cap, e.g.,
60 s). The pipe `serve()` side must likewise `recv_timeout` on the inspection's
own deadline, not the 5 s default. **This is the one change that touches the
existing timing model — get it right or waits silently vanish.**

---

## Constraints & ripple effects (designed-in, not discovered later)

- **Serial-pipe blocking.** The pipe handles one connection at a time. A long
  `snapshot --wait-for` (or big `--settle-ms`) holds the pipe for its duration,
  stalling *other* control-plane callers (dispatch, chat). Mitigation: keep
  blocking waits bounded and short by default; for long waits use the
  non-blocking pattern — `snapshot --since-seq S` returns instantly when nothing
  changed, so a client polls cheaply instead of holding the connection. Document
  this in HELP. (A future multiplexed pipe is out of scope.)
- **Wire-compat.** `send` reply adds no fields. `snapshot` adds `cells`,
  `cursor`, `matched` as optional `OpenReply` fields with
  `skip_serializing_if`; `seq` (already present) doubles as the grid generation.
  v1 readers ignore unknowns; v1 writers never set them. Add roundtrip tests like
  the existing `*_omits_none_fields` ones.
- **Security (same threat model, stated).** `snapshot` exposes a terminal's
  contents to any local process that can reach the pipe — and `send` injects
  input into any terminal. This is the existing model (the control plane is "a
  guardrail against confused agents, NOT a security boundary," per `control.rs`),
  but inspection sharpens it: a local process can now *read* what your agents see
  and *type* into them. Note it in the docs; real auth is the IPC-hardening item
  in the market-viability review, tracked separately.
- **Self-target precedent.** Resolve self-target exactly as `parse_close_args`
  does (env both required to name a `tN`, which is only unique within its project).

---

## Build order (TDD-friendly, each gate is an agent-run `cargo test`)

1. **`src/inspect.rs` pure fns + tests** — `snapshot_text`, `snapshot_cells`,
   `cursor_info`, `grid_contains`, `parse_keys`. Test-first against a
   byte-driven `Term`. No control/GUI yet. (in-process seam; highest value first.)
2. **`control.rs` wire** — `SendRequest`/`SnapshotRequest`, `CtrlMsg::Send/
   Snapshot`, `serve()` arms, `parse_*_args`, `*_main`, HELP; reply fields on
   `OpenReply`. Pipe-roundtrip + parse + wire-compat tests.
3. **GUI integration (`wm.rs`/`main.rs`)** — the pending-inspections list, the
   per-frame settle/wait advance, the grid-generation counter in `pump()`, and
   the `REPLY_TIMEOUT` exemption. The hardest part; review against the landmines.
4. **Opt-ins** — `--attrs`/`--cursor`/`--region`/`--rows`, then `--wait-for`/
   `--timeout-ms`, then `--since-seq`.
5. **Dogfood** — replace a manual acid-test from the terminal-completeness epic
   with a scripted `send` + `snapshot` assertion (e.g. send `F5` to `cat -v`,
   snapshot, assert `^[[15~` is on screen). This is the payoff: the loop tests
   itself.

## Definition of done

- [ ] `foreman send --text "echo hi\r"` then `foreman snapshot` shows `hi` (self-target, settled).
- [ ] `foreman send --keys "F5"` into `cat -v` + `snapshot` shows `^[[15~` — end-to-end key delivery, finally asserted.
- [ ] `snapshot --attrs` reports underline/inverse cells; `--cursor` reports shape.
- [ ] `snapshot --wait-for "PASS" --timeout-ms 30000` blocks then returns matched — and does NOT get dropped by the 5 s stale-drop.
- [ ] A long wait doesn't freeze the UI (settle runs across frames).
- [ ] All logic unit-tested through `inspect.rs`; control verbs pipe-roundtrip tested; wire-compat tests green.

## Key files

- `src/inspect.rs` (NEW) — the pure inspection seam: snapshot grid-walk, cell/cursor extraction, `parse_keys` (→ `input::encode_key`) + tests.
- `src/control.rs` — `send`/`snapshot` request structs, `CtrlMsg` variants, parse/serve/client/HELP; new optional `OpenReply` fields.
- `src/wm.rs` / `src/main.rs` — pending-inspections list, per-frame settle/wait advance, grid-generation counter, `REPLY_TIMEOUT` exemption.
- `src/input.rs` — reused as-is: `encode_key` is the key-encoding depth.
- `src/terminal.rs` — `Session` grid access + `pump()`/`send()`; `selection_text` is the grid-walk precedent.

---

## Appendix — the three explorations (design-it-twice)

- **A · minimal** — one `probe` verb folding send→wait→snapshot; plain text;
  `wait_for` as the settle. Strength: max leverage per call. Cost: conflates
  drive+read; "just send" must discard a snapshot.
- **B · flexible** — three verbs `send`/`snapshot`/`wait`; structured `CellData`
  with an `attrs` projection mask, region+scrollback, `since_seq`. Strength:
  surfaced the real constraints (pipe-wedge, JSON size, the `since_seq` poll
  escape). Cost: verb sprawl; `wait` is really a snapshot flag.
- **C · common-caller** — one `inspect` verb, self-target by env, **150 ms
  quiescence settle by default**, plain text in `history`. Strength: the trivial
  case is a zero-flag one-liner. Cost: same drive+read conflation as A.

**Hybrid taken:** B's two-primitive instinct (`send`+`snapshot`) minus the third
verb, + C's quiescence-by-default (moved onto `send`) and env self-target and
text-by-default reply, + A/B's `wait-for` and B's `since_seq` as opt-in modifiers
on `snapshot`, + the cross-frame settle model (mine) that none specified.
