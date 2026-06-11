# Foreman Global Skill Install — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make foreman embed `foreman-dispatch`/`foreman-chat` and install them into the user's Claude config dir on startup, so an agent in any project (including an external repo) can discover them.

**Architecture:** A new `src/skills_install.rs` module embeds each `SKILL.md` via `include_str!`, resolves the Claude config dir (`CLAUDE_CONFIG_DIR` else `%USERPROFILE%\.claude`), and on startup writes each skill into `<config>\skills\<name>\SKILL.md` only when missing or byte-different (no marker file), using atomic temp-file + rename. `main()` calls it once, best-effort — failures are logged and never block launch.

**Tech Stack:** Rust, `std::fs`/`std::env` only (no new crates — matches the repo's `keymap.rs` no-`dirs`-dep convention).

**Spec:** `docs/superpowers/specs/2026-06-11-foreman-skills-global-install-design.md`

---

## Build / test commands (Windows, PowerShell, GNU toolchain)

Kill the running app first or the link fails with `Access is denied (os error 5)`:

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo test skills_install 2>&1 | Select-Object -Last 30
```

Each task introduces private functions that are not yet called from non-test code; transient `dead_code` warnings are expected and are fully resolved in Task 6 when `install()` is wired into `main()`. Add `#[allow(dead_code)]` on each function as introduced and remove those attributes in Task 6.

---

## File structure

- **Create** `src/skills_install.rs` — the whole feature: embeds, config-dir resolution, per-skill atomic write, obsolete cleanup, the best-effort `install()` entry, and an inline `#[cfg(test)] mod tests` (mirrors `keymap.rs`, which keeps its tests inline).
- **Modify** `src/main.rs` — add `mod skills_install;` and one `skills_install::install();` call in `main()`.
- **Modify** `CLAUDE.md` — one line under Architecture noting skills auto-install on startup.

---

### Task 1: Module skeleton + `rendered_content`

**Files:**
- Create: `src/skills_install.rs`
- Modify: `src/main.rs:1-7` (module declarations block)
- Test: inline in `src/skills_install.rs`

- [ ] **Step 1: Declare the module** in `src/main.rs`. The `mod` list is alphabetized; insert `skills_install` after `settings`:

```rust
mod chat;
mod control;
mod dirpicker;
mod keymap;
mod settings;
mod skills_install;
mod terminal;
mod wm;
```

- [ ] **Step 2: Write the failing test.** Create `src/skills_install.rs` with only the doc comment, the notice constant, and the test module:

```rust
//! Installs foreman's bundled agent skills into the user's Claude config dir
//! so any project's `claude` session can discover them. Best-effort and
//! idempotent: called once at startup.
//!
//! `OBSOLETE_SKILLS` (below) is the rename/removal hook — when a shipped skill
//! is ever renamed or dropped, add its OLD directory name there so stale copies
//! are deleted from every machine on next launch.

const MANAGED_NOTICE: &str = "<!-- managed by foreman; edits are overwritten on launch -->";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_appends_managed_notice_once() {
        let out = rendered_content("# Skill\n\nbody\n\n");
        assert!(out.ends_with(&format!("{MANAGED_NOTICE}\n")));
        assert_eq!(out.matches(MANAGED_NOTICE).count(), 1);
        assert!(out.starts_with("# Skill\n\nbody\n"));
        assert!(!out.contains("body\n\n<!-- managed"), "no double trailing blank line");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: FAIL — compile error `cannot find function 'rendered_content' in this scope`.

- [ ] **Step 4: Implement `rendered_content`.** Add above the test module:

```rust
/// The exact bytes foreman wants on disk for a skill: the embedded body with
/// trailing whitespace trimmed, then the managed-by notice on its own line.
/// Deterministic so the on-disk byte-compare in `write_skill_if_changed` is stable.
#[allow(dead_code)]
fn rendered_content(raw: &str) -> String {
    format!("{}\n{}\n", raw.trim_end(), MANAGED_NOTICE)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: PASS (`rendered_appends_managed_notice_once`).

- [ ] **Step 6: Commit**

```powershell
git add src/skills_install.rs src/main.rs
git commit -m "feat(skills): module skeleton + rendered_content"
```

---

### Task 2: `resolve_skills_dir` (pure config-dir resolution)

**Files:**
- Modify: `src/skills_install.rs`
- Test: inline

Resolution is a pure function over the two env values (passed in) so tests never race on process-global env vars — the same reason `keymap.rs` notes it "can't easily point load() at a temp path".

- [ ] **Step 1: Write the failing tests.** Add to `mod tests`:

```rust
    use std::path::PathBuf;

    #[test]
    fn resolve_prefers_claude_config_dir() {
        let d = resolve_skills_dir(Some("C:/cfg"), Some("C:/Users/x")).unwrap();
        assert_eq!(d, PathBuf::from("C:/cfg").join("skills"));
    }

    #[test]
    fn resolve_empty_config_falls_back_to_userprofile() {
        let d = resolve_skills_dir(Some("   "), Some("C:/Users/x")).unwrap();
        assert_eq!(d, PathBuf::from("C:/Users/x").join(".claude").join("skills"));
    }

    #[test]
    fn resolve_none_when_nothing_usable() {
        assert!(resolve_skills_dir(None, None).is_none());
        assert!(resolve_skills_dir(Some(""), None).is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: FAIL — `cannot find function 'resolve_skills_dir'`.

- [ ] **Step 3: Implement.** Add near the top (after `rendered_content`):

```rust
use std::path::PathBuf;

/// Resolve `<claude-config>/skills`. Prefers a non-empty `CLAUDE_CONFIG_DIR`
/// (matching Claude Code's own precedence); otherwise `<userprofile>/.claude`.
/// Returns `None` only when neither is usable.
#[allow(dead_code)]
fn resolve_skills_dir(claude_config: Option<&str>, userprofile: Option<&str>) -> Option<PathBuf> {
    let base = match claude_config {
        Some(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => PathBuf::from(userprofile?).join(".claude"),
    };
    Some(base.join("skills"))
}
```

(Delete the `use std::path::PathBuf;` line inside `mod tests` if it now conflicts — keep just one import. If both are flagged, leave the test-module one and remove the top-level `use`, referring to it as `PathBuf` works either way since `super::*` re-exports it.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: PASS (3 resolve tests).

- [ ] **Step 5: Commit**

```powershell
git add src/skills_install.rs
git commit -m "feat(skills): pure config-dir resolution"
```

---

### Task 3: `write_skill_if_changed` (atomic, byte-compare)

**Files:**
- Modify: `src/skills_install.rs`
- Test: inline

- [ ] **Step 1: Write the failing tests.** Add a temp-dir helper and tests to `mod tests`:

```rust
    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("foreman-skills-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_when_missing_then_skips_when_current() {
        let dir = temp("write-skip");
        let raw = "# Hi\nbody";
        assert!(write_skill_if_changed(&dir, "foreman-dispatch", raw).unwrap());
        let file = dir.join("foreman-dispatch").join("SKILL.md");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), rendered_content(raw));
        // up-to-date second run does not rewrite
        assert!(!write_skill_if_changed(&dir, "foreman-dispatch", raw).unwrap());
        // no leftover temp file
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("foreman-dispatch"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp: {leftovers:?}");
    }

    #[test]
    fn rewrites_when_content_differs() {
        let dir = temp("rewrite");
        write_skill_if_changed(&dir, "foreman-chat", "old").unwrap();
        assert!(write_skill_if_changed(&dir, "foreman-chat", "new").unwrap());
        let file = dir.join("foreman-chat").join("SKILL.md");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), rendered_content("new"));
    }

    #[test]
    fn recreates_when_deleted() {
        let dir = temp("recreate");
        write_skill_if_changed(&dir, "foreman-chat", "x").unwrap();
        std::fs::remove_dir_all(dir.join("foreman-chat")).unwrap();
        assert!(write_skill_if_changed(&dir, "foreman-chat", "x").unwrap());
        assert!(dir.join("foreman-chat").join("SKILL.md").exists());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: FAIL — `cannot find function 'write_skill_if_changed'`.

- [ ] **Step 3: Implement.** Add `use std::io;` and `use std::path::Path;` near the top imports, then:

```rust
/// Write `<skills_dir>/<name>/SKILL.md` iff it is missing or its bytes differ
/// from `rendered_content(raw)`. Returns `true` when it wrote. The write is
/// atomic: a temp file in the same directory is renamed over the target, so a
/// `claude` session scanning the dir never sees a half-written skill.
#[allow(dead_code)]
fn write_skill_if_changed(skills_dir: &Path, name: &str, raw: &str) -> io::Result<bool> {
    let want = rendered_content(raw);
    let dir = skills_dir.join(name);
    let file = dir.join("SKILL.md");
    if let Ok(existing) = std::fs::read_to_string(&file) {
        if existing == want {
            return Ok(false);
        }
    }
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("SKILL.md.tmp");
    std::fs::write(&tmp, want.as_bytes())?;
    std::fs::rename(&tmp, &file)?;
    Ok(true)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: PASS (3 new tests + earlier ones).

- [ ] **Step 5: Commit**

```powershell
git add src/skills_install.rs
git commit -m "feat(skills): atomic byte-compare skill writer"
```

---

### Task 4: `remove_obsolete` (rename/removal cleanup)

**Files:**
- Modify: `src/skills_install.rs`
- Test: inline

- [ ] **Step 1: Write the failing test.** Add to `mod tests`:

```rust
    #[test]
    fn remove_obsolete_deletes_named_leaves_others() {
        let dir = temp("obsolete");
        std::fs::create_dir_all(dir.join("old-skill")).unwrap();
        std::fs::create_dir_all(dir.join("keep")).unwrap();
        let removed = remove_obsolete(&dir, &["old-skill", "never-existed"]).unwrap();
        assert_eq!(removed, vec!["old-skill".to_string()]);
        assert!(!dir.join("old-skill").exists());
        assert!(dir.join("keep").exists());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: FAIL — `cannot find function 'remove_obsolete'`.

- [ ] **Step 3: Implement.** Add:

```rust
/// Delete any obsolete skill directories named in `names` from `skills_dir`.
/// Returns the names actually removed. Missing dirs are skipped silently.
#[allow(dead_code)]
fn remove_obsolete(skills_dir: &Path, names: &[&str]) -> io::Result<Vec<String>> {
    let mut removed = Vec::new();
    for &name in names {
        let dir = skills_dir.join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
            removed.push(name.to_string());
        }
    }
    Ok(removed)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/skills_install.rs
git commit -m "feat(skills): obsolete-skill cleanup hook"
```

---

### Task 5: Embeds + `install_into` orchestration

**Files:**
- Modify: `src/skills_install.rs`
- Test: inline

This is the task that first references the embedded skill files via `include_str!`, so a successful build here also proves the embed paths are correct.

- [ ] **Step 1: Write the failing test.** Add to `mod tests`:

```rust
    #[test]
    fn install_into_writes_both_then_is_idempotent() {
        let dir = temp("install-into");
        let first = install_into(&dir).unwrap();
        assert_eq!(first.written, vec!["foreman-dispatch", "foreman-chat"]);
        assert!(dir.join("foreman-dispatch").join("SKILL.md").exists());
        assert!(dir.join("foreman-chat").join("SKILL.md").exists());
        // second run: nothing changes
        let second = install_into(&dir).unwrap();
        assert!(second.written.is_empty(), "expected no rewrites, got {:?}", second.written);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: FAIL — `cannot find function 'install_into'` / `InstallReport`.

- [ ] **Step 3: Implement.** Add the embeds, table, obsolete list, report type, and orchestrator:

```rust
const DISPATCH_SKILL: &str = include_str!("../.claude/skills/foreman-dispatch/SKILL.md");
const CHAT_SKILL: &str = include_str!("../.claude/skills/foreman-chat/SKILL.md");

/// (directory name, embedded SKILL.md body). The directory name MUST match the
/// `name:` in each skill's frontmatter so Claude Code discovers it.
const SKILLS: &[(&str, &str)] = &[
    ("foreman-dispatch", DISPATCH_SKILL),
    ("foreman-chat", CHAT_SKILL),
];

/// Old skill directory names to delete on install (the rename/removal hook).
/// Empty until a shipped skill is renamed or dropped.
const OBSOLETE_SKILLS: &[&str] = &[];

#[derive(Debug, Default, PartialEq)]
pub struct InstallReport {
    pub written: Vec<&'static str>,
    pub removed: Vec<String>,
}

/// Ensure `skills_dir` exists, drop obsolete skills, then write each bundled
/// skill that is missing or stale.
#[allow(dead_code)]
fn install_into(skills_dir: &Path) -> io::Result<InstallReport> {
    std::fs::create_dir_all(skills_dir)?;
    let removed = remove_obsolete(skills_dir, OBSOLETE_SKILLS)?;
    let mut written = Vec::new();
    for &(name, raw) in SKILLS {
        if write_skill_if_changed(skills_dir, name, raw)? {
            written.push(name);
        }
    }
    Ok(InstallReport { written, removed })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test skills_install 2>&1 | Select-Object -Last 30`
Expected: PASS — `install_into_writes_both_then_is_idempotent` and all earlier tests.

- [ ] **Step 5: Commit**

```powershell
git add src/skills_install.rs
git commit -m "feat(skills): embed skills + install_into orchestration"
```

---

### Task 6: Public `install()` + wire into `main()`

**Files:**
- Modify: `src/skills_install.rs`
- Modify: `src/main.rs:372`

- [ ] **Step 1: Implement `install()` and `config_skills_dir()`.** Add to `src/skills_install.rs`:

```rust
/// Resolve `<claude-config>/skills` from the live environment.
fn config_skills_dir() -> Option<PathBuf> {
    resolve_skills_dir(
        std::env::var("CLAUDE_CONFIG_DIR").ok().as_deref(),
        std::env::var("USERPROFILE").ok().as_deref(),
    )
}

/// Best-effort: install the bundled skills into the user's Claude config dir so
/// agents in any project can discover dispatch/chat. Never panics; every failure
/// is logged to stderr and the app continues — the env wiring works regardless.
pub fn install() {
    let Some(dir) = config_skills_dir() else {
        eprintln!("foreman: skill install skipped (no CLAUDE_CONFIG_DIR or USERPROFILE)");
        return;
    };
    match install_into(&dir) {
        Ok(r) if !r.written.is_empty() || !r.removed.is_empty() => {
            eprintln!(
                "foreman: skills updated in {} (wrote {:?}, removed {:?})",
                dir.display(),
                r.written,
                r.removed
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("foreman: skill install failed: {e}"),
    }
}
```

- [ ] **Step 2: Remove the now-unneeded `#[allow(dead_code)]` attributes.** Every function is now reachable from the `pub fn install()` chain. Delete the `#[allow(dead_code)]` line above `rendered_content`, `resolve_skills_dir`, `write_skill_if_changed`, `remove_obsolete`, and `install_into`.

- [ ] **Step 3: Call it from `main()`.** In `src/main.rs`, after `install_panic_logger();` (line 372), add the install call. Place it after the subcommand early-exit so the `foreman open` pipe client never triggers it:

```rust
    install_panic_logger();
    skills_install::install();
    let (tx, rx) = std::sync::mpsc::channel();
```

- [ ] **Step 4: Build (warnings must be clean) and test.**

Run:
```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test skills_install 2>&1 | Select-Object -Last 30
```
Expected: build succeeds with no `dead_code` warnings from `skills_install`; all tests PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/skills_install.rs src/main.rs
git commit -m "feat(skills): wire best-effort global install into startup"
```

---

### Task 7: End-to-end verification + doc note

**Files:**
- Modify: `CLAUDE.md` (Architecture section)

- [ ] **Step 1: Verify the real install path manually.** Run the built app once against a throwaway config dir so the real `~/.claude` is untouched, then confirm the files landed:

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
$env:CLAUDE_CONFIG_DIR = "$env:TEMP\foreman-skillcheck"
Remove-Item -Recurse -Force $env:CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue
Start-Process -FilePath ".\target\debug\foreman.exe"
Start-Sleep -Seconds 3
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
Get-ChildItem -Recurse "$env:CLAUDE_CONFIG_DIR\skills" | Select-Object FullName
```
Expected: `foreman-dispatch\SKILL.md` and `foreman-chat\SKILL.md` exist under the temp config dir; no `.tmp` files remain.

- [ ] **Step 2: Clean up the throwaway dir and unset the override:**

```powershell
Remove-Item -Recurse -Force $env:CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue
Remove-Item Env:\CLAUDE_CONFIG_DIR
```

- [ ] **Step 3: Add the doc note.** In `CLAUDE.md`, under the `## Architecture` bullet list, add after the `src/settings.rs` bullet:

```markdown
- `src/skills_install.rs` — on startup, embeds (`include_str!`) and installs the
  `foreman-dispatch`/`foreman-chat` skills into `<CLAUDE_CONFIG_DIR|~/.claude>\skills`
  so agents in any project (incl. external repos) can discover them. Source of
  truth stays `.claude/skills/`; edit a skill there + rebuild to propagate.
  Best-effort — failures are logged, never block launch.
```

- [ ] **Step 4: Commit**

```powershell
git add CLAUDE.md
git commit -m "docs: note startup skill auto-install"
```

---

## Self-review

**Spec coverage:**
- §1 embedded source of truth → Task 5 (`include_str!` consts).
- §2 target location / `CLAUDE_CONFIG_DIR` else `%USERPROFILE%`, empty-as-unset → Task 2 + Task 6 (`config_skills_dir`).
- §3 byte-compare, no marker → Task 3.
- §4 clobber-always + managed-notice line → Task 1 (`rendered_content`).
- §5 atomic temp+rename → Task 3.
- §6 `OBSOLETE_SKILLS` cleanup → Task 4 + Task 5.
- §7 best-effort, sync in `main()` before eframe → Task 6.
- Testing items 1-7 → Tasks 2-5 (config resolution, fresh/no-op/repair/missing, obsolete cleanup, no `.tmp` leftover, idempotent install).
- Touch list (module, main.rs, CLAUDE.md doc note) → Tasks 1, 6, 7.

**Placeholder scan:** No TBD/TODO; every code step shows complete code; every command has an expected result.

**Type/name consistency:** `rendered_content`, `resolve_skills_dir`, `write_skill_if_changed`, `remove_obsolete`, `install_into`, `config_skills_dir`, `install`, `InstallReport { written, removed }`, `SKILLS`, `OBSOLETE_SKILLS`, `MANAGED_NOTICE` are used identically across all tasks.
