//! Isolated theme conversation: provider text becomes a preview only after a
//! complete, known-field `Theme` value validates. No provider output is a command.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::Value;

use crate::config::NamingProvider;
use crate::theme::Theme;

const MAX_OUTPUT: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug)]
pub struct Reply {
    pub message: String,
    pub theme: Theme,
}

/// The only accepted provider payload. `Theme`'s disk serde accepts missing
/// fields for forward compatibility; conversation output deliberately does not.
pub fn parse_reply(raw: &str) -> Result<Reply, String> {
    let value: Value = serde_json::from_str(raw.trim())
        .map_err(|_| "Expected a JSON theme proposal".to_string())?;
    let object = value.as_object().ok_or("Expected a JSON object")?;
    if object.len() != 2 || !object.contains_key("message") || !object.contains_key("theme") {
        return Err("Proposal must contain only message and theme".into());
    }
    let message = object["message"]
        .as_str()
        .ok_or("Proposal message must be text")?;
    if message.trim().is_empty() || message.len() > 2000 {
        return Err("Proposal message is empty or too long".into());
    }
    let theme_value = &object["theme"];
    let fields = theme_value.as_object().ok_or("Theme must be an object")?;
    let expected = serde_json::to_value(Theme::foreman_warm()).map_err(|e| e.to_string())?;
    let known = expected.as_object().expect("Theme serializes to an object");
    if fields.len() != known.len() || fields.keys().any(|key| !known.contains_key(key)) {
        return Err("Theme must contain every supported color and no other fields".into());
    }
    let theme = serde_json::from_value(theme_value.clone())
        .map_err(|e| format!("Invalid theme color: {e}"))?;
    Ok(Reply {
        message: message.to_owned(),
        theme,
    })
}

#[derive(Clone, Debug)]
pub struct Turn {
    pub user: bool,
    pub text: String,
}

/// Keeps provider history and previews separate from Appearance's persisted
/// working theme. Dropping the Settings window drops the receiver; the bounded
/// worker still finishes or times out without retaining any GUI state.
#[derive(Debug)]
pub struct ThemeExpert {
    pub provider: NamingProvider,
    pub model: String,
    pub input: String,
    pub turns: Vec<Turn>,
    pub proposals: Vec<Theme>,
    pub selected: Option<usize>,
    pub error: Option<String>,
    pending: Option<Receiver<Result<Reply, String>>>,
}

impl ThemeExpert {
    pub fn new() -> Self {
        Self {
            provider: NamingProvider::Codex,
            model: String::new(), // configured Codex default
            input: String::new(),
            turns: Vec::new(),
            proposals: Vec::new(),
            selected: None,
            error: None,
            pending: None,
        }
    }

