# Font Fallback (CJK/Emoji Tofu) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When non-ASCII CJK or emoji appears on screen, egui draws real glyphs instead of empty boxes (tofu). ASCII-only panes look unchanged.

**Architecture:** Pure helper mutates a `FontDefinitions` value by appending named font blobs as **lowest-priority** fallbacks on Monospace and Proportional. Unit tests drive that helper with fake bytes (no real Windows fonts, no GUI). At `eframe::run_native` startup, best-effort `std::fs::read` of known Windows paths feeds the helper, then `ctx.set_fonts(...)`. No new crates. Grid/wide-char logic is already correct — this is display only.

**Tech Stack:** Rust, eframe/egui 0.34.3 (`FontDefinitions`, `FontData`, `FontFamily`), std only. Windows font paths hardcoded (app is Windows-first).

**Source of truth:** `docs/warp-feature-candidates.md` §8 + ranked shovel #1. Why: grid is wide-char-correct; renderer has no CJK glyphs. When you see tofu: Chinese paths, agent emoji. When you don't: plain ASCII.

## Global Constraints

- Windows, GNU toolchain. Before build/run if the app is open:
  `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500`
  (Skip kill when `FOREMAN=1` — do not kill host from inside a foreman terminal; use `cargo build --target-dir target/agent` instead.)
- Bin-only crate: `cargo test <filter>` — **not** `cargo test --lib`.
- **No new dependencies** (no font-kit, no font-loader).
- Fallbacks **push** onto family lists — never replace or prepend (keep Hack/default primary).
- Missing font file = skip that font, **do not panic**, do not block launch.
- egui 0.34 stores `font_data` as `BTreeMap<String, Arc<FontData>>` — insert with `Arc::new(FontData::from_owned(bytes))` (see epaint docs example).
- Color emoji is **out of scope** (epaint shapes only). Mono emoji shapes from Segoe UI Emoji are fine.
- Stage by name, never `git add -A`. Unrelated dirty files may exist (`src/wm.rs`, warp docs, etc.).
- Commits: `type(scope): subject`, body = why, trailer
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
  Prefer `git commit -F -` with a real heredoc; verify with `git log -1 --format=%B`.

## File map

| File | Role |
|------|------|
| `src/main.rs` | Pure `append_font_fallbacks` + `windows_fallback_font_paths` + `load_font_definitions`; call `set_fonts` in `run_native` closure; unit tests next to existing `app_icon_tests` |
| `docs/font-fallback.md` | Short feature doc (what/why/when/how/key files) after code ships |
| `docs/warp-feature-candidates.md` | Optional one-line "shipped" note on shovel #1 — only if still open when implementing |

---

### Task 1: Pure `append_font_fallbacks` + failing tests

**Files:**
- Modify: `src/main.rs` (helpers above `fn main`; tests in new or existing `#[cfg(test)]` module)

**Interfaces:**
- Consumes: nothing from later tasks.
- Produces:
  ```rust
  fn append_font_fallbacks(
      fonts: &mut egui::FontDefinitions,
      named_fonts: impl IntoIterator<Item = (String, Vec<u8>)>,
  )
  ```
  Mutates `fonts` in place: each non-empty blob is inserted under its name and **pushed** onto Monospace and Proportional family lists. Empty blobs skipped. Idempotent name push (do not double-push if name already in list).

- [ ] **Step 1: Write the failing tests**

Add near the bottom of `src/main.rs` (same file as `app_icon_tests`, can be the same `mod` or a sibling `mod font_fallback_tests`):

