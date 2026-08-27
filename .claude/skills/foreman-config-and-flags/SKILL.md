---
name: foreman-config-and-flags
description: Use when auditing or changing any foreman configuration axis - %APPDATA%\foreman settings.json or keybindings.json, font-size zoom persistence, default Leader/Chord bindings, injected env vars (FOREMAN, FOREMAN_TERMINAL_ID, FOREMAN_PROJECT_ID, FOREMAN_EXE, FOREMAN_TITLE_PIPE, TERM, COLORTERM), CLAUDE_CONFIG_DIR/CODEX_HOME/GROK_HOME installs, tunable timing constants, managed agent hooks, .claude development hooks, adding a persisted setting. Symptoms - setting not saved, binding ignored, "FOREMAN_TERMINAL_ID unset", zoom resets on restart.
---

# Foreman configuration and flags

Every configuration axis in foreman: the persisted files, the env-var contract,
the compile-time tuning constants, and the Claude Code hooks — plus the
checklist for adding a new axis. Cites name a file and a symbol, never a line
number.

The tuning constants in Axis 4 have **no config file** — they are compile-time
Rust `const`s, and changing one means a rebuild through
**foreman-change-control** like any other change. Promoting one to a `Settings`
field is the normal way a knob becomes user-facing; several already have.

## Config axes at a glance

