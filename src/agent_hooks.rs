//! Managed global UserPromptSubmit hooks for Claude, Codex, and Grok.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

const GROK_FILE: &str = "foreman-session-naming.json";
const POWERSHELL_RELAY_PREFIX: &str =
    "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ";
static NEXT_TMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Default)]
pub struct InstallReport {
    pub changed: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug)]
struct HookRoots {
    claude: PathBuf,
    codex: PathBuf,
    grok: PathBuf,
}

/// Install/update the guarded hooks off the GUI thread. Provider availability
/// and login are deliberately not probed here: that would either spend a turn
/// or duplicate each CLI's own authentication behavior.
pub fn spawn_install(ctx: eframe::egui::Context) -> mpsc::Receiver<InstallReport> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let report = install();
        let _ = tx.send(report);
        ctx.request_repaint();
    });
    rx
}

fn install() -> InstallReport {
    let Some(roots) = roots_from_env() else {
        return InstallReport {
            changed: 0,
            errors: vec!["user home is unavailable; agent hooks were not installed".into()],
        };
    };
    install_in(&roots)
}

fn roots_from_env() -> Option<HookRoots> {
    roots_from(|name| std::env::var_os(name))
}

fn roots_from(getenv: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<HookRoots> {
    let path = |name| {
        getenv(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let home = path("USERPROFILE").or_else(|| path("HOME"))?;
    Some(HookRoots {
        claude: path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
        codex: path("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
        grok: path("GROK_HOME").unwrap_or_else(|| home.join(".grok")),
    })
}

fn install_in(roots: &HookRoots) -> InstallReport {
    let targets = [
        (roots.claude.join("settings.json"), "claude"),
        (roots.codex.join("hooks.json"), "codex"),
        (roots.grok.join("hooks").join(GROK_FILE), "grok"),
    ];
    let mut report = InstallReport::default();
    for (path, agent) in targets {
        match install_one(&path, agent) {
            Ok(true) => report.changed += 1,
            Ok(false) => {}
            Err(error) => report.errors.push(format!("{agent} hook: {error}")),
        }
    }
    report
}

fn install_one(path: &Path, agent: &str) -> Result<bool, String> {
    let original = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    let root = match &original {
        Some(bytes) => serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|error| format!("{} is malformed JSON: {error}", path.display()))?,
        None => serde_json::json!({}),
    };
    if !root.is_object() {
        return Err(format!("{} must contain a JSON object", path.display()));
    }
    let merged = merge_hook(root, agent)?;
    let mut desired = serde_json::to_vec_pretty(&merged)
        .map_err(|error| format!("could not serialize hook: {error}"))?;
    desired.push(b'\n');
    if original.as_deref() == Some(desired.as_slice()) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    if let Some(bytes) = &original {
        let backup = backup_path(path);
        if !backup.exists() {
            std::fs::write(&backup, bytes)
                .map_err(|error| format!("could not create {}: {error}", backup.display()))?;
        }
    }
    atomic_write(path, &desired)?;
    Ok(true)
}

fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("hooks.json");
    path.with_file_name(format!("{name}.pre-foreman.bak"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let nonce = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(
        ".{}.foreman-{}-{nonce}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("hooks"),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes)
        .map_err(|error| format!("could not write {}: {error}", tmp.display()))?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not replace {}: {error}", path.display()));
    }
    Ok(())
}

fn managed_command(agent: &str) -> String {
    if cfg!(windows) {
        match agent {
            // Pin Claude to PowerShell below, which is present on every Windows
            // version Foreman supports and handles paths with spaces.
            "claude" => format!(
                "try {{ if ($env:FOREMAN_EXE) {{ & $env:FOREMAN_EXE title-event --agent {agent} *> $null }} }} catch {{}}; exit 0"
            ),
            // Grok's Windows runner and Codex's selected session shell are not
            // guaranteed to be cmd.exe. An encoded PowerShell command is one
            // shell-neutral argv shape: neither cmd nor PowerShell reparses
            // the relay script, and the native helper inherits hook stdin.
            "grok" => windows_powershell_relay(agent),
            // Codex keeps this portable value for shared home directories and
            // uses the commandWindows override installed below.
            _ => unix_managed_command(agent),
        }
    } else {
        unix_managed_command(agent)
    }
}

fn unix_managed_command(agent: &str) -> String {
    format!(
        "if [ -n \"${{FOREMAN_EXE:-}}\" ]; then \"$FOREMAN_EXE\" title-event --agent {agent} >/dev/null 2>&1 || true; fi"
    )
}

#[cfg(windows)]
fn windows_powershell_relay(agent: &str) -> String {
    use base64::Engine as _;

    let script = format!(
        "try {{ if ($env:FOREMAN_EXE) {{ & $env:FOREMAN_EXE title-event --agent {agent} *> $null }} }} catch {{}}; exit 0"
    );
    let utf16le = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16le);
    format!("{POWERSHELL_RELAY_PREFIX}{encoded}")
}

fn is_managed_handler(value: &serde_json::Value) -> bool {
    ["command", "commandWindows", "command_windows"]
        .into_iter()
        .filter_map(|field| value.get(field).and_then(serde_json::Value::as_str))
        .any(is_managed_command)
}

fn is_managed_command(command: &str) -> bool {
    if command.contains("FOREMAN_EXE") && command.contains("title-event --agent ") {
        return true;
    }
    let Some(encoded) = command.strip_prefix(POWERSHELL_RELAY_PREFIX) else {
        return false;
    };
    use base64::Engine as _;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    if bytes.len() % 2 != 0 {
        return false;
    }
    let utf16 = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&utf16).is_ok_and(|script| {
        script.contains("FOREMAN_EXE") && script.contains("title-event --agent ")
    })
}

fn merge_hook(mut root: serde_json::Value, agent: &str) -> Result<serde_json::Value, String> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| "the hook file must contain a JSON object".to_string())?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        return Err("the existing `hooks` value must be a JSON object".into());
    }
    let events = hooks
        .as_object_mut()
        .expect("just made an object")
        .entry("UserPromptSubmit")
        .or_insert_with(|| serde_json::json!([]));
    if !events.is_array() {
        return Err("the existing `hooks.UserPromptSubmit` value must be an array".into());
    }
    let groups = events.as_array_mut().expect("just made an array");
    groups.retain_mut(|group| {
        let Some(handlers) = group
            .get_mut("hooks")
            .and_then(serde_json::Value::as_array_mut)
        else {
            return true;
        };
        let before = handlers.len();
        handlers.retain(|handler| !is_managed_handler(handler));
        before == handlers.len() || !handlers.is_empty()
    });
    let mut handler = serde_json::json!({
            "type": "command",
            "command": managed_command(agent),
            "timeout": 1
    });
    // Both schemas support an explicit background command. Naming is passive,
    // so PowerShell/helper startup must never delay prompt submission. Grok's
    // UserPromptSubmit event is already non-blocking and has no documented
    // per-handler `async` field.
    if matches!(agent, "claude" | "codex") {
        handler
            .as_object_mut()
            .expect("handler literal is an object")
            .insert("async".into(), serde_json::json!(true));
    }
    if cfg!(windows) {
        let handler = handler
            .as_object_mut()
            .expect("handler literal is an object");
        if agent == "claude" {
            handler.insert("shell".into(), serde_json::json!("powershell"));
        } else if agent == "codex" {
            // `commandWindows` selects a platform string, not an execution
            // shell: Codex uses the Session's selected shell when one exists.
            // The encoded relay therefore has to work under both cmd.exe and
            // PowerShell.
            handler.insert(
                "commandWindows".into(),
                serde_json::json!(windows_powershell_relay(agent)),
            );
            handler.insert("timeout".into(), serde_json::json!(2));
        }
    }
    groups.push(serde_json::json!({"hooks": [handler]}));
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_unrelated_hooks_and_is_idempotent() {
        let original = serde_json::json!({
            "theme": "dark",
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{"type":"command", "command":"keep-me"}]
                }],
                "Stop": [{"hooks":[{"type":"command", "command":"also-keep"}]}]
            }
        });
        let once = merge_hook(original, "claude").unwrap();
        let twice = merge_hook(once.clone(), "claude").unwrap();
        assert_eq!(once, twice);
        assert_eq!(once["theme"], "dark");
        let text = serde_json::to_string(&once).unwrap();
        assert!(text.contains("keep-me"));
        assert!(text.contains("also-keep"));
        assert_eq!(text.matches("title-event --agent claude").count(), 1);
        assert_eq!(
            once["hooks"]["UserPromptSubmit"][1]["hooks"][0]["async"], true,
            "the passive naming hook must never delay prompt submission"
        );
    }

    #[test]
    fn empty_home_overrides_fall_back_instead_of_becoming_relative_paths() {
        let roots = roots_from(|name| match name {
            "USERPROFILE" => Some(std::ffi::OsString::from(r"C:\Users\tester")),
            "CLAUDE_CONFIG_DIR" | "CODEX_HOME" | "GROK_HOME" => Some(std::ffi::OsString::new()),
            _ => None,
        })
        .expect("non-empty user home");

        assert_eq!(roots.claude, PathBuf::from(r"C:\Users\tester\.claude"));
        assert_eq!(roots.codex, PathBuf::from(r"C:\Users\tester\.codex"));
        assert_eq!(roots.grok, PathBuf::from(r"C:\Users\tester\.grok"));
    }

    #[test]
    fn merge_refuses_semantically_invalid_hook_containers() {
        assert!(merge_hook(serde_json::json!([]), "codex").is_err());
        assert!(merge_hook(serde_json::json!({"hooks": []}), "codex").is_err());
        assert!(
            merge_hook(
                serde_json::json!({"hooks": {"UserPromptSubmit": {}}}),
                "codex"
            )
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn managed_commands_match_each_windows_hook_shell() {
        assert_eq!(
            managed_command("codex"),
            "if [ -n \"${FOREMAN_EXE:-}\" ]; then \"$FOREMAN_EXE\" title-event --agent codex >/dev/null 2>&1 || true; fi"
        );
        assert_eq!(
            managed_command("claude"),
            "try { if ($env:FOREMAN_EXE) { & $env:FOREMAN_EXE title-event --agent claude *> $null } } catch {}; exit 0"
        );
        assert!(
            managed_command("grok")
                .starts_with("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ")
        );
        let merged = merge_hook(serde_json::json!({}), "claude").unwrap();
        assert_eq!(
            merged["hooks"]["UserPromptSubmit"][0]["hooks"][0]["shell"],
            "powershell"
        );
        let merged = merge_hook(serde_json::json!({}), "codex").unwrap();
        assert_eq!(
            merged["hooks"]["UserPromptSubmit"][0]["hooks"][0]["commandWindows"],
            windows_powershell_relay("codex")
        );
        assert_eq!(
            merged["hooks"]["UserPromptSubmit"][0]["hooks"][0]["async"],
            true
        );
        assert_eq!(
            merged["hooks"]["UserPromptSubmit"][0]["hooks"][0]["timeout"],
            2
        );
    }

    #[cfg(windows)]
    #[test]
    fn codex_windows_relay_is_shell_neutral_for_absent_and_failing_helpers() {
        use std::io::Write as _;
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;

        let merged = merge_hook(serde_json::json!({}), "codex").unwrap();
        let command = merged["hooks"]["UserPromptSubmit"][0]["hooks"][0]["commandWindows"]
            .as_str()
            .unwrap();
        let status = std::process::Command::new("cmd.exe")
            // Match Codex's Windows command runner exactly. It passes `/C`
            // followed by one raw, outer-quoted command line; adding `/S`
            // changes cmd.exe's quote-stripping rules and masks integration
            // failures.
            .arg("/C")
            .raw_arg(format!(r#""{command}""#))
            .env_remove("FOREMAN_EXE")
            .status()
            .unwrap();
        assert!(status.success());

        let temp = tempfile::tempdir().unwrap();
        let helper_dir = temp.path().join("helper with spaces");
        std::fs::create_dir(&helper_dir).unwrap();
        let helper = helper_dir.join("foreman.cmd");
        std::fs::write(&helper, "@more > \"%~dp0payload.json\"\r\n@exit /b 7\r\n").unwrap();
        let payload_path = helper_dir.join("payload.json");
        let payload = br#"{"session_id":"probe","prompt":"review settings"}"#;

        let mut child = std::process::Command::new("cmd.exe")
            .arg("/C")
            .raw_arg(format!(r#""{command}""#))
            .env("FOREMAN_EXE", &helper)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(payload).unwrap();
        assert!(child.wait().unwrap().success());
        assert!(std::fs::read(&payload_path).unwrap().starts_with(payload));

        std::fs::remove_file(&payload_path).unwrap();
        let mut child = std::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(command)
            .env("FOREMAN_EXE", &helper)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(payload).unwrap();
        assert!(child.wait().unwrap().success());
        assert!(std::fs::read(&payload_path).unwrap().starts_with(payload));
    }

    #[test]
    fn installer_creates_backups_once_and_refuses_malformed_json() {
        let temp = tempfile::tempdir().unwrap();
        let roots = HookRoots {
            claude: temp.path().join("claude"),
            codex: temp.path().join("codex"),
            grok: temp.path().join("grok"),
        };
        std::fs::create_dir_all(&roots.claude).unwrap();
        let claude = roots.claude.join("settings.json");
        std::fs::write(&claude, br#"{"keep":true}"#).unwrap();

        let first = install_in(&roots);
        assert_eq!(first.changed, 3);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(
            std::fs::read(backup_path(&claude)).unwrap(),
            br#"{"keep":true}"#
        );
        let installed = std::fs::read(&claude).unwrap();
        let second = install_in(&roots);
        assert_eq!(second.changed, 0);
        assert_eq!(std::fs::read(&claude).unwrap(), installed);

        let codex = roots.codex.join("hooks.json");
        std::fs::write(&codex, b"not-json").unwrap();
        let report = install_in(&roots);
        assert_eq!(std::fs::read(&codex).unwrap(), b"not-json");
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("malformed JSON"))
        );
    }
}