```rust
#[cfg(test)]
mod font_fallback_tests {
    use super::*;

    fn mono_names(fonts: &egui::FontDefinitions) -> Vec<String> {
        fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default()
    }

    fn prop_names(fonts: &egui::FontDefinitions) -> Vec<String> {
        fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn append_fallbacks_pushes_name_to_mono_and_proportional() {
        let mut fonts = egui::FontDefinitions::default();
        let before_mono = mono_names(&fonts);
        let before_prop = prop_names(&fonts);

        append_font_fallbacks(
            &mut fonts,
            [("yahei".into(), vec![0u8, 1, 2, 3])],
        );

        let after_mono = mono_names(&fonts);
        let after_prop = prop_names(&fonts);
        assert_eq!(&after_mono[..before_mono.len()], &before_mono[..]);
        assert_eq!(after_mono.last().map(String::as_str), Some("yahei"));
        assert_eq!(&after_prop[..before_prop.len()], &before_prop[..]);
        assert_eq!(after_prop.last().map(String::as_str), Some("yahei"));
        assert!(fonts.font_data.contains_key("yahei"));
    }

    #[test]
    fn append_fallbacks_preserves_primary_first() {
        // First entry stays primary — tofu fix must not replace Hack/etc.
        let mut fonts = egui::FontDefinitions::default();
        let primary = mono_names(&fonts)
            .first()
            .cloned()
            .expect("default mono family non-empty");

        append_font_fallbacks(
            &mut fonts,
            [("seguiemj".into(), vec![9u8, 9, 9])],
        );

        assert_eq!(mono_names(&fonts).first().map(String::as_str), Some(primary.as_str()));
        assert_eq!(mono_names(&fonts).last().map(String::as_str), Some("seguiemj"));
    }

    #[test]
    fn append_fallbacks_skips_empty_blob() {
        let mut fonts = egui::FontDefinitions::default();
        let before = mono_names(&fonts);

        append_font_fallbacks(&mut fonts, [("empty".into(), Vec::new())]);

        assert_eq!(mono_names(&fonts), before);
        assert!(!fonts.font_data.contains_key("empty"));
    }

    #[test]
    fn append_fallbacks_two_fonts_order_stable() {
        let mut fonts = egui::FontDefinitions::default();
        append_font_fallbacks(
            &mut fonts,
            [
                ("yahei".into(), vec![1u8]),
                ("seguiemj".into(), vec![2u8]),
            ],
        );
        let mono = mono_names(&fonts);
        let n = mono.len();
        assert!(n >= 2);
        assert_eq!(mono[n - 2], "yahei");
        assert_eq!(mono[n - 1], "seguiemj");
    }

    #[test]
    fn append_fallbacks_does_not_duplicate_name_in_family() {
        let mut fonts = egui::FontDefinitions::default();
        append_font_fallbacks(&mut fonts, [("yahei".into(), vec![1u8])]);
        append_font_fallbacks(&mut fonts, [("yahei".into(), vec![2u8])]);
        let count = mono_names(&fonts)
            .iter()
            .filter(|n| n.as_str() == "yahei")
            .count();
        assert_eq!(count, 1);
        // Second call may replace font_data bytes; name appears once in family.
        assert!(fonts.font_data.contains_key("yahei"));
    }
}
```

- [ ] **Step 2: Run tests — expect RED (compile fail)**

```powershell
Set-Location "H:/claude code/foreman"
cargo test append_fallbacks 2>&1 | Select-Object -Last 30
```

Expected: compile error — `cannot find function append_font_fallbacks`.

- [ ] **Step 3: Minimal implementation**

In `src/main.rs`, above `fn main` (near other free helpers like `load_app_icon`):

```rust
/// Append named font blobs as lowest-priority fallbacks for Monospace and
/// Proportional. Empty blobs are skipped. Existing primary fonts stay first.
/// Pure: no filesystem, no Context — unit-tested with fake bytes.
fn append_font_fallbacks(
    fonts: &mut egui::FontDefinitions,
    named_fonts: impl IntoIterator<Item = (String, Vec<u8>)>,
) {
    for (name, bytes) in named_fonts {
        if bytes.is_empty() {
            continue;
        }
        fonts.font_data.insert(
            name.clone(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [
            egui::FontFamily::Monospace,
            egui::FontFamily::Proportional,
        ] {
            if let Some(list) = fonts.families.get_mut(&family) {
                if !list.iter().any(|n| n == &name) {
                    list.push(name.clone());
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run tests — expect GREEN**

```powershell
cargo test append_fallbacks 2>&1 | Select-Object -Last 20
```

Expected: `5 passed` (or whatever count matches the tests above). No other suite required yet.

- [ ] **Step 5: Commit**

```powershell
git add src/main.rs
git commit -F - <<'EOF'
feat(ui): pure append_font_fallbacks for CJK/emoji coverage

