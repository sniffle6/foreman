//! WCAG 2 contrast audit of a [`Theme`] — a REPORT, never a gate.
//!
//! [`PAIRS`] lists which token sits on which surface and what bar it must clear
//! (4.5:1 for text, 3.0:1 for UI marks). [`audit`] measures every pair with the
//! standard relative-luminance formula and says how many fall below their bar.
//! The Appearance pane shows the count as a chip and the per-token ratios on
//! hover; the Theme Expert sees the failing pairs in its prompt and its cards
//! show the fixes/breaks delta. Nothing here disables anything: a theme may
//! fail every pair and still be applied or saved.
//!
//! Translucent tokens (selection, caret, search washes, the scroll thumb) are
//! composited over `bg` before measuring. Stored bytes are PREMULTIPLIED, so
//! `out = src + dst * (1 - a)`, not the straight-alpha lerp.

use crate::theme::{TOKENS, Theme};
use eframe::egui;

/// What a pair's foreground paints: running text (AA bar 4.5:1) or a UI mark —
/// cursor block, focus border — (3.0:1, WCAG's non-text bar).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Ui,
}

impl Kind {
    /// The minimum ratio this kind must reach.
    pub fn bar(self) -> f64 {
        match self {
            Kind::Text => 4.5,
            Kind::Ui => 3.0,
        }
    }
}

/// Where a pair's colour comes from. Tokens are looked up through
/// [`TOKENS`] by key, so a typo is a test failure, not a wrong colour.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Slot {
    Token(&'static str),
    Palette(usize),
    Chat(usize),
}

impl Slot {
    /// The key as the pane and the theme file spell it.
    pub fn key(self) -> String {
        match self {
            Slot::Token(k) => k.to_string(),
            Slot::Palette(i) => format!("palette[{i}]"),
            Slot::Chat(i) => format!("chat_colors[{i}]"),
        }
    }

    /// The raw stored colour (premultiplied, possibly translucent).
    fn raw(self, t: &Theme) -> Option<egui::Color32> {
        match self {
            Slot::Token(k) => TOKENS.iter().find(|s| s.key == k).map(|s| (s.get)(t)),
            Slot::Palette(i) => t.palette.get(i).copied(),
            Slot::Chat(i) => t.chat_colors.get(i).copied(),
        }
    }

    /// The colour as painted: translucent tokens are composited over `bg`.
    fn resolve(self, t: &Theme) -> egui::Color32 {
        let c = self
            .raw(t)
            .unwrap_or_else(|| panic!("contrast::PAIRS names an unknown slot {self:?}"));
        over(c, t.bg)
    }
}

/// One audited foreground/background relationship.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pair {
    pub fg: Slot,
    pub bg: Slot,
    pub kind: Kind,
}

const fn text(fg: Slot, bg: Slot) -> Pair {
    Pair {
        fg,
        bg,
        kind: Kind::Text,
    }
}

const fn ui(fg: Slot, bg: Slot) -> Pair {
    Pair {
        fg,
        bg,
        kind: Kind::Ui,
    }
}

const BG: Slot = Slot::Token("bg");

/// Every relationship the audit measures. Hover states, the scrim, the snap
/// overlay, the OS-bar `chrome_*` tokens and the plain `border` are deliberately
/// absent: they are transient, decorative, or never carry information alone.
/// `palette[0]` is a background slot, so it is not measured as text.
pub const PAIRS: &[Pair] = &[
    // Terminal text on the grid.
    text(Slot::Token("fg"), BG),
    text(Slot::Token("dim"), BG),
    text(Slot::Palette(1), BG),
    text(Slot::Palette(2), BG),
    text(Slot::Palette(3), BG),
    text(Slot::Palette(4), BG),
    text(Slot::Palette(5), BG),
    text(Slot::Palette(6), BG),
    text(Slot::Palette(7), BG),
    text(Slot::Palette(8), BG),
    text(Slot::Palette(9), BG),
    text(Slot::Palette(10), BG),
    text(Slot::Palette(11), BG),
    text(Slot::Palette(12), BG),
    text(Slot::Palette(13), BG),
    text(Slot::Palette(14), BG),
    text(Slot::Palette(15), BG),
    // UI text on the window surfaces.
    text(Slot::Token("text"), Slot::Token("win_bg")),
    text(Slot::Token("text"), Slot::Token("title_bg")),
    text(Slot::Token("text"), Slot::Token("title_bg_focus")),
    text(Slot::Token("dim"), Slot::Token("title_bg_focus")),
    // Chat.
    text(Slot::Chat(0), BG),
    text(Slot::Chat(1), BG),
    text(Slot::Chat(2), BG),
    text(Slot::Chat(3), BG),
    text(Slot::Chat(4), BG),
    text(Slot::Chat(5), BG),
    text(Slot::Token("chat_live"), BG),
    text(Slot::Token("chat_stale"), BG),
    // A 2 px stroke beside a post, not text: WCAG's non-text bar.
    ui(Slot::Token("chat_edge"), BG),
    // Accents.
    text(Slot::Token("danger"), BG),
    text(Slot::Token("search_error"), BG),
    // Text over the translucent washes (composited over bg).
    text(Slot::Token("fg"), Slot::Token("selection")),
    text(Slot::Token("fg"), Slot::Token("search_current")),
    // UI marks.
    ui(Slot::Token("caret"), BG),
    ui(Slot::Token("border_focus"), BG),
];

