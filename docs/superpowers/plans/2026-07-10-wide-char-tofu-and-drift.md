# Wide-Char Tofu + Drift Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the `�` (U+FFFD) tofu corruption during hold-Backspace over emoji, and collapse the four scattered wide-cell classification sites into one so the next alacritty flag is a one-edit change.

**Architecture:** Evidence first — a decisive experiment pins whether PSReadLine or foreman's key-doubling corrupts the buffer (they produce different observable signatures). Then a pure-seam consolidation (`CellWide::from_flags`) that is correct under either outcome, then the fix branch the evidence selects. The doubling policy lives in `src/input.rs` (pure, unit-tested); grid walks live in `frame.rs`/`inspect.rs`/`terminal.rs`.

**Tech Stack:** Rust, alacritty_terminal 0.26, egui 0.34, `foreman send`/`snapshot` control plane for headless evidence.

## Global Constraints

- Kill the running foreman exe before building, or the link fails with `Access is denied (os error 5)` — **unless `$env:FOREMAN` is `1`** (you are inside foreman): then build with `cargo build --target-dir target/agent` and never `Stop-Process foreman`.
- GNU toolchain (`stable-gnu`), w64devkit linker. Never MSVC.
- `cargo test` must stay green after every task; run `cargo test --lib input` / `--lib frame` / `--lib inspect` for the touched modules.
- Wire compat v1: no changes to control-plane JSON shapes in this plan.
- Commit style: `type(scope): subject`, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Stage files by name.
- Do not re-open the settled ConPTY resize/reflow fence (`docs/conpty-resize-reflow.md`). Task 1 may *observe* ConPTY residue rows; it does not fix them.

## Background (read before Task 1)

Reproduced symptom (2026-07-10, pwsh 7.5.8 in foreman): prompt line of `🥒🤣…`, hold Backspace → `�` (U+FFFD) cells appear at the line tail and on rows below the prompt. `�` = a lone UTF-16 surrogate half hit ConPTY's UTF-8 re-render, i.e. **half a surrogate pair was deleted in the child's buffer**. Two candidate causes with different signatures:

| Hypothesis | Who is wrong | Signature |
|---|---|---|
| **A** | foreman: `wide_key_doubles` sends 2×DEL per Backspace, but PSReadLine deletes one *whole* emoji per DEL → over-delete corrupts | one foreman Backspace press removes **two** emoji; Windows Terminal never shows `�` |
| **B** | PSReadLine/conhost: cooked editing is UTF-16-unit-based → one DEL removes half a pair; doubling was correct but desyncs across frames during hold-repeat | Windows Terminal shows the **same** `�` corruption; one foreman press removes exactly one emoji |

Current doubling seam (all in `src/input.rs`): `CellWide` (:37), `wide_hint_at` (:62), `wide_key_doubles` (:~75), `encode_key_wide` (:93), `clear_wide_pair`/`apply_wide_key_to_line` (:134-181), consumed by `process_input_wide` (:209) and `inspect::parse_keys_wide` (inspect.rs:286). Known structural gaps regardless of branch: no `ALT_SCREEN` gate; per-frame grid resample is stale for the whole PTY round-trip during key repeat; Ctrl/Alt chords bypass doubling and desync it.

---

### Task 1: Decisive experiment — who corrupts the buffer?

**Files:**
- Create: `docs/wide-chars.md` (evidence section only; ownership doc completed in Task 5)
- No source changes.

**Interfaces:**
- Produces: a recorded verdict — `BRANCH A` (foreman over-deletes) or `BRANCH B` (PSReadLine unit-based) — written into `docs/wide-chars.md`. Task 3 consumes this verdict.

**Needs the user for the Windows Terminal step (foreman steps are headless).**

- [ ] **Step 1: Build and launch foreman**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
Start-Process target\debug\foreman.exe
```

Expected: build OK, GUI opens. Have the user open a project with one PowerShell terminal (or dispatch one), then get ids:

```powershell
target\debug\foreman.exe status
```

Expected: JSON/text listing the project and a terminal id like `t1`. Use that id as `tN` below.

- [ ] **Step 2: Baseline — Windows Terminal (user, manual)**

Ask the user to run in **Windows Terminal**, pwsh 7.5.8: paste `🥒🤣🤣🤣`, then (a) press Backspace once — record whether exactly one whole 🤣 disappears; (b) retype and **hold** Backspace to empty the line — record whether any `�` ever appears.

Record both observations verbatim in `docs/wide-chars.md` under `## Evidence 2026-07-10`.

