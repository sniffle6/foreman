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
    /// A short display name the expert suggests for the proposal (used when
    /// Apply has to fork the built-in). Optional in the payload.
    pub name: Option<String>,
}

/// Longest accepted `name` in a reply.
pub const MAX_NAME_CHARS: usize = 40;

/// The only accepted provider payload: `message` + a complete `theme`, plus an
/// optional `name`. `Theme`'s disk serde accepts missing fields for forward
/// compatibility; conversation output deliberately does not.
pub fn parse_reply(raw: &str) -> Result<Reply, String> {
    let value: Value = serde_json::from_str(raw.trim())
        .map_err(|_| "Expected a JSON theme proposal".to_string())?;
    let object = value.as_object().ok_or("Expected a JSON object")?;
    let allowed = object
        .keys()
        .all(|k| matches!(k.as_str(), "message" | "theme" | "name"));
    if !allowed || !object.contains_key("message") || !object.contains_key("theme") {
        return Err("Proposal must contain only message, theme and an optional name".into());
    }
    let message = object["message"]
        .as_str()
        .ok_or("Proposal message must be text")?;
    if message.trim().is_empty() || message.len() > 2000 {
        return Err("Proposal message is empty or too long".into());
    }
    let name = match object.get("name") {
        None => None,
        Some(v) => {
            let s = v.as_str().ok_or("Proposal name must be text")?.trim();
            if s.is_empty()
                || s.chars().count() > MAX_NAME_CHARS
                || crate::theme::slug(s).trim_matches('-').is_empty()
            {
                return Err(
                    "Proposal name must be 1-40 characters with some letters or digits".into(),
                );
            }
            Some(s.to_owned())
        }
    };
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
        name,
    })
}

/// Which keys a request lets the expert change. The merge in [`Scope::merge`]
/// enforces it locally, so an out-of-scope key cannot move even if the model
/// ignores the instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    All,
    Group(crate::theme::TokenGroup),
    Palette,
}

impl Scope {
    /// Chip order in the pane.
    pub const ALL: [Scope; 8] = [
        Scope::All,
        Scope::Group(crate::theme::TokenGroup::Terminal),
        Scope::Group(crate::theme::TokenGroup::Windows),
        Scope::Group(crate::theme::TokenGroup::Text),
        Scope::Group(crate::theme::TokenGroup::AppBar),
        Scope::Group(crate::theme::TokenGroup::Chat),
        Scope::Group(crate::theme::TokenGroup::Search),
        Scope::Palette,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "All",
            Scope::Group(g) => g.label(),
            Scope::Palette => "Palette",
        }
    }

    /// The JSON keys this scope allows the expert to change (for the prompt).
    pub fn keys(self) -> Vec<&'static str> {
        use crate::theme::{TOKENS, TokenGroup};
        match self {
            Scope::All => {
                let mut k: Vec<&str> = TOKENS.iter().map(|s| s.key).collect();
                k.push("palette");
                k.push("chat_colors");
                k
            }
            Scope::Group(g) => {
                let mut k: Vec<&str> = TOKENS
                    .iter()
                    .filter(|s| s.group == g)
                    .map(|s| s.key)
                    .collect();
                if g == TokenGroup::Chat {
                    k.push("chat_colors");
                }
                k
            }
            Scope::Palette => vec!["palette"],
        }
    }

    /// `base` with only this scope's keys taken from `reply`.
    pub fn merge(self, base: &Theme, reply: &Theme) -> Theme {
        use crate::theme::{TOKENS, TokenGroup};
        match self {
            Scope::All => reply.clone(),
            Scope::Group(g) => {
                let mut out = base.clone();
                for spec in TOKENS.iter().filter(|s| s.group == g) {
                    (spec.set)(&mut out, (spec.get)(reply));
                }
                if g == TokenGroup::Chat {
                    out.chat_colors = reply.chat_colors;
                }
                out
            }
            Scope::Palette => {
                let mut out = base.clone();
                out.palette = reply.palette;
                out
            }
        }
    }
}

/// One token that a proposal changed relative to the theme it was made from.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// `bg`, `palette[3]`, `chat_colors[0]` …
    pub key: String,
    pub before: egui::Color32,
    pub after: egui::Color32,
}

/// A validated, scope-merged proposal with its diff against its base.
#[derive(Clone, Debug)]
pub struct Proposal {
    pub theme: Theme,
    pub changes: Vec<Change>,
    pub name: Option<String>,
}

impl Proposal {
    pub fn new(base: &Theme, theme: Theme, name: Option<String>) -> Self {
        let changes = changes_between(base, &theme);
        Self {
            theme,
            changes,
            name,
        }
    }
}

