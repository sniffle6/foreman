//! Managed global lifecycle hooks for Claude, Codex, and Grok: Session naming
//! and agent state share one hook command, installed on the events the
//! settings ask for.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

const GROK_FILE: &str = "foreman-session-naming.json";
const POWERSHELL_RELAY_PREFIX: &str =
    "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ";
static NEXT_TMP: AtomicU64 = AtomicU64::new(1);

/// Which features want hooks. The installed event set is derived from both,
/// so changing either setting reruns the installer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookWants {
    pub naming: bool,
    pub state: bool,
}

impl HookWants {
    pub fn from_settings(settings: &crate::config::Settings) -> Self {
        Self {
            naming: settings.auto_name_agent_sessions,
            // Task 4 wires the setting.
            state: false,
        }
    }
}

/// Every event Foreman has ever managed, so an event that is no longer wanted
/// is cleaned up rather than left behind.
const KNOWN_EVENTS: [&str; 6] = [
    "UserPromptSubmit",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "StopFailure",
    "Interrupt",
];

/// `PostToolUse` has no matcher: which tools prompt depends on the user's
/// permission mode. Grok has no `PermissionRequest`; Codex has no
/// `StopFailure` and is the only provider with `Interrupt`.
fn wanted_events(agent: &str, wants: HookWants) -> &'static [&'static str] {
    if wants.state {
        match agent {
            "claude" => &[
                "UserPromptSubmit",
                "PermissionRequest",
                "PostToolUse",
                "Stop",
                "StopFailure",
            ],
            "codex" => &[
                "UserPromptSubmit",
                "PermissionRequest",
                "PostToolUse",
                "Stop",
                "Interrupt",
            ],
            _ => &["UserPromptSubmit", "PostToolUse", "Stop", "StopFailure"],
        }
    } else if wants.naming {
        &["UserPromptSubmit"]
    } else {
        &[]
    }
}

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
pub fn spawn_install(
    ctx: eframe::egui::Context,
    wants: HookWants,
) -> mpsc::Receiver<InstallReport> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let report = install(wants);
        let _ = tx.send(report);
        ctx.request_repaint();
    });
    rx
}

