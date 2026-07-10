# Warp-derived feature candidates

Reference doc for deciding whether to adopt ideas from Warp (warp.dev). Compiled
2026-07-09 from a Warp research pass (github.com/warpdotdev/warp README,
warp.dev/blog/how-warp-works, warp.dev homepage) plus **8 parallel subagent
reviews, each verified against the actual foreman codebase** (file:line refs
below were checked, not assumed).

Warp context as of mid-2026: client codebase is open-source (AGPL v3; the
`warpui_core`/`warpui` UI-framework crates are MIT), rebranded from "terminal"
to "Agentic Development Environment", added **Oz** (cloud orchestration of
Claude Code / Codex / Warp Agent). Warp forked Alacritty's model code, built a
custom GPU UI framework (rects/glyphs/images primitives, ~200-line shaders),
a full text-editor input (SumTree rope), the "blocks" data model, and reuses
Fig completion specs (MIT).

## Verdict summary

| # | Candidate | Verdict | Effort | Gate / next step |
|---|-----------|---------|--------|------------------|
| 1 | OSC 133 semantic prompt marks (blocks-lite) | **SPIKE-FIRST** | M (spike: 1 day) | Verify vendored OpenConsole passes OSC 133 through; detect-only MVP; route via agent-state campaign |
| 2 | Command/output addressability in control plane | **ADOPT-LATER** (standalone `--tail` slice: **ADOPT now**, S) | S standalone / M full | Full block model hard-depends on #1 + honest answer on alt-screen coverage |
| 3 | Renderer: custom GPU framework (warpui-style) | **REJECT** (egui optimizations: ADOPT-LATER, gated) | S–M (opts) / L–XL (renderer) | 4K perf measurement; real bottleneck is VTE parsing, not rendering |
| 4 | Fig completion specs (autocomplete) | **REJECT for now** | L | Reconsider only if composer ships AND humans demonstrably type raw shell |
| 5 | Keybinding enablement predicates | **REJECT** (20-line `keyboard_owner` extraction: worthwhile) | M | Revisit only if non-leader globals or rebindable modal keys arrive |
| 6 | Per-terminal composer/input pane | **OPTIONAL** (not equal to the S fixes above) | S | Only if multi-line *unframed* draft is wanted; chat multiline is a cheaper partial sub |
| 7 | Fleet dashboard / agent-state badges | **ADOPT-LATER** | M + M | Campaign phases 0–2 first; badges = phase 3. No lying dots. |
| 8 | Warp's dependency stack (Tokio/font-kit/fork) | **KEEP-AS-IS** — except **font fallback: ADOPT** (S) | S | Load YaHei + Segoe into egui fallbacks; only matters when non-ASCII hits the screen |

### Goal vs do-next (do not confuse)

| | What | Meaning |
|---|------|---------|
| **Product goal** | Agent-state detector | "Which pane needs me" — HANDOFF differentiator. Hard. Research. Not badges first. |
| **Do next** | Ranked shovel list below | Small, real gaps. No research theater. |
| **Not yet** | Fleet / badges / block model | Need honest detector (or self-report) first. |

**Do next (ranked — not equal):**

1. **Font fallback** — **shipped** (`docs/font-fallback.md`). CJK/emoji glyphs via YaHei + Segoe fallbacks.
2. **snapshot `--tail N`** — agents only see the viewport today; long builds fall off.
3. **READY_GRACE** (agent-state campaign Phase 0) — inject/chat can stick forever if DSR never latches. Foundation for state later.
4. **`keyboard_owner()`** — ~20-line cleanup when touching keymap/wm. Lowest product value.
5. **Composer** — optional human multi-line draft. Not the fleet problem. Soft adopt only.

Then: OSC 133 spike (signal experiment) → campaign Phase 1 audit → detector or `foreman state` verb → badges last.

---

## 1. OSC 133 semantic prompt marks (blocks-lite)

**What Warp does:** Models scrollback as first-class "blocks" — each
prompt+command+output triple is a discrete object with exit status, copyable
output, and navigation. Warp gets boundaries from its own shell-integration
hooks; OSC 133 (FinalTerm `ESC]133;A/B/C/D;exit=N`) is the open-standard
equivalent consumed by Windows Terminal, WezTerm, and Kitty.

**How it maps to foreman:**
- **Interception seam exists:** foreman owns the raw PTY byte stream before
  alacritty sees it — reader thread → mpsc → `Session::pump()` →
  `advance_scanned()` (`src/terminal.rs:242`), which already pre-scans bytes
  (`graphics.feed` cuts kitty APC sequences). An OSC 133 scanner is a second
  tap at exactly this seam. **No alacritty fork needed.**
- State: new fields on `Session` (`last_mark`, `last_exit`, ring of mark
  positions) alongside `osc_title`/`ready`/`output_gen`.
