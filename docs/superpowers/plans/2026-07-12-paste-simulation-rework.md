# Paste-Simulation Rework Implementation Plan

> **EXECUTED 2026-07-12 with one deviation.** Task 6's kill-switch fired on
> the first S2 replay and produced **v4** — the shipped phase machine splits
> a failed Accepted recheck: cursor still on `p_col` with a broken
> fingerprint → **Dead permanently**; cursor off `p_col` (intermediate CUP)
> → **Seeking**, re-acceptance allowed. Task 5's embedded code/test below
> show the pre-v4 permanent-Dead design and are SUPERSEDED;
> `src/paste_sim.rs::phase_machine_transition_table` is authoritative.
> Evidence: results doc § "Task 0 gate" (S2 strike 1; S4 frozen before v4,
> re-run green).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace c0bdb7c's CUP-interception machinery (`CupScanner`) with the dual-simulation paste alias (predicate v3), validated through the Task 0 gate before any production wiring.

**Architecture:** A new deep module `src/paste_sim.rs` owns the P/E simulations, predicate v3, the mode sniffer, and the Seeking→Accepted→Dead phase machine behind two entry points (`try_arm`, `observe_chunk`). `Session` keeps its session-level gates and maps phases onto the existing `psreadline_cursor_alias`/`_gen` fields. The scanner is deleted only after the replay gate and live tests are green.

**Tech Stack:** Rust, `alacritty_terminal` 0.26 (`Term`, `Flags`, `Point`), `unicode-width` 0.2.2 (already a dep), `serde_json` (already a dep) for fixture sidecars. **No new dependencies** (foreman-change-control gate).

**Spec:** `docs/superpowers/specs/2026-07-12-paste-simulation-rework-design.md` (read it first — every constraint below is normative there). Evidence: `2026-07-12-paste-sim-phase0-{protocol,results}.md`.

## Global Constraints

- Build loop: kill running foreman first unless `$env:FOREMAN` is `1` (then use `--target-dir target/agent`). Crate is **bin-only** — no `--lib`.
- Sacred: DSR/CPR flush-after-exact-chunk cadence in `pump_at`; Ready gating; `Session::resize`; no wire/control-plane changes.
- Live tests are `#[ignore]`d, release-mode, need pwsh on PATH: `cargo test --release <name> -- --ignored --nocapture`.
- Commit style: `type(scope): subject`, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Stage files by name.
- Zero cost when unarmed. Alias consumers (`input_cursor_point`, `display_cursor_point`, `effective_cursor_info`, shadow seeding) must not change.
- Payload arming cap: `MAX_SIM_CELLS = 4096` expected cells (matches the existing 4096 wrap-walk bounds; adversarial benchmark in Task 7 justifies or shrinks it).
- Frozen prediction model: `docs/superpowers/specs/phase0_predict.py` — do not edit; extend in a new file.
- **Commits are CONDITIONAL.** Implementation authorization is not commit
  authorization. Default is ONE aggregate commit at the end (the unit the
  user approved); per-task commit steps below execute only if the user has
  explicitly authorized per-task commits. Otherwise treat each "Commit" step
  as "verify `git status` is clean of unintended files and move on".
- Never `Stop-Process foreman` when `$env:FOREMAN` is `1` — you would kill
  your own host. Use `cargo ... --target-dir target/agent` instead.
- Module visibility: only `PasteSim`, `Phase`, `try_arm`, `observe_chunk`
  are `pub(crate)`. `SimPlan`, `ExpectedCell`, `plan`, `accept`, `ModeSniff`
  stay private (tests live in-module and see them).

---

### Task 1: Fixture sidecars + re-capture (spec Task 0 step 0)

**Files:**
- Modify: `src/terminal.rs` (tests module — `phase0_probe`, added this branch, search for `fn phase0_probe`)
- Create: `tests/fixtures/phase0/*.bin` + `*.json` (regenerated)
- Modify: `docs/superpowers/specs/2026-07-12-paste-sim-phase0-results.md` (new hashes appendix)

**Interfaces:**
- Produces: sidecar JSON schema consumed by Task 6's replay test:
  `{ "cols": usize, "rows": usize, "pre": {"row": i32, "col": usize, "input_needs_wrap": bool, "history": usize}, "post": {"raw_row": i32, "raw_col": usize, "input_needs_wrap": bool, "history": usize}, "final_grid": [String], "modes_alt_or_bracketed": bool, "provenance": {"commit": String, "shell": String} }`
  where `final_grid[r]` is the canonical row dump: per cell `format!("{}|{}{}{};", ch, W?, S?, L?)` with `W`=WIDE_CHAR, `S`=WIDE_CHAR_SPACER, `L`=LEADING_WIDE_CHAR_SPACER (flag letters present iff set).

- [ ] **Step 1: Add PSReadLine hardening + sidecar emission to `phase0_probe`**

In `phase0_probe`, change the `-Command` argv element to disable prediction/history:

```rust
format!(
    "Set-PSReadLineOption -PredictionSource None -HistorySaveStyle SaveNothing; function global:prompt {{ '{prompt}' }}"
),
```

At the end of `phase0_probe` (after the framed-capture write), add:

```rust
        let canon_row = |r: usize| -> String {
            let mut s = String::new();
            for c in 0..vcols {
                let cell = &g[Line(r as i32)][Column(c)];
                s.push(cell.c);
                s.push('|');
                if cell.flags.contains(Flags::WIDE_CHAR) { s.push('W'); }
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) { s.push('S'); }
                if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) { s.push('L'); }
                s.push(';');
            }
            s
        };
        let final_grid: Vec<String> = (0..vlines).map(canon_row).collect();
        let sidecar = serde_json::json!({
            "cols": vcols, "rows": vlines,
            "pre": { "row": pre_point.line.0, "col": pre_point.column.0,
                     "input_needs_wrap": pre_wrap, "history": pre_hist },
            "post": { "raw_row": raw.line.0, "raw_col": raw.column.0,
                      "input_needs_wrap": pending, "history": hist },
            "final_grid": final_grid,
            "modes_alt_or_bracketed": session.term.mode()
                .intersects(TermMode::ALT_SCREEN | TermMode::BRACKETED_PASTE),
            "oracle": { "actual_end": actual_end, "one_past": one_past,
                        "leading_pads": leading_pads, "wrapline_rows": wrap_rows },
            "term_mode_bits": format!("{:?}", session.term.mode()),
            "provenance": {
                "commit": option_env!("PHASE0_COMMIT").unwrap_or("unset"),
                "shell": shell,
                // captured once per run via std::process::Command:
                //   pwsh -NoProfile -Command
                //   "$PSVersionTable.PSVersion.ToString(); (Get-Module PSReadLine -ListAvailable)[0].Version.ToString(); [System.Environment]::OSVersion.Version.ToString()"
                "pwsh": pwsh_version, "psreadline": psrl_version, "os": os_version,
            },
            "paste_start_frame": paste_start_frame,
        });
        let jpath = std::env::temp_dir().join(format!("phase0-{name}.json"));
        std::fs::write(&jpath, serde_json::to_string_pretty(&sidecar).unwrap()).unwrap();
        println!("PHASE0|{name}|sidecar|path={}", jpath.display());
```

Note: `raw`/`pending`/`hist` are captured **before** the oracle borrows `g`; move the `let g = session.term.grid();` for the oracle *after* this block if the borrow checker complains — the sidecar block needs `g` too, so capture `raw`/`pending`/`hist` first, then take one `g` borrow for oracle + sidecar together.

- [ ] **Step 2: Rebuild and re-capture all three scenarios**

```powershell
if (-not $env:FOREMAN) { Get-Process foreman -ErrorAction SilentlyContinue | Stop-Process -Force }
$env:PHASE0_COMMIT = (git rev-parse --short HEAD)
cargo test --release phase0_s1_primary -- --ignored --nocapture
cargo test --release phase0_s2_holdout -- --ignored --nocapture
cargo test --release phase0_s3_control -- --ignored --nocapture
```
Expected: three `PHASE0|...|sidecar|path=...` lines; `post` values match the recorded Phase 0 measurements (raw (10,35) / (10,6) / (10,3)).

- [ ] **Step 3: Install fixtures + record hashes**

