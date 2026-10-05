//! Isolated theme conversation: provider text becomes a preview only after a
//! complete, known-field `Theme` value validates. No provider output is a command.

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui;
use serde_json::Value;

use crate::ai_oneshot::{self, LaunchError};
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
    /// For an expert reply: the index into `ThemeExpert::proposals` it produced,
    /// so the pane can anchor the proposal card under that message.
    pub proposal: Option<usize>,
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
            proposal: None,
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

    /// Adopt a validated reply: its theme becomes the newest proposal (selected
    /// for preview) and its message a turn linked to that proposal.
    pub fn accept(&mut self, reply: Reply) {
        self.proposals.push(reply.theme);
        let idx = self.proposals.len() - 1;
        self.turns.push(Turn {
            user: false,
            text: reply.message,
            proposal: Some(idx),
        });
        self.selected = Some(idx);
        self.pending = None;
    }

    pub fn poll(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(reply)) => self.accept(reply),
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
    let cwd = crate::config::config_dir()
        .ok_or("No settings directory")?
        .join("theme-expert");
    std::fs::create_dir_all(&cwd).map_err(|_| "Could not prepare theme expert".to_string())?;
    let program = ai_oneshot::program(provider);
    let raw = ai_oneshot::run(&ai_oneshot::Request {
        provider,
        model,
        system: None,
        prompt: &prompt,
        cwd: &cwd,
        timeout: TIMEOUT,
        max_output: MAX_OUTPUT,
    })
    .map_err(|error| match error {
        LaunchError::Unavailable => format!("{program} CLI is unavailable"),
        LaunchError::Failed => format!("{program} could not generate a theme"),
        LaunchError::Timeout => "Theme provider timed out".into(),
        LaunchError::TooLarge => "Provider response was too large".into(),
    })?;
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
    fn expert_replies_link_to_their_proposal() {
        let mut x = ThemeExpert::new();
        x.turns.push(Turn {
            user: true,
            text: "warmer".into(),
            proposal: None,
        });
        x.accept(Reply {
            message: "done".into(),
            theme: Theme::foreman_warm(),
        });
        assert_eq!(x.proposals.len(), 1);
        assert_eq!(x.turns.last().unwrap().proposal, Some(0));
        assert_eq!(x.selected, Some(0));
        assert!(!x.busy());
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
