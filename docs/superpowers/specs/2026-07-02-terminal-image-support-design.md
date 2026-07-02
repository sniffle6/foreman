# Terminal Image Support — Design

**Date:** 2026-07-02
**Branch:** `feat/terminal-images`
**Status:** Approved design, pre-implementation

## Goal

Three related wants, smallest first:

1. **Fix the Alt-key double-print bug** — pressing Alt+any letter sends the key
   twice to the child app.
2. **Image paste** — pasting a clipboard image into agents (Claude Code, Codex)
   running inside foreman should work like it does in other terminals.
3. **Inline images (Kitty graphics)** — enough of the kitty graphics protocol
   that Codex terminal pets render and animate in a foreman pane.

Hard constraints, in the user's words: **no lag, instant response time, and the
code must not get complicated.** Every design choice below is subordinate to
those two.

## Research findings (verified against source)

### How Codex decides pets are available

Pure env-var sniffing, no runtime probe
([image_protocol.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/pets/image_protocol.rs)):
`KITTY_WINDOW_ID` set → Kitty protocol, checked first, full stop. Foreman
already injects per-terminal env in `term_env` (src/wm.rs), so triggering
detection is one line. The work is implementing the protocol.

### What pets actually emits

A small kitty subset, always quiet (`q=2` — never expects replies):

- `ESC _ G a=T,t=d,f=100,c=C,r=R,q=2[,i=ID],m=0|1 ; <base64 PNG> ESC \` —
  transmit + display PNG at cursor, sized C×R cells, chunked at 4096 base64
  chars with `ESC _ G m=0|1 ; chunk ESC \` continuations.
- `ESC _ G a=d,d=I,i=ID,q=2 ; ESC \` — delete by image id.

Animation is client-side: delete + retransmit the next frame. Codex does NOT
use kitty's server-side animation sub-protocol, `a=p` placements, z-index, or
unicode placeholders.

### How agents receive pasted images

The terminal never touches image bytes. Both agents read the image themselves
from the OS clipboard when they see their paste-image keystroke:

- **Codex:** `fixed.paste_image` = Ctrl+V and Ctrl+Alt+V (keymap.rs); reads via
  arboard in
  [clipboard_paste.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/clipboard_paste.rs).
- **Claude Code:** dedicated **Alt+V** binding on Windows
  ([issue #24092](https://github.com/anthropics/claude-code/issues/24092)).

Foreman terminals are local ConPTY children sharing the Windows clipboard, so
foreman's only job is delivering the keystroke intact.

### The Alt bug root cause

`process_input` (src/input.rs) forwards every `Event::Text` to the PTY, and
`encode_key` also encodes Alt+letter as `ESC <letter>`, with a comment claiming
"Alt suppresses the Text event." On Windows that assumption is false: egui
delivers both, so the PTY receives `ESC v` **and** `v`. Bonus bug: the
Ctrl-chord matcher catches `(Key::V, _)` whenever Ctrl is held, shadowing
Codex's Ctrl+Alt+V binding.

## Decisions (all user-approved)

| Decision | Choice |
|---|---|
| Kitty scope | Pets-first subset (see Supported commands) |
| Rendering | Inline overlay (hover-preview mode explicitly rejected — a pet you must hover to see isn't a pet) |
| Ctrl+V with image-only clipboard | Forward raw `0x16` to the app |
| Advertisement | `KITTY_WINDOW_ID=1` only; `TERM` stays truthful |
| New dependencies | `png` (decode) + `base64` (payload) — both small |
| Workflow | Feature branch `feat/terminal-images`, perf metrics before/after |

## Workstream 1 — Alt-key routing fix (src/input.rs)

- `process_input` gains a `mods: Modifiers` parameter (live modifier state).
  Drop `Event::Text` when `alt && !ctrl` is held. The `!ctrl` guard protects
  AltGr international layouts: Windows reports AltGr as Ctrl+Alt, and those
  must keep producing text (German AltGr+Q = @). The condition mirrors exactly
  when `encode_key` meta-encodes, so no key is double-sent or lost.
- Policy chords (`Ctrl+C/V/X/0`) exclude `alt`, so Ctrl+Alt combos fall through
  to the encoder.
- `encode_key` learns `ctrl && alt` → `ESC + control-code`
  (Ctrl+Alt+V → `1b 16`), so Codex's second paste binding works.

Tests (pure, no GUI): Alt+V with a stray `Text("v")` in the same frame emits
exactly `1b 76`; ctrl+alt text (AltGr) passes through; Ctrl+Alt+V → `1b 16`;
plain typing unchanged.

## Workstream 2 — image paste

After WS1, **Alt+V works in Claude Code and Codex with zero further foreman
code** (they read the clipboard themselves). One addition at the existing
`paste_clipboard` execution site: clipboard has text → bracketed-paste as
today; clipboard has only an image (arboard `get_image` succeeds) → write raw
`0x16` to the PTY so agents trigger their native image paste. Plain shells
treat `0x16` as readline quoted-insert — harmless. Foreman never touches image
bytes.

## Workstream 3 — Kitty graphics, pets-first

### Module design (codebase-design vocabulary)

**`graphics` (new `src/graphics.rs`) — one deep module, pure.** Dependency
category: in-process (no I/O, no egui, no alacritty mutation). The whole
interface is three methods:

```rust
impl Graphics {
    /// Scan one PTY chunk. Returns offsets ("cuts") where a graphics
    /// command completed mid-stream and the cursor must be sampled.
    /// A cut's offset is the chunk index immediately after the APC
    /// terminator (ESC \), so `chunk[..offset]` includes the sequence.
    fn feed(&mut self, chunk: &[u8]) -> Vec<Cut>;

