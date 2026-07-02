# Terminal Image Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the Alt-key double-print bug, deliver clipboard-image paste keystrokes to agents, and render a pets-first kitty graphics subset — per `docs/superpowers/specs/2026-07-02-terminal-image-support-design.md`.

**Architecture:** A new pure module `src/graphics.rs` (deep module: `feed`/`apply`/`visible`) taps the same PTY bytes alacritty parses; `Session::pump` advances the parser in segments around graphics "cuts" so placements sample the cursor exactly; `Session::show` paints egui textures over the glyphs. Input fixes are pure changes in `src/input.rs`.

**Tech Stack:** Rust (edition 2024, **GNU toolchain — not MSVC**), egui/eframe 0.34.3, alacritty_terminal 0.26, arboard 3.6. New deps: `png = "0.17"`, `base64 = "0.22"`.

## Global Constraints

- Build loop (Windows/PowerShell): `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500` **before every build**, else the link fails with `Access is denied (os error 5)`.
- Tests: `cargo test` (all), or `cargo test --lib input` / `--lib graphics` / `--lib terminal` per module.
- Perf gate (spec): **<2% flood-throughput regression**; no new threads, locks, or async; no allocation on the no-graphics byte path.
- Only new dependencies allowed: `png = "0.17"`, `base64 = "0.22"` (user-approved).
- Never use `VoidListener` in a real `Session` (DSR trap). `Term<VoidListener>` is fine in pure parse tests (existing pattern).
- Commit trailer (project rule): `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`. Stage files by name.
- Branch: all work on `feat/terminal-images`.
- Serena-first tool rule applies (project CLAUDE.md): symbol reads/edits via Serena tools.

---

### Task 1: Baseline flood measurement (BEFORE any code change)

**Files:**
- Create: `docs/terminal-images.md` (skeleton with a Performance table)

**Interfaces:**
- Produces: baseline numbers that Task 11 compares against.

- [ ] **Step 1: Build release at the current commit**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build --release 2>&1 | Select-Object -Last 5
```
Expected: `Finished` line, no errors.

- [ ] **Step 2: Run the flood benchmark in a foreman pane**

Launch `target\release\foreman.exe`, open a project (any directory). In the pane's PowerShell, run this exact command **3 times** and record the median `TotalSeconds`:

```powershell
$f="$env:TEMP\flood.txt"; if (!(Test-Path $f)) { $line='x'*120; Set-Content $f (1..200000 | ForEach-Object { $line }) }; (Measure-Command { cmd /c type $f }).TotalSeconds
```

(If driving the pane headlessly, use the control CLI — see `docs/HANDOFF.md` § foreman send/snapshot for syntax — but typing it interactively is fine; the measurement is self-contained inside the pane.)

- [ ] **Step 3: Record the baseline**

Create `docs/terminal-images.md`:

```markdown
# Terminal Images (kitty graphics, image paste, alt-key routing)

Status: in progress — see docs/superpowers/specs/2026-07-02-terminal-image-support-design.md

## Performance

Flood benchmark: median of 3 runs of `cmd /c type` on a 200k-line file (120 cols)
inside a release foreman pane.

| Point | TotalSeconds |
|---|---|
| Baseline (pre-change, commit <hash>) | <measured> |
| After (feat/terminal-images complete) | pending |

Scanner micro-benchmark (Task 11): pending
```

Fill in `<hash>` (`git rev-parse --short HEAD`) and `<measured>`.

- [ ] **Step 4: Commit**

```powershell
git add docs/terminal-images.md
git commit -m "docs(perf): baseline flood throughput before image support

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Alt+letter double-send fix (src/input.rs)

**Files:**
- Modify: `src/input.rs` (`process_input` signature + `Event::Text` arm; all existing tests)
- Modify: `src/terminal.rs` (`Session::read_input` call site, ~line 790)

**Interfaces:**
- Consumes: existing `process_input(events, mode, has_selection)`, test helpers `key_ev(Key, Modifiers)`, `mods(ctrl, alt, shift)`, `none()`.
- Produces: `pub fn process_input(events: &[Event], mods: Modifiers, mode: TermMode, has_selection: bool) -> InputOutcome` — the new second parameter is the **live frame modifier state** (`i.modifiers`), distinct from per-event modifiers. All later tasks use this 4-arg signature.

- [ ] **Step 1: Write the failing tests** (in `src/input.rs` `mod tests`)

```rust
    #[test]
    fn alt_letter_sends_meta_only_once_despite_text_event() {
        // Windows egui delivers BOTH the Key event and a Text event for
        // Alt+letter; only the ESC-prefixed meta byte may reach the PTY.
        let live = egui::Modifiers { alt: true, ..Default::default() };
        let out = process_input(
            &[key_ev(Key::V, mods(false, true, false)), Event::Text("v".into())],
            live,
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, b"\x1bv");
    }

    #[test]
    fn altgr_text_still_types() {
        // AltGr arrives as Ctrl+Alt on Windows; intl layouts must keep typing.
        let live = egui::Modifiers { alt: true, ctrl: true, ..Default::default() };
        let out = process_input(&[Event::Text("@".into())], live, TermMode::empty(), false);
        assert_eq!(out.pty_bytes, b"@");
    }
```

- [ ] **Step 2: Thread the new parameter through** — change the signature and the `Event::Text` arm:

```rust
pub fn process_input(
    events: &[Event],
    mods: Modifiers,
    mode: TermMode,
    has_selection: bool,
) -> InputOutcome {
```

```rust
            Event::Text(t) => {
                // Windows egui delivers BOTH a Key event and a Text event for
                // Alt+letter. encode_key already sends ESC+letter for alt
                // (without ctrl), so the Text copy must be dropped — but AltGr
                // arrives as Ctrl+Alt and must keep typing (intl layouts).
                // Mirrors encode_key's meta condition exactly.
                if !(mods.alt && !(mods.ctrl || mods.command)) {
                    out.pty_bytes.extend_from_slice(t.as_bytes());
                }
            }
```

Update the call site in `src/terminal.rs` `read_input`:

```rust
        let outcome =
            ui.input(|i| crate::input::process_input(&i.events, i.modifiers, mode, has_selection));
```

Update every existing test call mechanically: insert a second argument after the events slice. Tests exercising plain typing/keys pass `egui::Modifiers::default()`; tests whose events carry ctrl/shift pass the matching value only if the test asserts text suppression (none do today), so `egui::Modifiers::default()` is correct for **all** existing calls. Example:

```rust
        let out = process_input(&[Event::Text("a".into())], egui::Modifiers::default(), TermMode::empty(), false);
```

- [ ] **Step 3: Run the module tests**

Run: `cargo test --lib input`
Expected: all pass, including the two new tests.

- [ ] **Step 4: Run the full suite** — `cargo test` — expected: green.

- [ ] **Step 5: Commit**

```powershell
git add src/input.rs src/terminal.rs
git commit -m "fix(input): stop Alt+letter double-send by dropping the duplicate Text event

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Ctrl+Alt chords reach the app (src/input.rs)

**Files:**
- Modify: `src/input.rs` (`process_input` ctrl-chord guard, `encode_key`)

**Interfaces:**
- Consumes: 4-arg `process_input` from Task 2.
- Produces: Ctrl+Alt+letter encodes as `ESC + control-code` (Codex binds paste-image to Ctrl+Alt+V → must arrive as `1b 16`).

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn ctrl_alt_v_encodes_meta_ctrl_v_not_clipboard_paste() {
        // Codex's second paste-image binding; must not be shadowed by the
        // Ctrl+V clipboard chord.
        let live = egui::Modifiers { alt: true, ctrl: true, ..Default::default() };
        let out = process_input(
            &[key_ev(Key::V, mods(true, true, false))],
            live,
            TermMode::empty(),
            false,
        );
        assert_eq!(out.pty_bytes, b"\x1b\x16");
        assert!(!out.paste_clipboard);
    }

    #[test]
    fn ctrl_alt_c_does_not_copy_or_interrupt() {
        let live = egui::Modifiers { alt: true, ctrl: true, ..Default::default() };
        let out = process_input(
            &[key_ev(Key::C, mods(true, true, false))],
            live,
            TermMode::empty(),
            true,
        );
        assert_eq!(out.pty_bytes, b"\x1b\x03");
        assert!(!out.copy && !out.interrupt);
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib input` — expected: both FAIL (chord swallows V; encoder emits nothing).

