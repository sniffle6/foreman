---
name: foreman-config-and-flags
description: Use when auditing or changing any foreman configuration axis - %APPDATA%\foreman settings.json or keybindings.json, font-size zoom persistence, default Leader/Chord bindings, injected env vars (FOREMAN, FOREMAN_TERMINAL_ID, FOREMAN_PROJECT_ID, FOREMAN_EXE, TERM, COLORTERM), CLAUDE_CONFIG_DIR/CODEX_HOME skill installs, tunable timing constants, .claude hooks, adding a persisted setting. Symptoms - setting not saved, binding ignored, "FOREMAN_TERMINAL_ID unset", zoom resets on restart.
---

# Foreman configuration and flags

Every configuration axis in foreman: the two persisted files, the env-var
contract, the compile-time tuning constants, and the Claude Code hooks — plus
the checklist for adding a new axis. Baseline: committed HEAD `7fda1c2`
(2026-07-01). All `file:line` cites are against that commit; re-verify with the
one-liners in Provenance before trusting a line number.

There is **no config file for the tuning constants** — they are compile-time
Rust `const`s. Changing one means a rebuild, and goes through
**foreman-change-control** like any other change.

## Config axes at a glance

| Axis | Where | Kind | Status |
|---|---|---|---|
| App settings (font size) | `%APPDATA%\foreman\settings.json` via `src/config.rs` | User-facing, persisted, written by the app | Production |
| Keymap overrides | `%APPDATA%\foreman\keybindings.json` via `src/keymap.rs` | User-facing, persisted, hand-editable AND written by the in-app editor | Production (older bespoke persistence — see below) |
| Env vars injected into every Session | `term_env`, `src/wm.rs:787-804` | Contract for agents/CLI inside Sessions | Production |
| Env vars foreman consumes | `APPDATA`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `USERPROFILE`, `FOREMAN_*` (CLI client) | Ambient environment | Production |
| Skill auto-install | `src/skills_install.rs`, runs at GUI startup (`src/main.rs:452`) | Writes managed SKILL.md files into Claude/Codex skill dirs | Production, best-effort |
| Tuning constants | `const`s across `src/` (table below) | Code-level knobs, compile-time only | Production values; change via foreman-change-control |
| Claude Code hooks | `.claude/settings.json` + `.claude/hooks/*.ps1` | Dev-workflow automation for sessions working ON this repo (not app config) | Production |
| Local permissions | `.claude/settings.local.json` (untracked/ignored) | Machine-local Claude Code permissions | Local only |

Not configuration (common confusions):

- **Chat log persistence** is an append-only JSONL event log, deliberately NOT
  through `config.rs` — "this is for settings, not logs" (`src/config.rs:7-9`,
  `docs/settings-persistence.md`). See `docs/chat-persistence.md`.
- **Default shell for new Sessions** is hardcoded `Shell::PowerShell`
  (`src/wm.rs:2106`, `:3474`); the per-Project `PS`/`CMD`/`SH` chips
  (`src/wm.rs:2940-2943`) add other shells per-Session. No config axis exists
  for it (candidate, not built).
- **GUI binary flags**: there are none. Any argv beyond the exe name switches
  `foreman.exe` into Control-plane CLI client mode (`src/main.rs:447-450`).
  Verbs and flags live in **foreman-run-and-operate**.

## Axis 1: `%APPDATA%\foreman\settings.json` (`src/config.rs`)

The canonical settings layer. Reuse it for any new flat persisted setting.

| Fact | Value | Cite (as of 2026-07-01) |
|---|---|---|
| File | `settings.json` under `config_dir()` = `%APPDATA%\foreman` (created on demand) | `src/config.rs:24,29-34` |
| Contents | `Settings { font_size: f32 }` — the global terminal text size | `src/config.rs:93-96` |
| Default | `DEFAULT_FONT_SIZE = 13.0` (also the Ctrl+0 reset target) | `src/config.rs:16`; reset at `src/terminal.rs:816-818`, request set in `src/input.rs:102-105` |
| Clamp | `MIN_FONT_SIZE = 6.0` .. `MAX_FONT_SIZE = 40.0`, applied in pure `zoom_step` and `set_font_size` | `src/config.rs:19-20`; `src/input.rs:138-141`; `src/terminal.rs:330-333` |
| Step | `FONT_ZOOM_STEP = 1.0` point per wheel notch (a notch ≈ 50 smoothed-scroll points, `ZOOM_NOTCH_PX`, `src/terminal.rs:305`) | `src/config.rs:22` |
| Save debounce | `FONT_SAVE_DEBOUNCE = 400 ms` after the last zoom change — a scroll gesture writes the file once, not once per notch | `src/main.rs:46,378-390` |

