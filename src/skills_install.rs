//! Installs foreman's bundled agent skills into the user's Claude config dir
//! so any project's `claude` session can discover them. Best-effort and
//! idempotent: called once at startup.
//!
//! `OBSOLETE_SKILLS` (below) is the rename/removal hook — when a shipped skill
//! is ever renamed or dropped, add its OLD directory name there so stale copies
//! are deleted from every machine on next launch.

const MANAGED_NOTICE: &str = "<!-- managed by foreman; edits are overwritten on launch -->";

/// The exact bytes foreman wants on disk for a skill: the embedded body with
/// trailing whitespace trimmed, then the managed-by notice on its own line.
/// Deterministic so the on-disk byte-compare in `write_skill_if_changed` is stable.
#[allow(dead_code)]
fn rendered_content(raw: &str) -> String {
    format!("{}\n{}\n", raw.trim_end(), MANAGED_NOTICE)
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
}