- [ ] **Step 3: foreman — single Backspace press**

```powershell
target\debug\foreman.exe send --terminal tN --text "🥒🤣🤣🤣" --settle-ms 300
target\debug\foreman.exe snapshot --terminal tN
target\debug\foreman.exe send --terminal tN --keys "Backspace" --settle-ms 300
target\debug\foreman.exe snapshot --terminal tN
```

Expected before: prompt line ends `🥒🤣🤣🤣`. Record after-state:
- Two emoji gone → **BRANCH A confirmed** (doubling over-deletes).
- Exactly one gone, no `�` → consistent with B (doubling matched a 2-unit need).
- Half gone / `�` present → PSReadLine mishandled even a single doubled press; still A-family (over/under mismatch).

- [ ] **Step 4: foreman — hold-repeat simulation (cross-frame staleness)**

```powershell
target\debug\foreman.exe send --terminal tN --text "🥒🤣🤣🤣🥒🤣🤣🤣" --settle-ms 300
# Back-to-back sends with NO settle: each send samples the grid before the
# previous send's echo lands — same staleness as hold-repeat frames.
target\debug\foreman.exe send --terminal tN --keys "Backspace"
target\debug\foreman.exe send --terminal tN --keys "Backspace"
target\debug\foreman.exe send --terminal tN --keys "Backspace"
target\debug\foreman.exe send --terminal tN --keys "Backspace"
target\debug\foreman.exe snapshot --terminal tN --attrs
```

Record: any `�` in the snapshot text, and (from `--attrs`) whether the tofu cells carry wide/spacer flags or are plain narrow cells. Also note whether `�` residue appears on rows *below* the prompt (ConPTY not clearing unwrapped rows — observe only, do not fix; check whether `Ctrl+L` heals it via `send --keys` if convenient).

- [ ] **Step 5: Write the verdict**

In `docs/wide-chars.md`, under `## Evidence 2026-07-10`, record the full matrix (WT single, WT hold, foreman single, foreman burst) and one line: `Verdict: BRANCH A` or `Verdict: BRANCH B`, with the observation that decided it.

- [ ] **Step 6: Commit**