- Consumers: pane-header badge (`src/wm.rs`), `foreman status` precision
  (`src/control.rs`), copy-last-output as grid slice between marks.
- Injection: per-`Shell`-variant snippet (`src/terminal.rs:135` — Cmd,
  PowerShell, Bash); install pattern precedent in `src/skills_install.rs`.

**Verified findings:**
1. alacritty_terminal 0.26 **silently drops unknown OSC** (no Handler hook, no
   Event variant) — irrelevant, because the `advance_scanned` pre-parse tap is
   the right place anyway.
2. Shell spawn is bare (`CommandBuilder::new(shell.program())`, no args/rc
   injection, `src/terminal.rs:522`). `docs/shell-selection.md` pins a
   deliberate hands-off policy: "the user's profile is in charge." Emitting
   marks from pwsh needs a `prompt`-function wrap; **cmd.exe can never emit
   marks**; WSL bash needs PROMPT_COMMAND/PS0 hooks. Injection is a *policy*
   decision, not just plumbing. Note: starship/oh-my-posh already emit OSC 133
   — some users' shells emit marks today with zero injection.
3. **ConPTY passthrough unverified.** Foreman already sideloads vendored
   OpenConsole (`src/conpty_install.rs`) because in-box conhost strips kitty
   APC. Modern OpenConsole understands FTCS, so passthrough is plausible — but
   the in-box-conhost fallback path may strip marks. Must spike.
4. OSC 133 is **absent** from the agent-state campaign's signal inventory
   (`.claude/skills/foreman-agent-state-campaign/SKILL.md` §3) — a genuinely
   new candidate signal. Also serves the unsolved chat quiescence-gating
   problem (`docs/superpowers/specs/2026-06-10-chat-mentions-design.md`), at
   shell level only.

**Effort:** M. Parser tap itself is S (~150 lines + tests, mirrors
`graphics.feed`); the M is per-shell snippets + injection policy +
reflow-safe mark bookkeeping + badge/status surfaces + ConPTY verification.

**Pros:**
- Exact, event-driven prompt/exit-code truth for plain shells — strictly better
  than output-idle + cursor-stability heuristics, and cheap (no polling).
- Lands at a seam foreman owns; zero new dependency.
- Exit codes enable "command failed" badges and `foreman status` precision.
- Open standard; WT/WezTerm/Kitty snippets exist to crib from.

**Cons / risks:**
- **Headline limitation: marks stop flowing the moment an agent starts.**
  Claude Code/Codex are long-lived TUIs; OSC 133 says "agent running" /
  "agent exited N" — nothing about needs-input vs working *inside* the agent,
  which is the hard core of the state problem. Complement, not solution.
- ConPTY may strip marks on the in-box conhost fallback — silent per-machine
  degradation.
- cmd.exe gets nothing; pwsh 5.1 vs 7 differences; WSL needs its own mechanism.
- Profile injection tension with the shell-selection hands-off policy; can
  collide with users' prompt customization.
- Mark grid rows rot under ConPTY reflow (`docs/conpty-resize-reflow.md`) —
  jump-to-command needs scrollback-anchored bookkeeping, not viewport rows.
- Scanner must be sync-update-aware (`?2026h` already special-cased in
  `advance_scanned`).

**Open questions:**
- Does vendored OpenConsole pass OSC 133 verbatim, interpret-and-forward, or
  strip? (1-hour spike: pwsh emits `ESC]133;D;7`, byte-trace at reader thread.)
- Do Claude Code / Codex emit OSC 133 around their embedded tool executions?
  Check empirically — if yes, the TUI limitation partially dissolves.
- Injection policy: skills_install-style profile snippet, startup arg, or
  **detect-only** (rely on starship/oh-my-posh/WT snippets users have)?
  Detect-only is the zero-risk MVP.
- Should the A mark be an alternative Ready latch alongside DSR?

**Verdict: SPIKE-FIRST** — seam is proven and cheap, but ConPTY passthrough is
unverified and value for agent-internal state is bounded. Run the one-day
passthrough + detect-only spike; route the design through the
foreman-agent-state-campaign gates.

---

## 2. Command/output addressability in the control plane

**What Warp does:** Each block is a first-class object — copy "output of the
last command," see its exit status, hand a specific block to an agent instead
of scraping the screen. Block boundary + exit code turns a character grid into
an addressable log of structured command results.

**How it maps to foreman:**
- `src/control.rs`: `SnapshotRequest` (line 155) gains opt-in flags
  (`last_command`, `command: Option<i64>`, `tail: Option<usize>`), each
  `#[serde(default, skip_serializing_if = ...)]`; `parse_snapshot_args`
  (line 621) gains CLI flags; `OpenReply` (line 35) gains optional
  `exit_code`/`command_text`; output rides the existing generic
  `history: Option<Vec<String>>` payload. No new verb, no protocol v2.
