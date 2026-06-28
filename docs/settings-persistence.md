# Settings persistence (`config.rs`)

The one place to store small app settings on disk. Use it for any new persisted
setting instead of hand-rolling file I/O again.

## What it does

`src/config.rs` gives you three reusable functions and one settings struct:

- `config_dir()` → `%APPDATA%\foreman` (created if missing). `None` only if
  `APPDATA` is unset.
- `load_json::<T>(file)` → reads a JSON file from that dir. Missing file, bad
  file, or invalid JSON all fall back to `T::default()` (with a stderr warning
  for the bad cases). **Never panics** — a corrupt config can't crash the app.
- `save_json(file, &value)` → writes JSON **atomically** (write a `.tmp`, then
  rename over the real file). A crash mid-write leaves the old good file intact.
- `Settings` → the actual app-settings struct, saved to
  `%APPDATA%\foreman\settings.json`. Today it holds `font_size`.

## Why it exists

Before this, the only persisted thing (`keybindings.json`) hand-rolled the whole
"resolve APPDATA → create dir → serde → fall back on error" dance inline in
`keymap.rs`, and the chat-log plan was about to do it a third time. This is the
shared, **production** version of that pattern (atomic write added), so the next
setting is a one-line field, not a new copy of the plumbing.

## How to use it

Add a persisted setting = add a field to `Settings`:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]              // <- the important part
pub struct Settings {
    pub font_size: f32,
    pub my_new_thing: bool,    // add a field...
}
impl Default for Settings {    // ...and its default
    fn default() -> Self { Self { font_size: 13.0, my_new_thing: false } }
}
```

Read with `Settings::load()`, write with `settings.save()`. That's it.

## Gotchas

- **`#[serde(default)]` on the struct is load-bearing.** It means an old file
  missing your new field, OR a file written by a *newer* foreman with extra
  fields, both still load. Drop it and adding a field breaks every existing file.
- **Don't save on a hot path.** `save_json` touches disk. The font-zoom caller
  debounces (writes once ~400ms after the last change), not once per scroll
  notch. Do the same for anything that changes rapidly.
- **This is for settings, not logs.** A growing append-only log (e.g. the chat
  history in `docs/chat-persistence.md`) is a different problem — JSONL, one line
  per event — and intentionally does not go through here.
- `keybindings.json` still uses its own older code (it has bespoke merge-over-
  defaults semantics). Fine to leave; migrate it onto `config_dir()` /
  `save_json` opportunistically if you touch it.

## Key files

- `src/config.rs` — the whole thing: `config_dir`, `load_json`, `save_json`,
  `Settings`, and the font-size constants (`DEFAULT/MIN/MAX_FONT_SIZE`,
  `FONT_ZOOM_STEP`).
- `src/main.rs` — `App` owns a `Settings`, loads it at startup, and saves it
  (debounced) when the live font size changes.
- `src/keymap.rs` — the older hand-rolled precedent this generalizes.
