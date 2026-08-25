# Terminal Bell (visual attention pulse)

**Status (2026-07-17): built** (v1 shipped, then reworked same-day to sticky +
animated + panel per user direction). Original decisions locked in a design
grill (2026-07-16); implemented on `feat/terminal-bell`. Phase 6 of
`docs/epics/terminal-completeness-epic.md` bundled title + bell — this doc is
**Bell only**. OSC tab titles stay a separate cut.

Glossary term: **Bell** in `CONTEXT.md`.

## What it does

When a Session's program rings BEL (`\a` / ASCII 0x07), Foreman **latches** an
attention state on that Session and shows a **breathing amber pulse** on its
chrome — and on its row in the Sessions panel — until you give that Session
keyboard focus. Find the ringing pane, click it, the pulse dies.

It is **attention routing**, not an alarm and not an OS notification.

## Why it exists

Foreman runs many Sessions. A build finishing with `printf '\a'`, or a TUI
ringing on a bad key, is useless if nothing moves outside the text cells.
Alacritty / Windows Terminal flash (and often sound); before this, Foreman
dropped `Event::Bell` on the floor.

Supervision push (OS toast / sound when an agent needs you) is a **later** job
— chat already tracks that under human push-notifications. Do not fold it into
this feature.

## What you see

| Surface | Behavior |
|---------|----------|
| **Win border** | Breathes while **any** tab in that Win's stack has a latched Bell |
| **Tab chip** | Only the ringing Session's chip breathes |
| **Bare** lone tiled terminal (no chrome) | Thin breathing inset ring on the content rect |
| **Sessions panel — expanded rows / columns / strip** | Pulsing amber dot on the ringing terminal's row/chip (outranks the "min"/"tab" label slot) |
| **Sessions panel — collapsed rail** | Pulsing amber dot on the project icon when any child rings (the rail is the only surface for its rows) |
| **Minimized window** | No window chrome, but its panel row/rail dot still pulses |

- **Color:** caret amber family (`theme::BELL`), breathing between ~40% and
  full strength via `theme::bell_pulse(t, period, color)` — every surface
  breathes in sync because they all pass the same egui wall time. The period
  comes from `Settings::bell_period`, defaulting to `theme::BELL_PERIOD`.
- **Duration:** **sticky** — the latch holds until the ringing Session becomes
  the keyboard-focused terminal. There is no timeout.
- **Spam:** more BELs while latched just refresh the ring timestamp (no
  visual change — it is already on).
- **Cancel:** keyboard focus on the ringing Session clears it. Hover does not.
  A Session that rings **while focused** shows nothing — you are already
  looking at it (the clear runs after the frame's PTY pump, so there is no
  one-frame flicker).
- **Sound / OS toast:** not in scope.

## How to use

### Try it

In any terminal Session (note: **backtick**, not apostrophe — or use the
unambiguous `[char]7` form):

```powershell
Write-Host ([char]7)
# or
printf "\a"          # sh/bash
```

Ring an unfocused pane and its border/chip/panel row breathes amber until you
click into it.

### Turn it off

Master switch in `%APPDATA%\foreman\settings.json`:

```json
{
  "bell": false
}
```

- **Default:** `true` (missing key = on, via `#[serde(default)]`).
- **Scope:** mutes **all** Bell attention — window chrome and panel; later
  sound/push must honor the same key.
- **UI:** file only — no settings checkbox, no leader mute chord.

## How it works

1. alacritty emits `Event::Bell` when BEL is parsed; the Session's `Listener`
   latches `Some(ring_time)` on a shared slot (sibling of title/color handling
   — never the `PtyWrite` flush path that latches Ready).
2. `Session::show` clears the latch **after** its pump whenever the Session is
   the keyboard-focused terminal — attended sessions never show (and never
   flicker) the pulse.
3. Each frame, gated by `terminal::bell_enabled(ctx)` (published from
   `Settings.bell` by App):
   - `wm` paints the border (any tab), the ringing chip(s), and the bare-pane
     inset ring in `bell_pulse(time)`;
   - `panel_model()` carries `bell` per tab row and per project
     (`any tab`); `panel.rs` paints the row/rail/strip dots.
