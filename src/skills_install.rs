//! Installs foreman's bundled agent skills into the user's Claude config dir
//! so any project's `claude` session can discover them. Best-effort and
//! idempotent: called once at startup.
//!
//! `OBSOLETE_SKILLS` (below) is the rename/removal hook — when a shipped skill
//! is ever renamed or dropped, add its OLD directory name there so stale copies
//! are deleted from every machine on next launch.

use std::path::PathBuf;

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
}