```powershell
Copy-Item "$env:TEMP\phase0-s1-primary.*","$env:TEMP\phase0-s2-holdout.*","$env:TEMP\phase0-s3-control.*" tests\fixtures\phase0\
Get-FileHash tests\fixtures\phase0\* -Algorithm SHA256
```
Append the new full hashes to the results doc under a "Re-capture (sidecar generation)" heading, noting the old hashes are superseded and S2 is no longer blind (P already validated; v3 blindness comes from Task 6's S4).

- [ ] **Step 4: Commit**

```powershell
git add src/terminal.rs tests/fixtures/phase0 docs/superpowers/specs/2026-07-12-paste-sim-phase0-results.md
git commit -m "test(paste-sim): fixture sidecars + hardened re-capture (Task 0.0)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `paste_sim` module — the two simulations and arm-domain gates

**Files:**
- Create: `src/paste_sim.rs`
- Modify: `src/main.rs` (add `mod paste_sim;` next to the existing `mod terminal;` line)

**Interfaces:**
- Produces (consumed by Tasks 4/5/8):

```rust
pub const MAX_SIM_CELLS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedCell {
    Base(char), // wide glyph base: char + WIDE_CHAR required
    Spacer,     // trailing wide spacer: WIDE_CHAR_SPACER required
    Pad,        // leading deferral pad: char ' ' only (F2 scrub — flags don't-care)
    Narrow(char), // width-1 char incl. payload spaces: char only
}

pub struct SimPlan {
    pub cols: usize,              // armed width; a resize/width change fails closed
    pub p_col: usize,
    pub delta: usize,             // E_flat - P_flat, > 0
    pub expected: Vec<ExpectedCell>,
}

/// Pure arm-domain gate + simulation. `None` = fail closed (do not arm).
pub fn plan(cols: usize, start_col: usize, start_pending: bool, text: &str) -> Option<SimPlan>;
```

- [ ] **Step 1: Write the failing tests**

Create `src/paste_sim.rs` with the module doc and the tests first:

```rust
//! PSReadLine paste-simulation compatibility (spec:
//! docs/superpowers/specs/2026-07-12-paste-simulation-rework-design.md).
//! Deep module: simulations, predicate v3, mode sniffer, phase machine.
//! Evidence: the Phase 0 protocol/results docs; fixtures in
//! tests/fixtures/phase0/.

#[cfg(test)]
mod tests {
    use super::*;

    // Frozen-model fixtures (phase0_predict.py; validated live in Phase 0).
    #[test]
    fn s1_primary_plan_matches_frozen_model() {
        let text = "\u{1F952}\u{1F923}\u{1F923}\u{1F923} ".repeat(48);
        let p = plan(40, 3, false, &text).expect("arms");
        assert_eq!((p.p_col, p.delta), (35, 4));
        assert_eq!(p.expected.len(), 436); // E_flat 439 - start 3
    }

    #[test]
    fn s2_holdout_plan_matches_frozen_model() {
        let text = "ab\u{1F923}\u{1F952}cd\u{1F923} ".repeat(30);
        let p = plan(33, 6, false, &text).expect("arms");
        assert_eq!((p.p_col, p.delta), (6, 1));
        assert_eq!(p.expected.len(), 331); // E_flat 337 - start 6
    }

    #[test]
    fn p_equals_e_does_not_arm() {
        assert!(plan(40, 3, false, &"abcd ".repeat(80)).is_none());
    }

    #[test]
    fn arm_domain_fails_closed() {
        let ok = "\u{1F952}\u{1F923}\u{1F923}\u{1F923} ".repeat(48);
        assert!(plan(40, 3, false, "a\tb").is_none(), "tab is control");
        assert!(plan(40, 3, false, "a\u{0085}b").is_none(), "C1 control");
        assert!(plan(40, 3, false, "a\u{200D}b").is_none(), "ZWJ zero-width");
        assert!(plan(40, 3, false, "a\u{FE0F}b").is_none(), "variation selector");
        assert!(plan(40, 3, false, "a\u{0301}b").is_none(), "combining mark");
        assert!(plan(40, 3, true, &ok).is_none(), "wrap-pending start unmeasured");
        assert!(plan(40, 3, false, &"\u{1F923}".repeat(3000)).is_none(), "over cap");
        assert!(plan(1, 0, false, &ok).is_none(), "degenerate width");
    }

    #[test]
    fn cjk_at_margin_pins_the_defer_whole_p_model() {
        // 中 is one UTF-16 unit of width 2. At col cols-1 it must defer
        // whole (P x = 2 on the next row), NOT wrap with remainder 1.
        // 36 'a' from col 3 puts 中 at col 39 of a 40-col row.
        let text = format!("{}中b", "a".repeat(36));
        // P: defer-whole → after 中: (row+1, x=2); after 'b': x=3.
        // E: identical here (BMP wide defers in both models) → delta 0 →
        // must NOT arm; the point of this test is pinning simulate_p.
        assert!(plan(40, 3, false, &text).is_none());
        let (x, y) = simulate_p(40, 3, &text).unwrap();
        assert_eq!((x, y), (3, 1), "defer-whole semantics, not remainder");
    }

    #[test]
    fn wrap_pending_endpoint_fails_closed() {
        // 37 narrow cells from col 3 end exactly at the margin (pending) —
        // then one emoji at row start + pad-free tail; construct a payload
        // whose E endpoint is wrap-pending: 36 'a' then one emoji makes
        // E land pending at the last column only if the glyph fills it.
        // 35 'a' (cols 3..37) + emoji at 38-39 -> cursor pins at 39 pending.
        let text = format!("{}\u{1F923}", "a".repeat(35));
        assert!(plan(40, 3, false, &text).is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

```powershell
cargo test paste_sim 2>&1 | Select-Object -Last 5
```
Expected: compile error — `plan` not defined.

- [ ] **Step 3: Implement**

Above the tests module:

```rust
use unicode_width::UnicodeWidthChar;

pub const MAX_SIM_CELLS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedCell {
    Base(char),
    Spacer,
    Pad,
    Narrow(char),
}

pub struct SimPlan {
    pub p_col: usize,
    pub delta: usize,
    pub expected: Vec<ExpectedCell>,
}

/// E: whole-glyph flow, alacritty 0.26 semantics (vendored term/mod.rs
/// input(): lazy pending-wrap before the char; width-2 at the last column
/// pads + wraps; cursor never occupies col == cols).
fn simulate_e(cols: usize, start_col: usize, text: &str) -> Option<(usize, bool, Vec<ExpectedCell>)> {
    let (mut col, mut pending) = (start_col, false);
    let mut cells = Vec::new();
    for ch in text.chars() {
        let w = ch.width()?; // None => unrepresentable, fail closed
        if w == 0 {
            return None; // ZWJ/VS/combining: rejected until they earn a fixture
        }
        if pending {
            col = 0;
            pending = false;
        }
        if w == 2 && col == cols - 1 {
            cells.push(ExpectedCell::Pad);
            col = 0;
        }
        if w == 1 {
            cells.push(ExpectedCell::Narrow(ch));
        } else {
            cells.push(ExpectedCell::Base(ch));
            cells.push(ExpectedCell::Spacer);
        }
        let nc = col + w;
        if nc >= cols {
            col = cols - 1;
            pending = true;
        } else {
            col = nc;
        }
        if cells.len() > MAX_SIM_CELLS {
            return None;
        }
    }
    Some((col, pending, cells))
}

/// P: PSReadLine 2.4.5 per-UTF-16-unit math (issue #1329): a non-BMP char is
/// two width-1 units that may split at the margin; x == cols rolls to
/// (0, y+1) (assumption A1 — validated exact in Phase 0); a width-2 BMP unit
/// that does NOT fit defers whole: y += 1, x = w — NOT the remainder
/// (seventh review; ConvertOffsetToPoint semantics; UNPROVEN live — Phase 0
/// fixtures were ASCII/emoji only, so a wrong model here fails closed via
/// raw != P and only costs CJK coverage).
fn simulate_p(cols: usize, start_col: usize, text: &str) -> Option<(usize, usize)> {
    let (mut x, mut y) = (start_col, 0usize);
    for ch in text.chars() {
        let units: &[usize] = if ch as u32 > 0xFFFF { &[1, 1] } else { &[ch.width()?] };
        for &w in units {
            if x + w > cols {
                y += 1;
                x = w; // defer the whole unit to the next row
            } else {
                x += w;
                if x == cols {
                    y += 1;
                    x = 0;
                }
            }
        }
    }
    Some((x, y))
}

pub fn plan(cols: usize, start_col: usize, start_pending: bool, text: &str) -> Option<SimPlan> {
    if cols < 2 || start_col >= cols || text.is_empty() {
        return None;
    }
    if start_pending {
        return None; // unmeasured; fail closed until it has its own fixture
    }
    if text.chars().any(|c| c.is_control()) {
        return None; // C0, DEL, C1
    }
    let (e_col, e_pending, expected) = simulate_e(cols, start_col, text)?;
    if e_pending {
        return None; // point-only alias cannot represent input_needs_wrap
    }
    let (p_x, p_y) = simulate_p(cols, start_col, text)?;
    if p_x >= cols {
        return None;
    }
    let e_flat = {
        // rows consumed by E = cells laid from start_col in rows of `cols`
        start_col + expected.len()
    };
    let p_flat = p_y * cols + p_x;
    let delta = e_flat.checked_sub(p_flat).filter(|d| *d > 0)?;
    Some(SimPlan { cols, p_col: p_x, delta, expected })
}
```

And in `src/main.rs`, next to the other module declarations:

```rust
mod paste_sim;
```

- [ ] **Step 4: Run tests**

```powershell
cargo test paste_sim 2>&1 | Select-Object -Last 5
```
Expected: all `paste_sim::tests` pass. If `s1`/`s2` expected-length asserts fail, the sim diverges from the frozen model — reconcile against `python docs/superpowers/specs/phase0_predict.py` before touching the test values.

- [ ] **Step 5: Commit**

```powershell
git add src/paste_sim.rs src/main.rs
git commit -m "feat(paste-sim): pure P/E simulations + arm-domain gates

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Mode sniffer

**Files:**
- Modify: `src/paste_sim.rs`

**Interfaces:**
- Produces: `pub(crate) struct ModeSniff` with `fn scan(&mut self, bytes: &[u8]) -> bool` — returns true iff a CSI private mode set/reset for 47/1047/1049/2004 completes in this chunk (state retained across chunks). Allocation-free.

- [ ] **Step 1: Write the failing tests** (append to the tests module)

```rust
    #[test]
    fn mode_sniff_transition_table() {
        // (input bytes, expected hit) — one sniffer per row.
        let cases: &[(&[u8], bool)] = &[
            (b"\x1b[?1049h", true),
            (b"\x1b[?1049l", true),
            (b"\x1b[?47h", true),
            (b"\x1b[?1047l", true),
            (b"\x1b[?2004h", true),
            (b"\x1b[?25l", false),          // cursor visibility: not a barrier
            (b"\x1b[0m", false),            // SGR
            (b"\x1b[11;36H", false),        // CUP
            (b"plain text \xf0\x9f\xa4\xa3", false),
            // UTF-8 continuation bytes include 0x9B (8-bit CSI); an emoji
            // containing one must not trip the sniffer (U+1F6D2 = F0 9F 9B 92)
            (b"\xf0\x9f\x9b\x92", false),
            (b"\x9b?1049h", true), // bare 8-bit CSI barrier
            (b"\x1b[?1049;25h", true),      // multi-param containing a barrier
        ];
        for (bytes, want) in cases {
            let mut s = ModeSniff::default();
            assert_eq!(s.scan(bytes), *want, "bytes {bytes:?}");
        }
    }

    #[test]
    fn mode_sniff_survives_chunk_splits() {
        // Split at every position of an armed barrier sequence.
        let seq = b"\x1b[?1049h";
        for cut in 1..seq.len() {
            let mut s = ModeSniff::default();
            let first = s.scan(&seq[..cut]);
            let second = s.scan(&seq[cut..]);
            assert!(first || second, "split at {cut} missed the barrier");
        }
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test mode_sniff` → compile error.

- [ ] **Step 3: Implement** (above the tests)

```rust
/// Streaming recognizer for CSI ? 47|1047|1049|2004 h/l across chunk
/// boundaries. Deliberately NOT a VTE parser: false positives (e.g. the
/// pattern inside an OSC payload) only lose coverage, which is acceptable;
/// false negatives are backstopped by the post-chunk term-mode check.
#[derive(Default)]
pub(crate) struct ModeSniff {
    state: SniffState,
    param: u32,
    param_hit: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum SniffState {
    #[default]
    Ground,
    Esc,
    Csi,
    Private, // inside CSI ? params
}

impl ModeSniff {
    pub(crate) fn scan(&mut self, bytes: &[u8]) -> bool {
        let mut hit = false;
        for &b in bytes {
            self.state = match (self.state, b) {
                (_, 0x1b) => SniffState::Esc,
                (SniffState::Esc, b'[') => SniffState::Csi,
                // 8-bit CSI (0x9B) — cheaper to recognize than to prove
                // ConPTY never emits it (seventh review)
                (_, 0x9b) => SniffState::Csi,
                (SniffState::Csi, b'?') => {
                    self.param = 0;
                    self.param_hit = false;
                    SniffState::Private
                }
                (SniffState::Private, b'0'..=b'9') => {
                    self.param = self.param.saturating_mul(10) + u32::from(b - b'0');
                    SniffState::Private
                }
                (SniffState::Private, b';') => {
                    self.note_param();
                    self.param = 0;
                    SniffState::Private
                }
                (SniffState::Private, b'h' | b'l') => {
                    self.note_param();
                    if self.param_hit {
                        hit = true;
                    }
                    SniffState::Ground
                }
                (SniffState::Private, _) => SniffState::Ground,
                (SniffState::Csi, _) => SniffState::Ground, // non-private CSI: done
                (SniffState::Esc, _) => SniffState::Ground,
                (SniffState::Ground, _) => SniffState::Ground,
            };
        }
        hit
    }

    fn note_param(&mut self) {
        if matches!(self.param, 47 | 1047 | 1049 | 2004) {
            self.param_hit = true;
        }
    }
}
```

- [ ] **Step 4: Run tests** — `cargo test mode_sniff` → both pass.

- [ ] **Step 5: Commit**

```powershell
git add src/paste_sim.rs
git commit -m "feat(paste-sim): streaming mode-barrier sniffer

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Predicate v3 over a real `Term`

**Files:**
- Modify: `src/paste_sim.rs`

**Interfaces:**
- Consumes: `SimPlan` (Task 2).
- Produces: `fn accept<L: EventListener>(term: &Term<L>, plan: &SimPlan) -> Option<(Point, Point)>` — `(raw, physical)` when all predicate conditions hold. Private to the module (Task 5 wraps it).

- [ ] **Step 1: Write the failing tests** (append; use a byte-fed fixture Term)

```rust
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{test::TermSize, Config, Term};

    fn term_from(cols: usize, rows: usize, bytes: &[u8]) -> Term<VoidListener> {
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(cols, rows),
            VoidListener,
        );
        let mut parser = alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    // NOTE: if TermSize/Processor paths differ at HEAD, copy the imports the
    // existing terminal.rs tests use for VoidListener terms (fn term_with /
    // scan_psreadline_cup) — same construction, one home.

    #[test]
    fn v3_accepts_the_pad_variant_endpoint() {
        // 16 'a' + emoji from col 3 at 20 cols: pad at col 19, emoji wraps,
        // PSReadLine's CUP parks one short (the existing live-test geometry).
        let text = format!("{}\u{1F923}", "a".repeat(16));
        let p = plan(20, 3, false, &text).expect("arms");
        let echo = format!("\x1b[1;4H{text}\x1b[2;2H"); // final CUP = P (row 2 col 2, 1-based)
        let term = term_from(20, 8, format!("P> {echo}").as_bytes());
        let (raw, physical) = accept(&term, &p).expect("accepts");
        assert_eq!((raw.line.0, raw.column.0), (1, 1));
        assert_eq!((physical.line.0, physical.column.0), (1, 2));
    }

    #[test]
    fn v3_rejects_wrong_row_on_periodic_payload() {
        // S2's periodicity: draw only 3 of 10 rows, park the cursor at the
        // per-row P column — mid-echo state; extent must reject.
        let text = "ab\u{1F923}\u{1F952}cd\u{1F923} ".repeat(30);
        let p = plan(33, 6, false, &text).expect("arms");
        let partial: String = text.chars().take(72).collect(); // 3 reps * 8 chars * 3 rows
        let bytes = format!("HOLD> {partial}\x1b[3;7H");
        let term = term_from(33, 14, bytes.as_bytes());
        assert!(accept(&term, &p).is_none());
    }

    #[test]
    fn v3_rejects_stale_residue_at_e_prime_minus_one() {
        let text = format!("{}\u{1F923}", "a".repeat(16));
        let p = plan(20, 3, false, &text).expect("arms");
        // residue 'X' exactly where the fingerprint expects the emoji spacer
        let bytes = format!("P> \x1b[1;4H{}X\x1b[2;2H", "a".repeat(16));
        let term = term_from(20, 8, bytes.as_bytes());
        assert!(accept(&term, &p).is_none());
    }

    #[test]
    fn v3_rejects_nonempty_suffix_after_e_prime() {
        let text = format!("{}\u{1F923}", "a".repeat(16));
        let p = plan(20, 3, false, &text).expect("arms");
        let bytes = format!("P> \x1b[1;4H{text}junk\x1b[2;2H");
        let term = term_from(20, 8, bytes.as_bytes());
        assert!(accept(&term, &p).is_none());
    }

    #[test]
    fn v3_accepts_scrubbed_pads_via_repaint() {
        // Overwrite-in-place repaint scrubs the LEADING pad flag (F2).
        // Predicate must still accept: pad cells are char-only.
        let text = format!("{}\u{1F923}", "a".repeat(16));
        let p = plan(20, 3, false, &text).expect("arms");
        let one = format!("\x1b[1;4H{text}");
        let bytes = format!("P> {one}{one}\x1b[2;2H"); // repaint twice
        let term = term_from(20, 8, bytes.as_bytes());
        assert!(accept(&term, &p).is_some());
    }

    #[test]
    fn v3_rejects_runtime_wrap_pending_cursor() {
        let text = format!("{}\u{1F923}", "a".repeat(16));
        let p = plan(20, 3, false, &text).expect("arms");
        // park the cursor at the last column by filling the row exactly
        let bytes = format!("P> \x1b[1;4H{}", "b".repeat(17));
        let term = term_from(20, 8, bytes.as_bytes());
        assert!(term.grid().cursor.input_needs_wrap);
        assert!(accept(&term, &p).is_none());
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test v3_` → compile error (`accept` undefined).

- [ ] **Step 3: Implement**

```rust
use alacritty_terminal::event::EventListener;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::{cell::Flags, Term};

fn cell_is_empty<L: EventListener>(term: &Term<L>, r: i32, c: usize) -> bool {
    // Match cursor_at_content_end's emptiness exactly (Cell::is_empty), so
    // the arm gate and the acceptance suffix cannot disagree (seventh
    // review). Spacer-flagged cells are non-empty under is_empty; verify
    // that at implementation time and add an explicit flags check ONLY if
    // is_empty proves flag-blind.
    term.grid()[Line(r)][Column(c)].is_empty()
}

/// Predicate v3 (spec "Acceptance"). Returns (raw, physical) on success.
/// The back-fingerprint traverses SIGNED lines into retained history
/// (seventh review: a permitted 4096-cell paste commonly exceeds the
/// viewport); it fails closed when the walk would pass -history_size
/// (content evicted) or when the grid width changed since arming.
fn accept<L: EventListener>(term: &Term<L>, plan: &SimPlan) -> Option<(Point, Point)> {
    let grid = term.grid();
    let cols = grid.columns();
    let lines = grid.screen_lines();
    let raw = grid.cursor.point;
    // 0. runtime wrap-pending raw cursor / width change: fail this chunk
    if grid.cursor.input_needs_wrap || raw.line.0 < 0 || cols != plan.cols {
        return None;
    }
    // 1. O(1) column gate
    if cols < 2 || raw.column.0 != plan.p_col {
        return None;
    }
    // E' = raw advanced delta cells (viewport-signed flat space: flat 0 is
    // viewport row 0 col 0; negative flats reach history)
    let raw_flat = raw.line.0 as i64 * cols as i64 + raw.column.0 as i64;
    let e_flat = raw_flat + plan.delta as i64;
    let (e_row, e_col) = (e_flat.div_euclid(cols as i64), e_flat.rem_euclid(cols as i64) as usize);
    if e_row < 0 || e_row >= lines as i64 {
        return None;
    }
    // 2. suffix from E' to the end of the viewport is empty
    for r in e_row as usize..lines {
        for c in (if r == e_row as usize { e_col } else { 0 })..cols {
            if !cell_is_empty(term, r as i32, c) {
                return None;
            }
        }
    }
    // 3. full-extent back-fingerprint, signed lines, bounded by history
    let history_top = -(grid.history_size() as i64) * cols as i64;
    if e_flat - (plan.expected.len() as i64) < history_top {
        return None; // content evicted from scrollback: fail closed
    }
    for (i, want) in plan.expected.iter().rev().enumerate() {
        let f = e_flat - 1 - i as i64;
        let (r, c) = (
            Line(f.div_euclid(cols as i64) as i32),
            Column(f.rem_euclid(cols as i64) as usize),
        );
        let cell = &term.grid()[r][c];
        let ok = match want {
            ExpectedCell::Base(ch) => cell.c == *ch && cell.flags.contains(Flags::WIDE_CHAR),
            // trailing spacer: rewritten fresh by the final repaint — require
            // BOTH the space char and the flag (an ordinary space must not
            // match a spacer slot, nor vice versa; seventh review)
            ExpectedCell::Spacer => {
                cell.c == ' ' && cell.flags.contains(Flags::WIDE_CHAR_SPACER)
            }
            // F2 scrub makes leading pads observationally plain spaces:
            // char-only, but never a wide base or trailing spacer
            ExpectedCell::Pad => {
                cell.c == ' '
                    && !cell.flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
            }
            // ordinary narrow char (incl. payload spaces): must not sit on
            // any wide-structure cell
            ExpectedCell::Narrow(ch) => {
                cell.c == *ch
                    && !cell.flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
            }
        };
        if !ok {
            return None;
        }
    }
    Some((raw, Point::new(Line(e_row as i32), Column(e_col))))
}
```

- [ ] **Step 4: Run tests** — `cargo test v3_` → all six pass. If `term_from` fails to compile, mirror the construction used by `scan_psreadline_cup` in terminal.rs tests (same crate version, known-good imports).

- [ ] **Step 5: Commit**

```powershell
git add src/paste_sim.rs
git commit -m "feat(paste-sim): predicate v3 over a real Term

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Phase machine — `PasteSim::try_arm` / `observe_chunk`

**Files:**
- Modify: `src/paste_sim.rs`

**Interfaces:**
- Produces (the module's whole public surface, consumed by Task 8 and Task 6):

```rust
pub struct PasteSim { /* private: plan, sniff, phase state, gen */ }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Seeking,
    Accepted { raw: Point, physical: Point, gen: u64 },
    Dead,
}

impl PasteSim {
    /// Pure arm gate + simulation; session-level gates (shell, term modes,
    /// content-end) stay in Session.
    pub fn try_arm(cols: usize, start_col: usize, start_pending: bool, text: &str) -> Option<PasteSim>;
    /// Feed one pumped chunk. `next_gen` is stamped only at the FIRST
    /// acceptance; later successful rechecks retain it.
    pub fn observe_chunk<L: EventListener>(&mut self, term: &Term<L>, bytes: &[u8], next_gen: u64) -> Phase;
}
```

- [ ] **Step 1: Write the failing transition-table test** (append)

```rust
    #[test]
    fn phase_machine_transition_table() {
        let text = format!("{}\u{1F923}", "a".repeat(16));
        let cols = 20;
        let mk = || PasteSim::try_arm(cols, 3, false, &text).expect("arms");
        let full = format!("P> \x1b[1;4H{text}\x1b[2;2H");
        let partial = "P> \x1b[1;4Haaaa".to_string();
        let full_term = term_from(cols, 8, full.as_bytes());
        let partial_term = term_from(cols, 8, partial.as_bytes());

        // Seeking + miss stays Seeking (early echo chunks must not kill it)
        let mut sim = mk();
        assert_eq!(sim.observe_chunk(&partial_term, b"aaaa", 7), Phase::Seeking);
        assert_eq!(sim.observe_chunk(&partial_term, b"", 8), Phase::Seeking);

        // Seeking + hit -> Accepted, stamped with THIS call's next_gen
        let mut sim = mk();
        let got = sim.observe_chunk(&full_term, b"", 7);
        let Phase::Accepted { raw, physical, gen } = got else {
            panic!("expected acceptance, got {got:?}");
        };
        assert_eq!((raw.line.0, raw.column.0), (1, 1));
        assert_eq!((physical.line.0, physical.column.0), (1, 2));
        assert_eq!(gen, 7);

        // Accepted + still-matching recheck retains the ORIGINAL gen
        assert_eq!(
            sim.observe_chunk(&full_term, b"", 99),
            Phase::Accepted { raw, physical, gen: 7 }
        );

        // [SUPERSEDED by v4 — see the header banner. Shipped behavior:
        // off-p_col miss -> Seeking (re-accept allowed, stamps new gen);
        // on-p_col miss with broken fingerprint -> Dead permanently.]
        // Accepted + failed recheck -> Dead, permanently (no re-acceptance)
        assert_eq!(sim.observe_chunk(&partial_term, b"", 100), Phase::Dead);
        assert_eq!(sim.observe_chunk(&full_term, b"", 101), Phase::Dead);

        // Seeking + mode barrier -> Dead even though the grid never matched
        let mut sim = mk();
        assert_eq!(
            sim.observe_chunk(&partial_term, b"\x1b[?1049h", 7),
            Phase::Dead
        );

        // Barrier split across two chunks still kills it
        let mut sim = mk();
        assert_eq!(sim.observe_chunk(&partial_term, b"\x1b[?10", 7), Phase::Seeking);
        assert_eq!(sim.observe_chunk(&partial_term, b"49l", 8), Phase::Dead);
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test phase_machine` → compile error.

- [ ] **Step 3: Implement**

```rust
pub struct PasteSim {
    plan: SimPlan,
    sniff: ModeSniff,
    state: State,
}

enum State {
    Seeking,
    Accepted { raw: Point, physical: Point, gen: u64 },
    Dead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Seeking,
    Accepted { raw: Point, physical: Point, gen: u64 },
    Dead,
}

impl PasteSim {
    pub fn try_arm(cols: usize, start_col: usize, start_pending: bool, text: &str) -> Option<PasteSim> {
        plan(cols, start_col, start_pending, text).map(|plan| PasteSim {
            plan,
            sniff: ModeSniff::default(),
            state: State::Seeking,
        })
    }

    pub fn observe_chunk<L: EventListener>(
        &mut self,
        term: &Term<L>,
        bytes: &[u8],
        next_gen: u64,
    ) -> Phase {
        if self.sniff.scan(bytes) {
            self.state = State::Dead;
        }
        match self.state {
            State::Dead => Phase::Dead,
            State::Seeking => match accept(term, &self.plan) {
                Some((raw, physical)) => {
                    self.state = State::Accepted { raw, physical, gen: next_gen };
                    Phase::Accepted { raw, physical, gen: next_gen }
                }
                None => Phase::Seeking,
            },
            State::Accepted { raw, physical, gen } => match accept(term, &self.plan) {
                // retain the original stamp; a moved raw is a failed recheck
                Some((r2, p2)) if r2 == raw && p2 == physical => {
                    Phase::Accepted { raw, physical, gen }
                }
                _ => {
                    self.state = State::Dead;
                    Phase::Dead
                }
            },
        }
    }
}
```

- [ ] **Step 4: Run tests** — `cargo test phase_machine` and `cargo test paste_sim` → all pass.

- [ ] **Step 5: Commit**

```powershell
git add src/paste_sim.rs
git commit -m "feat(paste-sim): Seeking/Accepted/Dead phase machine

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Task 0 replay gate — fixtures + fresh blind holdout + non-BMP control

**Files:**
- Modify: `src/paste_sim.rs` (replay test), `src/terminal.rs` (two new probe scenarios)
- Create: `docs/superpowers/specs/phase1_predict_v3.py` (extends the frozen model — do NOT edit `phase0_predict.py`)
- Create: `tests/fixtures/phase0/phase0-s4-v3holdout.{bin,json}`, `phase0-s5-nonbmp-control.{bin,json}`

**Interfaces:**
- Consumes: sidecar schema (Task 1), `PasteSim` public surface (Task 5).

- [ ] **Step 1: Freeze S4/S5 BEFORE running them.** Create `phase1_predict_v3.py`:

```python
#!/usr/bin/env python3
"""S4/S5 predictions for the v3 validation gate. Frozen before capture.
S4 = v3 blind holdout: non-periodic (13-cell rep vs 47 cols), different
prompt/cols/phase from every prior fixture. S5 = non-BMP P==E control:
emoji present, no margin straddle, must not arm."""
from phase0_predict import simulate_E, simulate_P, SCENARIOS

EXTRA = {
    # reps chosen so the paste MUST scroll on the 14-row capture window
    # (seventh review: v3's history traversal needs a scrolled fixture);
    # 60 reps * 13 cells = 780 cells ~= 16.6 rows at 47 cols > 14 rows.
    "s4-v3holdout": dict(cols=47, start_col=5, text="q\U0001F923\U0001F952rs\U0001F923\U0001F923t " * 60),
    "s5-nonbmp-control": dict(cols=40, start_col=3, text=None),  # chosen below
}
# S5 selection rule (frozen): smallest k in 4..12 such that the payload
# ("\U0001F923" * 2 + "x" * (k - 4) + " ") * 40 yields delta == 0 at
# cols=40, start_col=3. Record the chosen k and predictions before capture.
```

Run it, iterate the S5 rule until `delta == 0` with emoji present, and append the chosen payloads + predicted P/E/delta for both scenarios to the file as comments. Only then proceed.

- [ ] **Step 2: Add the two probe scenarios to terminal.rs tests** (same shape as the existing three):

```rust
    #[test]
    #[ignore = "phase0 diagnostic: v3 BLIND holdout — do not inspect until v3 is final"]
    fn phase0_s4_v3holdout() {
        phase0_probe(
            "s4-v3holdout",
            47,
            14,
            "V3~> ",
            &"q\u{1F923}\u{1F952}rs\u{1F923}\u{1F923}t ".repeat(60), // must scroll
        );
    }

    #[test]
    #[ignore = "phase0 diagnostic: non-BMP P==E control — must not arm"]
    fn phase0_s5_nonbmp_control() {
        phase0_probe("s5-nonbmp-control", 40, 14, "P> ", /* frozen S5 payload from step 1 */);
    }
```

Run S5 normally; run S4 with output redirected to a quarantine file, unread. Copy both `.bin`/`.json` into `tests/fixtures/phase0/`, record SHA-256 in the results doc.

- [ ] **Step 3: Write the replay test** (in `paste_sim.rs` tests; NOT ignored — fixtures are checked in):

```rust
    #[derive(serde::Deserialize)]
    struct Sidecar {
        cols: usize,
        rows: usize,
        pre: SidecarPre,
        post: SidecarPost,
        final_grid: Vec<String>,
        paste_start_frame: usize,
    }
    #[derive(serde::Deserialize)]
    struct SidecarPre {
        row: i32,
        col: usize,
        input_needs_wrap: bool,
    }
    #[derive(serde::Deserialize)]
    struct SidecarPost {
        raw_row: i32,
        raw_col: usize,
    }

    fn read_fixture(name: &str) -> (Sidecar, Vec<Vec<u8>>) {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/phase0/");
        let side: Sidecar =
            serde_json::from_str(&std::fs::read_to_string(format!("{dir}{name}.json")).unwrap())
                .unwrap();
        let raw = std::fs::read(format!("{dir}{name}.bin")).unwrap();
        let mut frames = Vec::new();
        let mut off = 4; // skip the u32 paste_start_frame header (also in JSON)
        while off < raw.len() {
            let n = u32::from_le_bytes(raw[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            frames.push(raw[off..off + n].to_vec());
            off += n;
        }
        (side, frames)
    }

    fn canon_grid<L: EventListener>(term: &Term<L>) -> Vec<String> {
        let g = term.grid();
        (0..g.screen_lines())
            .map(|r| {
                let mut s = String::new();
                for c in 0..g.columns() {
                    let cell = &g[Line(r as i32)][Column(c)];
                    s.push(cell.c);
                    s.push('|');
                    if cell.flags.contains(Flags::WIDE_CHAR) { s.push('W'); }
                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) { s.push('S'); }
                    if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) { s.push('L'); }
                    s.push(';');
                }
                s
            })
            .collect()
    }

    /// The Task 0 gate: replay a fixture byte-at-a-time. For EVERY fixture
    /// (arming or not): full replay + final-grid + post-cursor equality with
    /// the sidecar. For coverage fixtures additionally: no wrong alias at
    /// ANY prefix AND the FINAL phase is Accepted(expected) — a transient
    /// acceptance that later dies is a FAILURE (seventh review).
    fn replay_fixture(
        name: &str,
        payload: &str,
        expect_alias: Option<(i32, usize, i32, usize)>,
        expect_arm: bool,
    ) {
        let (side, frames) = read_fixture(name);
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(side.cols, side.rows),
            VoidListener,
        );
        let mut parser = alacritty_terminal::vte::ansi::Processor::new();
        for frame in &frames[..side.paste_start_frame] {
            parser.advance(&mut term, frame);
        }
        assert_eq!(
            (term.grid().cursor.point.line.0, term.grid().cursor.point.column.0,
             term.grid().cursor.input_needs_wrap),
            (side.pre.row, side.pre.col, side.pre.input_needs_wrap),
            "prestate mismatch — capture and replay disagree"
        );
        let sim = PasteSim::try_arm(side.cols, side.pre.col, side.pre.input_needs_wrap, payload);
        assert_eq!(sim.is_some(), expect_arm, "{name}: arm disagreement");
        let mut sim = sim; // None for controls; still replay everything
        let mut gen = 1u64;
        let mut last_phase = Phase::Seeking;
        for frame in &frames[side.paste_start_frame..] {
            for &b in frame.iter() {
                parser.advance(&mut term, &[b]);
                gen += 1;
                if let Some(sim) = sim.as_mut() {
                    last_phase = sim.observe_chunk(&term, &[b], gen);
                    if let Phase::Accepted { raw, physical, .. } = last_phase {
                        let got =
                            (raw.line.0, raw.column.0, physical.line.0, physical.column.0);
                        assert_eq!(Some(got), expect_alias,
                            "{name}: WRONG alias at some byte prefix");
                    }
                }
            }
        }
        match expect_alias {
            Some(want) => match last_phase {
                Phase::Accepted { raw, physical, .. } => assert_eq!(
                    (raw.line.0, raw.column.0, physical.line.0, physical.column.0),
                    want,
                    "{name}: final alias wrong"
                ),
                other => panic!(
                    "{name}: final phase must be Accepted, got {other:?} \
                     (a transient accept that died is a failure)"
                ),
            },
            None => assert!(
                !matches!(last_phase, Phase::Accepted { .. }),
                "{name}: control fixture must never end Accepted"
            ),
        }
        // final-state equality for EVERY fixture, controls included
        assert_eq!(
            (term.grid().cursor.point.line.0, term.grid().cursor.point.column.0),
            (side.post.raw_row, side.post.raw_col),
            "{name}: post cursor mismatch"
        );
        assert_eq!(canon_grid(&term), side.final_grid, "{name}: final grid mismatch");
    }

    #[test]
    fn task0_replay_s1_primary() {
        replay_fixture(
            "phase0-s1-primary",
            &"\u{1F952}\u{1F923}\u{1F923}\u{1F923} ".repeat(48),
            Some((10, 35, 10, 39)),
            true,
        );
    }

    #[test]
    fn task0_replay_s2_holdout() {
        replay_fixture(
            "phase0-s2-holdout",
            &"ab\u{1F923}\u{1F952}cd\u{1F923} ".repeat(30),
            Some((10, 6, 10, 7)),
            true,
        );
    }

    #[test]
    fn task0_replay_s3_control() {
        replay_fixture("phase0-s3-control", &"abcd ".repeat(80), None, false);
    }

    // COMPILE-BLOCKING pre-registration (seventh review: placeholder
    // literals could compile and false-green). These consts DO NOT EXIST
    // until the freeze step defines them from phase1_predict_v3.py — the
    // tests below fail to compile until then, which is the point.
    //
    //   const FROZEN_S4_PAYLOAD_REPS: usize = ...;   // must force scrolling
    //   const FROZEN_S4_ALIAS: (i32, usize, i32, usize) = (...);
    //   const FROZEN_S5_PAYLOAD: &str = "...";       // emoji, delta == 0
    //
    // S4 viewport math: expected raw viewport row =
    //   pre.row + P_rows − (post.history − pre.history), from the sidecar.

    #[test]
    fn task0_replay_s4_v3holdout() {
        replay_fixture(
            "phase0-s4-v3holdout",
            &"q\u{1F923}\u{1F952}rs\u{1F923}\u{1F923}t ".repeat(FROZEN_S4_PAYLOAD_REPS),
            Some(FROZEN_S4_ALIAS),
            true,
        );
    }

    #[test]
    fn task0_replay_s5_nonbmp_control() {
        assert!(!FROZEN_S5_PAYLOAD.is_empty());
        assert!(FROZEN_S5_PAYLOAD.chars().any(|c| c as u32 > 0xFFFF));
        replay_fixture("phase0-s5-nonbmp-control", FROZEN_S5_PAYLOAD, None, false);
    }
```

Fill the two `/* frozen */` literals from step 1's recorded predictions before the first run — that IS the pre-registration.

- [ ] **Step 4: Run the gate** — `cargo test task0_replay` → all five pass. **Kill-switch (spec):** any wrong-alias assert or missed coverage → iterate to v4 against the captures, freeze another holdout, re-run; two consecutive failures → STOP, record the negative in the results doc, park.

- [ ] **Step 5: Update the results doc** — append a "Task 0 gate" section: pass/fail per fixture, S4 hashes, S5 payload, v3 status flipped from candidate to validated-on-fixtures (only if all green).

- [ ] **Step 6: Commit**

```powershell
git add src/paste_sim.rs src/terminal.rs tests/fixtures/phase0 docs/superpowers/specs/phase1_predict_v3.py docs/superpowers/specs/2026-07-12-paste-sim-phase0-results.md
git commit -m "test(paste-sim): Task 0 replay gate green on 5 fixtures

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Adversarial armed-path benchmark

**Files:**
- Modify: `src/paste_sim.rs` (a `#[test]` benchmark mirroring `scanner_overhead_on_plain_and_ansi_floods`'s style in terminal.rs)

- [ ] **Step 1: Write the benchmark**

```rust
    /// Worst case: every chunk leaves the cursor on p_col with a payload at
    /// the arming cap, forcing the full fingerprint walk per chunk. Mirrors
    /// scanner_overhead_on_plain_and_ansi_floods (terminal.rs) in spirit:
    /// measure, print, and assert a generous ceiling so regressions scream.
    #[test]
    #[ignore = "wall-clock benchmark: run in --release only"]
    fn armed_path_worst_case_is_bounded() {
        // cols must be ODD so width-2 glyphs actually straddle the margin
        // (200 cols: bases stay even, no pads, delta==0, plan() is None).
        let cols = 199;
        let text = "\u{1F923}".repeat(1800); // ~3600 cells + pads, under cap
        let mut sim = PasteSim::try_arm(cols, 0, false, &text).expect("arms");
        // Sustained worst case: an ACCEPTED alias re-verified every chunk —
        // each recheck walks the full fingerprint AND the viewport suffix.
        // Build the real echoed grid: payload flow + PSReadLine-style final
        // CUP at P (1-based), so the first observe accepts.
        let p = plan(cols, 0, false, &text).expect("arms");
        // Feed the full flow, then a PSReadLine-style final CUP at P
        // (derived from the plan's own numbers; start_col = 0):
        let e_cells = p.expected.len();
        let p_flat = e_cells - p.delta; // E_flat - delta, start_col = 0
        let cup = format!("\x1b[{};{}H", p_flat / cols + 1, p_flat % cols + 1);
        let term = term_from(cols, 50, format!("{text}{cup}").as_bytes());
        assert!(
            matches!(sim.observe_chunk(&term, b"", 0), Phase::Accepted { .. }),
            "benchmark precondition: alias must accept"
        );
        let start = std::time::Instant::now();
        let rounds = 10_000;
        for i in 1..=rounds {
            assert!(matches!(
                sim.observe_chunk(&term, b"", i),
                Phase::Accepted { .. }
            ));
        }
        let per = start.elapsed() / rounds as u32;
        println!("armed worst-case observe_chunk: {per:?}");
        assert!(
            per < std::time::Duration::from_micros(200),
            "armed path too slow: {per:?}"
        );
    }
```

- [ ] **Step 2: Extend to the full scenario matrix, then run in release** —
add two sibling `#[ignore]`d benchmarks in the same shape: (a) **column
misses** (cursor parked off `p_col` — the common armed case; must be
near-zero since only the O(1) gate runs), and (b) **sniffer throughput**
(feed 1 MiB of mixed SGR/CUP/emoji bytes through `observe_chunk` while
Seeking, cursor never on `p_col`). Use the largest supported geometry
(e.g. 400×100) for all three. Run:
`cargo test --release armed_ -- --ignored --nocapture`. Record all printed
numbers in the results doc. If it fails the 200µs ceiling, the fingerprint needs an early-out ordering fix (walk the suffix check first — it fails fastest on a mid-echo grid) before loosening any threshold; if it passes with >10x headroom, note whether `MAX_SIM_CELLS` could rise.

- [ ] **Step 3: Commit**

```powershell
git add src/paste_sim.rs docs/superpowers/specs/2026-07-12-paste-sim-phase0-results.md
git commit -m "test(paste-sim): adversarial armed-path benchmark

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Production wiring in `Session`

**Files:**
- Modify: `src/terminal.rs` — `Session` fields, `arm_psreadline_paste_cursor`, `clear_psreadline_paste_cursor`, `send_external_input`, `pump_at`, `read_input` clear sites

**Interfaces:**
- Consumes: `crate::paste_sim::{PasteSim, Phase}`.
- Produces: unchanged external behavior of `psreadline_cursor_alias`/`_gen` for all existing consumers.

- [ ] **Step 1: Swap the Session field**

Replace `psreadline_cup_scanner: Option<CupScanner>` (declaration, init in `spawn_with`, and `clear_psreadline_paste_cursor`) with:

```rust
    psreadline_paste_sim: Option<crate::paste_sim::PasteSim>,
```

`clear_psreadline_paste_cursor` clears all three: `psreadline_paste_sim`, `psreadline_cursor_alias`, `psreadline_cursor_alias_gen`.

- [ ] **Step 2: Rewrite `arm_psreadline_paste_cursor` with the pinned atomic order**

```rust
    fn arm_psreadline_paste_cursor(&mut self, text: &str) {
        // Every new paste starts a new decision epoch. Session-level gates
        // here; the sim-domain gates (control chars, zero-width, cap,
        // wrap-pending, delta>0) live in PasteSim::try_arm. Spec: arming
        // order is invalidate -> compute from pre-paste state -> (caller
        // sends the one exempt payload) -> install.
        let point = self.display_cursor_point();
        let pending = self.term.grid().cursor.input_needs_wrap;
        self.clear_psreadline_paste_cursor();
        let mode = *self.term.mode();
        if self.shell != Shell::PowerShell
            || mode.intersects(TermMode::ALT_SCREEN | TermMode::BRACKETED_PASTE)
            || text.is_empty()
            || text.contains(['\r', '\n'])
            || !cursor_at_content_end(&self.term, point)
        {
            return;
        }
        self.psreadline_paste_sim = crate::paste_sim::PasteSim::try_arm(
            self.term.grid().columns(),
            point.column.0,
            pending,
            text,
        );
    }
```

**One shared operation for every paste path (seventh review — the per-site
exemption was too broad):**

```rust
    /// The ONLY way a paste epoch is installed. Ordering per spec:
    /// prepare candidate -> clear old epoch -> send exactly one eligible
    /// paste -> install. `extra_bytes_in_batch` = anything else going to
    /// the PTY this frame (key events, interrupt, encoded chords): if the
    /// batch is not purely the paste, the candidate is NOT installed.
    fn paste_with_epoch(&mut self, text: &str, seq: &[u8], extra_bytes_in_batch: bool) {
        let pre_point = self.display_cursor_point();
        let pre_pending = self.term.grid().cursor.input_needs_wrap;
        let candidate = self.paste_epoch_candidate(pre_point, pre_pending, text);
        self.clear_psreadline_paste_cursor(); // old epoch dies with this send
        self.invalidate_wide_shadow();
        self.send(seq);
        if !extra_bytes_in_batch {
            self.psreadline_paste_sim = candidate;
        }
    }

    /// Session-level gates + PasteSim::try_arm (pure); no side effects.
    fn paste_epoch_candidate(
        &self,
        point: Point,
        pending: bool,
        text: &str,
    ) -> Option<crate::paste_sim::PasteSim> {
        let mode = *self.term.mode();
        if self.shell != Shell::PowerShell
            || mode.intersects(TermMode::ALT_SCREEN | TermMode::BRACKETED_PASTE)
            || text.is_empty()
            || text.contains(['\r', '\n'])
            || !cursor_at_content_end(&self.term, point)
        {
            return None;
        }
        crate::paste_sim::PasteSim::try_arm(
            self.term.grid().columns(),
            point.column.0,
            pending,
            text,
        )
    }
```

All four paste paths route through `paste_with_epoch`:
- `feed_text`: `self.paste_with_epoch(text, text.as_bytes(), false)`
- `paste_text` (right-click): `let seq = crate::input::paste_seq(*self.term.mode(), txt); self.paste_with_epoch(txt, &seq, false)`
- `read_input` `Event::Paste` and Ctrl+Shift+V: the paste seq is appended to
  the frame's `bytes`; pass `extra_bytes_in_batch = (bytes.len() != paste_seq.len()) || submitted || outcome.interrupt`
  so a paste sharing its frame with ANY other input never installs an epoch.
The old `arm_psreadline_paste_cursor` becomes `paste_epoch_candidate` (above)
plus the install; delete the arm-before-send calls at all four sites.

- [ ] **Step 3: Broaden the user-input clear**

In `send_external_input`, replace the CR/LF/^C-only epoch clear with an unconditional one (protocol replies don't pass through here, per its doc comment):

```rust
    fn send_external_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // Any user-sent bytes end the paste epoch (spec: epoch lifecycle).
        self.clear_psreadline_paste_cursor();
        self.invalidate_wide_shadow();
        self.send(bytes);
    }
```

In `read_input`, where `bytes` are about to be sent (the block containing `if !bytes.is_empty() { ... self.send(&bytes) }`), add `self.clear_psreadline_paste_cursor();` before `self.send(&bytes)` — but only when the paste epoch wasn't armed *this frame* (the `pasted_text`/Ctrl+Shift+V paths arm and send in the same frame; guard with a local `armed_this_frame` bool set by the arm calls).

- [ ] **Step 4: Replace the `pump_at` scanner branch**

The `if let Some(scanner) = self.psreadline_cup_scanner.as_mut()` branch becomes:

```rust
            advance_scanned(
                &mut self.parser,
                &mut self.term,
                &mut self.graphics,
                &bytes,
                &mut greplies,
            );
            if let Some(sim) = self.psreadline_paste_sim.as_mut() {
                let next_gen = self.output_gen.wrapping_add(1);
                match sim.observe_chunk(&self.term, &bytes, next_gen) {
                    crate::paste_sim::Phase::Accepted { raw, physical, gen } => {
                        self.psreadline_cursor_alias = Some(CursorAlias { raw, physical });
                        self.psreadline_cursor_alias_gen = Some(gen);
                    }
                    crate::paste_sim::Phase::Seeking => {}
                    crate::paste_sim::Phase::Dead => {
                        self.clear_psreadline_paste_cursor();
                    }
                }
            }
```

The existing post-chunk `ALT_SCREEN | BRACKETED_PASTE` clear stays (backstop). The CPR flush cadence lines below it are untouched.

- [ ] **Step 5: Fix the tests that pinned scanner internals**

`cargo test 2>&1 | Select-String "FAILED|error"` and repair by intent, not deletion:
- `psreadline_alias_drops_on_alt_screen_enter_and_exit_in_one_chunk` / `..._bracketed_paste_...`: keep the expectation (drop) — they now pass via the sniffer; update any direct `psreadline_cup_scanner` field pokes to `psreadline_paste_sim = PasteSim::try_arm(...)`.
- `psreadline_alias_drops_on_grid_mutation_even_if_cursor_returns`, `paste_alias_must_be_observed_after_the_input_that_invalidated_shadow`, `session_rejects_a_carried_paste_alias_after_later_input_invalidation`: retarget setup to the new field; expectations unchanged.
- The five pure `psreadline_cup_*` scanner tests are deleted in Task 9 with the scanner itself; leave them compiling for now.
- Add one new test pinning the arming order:

```rust
    #[test]
    fn paste_epoch_install_rules() {
        // use the same fixture helper the neighboring alias tests use for a
        // Session with a fake writer; do not invent a new one
        let mut s = /* session fixture */;
        s.shell = Shell::PowerShell;
        let emoji_line = format!("{}\u{1F923}", "a".repeat(16));

        // 1. feed_text: the epoch survives its own arming send
        s.feed_text(&emoji_line);
        assert!(s.psreadline_paste_sim.is_some(), "epoch survived its own send");

        // 2. a later plain send is user input: old epoch dies; "x" itself
        //    cannot arm (delta 0), so the field must now be None
        s.feed_text("x");
        assert!(s.psreadline_paste_sim.is_none(), "old epoch must die; x cannot arm");

        // 3. paste_text (right-click) installs
        s.paste_text(&emoji_line);
        assert!(s.psreadline_paste_sim.is_some());

        // 4. Event::Paste alone in a frame installs (drive via ctx.run_ui
        //    exactly like inject-tests do), and
        // 5. Event::Paste + a key event in the SAME frame must NOT install:
        //    push both egui::Event::Paste(emoji_line) and a Key event into
        //    one RawInput, run read_input, then:
        //    assert!(s.psreadline_paste_sim.is_none(), "mixed batch installed");
    }
```

(Adopt whatever Session fixture the surrounding tests actually use — search for the helper the alias tests construct with; do not invent a new one.)

- [ ] **Step 6: Full suite + commit**

```powershell
cargo test 2>&1 | Select-Object -Last 3
git add src/terminal.rs src/paste_sim.rs
git commit -m "feat(terminal): wire PasteSim epoch into Session; broaden user-input clear

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Live evidence on the new path (scanner still present, dead)

After Task 8 the scanner code exists but `pump_at` no longer calls it — the
live behavior under test is the new path. **Both live tests must be green
here, BEFORE any deletion** (seventh review: deleting first contradicts the
gate order).

**Files:**
- Modify: `src/terminal.rs` (one new live test), results doc (probe readouts)

- [ ] **Step 1: Run the existing live test unchanged**

```powershell
cargo test --release live_psreadline_paste_wrap_uses_the_whole_emoji_endpoint -- --ignored --nocapture
```
Expected: PASS with zero edits to the test. Any edit needed = spec violation; stop and re-review.

- [ ] **Step 2: Add the multi-row live test** (next to the existing one; same fixture patterns):

```rust
    /// Live proof for the residual variant c0bdb7c parked: an 11-row emoji
    /// paste. The scanner's pad-count gate failed closed here (F2 scrub);
    /// the simulation must alias to E' and make Backspace double.
    #[test]
    #[ignore = "diagnostic: drives a real PowerShell paste through ConPTY"]
    fn live_psreadline_multirow_paste_aliases_to_flow_end() {
        // Reuse the spawn/ready/pump scaffolding of the sibling live test
        // verbatim (spawn pwsh with -NoProfile + plain prompt, resize 40x14,
        // wait ready, settle 300ms) — then:
        let payload = "\u{1F952}\u{1F923}\u{1F923}\u{1F923} ".repeat(48);
        let mut raw_input = egui::RawInput::default();
        raw_input.events.push(egui::Event::Paste(payload));
        let _ = ctx.run_ui(raw_input, |ui| session.read_input(ui));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.psreadline_cursor_alias.is_none() {
            session.pump();
            assert!(std::time::Instant::now() < deadline, "alias never accepted");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let alias = session.psreadline_cursor_alias.unwrap();
        assert_eq!((alias.raw.line.0 - alias.physical.line.0), 0);
        assert_eq!(alias.physical.column.0, alias.raw.column.0 + 4); // delta from frozen model
        assert_eq!(session.display_cursor_point(), alias.physical);
        // Backspace oracle (seventh review): the payload ends in a SPACE.
        // First Backspace deletes that space — SINGLE encoding (one DEL).
        // Second Backspace crosses the trailing emoji — DOUBLED (two DEL).
        // Drive two press/release cycles through read_input (sibling test's
        // key() helper), then assert on the snapshot:
        //   - exactly one trailing emoji removed in total,
        //   - zero U+FFFD anywhere,
        //   - the 47 preceding reps intact.
        // Additionally assert the byte counts via the recorded writer if the
        // session fixture exposes one (single DEL then double DEL).
    }
```

(The implementer copies the sibling test's scaffolding blocks literally where the comments say so — it is 40 lines above in the same file.)

Run: `cargo test --release live_psreadline_multirow -- --ignored --nocapture` → PASS.

- [ ] **Step 3: Numerical headless probe on the release app** (handoff
recipe, made numeric — no eyeballing): launch `target\release\foreman.exe`,
`foreman send --text` the S1 payload at a pwsh pane, then:

```powershell
$f = ".\target\release\foreman.exe"
$cur = (& $f snapshot --project p1 --terminal t1 --cursor | ConvertFrom-Json)
# compute expected E' col from the frozen model (phase0_predict.py S1 with
# this pane's actual cols/prompt) and assert numerically:
if ($cur.cursor.col -ne $EXPECTED_E_COL) { throw "caret at $($cur.cursor.col), expected $EXPECTED_E_COL" }
```

(Adapt field names to the actual `snapshot --cursor` JSON — see
`foreman-run-and-operate`.) Record the numbers in the results doc.

- [ ] **Step 4: Screenshot evidence** (working agreement): screenshot the
foreman window after the paste (script in `docs/HANDOFF.md` §3), `Read` the
PNG, confirm the caret visually sits at the text end. Save alongside the
probe readout note.

---

### Task 10: Delete the scanner, re-verify, docs, final loop

**Files:**
- Modify: `src/terminal.rs`, `docs/wide-chars.md`, results doc

- [ ] **Step 1: Delete** `CupScanner`, `CupSink`, `CupScanEvent`,
`CupScanResult`, `advance_psreadline_scanned`, the free fn
`psreadline_cursor_alias(...)`, and their tests (`scan_psreadline_cup`, the
five `psreadline_cup_*` pure tests,
`psreadline_cup_scanner_handles_a_sequence_split_between_pty_chunks`).
**Keep** `CursorAlias`, the `Session.psreadline_cursor_alias`/`_gen` fields,
`cursor_at_content_end`, `generation_after`, and every consumer.

- [ ] **Step 2: Verify no dangling references + record LOC delta**

```powershell
Select-String -Path src\*.rs -Pattern "CupScanner|CupSink|CupScanEvent|CupScanResult|advance_psreadline_scanned"
git diff --stat
```
Expected: no matches; note the net LOC in the results doc.

- [ ] **Step 3: RERUN both live tests post-deletion** (seventh review — the
pre-deletion green in Task 9 does not carry over):

```powershell
cargo test --release live_psreadline_paste_wrap_uses_the_whole_emoji_endpoint -- --ignored --nocapture
cargo test --release live_psreadline_multirow -- --ignored --nocapture
```
Expected: both PASS.

- [ ] **Step 4: Correct `docs/wide-chars.md`**
  - "Residual variant" paragraph: replace the "hard-positioned, zero pads" mechanism with the F2 finding (pads scrubbed by `write_at_cursor` overwrite cleanup under repaint-in-place; observation stands, mechanism corrected; scanner counted a flag the redraw erases).
  - The repro paragraph's "`foreman send --text` does NOT arm the paste epoch" → corrected: it arms via `feed_text` (wm.rs:1091).
  - Replace the "Tracked follow-up" paragraph with a pointer to the spec + results docs and the new mechanism summary.

- [ ] **Step 5: Final verification loop** (all of it, in order):

```powershell
if (-not $env:FOREMAN) { Get-Process foreman -ErrorAction SilentlyContinue | Stop-Process -Force }
cargo test 2>&1 | Select-Object -Last 3                              # full suite green
cargo test --release task0_replay 2>&1 | Select-Object -Last 3      # gate still green
cargo test --release armed_ -- --ignored --nocapture                # armed perf numbers
cargo test --release scanner_overhead_on_plain_and_ansi_floods -- --nocapture  # unarmed guard
```
Then the Task 9 step-3 numerical headless probe and step-4 screenshot once
more against the final build. Record everything in the results doc.

- [ ] **Step 6: The aggregate commit (CONDITIONAL — requires explicit user
authorization; this is the single commit unit the user approved)**

```powershell
git add src/paste_sim.rs src/terminal.rs src/main.rs tests/fixtures/phase0 docs/wide-chars.md docs/superpowers/specs docs/superpowers/plans docs/superpowers/paste-sim-rework.html .gitignore
git commit -m "feat(terminal): replace CupScanner with paste-simulation alias (predicate v3)

Phase 0 + Task 0 validated; see
docs/superpowers/specs/2026-07-12-paste-simulation-rework-design.md.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Execution notes

- Tasks 1–7 are safe in any session (tests + fixtures only; no production behavior change). **Task 8 is the first production change and must not start until Task 6's gate is green.** **Task 10 (deletion) must not start until Task 9's live tests are green on the new path**, and both live tests rerun after deletion.
- **Nothing in this plan is commit-authorized by default** — see Global Constraints; the single aggregate commit in Task 10 step 6 is the approved unit, executed only on explicit user say-so.
- Tasks 2→5 are sequential (same file, each consumes the previous interface). Task 3 can run parallel to Task 2 if desired (disjoint code in the same file — prefer sequential to avoid merge noise).
- If any live test flakes: `foreman-validation-and-qa` + recipe 7 (no fix-by-retry).
