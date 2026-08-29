//! Installs foreman's bundled agent skills into the user's Claude and Codex
//! config dirs so any project session can discover them. Best-effort and
//! idempotent: called once at startup.
//!
//! `OBSOLETE_SKILLS` (below) is the rename/removal hook — when a shipped skill
//! is ever renamed or dropped, add its OLD directory name there so stale copies
//! are deleted from every machine on next launch.

use std::io;
use std::path::{Path, PathBuf};

const MANAGED_NOTICE: &str = "<!-- managed by foreman; edits are overwritten on launch -->";
const YAML_MANAGED_NOTICE: &str = "# managed by foreman; edits are overwritten on launch";

#[derive(Clone, Copy)]
enum NoticeStyle {
    Markdown,
    Yaml,
}

/// The exact bytes foreman wants on disk for a skill: the embedded body with
/// trailing whitespace trimmed, then the managed-by notice on its own line.
/// Deterministic so the on-disk byte-compare in `write_skill_if_changed` is stable.
fn rendered_content(raw: &str) -> String {
    rendered_file_content(raw, NoticeStyle::Markdown)
}

fn rendered_file_content(raw: &str, notice: NoticeStyle) -> String {
    let notice = match notice {
        NoticeStyle::Markdown => MANAGED_NOTICE,
        NoticeStyle::Yaml => YAML_MANAGED_NOTICE,
    };
    format!("{}\n{}\n", raw.trim_end(), notice)
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

/// Resolve `<codex-home>/skills`. Prefers a non-empty `CODEX_HOME`;
/// otherwise `<userprofile>/.codex`. Returns `None` only when neither is usable.
fn resolve_codex_skills_dir(
    codex_home: Option<&str>,
    userprofile: Option<&str>,
) -> Option<PathBuf> {
    let base = match codex_home {
        Some(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => PathBuf::from(userprofile?).join(".codex"),
    };
    Some(base.join("skills"))
}

/// Write `<skills_dir>/<name>/SKILL.md` iff it is missing or its bytes differ
/// from `rendered_content(raw)`. Returns `true` when it wrote. The write is
/// atomic: a temp file in the same directory is renamed over the target, so a
/// session scanning the dir never sees a half-written skill.
fn write_skill_if_changed(skills_dir: &Path, name: &str, raw: &str) -> io::Result<bool> {
    write_managed_file_if_changed(
        &skills_dir.join(name),
        "SKILL.md",
        raw,
        NoticeStyle::Markdown,
    )
}

fn write_managed_file_if_changed(
    root: &Path,
    relative_path: &str,
    raw: &str,
    notice: NoticeStyle,
) -> io::Result<bool> {
    let want = match notice {
        NoticeStyle::Markdown => rendered_content(raw),
        NoticeStyle::Yaml => rendered_file_content(raw, notice),
    };
    let file = root.join(relative_path);
    if let Ok(existing) = std::fs::read_to_string(&file) {
        if existing == want {
            return Ok(false);
        }
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension(format!(
        "{}.tmp",
        file.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
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
const CODEX_DISPATCH_SKILL: &str = include_str!("../.codex/skills/foreman-dispatch/SKILL.md");
const CODEX_DISPATCH_OPENAI: &str =
    include_str!("../.codex/skills/foreman-dispatch/agents/openai.yaml");
const CODEX_CHAT_SKILL: &str = include_str!("../.codex/skills/foreman-chat/SKILL.md");
const CODEX_CHAT_OPENAI: &str = include_str!("../.codex/skills/foreman-chat/agents/openai.yaml");
const ICAT_SKILL: &str = include_str!("../.claude/skills/foreman-icat/SKILL.md");
const CODEX_ICAT_SKILL: &str = include_str!("../.codex/skills/foreman-icat/SKILL.md");
const CODEX_ICAT_OPENAI: &str = include_str!("../.codex/skills/foreman-icat/agents/openai.yaml");
const KANBAN_SKILL: &str = include_str!("../.claude/skills/foreman-kanban/SKILL.md");
const CODEX_KANBAN_SKILL: &str = include_str!("../.codex/skills/foreman-kanban/SKILL.md");
const CODEX_KANBAN_OPENAI: &str =
    include_str!("../.codex/skills/foreman-kanban/agents/openai.yaml");

#[derive(Clone, Copy)]
struct SkillBundle {
    name: &'static str,
    skill_md: &'static str,
    openai_yaml: Option<&'static str>,
}

/// (directory name, embedded SKILL.md body). The directory name MUST match the
/// `name:` in each skill's frontmatter so Claude Code discovers it.
const CLAUDE_SKILLS: &[SkillBundle] = &[
    SkillBundle {
        name: "foreman-dispatch",
        skill_md: DISPATCH_SKILL,
        openai_yaml: None,
    },
    SkillBundle {
        name: "foreman-chat",
        skill_md: CHAT_SKILL,
        openai_yaml: None,
    },
    SkillBundle {
        name: "foreman-icat",
        skill_md: ICAT_SKILL,
        openai_yaml: None,
    },
    SkillBundle {
        name: "foreman-kanban",
        skill_md: KANBAN_SKILL,
        openai_yaml: None,
    },
];

/// Codex installs also include UI metadata under `agents/openai.yaml`.
const CODEX_SKILLS: &[SkillBundle] = &[
    SkillBundle {
        name: "foreman-dispatch",
        skill_md: CODEX_DISPATCH_SKILL,
        openai_yaml: Some(CODEX_DISPATCH_OPENAI),
    },
    SkillBundle {
        name: "foreman-chat",
        skill_md: CODEX_CHAT_SKILL,
        openai_yaml: Some(CODEX_CHAT_OPENAI),
    },
    SkillBundle {
        name: "foreman-icat",
        skill_md: CODEX_ICAT_SKILL,
        openai_yaml: Some(CODEX_ICAT_OPENAI),
    },
    SkillBundle {
        name: "foreman-kanban",
        skill_md: CODEX_KANBAN_SKILL,
        openai_yaml: Some(CODEX_KANBAN_OPENAI),
    },
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
fn install_into(
    skills_dir: &Path,
    skills: &[SkillBundle],
    obsolete: &[&str],
) -> io::Result<InstallReport> {
    std::fs::create_dir_all(skills_dir)?;
    let removed = remove_obsolete(skills_dir, obsolete)?;
    let mut written = Vec::new();
    for skill in skills {
        if write_bundle_if_changed(skills_dir, skill)? {
            written.push(skill.name);
        }
    }
    Ok(InstallReport { written, removed })
}

fn write_bundle_if_changed(skills_dir: &Path, skill: &SkillBundle) -> io::Result<bool> {
    let root = skills_dir.join(skill.name);
    let mut wrote = write_skill_if_changed(skills_dir, skill.name, skill.skill_md)?;
    if let Some(raw) = skill.openai_yaml {
        wrote |=
            write_managed_file_if_changed(&root, "agents/openai.yaml", raw, NoticeStyle::Yaml)?;
    }
    Ok(wrote)
}

/// Resolve `<claude-config>/skills` from the live environment.
fn config_claude_skills_dir() -> Option<PathBuf> {
    resolve_skills_dir(
        std::env::var("CLAUDE_CONFIG_DIR").ok().as_deref(),
        std::env::var("USERPROFILE").ok().as_deref(),
    )
}

/// Resolve `<codex-home>/skills` from the live environment.
fn config_codex_skills_dir() -> Option<PathBuf> {
    resolve_codex_skills_dir(
        std::env::var("CODEX_HOME").ok().as_deref(),
        std::env::var("USERPROFILE").ok().as_deref(),
    )
}

fn install_target(label: &str, dir: Option<PathBuf>, skills: &[SkillBundle], obsolete: &[&str]) {
    let Some(dir) = dir else {
        eprintln!("foreman: {label} skill install skipped (no usable config dir)");
        return;
    };
    match install_into(&dir, skills, obsolete) {
        Ok(r) if !r.written.is_empty() || !r.removed.is_empty() => {
            eprintln!(
                "foreman: {label} skills updated in {} (wrote {:?}, removed {:?})",
                dir.display(),
                r.written,
                r.removed
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("foreman: {label} skill install failed: {e}"),
    }
}

/// Best-effort: install the bundled skills into the user's agent config dirs so
/// agents in any project can discover dispatch/chat. Never panics; every failure
/// is logged to stderr and the app continues — the env wiring works regardless.
pub fn install() {
    install_target(
        "Claude",
        config_claude_skills_dir(),
        CLAUDE_SKILLS,
        OBSOLETE_SKILLS,
    );
    install_target(
        "Codex",
        config_codex_skills_dir(),
        CODEX_SKILLS,
        OBSOLETE_SKILLS,
    );
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
    fn rendered_yaml_uses_yaml_comment_notice() {
        let out = rendered_file_content("interface:\n  display_name: \"X\"\n", NoticeStyle::Yaml);
        assert!(out.ends_with(&format!("{YAML_MANAGED_NOTICE}\n")));
        assert!(!out.contains(MANAGED_NOTICE));
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

    #[test]
    fn codex_resolve_prefers_codex_home() {
        let d = resolve_codex_skills_dir(Some("C:/codex"), Some("C:/Users/x")).unwrap();
        assert_eq!(d, PathBuf::from("C:/codex").join("skills"));
    }

    #[test]
    fn codex_resolve_empty_config_falls_back_to_userprofile() {
        let d = resolve_codex_skills_dir(Some("   "), Some("C:/Users/x")).unwrap();
        assert_eq!(d, PathBuf::from("C:/Users/x").join(".codex").join("skills"));
    }

    #[test]
    fn codex_resolve_none_when_nothing_usable() {
        assert!(resolve_codex_skills_dir(None, None).is_none());
        assert!(resolve_codex_skills_dir(Some(""), None).is_none());
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
    fn install_into_writes_claude_both_then_is_idempotent() {
        let dir = temp("install-into");
        let first = install_into(&dir, CLAUDE_SKILLS, OBSOLETE_SKILLS).unwrap();
        assert_eq!(
            first.written,
            vec![
                "foreman-dispatch",
                "foreman-chat",
                "foreman-icat",
                "foreman-kanban"
            ]
        );
        assert!(dir.join("foreman-dispatch").join("SKILL.md").exists());
        assert!(dir.join("foreman-chat").join("SKILL.md").exists());
        assert!(dir.join("foreman-icat").join("SKILL.md").exists());
        assert!(dir.join("foreman-kanban").join("SKILL.md").exists());
        // second run: nothing changes
        let second = install_into(&dir, CLAUDE_SKILLS, OBSOLETE_SKILLS).unwrap();
        assert!(
            second.written.is_empty(),
            "expected no rewrites, got {:?}",
            second.written
        );
    }

    #[test]
    fn install_into_writes_codex_openai_yaml_then_is_idempotent() {
        let dir = temp("install-codex");
        let first = install_into(&dir, CODEX_SKILLS, OBSOLETE_SKILLS).unwrap();
        assert_eq!(
            first.written,
            vec![
                "foreman-dispatch",
                "foreman-chat",
                "foreman-icat",
                "foreman-kanban"
            ]
        );
        let yaml = dir
            .join("foreman-dispatch")
            .join("agents")
            .join("openai.yaml");
        assert!(yaml.exists());
        let content = std::fs::read_to_string(&yaml).unwrap();
        assert!(content.contains("default_prompt"));
        assert!(content.ends_with(&format!("{YAML_MANAGED_NOTICE}\n")));
        assert!(!content.contains(MANAGED_NOTICE));
        let second = install_into(&dir, CODEX_SKILLS, OBSOLETE_SKILLS).unwrap();
        assert!(
            second.written.is_empty(),
            "expected no rewrites, got {:?}",
            second.written
        );
    }
}