- `src/wm.rs`: `snapshot_dispatch` (line 1373) resolves the span;
  `handle_status` already reports `exited(code)` — but that's the *pane
  process*, not the last command.
- `src/inspect.rs`: pure seam; would gain `snapshot_range(term, start, end)`
  alongside `snapshot_text` (line 78).
- `src/terminal.rs`: `Session` (line 337) stores the command-span table fed by
  the OSC 133 sibling.

**Verified findings:**
- **Snapshot is viewport-only today** (`snapshot_text` walks
  `0..screen_lines`); no flag reaches scrollback. (Doc drift found: the
  control.rs:130 comment saying `--settle-ms` is "not yet honored" is stale —
  wm.rs does honor it.)
- **Scrollback is sliceable:** default 10k-line history; `graphics.rs:476-496`
  already tracks content anchors across scroll via `history_size` deltas —
  precedent for buffer-stable "total line" coordinates.
- Session keeps **zero** per-command metadata (confirmed). No OSC 133 handling
  anywhere in src/.
- **Wire-compat v1 rules are explicit and paved:** optional fields +
  `skip_serializing_if` + a compat test (four precedents exist, e.g.
  `snapshot_reply_without_attrs_cursor_is_wire_compat` ~control.rs:1905).
  Rules in foreman-change-control skill + `docs/adr/0001-...` (reply stays a
  presence-discriminated bag; reshaping is ask-first).
- Roadmap already points this way: `docs/epics/terminal-inspection-epic.md:80-118`
  reserves `--rows`, `--region`, `--wait-for`, `--since-seq`. Composes cleanly.

**Effort:** M overall, split: **standalone slice (scrollback `--tail N` /
line-range read) is S — ~a day, no dependency on marks.** Full block model
(span table, `--last-command`, `--failed`, per-command exit codes) is M and
hard-depends on #1. Span bookkeeping across scroll/clear/reflow is the real
cost, not the wire work.

**Pros:**
- Directly serves the product thesis: supervising agents need "did the build
  pass and what did it say," not a 24-row screen scrape.
- Wire mechanics cheap and rehearsed; core logic pure and testable with
  `Term<VoidListener>`.
- Composes with planned `--wait-for`/`--since-seq`: "wait for command N, give
  me output + exit code" becomes a one-call agent primitive.
- The `--tail` slice has immediate value (long `cargo build` output scrolls
  off the viewport today) and de-risks the rest.

**Cons / risks:**
- **Alt-screen blindness (the big one):** flagship panes run Claude Code/Codex
  — full-screen TUIs where marks don't exist and "last command" is
  meaningless. Pays off only in plain-shell panes (build/test runners).
- ConPTY reflow vs mark anchoring: spans can silently shift after resize
  (documented battle, `docs/conpty-resize-reflow.md`); treat spans as
  best-effort, invalidate or re-anchor on resize/clear/ED-3.
- 10k-line truncation: a span whose head is gone must degrade explicitly
  ("output truncated"), not lie.
- Reply size: `--last-command` on a 50k-line build is a multi-MB JSON line —
  needs a cap/`--tail` interaction from day one.
- Shell-hook-only exit codes (no-marks fallback) are fragile and give no
  output boundaries — weak standalone value.

**Open questions:**
- Address spans by index/seq (`--command -1`) or only `--last-command`/`--failed`?
- Invalidation contract on resize: drop all spans (honest, cheap) or re-anchor
  via history deltas (better, riskier)?
- `--failed` = last nonzero-exit command, or all failed still in scrollback?
- Size cap: server truncates with marker field, or client must pass `--tail`?
- Does #1 deliver `133;D;<code>` (exit codes) or boundaries only?

**Verdict: ADOPT-LATER** — ship the S-sized `--tail`/range slice now (no
dependencies, real gap); gate the block model on #1 landing and an honest
answer on alt-screen coverage.

---

## 3. Renderer ceiling: egui vs warpui-style custom GPU framework

**What Warp does:** Custom retained-ish GPU UI framework (`warpui_core`/
`warpui`, now MIT) with exactly three primitives — solid rects, atlas glyphs,
images — through ~200-line shaders; a frame is a couple of instanced draw
calls with near-zero CPU-side layout. 144–400+ fps.

**Current foreman reality (verified):**
- **Repaint is already event-driven + adaptive:** PTY readers call
  `ctx.request_repaint()` per chunk (`src/terminal.rs:675`); timer is only a
  backstop — 4 ms while hot (<250 ms since activity), 100 ms idle
  (`src/main.rs:483-500`). Deliberate, measured (docs/followups-latency-and-control.md).