fn install(wants: HookWants) -> InstallReport {
    let Some(roots) = roots_from_env() else {
        return InstallReport {
            changed: 0,
            errors: vec!["user home is unavailable; agent hooks were not installed".into()],
        };
    };
    install_in(&roots, wants)
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

fn install_in(roots: &HookRoots, wants: HookWants) -> InstallReport {
    let targets = [
        (roots.claude.join("settings.json"), "claude"),
        (roots.codex.join("hooks.json"), "codex"),
        (roots.grok.join("hooks").join(GROK_FILE), "grok"),
    ];
    let mut report = InstallReport::default();
    for (path, agent) in targets {
        match install_one(&path, agent, wants) {
            Ok(true) => report.changed += 1,
            Ok(false) => {}
            Err(error) => report.errors.push(format!("{agent} hook: {error}")),
        }
    }
    report
}

fn install_one(path: &Path, agent: &str, wants: HookWants) -> Result<bool, String> {
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
    let merged = merge_hooks(root, agent, wants)?;
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

fn managed_command(agent: &str, event: &str) -> String {
    if cfg!(windows) {
        match agent {
            // Pin Claude to PowerShell below, which is present on every Windows
            // version Foreman supports and handles paths with spaces.
            "claude" => format!(
                "try {{ if ($env:FOREMAN_EXE) {{ & $env:FOREMAN_EXE title-event --agent {agent} --event {event} *> $null }} }} catch {{}}; exit 0"
            ),
            // Grok's Windows runner and Codex's selected session shell are not
            // guaranteed to be cmd.exe. An encoded PowerShell command is one
            // shell-neutral argv shape: neither cmd nor PowerShell reparses
            // the relay script, and the native helper inherits hook stdin.
            "grok" => windows_powershell_relay(agent, event),
            // Codex keeps this portable value for shared home directories and
            // uses the commandWindows override installed below.
            _ => unix_managed_command(agent, event),
        }
    } else {
        unix_managed_command(agent, event)
    }
}

fn unix_managed_command(agent: &str, event: &str) -> String {
    format!(
        "if [ -n \"${{FOREMAN_EXE:-}}\" ]; then \"$FOREMAN_EXE\" title-event --agent {agent} --event {event} >/dev/null 2>&1 || true; fi"
    )
}

#[cfg(windows)]
fn windows_powershell_relay(agent: &str, event: &str) -> String {
    use base64::Engine as _;

    let script = format!(
        "try {{ if ($env:FOREMAN_EXE) {{ & $env:FOREMAN_EXE title-event --agent {agent} --event {event} *> $null }} }} catch {{}}; exit 0"
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
    let is_ours =
        |text: &str| text.contains("FOREMAN_EXE") && text.contains("title-event --agent ");
    is_ours(command) || relay_script(command).is_some_and(|script| is_ours(&script))
}

/// The PowerShell script inside an encoded relay command, if `command` is one.
fn relay_script(command: &str) -> Option<String> {
    let encoded = command.strip_prefix(POWERSHELL_RELAY_PREFIX)?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let utf16 = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&utf16).ok()
}

fn managed_handler(agent: &str, event: &str) -> serde_json::Value {
    let mut handler = serde_json::json!({
        "type": "command",
        "command": managed_command(agent, event),
        "timeout": 1
    });
    let object = handler
        .as_object_mut()
        .expect("handler literal is an object");
    // Both schemas support an explicit background command. The hook is
    // passive, so helper startup must never delay the agent. Grok's events
    // are already non-blocking and have no documented per-handler `async`.
    if matches!(agent, "claude" | "codex") {
        object.insert("async".into(), serde_json::json!(true));
    }
    if cfg!(windows) {
        if agent == "claude" {
            object.insert("shell".into(), serde_json::json!("powershell"));
        } else if agent == "codex" {
            // `commandWindows` selects a platform string, not an execution
            // shell: Codex uses the Session's selected shell when one exists,
            // so the encoded relay has to work under both cmd.exe and
            // PowerShell.
            object.insert(
                "commandWindows".into(),
                serde_json::json!(windows_powershell_relay(agent, event)),
            );
            object.insert("timeout".into(), serde_json::json!(2));
        }
    }
    handler
}

fn merge_hooks(
    mut root: serde_json::Value,
    agent: &str,
    wants: HookWants,
) -> Result<serde_json::Value, String> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| "the hook file must contain a JSON object".to_string())?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "the existing `hooks` value must be a JSON object".to_string())?;
    let wanted = wanted_events(agent, wants);
    for event in KNOWN_EVENTS {
        merge_event(hooks, agent, event, wanted.contains(&event))?;
    }
    Ok(root)
}

