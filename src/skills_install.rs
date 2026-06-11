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
fn rendered_content(raw: &str) -> String {
    format!("{}\n{}\n", raw.trim_end(), MANAGED_NOTICE)
}

/// Resolve `<claude-config>/skills`. Prefers a non-empty `CLAUDE_CONFIG_DIR`
/// (matching Claude Code's own precedence); otherwise `<userprofile>/.claude`.
/// Returns `None` only when neither is usable.
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

/// Delete any obsolete skill directories named in `names` from `skills_dir`.
/// Returns the names actually removed. Missing dirs are skipped silently.
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

    #[test]
    fn install_into_writes_both_then_is_idempotent() {
        let dir = temp("install-into");
        let first = install_into(&dir).unwrap();
        assert_eq!(first.written, vec!["foreman-dispatch", "foreman-chat"]);
        assert!(dir.join("foreman-dispatch").join("SKILL.md").exists());
        assert!(dir.join("foreman-chat").join("SKILL.md").exists());
        // second run: nothing changes
        let second = install_into(&dir).unwrap();
        assert!(
            second.written.is_empty(),
            "expected no rewrites, got {:?}",
            second.written
        );
    }
}
