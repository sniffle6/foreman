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

use eframe::egui;
use std::time::{Duration, Instant};

/// How long a toast lingers before it auto-dismisses.
const TTL: Duration = Duration::from_secs(6);

/// Severity — tints the toast's accent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // reusable API: Info/Success/Warning land with future callers
pub enum Level {
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
}

/// The notification center. Owned by `App`; rendered on top of everything.
pub struct Notifications {
    toasts: Vec<Toast>,
    /// How long a toast lingers before it auto-dismisses (settings `toast_secs`);
    /// `TTL` is the default until `App` seeds a live value.
    ttl: Duration,
}

impl Notifications {
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            ttl: TTL,
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
        });
    }

    #[cfg(test)]
    pub(crate) fn contains_text(&self, needle: &str) -> bool {
        self.toasts.iter().any(|toast| toast.text.contains(needle))
    }

    /// Start unpainted toasts and drop ones older than their visible TTL. Pure —
    /// the tested seam.
    fn prune(&mut self, now: Instant) {
        self.toasts.retain_mut(|toast| {
            let visible_since = toast.visible_since.get_or_insert(now);
            now.saturating_duration_since(*visible_since) < self.ttl
        });
    }

    /// Prune expired toasts and paint the rest as a top-right overlay. Call once
    /// per frame, after everything else, so toasts sit on top.
    pub fn show(&mut self, ctx: &egui::Context, now: Instant) {
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
            self.toasts.remove(i);
        }
        // Keep expiry ticking without needing other input (only while live).
        ctx.request_repaint_after(Duration::from_millis(120));
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
}