Lowest-priority Monospace+Proportional fallbacks, tested with fake bytes
so CI never needs Windows font files. Wiring + disk load next.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
git log -1 --format=%B
```

(On PowerShell, if heredoc fails, use a temp msg file: `git commit -F msg.txt`.)

---

### Task 2: Path table + best-effort loader (pure-ish) + tests

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `append_font_fallbacks` from Task 1.
- Produces:
  ```rust
  fn windows_fallback_font_paths() -> &'static [(&'static str, &'static str)]
  // name, absolute path

  fn load_font_definitions(
      read: &dyn Fn(&std::path::Path) -> std::io::Result<Vec<u8>>,
  ) -> egui::FontDefinitions
  ```
  `load_font_definitions` starts from `FontDefinitions::default()`, reads each path via `read`, skips errors, calls `append_font_fallbacks`.

- [ ] **Step 1: Write the failing tests**

In `font_fallback_tests`:

```rust
    #[test]
    fn windows_fallback_paths_name_yahei_and_seguiemj() {
        let paths = windows_fallback_font_paths();
        let names: Vec<&str> = paths.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"yahei"));
        assert!(names.contains(&"seguiemj"));
        for (_, p) in paths {
            assert!(p.starts_with(r"C:\Windows\Fonts\"), "{p}");
        }
    }

    #[test]
    fn load_font_definitions_skips_missing_files() {
        let fonts = load_font_definitions(&|_| {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "nope"))
        });
        // Defaults still present; no panic; our names absent.
        assert!(!fonts.font_data.contains_key("yahei"));
        assert!(!fonts.font_data.contains_key("seguiemj"));
        assert!(
            fonts
                .families
                .get(&egui::FontFamily::Monospace)
                .map(|v| !v.is_empty())
                .unwrap_or(false)
        );
    }

    #[test]
    fn load_font_definitions_installs_readable_fonts() {
        let fonts = load_font_definitions(&|path| {
            let s = path.to_string_lossy();
            if s.contains("msyh") {
                Ok(vec![0xAA])
            } else if s.contains("seguiemj") {
                Ok(vec![0xBB])
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "x"))
            }
        });
        assert!(fonts.font_data.contains_key("yahei"));
        assert!(fonts.font_data.contains_key("seguiemj"));
        let mono = fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap();
        assert_eq!(mono.last().map(String::as_str), Some("seguiemj"));
        assert!(mono.iter().any(|n| n == "yahei"));
    }
```

- [ ] **Step 2: Run — RED**

```powershell
cargo test font_fallback 2>&1 | Select-Object -Last 30
```

Expected: compile error — missing `windows_fallback_font_paths` / `load_font_definitions`.

- [ ] **Step 3: Minimal implementation**

```rust
/// Known Windows system fonts used as glyph fallbacks (CJK + emoji shapes).
/// Order: CJK first, emoji second (both lowest priority after defaults).
fn windows_fallback_font_paths() -> &'static [(&'static str, &'static str)] {
    &[
        ("yahei", r"C:\Windows\Fonts\msyh.ttc"),
        ("seguiemj", r"C:\Windows\Fonts\seguiemj.ttf"),
    ]
}

/// Build default FontDefinitions plus any fallbacks `read` can supply.
/// Inject `read` so tests never touch the real disk.
fn load_font_definitions(
    read: &dyn Fn(&std::path::Path) -> std::io::Result<Vec<u8>>,
) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let mut loaded = Vec::new();
    for &(name, path) in windows_fallback_font_paths() {
        match read(std::path::Path::new(path)) {
            Ok(bytes) if !bytes.is_empty() => loaded.push((name.to_string(), bytes)),
            Ok(_) => {} // empty file — skip
            Err(_) => {} // missing / unreadable — skip
        }
    }
    append_font_fallbacks(&mut fonts, loaded);
    fonts
}
```

- [ ] **Step 4: Run — GREEN**

```powershell
cargo test font_fallback 2>&1 | Select-Object -Last 20
```

Expected: all `font_fallback` / `append_fallbacks` tests pass.

- [ ] **Step 5: Commit**

```text
feat(ui): load Windows font fallbacks via injectable reader

Paths for YaHei + Segoe UI Emoji; missing files skip cleanly. Ready to
wire into eframe startup with std::fs::read.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 3: Wire into `eframe::run_native` startup

**Files:**
- Modify: `src/main.rs` (`run_native` `Box::new(move |cc| { ... })` closure only)

**Interfaces:**
- Consumes: `load_font_definitions`.
- Produces: fonts installed once before first frame via `cc.egui_ctx.set_fonts(...)`.

- [ ] **Step 1: No new unit test required for the one-liner** — pure path is covered. Visual proof is Step 4.

- [ ] **Step 2: Implementation**

In the `eframe::run_native` creation closure, **before** spawning the control server / building `App`:

```rust
Box::new(move |cc| {
    cc.egui_ctx.set_fonts(load_font_definitions(&|p| std::fs::read(p)));

    // Spawn the control server here (not before run_native) ...
    let ctx = cc.egui_ctx.clone();
    std::thread::spawn(move || control::serve(control::PIPE, tx, ctx));
    Ok(Box::new(App::new(rx)))
}),
```