/// Where a measured ratio lands against WCAG 2. `Below` is below the pair's
/// bar; `Aa` clears it; `Aaa` clears the enhanced 7:1 text bar (text only —
/// WCAG has no enhanced tier for non-text, so a UI pair tops out at `Aa`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Below,
    Aa,
    Aaa,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Below => "below AA",
            Tier::Aa => "AA",
            Tier::Aaa => "AAA",
        }
    }

    fn of(ratio: f64, kind: Kind) -> Tier {
        if ratio < kind.bar() {
            Tier::Below
        } else if kind == Kind::Text && ratio >= 7.0 {
            Tier::Aaa
        } else {
            Tier::Aa
        }
    }
}

/// One pair's measurement.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Measure {
    pub pair: Pair,
    pub ratio: f64,
    pub tier: Tier,
}

impl Measure {
    pub fn below(&self) -> bool {
        self.tier == Tier::Below
    }

    /// `13.8:1 AAA` — the ratio as the pane prints it.
    pub fn verdict(&self) -> String {
        format!("{} {}", ratio_label(self.ratio), self.tier.label())
    }
}

/// The whole report, in [`PAIRS`] order.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Audit {
    pub measures: Vec<Measure>,
}

impl Audit {
    /// How many pairs fall below their bar.
    pub fn below(&self) -> usize {
        self.measures.iter().filter(|m| m.below()).count()
    }

    /// The pairs below their bar, in table order.
    pub fn failing(&self) -> impl Iterator<Item = &Measure> {
        self.measures.iter().filter(|m| m.below())
    }

    /// Every pair whose foreground is `slot` — what a token's hover lists.
    pub fn with_foreground(&self, slot: Slot) -> impl Iterator<Item = &Measure> {
        self.measures.iter().filter(move |m| m.pair.fg == slot)
    }

    /// The status chip's text: `Contrast: AA` when clean, else the count.
    pub fn summary(&self) -> String {
        match self.below() {
            0 => "Contrast: AA".to_string(),
            n => format!("Contrast: {n} below AA"),
        }
    }

    /// One line per failing pair, for the Theme Expert's prompt and the chip's
    /// hover: `dim on title_bg_focus 4.0:1 (needs 4.5:1)`.
    pub fn failing_lines(&self) -> Vec<String> {
        self.failing()
            .map(|m| {
                format!(
                    "{} on {} {} (needs {})",
                    m.pair.fg.key(),
                    m.pair.bg.key(),
                    ratio_label(m.ratio),
                    ratio_label(m.pair.kind.bar())
                )
            })
            .collect()
    }
}

/// How a proposal moves the audit relative to its base: pairs that were below
/// and now clear (`fixes`) and pairs that cleared and are now below (`breaks`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Delta {
    pub fixes: usize,
    pub breaks: usize,
}

impl Delta {
    pub fn between(base: &Theme, next: &Theme) -> Delta {
        let (a, b) = (audit(base), audit(next));
        let mut d = Delta::default();
        for (before, after) in a.measures.iter().zip(b.measures.iter()) {
            match (before.below(), after.below()) {
                (true, false) => d.fixes += 1,
                (false, true) => d.breaks += 1,
                _ => {}
            }
        }
        d
    }