- [ ] **Step 3: Implement.** In `process_input`, exclude alt from the policy chords:

```rust
                // Copy/paste policy chords (Ctrl held) — intercepted before
                // encoding. Ctrl+Alt combos fall through to the encoder
                // (Codex binds paste-image to Ctrl+Alt+V).
                if ctrl && !m.alt {
```

In `encode_key`, insert a ctrl+alt branch **before** the existing ctrl-only branch (inside the `b.len() == 1` block):

```rust
                // Ctrl+Alt+letter → ESC + control code (meta over the ctrl
                // code); Codex's paste-image needs Ctrl+Alt+V = 1b 16.
                if ctrl && mods.alt {
                    let up = b[0].to_ascii_uppercase();
                    if up.is_ascii_uppercase() {
                        return vec![0x1b, up - 0x40];
                    }
                }
```

(The existing ctrl-only branch already guards `ctrl && !mods.alt`; leave it.)

- [ ] **Step 4: Run tests** — `cargo test --lib input` — expected: PASS. Then `cargo test` — green.

- [ ] **Step 5: Commit**

```powershell
git add src/input.rs
git commit -m "feat(input): route Ctrl+Alt chords to the app as meta control codes

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Image-only Ctrl+V forwards 0x16 (src/input.rs + src/terminal.rs)

**Files:**
- Modify: `src/input.rs` (`Event::Paste` arm)
- Modify: `src/terminal.rs` (`read_input` paste fallback, new `clipboard_has_image` beside `read_clipboard`)

**Interfaces:**
- Consumes: `InputOutcome.paste_clipboard`, `read_clipboard() -> Option<String>`.
- Produces: `fn clipboard_has_image() -> bool` (terminal.rs, private, arboard-backed).

- [ ] **Step 1: Write the failing test** (input.rs — the pure half: an empty Paste event must not satisfy the clipboard request)

```rust
    #[test]
    fn empty_paste_event_still_flags_clipboard_read() {
        // With an image-only clipboard, egui may deliver Paste("") for Ctrl+V.
        // The empty event must neither type anything nor satisfy the request —
        // the shell then falls back to the clipboard (text, else image → 0x16).
        let live = egui::Modifiers { ctrl: true, ..Default::default() };
        let out = process_input(
            &[key_ev(Key::V, mods(true, false, false)), Event::Paste(String::new())],
            live,
            TermMode::empty(),
            false,
        );
        assert!(out.paste_clipboard);
        assert!(out.pty_bytes.is_empty());
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib input` — expected: FAIL (`paste_clipboard` is false: the empty paste set `saw_paste`).

- [ ] **Step 3: Implement.** Change the `Event::Paste` arm:

```rust
            Event::Paste(s) if !s.is_empty() => {
                out.pty_bytes.extend_from_slice(&paste_seq(mode, s));
                saw_paste = true;
            }
            Event::Paste(_) => {} // empty paste (image-only clipboard) — fall through
```

In `src/terminal.rs`, add beside `read_clipboard`:

```rust
fn clipboard_has_image() -> bool {
    arboard::Clipboard::new().is_ok_and(|mut c| c.get_image().is_ok())
}
```

And in `read_input`, extend the fallback:

```rust
        if outcome.paste_clipboard {
            if let Some(txt) = read_clipboard() {
                bytes.extend_from_slice(&crate::input::paste_seq(mode, &txt));
            } else if clipboard_has_image() {
                // Image-only clipboard: forward raw Ctrl+V so agents (Claude,
                // Codex) run their native clipboard-image paste. Plain shells
                // see readline quoted-insert — harmless. (spec WS2)
                bytes.push(0x16);
            }
        }
```

- [ ] **Step 4: Run tests** — `cargo test` — expected: green (the arboard helper is thin-shell, exercised in Task 12's acceptance pass).

- [ ] **Step 5: Commit**

```powershell
git add src/input.rs src/terminal.rs
git commit -m "feat(input): image-only Ctrl+V forwards 0x16 for agent-native image paste

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: APC scanner (new src/graphics.rs — no new deps yet)

**Files:**
- Create: `src/graphics.rs`
- Modify: `src/main.rs` (add `mod graphics;` beside the other module declarations)

**Interfaces:**
- Produces (used by Tasks 6–9):
  - `pub struct Graphics` (`Default`)
  - `pub struct Cut { pub offset: usize }` — index just past a completed command's `ESC \`
  - `pub fn Graphics::feed(&mut self, chunk: &[u8]) -> Vec<Cut>`
  - invariant: **one `Cut` per completed command, queued FIFO**; `feed` never allocates when the chunk contains no graphics.

- [ ] **Step 1: Create the module with types and failing tests**

`src/graphics.rs` (initial content — scanner skeleton with `todo!()`-free stub that returns no cuts, plus tests):

```rust
//! Kitty graphics protocol — pets-first subset.
//! Spec: docs/superpowers/specs/2026-07-02-terminal-image-support-design.md
//!
//! Pure module: no I/O, no egui, no alacritty mutation. `Session::pump` feeds
//! it the same PTY bytes alacritty parses (alacritty's vte discards APC, so
//! the grid never sees these sequences either way); `Session::show` asks what
//! to paint. Unsupported commands are skipped silently — the failure mode is
//! "image doesn't show", never a corrupted pane.

use std::collections::{HashMap, VecDeque};

/// One APC sequence's buffered payload cap.
const MAX_APC: usize = 8 * 1024 * 1024;

/// Byte offset in the fed chunk just past a completed command's `ESC \`.
/// `pump` advances alacritty over `chunk[..offset]`, then samples the cursor.
pub struct Cut {
    pub offset: usize,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Scan {
    #[default]
    Ground,
    Esc,
    Apc,
    ApcEsc,
}

#[derive(Default)]
pub struct Graphics {
    scan: Scan,
    /// Current APC body (starting at its 'G'), buffered only for graphics APCs.
    apc: Vec<u8>,
    seen_first: bool,
    is_graphics: bool,
    overflow: bool,
    /// Completed-but-unapplied commands, FIFO; one per emitted `Cut`.
    pending: VecDeque<Cmd>,
}

enum Cmd {
    /// Placeholder until Task 6 — a completed graphics APC body.
    Raw(Vec<u8>),
}

impl Graphics {
    /// Scan one PTY chunk. Ground state fast-skips via byte search, so plain
    /// text costs one memchr-style pass and zero allocations.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Cut> {
        let mut cuts = Vec::new();
        let mut i = 0;
        while i < chunk.len() {
            match self.scan {
                Scan::Ground => match chunk[i..].iter().position(|&c| c == 0x1b) {
                    Some(off) => {
                        i += off + 1;
                        self.scan = Scan::Esc;
                    }
                    None => break,
                },
                Scan::Esc => {
                    match chunk[i] {
                        b'_' => {
                            self.apc.clear();
                            self.seen_first = false;
                            self.is_graphics = false;
                            self.overflow = false;
                            self.scan = Scan::Apc;
                        }
                        0x1b => {} // ESC ESC — stay in Esc for the next byte
                        _ => self.scan = Scan::Ground,
                    }
                    i += 1;
                }
                Scan::Apc => {
                    let end = chunk[i..]
                        .iter()
                        .position(|&c| c == 0x1b)
                        .map(|o| i + o)
                        .unwrap_or(chunk.len());
                    if i < end && !self.seen_first {
                        self.seen_first = true;
                        self.is_graphics = chunk[i] == b'G';
                    }
                    if self.is_graphics && !self.overflow {
                        if self.apc.len() + (end - i) > MAX_APC {
                            self.overflow = true;
                            self.apc.clear();
                        } else {
                            self.apc.extend_from_slice(&chunk[i..end]);
                        }
                    }
                    if end < chunk.len() {
                        self.scan = Scan::ApcEsc;
                        i = end + 1;
                    } else {
                        i = chunk.len();
                    }
                }
                Scan::ApcEsc => {
                    match chunk[i] {
                        b'\\' => {
                            // Sequence complete just past this byte.
                            if self.is_graphics && !self.overflow && self.ingest_apc() {
                                cuts.push(Cut { offset: i + 1 });
                            }
                            self.scan = Scan::Ground;
                        }
                        0x1b => {
                            // ESC ESC inside the APC: previous ESC was content
                            // we ignore; this one may still start the ST.
                        }
                        _ => self.scan = Scan::Ground, // aborted APC — discard
                    }
                    i += 1;
                }
            }
        }
        cuts
    }

    /// Parse a complete graphics APC body (`self.apc`, starting at 'G').
    /// Returns true when a command completed (caller emits a `Cut`).
    fn ingest_apc(&mut self) -> bool {
        let body = std::mem::take(&mut self.apc);
        self.pending.push_back(Cmd::Raw(body));
        true
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEQ: &[u8] = b"\x1b_Ga=T,t=d,f=32,s=1,v=1,c=1,r=1,q=2;AAAAAA==\x1b\\";

    #[test]
    fn plain_text_yields_no_cuts_and_buffers_nothing() {
        let mut g = Graphics::default();
        let cuts = g.feed(b"the quick brown fox\r\njumps over\x1b[31mred\x1b[0m");
        assert!(cuts.is_empty());
        assert_eq!(g.pending_len(), 0);
    }

    #[test]
    fn one_sequence_yields_one_cut_just_past_the_terminator() {
        let mut g = Graphics::default();
        let mut input = b"before".to_vec();
        input.extend_from_slice(SEQ);
        input.extend_from_slice(b"after");
        let cuts = g.feed(&input);
        assert_eq!(cuts.len(), 1);
        assert_eq!(cuts[0].offset, 6 + SEQ.len());
        assert_eq!(g.pending_len(), 1);
    }

    #[test]
    fn sequence_split_at_every_byte_boundary_still_completes() {
        for split in 1..SEQ.len() {
            let mut g = Graphics::default();
            let a = g.feed(&SEQ[..split]);
            let b = g.feed(&SEQ[split..]);
            assert!(a.is_empty(), "no cut before the terminator (split {split})");
            assert_eq!(b.len(), 1, "split {split}");
            assert_eq!(b[0].offset, SEQ.len() - split, "split {split}");
        }
    }

    #[test]
    fn non_graphics_apc_is_ignored() {
        let mut g = Graphics::default();
        let cuts = g.feed(b"\x1b_Xsome other apc\x1b\\text");
        assert!(cuts.is_empty());
        assert_eq!(g.pending_len(), 0);
    }

    #[test]
    fn lone_esc_at_chunk_end_does_not_lose_state() {
        let mut g = Graphics::default();
        assert!(g.feed(b"text\x1b").is_empty());
        assert!(g.feed(b"[31m more").is_empty()); // it was a CSI, not an APC
        assert!(g.feed(b"\x1b").is_empty());
        let cuts = g.feed(&SEQ[1..]); // rest of a graphics APC after the ESC
        assert_eq!(cuts.len(), 1);
    }

    #[test]
    fn oversized_apc_is_discarded_not_buffered() {
        let mut g = Graphics::default();
        let mut input = b"\x1b_G".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_APC + 2));
        input.extend_from_slice(b"\x1b\\");
        let cuts = g.feed(&input);
        assert!(cuts.is_empty());
        assert_eq!(g.pending_len(), 0);
    }
}
```

Add `mod graphics;` to `src/main.rs` next to the existing `mod` declarations (e.g. after `mod geom;`).

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib graphics`
Expected: all 6 PASS. (This task's code is written test-first as one unit because the state machine is unsplittable; the tests were still written before running anything.)

- [ ] **Step 3: Full suite + warnings check** — `cargo test` green; `cargo build 2>&1 | Select-Object -Last 20` — no new warnings beyond the project baseline.

- [ ] **Step 4: Commit**

```powershell
git add src/graphics.rs src/main.rs
git commit -m "feat(graphics): resumable APC scanner for kitty graphics sequences

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Kitty header parsing, chunk reassembly, command queue (dep: base64)

**Files:**
- Modify: `Cargo.toml` (add `base64 = "0.22"`)
- Modify: `src/graphics.rs` (replace `Cmd::Raw` with real commands; add `Header`, `parse_header`, chunk reassembly, `reply`)

**Interfaces:**
- Consumes: scanner + `ingest_apc` hook from Task 5.
- Produces (used by Task 7):
  - `struct Header { action: u8, medium: u8, format: u32, cols: u16, rows: u16, pix_w: u32, pix_h: u32, id: Option<u32>, more: bool, quiet: u8, delete: u8 }`
  - `enum Cmd { Transmit { header: Header, payload: Vec<u8>, display: bool }, Delete { spec: u8, id: Option<u32> }, Query { header: Header, payload: Vec<u8> }, Nop { reply: Option<Vec<u8>> } }`
  - `fn reply(out: &mut Vec<u8>, id: Option<u32>, msg: &[u8])` — emits `ESC _ G [i=<id>] ; <msg> ESC \`
  - Chunking invariant: `m=1` transmits buffer without a `Cut`; only the final `m=0` chunk completes the command and cuts.

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` `[dependencies]` (alphabetical, after `arboard`):

```toml
base64 = "0.22"
```

- [ ] **Step 2: Write the failing tests** (append to `src/graphics.rs` tests; the exact wire format is copied from Codex's own emission tests)

```rust
    // Codex chunk format: first chunk carries the full header + m=1,
    // continuations are ESC_Gm=<flag>;<chunk>ESC\ (see codex image_protocol.rs).
    #[test]
    fn chunked_transmit_completes_only_on_the_final_chunk() {
        let mut g = Graphics::default();
        let c1 = g.feed(b"\x1b_Ga=T,t=d,f=100,c=4,r=3,q=2,i=9,m=1;cG5n\x1b\\");
        assert!(c1.is_empty());
        let c2 = g.feed(b"\x1b_Gm=1;cG5n\x1b\\");
        assert!(c2.is_empty());
        let c3 = g.feed(b"\x1b_Gm=0;cG5n\x1b\\");
        assert_eq!(c3.len(), 1);
        assert_eq!(g.pending_len(), 1);
    }

    #[test]
    fn delete_by_id_parses_codex_format() {
        let mut g = Graphics::default();
        let cuts = g.feed(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\");
        assert_eq!(cuts.len(), 1);
        assert_eq!(g.pending_len(), 1);
    }

    #[test]
    fn unknown_action_queues_a_nop() {
        let mut g = Graphics::default();
        let cuts = g.feed(b"\x1b_Ga=z,q=2;\x1b\\");
        assert_eq!(cuts.len(), 1); // one cut per completed command, even a nop
        assert_eq!(g.pending_len(), 1);
    }
```

- [ ] **Step 3: Run to verify failure** — `cargo test --lib graphics` — Task 5's `Cmd::Raw` makes chunking tests fail (3 cuts instead of 1).

- [ ] **Step 4: Implement.** Replace `enum Cmd` and `ingest_apc`; add the header machinery:

```rust
#[derive(Default, Clone)]
struct Header {
    action: u8,
    medium: u8,
    format: u32,
    cols: u16,
    rows: u16,
    pix_w: u32,
    pix_h: u32,
    id: Option<u32>,
    more: bool,
    quiet: u8,
    delete: u8,
}

enum Cmd {
    Transmit { header: Header, payload: Vec<u8>, display: bool },
    Delete { spec: u8, id: Option<u32> },
    Query { header: Header, payload: Vec<u8> },
    Nop { reply: Option<Vec<u8>> },
}

/// Kitty `k=v,k=v` header. Unknown keys are ignored — tolerance is the spec's
/// safety story ("silently skip, never corrupt").
fn parse_header(h: &[u8]) -> Header {
    let mut out = Header { action: b't', medium: b'd', format: 32, ..Default::default() };
    for kv in h.split(|&b| b == b',') {
        let mut it = kv.splitn(2, |&b| b == b'=');
        let (Some(k), Some(v)) = (it.next(), it.next()) else { continue };
        let num = |v: &[u8]| std::str::from_utf8(v).ok().and_then(|s| s.parse::<u32>().ok());
        match k {
            b"a" => out.action = v.first().copied().unwrap_or(b't'),
            b"t" => out.medium = v.first().copied().unwrap_or(b'd'),
            b"f" => out.format = num(v).unwrap_or(32),
            b"c" => out.cols = num(v).unwrap_or(0) as u16,
            b"r" => out.rows = num(v).unwrap_or(0) as u16,
            b"s" => out.pix_w = num(v).unwrap_or(0),
            b"v" => out.pix_h = num(v).unwrap_or(0),
            b"i" => out.id = num(v),
            b"m" => out.more = num(v) == Some(1),
            b"q" => out.quiet = num(v).unwrap_or(0) as u8,
            b"d" => out.delete = v.first().copied().unwrap_or(b'a'),
            _ => {}
        }
    }
    out
}

fn reply(out: &mut Vec<u8>, id: Option<u32>, msg: &[u8]) {
    out.extend_from_slice(b"\x1b_G");
    if let Some(id) = id {
        out.extend_from_slice(format!("i={id}").as_bytes());
    }
    out.push(b';');
    out.extend_from_slice(msg);
    out.extend_from_slice(b"\x1b\\");
}
```

New `Graphics` field: `chunk: Option<(Header, Vec<u8>)>,` and the real `ingest_apc`:

```rust
    /// Parse a complete graphics APC body (`self.apc`, starting at 'G').
    /// Returns true when a command completed (caller emits a `Cut`).
    fn ingest_apc(&mut self) -> bool {
        let body = std::mem::take(&mut self.apc);
        let rest = &body[1..]; // strip the 'G' (guaranteed by is_graphics)
        let (header, payload) = match rest.iter().position(|&b| b == b';') {
            Some(p) => (&rest[..p], &rest[p + 1..]),
            None => (rest, &rest[..0]),
        };
        let h = parse_header(header);

        // Continuation of a chunked transmit: only m matters (codex sends bare
        // `m=<flag>;<chunk>` continuations); finish on m=0.
        if let Some((_, data)) = self.chunk.as_mut() {
            if data.len() + payload.len() > MAX_APC {
                self.chunk = None; // runaway chunk stream — drop it whole
                return false;
            }
            data.extend_from_slice(payload);
            if h.more {
                return false;
            }
            let (first, data) = self.chunk.take().expect("checked above");
            let display = first.action == b'T';
            self.pending.push_back(Cmd::Transmit { header: first, payload: data, display });
            return true;
        }

        match h.action {
            b'T' | b't' => {
                if h.more {
                    self.chunk = Some((h, payload.to_vec()));
                    return false;
                }
                let display = h.action == b'T';
                self.pending.push_back(Cmd::Transmit { header: h, payload: payload.to_vec(), display });
                true
            }
            b'd' => {
                self.pending.push_back(Cmd::Delete { spec: h.delete, id: h.id });
                true
            }
            b'q' => {
                self.pending.push_back(Cmd::Query { header: h, payload: payload.to_vec() });
                true
            }
            _ => {
                // Unsupported action: skip silently; honest error only when the
                // client asked for responses (q<2 suppresses nothing... q=2 all).
                let r = (h.quiet < 2).then(|| {
                    let mut v = Vec::new();
                    reply(&mut v, h.id, b"ENOTSUPPORTED");
                    v
                });
                self.pending.push_back(Cmd::Nop { reply: r });
                true
            }
        }
    }
```

- [ ] **Step 5: Run tests** — `cargo test --lib graphics` — all PASS (including Task 5's).

- [ ] **Step 6: Commit**

```powershell
git add Cargo.toml Cargo.lock src/graphics.rs
git commit -m "feat(graphics): kitty header parsing and chunked-transmit reassembly

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Decode, store, apply, visible (dep: png)

**Files:**
- Modify: `Cargo.toml` (add `png = "0.17"`)
- Modify: `src/graphics.rs` (add `TermView`, `ViewportView`, `Placed`, `Image`, `Placement`, `apply`, `visible`, `active`, `has_image`, decode + quota)

**Interfaces:**
- Consumes: `Cmd` queue from Task 6.
- Produces (used by Tasks 8–9):
  - `pub struct TermView { pub cursor_col: usize, pub cursor_line: usize, pub alt_screen: bool, pub history_size: usize }`
  - `pub struct ViewportView { pub alt_screen: bool, pub history_size: usize, pub display_offset: usize, pub screen_lines: usize }`
  - `pub struct Placed<'a> { pub id: u32, pub gen: u64, pub col: usize, pub line: isize, pub cols: u16, pub rows: u16, pub w: u32, pub h: u32, pub rgba: &'a [u8] }` (`cols`/`rows` of 0 = caller derives the span from pixels)
  - `pub fn apply(&mut self, view: TermView, out: &mut Vec<u8>)` — pops ONE pending command (one per `Cut`, in order)
  - `pub fn visible(&self, v: &ViewportView) -> Vec<Placed<'_>>`
  - `pub fn active(&self) -> bool` (any placements — the paint guard)
  - `pub fn has_image(&self, id: u32) -> bool` (texture-cache retention)

- [ ] **Step 1: Add the dependency** — in `Cargo.toml` (alphabetical):

```toml
png = "0.17"
```

- [ ] **Step 2: Write the failing tests**

```rust
    use base64::Engine as _;

    /// 2x2 raw RGBA red square, transmitted+displayed with the given id.
    fn red_transmit(id: u32) -> Vec<u8> {
        let rgba: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 4).flatten().collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
        format!("\x1b_Ga=T,t=d,f=32,s=2,v=2,c=2,r=1,q=2,i={id};{b64}\x1b\\").into_bytes()
    }

    fn view(col: usize, line: usize) -> TermView {
        TermView { cursor_col: col, cursor_line: line, alt_screen: false, history_size: 0 }
    }

    const VP: ViewportView =
        ViewportView { alt_screen: false, history_size: 0, display_offset: 0, screen_lines: 40 };

    #[test]
    fn transmit_places_at_the_sampled_cursor() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        assert_eq!(g.feed(&red_transmit(5)).len(), 1);
        g.apply(view(3, 2), &mut out);
        assert!(out.is_empty()); // q=2 — silent
        let vis = g.visible(&VP);
        assert_eq!(vis.len(), 1);
        assert_eq!((vis[0].col, vis[0].line), (3, 2));
        assert_eq!((vis[0].w, vis[0].h), (2, 2));
        assert_eq!(vis[0].rgba[0..4], [255, 0, 0, 255]);
        assert!(g.active());
    }

    #[test]
    fn delete_by_id_removes_placement_and_frees_data() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(&red_transmit(7));
        g.apply(view(0, 0), &mut out);
        g.feed(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\");
        g.apply(view(0, 0), &mut out);
        assert!(g.visible(&VP).is_empty());
        assert!(!g.has_image(7));
        assert!(!g.active());
    }

    #[test]
    fn primary_screen_placement_scrolls_with_history() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(&red_transmit(1));
        // placed at line 30 when history was 100
        g.apply(TermView { cursor_col: 0, cursor_line: 30, alt_screen: false, history_size: 100 }, &mut out);
        // 15 more lines scrolled into history since
        let v = ViewportView { alt_screen: false, history_size: 115, display_offset: 0, screen_lines: 40 };
        assert_eq!(g.visible(&v)[0].line, 15);
        // scrolling back 5 lines shifts it back down
        let v = ViewportView { alt_screen: false, history_size: 115, display_offset: 5, screen_lines: 40 };
        assert_eq!(g.visible(&v)[0].line, 20);
    }

    #[test]
    fn alt_screen_placements_only_show_on_the_alt_screen() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(&red_transmit(2));
        g.apply(TermView { cursor_col: 1, cursor_line: 1, alt_screen: true, history_size: 0 }, &mut out);
        assert!(g.visible(&VP).is_empty()); // primary viewport
        let alt = ViewportView { alt_screen: true, ..VP };
        assert_eq!(g.visible(&alt).len(), 1);
    }

    #[test]
    fn transmit_with_q0_replies_ok_and_bad_payload_replies_error() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        let rgba = [255u8, 0, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(rgba);
        g.feed(format!("\x1b_Ga=T,t=d,f=32,s=1,v=1,i=5;{b64}\x1b\\").as_bytes());
        g.apply(view(0, 0), &mut out);
        assert_eq!(out, b"\x1b_Gi=5;OK\x1b\\");
        out.clear();
        g.feed(b"\x1b_Ga=T,t=d,f=32,s=9,v=9,i=6;AAAA\x1b\\"); // wrong size for 9x9
        g.apply(view(0, 0), &mut out);
        assert_eq!(out, b"\x1b_Gi=6;EBADRAW\x1b\\");
    }

    #[test]
    fn codex_style_query_probe_gets_ok() {
        // ratatui-image / codex probe: 1x1 f=24 with payload AAAA.
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
        g.apply(view(0, 0), &mut out);
        assert_eq!(out, b"\x1b_Gi=31;OK\x1b\\");
    }

    #[test]
    fn png_transmit_decodes() {
        // A real PNG encoded in the test to avoid a fixture: 1x1 opaque white.
        // Generated with the png crate itself so the bytes are always valid.
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, 1, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[255, 255, 255, 255]).unwrap();
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        let mut g = Graphics::default();
        let mut out = Vec::new();
        g.feed(format!("\x1b_Ga=T,t=d,f=100,c=4,r=2,q=2,i=8;{b64}\x1b\\").as_bytes());
        g.apply(view(0, 0), &mut out);
        let vis = g.visible(&VP);
        assert_eq!((vis[0].w, vis[0].h), (1, 1));
        assert_eq!((vis[0].cols, vis[0].rows), (4, 2));
        assert_eq!(vis[0].rgba, [255, 255, 255, 255]);
    }

    #[test]
    fn store_quota_evicts_oldest_image() {
        let mut g = Graphics::default();
        let mut out = Vec::new();
        // Each image is 1024x1024 RGBA = 4 MiB decoded; 17 of them > 64 MiB.
        let rgba = vec![128u8; 1024 * 1024 * 4];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
        for id in 1..=17u32 {
            g.feed(format!("\x1b_Ga=T,t=d,f=32,s=1024,v=1024,q=2,i={id};{b64}\x1b\\").as_bytes());
            g.apply(view(0, 0), &mut out);
        }
        assert!(!g.has_image(1), "oldest image evicted past the quota");
        assert!(g.has_image(17));
    }
```

- [ ] **Step 3: Run to verify failure** — `cargo test --lib graphics` — new tests fail to compile (missing types) → add the implementation.

- [ ] **Step 4: Implement.** Add to `src/graphics.rs`:

```rust
/// Decoded-RGBA quota per session; oldest images evicted past this.
const MAX_STORE: usize = 64 * 1024 * 1024;
/// Placement cap — a misbehaving client can't accumulate unbounded overlays.
const MAX_PLACEMENTS: usize = 64;
/// Placements scrolled further than this into history are dropped for good.
const MAX_SCROLL_KEEP: usize = 10_000;
/// Ids we assign when the client omits i= (top bit dodges client-chosen ids).
const ANON_BASE: u32 = 0x8000_0000;

/// Term facts sampled at a cut (built by terminal.rs `term_view`).
pub struct TermView {
    pub cursor_col: usize,
    pub cursor_line: usize,
    pub alt_screen: bool,
    pub history_size: usize,
}

/// Viewport facts sampled at paint time.
pub struct ViewportView {
    pub alt_screen: bool,
    pub history_size: usize,
    pub display_offset: usize,
    pub screen_lines: usize,
}

/// One image to paint. `line` is a viewport row and may be negative (partially
/// scrolled off the top). `cols`/`rows` of 0 = derive the span from pixels.
pub struct Placed<'a> {
    pub id: u32,
    pub gen: u64,
    pub col: usize,
    pub line: isize,
    pub cols: u16,
    pub rows: u16,
    pub w: u32,
    pub h: u32,
    pub rgba: &'a [u8],
}

struct Image {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    gen: u64,
}

struct Placement {
    img: u32,
    col: usize,
    line: usize,
    alt: bool,
    /// history_size when placed — primary-screen placements scroll by the delta.
    history: usize,
    cols: u16,
    rows: u16,
}
```

New `Graphics` fields:

```rust
    images: HashMap<u32, Image>,
    placements: Vec<Placement>,
    next_anon: u32,
    gen: u64,
    store_bytes: usize,
```

Methods:

```rust
    /// Apply the next pending command using sampled term facts. Called exactly
    /// once per `Cut`, in order (see terminal.rs `advance_scanned`).
    pub fn apply(&mut self, view: TermView, out: &mut Vec<u8>) {
        let Some(cmd) = self.pending.pop_front() else { return };
        match cmd {
            Cmd::Transmit { header, payload, display } => match decode_image(&header, &payload) {
                Ok((w, h, rgba)) => {
                    let id = header.id.unwrap_or_else(|| {
                        self.next_anon = self.next_anon.wrapping_add(1);
                        ANON_BASE | self.next_anon
                    });
                    self.gen += 1;
                    self.store_bytes += rgba.len();
                    if let Some(old) = self.images.insert(id, Image { rgba, w, h, gen: self.gen }) {
                        self.store_bytes -= old.rgba.len();
                    }
                    self.evict_over_quota();
                    if display {
                        // Retransmit with the same id replaces its placement
                        // (pets deletes first anyway; keeps rogue clients bounded).
                        self.placements.retain(|p| p.img != id);
                        self.placements.push(Placement {
                            img: id,
                            col: view.cursor_col,
                            line: view.cursor_line,
                            alt: view.alt_screen,
                            history: view.history_size,
                            cols: header.cols,
                            rows: header.rows,
                        });
                        if self.placements.len() > MAX_PLACEMENTS {
                            self.placements.remove(0);
                        }
                    }
                    if header.quiet == 0 {
                        reply(out, header.id, b"OK");
                    }
                }
                Err(e) => {
                    if header.quiet < 2 {
                        reply(out, header.id, e.as_bytes());
                    }
                }
            },
            Cmd::Delete { spec, id } => match (spec, id) {
                (b'I' | b'i', Some(id)) => {
                    self.placements.retain(|p| p.img != id);
                    if spec == b'I'
                        && let Some(img) = self.images.remove(&id)
                    {
                        self.store_bytes -= img.rgba.len();
                    }
                }
                _ => self.placements.clear(), // bare a=d / d=a: clear visible
            },
            Cmd::Query { header, payload } => {
                if header.quiet < 2 {
                    let ok = header.medium == b'd'
                        && matches!(header.format, 24 | 32 | 100)
                        && decode_image(&header, &payload).is_ok();
                    reply(out, header.id, if ok { b"OK" } else { b"ENOTSUPPORTED" });
                }
            }
            Cmd::Nop { reply: r } => {
                if let Some(r) = r {
                    out.extend_from_slice(&r);
                }
            }
        }
        // Scroll-cull: placements far into history can never return to view.
        let hist = view.history_size;
        self.placements
            .retain(|p| p.alt || hist.saturating_sub(p.history) < MAX_SCROLL_KEEP);
    }

    /// What's visible right now. `line` already accounts for scrollback offset;
    /// the painter clips partially-visible images.
    pub fn visible(&self, v: &ViewportView) -> Vec<Placed<'_>> {
        let mut out = Vec::new();
        for p in &self.placements {
            if p.alt != v.alt_screen {
                continue;
            }
            let line = if p.alt {
                p.line as isize
            } else {
                p.line as isize - (v.history_size - p.history) as isize
                    + v.display_offset as isize
            };
            if line >= v.screen_lines as isize || line < -300 {
                continue; // fully below, or absurdly far above (max rows is 300)
            }
            let Some(img) = self.images.get(&p.img) else { continue };
            out.push(Placed {
                id: p.img,
                gen: img.gen,
                col: p.col,
                line,
                cols: p.cols,
                rows: p.rows,
                w: img.w,
                h: img.h,
                rgba: &img.rgba,
            });
        }
        out
    }

    /// Paint guard: `show` skips all image work when this is false.
    pub fn active(&self) -> bool {
        !self.placements.is_empty()
    }

    /// Texture-cache retention (terminal.rs drops textures for gone images).
    pub fn has_image(&self, id: u32) -> bool {
        self.images.contains_key(&id)
    }

    fn evict_over_quota(&mut self) {
        while self.store_bytes > MAX_STORE && self.images.len() > 1 {
            let Some((&id, _)) = self.images.iter().min_by_key(|(_, i)| i.gen) else { break };
            if let Some(img) = self.images.remove(&id) {
                self.store_bytes -= img.rgba.len();
            }
            self.placements.retain(|p| p.img != id);
        }
    }
```

Decoders (free functions):

```rust
fn decode_image(h: &Header, b64: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "EBASE64")?;
    let px = (h.pix_w as usize, h.pix_h as usize);
    match h.format {
        100 => decode_png(&data),
        32 => {
            if px.0 == 0 || px.1 == 0 || data.len() != px.0 * px.1 * 4 {
                return Err("EBADRAW");
            }
            Ok((h.pix_w, h.pix_h, data))
        }
        24 => {
            if px.0 == 0 || px.1 == 0 || data.len() != px.0 * px.1 * 3 {
                return Err("EBADRAW");
            }
            let mut rgba = Vec::with_capacity(px.0 * px.1 * 4);
            for p in data.chunks_exact(3) {
                rgba.extend_from_slice(p);
                rgba.push(255);
            }
            Ok((h.pix_w, h.pix_h, rgba))
        }
        _ => Err("ENOTSUPPORTED"),
    }
}

fn decode_png(data: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
    let mut dec = png::Decoder::new(data);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|_| "EBADPNG")?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|_| "EBADPNG")?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    match info.color_type {
        png::ColorType::Rgba => Ok((w, h, buf)),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(buf.len() / 3 * 4);
            for p in buf.chunks_exact(3) {
                rgba.extend_from_slice(p);
                rgba.push(255);
            }
            Ok((w, h, rgba))
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(buf.len() * 2);
            for p in buf.chunks_exact(2) {
                rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
            Ok((w, h, rgba))
        }
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity(buf.len() * 4);
            for &v in &buf {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
            Ok((w, h, rgba))
        }
        // Indexed is EXPANDed away by Transformations::EXPAND.
        _ => Err("EBADPNG"),
    }
}
```

Also change `Cmd::Delete` construction in `ingest_apc` (Task 6 wrote `Delete { spec, id }` — verify it matches; `quiet` is not needed on Delete since kitty deletes reply nothing on success).

- [ ] **Step 5: Run tests** — `cargo test --lib graphics` — all PASS. Then `cargo test` — green.

- [ ] **Step 6: Commit**

```powershell
git add Cargo.toml Cargo.lock src/graphics.rs
git commit -m "feat(graphics): image decode, store with quota, placements, apply/visible

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: Session integration — segmented pump (src/terminal.rs)

**Files:**
- Modify: `src/terminal.rs` (`Session` struct + `spawn_with` init, `pump`, new free functions `advance_scanned` + `term_view`)

**Interfaces:**
- Consumes: `Graphics::{feed, apply}`, `TermView` (Task 7); `Session.parser: Processor`, `Session.term: Term<Listener>`.
- Produces (used by Task 9): `Session.graphics: crate::graphics::Graphics` field; `pub(crate)` is unnecessary — same file.

- [ ] **Step 1: Write the failing test** (terminal.rs `mod tests` — uses existing `term_with`/`grid_row` helpers and `Term<VoidListener>`, the sanctioned pure-parse pattern)

```rust
    #[test]
    fn advance_scanned_places_at_the_cursor_where_the_command_completed() {
        use base64::Engine as _;
        let mut term = term_with(b"", 40, 10);
        let mut parser: Processor = Processor::new();
        let mut g = crate::graphics::Graphics::default();
        let mut replies = Vec::new();

        let rgba = [255u8, 0, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(rgba);
        // Move to row 5, col 10 (1-based CUP), then place, then keep printing —
        // all in ONE chunk. The placement must sample the moved cursor, and the
        // trailing text must land where the app expects (grid untouched by APC).
        let bytes = format!(
            "AB\x1b[5;10H\x1b_Ga=T,t=d,f=32,s=1,v=1,c=2,r=1,q=2,i=3;{b64}\x1b\\tail"
        );
        advance_scanned(&mut parser, &mut term, &mut g, bytes.as_bytes(), &mut replies);

        assert!(replies.is_empty());
        let vp = crate::graphics::ViewportView {
            alt_screen: false,
            history_size: 0,
            display_offset: 0,
            screen_lines: 10,
        };
        let vis = g.visible(&vp);
        assert_eq!(vis.len(), 1);
        assert_eq!((vis[0].col, vis[0].line), (9, 4)); // CUP 5;10 is 0-based (4,9)
        assert!(grid_row(&term, 0).starts_with("AB"));
        assert!(grid_row(&term, 4).contains("tail")); // vte ignored the APC
    }
```

(Check `grid_row`'s exact signature in the tests module first and match it — it exists and is used by the selection tests.)

- [ ] **Step 2: Run to verify failure** — `cargo test --lib terminal` — FAIL: `advance_scanned` not found.

- [ ] **Step 3: Implement.** Free functions above `impl Session` (import `alacritty_terminal::event::EventListener` — check the existing `use` block; `Listener` implements it):

```rust
/// Advance the parser over `bytes`, splitting at graphics cuts so each command
/// samples the cursor exactly where it completed in the stream (spec WS3).
/// alacritty sees byte-identical input — only the advance() boundaries move,
/// and chunk boundaries already occur anywhere. Zero cuts = today's code path.
fn advance_scanned<L: EventListener>(
    parser: &mut Processor,
    term: &mut Term<L>,
    graphics: &mut crate::graphics::Graphics,
    bytes: &[u8],
    replies: &mut Vec<u8>,
) {
    let cuts = graphics.feed(bytes);
    let mut at = 0;
    for cut in cuts {
        parser.advance(term, &bytes[at..cut.offset]);
        at = cut.offset;
        graphics.apply(term_view(term), replies);
    }
    parser.advance(term, &bytes[at..]);
}

fn term_view<L: EventListener>(term: &Term<L>) -> crate::graphics::TermView {
    let g = term.grid();
    crate::graphics::TermView {
        cursor_col: g.cursor.point.column.0,
        cursor_line: g.cursor.point.line.0.max(0) as usize,
        alt_screen: term.mode().contains(TermMode::ALT_SCREEN),
        history_size: g.history_size(),
    }
}
```

Add the field to `Session` (after `caret`):

```rust
    /// Kitty graphics state: overlay images only — the grid stays pure text.
    /// See src/graphics.rs and the spec.
    graphics: crate::graphics::Graphics,
```

Initialize `graphics: crate::graphics::Graphics::default(),` in `spawn_with`'s struct literal (find the literal; it lists every field).

Rewrite the byte loop in `pump`:

```rust
    fn pump(&mut self) {
        let mut greplies = Vec::new();
        while let Ok(bytes) = self.rx.try_recv() {
            advance_scanned(&mut self.parser, &mut self.term, &mut self.graphics, &bytes, &mut greplies);
            self.output_gen = self.output_gen.wrapping_add(1);
        }
        // Graphics replies (a=q probes etc.) go straight back to the app — NOT
        // via `resp`: that buffer's flush is what latches `ready` (the DSR
        // contract), and a graphics reply must never fake readiness.
        if !greplies.is_empty() {
            let _ = self.writer.write_all(&greplies);
            let _ = self.writer.flush();
        }
        // ... rest of pump unchanged (resp flush, ready latch, pending_inject,
        // pending_submit)
    }
```

- [ ] **Step 4: Run tests** — `cargo test --lib terminal` then `cargo test` — green (watch the PTY tests: none should change behavior; the DSR/ready path is untouched).

- [ ] **Step 5: Commit**

```powershell
git add src/terminal.rs
git commit -m "feat(terminal): segmented parser advance feeds the graphics scanner

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Overlay rendering + texture cache (src/terminal.rs)

**Files:**
- Modify: `src/terminal.rs` (`Session` struct: `textures` field; `Session::show`: paint block)

**Interfaces:**
- Consumes: `Graphics::{active, visible, has_image}`, `Placed` (Task 7); `show`'s in-scope `rect`, `cw`, `rh`, `painter` (an `egui::Painter` from `ui.painter_at(rect)` — clips to the pane).
- Produces: visible images. No new public surface.

- [ ] **Step 1: Add the texture cache field** to `Session` (after `graphics`):

```rust
    /// egui textures for graphics images, keyed by image id → (data generation,
    /// handle). The egui adapter stays here so `graphics` remains egui-free.
    textures: std::collections::HashMap<u32, (u64, egui::TextureHandle)>,
```

Initialize `textures: std::collections::HashMap::new(),` in `spawn_with`.

- [ ] **Step 2: Paint.** In `Session::show`, insert **after** `painter.galley(rect.min, galley, FG);` and **before** the `plan.highlights` loop (images sit above glyphs; selection highlight and caret stay visible on top):

```rust
        // Kitty graphics overlay — images are pure overlay; the grid stays
        // text (spec: docs/superpowers/specs/2026-07-02-terminal-image-support-design.md).
        if self.graphics.active() {
            let (alt, hist, off, lines) = {
                let g = self.term.grid();
                (
                    self.term.mode().contains(TermMode::ALT_SCREEN),
                    g.history_size(),
                    g.display_offset(),
                    g.screen_lines(),
                )
            };
            let vv = crate::graphics::ViewportView {
                alt_screen: alt,
                history_size: hist,
                display_offset: off,
                screen_lines: lines,
            };
            for p in self.graphics.visible(&vv) {
                let tex = match self.textures.get(&p.id) {
                    Some((gen, t)) if *gen == p.gen => t.clone(), // cheap Arc clone
                    _ => {
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [p.w as usize, p.h as usize],
                            p.rgba,
                        );
                        let t = ui.ctx().load_texture(
                            format!("kittyimg{}", p.id),
                            img,
                            egui::TextureOptions::LINEAR,
                        );
                        self.textures.insert(p.id, (p.gen, t.clone()));
                        t
                    }
                };
                // c/r from the client when given (pets always sends them);
                // otherwise derive the cell span from pixel size.
                let cols_f = if p.cols > 0 { p.cols as f32 } else { (p.w as f32 / cw).ceil().max(1.0) };
                let rows_f = if p.rows > 0 { p.rows as f32 } else { (p.h as f32 / rh).ceil().max(1.0) };
                let min = egui::pos2(
                    rect.min.x + p.col as f32 * cw,
                    rect.min.y + p.line as f32 * rh,
                );
                let img_rect = egui::Rect::from_min_size(min, egui::vec2(cols_f * cw, rows_f * rh));
                painter.image(
                    tex.id(),
                    img_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            // Drop textures whose image data is gone (deleted/evicted).
            self.textures.retain(|id, _| self.graphics.has_image(*id));
        }
```

Borrow note: `self.graphics.visible` borrows `self.graphics` immutably while `self.textures` is mutated — disjoint fields, compiles. If the earlier `painter` binding fights the borrow checker (it borrows `ui` immutably; `ui.ctx()` is fine), bind `let ctx = ui.ctx().clone();` before the loop and use `ctx.load_texture(...)` — `egui::Context` is a cheap `Arc` clone.

- [ ] **Step 3: Build and test** —

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 10
cargo test
```
Expected: builds clean, tests green (this step is paint-only; correctness evidence comes from Task 10's smoke test).

- [ ] **Step 4: Commit**

```powershell
git add src/terminal.rs
git commit -m "feat(terminal): paint kitty graphics as cell-anchored egui textures

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: Advertise KITTY_WINDOW_ID + visual smoke test

**Files:**
- Modify: `src/wm.rs` (`term_env`; its tests near `shell_sessions_still_spawn_with_env` / `term_env` assertions around wm.rs:4581 if they enumerate exact vars)

**Interfaces:**
- Consumes: everything prior.
- Produces: Codex detection fires (`KITTY_WINDOW_ID` is the first thing it checks).

- [ ] **Step 1: Add the env var** in `term_env`, after the TERM line:

```rust
            // Kitty graphics: the narrowest signal that makes agents (Codex
            // pets) pick the kitty protocol. TERM stays truthful — we implement
            // the graphics subset in src/graphics.rs, not all of kitty.
            ("KITTY_WINDOW_ID".to_string(), "1".to_string()),
```

- [ ] **Step 2: Run wm tests** — `cargo test --lib wm` — if `term_env` tests assert an exact var set, extend the expectation with `KITTY_WINDOW_ID=1`; contains-style asserts pass untouched.

- [ ] **Step 3: Visual smoke test (deterministic, no Codex needed).** Build + launch release foreman, open a project; in the pane's PowerShell:

```powershell
$e=[char]27; $rgba=[byte[]]::new(256); for($i=0;$i -lt 64;$i++){$rgba[$i*4]=255;$rgba[$i*4+3]=255}; Write-Host ("{0}_Ga=T,t=d,f=32,s=8,v=8,c=4,r=2;{1}{0}\" -f $e,[Convert]::ToBase64String($rgba))
```

Expected: a solid red block spanning 4 columns × 2 rows at the cursor position. Then scroll the pane (produce output, wheel up/down): the block stays glued to its line. Screenshot the window (script in `docs/HANDOFF.md` § 3) and `Read` the PNG to confirm — the project's evidence rule.

- [ ] **Step 4: Env check** — in the same pane: `$env:KITTY_WINDOW_ID` prints `1`.

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs
git commit -m "feat(wm): advertise KITTY_WINDOW_ID so agents enable kitty graphics

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: Performance verification (the gate)

**Files:**
- Modify: `src/terminal.rs` (one `#[ignore]` bench test in `mod tests`)
- Modify: `docs/terminal-images.md` (fill the After row + micro numbers)

**Interfaces:**
- Consumes: `Graphics::feed`, `term_with`, `Processor`.

- [ ] **Step 1: Add the micro-benchmark** (terminal.rs tests):

```rust
    #[test]
    #[ignore = "perf: cargo test --release --lib terminal -- --ignored --nocapture"]
    fn scanner_overhead_on_plain_and_ansi_floods() {
        let plain = {
            let line = "x".repeat(120) + "\r\n";
            line.repeat(200_000).into_bytes() // ~24 MB
        };
        let ansi = {
            let mut v = Vec::new();
            for r in 0..200_000 {
                v.extend_from_slice(
                    format!("\x1b[{};1H\x1b[38;5;{}mrow of colorful tui text", (r % 40) + 1, r % 256)
                        .as_bytes(),
                );
            }
            v
        };
        for (name, corpus) in [("plain", &plain), ("ansi", &ansi)] {
            let mut term = term_with(b"", 120, 40);
            let mut parser: Processor = Processor::new();
            let t0 = std::time::Instant::now();
            parser.advance(&mut term, corpus);
            let vte = t0.elapsed();
            let mut g = crate::graphics::Graphics::default();
            let t1 = std::time::Instant::now();
            let cuts = g.feed(corpus);
            let scan = t1.elapsed();
            assert!(cuts.is_empty());
            println!(
                "{name}: vte {vte:?} ({:.0} MB/s) | scanner {scan:?} ({:.0} MB/s) | overhead {:.2}%",
                corpus.len() as f64 / vte.as_secs_f64() / 1e6,
                corpus.len() as f64 / scan.as_secs_f64() / 1e6,
                100.0 * scan.as_secs_f64() / vte.as_secs_f64(),
            );
        }
    }
```

- [ ] **Step 2: Run it** — `cargo test --release --lib terminal -- --ignored --nocapture` — record both corpus lines. Expected shape: scanner MB/s ≫ vte MB/s (overhead well under 5% relative; the end-to-end gate is Step 3).

- [ ] **Step 3: Re-run the Task 1 flood** — same command, same pane conditions, release build of the branch, 3 runs, median. **Gate: median within 2% of the Task 1 baseline.** If it fails: profile `feed`'s ground-state loop first (it should be a straight byte-position scan; no allocation, no per-byte state dispatch).

- [ ] **Step 4: Record** both results in `docs/terminal-images.md` (After row + a "Scanner micro-benchmark" line with the printed numbers).

- [ ] **Step 5: Commit**

```powershell
git add src/terminal.rs docs/terminal-images.md
git commit -m "test(perf): scanner overhead micro-benchmark + before/after flood numbers

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 12: Acceptance — pets, paste, acid test, docs

**Files:**
- Modify: `docs/terminal-images.md` (finish: what/why/how/gotchas/key files)

- [ ] **Step 1: Codex pet live.** In a release foreman pane, run Codex and enable a pet (Codex ≥ the pets release; check `codex --help` / its config for the pets toggle). Expected: pet renders and animates in the pane. Screenshot + `Read` the PNG. If Codex still reports no image support, verify `$env:KITTY_WINDOW_ID` inside that exact pane and that TMUX/ZELLIJ vars are absent (Codex checks those first and bails).

- [ ] **Step 2: Image paste.** Copy a screenshot to the clipboard (Win+Shift+S). In Claude Code inside foreman press **Alt+V** → expected `[Image #1]` attached, and exactly once (no doubled `v`). In Codex press **Ctrl+V** with an image-only clipboard → expected its native image attach (the 0x16 forward). Also sanity-check Alt+B/Alt+F in a bash pane: each moves by word, nothing typed twice.

- [ ] **Step 3: Acid test (unchanged behavior).** In panes: `vim` (open/edit/quit), `lazygit`, `less` on a long file, plain `dir` flooding — no visual difference from main, DSR startup still latches (shell prompts appear), Ctrl+C/Ctrl+V text flows unchanged, selection + copy still work over text where an image overlaps.

- [ ] **Step 4: Finish `docs/terminal-images.md`** (grug-simple, per the user's doc rule):

```markdown
# Terminal Images (kitty graphics, image paste, alt-key routing)

## What / why

foreman renders the kitty graphics subset Codex pets uses (transmit PNG/raw →
display at cursor → delete by id), delivers image-paste keystrokes to agents
(Alt+V, Ctrl+V, Ctrl+Alt+V), and no longer double-sends Alt+letter. Spec with
all decisions: docs/superpowers/specs/2026-07-02-terminal-image-support-design.md

## How it works

PTY bytes flow to alacritty exactly as before; `Session::pump` also hands each
chunk to `graphics::Graphics::feed`, which returns "cuts" — offsets where a
graphics command completed. pump advances the parser in segments around cuts so
each placement samples the cursor at the right stream position. `Session::show`
paints the visible images as egui textures after the glyphs. Images never enter
the grid — selection/copy/snapshot stay pure text.

## Gotchas

- The cursor does NOT advance after an image (v1): ratatui apps don't care;
  `kitten icat` in a bare shell overprints. Deliberate — see spec limits.
- KITTY_WINDOW_ID=1 is injected; TERM stays xterm-256color on purpose.
- Graphics replies bypass `resp` — resp's flush latches `ready` (DSR contract).
- A `clear`/RIS doesn't delete placements; scrolling or `a=d` does. Pets
  deletes its own frames constantly, so this only shows with rogue clients.
- Alt text suppression is `alt && !ctrl` — AltGr (= Ctrl+Alt on Windows) must
  keep typing on intl layouts. Don't "simplify" it to plain `alt`.

## Performance

(numbers from Tasks 1 and 11)

## Key files

- src/graphics.rs — scanner, kitty parser, image store, placements (pure)
- src/terminal.rs — advance_scanned/term_view, pump segmenting, texture cache,
  overlay painting, clipboard_has_image
- src/input.rs — alt/ctrl+alt routing, empty-paste fallback
- src/wm.rs — KITTY_WINDOW_ID in term_env
```

- [ ] **Step 5: Final full run** — `cargo test` green; `cargo build --release` clean.

- [ ] **Step 6: Commit**

```powershell
git add docs/terminal-images.md
git commit -m "docs: terminal images feature doc with perf numbers and gotchas

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task order and gates

1 (baseline) → 2 → 3 → 4 (input trio; each shippable) → 5 → 6 → 7 (pure module) → 8 (pump) → 9 (paint) → 10 (env + smoke) → 11 (**perf gate**) → 12 (acceptance).

If the Task 11 gate fails, do not proceed to Task 12 — fix `feed` first; every suspect is behind the `graphics` seam.
