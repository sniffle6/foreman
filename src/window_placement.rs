//! Remember the OS main window's position, size and maximized state across
//! restarts (`Settings::window`, file-only in `settings.json`).
//!
//! Coordinates: outer position is stored in PHYSICAL pixels (what `GetWindowRect`
//! reports and what `ViewportCommand::OuterPosition` ultimately applies), the
//! inner size in logical POINTS (what the egui layout sees and what
//! `ViewportBuilder::with_inner_size` / `ViewportCommand::InnerSize` take), plus
//! the pixels-per-point the capture happened at so the position can be turned
//! back into the points `with_position` wants.
//!
//! The pure parts (`capture`, `placement`, `Restore::step`) carry the hazard
//! handling and are table-tested; `monitors()` is the only Win32 call.

use eframe::egui::{self, ViewportInfo};
use serde::{Deserialize, Serialize};

/// Default inner size when nothing (sane) is remembered.
pub const DEFAULT_INNER_SIZE: [f32; 2] = [1280.0, 800.0];

/// Smallest inner size `sanitize` accepts, in points. Below this the chrome
/// buttons do not even fit.
const MIN_SIZE_PT: f32 = 200.0;
/// Largest inner size `sanitize` accepts, in points (a 16k wall is the cap).
const MAX_SIZE_PT: f32 = 16_384.0;
/// Largest |coordinate| `sanitize` accepts, in physical px. Windows' virtual
/// screen is i32 but real desktops are a few tens of thousands px across.
const MAX_COORD_PX: i32 = 1 << 20;

/// How much of the window's top band must land on a monitor's work area for
/// the saved rect to be reused: enough width to grab the drag bar and enough
/// height to see it. Anything less falls back to the default placement.
const MIN_VISIBLE_W: i32 = 160;
const MIN_VISIBLE_H: i32 = 64;

/// Frames the first-frame correction keeps nudging before giving up (the
/// window may legitimately refuse — a size larger than its monitor, say).
const RESTORE_FRAME_BUDGET: u8 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    /// Outer left edge, physical px (screen coordinates; negative is legal on
    /// a monitor left of / above the primary).
    pub x: i32,
    /// Outer top edge, physical px.
    pub y: i32,
    /// Inner width, logical points.
    pub w: f32,
    /// Inner height, logical points.
    pub h: f32,
    /// Pixels-per-point the capture happened at (`x`/`ppp` is the point
    /// position on that monitor).
    pub ppp: f32,
    /// Window was maximized; `x`/`y`/`w`/`h` are the normal rect un-maximize
    /// returns to, not the maximized extent.
    pub maximized: bool,
}

impl WindowGeometry {
    /// Rejects anything a hand-edited file could use to make a 0x0, 1e9 or NaN
    /// window. Position may be negative (multi-monitor); size and scale may not.
    pub fn is_sane(&self) -> bool {
        let finite = self.w.is_finite() && self.h.is_finite() && self.ppp.is_finite();
        finite
            && (MIN_SIZE_PT..=MAX_SIZE_PT).contains(&self.w)
            && (MIN_SIZE_PT..=MAX_SIZE_PT).contains(&self.h)
            && (0.5..=8.0).contains(&self.ppp)
            && self.x.abs() <= MAX_COORD_PX
            && self.y.abs() <= MAX_COORD_PX
    }

    /// Outer position in points on the monitor it was captured on — what
    /// `ViewportBuilder::with_position` takes.
    pub fn position_points(&self) -> egui::Pos2 {
        egui::pos2(self.x as f32 / self.ppp, self.y as f32 / self.ppp)
    }

    pub fn inner_size_points(&self) -> egui::Vec2 {
        egui::vec2(self.w, self.h)
    }

    /// Outer rect in physical px, assuming an undecorated window (outer ==
    /// inner, which is what `with_decorations(false)` gives us).
    fn physical_rect(&self) -> MonitorRect {
        MonitorRect {
            left: self.x,
            top: self.y,
            right: self.x + (self.w * self.ppp).round() as i32,
            bottom: self.y + (self.h * self.ppp).round() as i32,
        }
    }

