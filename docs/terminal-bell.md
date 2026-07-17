# Terminal Bell (visual attention pulse)

**Status (2026-07-17): built.** Decisions were locked in a design grill
(2026-07-16); implemented on `feat/terminal-bell`. Phase 6 of
`docs/epics/terminal-completeness-epic.md` bundled title + bell — this doc is
**Bell only**. OSC tab titles stay a separate cut.

Glossary term: **Bell** in `CONTEXT.md`.

## What it does

When a Session's program rings BEL (`\a` / ASCII 0x07), Foreman shows a
**short-lived visual pulse** on that Session's chrome so you can find which pane
rang without reading the grid.

It is **attention routing**, not an alarm and not an OS notification.

## Why it exists

Foreman runs many Sessions. A build finishing with `printf '\a'`, or a TUI
ringing on a bad key, is useless if nothing moves outside the text cells.
Alacritty / Windows Terminal flash (and often sound); today Foreman drops
`Event::Bell` on the floor.

Supervision push (OS toast / sound when an agent needs you) is a **later** job
— chat already tracks that under human push-notifications. Do not fold it into
this feature.

## What you see (v1)

| Surface | Behavior |
|---------|----------|
| **Win border** | Pulses while **any** tab in that Win's stack has an active Bell |
| **Tab chip** | Only the ringing Session's chip pulses |
| **Bare** lone tiled terminal (no chrome) | Edge / inset ring or light overlay on the content rect — only path without inventing chrome |
| **Minimized** | No visible surface until restored (silent gap, accepted) |
| **Panel / project bubble-up** | **Not** v1 |

- **Color:** caret amber family (`theme::CARET` / RGB `231, 169, 63`). Thin borders
  may use the same hue at higher/full alpha so the stroke stays readable.
- **Duration:** about **300 ms** from the last ring.
- **Spam:** a new BEL while pulsing **restarts** the timer (one continuous pulse,
  not a disco).
- **Cancel:** pulse ends early when that Session becomes the **keyboard-focused**
  terminal. Hover does not cancel. BEL on an already-focused Session still does
  a short pulse.
- **Sound / OS toast:** not v1.

## How to use (once built)

### Try it

In any terminal Session:

```powershell
printf "`a"
# or
echo `a
```

You should see the border / tab chip (or bare edge) flash amber briefly.

### Turn it off

Master switch in `%APPDATA%\foreman\settings.json`:

```json
{
  "bell": false
}
```

- **Default:** `true` (missing key = on, via `#[serde(default)]`).
- **Scope:** mutes **all** Bell attention — visual now; later sound/push must
  honor the same key.
- **UI:** file only in v1 — no settings checkbox, no leader mute chord.

## How it works

1. alacritty emits `Event::Bell` when BEL is parsed.
2. `Listener` records it on the **Session** (not the Win) — e.g. pulse deadline /
   last ring time.
3. Each frame, if `Settings.bell` and the pulse deadline is still in the future:
   - `wm` paints border (any tab pulsing) + the ringing tab chip(s);
   - bare / content path paints the bare fallback from the same Session state.
4. Keyboard focus landing on that Session clears the pulse.
5. Further BELs while active extend (restart) the deadline.

Multiple Sessions can pulse at once.

## Out of scope

- OSC 0/2 live tab titles (separate from Bell; title is partly captured for icons only today)
- Sound, OS notifications, chat `@you` push
- Task-manager panel row highlight
- Project-level “something inside rang” chrome
- In-app settings editor / leader toggle for `bell`

## Gotchas

- **Not focus.** Focus border stays the near-white focus ladder; Bell is caret
  amber and temporary. Do not reuse focus color for the pulse.
- **Not a sticky badge.** When the pulse ends, chrome looks normal. No “unread
  bell” counter.
- **Debounce is restart, not drop.** Spam keeps the pulse alive until rings stop;
  it does not ignore BEL while flashing.
- **Ready latch.** Wiring must not break `Event::PtyWrite` → ready. Bell is a
  sibling of title/color handling; do not route it through the PtyWrite flush
  path that latches Ready.
- **Bare Wins** have no titlebar border — if you only paint `wm` borders, bare
  Sessions stay silent. The content-rect fallback is required.
- Epic Phase 6 still lists title+bell together; implement against **this** doc
  for Bell, not the epic's combined “done when” alone.

## Acceptance (verified 2026-07-17)

Visual items verified by screenshot + border-pixel sampling against a demo
build (`FOREMAN_BELL_DEMO`, since reverted) with continuous `\a` loops; logic
items by unit tests (`config::tests::bell_*`, `terminal::tests::*bell*`,
`wm::tests::bell_pulses_the_stack_until_expiry_or_clear`).

- [x] `printf '\a'` pulses an unfocused Session's border (and tab chip if stacked)
- [x] Background tab rings → that chip pulses; border pulses; other chips do not
      (quiet neighbor window measured plain gray)
- [x] Bare lone tile still shows a pulse (inset ring, exact `231,169,63`)
- [x] ~300 ms; second BEL mid-pulse restarts (unit test; live loop stayed lit
      for minutes as one continuous pulse)
- [x] Focusing the ringing Session cancels early (unit-tested transition rule)
- [x] `"bell": false` in settings.json → no pulse (all borders measured gray
      with three live ringers); default / missing key → on
- [x] Focused Session that rings still does a short pulse (focused window's
      border pulsed amber over the focus color)
- [x] Minimized: no crash; visible only after restore — same `keepalive` path a
      hidden ringing tab exercised for minutes in the demo
- [x] Existing Ready / DSR / title / color-request paths still green (671 tests
      pass)

## Key files

| File | Role |
|------|------|
| `src/terminal.rs` | `Event::Bell` arm + `BELL_PULSE` (300 ms); Session pulse state (`bell_active`/`clear_bell`); focus-gain cancel (`bell_cancelled_by_focus`); `bell_enabled` ctx gate |
| `src/wm.rs` | `Win::bell_active` (border rule) + `Content::bell_active` (chip rule); border/chip/bare-ring paint; 30 ms repaint tail |
| `src/main.rs` | publishes `settings.bell` into the egui ctx each frame (beside `set_font_size`) |
| `src/config.rs` | `Settings.bell: bool` (default `true`) |
| `src/theme.rs` | `theme::BELL` — caret amber at full alpha (a 1 px stroke antialiases to ~65%, still clearly warm vs the gray border ladder) |
| `CONTEXT.md` | Glossary: **Bell** |
| `docs/epics/terminal-completeness-epic.md` | Historical Phase 6; title half still open |

## Related

- `docs/epics/terminal-completeness-epic.md` — Phase 6 (title + bell backlog)
- `docs/chat-missing-features.md` §7 — human push-notifications (later C, not Bell)
- `docs/cursor-rendering.md` — caret color source of truth for the pulse hue
- `docs/window-chrome.md` — borders, bare rule, tab chips