- **Per-pane galley memoization shipped 2026-07-04:** whole pane = one
  `LayoutJob` → one galley cached behind
  `GalleyKey{content_gen, display_offset, cols, rows, font_bits}`
  (`src/terminal.rs:1231-1277`); unchanged pane costs one Arc clone/frame.
- Background tabs are not painted (`src/wm.rs:2662, 2865`); occluded floats
  are painted but cost ~an Arc clone when unchanged.
- **Settled measurements say render is parse-bound, not draw-bound:** idle
  0.072 ms/frame; 12 max-rate floods 0.317 ms avg / 0.357 p95 post-galley-cache
  (docs/superpowers/plans/2026-07-04-render-read-perf.md metrics table).
  The lever for 20-30 noisy agents is **bounded per-frame VTE parsing** — a
  PTY-path change, not a renderer change.
- Vsync is a settled decision (ON; foreman-change-control settled list).
- Known small waste: `"M".to_string()` metrics probe per pane per frame
  (terminal.rs:1084-1086); scroll rebuilds the whole-pane galley
  (`display_offset` is in the key).

**Effort:** do nothing: S(zero). Targeted egui opts (bounded parsing,
occlusion skip, hoist metrics probe, offset-independent galley cache): S–M
each. Custom renderer: **L–XL** — reimplements atlas/shaping/fallback/
selection/caret/image compositing, forfeits the egui widget ecosystem the WM
chrome uses.

**Pros (recommended path — nothing now, gated opts later):**
- The two ceilings Warp's framework attacks (continuous repaint, per-frame
  text relayout) are already knocked down here, with committed evidence.
- Headroom proven: worst measured render ~0.36 ms p95 at 12 panes — ~2% of a
  16 ms budget.

**Cons / risks:**
- Existing measurements at unknown resolution; 4K grows atlas raster +
  tessellation cost (egui re-tessellates cached galleys each frame — NOT
  covered by the galley cache, grows with visible glyph count).
- Scroll-storm in a 300-row 4K pane rebuilds layout every frame — the one
  workload where a grid-native cell-atlas renderer is structurally better.
- GPU-side effects (ligatures, per-cell animation, subpixel AA control) are
  constrained by egui's text pipeline if ever wanted.

**Gating measurements:**
- Re-run the `[DEBUG-perf]` show-ms harness at 4K, 12-16 panes, adding
  tessellation + paint + GPU frame time. Gate: p95 full-frame > ~8 ms → do
  the egui-level opts; only if those can't hold <16 ms does a custom renderer
  enter the roadmap.
- Measure the scroll-storm case specifically.
- Split busy-frame cost: tessellation vs layout vs parse.

**Verdict:** do nothing now — **ADOPT**. Targeted egui opts — **ADOPT-LATER**,
individually gated. warpui-style custom renderer — **REJECT** for the roadmap
(SPIKE only if the 4K gate fails); studying warpui's atlas design is cheap and
worthwhile reading.

---

## 4. Fig completion specs (autocomplete)

**What Warp does:** IDE-style autocomplete in its input editor, powered by Fig
specs (github.com/withfig/autocomplete, MIT — 600+ tools as TypeScript modules:
static subcommand/option trees plus dynamic "generators" that shell out).
Possible only because Warp's composer owns the buffer before the shell sees it.

**How it maps to foreman:** the prerequisite does not exist.
- `src/terminal.rs:1006-1060` (`Session::read_input`): keys →
  `input::process_input` → `encode_key` → PTY bytes. Foreman never holds or
  models a command line. Only chrome-level chords intercept
  (`src/input.rs:51-133`).
- No shell integration / OSC 133 anywhere in src/.
- `src/inspect.rs` grid reads exist but are textual, not semantic — buffer
  reconstruction from the grid is exactly the ConPTY-reflow-ambiguity class
  this project got burned by. Reject option (c).
- Only sound host: the composer (#6). Option (b) (shell reports the buffer via
  custom OSC) is nonstandard per-shell hackery.

**Verified findings on spec format:** Fig specs ship as compiled JS modules
(`@withfig/autocomplete` on npm), **not JSON** — dynamic completions are JS
functions executing shell commands. Prior art confirms the runtime tax:
microsoft/inshellisense runs specs in Node; Amazon Q CLI (the Fig acquisition)
still executes the TS engine in a JS runtime. A Rust consumer needs
deno_core/quickjs embedded, or a build-time JSON transpile that keeps only the
static trees (loses git-branch-style dynamic completion).

**Effort:** L (composer prerequisite + JS runtime or transpile pipeline +
completion UI + spec update mechanism — each a subsystem).

**Pros:**
- 600+ tools instantly; MIT; community-maintained.
- If the composer lands anyway, completions are a natural enhancement with no
  PTY-path risk.