    /// Equal within capture noise: exact on the integer position and flag,
    /// half a point on the size (a 125% scale makes sizes fractional).
    fn close_to(&self, other: &Self) -> bool {
        self.x == other.x
            && self.y == other.y
            && self.maximized == other.maximized
            && (self.w - other.w).abs() < 0.5
            && (self.h - other.h).abs() < 0.5
            && (self.ppp - other.ppp).abs() < 0.01
    }
}

/// A monitor's usable area in physical px (`rcWork`: minus the taskbar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl MonitorRect {
    fn intersect(&self, o: &MonitorRect) -> (i32, i32) {
        let w = self.right.min(o.right) - self.left.max(o.left);
        let h = self.bottom.min(o.bottom) - self.top.max(o.top);
        (w, h)
    }
}

/// The saved rect is reusable when some monitor shows enough of its top band
/// to grab: the drag bar lives at the top, so a window whose top edge is off
/// every screen is stuck even if its bottom half is visible.
pub fn fits(g: &WindowGeometry, monitors: &[MonitorRect]) -> bool {
    let rect = g.physical_rect();
    let top_band = MonitorRect {
        bottom: rect.top + MIN_VISIBLE_H,
        ..rect
    };
    monitors.iter().any(|m| {
        let (w, h) = m.intersect(&top_band);
        w >= MIN_VISIBLE_W && h >= MIN_VISIBLE_H
    })
}

/// Pure decision: the geometry to restore, or `None` for the default
/// placement (nothing saved, garbage, or no longer on any monitor).
pub fn placement(
    saved: Option<&WindowGeometry>,
    monitors: &[MonitorRect],
) -> Option<WindowGeometry> {
    saved.filter(|g| g.is_sane() && fits(g, monitors)).copied()
}

/// What this frame's viewport says the geometry is, given what was last
/// recorded. `None` = keep what you have (minimized, or nothing to read).
///
/// - Minimized: never sample. winit's `outer_position` is a raw
///   `GetWindowRect`, which reports the -32000,-32000 iconic placement, and
///   `is_maximized` reads false while iconic.
/// - Maximized: only the flag changes; the normal rect is whatever was
///   recorded last. With no prior rect (first run, started maximized) the
///   default size is anchored at the maximized origin so the next launch
///   maximizes on the same monitor.
pub fn capture(info: &ViewportInfo, prev: Option<&WindowGeometry>) -> Option<WindowGeometry> {
    if info.minimized == Some(true) {
        return None;
    }
    let ppp = info.native_pixels_per_point?;
    let outer = info.outer_rect?;
    let maximized = info.maximized == Some(true);
    if maximized {
        return Some(match prev {
            Some(p) => WindowGeometry {
                maximized: true,
                ..*p
            },
            None => WindowGeometry {
                x: (outer.min.x * ppp).round() as i32,
                y: (outer.min.y * ppp).round() as i32,
                w: DEFAULT_INNER_SIZE[0],
                h: DEFAULT_INNER_SIZE[1],
                ppp,
                maximized: true,
            },
        });
    }
    let inner = info.inner_rect?;
    Some(WindowGeometry {
        x: (outer.min.x * ppp).round() as i32,
        y: (outer.min.y * ppp).round() as i32,
        w: inner.width(),
        h: inner.height(),
        ppp,
        maximized: false,
    })
}

/// `true` when `next` should replace `prev` in settings (and mark them dirty).
pub fn changed(prev: Option<&WindowGeometry>, next: &WindowGeometry) -> bool {
    !prev.is_some_and(|p| p.close_to(next))
}