    /// Apply the next pending command using sampled term facts.
    /// Invariant: called exactly once per cut, in order. Replies for
    /// non-quiet queries are appended to `out`.
    fn apply(&mut self, view: TermView, out: &mut Vec<u8>);

    /// What's visible right now, as cell-anchored rects + RGBA.
    fn visible(&self, view: ViewportView) -> impl Iterator<Item = Placed>;
}
```

Behind the seam: APC state machine, base64 decode, `m=1` chunk reassembly,
kitty header parsing, PNG decode, image store, quota/eviction, scroll-anchor
math. Every future increment (`a=p`, z-index, cursor policy, sixel-someday)
lands inside this file. Deletion test: remove the module and the terminal is
byte-for-byte today's terminal.

**Cursor sampling must be position-exact.** Ratatui writes `move cursor → APC`
in the same chunk, so `pump` advances alacritty in segments around each cut:

```rust
let mut at = 0;
for cut in self.graphics.feed(&bytes) {
    self.parser.advance(&mut self.term, &bytes[at..cut.offset]);
    self.graphics.apply(TermView::of(&self.term), &mut replies);
    at = cut.offset;
}
self.parser.advance(&mut self.term, &bytes[at..]);
```

Alacritty sees **byte-identical input** — just split across more `advance`
calls, which already happens at every chunk boundary. alacritty's vte discards
APC content anyway, so the grid path is untouched either way. Zero cuts (the
normal case) is today's exact code path. `pump` grows ~8 lines — the whole
blast radius on the DSR-hardened path.

**egui texture adapter — ~20 lines inside `Session::show`, not a module.**
Maps `Placed` → cached `TextureHandle` keyed on (image id, generation). egui
types stay out of `graphics`, so its tests run headless under `cargo test`
like layout.rs. One adapter → no trait; a port here would be a hypothetical
seam.

### Supported commands (v1)

| Command | Behavior |
|---|---|
| `a=T`, `t=d`, `f=100` (PNG) and `f=32/24` (raw RGBA/RGB) | Decode, place at cursor, sized c×r cells (computed from pixels + cell metrics when c/r absent) |
| `m=0/1` chunking | Reassembled across APC sequences |
| `a=d` (`d=I` by id; bare = clear visible) | Remove placements/data |
| `a=q` query | Honest reply when `q<2` (OK only for what we support), written straight to the PTY writer — NOT via the `resp` buffer, whose flush latches `ready` (DSR contract); a graphics reply must never fake readiness |
| Anything else | Silently skipped — never corrupts, never replies unless `q<2` demands an error |

Placement anchors: cursor cell at apply time; alt-screen placements are
fixed-position; primary-screen placements track scroll via history-size delta
and are culled once far off-screen or on clear/reset.

### Advertisement

`KITTY_WINDOW_ID=1` added in `term_env` (src/wm.rs). `TERM` stays truthful
(not `xterm-kitty`) so terminfo-driven behavior is unaffected. This is the
narrowest signal that satisfies Codex's detection.

## Performance requirements and measurement

Requirements:

- **Idle: zero cost.** Nothing runs unless PTY bytes arrive; `show` guards on
  an empty store — one branch per paint with no images.
- **Text throughput: one extra linear pass with a SIMD constant.** Ground-state
  scanner is a `memchr(0x1b)` skip; no allocation on the no-graphics path.
- **Graphics cost scales with graphics, not text.** Sprite-sized PNG decode
  per pet frame (sub-ms); decoded store under a hard quota with eviction.
- **No new threads, no locks, no async.**

Measurement (release build, run at the baseline commit and at completion,
numbers recorded in the feature doc):

1. **Flood throughput** — large-file dump / `seq` burst in a pane; wall-clock +
   frame times. **Gate: <2% regression.**
2. **Scanner micro-benchmark** — MB/s over plain text, ANSI-heavy TUI output,
   and text with embedded graphics.
3. **Input path** — O(1) branch changes only; verify unchanged test timings
   and key-to-echo feel.
4. **Pet running** — CPU while animating (bounded by Codex's frame rate — that
   cost is the feature); neighboring pane throughput unaffected.

If a gate fails, the scanner is the first suspect, and it sits behind one seam.

## Limits and failure modes (v1, accepted)

- **Cursor does not advance after image placement.** Full-screen/ratatui apps
  are unaffected (they reposition the cursor every draw). `kitten icat`-style
  tools in a bare shell will draw the next prompt over the image — visual
  overlap only, never corrupted text. Fixing this means mutating Term state
  (cursor moves + synthetic scrolls) — deferred to v2 deliberately.
- **Unsupported protocol features fail silent and blank.** Tools using `a=p`
  placements (e.g. image.nvim) show empty space; z-index is single-layer
  (images above glyphs); kitty's animation sub-protocol unsupported (Codex
  animates client-side). Failure mode is always "image doesn't show," never
  "pane corrupts."
- **`KITTY_WINDOW_ID` spoofing is a lie we keep safe.** Every Codex detection
  path requires impersonating a graphics-capable terminal; this is the
  smallest lie. Probing tools get honest `a=q` answers and degrade; blindly
  trusting tools hit the silent-skip behavior above. Unknown escape sequences
  are discarded by alacritty's parser — the pane never corrupts.
- **The terminal's content stays 100% text.** Images never enter the grid.
  Selection, copy, scrollback, `foreman snapshot`, chat injection all operate
  on pure text, byte-identical to today. Images are a painted overlay in
  `Session::show` only.

## Out of scope (v2 candidates, in rough order of likely demand)

Cursor advancement after placement → `a=p` placements → z-index → hover-preview
mode → sixel → kitty animation sub-protocol.

## Testing strategy

- **graphics module (pure, headless):** scanner split-boundary fuzz (feed
  byte-at-a-time and in random splits), chunk reassembly against Codex's exact
  emission format (copied from its own tests), header parsing, delete
  semantics, quota eviction, placement/scroll math.
- **input (pure):** the WS1 test list above.
- **Integration/visual:** release build; Codex pet animating in a foreman pane,
  screenshot as evidence; acid test that vim/lazygit/plain shells are
  unaffected; DSR handshake still latches Ready.
- **Perf:** the measurement plan above, before and after.

## Key files

- `src/graphics.rs` — new: the deep module (scanner, parser, store, placement).
- `src/terminal.rs` — `Session::pump` segmented advance (~8 lines);
  `Session::show` texture adapter (~20 lines); Session holds a `Graphics`.
- `src/input.rs` — WS1: `process_input` modifiers param, chord un-shadowing,
  `encode_key` ctrl+alt encoding.
- `src/wm.rs` — `term_env`: `KITTY_WINDOW_ID=1`; `paste_clipboard` execution
  site: image-only clipboard → `0x16`.
- `Cargo.toml` — add `png`, `base64`.
