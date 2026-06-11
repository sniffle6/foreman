//! Installs foreman's bundled agent skills into the user's Claude config dir
//! so any project's `claude` session can discover them. Best-effort and
//! idempotent: called once at startup.
//!
//! `OBSOLETE_SKILLS` (below) is the rename/removal hook — when a shipped skill
//! is ever renamed or dropped, add its OLD directory name there so stale copies
//! are deleted from every machine on next launch.

use std::io;
use std::path::{Path, PathBuf};

const MANAGED_NOTICE: &str = "<!-- managed by foreman; edits are overwritten on launch -->";

/// The exact bytes foreman wants on disk for a skill: the embedded body with
/// trailing whitespace trimmed, then the managed-by notice on its own line.
/// Deterministic so the on-disk byte-compare in `write_skill_if_changed` is stable.
#[allow(dead_code)]
fn rendered_content(raw: &str) -> String {
    format!("{}\n{}\n", raw.trim_end(), MANAGED_NOTICE)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_appends_managed_notice_once() {
        let out = rendered_content("# Skill\n\nbody\n\n");
        assert!(out.ends_with(&format!("{MANAGED_NOTICE}\n")));
        assert_eq!(out.matches(MANAGED_NOTICE).count(), 1);
        assert!(out.starts_with("# Skill\n\nbody\n"));
        assert!(
            !out.contains("body\n\n<!-- managed"),
            "no double trailing blank line"
        );
    }

    #[test]
    fn resolve_prefers_claude_config_dir() {
        let d = resolve_skills_dir(Some("C:/cfg"), Some("C:/Users/x")).unwrap();
        assert_eq!(d, PathBuf::from("C:/cfg").join("skills"));
    }

    #[test]
    fn resolve_empty_config_falls_back_to_userprofile() {
        let d = resolve_skills_dir(Some("   "), Some("C:/Users/x")).unwrap();
        assert_eq!(
            d,
            PathBuf::from("C:/Users/x").join(".claude").join("skills")
        );
    }

    #[test]
    fn resolve_none_when_nothing_usable() {
        assert!(resolve_skills_dir(None, None).is_none());
        assert!(resolve_skills_dir(Some(""), None).is_none());
    }

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
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            rendered_content(raw)
        );
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
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            rendered_content("new")
        );
    }

    #[test]
    fn recreates_when_deleted() {
        let dir = temp("recreate");
        write_skill_if_changed(&dir, "foreman-chat", "x").unwrap();
        std::fs::remove_dir_all(dir.join("foreman-chat")).unwrap();
        assert!(write_skill_if_changed(&dir, "foreman-chat", "x").unwrap());
        assert!(dir.join("foreman-chat").join("SKILL.md").exists());
    }
}