- foreman's own CLI could ship a spec.

**Cons / risks:**
- **Weak mission fit:** primary typists are AI agents that never use
  autocomplete; the human mostly types short dispatch/chat commands.
  Autocomplete optimizes the input mode foreman is designed to minimize.
- Embedding a JS engine fights the fast-native mandate (binary size, startup,
  a whole language runtime for a peripheral feature).
- Fig specs are Unix-first; PowerShell coverage is thin for a Windows-first app.
- Strictly downstream of the composer; premature to evaluate further now.

**Open questions:** static-only JSON variant (no JS runtime) worth it? How much
raw shell does the human actually type here? PowerShell spec quality?

**Verdict: REJECT (for now)** — reconsider as ADOPT-LATER only if the composer
ships and real human shell-typing shows up.

---

## 5. Keybinding enablement predicates (action-dispatch contexts)

**What Warp does:** Every keybinding maps to a named action carrying an
enablement predicate over app state ("ctrl-r only when input editor visible");
disabled chords fall through to typed input. Matters in Warp because its
shortcuts are *non-prefixed globals* competing with typing.

**How it maps to foreman:** `src/keymap.rs` — flat `HashMap<Chord, Command>`
(line 300), `resolve` (line 305); no context notion (only the cosmetic
`Command::group()`). Predicates would be evaluated in `pump_commands`
(wm.rs:3521) / `dispatch` (wm.rs:1792).

**Verified findings (routing order per frame):**
- `pump_commands` already runs a hand-rolled predicate stack:
  `desktop && active && picker.is_none() && renaming.is_none() &&
  settings.is_none() && no egui widget focused` (wm.rs:3521-3529) — the last
  guard is unit-tested (`leader_stays_dormant_while_a_widget_holds_focus`,
  wm.rs:5492).
- The leader state machine **swallows** its chords + stray Text events
  (`swallow_input`, wm.rs:1757) so nothing leaks to `read_input`.
- Terminal input gated at render: `is_focus = focused == Some(id) && live &&
  picker/renaming/settings all None` (wm.rs:2612-2616), commented with the bug
  it prevents. Both modal-freeze behaviors are unit-tested (wm.rs:7010, 7052).
- Similar ad-hoc guards: `overlay_blocks_close` (wm.rs:1989-1995), `apply_acts`
  mouse bail (wm.rs:3634), `app_modal` threading (wm.rs:2560-2571).