    pub fn is_zero(self) -> bool {
        self.fixes == 0 && self.breaks == 0
    }

    /// `fixes 2 · breaks 1`; `None` when nothing moved.
    pub fn label(self) -> Option<String> {
        if self.is_zero() {
            None
        } else {
            Some(format!("fixes {} · breaks {}", self.fixes, self.breaks))
        }
    }
}

/// Measure every pair of [`PAIRS`] against `t`.
pub fn audit(t: &Theme) -> Audit {
    let measures = PAIRS
        .iter()
        .map(|&pair| {
            let ratio = ratio(pair.fg.resolve(t), pair.bg.resolve(t));
            Measure {
                pair,
                ratio,
                tier: Tier::of(ratio, pair.kind),
            }
        })
        .collect();
    Audit { measures }
}

/// `13.8:1` — one decimal, as WCAG tools print it.
pub fn ratio_label(ratio: f64) -> String {
    format!("{ratio:.1}:1")
}

/// WCAG 2 contrast ratio of two opaque colours, `1.0..=21.0`, order-independent.
pub fn ratio(a: egui::Color32, b: egui::Color32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// WCAG 2 relative luminance of an opaque sRGB colour (alpha ignored).
pub fn luminance(c: egui::Color32) -> f64 {
    fn lin(v: u8) -> f64 {
        let s = v as f64 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// Composite premultiplied `src` over opaque `dst`: `out = src + dst * (1 - a)`.
/// An opaque `src` is returned unchanged.
pub fn over(src: egui::Color32, dst: egui::Color32) -> egui::Color32 {
    if src.a() == 255 {
        return src;
    }
    let inv = 1.0 - src.a() as f32 / 255.0;
    let ch = |s: u8, d: u8| (s as f32 + d as f32 * inv).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(
        ch(src.r(), dst.r()),
        ch(src.g(), dst.g()),
        ch(src.b(), dst.b()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    fn warm() -> Theme {
        Theme::foreman_warm()
    }

    #[test]
    fn white_on_black_is_twenty_one() {
        assert!((ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 1e-9);
        assert!(
            (ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 1e-9,
            "order-independent"
        );
        assert!((ratio(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn foreman_warm_fg_on_bg_is_about_thirteen_point_eight() {
        let t = warm();
        let r = ratio(t.fg, t.bg);
        assert!((r - 13.8).abs() < 0.1, "fg/bg = {r}");
        let a = audit(&t);
        let m = a
            .with_foreground(Slot::Token("fg"))
            .find(|m| m.pair.bg == BG)
            .unwrap();
        assert_eq!(m.tier, Tier::Aaa);
        assert_eq!(m.verdict(), "13.8:1 AAA");
    }

    #[test]
    fn premultiplied_composite_is_src_plus_dst_times_inverse_alpha() {
        // Premultiplied half-white (128,128,128,128) over black is just the
        // premultiplied rgb; over white it reaches white.
        let half = Color32::from_rgba_premultiplied(128, 128, 128, 128);
        assert_eq!(over(half, Color32::BLACK), Color32::from_rgb(128, 128, 128));
        let w = over(half, Color32::WHITE);
        assert!(w.r() >= 254 && w.g() >= 254 && w.b() >= 254, "{w:?}");
        // A straight-alpha value stores premultiplied bytes, so compositing it
        // over its own dst lands where a straight lerp would.
        let sel = Color32::from_rgba_unmultiplied(231, 231, 231, 70);
        let out = over(sel, Color32::from_rgb(20, 18, 15));
        let lerp = |s: f32, d: f32| (s * 70.0 / 255.0 + d * (1.0 - 70.0 / 255.0)).round() as i32;
        assert!((out.r() as i32 - lerp(231.0, 20.0)).abs() <= 1, "{out:?}");
        assert!((out.g() as i32 - lerp(231.0, 18.0)).abs() <= 1, "{out:?}");
        assert!((out.b() as i32 - lerp(231.0, 15.0)).abs() <= 1, "{out:?}");
        assert_eq!(out.a(), 255);
        // Opaque passes through untouched.
        assert_eq!(over(Color32::RED, Color32::BLUE), Color32::RED);
        // A translucent pair is measured against its composite, never raw.
        let t = warm();
        let on_sel = audit(&t)
            .with_foreground(Slot::Token("fg"))
            .find(|m| m.pair.bg == Slot::Token("selection"))
            .unwrap()
            .ratio;
        assert!((on_sel - ratio(t.fg, over(t.selection, t.bg))).abs() < 1e-9);
        assert!(
            on_sel < ratio(t.fg, t.bg),
            "a light wash lowers fg contrast"
        );
    }

    #[test]
    fn every_pair_slot_is_a_real_key() {
        let t = warm();
        let keys: Vec<&str> = TOKENS.iter().map(|s| s.key).collect();
        for p in PAIRS {
            for slot in [p.fg, p.bg] {
                match slot {
                    Slot::Token(k) => assert!(keys.contains(&k), "{k} is not a TOKENS key"),
                    Slot::Palette(i) => assert!(i < 16, "palette[{i}]"),
                    Slot::Chat(i) => assert!(i < 6, "chat_colors[{i}]"),
                }
                assert!(slot.raw(&t).is_some());
            }
            // Backgrounds are opaque surfaces or washes over bg; never a palette slot as text.
            assert_ne!(p.fg, Slot::Palette(0), "palette[0] is a background slot");
        }
        let mut seen = std::collections::HashSet::new();
        for p in PAIRS {
            assert!(
                seen.insert((p.fg, p.bg)),
                "duplicate pair {:?}",
                (p.fg, p.bg)
            );
        }
        assert_eq!(audit(&t).measures.len(), PAIRS.len());
    }

    /// Snapshot: which built-in pairs miss their bar. The two misses are known
    /// and accepted (`docs/theme-system.md`); the built-in is not retuned to
    /// hide them. A new miss or a vanished one must be a deliberate change.
    #[test]
    fn foreman_warm_failing_pairs_snapshot() {
        let a = audit(&warm());
        let failing: Vec<String> = a
            .failing()
            .map(|m| format!("{} on {}", m.pair.fg.key(), m.pair.bg.key()))
            .collect();
        assert_eq!(failing, ["palette[8] on bg", "dim on title_bg_focus"]);
        assert_eq!(a.below(), 2);
        assert_eq!(a.summary(), "Contrast: 2 below AA");
        let lines = a.failing_lines();
        assert_eq!(lines[0], "palette[8] on bg 3.5:1 (needs 4.5:1)");
        assert_eq!(lines[1], "dim on title_bg_focus 4.0:1 (needs 4.5:1)");
        // The other known baselines.
        let dim = a
            .with_foreground(Slot::Token("dim"))
            .find(|m| m.pair.bg == BG)
            .unwrap();
        assert_eq!(dim.verdict(), "5.8:1 AA");
    }

    #[test]
    fn tiers_follow_kind_bars() {
        assert_eq!(Tier::of(4.49, Kind::Text), Tier::Below);
        assert_eq!(Tier::of(4.5, Kind::Text), Tier::Aa);
        assert_eq!(Tier::of(7.0, Kind::Text), Tier::Aaa);
        assert_eq!(Tier::of(2.99, Kind::Ui), Tier::Below);
        assert_eq!(Tier::of(3.0, Kind::Ui), Tier::Aa);
        assert_eq!(Tier::of(21.0, Kind::Ui), Tier::Aa, "no AAA for non-text");
    }

    #[test]
    fn delta_counts_fixes_and_breaks_against_the_base() {
        let base = warm();
        assert!(Delta::between(&base, &base).is_zero());
        assert_eq!(Delta::between(&base, &base).label(), None);
        let mut next = base.clone();
        next.palette[8] = Color32::from_rgb(170, 165, 150); // bright black readable
        next.fg = Color32::from_rgb(60, 55, 50); // terminal text sinks into bg
        let d = Delta::between(&base, &next);
        assert_eq!(d.fixes, 1);
        // fg on bg, fg on selection, fg on search_current all break.
        assert_eq!(d.breaks, 3);
        assert_eq!(d.label().as_deref(), Some("fixes 1 · breaks 3"));
    }

    #[test]
    fn tokens_that_are_never_a_foreground_have_no_lines() {
        let a = audit(&warm());
        assert_eq!(a.with_foreground(Slot::Token("win_bg")).count(), 0);
        assert_eq!(a.with_foreground(Slot::Token("scrim")).count(), 0);
        assert_eq!(a.with_foreground(Slot::Palette(0)).count(), 0);
        assert_eq!(a.with_foreground(Slot::Token("fg")).count(), 3);
    }
}