/// Update one event's handler list in place: the first managed handler is
/// replaced where it stands, extra managed handlers are removed, and a new
/// one is appended only when none existed. Unmanaged handlers never move:
/// Codex keys hook trust by position.
fn merge_event(
    hooks: &mut serde_json::Map<String, serde_json::Value>,
    agent: &str,
    event: &str,
    wanted: bool,
) -> Result<(), String> {
    let desired = wanted.then(|| managed_handler(agent, event));
    let Some(groups) = hooks.get_mut(event) else {
        if let Some(handler) = desired {
            hooks.insert(event.into(), serde_json::json!([{ "hooks": [handler] }]));
        }
        return Ok(());
    };
    let groups = groups
        .as_array_mut()
        .ok_or_else(|| format!("the existing `hooks.{event}` value must be an array"))?;
    let mut placed = false;
    for group in groups.iter_mut() {
        let Some(handlers) = group
            .get_mut("hooks")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        let had_handlers = !handlers.is_empty();
        let mut kept = Vec::with_capacity(handlers.len());
        for handler in handlers.drain(..) {
            if !is_managed_handler(&handler) {
                kept.push(handler);
            } else if !placed && let Some(desired) = &desired {
                kept.push(desired.clone());
                placed = true;
            }
        }
        *handlers = kept;
        // A group we emptied goes away; a group the user left empty stays.
        if had_handlers && handlers.is_empty() {
            *group = serde_json::Value::Null;
        }
    }
    groups.retain(|group| !group.is_null());
    if let Some(handler) = desired
        && !placed
    {
        groups.push(serde_json::json!({ "hooks": [handler] }));
    }
    if groups.is_empty() {
        hooks.remove(event);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMING: HookWants = HookWants {
        naming: true,
        state: false,
    };
    const STATE: HookWants = HookWants {
        naming: true,
        state: true,
    };

    fn roots_in(temp: &tempfile::TempDir) -> HookRoots {
        HookRoots {
            claude: temp.path().join("claude"),
            codex: temp.path().join("codex"),
            grok: temp.path().join("grok"),
        }
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn state_install_adds_each_providers_events_with_the_event_name() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots_in(&temp);
        let report = install_in(&roots, STATE);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        for (agent, path) in [
            ("claude", roots.claude.join("settings.json")),
            ("codex", roots.codex.join("hooks.json")),
            ("grok", roots.grok.join("hooks").join(GROK_FILE)),
        ] {
            let root = read_json(&path);
            for event in wanted_events(agent, STATE) {
                let handler = &root["hooks"][*event][0]["hooks"][0];
                assert!(is_managed_handler(handler), "{agent} {event}: {handler}");
                // Grok's Windows handler is an encoded PowerShell relay.
                let command = handler["command"].as_str().unwrap();
                let script = relay_script(command).unwrap_or_else(|| command.to_string());
                assert!(
                    script.contains(&format!("--event {event}")),
                    "{agent} {event}: {script}"
                );
            }
            assert!(
                root["hooks"].get("SessionStart").is_none(),
                "{agent} must not install SessionStart"
            );
        }
        assert!(
            read_json(&roots.codex.join("hooks.json"))["hooks"]
                .get("StopFailure")
                .is_none()
        );
        assert!(
            read_json(&roots.grok.join("hooks").join(GROK_FILE))["hooks"]
                .get("PermissionRequest")
                .is_none()
        );
    }

    #[test]
    fn turning_state_off_removes_its_events_and_keeps_naming() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots_in(&temp);
        install_in(&roots, STATE);
        let report = install_in(&roots, NAMING);
        assert!(report.changed > 0);
        let claude = read_json(&roots.claude.join("settings.json"));
        let hooks = claude["hooks"].as_object().unwrap();
        assert_eq!(
            hooks.keys().collect::<Vec<_>>(),
            vec!["UserPromptSubmit"],
            "{hooks:?}"
        );
        let report = install_in(&roots, HookWants::default());
        assert!(report.changed > 0);
        let claude = read_json(&roots.claude.join("settings.json"));
        assert!(claude["hooks"].as_object().unwrap().is_empty(), "{claude}");
    }

    #[test]
    fn reinstalls_never_move_a_handler() {
        // Codex keys hook trust by position; any reorder forces re-trust.
        let temp = tempfile::tempdir().unwrap();
        let roots = roots_in(&temp);
        std::fs::create_dir_all(&roots.codex).unwrap();
        let codex = roots.codex.join("hooks.json");
        std::fs::write(
            &codex,
            br#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"keep-me"}]}],"Stop":[{"hooks":[{"type":"command","command":"also-keep"}]}]}}"#,
        )
        .unwrap();
        install_in(&roots, NAMING);
        let after_naming = std::fs::read(&codex).unwrap();
        install_in(&roots, STATE);
        let after_state = read_json(&codex);
        let prompt_groups = after_state["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(prompt_groups[0]["hooks"][0]["command"], "keep-me");
        assert!(is_managed_handler(&prompt_groups[1]["hooks"][0]));
        assert_eq!(prompt_groups.len(), 2);
        let stop_groups = after_state["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_groups[0]["hooks"][0]["command"], "also-keep");
        assert!(is_managed_handler(&stop_groups[1]["hooks"][0]));
        assert_eq!(
            install_in(&roots, STATE).changed,
            0,
            "state twice is a no-op"
        );
        install_in(&roots, NAMING);
        assert_eq!(
            std::fs::read(&codex).unwrap(),
            after_naming,
            "back to naming restores the exact bytes"
        );
    }

    #[test]
    fn an_older_handler_without_an_event_flag_is_replaced_in_place() {
        let old = serde_json::json!({"hooks":{"UserPromptSubmit":[
            {"hooks":[{"type":"command","command":"first"}]},
            {"hooks":[{"type":"command","command":"if [ -n \"${FOREMAN_EXE:-}\" ]; then \"$FOREMAN_EXE\" title-event --agent codex >/dev/null 2>&1 || true; fi"}]},
            {"hooks":[{"type":"command","command":"last"}]}
        ]}});
        let merged = merge_hooks(old, "codex", NAMING).unwrap();
        let groups = merged["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0]["hooks"][0]["command"], "first");
        assert_eq!(
            groups[1]["hooks"][0]["command"],
            managed_command("codex", "UserPromptSubmit")
        );
        assert_eq!(groups[2]["hooks"][0]["command"], "last");
    }

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
        let once = merge_hooks(original, "claude", NAMING).unwrap();
        let twice = merge_hooks(once.clone(), "claude", NAMING).unwrap();
        assert_eq!(once, twice);
        assert_eq!(once["theme"], "dark");
        let text = serde_json::to_string(&once).unwrap();
        assert!(text.contains("keep-me"));
        assert!(text.contains("also-keep"));
        assert_eq!(
            text.matches("title-event --agent claude --event UserPromptSubmit")
                .count(),
            1
        );
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
        assert!(merge_hooks(serde_json::json!([]), "codex", NAMING).is_err());
        assert!(merge_hooks(serde_json::json!({"hooks": []}), "codex", NAMING).is_err());
        assert!(
            merge_hooks(
                serde_json::json!({"hooks": {"UserPromptSubmit": {}}}),
                "codex",
                NAMING
            )
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn managed_commands_match_each_windows_hook_shell() {
        assert_eq!(
            managed_command("codex", "UserPromptSubmit"),
            "if [ -n \"${FOREMAN_EXE:-}\" ]; then \"$FOREMAN_EXE\" title-event --agent codex --event UserPromptSubmit >/dev/null 2>&1 || true; fi"
        );
        assert_eq!(
            managed_command("claude", "UserPromptSubmit"),
            "try { if ($env:FOREMAN_EXE) { & $env:FOREMAN_EXE title-event --agent claude --event UserPromptSubmit *> $null } } catch {}; exit 0"
        );
        assert!(
            managed_command("grok", "UserPromptSubmit")
                .starts_with("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ")
        );
        let merged = merge_hooks(serde_json::json!({}), "claude", NAMING).unwrap();
        assert_eq!(
            merged["hooks"]["UserPromptSubmit"][0]["hooks"][0]["shell"],
            "powershell"
        );
        let merged = merge_hooks(serde_json::json!({}), "codex", NAMING).unwrap();
        assert_eq!(
            merged["hooks"]["UserPromptSubmit"][0]["hooks"][0]["commandWindows"],
            windows_powershell_relay("codex", "UserPromptSubmit")
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

        let merged = merge_hooks(serde_json::json!({}), "codex", NAMING).unwrap();
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

        let first = install_in(&roots, NAMING);
        assert_eq!(first.changed, 3);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(
            std::fs::read(backup_path(&claude)).unwrap(),
            br#"{"keep":true}"#
        );
        let installed = std::fs::read(&claude).unwrap();
        let second = install_in(&roots, NAMING);
        assert_eq!(second.changed, 0);
        assert_eq!(std::fs::read(&claude).unwrap(), installed);

        let codex = roots.codex.join("hooks.json");
        std::fs::write(&codex, b"not-json").unwrap();
        let report = install_in(&roots, NAMING);
        assert_eq!(std::fs::read(&codex).unwrap(), b"not-json");
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("malformed JSON"))
        );
    }
}
