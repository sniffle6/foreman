//! A small, reusable notification center: transient toasts stacked top-right,
//! auto-dismissed after a TTL, colored by severity. The queue lifecycle is a
//! pure seam (`push` + `prune`) tested without a GUI; `show` starts each toast's
//! visible TTL, prunes, then paints.
//!
//! Callers learn two methods and one enum:
//! ```ignore
//! app.notify.push(notify::Level::Error, "Claude isn't installed");
//! // once per frame, on top of everything:
//! app.notify.show(ctx, now);
//! ```
//!
//! A *sticky* toast is for a condition that lasts (the integration queue
//! held behind a dirty checkout): it never expires, is shown or replaced by
//! a key, and goes away when its owner clears the key or the human clicks ✕.

use eframe::egui;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a toast lingers before it auto-dismisses.
const TTL: Duration = Duration::from_secs(6);

/// Severity — tints the toast's accent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    #[allow(dead_code)] // reusable API; no caller yet
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    fn accent(self, th: &crate::theme::Theme) -> egui::Color32 {
        match self {
            Level::Error | Level::Warning => th.danger,
            Level::Info | Level::Success => th.border_focus,
        }
    }
}

struct Toast {
    level: Level,
    text: String,
    visible_since: Option<Instant>,
    /// `Some` for a sticky toast: never expires; replaced and cleared by key.
    key: Option<String>,
}

/// The notification center. Owned by `App`; rendered on top of everything.
pub struct Notifications {
    toasts: Vec<Toast>,
    /// How long a toast lingers before it auto-dismisses (settings `toast_secs`);
    /// `TTL` is the default until `App` seeds a live value.
    ttl: Duration,
    /// Sticky toasts the human dismissed, with the text they dismissed: the
    /// owner re-asserting that same text stays hidden until it clears the key.
    dismissed: HashMap<String, String>,
}

/// A notification op queued from a nested window manager, applied in order
/// by [`Notifications::drain_pending`].
#[derive(Clone)]
enum Pending {
    Toast(Level, String),
    Sticky {
        key: String,
        level: Level,
        text: String,
    },
    Clear(String),
}

fn queue_pending(ctx: &egui::Context, op: Pending) {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Vec<Pending>>(egui::Id::new("pending_notifications"))
            .push(op);
    });
}

/// Queue from nested window managers without threading the App through them.
pub fn queue(ctx: &egui::Context, level: Level, text: impl Into<String>) {
    queue_pending(ctx, Pending::Toast(level, text.into()));
    ctx.request_repaint();
}

/// [`Notifications::set_sticky`] from a nested window manager.
pub fn queue_sticky(
    ctx: &egui::Context,
    key: impl Into<String>,
    level: Level,
    text: impl Into<String>,
) {
    queue_pending(
        ctx,
        Pending::Sticky {
            key: key.into(),
            level,
            text: text.into(),
        },
    );
    ctx.request_repaint();
}

/// [`Notifications::clear_sticky`] from a nested window manager. Cheap to
/// call every poll: it requests no repaint, because the ops drain in the
/// same frame they are queued from and a cleared toast needs no later one.
pub fn queue_clear_sticky(ctx: &egui::Context, key: impl Into<String>) {
    queue_pending(ctx, Pending::Clear(key.into()));
}