/// Every token that differs between `base` and `next`, in pane order: the
/// scalar tokens, then the palette slots, then the chat member colours.
pub fn changes_between(base: &Theme, next: &Theme) -> Vec<Change> {
    let mut out = Vec::new();
    for spec in crate::theme::TOKENS {
        let (b, a) = ((spec.get)(base), (spec.get)(next));
        if b != a {
            out.push(Change {
                key: spec.key.to_string(),
                before: b,
                after: a,
            });
        }
    }
    for (i, (b, a)) in base.palette.iter().zip(next.palette.iter()).enumerate() {
        if b != a {
            out.push(Change {
                key: format!("palette[{i}]"),
                before: *b,
                after: *a,
            });
        }
    }
    for (i, (b, a)) in base
        .chat_colors
        .iter()
        .zip(next.chat_colors.iter())
        .enumerate()
    {
        if b != a {
            out.push(Change {
                key: format!("chat_colors[{i}]"),
                before: *b,
                after: *a,
            });
        }
    }
    out
}

/// An in-flight request: the receiver plus what the reply must be merged onto.
#[derive(Debug)]
struct Pending {
    rx: Receiver<Result<Reply, String>>,
    base: Theme,
    scope: Scope,
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
    /// What the next request may change (the pane's scope chips).
    pub scope: Scope,
    pub turns: Vec<Turn>,
    pub proposals: Vec<Proposal>,
    pub selected: Option<usize>,
    pub error: Option<String>,
    pending: Option<Pending>,
}

impl ThemeExpert {
    pub fn new() -> Self {
        Self {
            provider: NamingProvider::Codex,
            model: String::new(), // configured Codex default
            input: String::new(),
            scope: Scope::All,
            turns: Vec::new(),
            proposals: Vec::new(),
            selected: None,
            error: None,
            pending: None,
        }
    }

    pub fn preview(&self) -> Option<&Theme> {
        self.preview_proposal().map(|p| &p.theme)
    }