Keep the existing control-server comment. Do not move `set_fonts` after App paint starts.

- [ ] **Step 3: Compile**

```powershell
# Only if NOT inside FOREMAN=1:
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
cargo test font_fallback 2>&1 | Select-Object -Last 15
cargo build 2>&1 | Select-Object -Last 20
```

Expected: tests green; build ok.

- [ ] **Step 4: Manual visual check (required for display claims)**

1. Run `target\debug\foreman.exe` (or `cargo run`).
2. In a shell pane, produce non-ASCII, e.g.:
   - `echo 你好世界`
   - paste an emoji: `🚀`
   - or `cd` into a directory with CJK in the name if you have one.
3. **Before this change:** tofu boxes. **After:** readable glyphs (emoji may be monochrome — OK).
4. Optional: screenshot via build-screenshot skill / `win.png` and `Read` the PNG if claiming GUI proof in a later summary.

- [ ] **Step 5: Commit**

```text
feat(ui): install CJK/emoji font fallbacks at startup

set_fonts(load_font_definitions(fs::read)) once in the eframe create
callback so terminal and chrome share the same fallback chain.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 4: Feature doc + warp shovel note

**Files:**
- Create: `docs/font-fallback.md`
- Modify (light): `docs/warp-feature-candidates.md` — shovel item 1 line only if still listing it as unbuilt

- [ ] **Step 1: Write `docs/font-fallback.md`**

```markdown
# Font fallback (CJK / emoji)

## What it does

Loads Windows system fonts as **egui fallbacks** so CJK characters and emoji
draw as real glyphs instead of empty boxes (tofu). Does not change the
terminal grid or selection math — only which font file supplies the picture.

## Why

egui defaults (Hack + friends) cover Latin well and almost no CJK. Agent
output and paths with Chinese/Japanese/Korean or emoji looked broken even
though the cell model was correct.

## When you see the bug (without this)

- CJK in paths, `ls` / `git`, compiler errors, agent diffs
- Emoji in agent replies or CLIs

Plain ASCII-only panes: you may never notice.

## How it works

At GUI startup (`src/main.rs`), `load_font_definitions` reads:

| Name | Path | Role |
|------|------|------|
| yahei | `C:\Windows\Fonts\msyh.ttc` | CJK (Microsoft YaHei) |
| seguiemj | `C:\Windows\Fonts\seguiemj.ttf` | Emoji shapes (Segoe UI Emoji) |

Missing file → skip. Fonts are **appended** after defaults (primary mono
stays first). Color emoji layers are not supported in egui 0.34 — shapes only.

## Gotchas

- ~20MB+ YaHei loaded into RAM at start if present.
- `.ttc` face index is `0` (FontData default); if a machine's YaHei face is wrong, check index.
- Not a cross-platform discovery system — hardcoded Windows paths by design.

## Key files

- `src/main.rs` — `append_font_fallbacks`, `windows_fallback_font_paths`,
  `load_font_definitions`, `set_fonts` in `run_native` create callback
```

- [ ] **Step 2: Warp doc touch (only if needed)**

In `docs/warp-feature-candidates.md`, under **Do next (ranked)**, change item 1 to note shipped, e.g.:

`1. **Font fallback** — shipped (see docs/font-fallback.md). …`

Do not rewrite the whole file.

- [ ] **Step 3: Commit**

```text
docs(ui): font-fallback feature note

What/why/when for CJK-emoji tofu fix; key files for cold readers.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

## Done when

- [ ] All `font_fallback` / `append_fallbacks` unit tests green (watched RED then GREEN on Tasks 1–2).
- [ ] Startup wires `set_fonts` once.
- [ ] Manual non-ASCII check shows glyphs, not boxes.
- [ ] `docs/font-fallback.md` exists with Key files.
- [ ] No new Cargo deps; no panic on missing fonts.
- [ ] Unrelated dirty files not staged.

## Out of scope

- Color emoji
- font-kit / system font discovery
- Linux/macOS paths
- Replacing the primary monospace font
- Agent-state, `--tail`, READY_GRACE (other shovel items)

## Self-review (plan author)

| Spec / finding | Task |
|----------------|------|
| CJK + emoji tofu fix | 1–3 |
| Fallbacks only, primary first | Task 1 tests + impl |
| Best-effort missing files | Task 2 |
| No font-kit | Global constraints |
| ~30 lines main.rs | Tasks 1–3 |
| When you see it / ASCII OK | Task 4 doc |
| TDD pure seam | Tasks 1–2 before wire |
| Visual proof for GUI | Task 3 Step 4 |
