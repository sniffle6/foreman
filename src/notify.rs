//! A small, reusable notification center: transient toasts stacked top-right,
//! auto-dismissed after a TTL, colored by severity. The queue lifecycle is a
//! pure seam (`push` + `prune`) tested without a GUI; `show` prunes then paints.
//!
//! Callers learn two methods and one enum:
//! ```ignore
//! app.notify.push(notify::Level::Error, "Claude isn't installed", now);
//! // once per frame, on top of everything:
//! app.notify.show(ctx, now);
//! ```

use eframe::egui;
use std::time::{Duration, Instant};

use crate::theme::{BORDER_FOCUS, DANGER, DIM, TEXT};

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
    fn accent(self) -> egui::Color32 {
        match self {
            Level::Error | Level::Warning => DANGER,
            Level::Info | Level::Success => BORDER_FOCUS,
        }
    }
}

struct Toast {
    level: Level,
    text: String,
    born: Instant,
}

/// The notification center. Owned by `App`; rendered on top of everything.
pub struct Notifications {
    toasts: Vec<Toast>,
}

impl Notifications {
    pub fn new() -> Self {
        Self { toasts: Vec::new() }
    }

    /// Queue a toast. Deduped by (level, text) so a repeated error doesn't
    /// stack. `now` is injected (not read from the clock) so expiry is testable.
    pub fn push(&mut self, level: Level, text: impl Into<String>, now: Instant) {
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
            born: now,
        });
    }

    /// Drop toasts older than the TTL. Pure — the tested seam.
    fn prune(&mut self, now: Instant) {
        self.toasts
            .retain(|t| now.saturating_duration_since(t.born) < TTL);
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
                ui.set_max_width(380.0);
                for (i, t) in self.toasts.iter().enumerate() {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (bar, _) =
                                ui.allocate_exact_size(egui::vec2(4.0, 18.0), egui::Sense::hover());
                            ui.painter().rect_filled(
                                bar,
                                egui::CornerRadius::same(2),
                                t.level.accent(),
                            );
                            ui.add(
                                egui::Label::new(egui::RichText::new(&t.text).color(TEXT)).wrap(),
                            );
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new("✕").color(DIM))
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
        n.push(Level::Error, "boom", t0);
        n.prune(t0 + Duration::from_secs(1));
        assert_eq!(n.toasts.len(), 1, "still live within the TTL");
        n.prune(t0 + TTL + Duration::from_secs(1));
        assert_eq!(n.toasts.len(), 0, "dropped past the TTL");
    }

    #[test]
    fn duplicate_toasts_are_deduped() {
        let mut n = Notifications::new();
        let t0 = Instant::now();
        n.push(Level::Error, "same", t0);
        n.push(Level::Error, "same", t0 + Duration::from_millis(10));
        assert_eq!(n.toasts.len(), 1);
    }

    #[test]
    fn different_levels_are_not_deduped() {
        let mut n = Notifications::new();
        let t0 = Instant::now();
        n.push(Level::Error, "x", t0);
        n.push(Level::Warning, "x", t0);
        assert_eq!(n.toasts.len(), 2);
    }
}