    pub fn preview_proposal(&self) -> Option<&Proposal> {
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
        let scope = self.scope;
        let model = self.model.trim().chars().take(128).collect::<String>();
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending = Some(Pending {
            rx,
            base: base.clone(),
            scope,
        });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = generate(provider, &model, &history, &base, scope);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Adopt a validated reply: its theme is merged onto `base` under `scope`
    /// (out-of-scope keys cannot move), becomes the newest proposal (selected
    /// for preview) with its diff, and its message a turn linked to it.
    pub fn accept(&mut self, reply: Reply, base: &Theme, scope: Scope) {
        let merged = scope.merge(base, &reply.theme);
        self.proposals.push(Proposal::new(base, merged, reply.name));
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
        let Some(pending) = &self.pending else {
            return;
        };
        match pending.rx.try_recv() {
            Ok(Ok(reply)) => {
                let Pending { base, scope, .. } = self.pending.take().expect("checked above");
                self.accept(reply, &base, scope);
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
    scope: Scope,
) -> Result<Reply, String> {
    let baseline = serde_json::to_string(base).map_err(|e| e.to_string())?;
    let recent = history.iter().rev().take(20).collect::<Vec<_>>();
    let dialogue = serde_json::to_string(&recent.into_iter().rev().map(|t| {
        serde_json::json!({"role": if t.user { "user" } else { "assistant" }, "text": t.text})
    }).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    let prompt = build_prompt(&baseline, &dialogue, scope);
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

/// The instruction block. `scope` narrows what may change; the merge enforces
/// it regardless, this just keeps the model from wasting its answer.
fn build_prompt(baseline: &str, dialogue: &str, scope: Scope) -> String {
    let scope_line = match scope {
        Scope::All => "You may change any color.".to_string(),
        other => format!(
            "You may change ONLY these keys: {}. Return every other value exactly as given.",
            other.keys().join(", ")
        ),
    };
    format!(
        "You are Foreman's theme expert. Discuss and refine the user's theme. Return ONLY a JSON object with these keys: message (brief explanation), theme (a COMPLETE Foreman Theme object), and name (a short display name, at most {MAX_NAME_CHARS} characters, that fits the proposal). {scope_line} Change colors only. Preserve every field and every array length, using hex strings in the supplied format. No commands, file edits, markdown, or extra keys.\nCurrent theme JSON:\n{baseline}\nConversation JSON:\n{dialogue}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_optional_name_and_rejects_a_bad_one() {
        let theme = serde_json::to_value(Theme::foreman_warm()).unwrap();
        let named = serde_json::json!({"message":"ok", "theme":theme, "name":" Ember Night "});
        assert_eq!(
            parse_reply(&named.to_string()).unwrap().name.as_deref(),
            Some("Ember Night")
        );
        let unnamed = serde_json::json!({"message":"ok", "theme":theme});
        assert_eq!(parse_reply(&unnamed.to_string()).unwrap().name, None);
        for bad in [
            serde_json::json!(""),
            serde_json::json!("---"),
            serde_json::json!("x".repeat(41)),
            serde_json::json!(7),
        ] {
            let v = serde_json::json!({"message":"ok", "theme":theme, "name":bad});
            assert!(
                parse_reply(&v.to_string()).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn scope_merge_keeps_out_of_scope_keys_even_if_the_model_changed_them() {
        let base = Theme::foreman_warm();
        let mut reply = base.clone();
        reply.palette[2] = egui::Color32::RED;
        reply.title_bg = egui::Color32::BLUE;
        reply.chat_colors[0] = egui::Color32::GREEN;
        let windows = Scope::Group(crate::theme::TokenGroup::Windows).merge(&base, &reply);
        assert_eq!(windows.title_bg, egui::Color32::BLUE);
        assert_eq!(
            windows.palette, base.palette,
            "palette untouched under Windows"
        );
        assert_eq!(windows.chat_colors, base.chat_colors);
        let palette = Scope::Palette.merge(&base, &reply);
        assert_eq!(palette.palette[2], egui::Color32::RED);
        assert_eq!(palette.title_bg, base.title_bg);
        let chat = Scope::Group(crate::theme::TokenGroup::Chat).merge(&base, &reply);
        assert_eq!(chat.chat_colors[0], egui::Color32::GREEN);
        assert_eq!(chat.title_bg, base.title_bg);
        assert_eq!(Scope::All.merge(&base, &reply), reply);
        assert!(Scope::Palette.keys() == vec!["palette"]);
        assert!(Scope::All.keys().contains(&"chat_colors"));
    }

    #[test]
    fn changes_list_tokens_then_palette_then_chat_slots() {
        let base = Theme::foreman_warm();
        let mut next = base.clone();
        next.palette[3] = egui::Color32::RED;
        next.bg = egui::Color32::BLACK;
        next.chat_colors[5] = egui::Color32::WHITE;
        let keys: Vec<String> = changes_between(&base, &next)
            .into_iter()
            .map(|c| c.key)
            .collect();
        assert_eq!(keys, vec!["bg", "palette[3]", "chat_colors[5]"]);
        let c = &changes_between(&base, &next)[0];
        assert_eq!((c.before, c.after), (base.bg, egui::Color32::BLACK));
        assert!(changes_between(&base, &base).is_empty());
    }

    #[test]
    fn prompt_names_the_scope_keys() {
        let p = build_prompt("{}", "[]", Scope::Palette);
        assert!(p.contains("ONLY these keys: palette."));
        let p = build_prompt("{}", "[]", Scope::All);
        assert!(p.contains("any color"));
        assert!(p.contains("name ("));
    }

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
        let base = Theme::foreman_warm();
        let mut reply_theme = base.clone();
        reply_theme.bg = egui::Color32::RED;
        reply_theme.palette[0] = egui::Color32::BLUE; // out of scope: must not land
        x.accept(
            Reply {
                message: "done".into(),
                theme: reply_theme,
                name: Some("Red Dawn".into()),
            },
            &base,
            Scope::Group(crate::theme::TokenGroup::Terminal),
        );
        assert_eq!(x.proposals.len(), 1);
        assert_eq!(x.turns.last().unwrap().proposal, Some(0));
        assert_eq!(x.selected, Some(0));
        assert!(!x.busy());
        let p = &x.proposals[0];
        assert_eq!(p.theme.bg, egui::Color32::RED);
        assert_eq!(p.theme.palette[0], base.palette[0]);
        assert_eq!(p.changes.len(), 1);
        assert_eq!(p.changes[0].key, "bg");
        assert_eq!(p.name.as_deref(), Some("Red Dawn"));
    }

    #[test]
    fn proposals_do_not_mutate_the_base_theme() {
        let mut expert = ThemeExpert::new();
        let base = Theme::foreman_warm();
        let mut next = base.clone();
        next.bg = egui::Color32::RED;
        expert
            .proposals
            .push(Proposal::new(&base, next.clone(), None));
        expert.selected = Some(0);
        assert_eq!(expert.preview(), Some(&next));
        expert.selected = None;
        assert_eq!(base, Theme::foreman_warm());
    }
}