Contracts (load-bearing — do not "clean up"):

- **`#[serde(default)]` on `Settings` is load-bearing** (`src/config.rs:92`).
  It makes an old file missing a new field AND a file written by a newer
  foreman with extra fields both load cleanly. Removing it breaks every
  existing install on the next added field. The tests at
  `src/config.rs:122-144` pin exactly this.
- **`load_json` never panics** — missing file, unreadable file, or invalid
  JSON all fall back to `T::default()` (stderr warning for the non-missing
  cases) (`src/config.rs:40-68`). A corrupt config must never take the app down.
- **`save_json` is atomic** — serialize, write a sibling `.tmp`, rename over
  the target (`src/config.rs:74-85`). A crash mid-write leaves the previous
  good file intact.
- **Settings, not logs** — growing append-only data (chat history) uses JSONL
  elsewhere, intentionally (`src/config.rs:7-9`).

Flow: `App::new` loads once (`src/main.rs:56`); each frame the app seeds the
live value into egui context data (`src/main.rs:374`, `terminal::set_font_size`)
so every Session reads one size, reads it back after the draw, and persists on
divergence after the debounce (`src/main.rs:378-390`).

## Axis 2: `%APPDATA%\foreman\keybindings.json` (`src/keymap.rs`)

The Keymap user file. This is the **older, bespoke persistence** that predates
`config.rs` — deliberately not migrated. Stance from
`docs/settings-persistence.md`: fine to leave; migrate onto
`config_dir()`/`save_json` **opportunistically if you touch it**. Two real
differences from the config.rs layer (verified):

- Its save is **not atomic** — plain `std::fs::write`, no `.tmp` + rename
  (`src/keymap.rs:393`).
- Its load has bespoke **merge-over-defaults** semantics (below), not a plain
  struct deserialize.

Contracts:

- **Merge over in-code defaults** (`src/keymap.rs:406-463`): defaults live in
  `Keymap::default` (code); the file is merged *over* them. A command absent
  from the file keeps its default Chord — **so new commands always get a
  default binding even against an old user file**. The file is authoritative
  for every command it mentions (a rebind doesn't resurrect the default).
- **Malformed file falls back silently to defaults** (stderr warning, never a
  crash) (`src/keymap.rs:417-441`).
- Written by the in-app keybindings editor (`src/settings.rs` signals
  `Outcome::Changed`; the desktop `WindowManager` calls `keymap.save()` at
  `src/wm.rs:3489`). Note: the module doc at `src/keymap.rs:7-8` still says
  "There is no write path in this phase — the file is hand-edited". **That
  comment is stale** — the write path exists (as of 2026-07-01).

On-disk shape (from `KeymapFile`, `src/keymap.rs:290-295`; both fields
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
`{"Variant": "Left|Right|Up|Down"}` (serde external tagging; unit form pinned
by the test at `src/keymap.rs:829-834`). Modifiers default to false and may be
omitted. Key names are the stable strings from `key_to_name`
(`src/keymap.rs:557-597`): `"Left"`, `"Tab"`, `"Comma"`, `"Questionmark"`,
letters `"A"`..`"Z"`, etc.

### Full default binding table (as of 2026-07-01, `Keymap::default`, `src/keymap.rs:469-551`)

**Leader = `Ctrl+B`** (`src/keymap.rs:548`). Every Chord below is pressed
*after* the Leader.

Naming gotcha: `TermSnap`/`ProjSnap` **kept their serialized names** for file
compatibility, but since the Layout tree landed they act as *move-in-tree* —
their labels are "Move terminal/project …" (`src/keymap.rs:127-135,144-149`).
Hand-edit files using the `TermSnap`/`ProjSnap` names.

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
  fallback covers it (`src/keymap.rs:529-534`, test at `:965-981`). Bindable by
  hand or via the editor.
- vi `h/j/k/l` focus was deliberately dropped from defaults
  (`src/keymap.rs:499-500`); re-add via the editor if wanted.

### Adding a new `Command` (checklist)

1. Add the variant to `Command` (`src/keymap.rs:19-52`).
2. Add it to `Command::ALL` (`src/keymap.rs:59-103`) — the editor and `?`
   overlay iterate this.
3. Add its default Chord in `Keymap::default` (`src/keymap.rs:469-551`) — the
   comment there says exactly this; the test
   `all_commands_have_a_default_chord_and_metadata` (`src/keymap.rs:965`)
   fails otherwise.
4. Add `label()` and `group()` arms — missing arms are compile errors.
5. Nothing else: the merge contract gives existing users the new default while
   preserving their overrides.

## Axis 3: environment variables

### Injected into every Session (`term_env`, `src/wm.rs:787-804`)

| Var | Value | When |
|---|---|---|
| `FOREMAN` | `1` | Always |
| `FOREMAN_TERMINAL_ID` | `t<id>` | Always. Also the Session's stable Member id (`src/wm.rs:728`) |
| `COLORTERM` | `truecolor` | Always (capability advertisement — see terminal-emulation-reference) |
| `TERM` | `xterm-256color` | Always |
| `FOREMAN_PROJECT_ID` | `p<id>` | **Only** Sessions inside a Project (the nested manager has a tag, `src/wm.rs:796-798`). Desktop-level Sessions get none |
| `FOREMAN_EXE` | Full path to the running exe | When `current_exe()` resolves (`src/wm.rs:800-802`) — `target\debug` is not on PATH, so agents need this to find the CLI |

Consequence of the `FOREMAN_PROJECT_ID` conditional: the CLI's self-target
verbs (bare `foreman close`, `foreman send` without `--terminal`) require BOTH
`FOREMAN_TERMINAL_ID` and `FOREMAN_PROJECT_ID` (a `tN` is only unique within
its Project — `src/control.rs:519-524,593-598`), so a desktop-level Session
cannot self-target and gets the exact error
`cannot resolve your own pane without FOREMAN_PROJECT_ID (terminal ids are only unique within a project)`.
Verb-by-verb behavior belongs to **foreman-run-and-operate**.

### Consumed by foreman

| Var | Read at | Purpose |
|---|---|---|
| `APPDATA` | `src/config.rs:30`, `src/keymap.rs:368,409` | Locate `%APPDATA%\foreman`. Unset (nearly impossible on Windows) → in-code defaults, no persistence |
| `CLAUDE_CONFIG_DIR` | `src/skills_install.rs:200` | Claude skills root when non-empty; else `%USERPROFILE%\.claude` (`:36-45`) |
| `CODEX_HOME` | `src/skills_install.rs:208` | Codex skills root when non-empty; else `%USERPROFILE%\.codex` (`:47-58`) |
| `USERPROFILE` | `src/skills_install.rs:201,209` | Fallback base for both of the above |
| `FOREMAN_PROJECT_ID`, `FOREMAN_TERMINAL_ID` | `src/control.rs:817-910` (CLI client mode) | Self-target defaults for `open`/`chat`/`close`/`send` |
| `FOREMAN_EXE` | **Not read by foreman source** | Consumed by agents (the user-facing foreman-dispatch / foreman-chat skills) to invoke the CLI |

### Skill auto-install as a config side effect

On every GUI startup (`src/main.rs:452`), foreman writes managed copies of the
`foreman-dispatch` and `foreman-chat` skills into `<claude>/skills` and
`<codex>/skills` (Codex also gets `agents/openai.yaml`), each stamped
`managed by foreman; edits are overwritten on launch`
(`src/skills_install.rs:12-13,115-157`). Idempotent byte-compare, atomic
rename, best-effort (failures logged, never block launch). Renamed/removed
skills are purged via `OBSOLETE_SKILLS` (`src/skills_install.rs:161`, empty as
of 2026-07-01). Source-of-truth copies: `.claude/skills/` and `.codex/skills/`
in this repo — edit those and rebuild to propagate.

## Axis 4: tuning constants (compile-time; as of 2026-07-01, HEAD `7fda1c2`)

All are code-level knobs — no file or env var overrides them. Each has a
rationale comment at its definition; read it before changing, then route the
change through **foreman-change-control**.

| Constant | Value | Cite | Governs |
|---|---|---|---|
| `SUBMIT_DELAY` | 150 ms | `src/terminal.rs:300` | Injected text→Enter split so agent CLIs (Claude Code) don't treat the burst as one paste with a literal newline |
| `ZOOM_NOTCH_PX` | 50.0 pts | `src/terminal.rs:305` | Smoothed-scroll points per Ctrl+Scroll zoom notch |
| `PIPE` | `"foreman"` | `src/control.rs:6` | Control plane named-pipe name (transport: foreman-run-and-operate) |
| `REPLY_TIMEOUT` | 5 s | `src/control.rs:10` | Pipe server wait for the GUI to answer one request |
| `CONNECT_TIMEOUT` | 10 s | `src/control.rs:17` | Client deadline connecting to a busy pipe |
| `MAX_INFLIGHT` | 64 | `src/control.rs:256` | Concurrent pipe handler cap (bounds thread leak from wedged clients) |
| `DEFAULT_HISTORY` | 20 | `src/control.rs:20` | Lines a bare `foreman chat --history` returns |
| `DEFAULT_SETTLE_MS` | 120 ms | `src/wm.rs:17` | Quiescence settle default for `foreman send` |
| `MAX_SETTLE_MS` | 4000 ms | `src/wm.rs:18` | Hard cap on total settle wait. **Invariant: `MAX_SETTLE_MS` < `REPLY_TIMEOUT`** so the pipe server's reply timeout never fires before a settle reply lands (`src/wm.rs:13-16`) |
| `CURSOR_SETTLE` | 50 ms | `src/caret.rs:24` | Caret gate: cursor must hold a cell this long before the painted caret adopts it |
| `INPUT_GRACE` | 150 ms | `src/caret.rs:30` | Caret gate: recent-typing window in which single-row moves are followed immediately |
| `STALE_AFTER` | 300 s (5 min) | `src/chat.rs:20` | Crew board: a Member unheard this long renders its age in amber |
| `MIN_RATIO` | 0.10 | `src/layout.rs:10` | Layout tree: no tile shrinks below this fraction of its split |
| `REFRESH_EVERY` | 1500 ms | `src/proc.rs:21` | Process-table rescan cadence for tab agent icons |
| `FONT_SAVE_DEBOUNCE` | 400 ms | `src/main.rs:46` | Zoom→disk save debounce (Axis 1) |
| repaint cadence | 4 ms hot / 100 ms idle, 250 ms hot window | `src/main.rs:415-419` | Adaptive repaint scheduling (inline literals, not named consts) |
| `DEFAULT/MIN/MAX_FONT_SIZE`, `FONT_ZOOM_STEP` | 13.0 / 6.0 / 40.0 / 1.0 | `src/config.rs:16-22` | User-facing via settings.json (Axis 1) — the only constants backing persisted user config |

Only the font-size group is user-facing configuration; everything else is a
code-level tuning knob.

**Known comment drift** (verified 2026-07-01): `src/control.rs:548` still says
"`settle_ms` is parsed but not yet honored (settle is the next phase)" — stale.
It IS honored: `src/wm.rs:938` uses `req.settle_ms.unwrap_or(DEFAULT_SETTLE_MS)`
and caps at `MAX_SETTLE_MS` (`src/wm.rs:951`), with tests at
`src/wm.rs:5356-5379`.

## Axis 5: Claude Code hooks (`.claude/settings.json`)

Dev-workflow configuration for sessions working ON this repo — not app config.

| Event | Matcher | Script | Does |
|---|---|---|---|
| `PreToolUse` | `Bash` | `.claude/hooks/kill-foreman.ps1` | If the command matches regex `cargo\s+(build\|run\|test)`: `Stop-Process -Name foreman`, sleep 500 ms. Always exits 0 (never blocks) |
| `PostToolUse` | `Edit\|Write` | `.claude/hooks/cargo-fmt.ps1` | If the edited path ends `.rs`: run `cargo fmt` (prepends `C:\w64devkit\bin` and `%USERPROFILE%\.cargo\bin` to PATH). Always exits 0 |

Gaps (verified against the matchers/scripts, as of 2026-07-01):

- **Bash-tool-only**: the kill hook matches ONLY the `Bash` tool. `cargo build`
  run via the PowerShell tool (or any other route) does not trigger it — you
  get the classic `Access is denied (os error 5)` link failure. Kill manually
  first (see **foreman-build-and-env**). Abbreviations (`cargo b`) also miss
  the regex.
- The fmt hook misses edits made through Bash (`sed` etc.) and non-`.rs` paths
  by design.

`.claude/settings.local.json` (ignored by git, machine-local) currently holds
`permissions.allow: ["Bash(powershell *)"]`.

## How to add a new persisted setting (checklist)

1. Add the field to `Settings` (`src/config.rs:93-96`) and its default to the
   `Default` impl (`src/config.rs:98-104`).
2. **Keep `#[serde(default)]`** on the struct (`src/config.rs:92`) — it is the
   entire forward+backward compat story.
3. Read via `Settings::load()`, write via `settings.save()` — do NOT hand-roll
   file I/O; for a genuinely separate file, reuse `load_json`/`save_json` with
   a new file name (settings bags only — never logs).
4. **Never save on a hot path.** Anything that changes rapidly gets a debounce;
   copy the `FONT_SAVE_DEBOUNCE` pattern (`src/main.rs:37-46,378-390`).
5. Add serde compat tests mirroring `src/config.rs:122-144`: missing field →
   default, known field round-trips, unknown fields ignored.
6. Update `docs/settings-persistence.md` (docs discipline:
   **foreman-docs-and-writing**) and classify the change per
   **foreman-change-control**.

## When NOT to use this skill

- CLI verbs, their flags, transport mechanics, timeouts-in-operation, and
  artifacts (`foreman_panic.log`) → **foreman-run-and-operate**.
- Recreating the toolchain, PATH, linker traps, build/test loop →
  **foreman-build-and-env**.
- Whether/how a constant change is allowed, review gates → **foreman-change-control**.
- Why the settle/caret seams exist and the threading model →
  **foreman-architecture-contract**; `TERM`/`COLORTERM` semantics →
  **terminal-emulation-reference**.
- Actually *using* dispatch/chat from inside a running foreman Session → the
  user-facing **foreman-dispatch** / **foreman-chat** skills (they are
  self-contained by design; don't send agents here for that).
- Measuring the effect of a knob change → **foreman-diagnostics-and-tooling**.

## Provenance and maintenance

Written 2026-07-01 against HEAD `7fda1c2` (clean tree at time of writing; the
frame-plan/geometry TDD work from earlier that day is committed in `7fda1c2`).
Line numbers WILL drift — re-verify from repo root (`H:/claude code/foreman`):

```powershell
# Axis 1: settings.json layer, font constants, serde(default), atomic save
rg -n "DEFAULT_FONT_SIZE|MIN_FONT_SIZE|MAX_FONT_SIZE|FONT_ZOOM_STEP|serde\(default\)|\.tmp" src/config.rs
rg -n "FONT_SAVE_DEBOUNCE" src/main.rs
# Axis 2: keymap defaults, leader, merge, non-atomic save
rg -n "fn default|leader: Chord::new|fs::write" src/keymap.rs
rg -n "keymap.save" src/wm.rs
# Axis 3: injected + consumed env
rg -n "fn term_env" -A 18 src/wm.rs
rg -n "env::var" src
# Axis 4: every tuning constant in one sweep
rg -n "const (SUBMIT_DELAY|ZOOM_NOTCH_PX|REPLY_TIMEOUT|CONNECT_TIMEOUT|MAX_INFLIGHT|DEFAULT_HISTORY|DEFAULT_SETTLE_MS|MAX_SETTLE_MS|CURSOR_SETTLE|INPUT_GRACE|STALE_AFTER|MIN_RATIO|REFRESH_EVERY)" src
# settle honored (control.rs:548 comment is the stale one)
rg -n "settle_ms.unwrap_or|not yet honored" src
# Axis 5: hooks + matchers
Get-Content .claude/settings.json; Get-Content .claude/hooks/kill-foreman.ps1
# Skill install roots + obsolete hook
rg -n "CLAUDE_CONFIG_DIR|CODEX_HOME|OBSOLETE_SKILLS" src/skills_install.rs
```

Drift-prone items to recheck first: every `file:line` cite; the default
binding table (any new `Command` lands in `Keymap::default`); the two flagged
stale comments (`src/keymap.rs:7-8`, `src/control.rs:548`) — delete the flags
here once the comments are fixed; `OBSOLETE_SKILLS` (empty today); the
`.claude/settings.json` matchers.