/// First-frame correction after a restore. `ViewportBuilder::with_position`
/// is applied by winit as LOGICAL points scaled by the DPI of wherever
/// Windows first put the window (`CW_USEDEFAULT`), so on a mixed-DPI desktop
/// the builder lands near, not on, the saved rect. This nudges the window to
/// the exact physical rect via `OuterPosition` (points / current ppp) and
/// `InnerSize` until the viewport reports a match, or the budget runs out.
///
/// A maximized target is first checked rather than nudged: the builder's
/// `with_maximized` already maximized it, on whatever monitor the scaled
/// position landed on. If that monitor does not contain the saved normal
/// rect's origin, the window is moved (which un-maximizes it), then
/// re-maximized — so it ends up on the monitor it was closed on.
#[derive(Clone, Debug)]
pub struct Restore {
    target: WindowGeometry,
    frames_left: u8,
    phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Normal window: nudge until the physical rect matches.
    Normal,
    /// Maximized target: wait for the builder's maximize, verify the monitor.
    MaxCheck,
    /// Wrong monitor: the move was sent; wait for the normal rect to match.
    MaxMove,
    /// `Maximize` was sent; wait for the flag.
    MaxConfirm,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Nothing to do this frame (no geometry yet, minimized, or waiting on a
    /// command sent last frame).
    Wait,
    /// Send these two viewport commands.
    Correct {
        outer_pos: egui::Pos2,
        inner_size: egui::Vec2,
    },
    /// Send `ViewportCommand::Maximized(true)`.
    Maximize,
    /// Restore finished (matched, gave up, or not needed); start capturing.
    Done,
}

impl Restore {
    pub fn new(target: WindowGeometry) -> Self {
        Self {
            target,
            frames_left: RESTORE_FRAME_BUDGET,
            phase: if target.maximized {
                Phase::MaxCheck
            } else {
                Phase::Normal
            },
        }
    }

    /// One frame of budget; `true` while there is still some left.
    fn spend(&mut self) -> bool {
        if self.frames_left == 0 {
            return false;
        }
        self.frames_left -= 1;
        true
    }

    fn correct(&self, ppp: f32) -> Step {
        Step::Correct {
            outer_pos: egui::pos2(self.target.x as f32 / ppp, self.target.y as f32 / ppp),
            inner_size: self.target.inner_size_points(),
        }
    }

    pub fn step(&mut self, info: &ViewportInfo) -> Step {
        if info.minimized == Some(true) {
            return Step::Wait;
        }
        let (Some(ppp), Some(outer), Some(inner)) = (
            info.native_pixels_per_point,
            info.outer_rect,
            info.inner_rect,
        ) else {
            return Step::Wait;
        };
        let maximized = info.maximized == Some(true);
        let x = (outer.min.x * ppp).round() as i32;
        let y = (outer.min.y * ppp).round() as i32;
        let rect_matches = (x - self.target.x).abs() <= 1
            && (y - self.target.y).abs() <= 1
            && (inner.width() - self.target.w).abs() <= 1.0
            && (inner.height() - self.target.h).abs() <= 1.0;
        match self.phase {
            Phase::Normal => {
                // The user maximized mid-restore: leave it alone.
                if maximized || rect_matches || !self.spend() {
                    return Step::Done;
                }
                self.correct(ppp)
            }
            Phase::MaxCheck => {
                if !maximized {
                    // The builder's maximize has not landed yet (or never
                    // will): keep waiting a bounded number of frames.
                    return if self.spend() { Step::Wait } else { Step::Done };
                }
                // The maximized extent is the monitor's work area (plus
                // invisible borders): if the saved origin is inside it, this
                // is the right monitor.
                let right = (outer.max.x * ppp).round() as i32;
                let bottom = (outer.max.y * ppp).round() as i32;
                let on_this_monitor =
                    (x..right).contains(&self.target.x) && (y..bottom).contains(&self.target.y);
                if on_this_monitor || !self.spend() {
                    return Step::Done;
                }
                // `set_outer_position` clears winit's MAXIMIZED flag, so
                // this both moves and un-maximizes; re-maximize next.
                self.phase = Phase::MaxMove;
                self.correct(ppp)
            }
            Phase::MaxMove => {
                if maximized {
                    // Last frame's move has not been applied yet.
                    return if self.spend() { Step::Wait } else { Step::Done };
                }
                if rect_matches || !self.spend() {
                    self.phase = Phase::MaxConfirm;
                    return Step::Maximize;
                }
                self.correct(ppp)
            }
            Phase::MaxConfirm => {
                if maximized || !self.spend() {
                    Step::Done
                } else {
                    Step::Wait
                }
            }
        }
    }
}

/// Every monitor's work area in physical px. Must run after winit has made
/// the process per-monitor DPI aware (i.e. inside eframe's `window_builder`
/// hook or later); before that Windows virtualizes the rects to 96 DPI.
#[cfg(windows)]
pub fn monitors() -> Vec<MonitorRect> {
    use windows_sys::Win32::Foundation::{LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    };
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _hdc: HDC,
        _clip: *mut RECT,
        data: LPARAM,
    ) -> windows_sys::core::BOOL {
        // SAFETY: `data` is the `*mut Vec<MonitorRect>` passed below and
        // outlives the synchronous enumeration.
        let out = unsafe { &mut *(data as *mut Vec<MonitorRect>) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcWork: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dwFlags: 0,
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
            let r = info.rcWork;
            out.push(MonitorRect {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            });
        }
        1
    }
    let mut out: Vec<MonitorRect> = Vec::new();
    // SAFETY: null hdc/clip = every monitor; the callback only touches `out`.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(each),
            &mut out as *mut Vec<MonitorRect> as LPARAM,
        );
    }
    out
}