    pub fn preview(&self) -> Option<&Theme> {
        self.selected.and_then(|i| self.proposals.get(i))
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn send(&mut self, base: &Theme, ctx: &egui::Context) {
        if self.busy() || self.input.trim().is_empty() {
            return;
        }
        let request = self.input.trim().chars().take(4000).collect::<String>();
        self.input.clear();
        self.turns.push(Turn {
            user: true,
            text: request,
        });
        self.error = None;
        let history = self.turns.clone();
        let base = self.preview().unwrap_or(base).clone();
        let provider = self.provider;
        let model = self.model.trim().chars().take(128).collect::<String>();
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = generate(provider, &model, &history, &base);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub fn poll(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(reply)) => {
                self.turns.push(Turn {
                    user: false,
                    text: reply.message,
                });
                self.proposals.push(reply.theme);
                self.selected = Some(self.proposals.len() - 1);
                self.pending = None;
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                self.pending = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.error = Some("Theme provider stopped unexpectedly".into());
                self.pending = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

fn generate(
    provider: NamingProvider,
    model: &str,
    history: &[Turn],
    base: &Theme,
) -> Result<Reply, String> {
    let baseline = serde_json::to_string(base).map_err(|e| e.to_string())?;
    let recent = history.iter().rev().take(20).collect::<Vec<_>>();
    let dialogue = serde_json::to_string(&recent.into_iter().rev().map(|t| {
        serde_json::json!({"role": if t.user { "user" } else { "assistant" }, "text": t.text})
    }).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    let prompt = format!(
        "You are Foreman's theme expert. Discuss and refine the user's theme. Return ONLY a JSON object with exactly two keys: message (brief explanation) and theme (a COMPLETE Foreman Theme object). Change colors only. Preserve every field and every array length, using hex strings in the supplied format. No commands, file edits, markdown, or extra keys.\nCurrent theme JSON:\n{baseline}\nConversation JSON:\n{dialogue}"
    );
    let mut args: Vec<String> = match provider {
        NamingProvider::Codex => vec![
            "exec",
            "--sandbox",
            "read-only",
            "--skip-git-repo-check",
            "--ephemeral",
            "--ignore-rules",
            "--disable",
            "shell_tool",
            "--disable",
            "unified_exec",
            "--color",
            "never",
            "-",
        ],
        NamingProvider::Claude => vec![
            "-p",
            "--restricted",
            "--tools",
            "",
            "--disable-slash-commands",
            "--no-chrome",
            "--no-session-persistence",
            "--output-format",
            "text",
        ],
        NamingProvider::Grok => vec![
            "--no-auto-update",
            "--single",
            "--output-format",
            "plain",
            "--tools",
            "",
            "--disable-web-search",
            "--no-subagents",
            "--max-turns",
            "1",
            "--permission-mode",
            "dontAsk",
            "--verbatim",
        ],
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    if !model.is_empty() {
        args.extend(["--model".into(), model.into()]);
    }
    if provider == NamingProvider::Grok {
        args.push(prompt.clone());
    }
    let program = match provider {
        NamingProvider::Codex => "codex",
        NamingProvider::Claude => "claude",
        NamingProvider::Grok => "grok",
    };
    let cwd = crate::config::config_dir()
        .ok_or("No settings directory")?
        .join("theme-expert");
    std::fs::create_dir_all(&cwd).map_err(|_| "Could not prepare theme expert".to_string())?;
    let argv = std::iter::once(program.to_owned())
        .chain(args)
        .collect::<Vec<_>>();
    let resolved =
        crate::agent_command::npm_codex(&argv, &cwd, &std::env::var_os("PATH").unwrap_or_default());
    let argv = resolved.unwrap_or(argv);
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|_| format!("{program} CLI is unavailable"))?;
    #[cfg(windows)]
    let child_job = crate::job::Job::assign(child.id());
    if let Some(mut stdin) = child.stdin.take() {
        let input = if provider == NamingProvider::Grok {
            String::new()
        } else {
            prompt
        };
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let stdout = child.stdout.take().ok_or("Provider output unavailable")?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take((MAX_OUTPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = tx.send(result);
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Theme provider timed out or failed".into());
            }
        }
    };
    #[cfg(windows)]
    drop(child_job);
    if !status.success() {
        return Err(format!("{program} could not generate a theme"));
    }
    let bytes = rx
        .recv_timeout(TIMEOUT.saturating_sub(start.elapsed()))
        .map_err(|_| "Provider output timed out")?
        .map_err(|_| "Provider output could not be read")?;
    if bytes.len() > MAX_OUTPUT {
        return Err("Provider response was too large".into());
    }
    let raw = String::from_utf8(bytes).map_err(|_| "Provider response was not UTF-8")?;
    parse_reply(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_complete_theme_and_rejects_extra_or_partial_values() {
        let theme = serde_json::to_value(Theme::foreman_warm()).unwrap();
        let valid = serde_json::json!({"message":"Cooler blues", "theme":theme});
        assert_eq!(
            parse_reply(&valid.to_string()).unwrap().theme,
            Theme::foreman_warm()
        );
        let mut partial = valid.clone();
        partial["theme"].as_object_mut().unwrap().remove("bg");
        assert!(parse_reply(&partial.to_string()).is_err());
        let mut extra = valid.clone();
        extra["theme"]["command"] = Value::String("echo unsafe".into());
        assert!(parse_reply(&extra.to_string()).is_err());
        let mut bad = valid;
        bad["theme"]["bg"] = Value::String("not a color".into());
        assert!(parse_reply(&bad.to_string()).is_err());
        assert!(parse_reply("```json\n{}\n```").is_err());
    }

    #[test]
    fn proposals_do_not_mutate_the_base_theme() {
        let mut expert = ThemeExpert::new();
        let base = Theme::foreman_warm();
        let mut next = base.clone();
        next.bg = egui::Color32::RED;
        expert.proposals.push(next.clone());
        expert.selected = Some(0);
        assert_eq!(expert.preview(), Some(&next));
        expert.selected = None;
        assert_eq!(base, Theme::foreman_warm());
    }
}