impl Notifications {
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            ttl: TTL,
            dismissed: HashMap::new(),
        }
    }

    /// Publish the live toast duration (settings `toast_secs`), seeded once per
    /// frame before `show`.
    pub fn set_ttl(&mut self, d: Duration) {
        self.ttl = d;
    }

    /// Queue a toast. Deduped by (level, text) so a repeated error doesn't
    /// stack. Its TTL begins when [`show`](Self::show) first observes it, which
    /// preserves background notifications across minimize/restore.
    pub fn push(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        if self
            .toasts
            .iter()
            .any(|t| t.level == level && t.text == text)
        {
            return;
        }
        self.toasts.push(Toast {
            level,
            text,
            visible_since: None,
            key: None,
        });
    }

    /// Show the sticky toast `key`, or replace its text in place. The same
    /// text again is a no-op, so an owner can re-assert a standing condition
    /// on every poll. That includes after the human dismissed it: the toast
    /// stays hidden until the text changes or the key is
    /// [cleared](Self::clear_sticky).
    pub fn set_sticky(&mut self, key: impl Into<String>, level: Level, text: impl Into<String>) {
        let (key, text) = (key.into(), text.into());
        if self.dismissed.get(&key) == Some(&text) {
            return;
        }
        self.dismissed.remove(&key);
        match self
            .toasts
            .iter_mut()
            .find(|t| t.key.as_deref() == Some(key.as_str()))
        {
            Some(t) => {
                t.level = level;
                t.text = text;
            }
            None => self.toasts.push(Toast {
                level,
                text,
                visible_since: None,
                key: Some(key),
            }),
        }
    }

    /// The condition behind `key` is over: remove its toast and forget any
    /// dismissal, so the same condition coming back shows again.
    pub fn clear_sticky(&mut self, key: &str) {
        self.toasts.retain(|t| t.key.as_deref() != Some(key));
        self.dismissed.remove(key);
    }

    /// The human clicked ✕ on toast `i`. A sticky toast remembers the text
    /// it was dismissed with (see [`set_sticky`](Self::set_sticky)).
    fn dismiss(&mut self, i: usize) {
        if i >= self.toasts.len() {
            return;
        }
        let t = self.toasts.remove(i);
        if let Some(key) = t.key {
            self.dismissed.insert(key, t.text);
        }
    }

    #[cfg(test)]
    pub(crate) fn contains_text(&self, needle: &str) -> bool {
        self.toasts.iter().any(|toast| toast.text.contains(needle))
    }

    #[cfg(test)]
    pub(crate) fn count_text(&self, needle: &str) -> usize {
        self.toasts
            .iter()
            .filter(|toast| toast.text.contains(needle))
            .count()
    }

    /// Start unpainted toasts and drop transient ones older than their
    /// visible TTL; sticky ones stay. Pure — the tested seam.
    fn prune(&mut self, now: Instant) {
        self.toasts.retain_mut(|toast| {
            let visible_since = toast.visible_since.get_or_insert(now);
            toast.key.is_some() || now.saturating_duration_since(*visible_since) < self.ttl
        });
    }

    /// Prune expired toasts and paint the rest as a top-right overlay. Call once
    /// per frame, after everything else, so toasts sit on top.
    pub fn show(&mut self, ctx: &egui::Context, now: Instant) {
        self.drain_pending(ctx);
        self.prune(now);
        if self.toasts.is_empty() {
            return;
        }
        let mut dismiss: Option<usize> = None;
        egui::Area::new(egui::Id::new("notifications"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-14.0, 40.0))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                let th = crate::theme::live(ui.ctx());
                ui.set_max_width(380.0);
                for (i, t) in self.toasts.iter().enumerate() {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (bar, _) =
                                ui.allocate_exact_size(egui::vec2(4.0, 18.0), egui::Sense::hover());
                            ui.painter().rect_filled(
                                bar,
                                egui::CornerRadius::same(2),
                                t.level.accent(&th),
                            );
                            ui.add(
                                egui::Label::new(egui::RichText::new(&t.text).color(th.text))
                                    .wrap(),
                            );
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new("✕").color(th.dim))
                                        .frame(false),
                                )
                                .clicked()
                            {
                                dismiss = Some(i);
                            }
                        });
                    });
                    ui.add_space(6.0);
                }
            });
        if let Some(i) = dismiss {
            self.dismiss(i);
        }
        // Keep expiry ticking without needing other input, only while a
        // transient toast is live: a sticky one can stand for an hour and
        // must not hold the app at ~8 fps for it.
        if self.toasts.iter().any(|t| t.key.is_none()) {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    pub(crate) fn drain_pending(&mut self, ctx: &egui::Context) {
        let pending = ctx.data_mut(|data| {
            data.remove_temp::<Vec<Pending>>(egui::Id::new("pending_notifications"))
                .unwrap_or_default()
        });
        for op in pending {
            match op {
                Pending::Toast(level, text) => self.push(level, text),
                Pending::Sticky { key, level, text } => self.set_sticky(key, level, text),
                Pending::Clear(key) => self.clear_sticky(&key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_expire_after_their_ttl() {
        let mut n = Notifications::new();
        let t0 = Instant::now();
        n.push(Level::Error, "boom");
        n.prune(t0 + Duration::from_secs(1));
        assert_eq!(n.toasts.len(), 1, "still live within the TTL");
        n.prune(t0 + TTL + Duration::from_secs(1));
        assert_eq!(n.toasts.len(), 0, "dropped past the TTL");
    }

    #[test]
    fn hidden_toast_gets_a_full_ttl_after_its_first_visible_frame() {
        let mut n = Notifications::new();
        let t0 = Instant::now();
        n.push(Level::Error, "arrived while hidden");

        let first_visible = t0 + TTL + Duration::from_secs(10);
        n.prune(first_visible);
        assert_eq!(n.toasts.len(), 1, "first paint starts the toast TTL");

        n.prune(first_visible + TTL + Duration::from_millis(1));
        assert_eq!(n.toasts.len(), 0, "toast expires after a full visible TTL");
    }

    #[test]
    fn set_ttl_prunes_at_the_custom_duration() {
        let mut n = Notifications::new();
        n.set_ttl(Duration::from_secs(1));
        let t0 = Instant::now();
        n.push(Level::Error, "boom");
        n.prune(t0 + Duration::from_millis(500));
        assert_eq!(n.toasts.len(), 1, "still live within the custom TTL");
        n.prune(t0 + Duration::from_secs(2));
        assert_eq!(n.toasts.len(), 0, "dropped past the custom TTL");
    }

    #[test]
    fn duplicate_toasts_are_deduped() {
        let mut n = Notifications::new();
        n.push(Level::Error, "same");
        n.push(Level::Error, "same");
        assert_eq!(n.toasts.len(), 1);
    }

    #[test]
    fn different_levels_are_not_deduped() {
        let mut n = Notifications::new();
        n.push(Level::Error, "x");
        n.push(Level::Warning, "x");
        assert_eq!(n.toasts.len(), 2);
    }

    const KEY: &str = "integration-hold:C:/repo/.git";

    #[test]
    fn a_sticky_toast_outlives_the_ttl() {
        let mut n = Notifications::new();
        let t0 = Instant::now();
        n.set_sticky(KEY, Level::Warning, "held");
        n.prune(t0);
        n.prune(t0 + TTL * 600);
        assert_eq!(n.toasts.len(), 1, "an hour later it is still up");
    }

    #[test]
    fn a_sticky_key_set_again_replaces_its_toast_in_place() {
        let mut n = Notifications::new();
        n.push(Level::Info, "transient");
        n.set_sticky(KEY, Level::Warning, "held: a.rs");
        n.set_sticky(KEY, Level::Warning, "held: a.rs");
        assert_eq!(n.toasts.len(), 2, "the identical text adds no duplicate");
        n.set_sticky(KEY, Level::Warning, "held: a.rs, b.rs");
        assert_eq!(n.toasts.len(), 2, "a new text replaces, never stacks");
        assert_eq!(n.toasts[1].text, "held: a.rs, b.rs");
        n.set_sticky("another-repo", Level::Warning, "held: a.rs");
        assert_eq!(n.toasts.len(), 3, "another key is its own toast");
    }

    #[test]
    fn clearing_a_sticky_removes_it_and_the_same_text_shows_again() {
        let mut n = Notifications::new();
        n.set_sticky(KEY, Level::Warning, "held");
        n.clear_sticky(KEY);
        assert!(n.toasts.is_empty());
        n.clear_sticky(KEY); // clearing an absent key is a no-op
        n.set_sticky(KEY, Level::Warning, "held");
        assert_eq!(n.count_text("held"), 1, "the same hold coming back shows");
    }

    #[test]
    fn a_dismissed_sticky_stays_down_until_its_text_changes_or_it_clears() {
        let mut n = Notifications::new();
        n.set_sticky(KEY, Level::Warning, "held: a.rs");
        n.dismiss(0);
        assert!(n.toasts.is_empty());
        n.set_sticky(KEY, Level::Warning, "held: a.rs");
        assert!(
            n.toasts.is_empty(),
            "re-asserting the dismissed text stays down"
        );
        n.set_sticky(KEY, Level::Warning, "held: b.rs");
        assert!(n.contains_text("b.rs"), "a changed hold is news");

        n.dismiss(0);
        n.clear_sticky(KEY);
        n.set_sticky(KEY, Level::Warning, "held: b.rs");
        assert!(n.contains_text("b.rs"), "clearing forgets the dismissal");
    }

    #[test]
    fn queued_ops_apply_in_order() {
        let ctx = egui::Context::default();
        let mut n = Notifications::new();
        queue(&ctx, Level::Info, "transient");
        queue_sticky(&ctx, KEY, Level::Warning, "held");
        queue_clear_sticky(&ctx, KEY);
        n.drain_pending(&ctx);
        assert!(n.contains_text("transient"));
        assert!(!n.contains_text("held"), "the clear after the set wins");

        queue_clear_sticky(&ctx, KEY);
        queue_sticky(&ctx, KEY, Level::Warning, "held");
        n.drain_pending(&ctx);
        assert!(n.contains_text("held"), "the set after the clear wins");
    }

    fn frame(ctx: &egui::Context, n: &mut Notifications, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| n.show(ui.ctx(), Instant::now()));
    }

    /// Each painted toast's ✕, from last frame's widget rects. The buttons
    /// are the click-only widgets on the notifications layer, apart from the
    /// Area's own (labels sense click-and-drag, for text selection).
    fn close_buttons(ctx: &egui::Context) -> Vec<egui::Pos2> {
        let id = egui::Id::new("notifications");
        let layer = egui::LayerId::new(egui::Order::Tooltip, id);
        ctx.viewport(|vp| {
            vp.prev_pass
                .widgets
                .get_layer(layer)
                .filter(|w| w.id != id.with("move"))
                .filter(|w| w.sense.senses_click() && !w.sense.senses_drag())
                .map(|w| w.rect.center())
                .collect()
        })
    }

    #[test]
    fn clicking_the_close_button_dismisses_a_sticky_toast() {
        let ctx = egui::Context::default();
        let mut n = Notifications::new();
        n.set_sticky(KEY, Level::Warning, "Integration queue waiting");
        frame(&ctx, &mut n, vec![]);
        frame(&ctx, &mut n, vec![]); // the anchored Area settles its size
        let buttons = close_buttons(&ctx);
        assert_eq!(buttons.len(), 1, "one toast, one ✕");
        let at = buttons[0];
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        frame(&ctx, &mut n, vec![egui::Event::PointerMoved(at)]);
        frame(&ctx, &mut n, vec![button(true)]);
        frame(&ctx, &mut n, vec![button(false)]);
        assert!(n.toasts.is_empty(), "the click dismissed it");

        n.set_sticky(KEY, Level::Warning, "Integration queue waiting");
        assert!(n.toasts.is_empty(), "the dismissal holds for the same text");
    }
}