| Axis | Where | Kind | Status |
|---|---|---|---|
| App settings | `%APPDATA%\foreman\settings.json` via `src/config.rs` (`struct Settings`) | User-facing, persisted, written by the app and by the in-app settings menu | Production |
| Keymap overrides | `%APPDATA%\foreman\keybindings.json` via `src/keymap.rs` | User-facing, persisted, hand-editable AND written by the in-app editor | Production (bespoke *load* path — see below) |
| Recent projects | `%APPDATA%\foreman\recents.json` via `src/recents.rs` | App-written history, not hand-edited | Production |
| Restored workspace | `%APPDATA%\foreman\workspace.json` via `src/workspace.rs` | App-written layout snapshot; gated on `Settings::restore_workspace` | Production |
| User themes | `%APPDATA%\foreman\themes\<slug>.json` via `src/theme.rs` (`config::themes_dir`) | One file per user theme; `Settings::theme` selects by name | Production |
| Env vars injected into every Session | `src/wm.rs` `fn term_env` | Contract for agents/CLI inside Sessions | Production |
| Env vars foreman consumes | `APPDATA`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GROK_HOME`, `USERPROFILE`, `FOREMAN_*` | Ambient environment | Production |
| Skill auto-install | `src/skills_install.rs` `install()`, run at GUI startup and gated on `Settings::install_skills` | Writes managed SKILL.md files into Claude/Codex skill dirs | Production, best-effort |
| Agent naming hook install | `src/agent_hooks.rs`, gated on `Settings::auto_name_agent_sessions` | Semantically merges guarded global Claude/Codex hooks and owns one dedicated Grok hook file | Production, opt-in, recoverable |
| Tuning constants | `const`s across `src/` (table below) | Code-level knobs, compile-time only | Production values; change via foreman-change-control |
| Claude Code hooks | `.claude/settings.json` + `.claude/hooks/*.ps1` | Dev-workflow automation for sessions working ON this repo (not app config) | Production |
| Local permissions | `.claude/settings.local.json` (untracked/ignored) | Machine-local Claude Code permissions | Local only |

Not configuration (common confusions):

- **Chat log persistence** is an append-only JSONL event log, deliberately NOT
  through `config.rs` — "this is for settings, not logs" (`src/config.rs` module
  docs, `docs/settings-persistence.md`). See `docs/chat-persistence.md`.
- **GUI binary flags**: there are none. Any argv beyond the exe name switches
  `foreman.exe` into Control-plane CLI client mode (the subcommand
  short-circuit in `src/main.rs`). Verbs and flags live in
  **foreman-run-and-operate**.

## Axis 1: `%APPDATA%\foreman\settings.json` (`src/config.rs`)

The canonical settings layer. Reuse it for any new flat persisted setting.

The file is `settings.json` under `config::config_dir()` = `%APPDATA%\foreman`
(created on demand). Its shape is **`struct Settings` in `src/config.rs`** —
read it there. Do not trust any field list written down elsewhere, this skill
included: the struct has grown steadily and every reproduced list has been
wrong within weeks. `rg -n "pub struct Settings" -A 60 src/config.rs` prints the
current one, with a doc comment per field.

The field *groups* are stable, and they mirror the settings-menu panes
(`settings_menu::Pane::ALL`): Appearance, Terminal, Bell & Alerts, Window
Manager, Keybindings, Agents, Startup. A new setting belongs to one of those
panes or it needs a pane.

The shell a **bare** new terminal spawns is one of these settings, not a
hardcode: `Settings::default_shell` (`src/config.rs`, a `DefaultShell` enum
resolved to a `terminal::Shell` by `DefaultShell::to_shell`), edited in the
Terminal pane via `settings_menu::Field::DefaultShellF`, and read at each spawn
site in `src/main.rs` / `src/wm.rs` through `config::live(ctx)`. It is only the
default: a project titlebar's "+" menu offers **New PS / CMD / SH terminal**
(`Act::AddTerm(WinId, Shell)` in `src/wm.rs`), which picks the shell for that
one Session and ignores the setting.

Contracts (load-bearing — do not "clean up"):

- **`#[serde(default)]` on `Settings` is load-bearing.** It makes an old file
  missing a new field AND a file written by a newer foreman with extra fields
  both load cleanly. Removing it breaks every existing install on the next
  added field. `src/config.rs`'s serde tests pin exactly this.
- **`Settings::sanitize` runs on load and clamps every numeric field.** This is
  the second half of the compat story and the half people forget: a
  hand-edited file cannot violate an invariant, notably that
  `send_settle_ms` stays far below `MAX_SETTLE_MS`/`REPLY_TIMEOUT`. A new
  numeric field with a legal range gets a clamp line here, or it is a hole.
- **`load_json` never panics** — missing file, unreadable file, or invalid
  JSON all fall back to `T::default()` (stderr warning for the non-missing
  cases). A corrupt config must never take the app down.
- **`save_json` is atomic** — serialize, write a sibling `.tmp`, rename over
  the target. A crash mid-write leaves the previous good file intact.
- **Settings, not logs** — growing append-only data (chat history) uses JSONL
  elsewhere, intentionally.

Flow: `App::new` loads once; each frame the app seeds the live struct into egui
context data (`config::seed_live` / `config::live`) so deep consumers
(`terminal.rs`, `wm.rs`) read one value without threading a parameter through
every call. The font size additionally round-trips: Ctrl+Scroll changes it
mid-frame, the app reads it back after the draw, and persists on divergence
after `FONT_SAVE_DEBOUNCE` — a scroll gesture writes the file once, not once
per notch.

Font-size specifics: `DEFAULT_FONT_SIZE` is also the Ctrl+0 reset target;
`MIN_FONT_SIZE`..`MAX_FONT_SIZE` clamp in the pure `input::zoom_step` and in
`Session::set_font_size`; the step is `Settings::zoom_step` points per wheel
notch, where a notch is `input::WHEEL_NOTCH_PX` of smoothed scroll — **the same
unit the scroll accumulator uses, deliberately.** Accumulating zoom against row
height instead was Issue #8, and `src/input.rs` carries the pinned regression
test ("Issue #8 cause (1): accumulate against WHEEL_NOTCH_PX, not row height").

## Axis 2: `%APPDATA%\foreman\keybindings.json` (`src/keymap.rs`)

The Keymap user file. Its **save** path was migrated onto the config.rs layer —
`Keymap::save` calls `crate::config::save_json`, so it is atomic like every
other settings write. Its **load** path is still bespoke, and that is the part
worth understanding, because it is not a plain struct deserialize:

- **Merge over in-code defaults** (`Keymap::load` → `Keymap::merge`): defaults
  live in `Keymap::default` (code); the file is merged *over* them. A command
  absent from the file keeps its default Chord — **so new commands always get a
  default binding even against an old user file**. The file is authoritative
  for every command it mentions (a rebind doesn't resurrect the default).
  That asymmetry is the whole reason the bespoke load exists; a plain
  deserialize would leave old configs unbound for every new command.
- It reads `APPDATA` directly rather than through `config::config_dir()` — the
  one remaining split. Migrating it is fine opportunistically
  (`docs/settings-persistence.md`), not urgent.
- **Malformed file falls back silently to defaults** (stderr warning, never a
  crash).
- Written by the in-app keybindings editor (`src/settings.rs` signals
  `Outcome::Changed`; the desktop `WindowManager` calls `keymap.save()`). Note:
  the module doc at the top of `src/keymap.rs` still says "There is no write
  path in this phase — the file is hand-edited". **That comment is stale** —
  the write path exists.

On-disk shape (from `struct KeymapFile`, `src/keymap.rs`; both fields
optional, partial files valid):

```json
{
  "leader": { "key": "B", "ctrl": true },
  "bindings": [
    { "key": "Q", "command": "CloseTerm" },
    { "key": "W", "alt": true, "command": { "Split": "Up" } }
  ]
}
```

Unit-variant commands serialize as bare strings, directional ones as
`{"Variant": "Left|Right|Up|Down"}` (serde external tagging; the unit form is
pinned by a test in `src/keymap.rs`). Modifiers default to false and may be
omitted. Key names are the stable strings from `key_to_name`: `"Left"`,
`"Tab"`, `"Comma"`, `"Questionmark"`, letters `"A"`..`"Z"`, etc.

### Default bindings (`Keymap::default`, `src/keymap.rs`)

**Leader = `Ctrl+B`.** Every Chord below is pressed *after* the Leader. The
authority is `Keymap::default`; the table below is a reading aid and any
disagreement means the code is right.

Naming gotcha: `TermSnap`/`ProjSnap` **kept their serialized names** for file
compatibility, but since the Layout tree landed they act as *move-in-tree* —
their labels are "Move terminal/project …". Hand-edit files using the
`TermSnap`/`ProjSnap` names.

| Chord | Command (serialized name) | Label |
|---|---|---|
| `←` `↓` `↑` `→` | `TermFocus(Left/Down/Up/Right)` | Focus terminal in direction |
| `W` `A` `S` `D` | `TermSnap(Up/Left/Down/Right)` | Move terminal (tree move) |
| `Alt+W/A/S/D` | `Split(Up/Left/Down/Right)` | Split new terminal in direction (tabs on collision) |
| `Ctrl+←/↓/↑/→` | `ProjFocus(Left/Down/Up/Right)` | Focus project in direction |
| `Ctrl+W/A/S/D` | `ProjSnap(Up/Left/Down/Right)` | Move project (tree move) |
| `C` | `NewTerm` | New terminal |
| `X` | `CloseTerm` | Close terminal |
| `Z` | `ZoomTerm` | Zoom terminal (overlay, tree untouched) |
| `,` | `Rename` | Rename focused window |
| `F` | `TermFloat` | Float / re-tile terminal |
| `G` | `OpenChat` | Open project Chat viewer |
| `Tab` | `TabCycle` | Next tab; falls back to last-terminal toggle when not a Tab stack |
| `Shift+Tab` | `TabPrev` | Previous tab (no fallback) |
| `P`, `Shift+P` | `NewProject` | New Project (picker); canonical chord is `Shift+P` |
| `Ctrl+X` | `CloseProject` | Close project |
| `Ctrl+Z` | `ZoomProject` | Zoom project |
| `Ctrl+F` | `ProjFloat` | Float / re-tile project |
| `Ctrl+Tab` | `LastProject` | Toggle last project |
| `?`, `Shift+?` | `Help` | Bindings cheat sheet; canonical is `Shift+?` |
| `Ctrl+,` | `OpenSettings` | Keybindings editor |

- `LastTerm` is dispatchable but **has no default Chord** — `TabCycle`'s
  fallback covers it. Bindable by hand or via the editor.
- vi `h/j/k/l` focus was deliberately dropped from defaults; re-add via the
  editor if wanted.

### Adding a new `Command` (checklist)

1. Add the variant to `enum Command` (`src/keymap.rs`).
2. Add it to `Command::ALL` — the editor and `?` overlay iterate this.
3. Add its default Chord in `Keymap::default` — the comment there says exactly
   this; the test `all_commands_have_a_default_chord_and_metadata` fails
   otherwise.
4. Add `label()` and `group()` arms — missing arms are compile errors.
5. Nothing else: the merge contract gives existing users the new default while
   preserving their overrides.

## Axis 3: environment variables

### Injected into every Session (`src/wm.rs` `fn term_env`)

Read the function for the current list; the entries with non-obvious reasons:

| Var | Value | When / why |
|---|---|---|
| `FOREMAN` | `1` | Always. Also what the kill-foreman hook checks to avoid killing its own host |
| `FOREMAN_TERMINAL_ID` | `t<id>` | Always. Also the Session's stable Member id |
| `COLORTERM` | `truecolor` | Always (capability advertisement — see terminal-emulation-reference) |
| `TERM` | `xterm-256color` | Always |
| `KITTY_WINDOW_ID` | `1` | Always. The narrowest signal that makes agent CLIs pick the kitty graphics protocol. `TERM` stays truthful because foreman implements the graphics *subset* (`src/graphics.rs`), not all of kitty |
| `FOREMAN_PROJECT_ID` | `p<id>` | **Only** Sessions inside a Project (the nested manager has a tag). Desktop-level Sessions get none |
| `FOREMAN_EXE` | Full path to the running exe | When `current_exe()` resolves — `target\debug` is not on PATH, so agents need this to find the CLI |
| `FOREMAN_TITLE_PIPE` | Random per-process local pipe name | When the title listener starts. Global prompt hooks inherit it from the Session and therefore route only to the Foreman instance that spawned that Session |

Consequence of the `FOREMAN_PROJECT_ID` conditional: the CLI's self-target
verbs (bare `foreman close`, `foreman send` without `--terminal`) require BOTH
`FOREMAN_TERMINAL_ID` and `FOREMAN_PROJECT_ID` (a `tN` is only unique within
its Project), so a desktop-level Session cannot self-target and gets the exact
error
`cannot resolve your own pane without FOREMAN_PROJECT_ID (terminal ids are only unique within a project)`.
Verb-by-verb behavior belongs to **foreman-run-and-operate**.

### Consumed by foreman

`rg -n "env::var" src/` is the authoritative sweep. The load-bearing ones:

| Var | Read at | Purpose |
|---|---|---|
| `APPDATA` | `src/config.rs` `config_dir`, `src/keymap.rs` `load`/`save` | Locate `%APPDATA%\foreman`. Unset (nearly impossible on Windows) → in-code defaults, no persistence. Also where `foreman_panic.log` lands |
| `CLAUDE_CONFIG_DIR` | `src/skills_install.rs`, `src/agent_hooks.rs`, `src/terminal_titles.rs` | Claude skills/hooks/session root when non-empty; else `%USERPROFILE%\.claude` |
| `CODEX_HOME` | `src/skills_install.rs`, `src/agent_hooks.rs`, `src/terminal_titles.rs` | Codex skills/hooks/session root when non-empty; else `%USERPROFILE%\.codex` |
| `GROK_HOME` | `src/agent_hooks.rs`, `src/terminal_titles.rs` | Grok hook/session root when non-empty; else `%USERPROFILE%\.grok` |
| `USERPROFILE` / `HOME` | skill, hook, and title-context resolvers | Non-empty fallback base for the provider homes above; an empty override is treated as absent, never as the current directory |
| `FOREMAN_PROJECT_ID`, `FOREMAN_TERMINAL_ID` | `src/control.rs` (CLI client mode) | Self-target defaults for `open`/`chat`/`close`/`send`/`snapshot`/`view` |
| `FOREMAN_TITLE_PIPE` | `src/title_notify.rs` (`title-event` client mode) | Routes one passive prompt event to the owning GUI instance; absent/invalid/unreachable is a silent no-op |
| `FOREMAN_EXE` | **Not read directly by foreman source** | Consumed by agent-facing skills and the managed prompt-hook command to invoke the exact running Foreman executable |

### Skill auto-install as a config side effect

On GUI startup — **gated on `Settings::install_skills`** — foreman writes
managed copies of the user-facing agent skills into `<claude>/skills` and
`<codex>/skills` (the Codex side also gets `agents/openai.yaml` variants), each
stamped `managed by foreman; edits are overwritten on launch`. Idempotent
byte-compare, atomic rename, best-effort (failures logged, never block launch).
Renamed/removed skills are purged via `OBSOLETE_SKILLS`.

Which skills ship is `CLAUDE_SKILLS` / `CODEX_SKILLS` in
`src/skills_install.rs`; the bundle contents are `include_str!`ed from this
repo at compile time, so **the source of truth is `.claude/skills/` and
`.codex/skills/` here — edit those and rebuild to propagate.** `rg -n
'include_str!' src/skills_install.rs` lists exactly what is embedded. Add a
bundle without adding its `include_str!` and it silently ships nothing.

### Agent naming hooks as a config side effect

When `Settings::auto_name_agent_sessions` changes from off to on — and at GUI
startup while it is already on — `src/agent_hooks.rs` updates three global
hook targets in one background install pass:

- Claude: `CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json`
- Codex: `CODEX_HOME/hooks.json` or `~/.codex/hooks.json`
- Grok: `GROK_HOME/hooks/foreman-session-naming.json` or the default equivalent

Claude/Codex are semantic merges; Grok owns only its dedicated file. Existing
unrelated hooks survive, malformed JSON is refused, the first replaced file is
backed up as `.pre-foreman.bak`, and repeated installs are byte-stable. Empty
home overrides fall back — they must never resolve to a relative path. Disabling
naming does not rewrite user files; installed passive hooks remain, but the GUI
drops their events before any provider request. This side effect is independent
of `Settings::install_skills`.

## Axis 4: tuning constants (compile-time)

These are code-level knobs: changing one means a rebuild. Several have since
grown a `Settings` field that supersedes them at runtime (the const then only
documents the default) — check for one before assuming a value is fixed. Each
has a rationale comment at its definition; read it before changing, then route
the change through **foreman-change-control**. Current values:
`rg -n "^(pub )?const " src/` — do not trust a value copied into prose.

| Constant | Where | Governs |
|---|---|---|
| `SUBMIT_DELAY` | `src/ready.rs` | Injected text→Enter split so agent CLIs (Claude Code) don't treat the burst as one paste with a literal newline. Set by a live incident, not by taste |
| `WHEEL_NOTCH_PX` | `src/input.rs` | Smoothed-scroll points per wheel notch — the unit for BOTH scroll and Ctrl+Scroll zoom. Using row height for zoom instead was Issue #8 |
| `PIPE` | `src/control.rs` | Control plane named-pipe name (transport: foreman-run-and-operate) |
| `REPLY_TIMEOUT` | `src/control.rs` | Pipe server wait for the GUI to answer one request |
| `CONNECT_TIMEOUT` | `src/control.rs` | Client deadline connecting to a busy pipe |
| `MAX_INFLIGHT` | `src/control.rs` | Concurrent pipe handler cap (bounds thread leak from wedged clients) |
| `PROMPT_CHARS`, `CONTEXT_PROMPT_CHARS`, `OPENING_PROMPTS`, `TRANSCRIPT_BYTES` | `src/terminal_titles.rs` | Bounded title-provider context: at most 2,000 current chars + three 600-char opening prompts from a 512 KiB transcript prefix |
| `QUEUE_MAX_AGE`, `PROCESS_TIMEOUT`, `OUTPUT_BYTES` | `src/terminal_titles.rs` | Single Title lane freshness/deadline/output caps; model work never blocks GUI or hook execution |
| `DEFAULT_HISTORY` | `src/control.rs` | Lines a bare `foreman chat --history` returns |
| `MAX_SETTLE_MS` | `src/wm.rs` | Hard cap on total settle wait. **Invariant: `MAX_SETTLE_MS` < `REPLY_TIMEOUT`** so the pipe server's reply timeout never fires before a settle reply lands |
| `STALE_AFTER` | `src/chat.rs` | Crew board fallback: a Member unheard this long renders its age in amber (`Settings::crew_stale_secs` is the live value) |
| `MIN_RATIO` | `src/layout.rs` | Layout tree: no tile shrinks below this fraction of its split |
| `REFRESH_EVERY` | `src/proc.rs` | Process-table rescan cadence for tab agent icons |
| `FONT_SAVE_DEBOUNCE` | `src/main.rs` | Zoom→disk save debounce (Axis 1) |
| repaint cadence | `App::ui`, `src/main.rs` | Adaptive repaint scheduling: a hot tick for a window after input/PTY output, a slow idle tick otherwise (inline literals, not named consts) |
| `DEFAULT/MIN/MAX_FONT_SIZE`, `FONT_ZOOM_STEP`, `DEFAULT_THEME` | `src/config.rs` | Defaults and clamps *behind* persisted `Settings` fields — the only constants backing user config |

**The settle default is no longer a constant.** It was `DEFAULT_SETTLE_MS` in
`src/wm.rs`; it is now `Settings::send_settle_ms` (`src/config.rs`) — persisted,
editable in the settings menu, and clamped by `Settings::sanitize`. The full
nesting is:

`send_settle_ms` (clamped ≤ 2000) < `MAX_SETTLE_MS` (4000) < `REPLY_TIMEOUT`
(5 s) < `CONNECT_TIMEOUT` (10 s)

That chain is the reason for the clamp: a user-editable timer sits inside three
timers it must not outlive, and the clamp is the only thing enforcing it. Any
new timer on the request path re-runs that analysis
(**foreman-proof-and-analysis-toolkit**, Recipe 5).

## Axis 5: Claude Code hooks (`.claude/settings.json`)

Dev-workflow configuration for sessions working ON this repo — not app config.

| Event | Matcher | Script | Does |
|---|---|---|---|
| `PreToolUse` | `Bash` | `.claude/hooks/kill-foreman.ps1` | No-op when `FOREMAN=1` (session runs inside foreman — killing it would kill the host; incident 2026-07-09). Otherwise, if the command matches the cargo regex below: kill foreman **by exe path** (only instances under this repo's `target\`; by-name killed the user's installed foreman — incident 2026-07-15), sleep 500 ms. Always exits 0 (never blocks) |
| `PostToolUse` | `Edit` or `Write` (one alternation matcher) | `.claude/hooks/cargo-fmt.ps1` | If the edited path ends `.rs`: run `cargo fmt` (prepends `C:\w64devkit\bin` and `%USERPROFILE%\.cargo\bin` to PATH). Always exits 0 |
| `PostToolUse` | same `Edit`/`Write` matcher (second hook in the array) | `.claude/hooks/cite-guard.ps1` | If the edited path is an agent-loaded `.md`: flag `src/foo.rs:NNN` line cites and backticked symbols cited beside a `src/*.rs` path that have zero hits in the tree. Exits **2** with findings on stderr so they come back as feedback; exits 0 on anything else, including its own errors. Scope, skips and suppression markers are documented in the script header — see **foreman-docs-and-writing** for the rule it enforces |

The matcher and regex literals, exactly as they appear in the files (a markdown
table cannot hold a raw `|`, so copy them from here, not from the rows above):

```
# .claude/settings.json, PostToolUse matcher
Edit|Write

# .claude/hooks/kill-foreman.ps1, the -match pattern
cargo\s+(build|run|test)
```

Gaps (verified against the matchers/scripts):

- **Bash-tool-only**: the kill hook matches ONLY the `Bash` tool. `cargo build`
  run via the PowerShell tool (or any other route) does not trigger it — you
  get the classic `Access is denied (os error 5)` link failure. Kill manually
  first (see **foreman-build-and-env**). Abbreviations (`cargo b`) also miss
  the regex.
- The fmt hook misses edits made through Bash (`sed` etc.) and non-`.rs` paths
  by design.

`.claude/settings.local.json` (ignored by git, machine-local) holds a
`permissions.allow` list.

## How to add a new persisted setting (checklist)

1. Add the field to `struct Settings` (`src/config.rs`) and its default to the
   `Default` impl.
2. **Keep `#[serde(default)]`** on the struct — it is the entire
   forward+backward compat story.
3. **If it is numeric with a legal range, add a clamp line to
   `Settings::sanitize`.** Skipping this is how a hand-edited file gets to
   violate a timing invariant.
4. Read via `Settings::load()` / `config::live(ctx)`, write via
   `settings.save()` — do NOT hand-roll file I/O; for a genuinely separate
   file, reuse `load_json`/`save_json` with a new file name (settings bags
   only — never logs).
5. **Never save on a hot path.** Anything that changes rapidly gets a debounce;
   copy the `FONT_SAVE_DEBOUNCE` pattern in `src/main.rs`.
6. Surface it in the matching `settings_menu::Pane` — add a `RowSpec` and the
   read/write arms for its `Field`. Leaving it file-only is allowed but must be
   a *stated* choice: say so in the field's doc comment, and keep that comment
   true. (`rg -n "Field::" src/settings_menu.rs` against the `Settings` field
   list tells you which fields are actually surfaced. `bell`'s doc comment
   still claims "File-only in v1 — no settings UI"; it has a Bell pane row, so
   do not copy it as the model.)
7. Add serde compat tests mirroring the existing ones in `src/config.rs`:
   missing field → default, known field round-trips, unknown fields ignored.
8. Update `docs/settings-persistence.md` and classify the change per
   **foreman-change-control**.

## When NOT to use this skill

- CLI verbs, their flags, and runtime timeout behavior →
  **foreman-run-and-operate** (this skill says what a knob *is*; that one says
  what it does to a running request).
- Whether a constant change is allowed → **foreman-change-control**.

## Provenance and maintenance

| Claim | Re-verify with |
|---|---|
| The `Settings` field set (never trust a list) | `rg -n "pub struct Settings" -A 60 src/config.rs` |
| Every numeric field is clamped on load | `rg -n "fn sanitize" -A 20 src/config.rs` |
| Which skills are embedded and shipped | `rg -n "include_str!" src/skills_install.rs` |
| `keymap.rs` module doc still claims "no write path" (delete the flag above when fixed) | `rg -n "no write path" src/keymap.rs` |
| The `.claude` hook matchers | `cat .claude/settings.json` |