```powershell
git add docs/wide-chars.md
git commit -m @'
docs(wide-chars): record hold-Backspace corruption experiment

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 2: One home for wide-cell classification — `CellWide::from_flags`

Correct under either branch (paint and snapshots need spacer classification even if key-doubling is later removed). Kills the df46b2d drift class: today the spacer predicate is duplicated in `frame.rs:110` (`is_wide_spacer`), `inspect.rs:93`, `inspect.rs:154`, and `terminal.rs:~1002`.

**Files:**
- Modify: `src/input.rs` (add `impl CellWide` right after the `CellWide` enum, ~line 44)
- Modify: `src/frame.rs:108-112` (`is_wide_spacer` body)
- Modify: `src/inspect.rs:90-97` and `src/inspect.rs:151-158` (both snapshot walks)
- Modify: `src/terminal.rs:~999-1010` (CellWide row sampling)
- Test: `src/input.rs` tests module

**Interfaces:**
- Produces: `pub fn CellWide::from_flags(flags: alacritty_terminal::term::cell::Flags) -> CellWide`. All later tasks and future flag additions go through this one function.

- [ ] **Step 1: Write the failing test** (in `src/input.rs` `mod tests`, next to the existing `wide_hint_at_*` tests)

```rust
#[test]
fn cellwide_from_flags_is_the_single_classification_home() {
    use alacritty_terminal::term::cell::Flags;
    assert_eq!(CellWide::from_flags(Flags::empty()), CellWide::Narrow);
    assert_eq!(CellWide::from_flags(Flags::WIDE_CHAR), CellWide::WideBase);
    assert_eq!(
        CellWide::from_flags(Flags::WIDE_CHAR_SPACER),
        CellWide::WideSpacer
    );
    // The df46b2d lesson: wrap placeholders are spacers too.
    assert_eq!(
        CellWide::from_flags(Flags::LEADING_WIDE_CHAR_SPACER),
        CellWide::WideSpacer
    );
    // Style flags must not affect classification.
    assert_eq!(
        CellWide::from_flags(Flags::BOLD | Flags::WIDE_CHAR),
        CellWide::WideBase
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib input cellwide_from_flags`
Expected: FAIL — `no function or associated item named 'from_flags'`.

- [ ] **Step 3: Implement** (insert after the `CellWide` enum in `src/input.rs`)

```rust
impl CellWide {
    /// The one home for wide-cell classification. Every grid walk that cares
    /// about width-2 glyphs — paint plan, snapshot text/cells, key-hint
    /// sampling — classifies through this, so a new alacritty spacer flag is
    /// one edit here (see df46b2d: LEADING_WIDE_CHAR_SPACER needed 4 edits).
    pub fn from_flags(flags: alacritty_terminal::term::cell::Flags) -> Self {
        use alacritty_terminal::term::cell::Flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            CellWide::WideSpacer
        } else if flags.contains(Flags::WIDE_CHAR) {
            CellWide::WideBase
        } else {
            CellWide::Narrow
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --lib input cellwide_from_flags`
Expected: PASS.

- [ ] **Step 5: Route all four call sites through it**

`src/frame.rs` — replace the body of `is_wide_spacer` (keep the local fn; callers unchanged):

```rust
/// Spacer halves of width-2 glyphs — never paint as their own cell (shows as
/// tofu □). Classification delegated to the single home in input::CellWide.
fn is_wide_spacer(flags: Flags) -> bool {
    crate::input::CellWide::from_flags(flags) == crate::input::CellWide::WideSpacer
}
```

`src/inspect.rs` — in **both** `snapshot_text` and `snapshot_cells`, replace

```rust
if cell
    .flags
    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
{
```

with

```rust
if crate::input::CellWide::from_flags(cell.flags) == crate::input::CellWide::WideSpacer {
```

(If `Flags` becomes unused in an import list after this, remove it from that list.)

`src/terminal.rs` — in the wide-row sampling (the `line.push(if f.contains(Flags::WIDE_CHAR) { ... })` block near line 999), replace the whole if/else push with:

```rust
let f = g[p.line][alacritty_terminal::index::Column(c)].flags;
line.push(CellWide::from_flags(f));
```

- [ ] **Step 6: Full test run**

Run: `cargo test`
Expected: all green — the existing `plan_paint_skips_leading_wide_char_spacer`, snapshot, and wide-hint tests prove behavior is unchanged.

- [ ] **Step 7: Commit**

```powershell
git add src/input.rs src/frame.rs src/inspect.rs src/terminal.rs
git commit -m @'
refactor(input): CellWide::from_flags is the one spacer-classification home

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 3: CHECKPOINT — pick the fix branch

**Not a coding task.** Read the `Verdict:` line from `docs/wide-chars.md` (Task 1).

- Verdict `BRANCH A` → execute **Task 4A**, skip Task 4B.
- Verdict `BRANCH B` → execute **Task 4B**, skip Task 4A.
- Evidence ambiguous → stop and review with the user; do not guess.

---

### Task 4A: (Branch A) Remove key doubling — PSReadLine owns grapheme editing

The doubling was compensating for what turned out to be the paint bug fixed in `df46b2d`, and it actively corrupts the child's buffer. Remove the policy; keep `CellWide` (paint/snapshot classification now depends on it via Task 2) and keep all paint-side fixes.

**Files:**
- Modify: `src/input.rs` — delete `wide_key_doubles`, `encode_key_wide`, `col_after_wide_key`, `clear_wide_pair`, `apply_wide_key_to_line`, `wide_hint_at`, `WideCursorHint`; simplify `process_input_wide` back to `process_input` (single signature, no `wide_cursor` param); delete the doubling tests, keep encode_key tests.
- Modify: `src/terminal.rs` — delete the cursor-row `CellWide` sampling in `show()` and call plain `process_input`.
- Modify: `src/inspect.rs` — delete `parse_keys_wide`, keep `parse_keys`.
- Modify: `src/wm.rs` — `send --keys` path calls `parse_keys` again.
- Test: existing suites.

**Interfaces:**
- Consumes: `CellWide::from_flags` (Task 2) — **stays**, used by frame/inspect/terminal walks.
- Produces: `process_input(events, mods, mode, has_selection) -> InputOutcome` as the only entry point (pre-be4901b shape).

- [ ] **Step 1: Write the regression test first** (in `src/input.rs` tests — this is the contract that Backspace is never doubled again)

```rust
#[test]
fn backspace_is_always_a_single_del() {
    // PSReadLine (and every grapheme-correct editor) deletes one full glyph
    // per DEL. Doubling corrupts surrogate pairs — see docs/wide-chars.md.
    let out = process_input(
        &[key_ev(Key::Backspace, none())],
        Modifiers::default(),
        TermMode::empty(),
        false,
    );
    assert_eq!(out.pty_bytes, [0x7f]);
}
```

- [ ] **Step 2: Delete the doubling seam**

In `src/input.rs`: remove `WideCursorHint`, `wide_hint_at`, `wide_key_doubles`, `encode_key_wide`, `col_after_wide_key`, `clear_wide_pair`, `apply_wide_key_to_line`, and the `wide_cursor` parameter of `process_input_wide`; fold the body into `process_input` (the key arm becomes `let seq = encode_key(k, m, mode); out.pty_bytes.extend_from_slice(&seq);`). Delete `process_input_wide` and every test whose name contains `wide` **except** `cellwide_from_flags_is_the_single_classification_home` and any `plan_paint`/snapshot tests (those are paint-side). Keep the `CellWide` enum + `from_flags`.

In `src/terminal.rs` `show()`: delete the `let wide = { ... }` cursor-row sampling block and call `crate::input::process_input(&i.events, i.modifiers, mode, has_selection)`.

In `src/inspect.rs`: delete `parse_keys_wide` and its tests; `parse_keys` stays. In `src/wm.rs`: the send-keys path calls `parse_keys(names, mode)`.

- [ ] **Step 3: Build + full test run**

Run: `cargo test`
Expected: green. Compiler errors are the checklist — every remaining reference to a deleted symbol must be resolved by simplification, not by keeping the symbol.

- [ ] **Step 4: Live re-verification (the original Task 1 recipe)**

Repeat Task 1 Steps 3-4 against the rebuilt exe. Expected: one Backspace press removes exactly one emoji; the burst leaves **zero** `�`. Append the result to `docs/wide-chars.md`.

- [ ] **Step 5: Commit**

```powershell
git add src/input.rs src/terminal.rs src/inspect.rs src/wm.rs docs/wide-chars.md
git commit -m @'
fix(input): remove wide-key doubling; child editors own grapheme deletes

Doubling 2x DEL per Backspace corrupted PSReadLine's UTF-16 buffer during
hold-repeat (lone surrogates -> U+FFFD tofu). Evidence: docs/wide-chars.md.
Paint-side spacer skips (df46b2d) stay; CellWide classification stays.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 4B: (Branch B, AMENDED 2026-07-10 from probe evidence) Per-codepoint doubling + persistent shadow

Probes proved conhost cooked editing is **UTF-16-unit-based for Backspace/arrows but grapheme-based for Delete**, and unit count ≠ cell width: emoji (non-BMP, 2 units) need doubling; CJK (BMP, 1 unit) must NOT be doubled (current code over-deletes/over-moves them). The hold-Backspace `�` corruption is the cross-frame hole: each frame re-samples a stale grid and restarts the shadow simulation. Fix in three parts:

1. **Double iff the crossed wide glyph is non-BMP** (`ch > U+FFFF`) — never for BMP CJK.
2. **Delete never doubles** (grapheme-aware in PSReadLine — doubled `CSI 3~` eats 1.5 emoji).
3. **Persist the shadow line across frames** while the child's echo is pending (`output_gen` unchanged); re-sample the live grid only after new child output. Plus the `ALT_SCREEN` gate (vim/lazygit are grapheme-correct everywhere).

**Files:**
- Modify: `src/input.rs` — `CellWide::WideBase` gains payload `{ non_bmp: bool }`; classification takes the cell char; `WideCursorHint` gains `left_glyph_non_bmp`/`at_glyph_non_bmp`; `wide_key_doubles` gains `mode` + new rule; `InputOutcome` gains `wide_after: Option<(Vec<CellWide>, usize)>`.
- Modify: `src/terminal.rs` — `Session.wide_shadow: Option<(Vec<CellWide>, usize, u64)>`; sample-or-reuse in `show()`.
- Modify: `src/inspect.rs` — `parse_keys_wide` threads `mode` (already has it) and the new `CellWide` shape.
- Modify: `src/wm.rs` — send-keys path samples with chars (already samples flags; add `cell.c`).
- Test: `src/input.rs` tests.

**Interfaces:**
- Consumes: the single classification home from Task 2 — its signature becomes `CellWide::classify(flags, ch)` in this task (Task 2's `from_flags` is renamed/extended here; spacer-ness unchanged).
- Produces: `CellWide::{Narrow, WideBase{non_bmp: bool}, WideSpacer}`; `wide_key_doubles(key, mods, mode, wide) -> bool`; `InputOutcome.wide_after`; `Session.wide_shadow`.

- [ ] **Step 1: Write the failing tests** (replace `hint_base`/`hint_spacer`/`hint_after_wide` helpers with emoji/CJK variants)

```rust
#[test]
fn backspace_after_emoji_doubles_but_after_cjk_stays_single() {
    // Emoji = surrogate pair = 2 UTF-16 units in conhost's buffer → 2 DELs.
    // CJK = BMP = 1 unit → 1 DEL. Evidence: docs/wide-chars.md 2026-07-10.
    let emoji = [CellWide::WideBase { non_bmp: true }, CellWide::WideSpacer];
    let cjk = [CellWide::WideBase { non_bmp: false }, CellWide::WideSpacer];
    assert_eq!(
        encode_key_wide(Key::Backspace, none(), TermMode::empty(), wide_hint_at(&emoji, 2)),
        vec![0x7f, 0x7f]
    );
    assert_eq!(
        encode_key_wide(Key::Backspace, none(), TermMode::empty(), wide_hint_at(&cjk, 2)),
        vec![0x7f]
    );
}

#[test]
fn delete_never_doubles() {
    // PSReadLine forward-Delete is grapheme-aware; doubling eats 1.5 emoji.
    let emoji = [CellWide::WideBase { non_bmp: true }, CellWide::WideSpacer];
    assert_eq!(
        encode_key_wide(Key::Delete, none(), TermMode::empty(), wide_hint_at(&emoji, 0)),
        b"\x1b[3~".to_vec()
    );
}

#[test]
fn alt_screen_never_doubles() {
    let emoji = [CellWide::WideBase { non_bmp: true }, CellWide::WideSpacer];
    assert_eq!(
        encode_key_wide(Key::ArrowLeft, none(), TermMode::ALT_SCREEN, wide_hint_at(&emoji, 2)),
        b"\x1b[D".to_vec()
    );
}

#[test]
fn outcome_returns_shadow_for_cross_frame_persistence() {
    // Hold-repeat: Session must carry the mutated shadow line into the next
    // frame while echo is pending, not re-sample the stale grid.
    let line = vec![
        CellWide::WideBase { non_bmp: true },
        CellWide::WideSpacer,
        CellWide::WideBase { non_bmp: true },
        CellWide::WideSpacer,
    ];
    let out = process_input_wide(
        &[key_ev(Key::Backspace, none())],
        Modifiers::default(),
        TermMode::empty(),
        false,
        Some((&line, 4)),
    );
    assert_eq!(out.pty_bytes, vec![0x7f, 0x7f]);
    let (after_line, after_col) = out.wide_after.expect("shadow returned");
    assert_eq!(after_col, 2);
    assert_eq!(after_line[2], CellWide::Narrow); // deleted pair cleared
    assert_eq!(after_line[3], CellWide::Narrow);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib input`
Expected: FAIL — `WideBase` has no field `non_bmp`, no `wide_after`.

- [ ] **Step 3: Implement**

`CellWide` + classification (extends Task 2's home; update its Task-2 call sites — paint/snapshot only test spacer-ness, so give them `is_spacer`):

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellWide {
    #[default]
    Narrow,
    /// Base cell of a width-2 glyph. `non_bmp` = needs a surrogate pair in
    /// UTF-16 (conhost's cooked buffer deletes/moves per unit → 2 keys).
    WideBase { non_bmp: bool },
    WideSpacer,
}

impl CellWide {
    /// The one home for wide-cell classification (flags + base char).
    pub fn classify(flags: alacritty_terminal::term::cell::Flags, ch: char) -> Self {
        use alacritty_terminal::term::cell::Flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            CellWide::WideSpacer
        } else if flags.contains(Flags::WIDE_CHAR) {
            CellWide::WideBase { non_bmp: ch > '\u{FFFF}' }
        } else {
            CellWide::Narrow
        }
    }
    pub fn is_spacer(self) -> bool {
        self == CellWide::WideSpacer
    }
    fn is_base(self) -> bool {
        matches!(self, CellWide::WideBase { .. })
    }
}
```

`WideCursorHint` carries the glyph class per direction (`wide_hint_at` fills them: at/right glyph base = `line[col]` if base else `line[col-1]` when on spacer; left glyph base = `line[col-2]` when `left_is_spacer` else `line[col-1]` when on spacer):

```rust
pub struct WideCursorHint {
    pub on_wide_base: bool,
    pub on_wide_spacer: bool,
    pub left_is_spacer: bool,
    /// The wide glyph a Left/Backspace would cross/remove is non-BMP.
    pub left_glyph_non_bmp: bool,
    /// The wide glyph a Right would cross is non-BMP.
    pub at_glyph_non_bmp: bool,
}
```

`wide_key_doubles` — the amended policy, one function:

```rust
pub fn wide_key_doubles(key: Key, mods: Modifiers, mode: TermMode, wide: WideCursorHint) -> bool {
    // Alt-screen TUIs (vim, lazygit) edit per-grapheme; never compensate.
    if mode.contains(TermMode::ALT_SCREEN) {
        return false;
    }
    let ctrl = mods.ctrl || mods.command;
    if ctrl || mods.alt {
        return false;
    }
    match key {
        // conhost moves per UTF-16 unit: only surrogate-pair glyphs need 2.
        Key::ArrowRight => (wide.on_wide_base || wide.on_wide_spacer) && wide.at_glyph_non_bmp,
        Key::ArrowLeft | Key::Backspace => {
            (wide.left_is_spacer || wide.on_wide_spacer) && wide.left_glyph_non_bmp
        }
        // PSReadLine forward-Delete is grapheme-aware (evidence 2026-07-10).
        Key::Delete => false,
        _ => false,
    }
}
```

Thread `mode` through `encode_key_wide` (already has it), `col_after_wide_key`, `apply_wide_key_to_line`. In `apply_wide_key_to_line`, Delete still clears the whole pair in the shadow (grapheme delete) but emits one sequence. `process_input_wide` returns the working buffer: add `pub wide_after: Option<(Vec<CellWide>, usize)>` to `InputOutcome`, set it at the end when `wide_line` is `Some`.

`src/terminal.rs` — `Session` field + sample-or-reuse:

```rust
/// Shadow cursor row for wide-key encoding, persisted while the child's
/// echo is pending. (line, col, output_gen at sample time). Re-sample the
/// live grid only when output_gen has advanced past the stored gen —
/// re-sampling a stale grid restarts the simulation and corrupts
/// hold-repeat bursts (the 2026-07-10 tofu).
wide_shadow: Option<(Vec<CellWide>, usize, u64)>,
```

In `show()`: if `self.wide_shadow` is `Some((_, _, gen))` and `gen == self.output_gen`, use the stored (line, col); otherwise sample the grid row with `CellWide::classify(cell.flags, cell.c)` at the live cursor. After `process_input_wide`, if `outcome.wide_after` is `Some((line, col))` and any bytes were sent, store `self.wide_shadow = Some((line, col, self.output_gen))`; if no bytes were sent, leave as-is. Clear `wide_shadow` to `None` whenever `output_gen` advances (checked at the top of the sample step by the `gen` comparison — a stale entry is simply not reused and gets overwritten).

`src/wm.rs` send-keys sampling adds `cell.c` to the classify call; `src/inspect.rs` `parse_keys_wide` signature unchanged apart from the `CellWide` shape.

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: PASS. Existing doubling tests that assumed CJK-doubles must be updated to non-BMP fixtures — deleting an assertion is only allowed where the *behavior* is now proven wrong (CJK doubling, Delete doubling); note each in the commit body.

- [ ] **Step 5: Live re-verification (full matrix)**

Rebuild, then repeat all Task 1 probes plus: `中中中` + one Backspace → exactly one `中` gone; `中中` + one Left → cursor −2 cols; emoji burst (4 rapid `send --keys "Backspace"` with no settle) → zero `�` in the final snapshot. Append results to `docs/wide-chars.md`.

- [ ] **Step 6: Commit**

```powershell
git add src/input.rs src/terminal.rs src/inspect.rs src/wm.rs docs/wide-chars.md
git commit -m @'
fix(input): wide-key doubling keys on surrogate pairs, not cell width

conhost cooked editing is UTF-16-unit-based for BS/arrows (emoji need 2
keys, BMP CJK needs 1 - doubling over-deleted CJK) and grapheme-based for
Delete (doubling ate 1.5 emoji). Shadow row now persists across frames
while echo is pending; re-sampling a stale grid restarted the simulation
and corrupted hold-Backspace bursts. Evidence: docs/wide-chars.md.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

### Task 5: Ownership doc — `docs/wide-chars.md`

**Files:**
- Modify: `docs/wide-chars.md` (created in Task 1)

**Interfaces:** none — documentation.

- [ ] **Step 1: Complete the doc** (grug-brain style; keep the Evidence section from Task 1/4 verbatim). Cover:

```markdown
# Wide characters (CJK / emoji)

What: how foreman handles width-2 glyphs (one WIDE_CHAR base cell + one
spacer cell) across paint, snapshots, and key input.

Ownership rules:
- Paint NEVER renders spacer cells (either spacer flag). One classifier:
  `CellWide::from_flags` in src/input.rs. New alacritty flag = edit there.
- Snapshot text/cells skip spacers through the same classifier.
- Key input: [Branch A: foreman sends exactly one sequence per keypress;
  the child editor owns grapheme deletion. Do NOT reintroduce doubling —
  it corrupted PSReadLine's UTF-16 buffer (see Evidence). |
  Branch B: doubling applies ONLY on the primary screen, only for
  unmodified ←/→/BS/Del, and only when the child's echo is quiescent.]

Gotchas:
- U+FFFD (�) in the grid = someone deleted half a surrogate pair in the
  child's buffer. That is buffer corruption upstream of foreman's renderer;
  foreman paints it faithfully.
- ConPTY may leave � residue on rows below the prompt after heavy
  unwrapping; Ctrl+L heals. Same family as the settled resize-reflow
  divergence (docs/conpty-resize-reflow.md) — do not chase.

Key files: src/input.rs (CellWide + key policy), src/frame.rs (paint skip),
src/inspect.rs (snapshot skip), src/terminal.rs (cursor-row sampling).

## Evidence 2026-07-10
[matrix + verdict from Task 1, re-verification from Task 4]
```

- [ ] **Step 2: Cross-link** — add one line to the wide-char row of the sequences table in `.claude/skills/terminal-emulation-reference/SKILL.md` pointing at `docs/wide-chars.md` (and keep `.codex` skill copies in sync if they mention wide chars).

- [ ] **Step 3: Commit**

```powershell
git add docs/wide-chars.md .claude/skills/terminal-emulation-reference/SKILL.md
git commit -m @'
docs(wide-chars): ownership rules for wide-cell paint/snapshot/input

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
'@
```

---

## Self-review notes

- Task 2 is branch-independent by construction (classification is needed for paint/snapshot even if doubling dies) — safe to run before the checkpoint.
- Task 4A keeps `CellWide` + `from_flags` even while deleting the doubling seam; verified no type referenced in 4A/4B is undefined elsewhere.
- Type consistency: `wide_key_doubles(key, mods, mode, wide)` in 4B matches the threading instructions; 4A's `process_input` signature matches the existing public one.
- Deliberately out of scope: ConPTY residue rows below the prompt (upstream, Ctrl+L heals), emoji stamp rendering, and any control-plane JSON change.