4. Every Bell paint site requests a 30 ms repaint while active, driving the
   breathe animation past the idle repaint cadence (the panel drives its own,
   since it can be the only visible surface — e.g. a minimized window).

Multiple Sessions can ring at once.

## Out of scope

- OSC 0/2 live tab titles (separate from Bell)
- Sound, OS notifications, chat `@you` push
- In-app settings editor / leader toggle for `bell`

## Gotchas

- **Not focus.** Focus chrome stays the near-white focus ladder; Bell is amber.
  While ringing, amber outranks the focus color on that window's border.
- **Not a counter.** One latched state per Session — no unread-bell counts.
- **Ready latch.** Bell handling must never route through the
  `Event::PtyWrite` → Ready flush path. It is a sibling arm in
  `Listener::send_event`.
- **Bare Wins** have no titlebar border — the content-rect inset ring is the
  required fallback; wm-border-only painting would leave them silent.
- **1 px strokes read dimmer than the token.** Antialiasing blends a 1 px
  amber stroke with the dark background to ~65% strength — expected, still
  clearly warm against the gray border ladder.
- Epic Phase 6 still lists title+bell together; implement against **this** doc
  for Bell, not the epic's combined "done when" alone.

## Acceptance (v1 verified 2026-07-17; sticky/panel rework same day)

v1 (300 ms pulse) was verified by screenshot + border-pixel sampling against a
demo build (`FOREMAN_BELL_DEMO`, since reverted) with continuous `\a` loops —
exact amber on ringing borders/chips/bare ring, gray on quiet neighbors and
project frames, all-gray with `"bell": false`. The sticky + animated + panel
rework is covered by unit tests (`config::tests::bell_*`,
`terminal::tests::listener_bell_latches_until_cleared`,
`theme::tests::bell_pulse_breathes_within_the_bell_color`,
`wm::tests::bell_latches_the_stack_until_cleared_and_reaches_the_panel`) and
interactive user verification.

- [x] Ringing an unfocused Session breathes its border (and chip if stacked)
- [x] Background tab rings → that chip + the border; other chips stay quiet
- [x] Bare lone tile shows the breathing inset ring
- [x] Latch is sticky: no self-expiry; re-rings refresh, never unlatch
- [x] Focusing the ringing Session clears it; ringing while focused shows nothing
- [x] Panel: ringing terminal's row dot pulses; project rail icon dot pulses;
      minimized windows still surface through the panel
- [x] `"bell": false` mutes chrome and panel; default / missing key → on
- [x] Existing Ready / DSR / title / color-request paths still green (full `cargo test`)

## Key files

| File | Role |
|------|------|
| `src/terminal.rs` | `Event::Bell` arm latches the Session's `bell` slot; `bell_active`/`clear_bell`; focused-clear after pump in `show`; `bell_enabled` ctx gate |
| `src/wm.rs` | `Win::bell_active` (border rule) + `Content::bell_active` (chip/row rule); border/chip/bare-ring paint; `panel_model()` bell flags; 30 ms repaint |
| `src/panel.rs` | `TabEntry.bell` / `ProjectEntry.bell`; pulsing dots on rows, rails, strip chips; panel-driven repaint |
| `src/main.rs` | publishes `settings.bell` into the egui ctx each frame (beside `set_font_size`) |
| `src/config.rs` | `Settings.bell: bool` (default `true`), `Settings.bell_period` |
| `src/theme.rs` | `BELL`, `BELL_PERIOD`, `bell_pulse` — the shared breathe animation |

## Related

- `docs/epics/terminal-completeness-epic.md` — Phase 6 (title half still open)
- `docs/chat-missing-features.md` §7 — human push-notifications (later, not Bell)
- `docs/cursor-rendering.md` — caret color source of truth for the pulse hue
- `docs/window-chrome.md` — borders, bare rule, tab chips
- `docs/task-manager-panel.md` — the panel the row/rail dots live in