- **No evidence of "key ate my input" keybinding bugs in docs/** — every
  "eaten/swallowed" hit is the pre-DSR PTY-injection issue, a different
  subsystem.

**Effort:** M — dispatcher change is small; the real cost is the keymap
persistence surface: `(context, chord)` keying breaks `rebind`/`chord_for`/
`reset_one`/`save` (keymap.rs:305-385), the settings editor's single-namespace
conflict model, keybindings.json schema (forward-compatible via
`#[serde(default)]`, but a new-schema file fails on an old build and the
all-or-nothing loader (keymap.rs:416-425) silently discards the user's whole
custom map), and ~40 tests.

**Pros:**
- Formalizes 4-5 hand-maintained boolean disjunctions into one source of truth.
- Enables chord reuse across contexts; could make modal-internal keys
  (picker/settings/help) rebindable someday.

**Cons / risks:**
- **Solves a problem foreman structurally doesn't have.** Leader-prefixed
  bindings + input-drain + centralized modal gating already eliminate Warp's
  conflict class. The guards are few, centralized, commented, and tested.
- Schema/migration + editor-complexity cost lands on a simple, tested contract.
- The blunt "any focused text widget disables the leader" is arguably *safer*
  than fine-grained predicates, which tempt exceptions that reintroduce leaks.

**Open questions:** any concrete desire for cross-context chord reuse or
non-prefixed globals? (Nothing in docs/epics asks.) Should modal-internal keys
become keymap-driven (separate, larger feature)?

**Verdict: REJECT** — revisit only if non-leader global shortcuts or rebindable
modal keys arrive. **Do take the ~20-line extraction:** unify the repeated
`picker/renaming/settings/app_modal` disjunction into one
`fn keyboard_owner(&self) -> InputOwner` consulted by `pump_commands`,
`is_focus`, `overlay_blocks_close`, `apply_acts` — all the real value at ~5%
of the cost.

---

## 6. Per-terminal composer/input pane (opt-in)

**What Warp does:** Replaces the shell's line editor with a native multi-line
input block (SumTree rope, real cursor/selection/clipboard/history); submit
hands the buffer to the shell. Proposal here is the minimal version: egui's
stock `TextEdit`, submit = write to PTY.

**How it maps to foreman (traced):**
- Injection path fully reusable: chat input strip (wm.rs ~405-445) →
  `ChatView::pending_post` (chat.rs:401) → `drain_chat_posts` (wm.rs:1524) →
  … → **`Session::inject_input` (terminal.rs:821)** — the composer calls
  `inject_input` on its own Session directly; no chat room in the loop.
- `inject_input` is exactly the write primitive: queues in `pending_inject`
  until `ready` (avoids the boot-window swallow), wraps in `paste_wrap()`
  (terminal.rs:479 — `ESC[200~…ESC[201~`, ESC-stripped so quoted `ESC[201~`
  can't break out), defers the submitting `\r` by `SUBMIT_DELAY` (Claude
  Code's burst detection folds a back-to-back `\r` into the paste — documented
  live failure 2026-06-10).
- UI: mirror of the `docs/window-chrome.md` reserved-band pattern — reserve a
  band at the *bottom* of the terminal rect; per-frame re-fit already resizes
  the Session to whatever rect it gets. Chat input strip is a working template
  incl. the egui Escape-defocus quirk (wm.rs:441-443).
- Keymap: `Command::ToggleComposer` leader chord fits the documented
  extension pattern (new commands auto-get default chords).

**Verified findings:**
- **Bracketed paste: already handled** — `paste_wrap` unconditional in v1,
  live-verified against Claude Code on ConPTY. Known hardening applies
  identically: bare PowerShell doesn't interpret the markers (shows literal
  `[200~`) — gate on `TermMode::BRACKETED_PASTE` via `Session::term_mode()`
  (terminal.rs:844) or keep composer opt-in on agent panes.
- **Focus precedent exists:** chat input already puts a TextEdit inside a wm
  window with `request_focus()`/`lost_focus()`; dirpicker shows
  intercept-keys-before-TextEdit (dirpicker.rs:296, 344, 384-397). The new
  bit: terminal keyboard reads are gated only by an `active` flag with **zero
  focus checks inside `read_input`** (terminal.rs:1011) — the wm must pass
  `active = focused && !composer_has_focus` or every composer keystroke also
  hits the PTY. One boolean, must be threaded exactly.

**Effort:** S — the hard 80% (bracketed paste, ready-gating, submit-delay,
strip layout, focus dance) is built and battle-tested. New code: per-Session
draft state + a bottom band in the terminal draw arm + one keymap command +
the `active` gate. Real design work: Enter semantics + focus handoff.

**Pros:**
- Inherits every hard-won injection fix for free.
- Real multi-line drafting with native selection/clipboard/undo — the chat
  input is **singleline** (Enter submits, wm.rs:428) and Claude Code's own TUI
  input under ConPTY doesn't give this.
- Delivers *unframed* text: chat injection frames the line with sender
  metadata; the composer writes the prompt as if typed.
- Opt-in + leader toggle; no new architecture.

**Cons / risks:**
- Focus invariant becomes "one terminal *or its composer*" — the gate must be
  exact; leader chords must still be intercepted while composer is focused
  (dirpicker pattern covers this) or Ctrl+B types into the draft.
- Partial redundancy with chat: composer's distinct value is exactly
  multiline + unframed + no room-log pollution. Upgrading chat input to
  multiline is a cheaper 60% substitute, but still frames and broadcasts.
- Bare-shell targets show literal `[200~` unless gated on `term_mode()`.
- Grid shrink on toggle = a ConPTY reflow (same exposure as any resize; #18725
  territory, not new).

**Open questions:** submit chord (Warp: Enter submits, Shift+Enter newline;
egui multiline defaults the opposite)? Draft state per-Session or transient
egui memory? Gate `paste_wrap` on BRACKETED_PASTE from day one? Composer on
agent panes only (reuse Claude/Codex tab-icon detection) or any terminal?

**Verdict: OPTIONAL** — plumbing is real and cheap, but this is a *human draft*
aid, not the product thesis. Distinct value = multi-line + unframed + no chat
broadcast. Upgrading chat to multi-line covers ~60% cheaper (still framed). Do
not treat as equal to font / `--tail` / READY_GRACE. Ship only if we care about
that draft UX; primary typists are agents that never use a composer.

---

## 7. Fleet dashboard / per-pane agent-state badges

**What Warp does:** Oz dashboard shows every agent across sessions with live
status (working / awaiting input / done), triage + click-through. Warp *is*
the agent host, so state is ground truth — foreman must infer from PTY bytes.

**How it maps to foreman:**
- **A full design campaign already exists:**
  `.claude/skills/foreman-agent-state-campaign/SKILL.md` — decision-gated
  runbook for exactly this (needs-input/working/done/idle, badge, "jump to
  next needs-you"). Status: design-stage, nothing built (no `AgentState` in
  src/). HANDOFF.md §5 calls state detection "the differentiator."
- Existing signals: `Session::ready` + `output_gen` (terminal.rs); quiescence
  settle machinery (`advance_settles`, wm.rs, DEFAULT_SETTLE_MS=120); cursor-
  rest gate (caret.rs, CURSOR_SETTLE=50ms); agent *identity* via process-tree
  scan (`src/proc.rs`, throttled 1.5s, WSL-blind) + OSC-title fallback
  (terminal.rs:444-462). `foreman status` reports running|exited(code) — no
  state column.
- **Crew board exists as the overview seed:** `ChatRoom::crew()` +
  `CrewRow{id,name,exited,last}` (chat.rs), rendered as "CREW · BY LAST HEARD"
  panel in `Content::Chat` (wm.rs:148-230) with click-to-focus via deferred
  actions (wm.rs:1479, 3507). Per-project, chat-members-only today.
- Content-variant pattern confirmed: `enum Content{Terminal, Project, Chat}`
  (wm.rs:94-102); a desktop-level `Content::Fleet` follows the same shape —
  wrinkle: must read across project boundaries.
- Badge sites: tab strip already paints per-tab icons (`icon_kind()`,
  wm.rs:2901-3132) — a state dot slots in beside; headers are quiet chrome, so
  a badge must be a small glyph/dot, not loud fill.

**Verified findings:**
- Zero OSC 133 support today; the campaign's Phase 1 gate anticipates the hard
  case — if needs-input is indistinguishable from resting by every passive
  signal, the escalation is explicit agent cooperation (a
  `foreman state working|blocked|done` verb), fenced as phase-3/sign-off.
- Keyword-sniffing screen text and parsing agent session files are explicitly
  fenced wrong paths (campaign §8, docs/chat-missing-features.md).

**Effort:** badges-with-heuristics **M** (signals exist in-process; campaign
prescribes a pure `src/agentstate.rs` + fixture tests + G1-G3 numeric gates).
Full overview surface **M on top** (crew board is the template; new work is
cross-project plumbing + a Content variant). Pointless before detection works.

**Pros:**
- The product differentiator HANDOFF already names; runbook, validation gates,
  and UI precedents all exist.
- Passive composite writes zero bytes into Sessions — provably can't corrupt
  in-flight work.
- Solves the real pain (which of 8 panes needs me); later unlocks
  quiescence-gated chat delivery.

**Cons / risks:**
- **State-detection reliability is the whole risk and is honestly unsolved.**
  Output flowing = working (good); quiet + cursor parked =
  done-or-idle-or-blocked-on-you — the three states users most need separated
  are the ambiguous cluster.
- TUI agents break prompt marks: while Claude Code/Codex run, the shell never
  returns to prompt — for needs-input, OSC 133 contributes nothing. Spinners
  keep the screen changing while parked at a prompt (campaign §6).
- False "needs-you" is the expensive error; a flappy badge is worse than none
  (hence the G1-G3 gates).
- Done vs idle may be observationally indistinguishable without agent
  self-reporting (new verb → wire-compat + skill-sync + per-provider adoption).

**Open questions:**
- Does the Phase 1 signal audit separate needs-input from resting for Claude
  Code and Codex, or does agent self-reporting (`foreman state` verb — the
  *real* Oz-parity mechanism) get promoted to required?
- Overview: new desktop `Content::Fleet` or generalize the crew board?
- Grow `foreman status` a state column in the same change (additive serde
  field)?

**Verdict: ADOPT-LATER** — the UI is cheap and templated, but badges before
the detector passes the campaign's own gates = a lying dashboard. Execute
foreman-agent-state-campaign phases 0-2 first; badges/overview are its
already-planned phase 3.

---

## 8. Dependency/architecture notes (async runtime, font stack)

**What Warp does:** Tokio/smol async runtime (driven mostly by its network
surface, not PTYs), font-kit for system-font discovery/fallback, forked
Alacritty model.

**Foreman's current reality (verified):**
- 14 runtime deps, **zero async** (alacritty_terminal, eframe, portable-pty,
  interprocess [sync pipes], arboard, sysinfo, serde/json, resvg, png, base64,
  chrono, unicode-width, windows-sys).
- Thread model: **1 detached reader thread per Session** (blocking 64KiB read
  → mpsc → `request_repaint`, terminal.rs:661; PTY writes are synchronous on
  the GUI thread — no writer thread). Control plane: 1 listener + short-lived
  per-connection threads capped at `MAX_INFLIGHT=64` (control.rs:256).
  Totals: ~22-25 threads at 20 terminals, ~52-55 at 50 — single-digit MB
  committed, parked in blocking ReadFile, exactly conhost's own idiom. Even
  500 sessions would be fine.
- Wakeups are event-driven; all sleep loops found are `#[cfg(test)]` only.
- **Font fallback is the actual gap:** no `FontDefinitions` config anywhere —
  foreman rides egui defaults (Hack + Ubuntu + monochrome Noto emoji). The
  *data model* is wide-char-correct (WIDE_CHAR_SPACER, CJK selection acid-
  tested per docs/terminal-selection.md) but the *renderer* has **no CJK
  glyphs (tofu)** and weak emoji coverage.
- **When you see it:** CJK in paths/`ls`/`git`/agent errors; emoji in agent
  replies or CLIs. **When you don't:** plain ASCII panes only — you may never
  notice. Display bug, not a new feature.
- Fix needs no font-kit: at `eframe::run_native` startup, best-effort
  `std::fs::read` of `C:\Windows\Fonts\msyh.ttc` (YaHei) + `seguiemj.ttf`
  (Segoe UI Emoji — shapes only in egui 0.34; not color), insert into
  `FontDefinitions::default()` and **push** onto Monospace + Proportional
  fallback lists (keep defaults first). ~30 lines in `main.rs`. Missing file
  = skip, don't crash.
- One acknowledged wart: a wedged control-handler thread is reclaimed only
  when its client dies (control.rs:252-255 comments) — bounded by the 64 cap.

**Verdicts:**
- Async runtime (Tokio/smol): **KEEP-AS-IS** — an executor adds a second
  scheduler beside egui's frame loop, awkward overlapped I/O on ConPTY
  handles, Send/lifetime friction around `Term`, and solves nothing foreman
  has. Revisit only for thousands of sessions or heavy network I/O (neither
  on the roadmap).
- Control-plane pipe server: **KEEP-AS-IS** — thread-per-connection + cap is
  adequate; revisit if wedged-handler leaks show up in practice.
- Alacritty: **KEEP-AS-IS** — consuming `alacritty_terminal` as a crate is the
  right altitude; forking is Warp-scale maintenance burden.
- font-kit: **don't adopt** — dynamic discovery is overkill for a Windows-only
  app with known font paths.
- **Font fallback itself: ADOPT (S)** — the one validated gap. Load YaHei +
  Segoe UI Emoji as egui fallbacks; grid model is already correct, purely a
  glyph-coverage fix.

**Open questions:** color emoji story (weak in egui 0.34) vs monochrome-ok?
Is emoji (agents love it) the real-world case rather than CJK? Cross-platform
font discovery only if foreman ever leaves Windows.

---

## Product goal vs first shovel (2026-07-10)

**Product goal (not "code this tomorrow"):** agent-state *detector* —
needs-input / working / done / idle per pane. Not badges. Not fleet UI.
Runbook: `.claude/skills/foreman-agent-state-campaign/SKILL.md`.

Why it matters: attention is the bottleneck, not pane count. HANDOFF names
this the differentiator. Badges, fleet, chat quiescence gating, and "agent
finished" pings all sit on top of it. OSC 133 is one *signal*, not the feature.

**Honest ceiling from outside the agent (PTY only):**

| Often works | Often impossible without help |
|-------------|-------------------------------|
| working (bytes flowing) | needs-you vs done vs idle |
| quiet | (all look: quiet + cursor parked) |
| exited / dead process | |

Warp Oz is first-party (Warp hosts the agent). Foreman is third-party
(bytes only). Copy the *problem* ("who needs me"), not the mechanism.

If Phase 1 audit cannot separate needs-input from resting → stop grinding
heuristics → ship `foreman state` self-report via skills. Flappy "needs you"
trains humans to ignore it. Prefer a boring 3-bin detector that is right over
a 4-state theater that flaps.

**Campaign order (when you leave the shovel list):**

0. READY_GRACE — state-detection v0; unwedge inject/outbox  
1. Signal audit — measure; no `agentstate.rs` yet  
2. Pure detector *or* self-report verb (audit decides)  
3. Badges / fleet / jump-to-needs-you last  

Features "fall out" only if the detector is trustworthy.

## Suggested sequencing

1. **Now (ranked):** font fallback → snapshot `--tail N` → READY_GRACE
   (Phase 0) → `keyboard_owner()` when convenient → composer only if wanted.
2. **Spike:** OSC 133 ConPTY passthrough + detect-only (1 day). Gates block
   model (#2); feeds campaign as a signal candidate.
3. **Campaign:** Phase 1 audit → Phase 2 detector or `foreman state` →
   Phase 3 badges/fleet (#7); full command addressability after marks.
4. **Dormant:** custom renderer (#3 — only if 4K perf gate fails), Fig (#4 —
   after composer + real demand), keybind predicates (#5 — non-leader globals).

Candidates, not commitments. State vocabulary, injection policy, wire changes
= ask-first per foreman-change-control.