#[cfg(not(windows))]
pub fn monitors() -> Vec<MonitorRect> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(x: i32, y: i32, w: f32, h: f32) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            w,
            h,
            ppp: 1.0,
            maximized: false,
        }
    }

    const PRIMARY: MonitorRect = MonitorRect {
        left: 0,
        top: 0,
        right: 2560,
        bottom: 1400,
    };
    const LEFT: MonitorRect = MonitorRect {
        left: -1920,
        top: 0,
        right: 0,
        bottom: 1080,
    };

    #[test]
    fn placement_table() {
        let mons = [PRIMARY, LEFT];
        let cases: Vec<(&str, WindowGeometry, bool)> = vec![
            ("on primary", geom(100, 100, 1280.0, 800.0), true),
            (
                "on left monitor (negative x)",
                geom(-1800, 50, 1000.0, 700.0),
                true,
            ),
            ("straddling the seam", geom(-500, 100, 1280.0, 800.0), true),
            (
                "unplugged monitor at x=20000",
                geom(20000, 100, 1280.0, 800.0),
                false,
            ),
            (
                "top edge above every screen",
                geom(100, -500, 1280.0, 800.0),
                false,
            ),
            (
                "top band just below the work area",
                geom(100, 1400 - 63, 1280.0, 800.0),
                false,
            ),
            (
                "only a sliver visible on the right",
                geom(2560 - 100, 100, 1280.0, 800.0),
                false,
            ),
            (
                "bottom half visible but top off-screen",
                geom(100, -400, 1280.0, 800.0),
                false,
            ),
            ("garbage size", geom(100, 100, 0.0, 0.0), false),
            ("absurd size", geom(100, 100, 1e9, 1e9), false),
            ("nan size", geom(100, 100, f32::NAN, 800.0), false),
            ("negative size", geom(100, 100, -1280.0, 800.0), false),
            ("absurd position", geom(i32::MAX, 100, 1280.0, 800.0), false),
        ];
        for (name, g, expect) in cases {
            let got = placement(Some(&g), &mons);
            assert_eq!(got.is_some(), expect, "{name}: {g:?}");
            if expect {
                assert_eq!(got, Some(g), "{name}");
            }
        }
        assert_eq!(placement(None, &mons), None);
        assert_eq!(
            placement(Some(&geom(100, 100, 1280.0, 800.0)), &[]),
            None,
            "no monitors"
        );
    }

    #[test]
    fn placement_uses_physical_extent_from_ppp() {
        // 800pt at 2x = 1600px: the top band starts at x=2000 and reaches to
        // 3600, so 560px of it lands on the 2560px primary — visible.
        let g = WindowGeometry {
            ppp: 2.0,
            ..geom(2000, 100, 800.0, 600.0)
        };
        assert!(fits(&g, &[PRIMARY]));
        // Shift it so only 100px of the band is on-screen.
        let g = WindowGeometry { x: 2460, ..g };
        assert!(!fits(&g, &[PRIMARY]));
    }

    #[test]
    fn sanitize_bounds() {
        assert!(geom(0, 0, 200.0, 200.0).is_sane());
        assert!(!geom(0, 0, 199.0, 200.0).is_sane());
        assert!(!geom(0, 0, 200.0, 20_000.0).is_sane());
        assert!(
            !WindowGeometry {
                ppp: 0.0,
                ..geom(0, 0, 800.0, 600.0)
            }
            .is_sane()
        );
        assert!(
            !WindowGeometry {
                ppp: f32::INFINITY,
                ..geom(0, 0, 800.0, 600.0)
            }
            .is_sane()
        );
        assert!(
            geom(-5000, -3000, 800.0, 600.0).is_sane(),
            "negative position is legal"
        );
    }

    fn info(outer: egui::Rect, inner: egui::Rect, ppp: f32) -> ViewportInfo {
        ViewportInfo {
            native_pixels_per_point: Some(ppp),
            outer_rect: Some(outer),
            inner_rect: Some(inner),
            minimized: Some(false),
            maximized: Some(false),
            ..Default::default()
        }
    }

    #[test]
    fn capture_normal_window_is_physical_pos_and_point_size() {
        let r = egui::Rect::from_min_size(egui::pos2(80.0, 40.0), egui::vec2(1000.0, 700.0));
        let g = capture(&info(r, r, 1.25), None).unwrap();
        assert_eq!((g.x, g.y), (100, 50));
        assert_eq!((g.w, g.h), (1000.0, 700.0));
        assert_eq!(g.ppp, 1.25);
        assert!(!g.maximized);
    }

    #[test]
    fn capture_never_samples_while_minimized() {
        let r = egui::Rect::from_min_size(egui::pos2(-32000.0, -32000.0), egui::vec2(160.0, 28.0));
        let mut i = info(r, r, 1.0);
        i.minimized = Some(true);
        assert_eq!(capture(&i, None), None);
        let prev = geom(100, 100, 1280.0, 800.0);
        assert_eq!(
            capture(&i, Some(&prev)),
            None,
            "keep the previous rect untouched"
        );
    }

    #[test]
    fn capture_maximized_keeps_normal_rect_and_sets_flag() {
        let full = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(2560.0, 1400.0));
        let mut i = info(full, full, 1.0);
        i.maximized = Some(true);
        let prev = geom(100, 100, 1280.0, 800.0);
        let g = capture(&i, Some(&prev)).unwrap();
        assert_eq!(
            g,
            WindowGeometry {
                maximized: true,
                ..prev
            }
        );
        // Un-maximize: the flag drops and the live rect is read again.
        i.maximized = Some(false);
        let back = capture(&i, Some(&g)).unwrap();
        assert!(!back.maximized);
        assert_eq!((back.w, back.h), (2560.0, 1400.0));
    }

    #[test]
    fn capture_maximized_with_nothing_remembered_anchors_default_size() {
        let full = egui::Rect::from_min_size(egui::pos2(-1920.0, 0.0), egui::vec2(1920.0, 1040.0));
        let mut i = info(full, full, 1.0);
        i.maximized = Some(true);
        let g = capture(&i, None).unwrap();
        assert!(g.maximized);
        assert_eq!((g.x, g.y), (-1920, 0), "anchored on the maximized monitor");
        assert_eq!([g.w, g.h], DEFAULT_INNER_SIZE);
    }

    #[test]
    fn changed_ignores_sub_point_jitter() {
        let a = geom(100, 100, 1000.8, 700.0);
        let b = geom(100, 100, 1001.0, 700.0);
        assert!(!changed(Some(&a), &b));
        assert!(changed(Some(&a), &geom(101, 100, 1000.8, 700.0)));
        assert!(changed(None, &a));
        assert!(changed(
            Some(&a),
            &WindowGeometry {
                maximized: true,
                ..a
            }
        ));
    }

    #[test]
    fn restore_corrects_until_matched_then_is_done() {
        let target = WindowGeometry {
            ppp: 1.5,
            ..geom(300, 200, 1000.0, 700.0)
        };
        let mut r = Restore::new(target);
        // Window came up where CW_USEDEFAULT + a 1.0 builder scale put it.
        let off = egui::Rect::from_min_size(egui::pos2(100.0, 80.0), egui::vec2(1000.0, 700.0));
        match r.step(&info(off, off, 1.5)) {
            Step::Correct {
                outer_pos,
                inner_size,
            } => {
                assert_eq!(outer_pos, egui::pos2(200.0, 200.0 / 1.5));
                assert_eq!(inner_size, egui::vec2(1000.0, 700.0));
            }
            other => panic!("expected Correct, got {other:?}"),
        }
        // Now it reports the target (in points at 1.5x): 300/1.5, 200/1.5.
        let on = egui::Rect::from_min_size(egui::pos2(200.0, 133.3333), egui::vec2(1000.0, 700.0));
        assert_eq!(r.step(&info(on, on, 1.5)), Step::Done);
    }

    #[test]
    fn restore_gives_up_after_budget() {
        let mut r = Restore::new(geom(300, 200, 1000.0, 700.0));
        let off = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 700.0));
        let i = info(off, off, 1.0);
        for _ in 0..RESTORE_FRAME_BUDGET {
            assert!(matches!(r.step(&i), Step::Correct { .. }));
        }
        assert_eq!(r.step(&i), Step::Done);
    }

    #[test]
    fn restore_waits_while_minimized_and_yields_to_a_user_maximize() {
        let mut r = Restore::new(geom(300, 200, 1000.0, 700.0));
        let off = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 700.0));
        let mut i = info(off, off, 1.0);
        i.minimized = Some(true);
        assert_eq!(r.step(&i), Step::Wait);
        i.minimized = Some(false);
        i.maximized = Some(true);
        assert_eq!(
            r.step(&i),
            Step::Done,
            "user maximized mid-restore: leave it"
        );
    }

    #[test]
    fn restore_maximized_on_the_right_monitor_is_done_once_the_flag_lands() {
        let mut r = Restore::new(WindowGeometry {
            maximized: true,
            ..geom(300, 200, 1000.0, 700.0)
        });
        let full = egui::Rect::from_min_size(egui::pos2(-8.0, -8.0), egui::vec2(2576.0, 1416.0));
        let mut i = info(full, full, 1.0);
        // The builder's maximize has not been reported yet: wait, don't capture.
        assert_eq!(r.step(&i), Step::Wait);
        i.maximized = Some(true);
        assert_eq!(r.step(&i), Step::Done);
    }

    #[test]
    fn restore_maximized_on_the_wrong_monitor_moves_then_remaximizes() {
        // Saved on a 1.5x secondary at x=3000; CW_USEDEFAULT put the window on
        // the 1.0x primary (0..2560) and with_maximized maximized it THERE.
        let target = WindowGeometry {
            maximized: true,
            ppp: 1.5,
            ..geom(3000, 100, 1000.0, 700.0)
        };
        let mut r = Restore::new(target);
        let primary = egui::Rect::from_min_size(egui::pos2(-8.0, -8.0), egui::vec2(2576.0, 1416.0));
        let mut i = info(primary, primary, 1.0);
        i.maximized = Some(true);
        match r.step(&i) {
            Step::Correct { outer_pos, .. } => assert_eq!(outer_pos, egui::pos2(3000.0, 100.0)),
            other => panic!("expected a move, got {other:?}"),
        }
        // Next frame still reports the old state: wait, don't re-send.
        assert_eq!(r.step(&i), Step::Wait);
        // The move landed (now un-maximized, on the 1.5x monitor).
        let moved =
            egui::Rect::from_min_size(egui::pos2(2000.0, 66.6667), egui::vec2(1000.0, 700.0));
        let mut i = info(moved, moved, 1.5);
        i.maximized = Some(false);
        assert_eq!(r.step(&i), Step::Maximize);
        assert_eq!(r.step(&i), Step::Wait, "flag not reported yet");
        i.maximized = Some(true);
        assert_eq!(r.step(&i), Step::Done);
    }

    #[test]
    fn restore_maximized_gives_up_if_the_flag_never_lands() {
        let mut r = Restore::new(WindowGeometry {
            maximized: true,
            ..geom(300, 200, 1000.0, 700.0)
        });
        let off = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 700.0));
        let i = info(off, off, 1.0);
        for _ in 0..RESTORE_FRAME_BUDGET {
            assert_eq!(r.step(&i), Step::Wait);
        }
        assert_eq!(r.step(&i), Step::Done);
    }
}
