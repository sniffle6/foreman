use crate::dirpicker::{DirPicker, Outcome};
use crate::keymap::{Chord, Command, Keymap};
use crate::settings::{Outcome as SettingsOutcome, SettingsView};
use crate::terminal::{Session, Shell};
use eframe::egui;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub type WinId = u64;

const DESK_BG: egui::Color32 = egui::Color32::from_rgb(25, 23, 19);
const WIN_BG: egui::Color32 = egui::Color32::from_rgb(33, 30, 24);
const TITLE_BG: egui::Color32 = egui::Color32::from_rgb(43, 39, 31);
const TITLE_BG_FOCUS: egui::Color32 = egui::Color32::from_rgb(56, 49, 36);
// Project windows (the outer nesting level) get a subtly cooler, deeper titlebar
// so the two levels read as distinct without breaking the warm-graphite look.
const PROJ_TITLE_BG: egui::Color32 = egui::Color32::from_rgb(37, 41, 39);
const PROJ_TITLE_BG_FOCUS: egui::Color32 = egui::Color32::from_rgb(48, 56, 52);
const BORDER: egui::Color32 = egui::Color32::from_rgb(60, 55, 45);
const BORDER_FOCUS: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);
// The focused project gets a punchier, more saturated amber than focused terminals
// so the selected project reads at a glance even with thin borders.
const PROJ_BORDER_FOCUS: egui::Color32 = egui::Color32::from_rgb(150, 107, 28);
const BORDER_W: f32 = 0.75; // uniform window border width; focus is shown by colour
const TEXT: egui::Color32 = egui::Color32::from_rgb(222, 222, 212);
const DIM: egui::Color32 = egui::Color32::from_rgb(150, 143, 125);

// Chat viewer palette. Sender colors are assigned by terminal-id hash —
// stable for a given id, distinct enough across a small fleet.
const CHAT_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(231, 169, 63), // amber (also the human "you")
    egui::Color32::from_rgb(127, 179, 127), // green
    egui::Color32::from_rgb(111, 167, 199), // blue
    egui::Color32::from_rgb(199, 127, 174), // pink
    egui::Color32::from_rgb(180, 160, 100), // sand
    egui::Color32::from_rgb(140, 170, 160), // sage
];
const CHAT_STALE: egui::Color32 = egui::Color32::from_rgb(202, 164, 90);
const CHAT_LIVE: egui::Color32 = egui::Color32::from_rgb(127, 179, 127);
const CHAT_EDGE: egui::Color32 = egui::Color32::from_rgb(150, 107, 28);
const CHAT_MENTION_BG: egui::Color32 = egui::Color32::from_rgb(69, 64, 47);
const CHAT_BOARD_W: f32 = 160.0;
const CHAT_BOARD_MIN_W: f32 = 480.0; // window narrower than this hides the board

fn chat_color(id: &str) -> egui::Color32 {
    if id == "you" {
        return CHAT_COLORS[0];
    }
    let n: u64 = id.trim_start_matches('t').parse().unwrap_or(0);
    CHAT_COLORS[(n as usize) % CHAT_COLORS.len()]
}

const TITLE_H: f32 = 26.0;

// Leader (prefix) key — tmux-style. After it is pressed the next chord is a
// *command* (consumed, never sent to the PTY). The leader is now data-driven:
// it lives in `Keymap::leader` (default `Ctrl+b`), loaded from
// `%APPDATA%\foreman\keybindings.json` and overridable per user.

const RESIZE_BAND: f32 = 6.0; // thickness of the invisible edge/corner resize hit-zones
const MIN_W: f32 = 240.0; // smallest a floating window may be dragged to
const MIN_H: f32 = 140.0;

// snap overlay (amber, matches BORDER_FOCUS / web mockup --needs #e7a93f)
const SNAP_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(231, 169, 63, 33); // ~13% alpha
const SNAP_STROKE: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);
const SNAP_GAP: f32 = 0.0; // inset of zones from the area edge; 0 = windows tile edge-to-edge

// A cardinal direction for directional focus / snap commands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

// True once a dragged tab chip has left its window's titlebar far enough to count
// as a drag-out (untab): well below/above the title row, or past either side edge.
// Shared by the live drag-out path and the release fallback so both agree.
fn tab_drag_off(p: egui::Pos2, scr: egui::Rect) -> bool {
    (p.y - scr.min.y).abs() > TITLE_H * 1.5 || p.x < scr.min.x || p.x > scr.max.x
}


pub enum Content {
    Terminal(Session),
    /// A project window is a sandbox hosting its own nested WindowManager.
    Project(Box<WindowManager>),
    /// Read-only viewer of the owning project's chat room. Carries per-window
    /// view state; shares the log via Rc — a viewer, not a member: never
    /// injected into (spec §4).
    Chat(crate::chat::ChatView),
}
impl Content {
    /// Returns whether a window in this content was interacted with this frame.
    /// Terminals are leaves (no child windows) so they always return false; a
    /// project returns whatever its nested manager reports, which lets the parent
    /// raise focus to a background project when one of its sub-windows is clicked.
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        base: egui::Id,
        win_id: WinId,
        resp: &egui::Response,
    ) -> bool {
        match self {
            Content::Terminal(s) => {
                s.show(ui, rect, active, resp);
                false
            }
            // Recurse: the project's content rect becomes the child manager's area.
            // The child only reads the keyboard if this project is itself active,
            // so `active` ANDs down the tree to exactly one leaf terminal.
            Content::Project(wm) => wm.show(ui, rect, active, base.with(("proj", win_id))),
            Content::Chat(view) => {
                // Reserve the input strip up front and shrink the working
                // rect so the board/log lay out above it. The painter keeps
                // the FULL rect (it must draw the strip chrome too).
                const INPUT_H: f32 = 32.0;
                let input_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.min.x, rect.max.y - INPUT_H),
                    rect.max,
                );
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 0.0, WIN_BG);
                let rect =
                    egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, input_rect.min.y));
                let pad = 8.0;
                let meta_font = egui::FontId::proportional(11.0);
                let body_font = egui::FontId::proportional(12.5);
                let compact = rect.width() < CHAT_BOARD_MIN_W;

                // ---- crew board (comfortable widths only) ----
                let mut log_left = rect.min.x;
                if !compact {
                    let board = egui::Rect::from_min_max(
                        rect.min,
                        egui::pos2(rect.min.x + CHAT_BOARD_W, rect.max.y),
                    );
                    log_left = board.max.x;
                    p.line_segment(
                        [
                            egui::pos2(board.max.x, rect.min.y),
                            egui::pos2(board.max.x, rect.max.y),
                        ],
                        egui::Stroke::new(1.0, BORDER),
                    );
                    p.text(
                        egui::pos2(board.min.x + pad, board.min.y + pad),
                        egui::Align2::LEFT_TOP,
                        "CREW · BY LAST HEARD",
                        egui::FontId::proportional(9.5),
                        DIM,
                    );
                    let now = std::time::SystemTime::now();
                    let row_h = 20.0;
                    let mut y = board.min.y + pad + 16.0;
                    for r in &view.crew {
                        let row = egui::Rect::from_min_size(
                            egui::pos2(board.min.x + 4.0, y),
                            egui::vec2(board.width() - 8.0, row_h),
                        );
                        let hovered =
                            resp.hovered() && resp.hover_pos().is_some_and(|p| row.contains(p));
                        if hovered {
                            p.rect_filled(row, 3.0, TITLE_BG);
                        }
                        if hovered && resp.clicked() {
                            view.click = Some((r.win, r.tab));
                        }
                        let dot = if r.exited { BORDER } else { CHAT_LIVE };
                        p.circle_filled(egui::pos2(row.min.x + 7.0, row.center().y), 3.0, dot);
                        let name_col = if r.exited { DIM } else { chat_color(&r.id) };
                        // The pane identity has name == id ("you") — a bare
                        // label beats the silly-looking "you · you".
                        let label = if r.name == r.id {
                            r.name.clone()
                        } else {
                            format!("{} · {}", r.name, r.id)
                        };
                        let (age, stale) = if r.exited {
                            ("exited".to_string(), false)
                        } else {
                            match r.last.and_then(|t| now.duration_since(t).ok()) {
                                Some(d) => crate::chat::age_label(d),
                                None => ("—".to_string(), false),
                            }
                        };
                        // Age paints first so the label can truncate into the
                        // space that's left — an unconstrained p.text label runs
                        // straight under the age column on long tab titles.
                        let age_rect = p.text(
                            egui::pos2(row.max.x - 4.0, row.center().y),
                            egui::Align2::RIGHT_CENTER,
                            age,
                            egui::FontId::proportional(10.5),
                            if stale { CHAT_STALE } else { DIM },
                        );
                        let label_x = row.min.x + 16.0;
                        let mut job = egui::text::LayoutJob::simple_singleline(
                            label,
                            egui::FontId::proportional(11.5),
                            name_col,
                        );
                        job.wrap = egui::text::TextWrapping::truncate_at_width(
                            (age_rect.min.x - 6.0 - label_x).max(0.0),
                        );
                        let g = p.layout_job(job);
                        p.galley(
                            egui::pos2(label_x, row.center().y - g.size().y * 0.5),
                            g,
                            name_col,
                        );
                        y += row_h;
                        if y + row_h > board.max.y {
                            break; // board overflow: clip; the log is the priority
                        }
                    }
                }

                // ---- log: layout pass (galleys + heights), then paint ----
                let log_rect = egui::Rect::from_min_max(
                    egui::pos2(log_left + pad, rect.min.y + pad),
                    egui::pos2(rect.max.x - pad, rect.max.y - pad),
                );
                let wrap = (log_rect.width() - 10.0).max(40.0);
                // Borrow stays scoped — never held across recursion into other
                // windows' show() (post paths borrow_mut the log).
                let blocks = {
                    let log = view.log.borrow();
                    crate::chat::build_blocks(log.msgs(), view.last_seen, compact)
                };
                enum Painted {
                    Galley(
                        std::sync::Arc<egui::Galley>,
                        egui::Color32,
                        f32,  /*indent*/
                        bool, /*edge*/
                    ),
                    Centered(std::sync::Arc<egui::Galley>),
                    MetaPair(std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>),
                    Rule(Option<std::sync::Arc<egui::Galley>>),
                    Gap(f32),
                }
                let mut items: Vec<Painted> = Vec::new();
                let mut total = 0.0f32;
                for b in &blocks {
                    match b {
                        crate::chat::ChatBlock::Sys(s) => {
                            let g = p.layout(s.clone(), meta_font.clone(), DIM, wrap);
                            total += g.size().y + 6.0;
                            items.push(Painted::Centered(g));
                            items.push(Painted::Gap(6.0));
                        }
                        crate::chat::ChatBlock::Divider => {
                            let g = p.layout(
                                "NEW".into(),
                                egui::FontId::proportional(9.0),
                                CHAT_STALE,
                                wrap,
                            );
                            total += 14.0;
                            items.push(Painted::Rule(Some(g)));
                        }
                        crate::chat::ChatBlock::Header { name, id, meta } => {
                            let gn = p.layout_no_wrap(
                                name.clone(),
                                egui::FontId::proportional(12.0),
                                chat_color(id),
                            );
                            let gm = p.layout_no_wrap(meta.clone(), meta_font.clone(), DIM);
                            total += gn.size().y + 2.0 + 4.0; // header + breathing room above
                            items.push(Painted::Gap(4.0));
                            items.push(Painted::MetaPair(gn, gm));
                        }
                        crate::chat::ChatBlock::Text { text, to } => {
                            // Mention chips: lay the body out as a LayoutJob so
                            // @tokens get their own colored sections inline.
                            let mut job = egui::text::LayoutJob::default();
                            job.wrap.max_width = wrap;
                            for (i, word) in text.split(' ').enumerate() {
                                let lead = if i == 0 { "" } else { " " };
                                let (col, bg) = if word.starts_with('@') && word.len() > 1 {
                                    (CHAT_COLORS[0], CHAT_MENTION_BG)
                                } else {
                                    (TEXT, egui::Color32::TRANSPARENT)
                                };
                                job.append(
                                    &format!("{lead}{word}"),
                                    0.0,
                                    egui::text::TextFormat {
                                        font_id: body_font.clone(),
                                        color: col,
                                        background: bg,
                                        ..Default::default()
                                    },
                                );
                            }
                            let g = p.layout_job(job);
                            total += g.size().y + 2.0;
                            items.push(Painted::Galley(
                                g,
                                TEXT,
                                if to.is_empty() { 0.0 } else { 10.0 },
                                !to.is_empty(),
                            ));
                            items.push(Painted::Gap(2.0));
                        }
                    }
                }

                // Scroll: stick-to-bottom by default; a wheel-up unsticks and
                // the view then holds its CONTENT position while new messages
                // arrive (autoscroll paused); wheeling back to the bottom
                // re-sticks. Offset is measured from the top so an unstuck
                // view doesn't slide as `total` grows.
                let max = (total - log_rect.height()).max(0.0);
                if resp.hovered() {
                    let dy = ui.input(|i| i.smooth_scroll_delta.y);
                    if dy != 0.0 {
                        let cur = if view.stick { max } else { view.scroll };
                        view.scroll = (cur - dy).clamp(0.0, max);
                        view.stick = view.scroll >= max - 1.0;
                    }
                }
                let offset = if view.stick {
                    max
                } else {
                    view.scroll.min(max)
                };
                let mut y = log_rect.min.y - offset;
                for it in items {
                    match it {
                        Painted::Gap(h) => y += h,
                        Painted::Centered(g) => {
                            let h = g.size().y;
                            let x = log_rect.center().x - g.size().x / 2.0;
                            p.galley(egui::pos2(x, y), g, DIM);
                            y += h;
                        }
                        Painted::Rule(label) => {
                            let mid = y + 7.0;
                            // The rule is intentionally dim (mockup: 1px #45402f); the amber NEW label is the affordance.
                            p.line_segment(
                                [
                                    egui::pos2(log_rect.min.x, mid),
                                    egui::pos2(log_rect.max.x, mid),
                                ],
                                egui::Stroke::new(1.0, CHAT_MENTION_BG),
                            );
                            if let Some(g) = label {
                                let w = g.size().x;
                                let lx = log_rect.center().x - w / 2.0;
                                p.rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(lx - 4.0, y),
                                        egui::vec2(w + 8.0, 14.0),
                                    ),
                                    0.0,
                                    WIN_BG,
                                );
                                p.galley(egui::pos2(lx, y + 1.0), g, CHAT_STALE);
                            }
                            y += 14.0;
                        }
                        Painted::MetaPair(gn, gm) => {
                            let h = gn.size().y;
                            let nw = gn.size().x;
                            p.galley(egui::pos2(log_rect.min.x, y), gn, TEXT);
                            p.galley(egui::pos2(log_rect.min.x + nw + 6.0, y + 1.5), gm, DIM);
                            y += h + 2.0;
                        }
                        Painted::Galley(g, col, indent, edge) => {
                            let h = g.size().y;
                            if edge {
                                p.line_segment(
                                    [
                                        egui::pos2(log_rect.min.x + 2.0, y),
                                        egui::pos2(log_rect.min.x + 2.0, y + h),
                                    ],
                                    egui::Stroke::new(2.0, CHAT_EDGE),
                                );
                            }
                            p.galley(egui::pos2(log_rect.min.x + indent, y), g, col);
                            y += h;
                        }
                    }
                }
                view.on_frame(active);

                // ---- input strip (slice 2): the human posts from here ----
                // Repaint the strip ground first: the painter's clip spans
                // the full window, so a partially-scrolled log line can bleed
                // under the strip.
                p.rect_filled(input_rect, 0.0, WIN_BG);
                p.line_segment(
                    [
                        input_rect.min,
                        egui::pos2(input_rect.max.x, input_rect.min.y),
                    ],
                    egui::Stroke::new(1.0, BORDER),
                );
                let te_rect = input_rect.shrink2(egui::vec2(8.0, 5.0));
                p.rect_filled(te_rect, egui::CornerRadius::same(3), DESK_BG);
                p.rect_stroke(
                    te_rect,
                    egui::CornerRadius::same(3),
                    egui::Stroke::new(1.0, BORDER),
                    egui::StrokeKind::Inside,
                );
                ui.visuals_mut().selection.bg_fill =
                    egui::Color32::from_rgba_unmultiplied(231, 169, 63, 90);
                let te = ui.put(
                    te_rect,
                    egui::TextEdit::singleline(&mut view.input)
                        .id(base.with((win_id, "chat-input")))
                        .font(egui::FontId::proportional(12.5))
                        .text_color(TEXT)
                        .hint_text("Message…")
                        .vertical_align(egui::Align::Center)
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::symmetric(6, 0))
                        .desired_width(te_rect.width()),
                );
                if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    view.pending_post = Some(std::mem::take(&mut view.input));
                    te.request_focus(); // keep typing; multi-post sessions are the norm
                // Escape defocuses the field at frame start (egui Focus::begin_pass),
                // so detect it as lost_focus + Escape — has_focus() is already false here.
                } else if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    view.input.clear();
                }
                false
            }
        }
    }

    /// Keep this content alive while it is an *inactive* tab (not rendered this
    /// frame). A terminal drains its PTY (answering startup device queries and
    /// buffering output); a project recurses so every nested terminal stays alive
    /// too. Pure book-keeping — no rendering, no input.
    fn keepalive(&mut self) {
        match self {
            Content::Terminal(s) => s.keepalive(),
            Content::Project(wm) => wm.keepalive(),
            Content::Chat(_) => {} // no PTY; the log is shared state, nothing to pump
        }
    }
}

/// One entry in a window's tab-stack: a title and the content it shows. The
/// per-tab title lives here (a window no longer has a single title); the active
/// tab's title is what the titlebar renders and what rename targets.
pub struct Tab {
    pub title: String,
    pub content: Content,
    /// Member of this project's chat room (spec: agent-group-chat §2).
    /// Dispatched terminals auto-join; others join on first post. Lives on
    /// the Tab so membership travels with its terminal through tab
    /// merges/untabs. Sender identity still resolves via Win id (active
    /// tab) — same staleness family as terminal-id resolution.
    pub chat_member: bool,
}

pub struct Win {
    pub id: WinId,
    /// The stack of tabs this window holds. Invariant: never empty — closing the
    /// last tab closes the window. A len-1 stack renders exactly like a classic
    /// single-content window (no tab bar drawn).
    pub tabs: Vec<Tab>,
    /// Index into `tabs` of the active (rendered + keyboard-owning) tab.
    pub active: usize,
    pub rect: egui::Rect, // local coords (origin = manager area.min)
    pub z: u64,
    pub minimized: bool,
    pub prev: Option<egui::Rect>, // floating rect to restore when un-tiled/un-zoomed
}

impl Win {
    /// The active tab's title (what the titlebar shows and rename edits).
    pub fn title(&self) -> &str {
        &self.tabs[self.active].title
    }
    /// Mutable handle to the active tab's content.
    fn active_content(&mut self) -> &mut Content {
        &mut self.tabs[self.active].content
    }
    /// Is the active tab a project? (Drives titlebar styling + the +project key.)
    fn is_project(&self) -> bool {
        matches!(self.tabs[self.active].content, Content::Project(_))
    }
    /// Pump every tab that is *not* the active one so backgrounded PTYs stay alive.
    fn keepalive_inactive(&mut self) {
        let active = self.active;
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if i != active {
                t.content.keepalive();
            }
        }
    }
}

// The resolved-command type lives in `keymap.rs` as `Command` (data-driven in
// Phase 2). Terminal-level variants act on the focused project's child manager;
// project-level variants act on the desktop.

enum Act {
    Focus(WinId),
    Close(WinId),
    Min(WinId),
    Max(WinId),
    Restore(WinId),
    /// Dispatch a terminal of `Shell` into project window `WinId`. Deferred like
    /// the rest: the header key is drawn mid-loop, but reaching into the project's
    /// nested manager has to wait until after the render borrow is released.
    AddTerm(WinId, Shell),
    /// Open the directory picker to create a new sibling project on the desktop.
    /// Fired by the "+" on a project titlebar; the actual project is created when
    /// the user accepts a directory in the picker.
    OpenProjectPicker,
    /// Switch window `WinId` to tab index `usize` (tab-bar click).
    SetTab(WinId, usize),
    /// Close tab index `usize` of window `WinId` (tab-bar close affordance).
    CloseTab(WinId, usize),
    /// Merge the source window's tabs onto the target window's stack, then remove
    /// the source. Fired when a window's titlebar is dropped onto another window.
    Merge {
        src: WinId,
        dst: WinId,
    },
    /// Detach tab `idx` of window `id` into a new floating window at `pos` (local).
    /// `grab` transfers the in-progress pointer drag onto the new window's title so
    /// it keeps following the cursor (live drag-out); set false for a drop-release
    /// detach where no drag continues.
    Untab {
        id: WinId,
        idx: usize,
        pos: egui::Pos2,
        grab: bool,
    },
}

/// What a validated chat request resolved to. Posting is split from injection
/// so the reply can be sent between the two (spec §3).
enum ChatOutcome {
    Posted {
        pid: WinId,
        from: WinId,
        framed: String,
        /// `None` = broadcast; `Some` = deliver only to these windows
        /// (`you` already filtered out — a pure-@you post is `Some(vec![])`).
        targets: Option<Vec<WinId>>,
        /// The posted message's seq — returned to the sender as its ack handle.
        seq: Option<u64>,
    },
    History(Vec<String>),
}

pub struct WindowManager {
    pub windows: Vec<Win>,
    z: u64,
    focused: Option<WinId>,
    next: WinId,
    /// Working directory new terminals in this manager spawn into. `None` on the
    /// desktop (process cwd); `Some` on a project, set when the project is created.
    cwd: Option<PathBuf>,
    /// Stable id string ("p3") when this manager is a project's child manager;
    /// env-injected into its terminals so dispatchers can self-target. None on
    /// the desktop.
    tag: Option<String>,
    /// This project's chat room (unused at desktop level). Shared with the
    /// viewer window (`Content::Chat`), hence the Rc<RefCell<…>>.
    pub chat: Rc<RefCell<crate::chat::ChatLog>>,
    /// When `Some`, the directory picker modal is open (desktop only). Opening it
    /// defers project creation until the user accepts a directory.
    picker: Option<DirPicker>,
    /// When `Some`, that window's title is being edited inline (double-click the
    /// name). `rename_buf` holds the in-progress text; `rename_focus` requests
    /// keyboard focus on the first frame of editing.
    renaming: Option<WinId>,
    rename_buf: String,
    rename_focus: bool,
    /// True only on the root (desktop) manager. The leader state machine and the
    /// `?` overlay run here once per frame, *before* the recursion reaches any
    /// terminal, so command chords never leak to a PTY.
    desktop: bool,
    /// Command mode is armed: the leader was pressed and the next chord is a
    /// command. No timeout, no multi-key sequences — deliberately dumb.
    armed: bool,
    /// Read-only bindings cheat sheet is open. Dismissed by any key.
    show_help: bool,
    /// When `Some`, the keybindings editor modal is open (desktop only). Like the
    /// picker, while it is up no terminal is active so its input is fully captured.
    settings: Option<SettingsView>,
    /// Previously-focused window in this manager, for the `Tab` toggle. On the
    /// desktop this is the last project; inside a project, the last terminal.
    last_focused: Option<WinId>,
    /// Size of the area this manager was last rendered into. Lets keyboard-driven
    /// zoom/snap commit a rect immediately (the show loop refits next frame).
    last_area: egui::Vec2,
    /// The active key bindings (leader + chord→command). Only the desktop manager
    /// consults it (the leader state machine runs there); child managers carry a
    /// default and never read it.
    keymap: Keymap,
    /// The tiling tree: windows whose ids are leaves are *tiled* and take their
    /// rect from `tree.layout()` each frame. Everything else floats.
    tree: crate::layout::LayoutTree,
    /// tmux-style zoom: render this window full-area on top, tree untouched.
    zoomed: Option<WinId>,
}

impl WindowManager {
    pub fn new() -> Self {
        Self {
            windows: vec![],
            z: 1,
            focused: None,
            next: 1,
            cwd: None,
            tag: None,
            chat: Rc::new(RefCell::new(crate::chat::ChatLog::new())),
            picker: None,
            renaming: None,
            rename_buf: String::new(),
            rename_focus: false,
            desktop: false,
            armed: false,
            show_help: false,
            settings: None,
            last_focused: None,
            last_area: egui::vec2(0.0, 0.0),
            keymap: Keymap::default(),
            tree: Default::default(),
            zoomed: None,
        }
    }

    /// Mark this manager as the root desktop: it runs the leader state machine,
    /// and load the user's key bindings (merged over the in-code defaults).
    pub fn as_desktop(mut self) -> Self {
        self.desktop = true;
        self.keymap = Keymap::load();
        self
    }

    // Cascading offset for a freshly spawned window, plus a fresh id + z.
    fn next_slot(&mut self, size: egui::Vec2) -> (WinId, egui::Rect) {
        let n = self.windows.len() as f32;
        let id = self.next;
        self.next += 1;
        self.z += 1;
        let off = 36.0 + 28.0 * (n % 6.0);
        let rect = egui::Rect::from_min_size(egui::pos2(off, off), size);
        (id, rect)
    }

    fn push_win(&mut self, id: WinId, title: String, rect: egui::Rect, content: Content) {
        self.windows.push(Win {
            id,
            tabs: vec![Tab {
                title,
                content,
                chat_member: false,
            }],
            active: 0,
            rect,
            z: self.z,
            minimized: false,
            prev: None,
        });
        self.focused = Some(id);
    }

    /// Spawn a terminal into this manager. Returns the new window's id, or `None`
    /// if the PTY failed to spawn (the caller treats that as a no-op).
    pub fn add_terminal(&mut self, shell: Shell, ctx: &egui::Context) -> Option<WinId> {
        let env = self.term_env(self.next);
        let s = Session::spawn(shell, self.cwd.as_deref(), &env, ctx.clone()).ok()?;
        let (id, rect) = self.next_slot(egui::vec2(580.0, 380.0));
        self.push_win(
            id,
            format!("{}  ·  #{}", shell.label(), id),
            rect,
            Content::Terminal(s),
        );
        Some(id)
    }

    /// Default placement for a freshly created window: split the anchor leaf
    /// (the previously-focused tiled window) along its longer axis; with no
    /// tiled anchor, enter at the root. The new window's floating rect is kept
    /// in `prev` for a later tear-out.
    pub(crate) fn tile_new(&mut self, id: WinId, anchor: Option<WinId>) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            if w.prev.is_none() {
                w.prev = Some(w.rect);
            }
        }
        match anchor.filter(|a| *a != id && self.tree.contains(*a)) {
            Some(a) => {
                let r = self
                    .windows
                    .iter()
                    .find(|w| w.id == a)
                    .map(|w| w.rect)
                    .unwrap_or(egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area));
                let side = if r.width() >= r.height() { Dir::Right } else { Dir::Down };
                self.tree.insert_split(a, id, side);
            }
            None => self.tree.insert_root(id, Dir::Right),
        }
    }

    /// Add a new project window. It starts as a sandbox containing one terminal.
    /// TODO(status line): show repo / branch on the project titlebar.
    pub fn add_project(&mut self, shell: Shell, cwd: PathBuf, ctx: &egui::Context) -> WinId {
        let (id, rect) = self.next_slot(egui::vec2(720.0, 480.0));
        let title = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("project {}", id));
        let mut child = WindowManager::new();
        child.tag = Some(format!("p{}", id));
        child.cwd = Some(cwd);
        if let Some(tid) = child.add_terminal(shell, ctx) {
            child.tile_new(tid, None);
        }
        self.push_win(id, title, rect, Content::Project(Box::new(child)));
        id
    }

    /// Env injected into every PTY this manager spawns (spec: agent-dispatch).
    fn term_env(&self, term_id: WinId) -> Vec<(String, String)> {
        let mut v = vec![
            ("FOREMAN".to_string(), "1".to_string()),
            ("FOREMAN_TERMINAL_ID".to_string(), format!("t{term_id}")),
        ];
        if let Some(t) = &self.tag {
            v.push(("FOREMAN_PROJECT_ID".to_string(), t.clone()));
        }
        // The client needs to find this exe; PATH won't have target\debug.
        if let Ok(exe) = std::env::current_exe() {
            v.push(("FOREMAN_EXE".to_string(), exe.display().to_string()));
        }
        v
    }

    /// Resolve a control-request project spec ("p3"; None = focused project)
    /// to a desktop window id. Only checks the ACTIVE tab — after tab-merging
    /// projects, the swallowed project's old id is stale (documented gotcha).
    fn resolve_project(&self, spec: Option<&str>) -> Result<WinId, String> {
        let is_project = |w: &&Win| matches!(w.tabs[w.active].content, Content::Project(_));
        match spec {
            Some(s) => {
                let id: WinId = s
                    .strip_prefix('p')
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| format!("bad project id: {s}"))?;
                self.windows
                    .iter()
                    .filter(is_project)
                    .find(|w| w.id == id)
                    .map(|w| w.id)
                    .ok_or_else(|| format!("no such project: {s}"))
            }
            None => self
                .focused
                .and_then(|id| self.windows.iter().filter(is_project).find(|w| w.id == id))
                .map(|w| w.id)
                .ok_or_else(|| "no focused project (pass --project)".to_string()),
        }
    }

    /// Drain-side handler for one control message (desktop manager only).
    /// Both verbs honor the reply-timeout contract (drop stale requests
    /// unexecuted). `open` additionally undoes orphaned spawns; chat posts
    /// instead reply BEFORE injecting — an injection cannot be undone, so the
    /// bytes only flow once the client is guaranteed to hear "ok" (spec §3).
    pub fn handle_ctrl(&mut self, msg: crate::control::CtrlMsg, ctx: &egui::Context) {
        use crate::control::{CtrlMsg, OpenReply, REPLY_TIMEOUT};
        match msg {
            CtrlMsg::Open(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                let res = self.open_dispatch(req, ctx);
                let undo = res.as_ref().ok().copied();
                if reply.send(Self::open_reply(res)).is_err() {
                    if let Some((pid, tid)) = undo {
                        self.close_terminal(pid, tid);
                    }
                }
            }
            CtrlMsg::Chat(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                match self.chat_dispatch(&req) {
                    Err(e) => {
                        let _ = reply.send(OpenReply::err(e));
                    }
                    Ok(ChatOutcome::History(lines)) => {
                        let _ = reply.send(OpenReply {
                            ok: true,
                            terminal: None,
                            project: None,
                            error: None,
                            history: Some(lines),
                            seq: None,
                        });
                    }
                    // Unlike open's spawn-undo, a post whose reply channel died
                    // STAYS in the log (spec §3: append-only; the room is the
                    // log, not the audience) — only the injection is skipped.
                    // A retrying client may therefore duplicate a history line;
                    // accepted v1.
                    Ok(ChatOutcome::Posted {
                        pid,
                        from,
                        framed,
                        targets,
                        seq,
                    }) => {
                        let ok = OpenReply {
                            ok: true,
                            terminal: None,
                            project: None,
                            error: None,
                            history: None,
                            seq,
                        };
                        if reply.send(ok).is_ok() {
                            self.chat_broadcast_in(pid, from, &framed, targets.as_deref());
                        }
                        ctx.request_repaint(); // log changed either way (viewer)
                    }
                }
            }
            CtrlMsg::Status(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                let _ = reply.send(match self.status_dispatch(&req) {
                    Ok(lines) => OpenReply {
                        ok: true,
                        terminal: None,
                        project: None,
                        error: None,
                        history: Some(lines),
                        seq: None,
                    },
                    Err(e) => OpenReply::err(e),
                });
            }
            CtrlMsg::Close(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                match self.close_dispatch(&req) {
                    Err(e) => {
                        let _ = reply.send(OpenReply::err(e));
                    }
                    Ok((pid, tids)) => {
                        let ok = OpenReply {
                            ok: true,
                            terminal: None,
                            project: Some(format!("p{pid}")),
                            error: None,
                            history: None,
                            seq: None,
                        };
                        // Reply BEFORE closing (chat's reply-before-inject
                        // pattern): a self-close kills the caller's own
                        // process tree, so the reply must be on the channel
                        // before its PTY drops. If the receiver is gone the
                        // client was already told foreman didn't respond —
                        // skip the close entirely (ids are never reused, so
                        // a retry errs loudly instead of double-closing).
                        if reply.send(ok).is_ok() {
                            for tid in tids {
                                self.close_terminal(pid, tid);
                            }
                            ctx.request_repaint();
                        }
                    }
                }
            }
        }
    }

    fn open_reply(res: Result<(WinId, WinId), String>) -> crate::control::OpenReply {
        use crate::control::OpenReply;
        match res {
            Ok((pid, tid)) => OpenReply {
                ok: true,
                terminal: Some(format!("t{tid}")),
                project: Some(format!("p{pid}")),
                error: None,
                history: None,
                seq: None,
            },
            Err(e) => OpenReply::err(e),
        }
    }

    /// Resolve + spawn for a dispatch request; returns (project id, terminal id).
    fn open_dispatch(
        &mut self,
        req: crate::control::OpenRequest,
        ctx: &egui::Context,
    ) -> Result<(WinId, WinId), String> {
        if req.command.is_empty() || req.command[0].is_empty() {
            return Err("empty command".into());
        }
        let pid = self.resolve_project(req.project.as_deref())?;
        let win = self
            .windows
            .iter_mut()
            .find(|w| w.id == pid)
            .expect("resolved");
        let Content::Project(child) = &mut win.tabs[win.active].content else {
            return Err("not a project".into()); // unreachable after resolve
        };
        child
            .add_terminal_cmd(
                &req.command,
                req.cwd.as_deref().map(std::path::Path::new),
                req.title.as_deref(),
                ctx,
            )
            .map(|tid| (pid, tid))
            .map_err(|e| format!("spawn failed: {e}"))
    }

    /// Resolve + execute the room-side half of a chat request: history reads
    /// answer immediately; posts append/join and return the framed line for
    /// the post-reply broadcast.
    fn chat_dispatch(&mut self, req: &crate::control::ChatRequest) -> Result<ChatOutcome, String> {
        let pid = self.resolve_project(req.project.as_deref())?;
        let win = self
            .windows
            .iter_mut()
            .find(|w| w.id == pid)
            .expect("resolved");
        let Content::Project(child) = &mut win.tabs[win.active].content else {
            return Err("not a project".into()); // unreachable after resolve
        };
        match (&req.text, req.history) {
            (None, Some(n)) => Ok(ChatOutcome::History(child.chat_history(n))),
            (Some(text), None) => {
                // history reads are anonymous; a post must name its sender
                let from = term_id(
                    req.from
                        .as_deref()
                        .ok_or("posting requires a sender (FOREMAN_TERMINAL_ID)")?,
                )?;
                let (framed, targets, seq) = child.chat_post_re(from, text, &req.to, req.re)?;
                Ok(ChatOutcome::Posted {
                    pid,
                    from,
                    framed,
                    targets,
                    seq: Some(seq),
                })
            }
            _ => Err("chat needs exactly one of text/history".into()),
        }
    }

    /// Build the `status` listing: one header line per project window, one
    /// line per terminal TAB inside it (`Content::Chat`/nested projects are
    /// skipped). Merged tabs in one window share the window's `tN` id —
    /// status emits one line per tab, duplicating the shared id, same
    /// identity family as chat. `None` project = every desktop window whose
    /// ACTIVE tab is a project (the same visibility rule as
    /// `resolve_project`); a filter that doesn't resolve is an error, not an
    /// empty list. Running/exited truth comes from `Session::exited()`
    /// (try_wait on the live process), never from the `"  ·  exited (code)"`
    /// title stamp — titles are cleaned with `display_name`.
    fn status_dispatch(
        &mut self,
        req: &crate::control::StatusRequest,
    ) -> Result<Vec<String>, String> {
        let filter = match req.project.as_deref() {
            Some(spec) => Some(self.resolve_project(Some(spec))?),
            None => None,
        };
        let mut lines = Vec::new();
        for w in self.windows.iter_mut() {
            if let Some(pid) = filter
                && w.id != pid
            {
                continue;
            }
            // title read (and detached) BEFORE the mutable content borrow
            let name = display_name(&w.tabs[w.active].title).to_string();
            let Content::Project(child) = &mut w.tabs[w.active].content else {
                continue; // resolve_project guarantees this never skips a filtered pid
            };
            let cwd = child
                .cwd
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".into());
            lines.push(format!("p{}  {}  {}", w.id, name, cwd));
            for win in child.windows.iter_mut() {
                let wid = win.id;
                for tab in win.tabs.iter_mut() {
                    let Content::Terminal(s) = &mut tab.content else {
                        continue;
                    };
                    let state = match s.exited() {
                        Some(code) => format!("exited({code})"),
                        None => "running".into(),
                    };
                    let member = if tab.chat_member { "chat" } else { "-" };
                    lines.push(format!(
                        "  t{}  {}  {}  {}",
                        wid,
                        state,
                        member,
                        display_name(&tab.title)
                    ));
                }
            }
        }
        if filter.is_none() && lines.is_empty() {
            lines.push("no projects".into());
        }
        Ok(lines)
    }

    /// Validate a close request WITHOUT executing it (D5: atomic and loud).
    /// Every id must name an existing terminal window in the project or the
    /// WHOLE request fails and nothing closes. A window whose tabs hold no
    /// `Content::Terminal` (the chat viewer) is refused. Exited terminals
    /// are valid targets; duplicates are allowed. Closing a window closes
    /// ALL its merged tabs — terminal identity is the window id, shared by
    /// merged tabs (same identity family as chat). Execution is the caller's
    /// job via [`Self::close_terminal`], AFTER the reply is delivered.
    fn close_dispatch(
        &self,
        req: &crate::control::CloseRequest,
    ) -> Result<(WinId, Vec<WinId>), String> {
        if req.terminals.is_empty() {
            return Err("no terminals to close".into());
        }
        let pid = self.resolve_project(req.project.as_deref())?;
        let win = self.windows.iter().find(|w| w.id == pid).expect("resolved");
        let Content::Project(child) = &win.tabs[win.active].content else {
            return Err("not a project".into()); // unreachable after resolve
        };
        let mut tids = Vec::new();
        for spec in &req.terminals {
            let tid = term_id(spec)?;
            let w = child
                .windows
                .iter()
                .find(|w| w.id == tid)
                .ok_or_else(|| format!("no such terminal: {spec}"))?;
            if !w
                .tabs
                .iter()
                .any(|t| matches!(t.content, Content::Terminal(_)))
            {
                return Err(format!("not a terminal: {spec}"));
            }
            tids.push(tid);
        }
        Ok((pid, tids))
    }

    /// Broadcast a framed post inside project `pid` (the after-reply half).
    fn chat_broadcast_in(
        &mut self,
        pid: WinId,
        from: WinId,
        framed: &str,
        targets: Option<&[WinId]>,
    ) {
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == pid)
            && let Content::Project(child) = &mut win.tabs[win.active].content
        {
            child.chat_broadcast(Some(from), framed, targets);
        }
    }

    /// Close terminal `tid` inside project `pid` (the dispatch undo path).
    fn close_terminal(&mut self, pid: WinId, tid: WinId) {
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == pid) {
            if let Content::Project(child) = &mut win.tabs[win.active].content {
                child.close(tid);
            }
        }
    }

    /// Spawn an explicit command (agent dispatch) as a terminal in this manager.
    /// The session opens with a dim banner line (see [`dispatch_banner`]) so the
    /// pane announces itself before a silent worker produces any output.
    fn add_terminal_cmd(
        &mut self,
        argv: &[String],
        cwd: Option<&std::path::Path>,
        title: Option<&str>,
        ctx: &egui::Context,
    ) -> std::io::Result<WinId> {
        let env = self.term_env(self.next);
        let cwd = cwd.or(self.cwd.as_deref());
        let mut s = Session::spawn_argv(argv, cwd, &env, ctx.clone())?;
        s.inject_note(&dispatch_banner(argv));
        let (id, rect) = self.next_slot(egui::vec2(580.0, 380.0));
        let title = title
            .map(str::to_string)
            .unwrap_or_else(|| format!("agent · {}", argv[0]));
        // push_win focuses the new window; a dispatched agent must never yank
        // the keyboard out from under the user mid-keystroke (fire-and-watch:
        // the new terminal is to LOOK at, not type into). Keep focus where it
        // was; the window still spawns on top visually (z from next_slot).
        let prev_focus = self.focused;
        self.push_win(id, title, rect, Content::Terminal(s));
        self.tile_new(id, prev_focus);
        // Dispatched agents auto-join the project chat room (spec §2) — and
        // the transcript records it.
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.tabs[w.active].chat_member = true;
            // (`title` was moved into push_win — read it back off the window)
            self.chat.borrow_mut().sys(
                crate::chat::ChatKind::Joined,
                &term_tag(id),
                display_name(w.title()),
            );
        } else {
            debug_assert!(false, "just-pushed window {id} missing");
        }
        self.focused = prev_focus;
        Ok(id)
    }

    /// Open (or focus) this project's chat viewer — singleton per project
    /// (spec §4). Closing it later doesn't touch the log; the room is the log.
    fn open_chat_window(&mut self) {
        if let Some(w) = self
            .windows
            .iter_mut()
            .find(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Chat(_))))
        {
            // Surface it like the taskbar's Restore does: unminimize and make
            // the chat tab active before focusing — focus() alone leaves a
            // minimized window invisible and a background tab hidden.
            if let Some(i) = w
                .tabs
                .iter()
                .position(|t| matches!(t.content, Content::Chat(_)))
            {
                w.active = i;
            }
            w.minimized = false;
            let id = w.id;
            self.focus(id);
            return;
        }
        let (id, rect) = self.next_slot(egui::vec2(420.0, 320.0));
        self.push_win(
            id,
            "chat".into(),
            rect,
            Content::Chat(crate::chat::ChatView::new(Rc::clone(&self.chat))),
        );
    }

    /// Rebuild the chat viewer's crew rows and title chip. Runs before the
    /// draw loop each frame (cheap: a handful of members). No-op when no
    /// viewer window is open.
    fn refresh_chat_view(&mut self) {
        if !self
            .windows
            .iter()
            .any(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Chat(_))))
        {
            return;
        }
        let mut rows = Vec::new();
        for w in &mut self.windows {
            let wid = w.id;
            for (i, t) in w.tabs.iter_mut().enumerate() {
                if !t.chat_member {
                    continue;
                }
                let Content::Terminal(s) = &mut t.content else {
                    continue;
                };
                // Merged member tabs in one window share term_tag(wid) — both rows get the same id and last-heard. Same accepted staleness family as the rest of chat identity (Tab doc).
                rows.push(crate::chat::CrewRow {
                    win: wid,
                    tab: i,
                    id: term_tag(wid),
                    name: display_name(&t.title).to_string(),
                    exited: s.exited().is_some(),
                    last: None,
                });
            }
        }
        {
            let log = self.chat.borrow();
            for r in &mut rows {
                r.last = log.last_activity(&r.id);
            }
        }
        crate::chat::sort_crew(&mut rows);
        let n_live = rows.iter().filter(|r| !r.exited).count();
        // The pane identity sits between live members and the exited — it is
        // "your seat", not fleet status, and never counts toward the live chip.
        let pos = rows.iter().take_while(|r| !r.exited).count();
        rows.insert(
            pos,
            crate::chat::CrewRow {
                win: 0, // no window: click is a no-op (ids start at 1)
                tab: 0,
                id: Self::HUMAN_ID.to_string(),
                name: Self::HUMAN_ID.to_string(),
                exited: false,
                last: self.chat.borrow().last_activity(Self::HUMAN_ID),
            },
        );
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    // mem::take, not a move — the compiler can't see that the
                    // singleton makes a second loop iteration unreachable.
                    v.crew = std::mem::take(&mut rows);
                    // Clobbers a user rename each frame — accepted: the chip
                    // IS the title for the chat window.
                    t.title = format!("chat · {n_live} live");
                    return;
                }
            }
        }
    }

    /// Apply crew-board clicks recorded during the draw (content cannot
    /// mutate sibling windows mid-loop). Stale targets (closed windows,
    /// merged-away tabs) are dropped silently — same staleness family as
    /// terminal-id resolution.
    fn drain_chat_clicks(&mut self) {
        let mut req = None;
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    if let Some(c) = v.click.take() {
                        req = Some(c);
                    }
                }
            }
        }
        if let Some((win, tab)) = req {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == win) {
                if tab < w.tabs.len() {
                    w.active = tab;
                    w.minimized = false;
                    self.focus(win);
                }
                // else: tab merged/closed away — drop silently, same as a missing window
            }
        }
    }

    /// Apply input-line submissions recorded during the draw. Human posts
    /// broadcast to ALL members — there is no sender terminal to exclude —
    /// unless a leading mention narrowed delivery (then only the targets).
    fn drain_chat_posts(&mut self) {
        let mut pending = None;
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    if let Some(p) = v.pending_post.take() {
                        pending = Some(p);
                    }
                }
            }
        }
        if let Some(text) = pending {
            if let Some((framed, targets)) = self.chat_post_human(&text) {
                self.chat_broadcast(None, &framed, targets.as_deref());
            }
        }
    }

    /// Resolve + validate mention targets against this project's members
    /// (mentions spec §5) — call BEFORE any mutation: a failed post must not
    /// append and must not join-on-first-post. `sender` None = the human
    /// (`you` then counts as self-mention). Returns the terminal WinIds to
    /// deliver to; `you` is valid markup but resolves to no terminal.
    fn validate_chat_targets(
        &mut self,
        sender: Option<WinId>,
        targets: &[String],
    ) -> Result<Vec<WinId>, String> {
        let mut ids = Vec::new();
        for t in targets {
            if t == "you" {
                if sender.is_none() {
                    return Err("cannot mention yourself".into());
                }
                continue;
            }
            let id = term_id(t)?;
            let win = self
                .windows
                .iter_mut()
                .find(|w| w.id == id)
                .ok_or_else(|| format!("no such terminal: {t}"))?;
            if Some(id) == sender {
                return Err("cannot mention yourself".into());
            }
            let (mut member, mut alive) = (false, false);
            for tab in &mut win.tabs {
                if !tab.chat_member {
                    continue;
                }
                member = true;
                if let Content::Terminal(s) = &mut tab.content
                    && s.exited().is_none()
                {
                    alive = true;
                }
            }
            if !member {
                return Err(format!("{t} is not a chat member"));
            }
            if !alive {
                return Err(format!("{t} has exited"));
            }
            ids.push(id);
        }
        Ok(ids)
    }

    /// Post into this project's chat: validate the sender, append, join the
    /// sender (spec §2: join-on-first-post). Targets (`--to` flags + leading
    /// inline mentions) validate all-or-nothing BEFORE the join/append, so a
    /// failed post mutates nothing (mentions spec §5). Returns the framed
    /// injection line plus the resolved delivery filter for `chat_broadcast`
    /// (`None` = broadcast). Injection itself is `chat_broadcast` — kept
    /// separate because the reply must be sent BEFORE bytes flow (spec §3:
    /// reply-before-inject).
    fn chat_post(
        &mut self,
        from: WinId,
        text: &str,
        to_flags: &[String],
    ) -> Result<(String, Option<Vec<WinId>>), String> {
        let (framed, targets, _seq) = self.chat_post_re(from, text, to_flags, None)?;
        Ok((framed, targets))
    }

    /// Post carrying a handshake back-pointer (`--re`); returns the framed line,
    /// the delivery filter, and the posted seq (the sender's ack handle). The
    /// no-`re` [`Self::chat_post`] wraps this for the common case.
    fn chat_post_re(
        &mut self,
        from: WinId,
        text: &str,
        to_flags: &[String],
        re: Option<u64>,
    ) -> Result<(String, Option<Vec<WinId>>, u64), String> {
        if text.is_empty() {
            return Err("empty message".into());
        }
        let targets = crate::chat::effective_targets(to_flags, text);
        let resolved = if targets.is_empty() {
            None
        } else {
            Some(self.validate_chat_targets(Some(from), &targets)?)
        };
        let sender = self
            .windows
            .iter_mut()
            .find(|w| w.id == from)
            .ok_or_else(|| format!("no such terminal: t{from}"))?;
        let newly_joined = !sender.tabs[sender.active].chat_member;
        sender.tabs[sender.active].chat_member = true;
        debug_assert!(
            self.tag.is_some(),
            "chat_post on a tag-less (desktop?) manager — routing bug"
        );
        let project = self.tag.as_deref().unwrap_or("p?");
        // .to_string() drops the &mut Win borrow before the RefCell borrow below
        let name = display_name(sender.title()).to_string();
        let from_tag = term_tag(from);
        let mut log = self.chat.borrow_mut();
        if newly_joined {
            // join-on-first-post: the sysline lands BEFORE the post so the
            // transcript reads join-then-speak
            log.sys(crate::chat::ChatKind::Joined, &from_tag, &name);
        }
        let msg = log.post_re(&from_tag, &name, text, targets, re);
        Ok((msg.frame(project), resolved, msg.seq))
    }

    /// The pane's reserved sender identity — can never collide with a "tN"
    /// terminal id (spec: chat-dispatcher-window §Slices).
    const HUMAN_ID: &'static str = "you";

    /// Append a post from the chat pane's input line. No membership games —
    /// the human is not a terminal. Leading mentions narrow delivery like CLI
    /// posts, but a bad mention demotes the post to plain broadcast instead
    /// of erroring — the input line has no error seat (mentions spec §7).
    /// Returns the framed line plus the delivery filter for `chat_broadcast`.
    fn chat_post_human(&mut self, text: &str) -> Option<(String, Option<Vec<WinId>>)> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        // effective_targets (not raw leading_mentions) for dedup parity with
        // CLI posts — `@t2 @t2 go` must not frame `you→t2,t2`
        let mentions = crate::chat::effective_targets(&[], text);
        let (to, resolved) = if mentions.is_empty() {
            (Vec::new(), None)
        } else {
            match self.validate_chat_targets(None, &mentions) {
                Ok(ids) => (mentions, Some(ids)),
                Err(_) => (Vec::new(), None), // prose fallback
            }
        };
        debug_assert!(self.tag.is_some(), "human post on a tag-less manager");
        let project = self.tag.as_deref().unwrap_or("p?").to_string();
        let mut log = self.chat.borrow_mut();
        let msg = log.post_to(Self::HUMAN_ID, Self::HUMAN_ID, text, to);
        Some((msg.frame(&project), resolved))
    }

    /// Inject a framed chat line into every member tab except the sender's
    /// active tab, skipping exited sessions and non-terminal content (the
    /// chat viewer renders the log directly — never injected). Background
    /// tabs receive too: keepalive keeps their PTYs drained, and chat's
    /// whole point is that members never have to poll. `None` = the human
    /// posting from the chat pane; excludes nobody.
    /// `targets`: None = broadcast; Some(ids) = only those windows' member
    /// tabs; Some(&[]) injects nobody (a pure @you post).
    fn chat_broadcast(&mut self, from: Option<WinId>, framed: &str, targets: Option<&[WinId]>) {
        for w in self.windows.iter_mut() {
            if let Some(t) = targets
                && !t.contains(&w.id)
            {
                continue;
            }
            let active = w.active;
            let is_sender = Some(w.id) == from;
            for (i, tab) in w.tabs.iter_mut().enumerate() {
                if (is_sender && i == active) || !tab.chat_member {
                    continue;
                }
                if let Content::Terminal(s) = &mut tab.content {
                    if s.exited().is_none() {
                        s.inject_input(framed);
                    }
                }
            }
        }
    }

    /// Last `n` chat lines (the `--history` verb; reading does not join).
    fn chat_history(&self, n: usize) -> Vec<String> {
        self.chat.borrow().tail_lines(n)
    }

    /// Where the picker opens: the focused project's cwd if there is one, else the
    /// process working directory, else `.`.
    fn picker_start(&self) -> PathBuf {
        self.focused
            .and_then(|id| self.windows.iter().find(|w| w.id == id))
            .and_then(|w| match &w.tabs[w.active].content {
                Content::Project(wm) => wm.cwd.clone(),
                _ => None,
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn focus(&mut self, id: WinId) {
        self.z += 1;
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.z = self.z;
        }
        // Remember the outgoing focus so `Tab` can toggle back to it.
        if self.focused != Some(id) {
            self.last_focused = self.focused;
        }
        self.focused = Some(id);
    }

    // --- leader / command mode (desktop only) -------------------------------

    /// Run the leader state machine for one frame. Returns the command chord to
    /// execute, if armed and a chord arrived this frame. All keystrokes that the
    /// command layer claims are *drained from egui input here* so they never
    /// reach the focused terminal's `read_input`.
    ///
    /// States: idle → (leader) → armed → (any chord) → idle. An unbound chord
    /// while armed disarms and is swallowed (tmux behaviour).
    fn pump_leader(&mut self, ui: &mut egui::Ui) -> Option<Command> {
        // The help overlay eats the next keystroke (any key dismisses it) so the
        // dismissing key never lands in a terminal.
        if self.show_help {
            let any_key = ui.input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key { pressed: true, .. }
                            | egui::Event::Text(_)
                            | egui::Event::Copy
                            | egui::Event::Cut
                            | egui::Event::Paste(_)
                    )
                })
            });
            if any_key {
                self.show_help = false;
            }
            // Always swallow input while the overlay is up so the dismissing key
            // (or any stray keystroke) never reaches a terminal.
            self.swallow_input(ui);
            return None;
        }

        if !self.armed {
            // Idle: arm when the leader chord arrives. We look for the *exact*
            // chord (key + modifiers) so e.g. a plain `b` never arms when the
            // leader is `Ctrl+b`. If matched, swallow this frame's input so the
            // leader never reaches a PTY.
            let leader = self.keymap.leader;
            let hit = ui.input(|i| {
                i.events
                    .iter()
                    .any(|e| Self::event_chord(e) == Some(leader))
            });
            if hit {
                self.armed = true;
                self.swallow_input(ui);
            }
            return None;
        }

        // Armed: the next keystroke is a command. Find the first key-press event,
        // map it to a command, then swallow *everything* this frame (including the
        // companion Event::Text) so no fragment leaks to the terminal.
        let chord = ui.input(|i| i.events.iter().find_map(Self::event_chord));

        let Some(chord) = chord else {
            // No key yet this frame (e.g. only Text from the held leader). Wait,
            // but still swallow any stray text so it can't reach the terminal.
            self.swallow_input(ui);
            return None;
        };

        self.armed = false;
        let cmd = self.keymap.resolve(chord);
        // Whether bound or not, the whole chord is ours: swallow it.
        self.swallow_input(ui);
        cmd
    }

    /// Drain every keyboard-ish input event for this frame so nothing reaches a
    /// focused terminal. Used while armed and while the help overlay is open.
    fn swallow_input(&self, ui: &mut egui::Ui) {
        ui.input_mut(|i| {
            i.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::Key { .. }
                        | egui::Event::Text(_)
                        | egui::Event::Copy
                        | egui::Event::Cut
                        | egui::Event::Paste(_)
                )
            });
        });
    }

    /// Map a single egui input `Event` to the [`Chord`] it represents, or `None`
    /// if it is not a key-press chord. `command` (⌘) is folded onto `ctrl` to
    /// match Phase 1. egui delivers `Ctrl+C` / `Ctrl+X` as `Copy` / `Cut`
    /// events, so we translate those back to their key chords.
    fn event_chord(e: &egui::Event) -> Option<Chord> {
        match e {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => Some(Chord::from_event(*key, *modifiers)),
            egui::Event::Copy => Some(Chord::new(egui::Key::C, true, false, false)),
            egui::Event::Cut => Some(Chord::new(egui::Key::X, true, false, false)),
            _ => None,
        }
    }

    /// Execute a resolved command. Terminal-level commands route into the focused
    /// project's child manager; project-level commands act on `self` (desktop).
    fn dispatch(&mut self, cmd: Command, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        match cmd {
            // ---- project (outer) level: act on the desktop ----
            Command::ProjFocus(d) => self.focus_dir(d),
            Command::ProjSnap(d) => self.move_dir(d),
            Command::ProjFloat => self.toggle_float(),
            Command::ZoomProject => {
                if let Some(id) = self.focused {
                    self.toggle_zoom(id);
                }
            }
            Command::CloseProject => {
                if let Some(id) = self.focused {
                    self.close_active_tab(id);
                }
            }
            Command::LastProject => self.toggle_last(),
            Command::NewProject => {
                self.picker = Some(DirPicker::new(self.picker_start()));
            }
            Command::Help => self.show_help = true,
            Command::OpenSettings => self.open_settings(),

            // ---- terminal (inner) level: act on the focused project's child ----
            other => {
                if let Some(child) = self.focused_child() {
                    match other {
                        Command::TermFocus(d) => child.focus_dir(d),
                        Command::TermSnap(d) => child.move_dir(d),
                        Command::TermFloat => child.toggle_float(),
                        Command::Split(d) => child.split_dir(d, &ctx),
                        Command::ZoomTerm => {
                            if let Some(id) = child.focused {
                                child.toggle_zoom(id);
                            }
                        }
                        Command::CloseTerm => {
                            if let Some(id) = child.focused {
                                child.close_active_tab(id);
                            }
                        }
                        Command::Rename => child.begin_rename(),
                        Command::NewTerm => {
                            let anchor = child.focused;
                            if let Some(nid) = child.add_terminal(Shell::PowerShell, &ctx) {
                                child.tile_new(nid, anchor);
                            }
                        }
                        Command::LastTerm => child.toggle_last(),
                        Command::TabCycle => child.cycle_tab(true),
                        Command::TabPrev => child.cycle_tab(false),
                        Command::OpenChat => child.open_chat_window(),
                        // project-level handled above
                        _ => {}
                    }
                }
            }
        }
    }

    /// Open the keybindings editor modal (desktop only). Closes the read-only
    /// help overlay if it was up, so the two modals never stack.
    fn open_settings(&mut self) {
        self.show_help = false;
        self.settings = Some(SettingsView::new());
    }

    /// Mutable borrow of the focused window's child manager, if it is a project.
    fn focused_child(&mut self) -> Option<&mut WindowManager> {
        let id = self.focused?;
        self.windows
            .iter_mut()
            .find(|w| w.id == id)
            .and_then(|w| match w.active_content() {
                Content::Project(wm) => Some(wm.as_mut()),
                _ => None,
            })
    }

    fn begin_rename(&mut self) {
        if let Some(id) = self.focused {
            if let Some(w) = self.windows.iter().find(|w| w.id == id) {
                self.renaming = Some(id);
                self.rename_buf = w.title().to_string();
                self.rename_focus = true;
            }
        }
    }

    /// Pull `id` out of the tiled layer entirely: drop its tree leaf (siblings
    /// absorb the space) and clear zoom if it was the zoomed window. Safe no-op
    /// for floating windows. Call before any close/minimize/merge-consume/tear-out.
    fn detach(&mut self, id: WinId) {
        self.tree.remove(id);
        if self.zoomed == Some(id) {
            self.zoomed = None;
        }
    }

    /// Remove an entire window (all of its tabs) and fix up focus.
    fn close(&mut self, id: WinId) {
        self.detach(id);
        self.windows.retain(|w| w.id != id);
        if self.focused == Some(id) {
            self.focused = self.last_focused.take();
        }
        if self.last_focused == Some(id) {
            self.last_focused = None;
        }
    }

    /// Close one tab: the given tab index of window `id`. Removing the last tab
    /// closes the window. Otherwise the active index is clamped so it still points
    /// at a live tab (prefer staying on the tab to the left of the one removed).
    fn close_tab(&mut self, id: WinId, idx: usize) {
        let Some(w) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        if idx >= w.tabs.len() {
            return;
        }
        if w.tabs.len() == 1 {
            self.close(id);
            return;
        }
        w.tabs.remove(idx);
        if w.active >= idx && w.active > 0 {
            w.active -= 1;
        }
        if w.active >= w.tabs.len() {
            w.active = w.tabs.len() - 1;
        }
    }

    /// Close the active tab of window `id` (used by `x` / the titlebar close
    /// control). Closes the window when it was the last tab.
    fn close_active_tab(&mut self, id: WinId) {
        let active = self.windows.iter().find(|w| w.id == id).map(|w| w.active);
        if let Some(a) = active {
            self.close_tab(id, a);
        }
    }

    /// Merge `src` window's tabs onto `dst` window's stack, then remove `src`.
    /// The merged tabs are appended; the first moved tab becomes active so the
    /// dropped window is what the user sees. No-op if either id is missing or
    /// `src == dst` (can't merge a window onto itself).
    fn merge_windows(&mut self, src: WinId, dst: WinId) {
        if src == dst {
            return;
        }
        let Some(si) = self.windows.iter().position(|w| w.id == src) else {
            return;
        };
        let Some(di) = self.windows.iter().position(|w| w.id == dst) else {
            return;
        };
        // Remove the source first; recompute the destination index afterwards
        // since removal may shift it.
        self.detach(src);
        let src_win = self.windows.remove(si);
        let di = self.windows.iter().position(|w| w.id == dst).unwrap_or(di);
        let dst_win = &mut self.windows[di];
        let first_new = dst_win.tabs.len();
        dst_win.tabs.extend(src_win.tabs);
        dst_win.active = first_new; // show the just-dropped tab
        // Focus the merged target; drop any dangling focus/last-focus on src.
        if self.last_focused == Some(src) {
            self.last_focused = None;
        }
        if self.focused == Some(src) {
            self.focused = None;
        }
        self.focus(dst);
    }

    /// Detach tab `idx` of window `id` into a brand-new floating window placed at
    /// `local_pos` (manager-local coords). Used by drag-out (untab). The new
    /// window restores a sensible floating size. Returns the new window's id, or
    /// `None` if nothing was detached (source had only one tab / bad index). If the
    /// source had only one tab, this is a no-op (dragging the sole tab just moves
    /// the window, handled by the normal title drag).
    fn untab(&mut self, id: WinId, idx: usize, local_pos: egui::Pos2) -> Option<WinId> {
        let w = self.windows.iter_mut().find(|w| w.id == id)?;
        if w.tabs.len() <= 1 || idx >= w.tabs.len() {
            return None;
        }
        let tab = w.tabs.remove(idx);
        if w.active >= idx && w.active > 0 {
            w.active -= 1;
        }
        if w.active >= w.tabs.len() {
            w.active = w.tabs.len() - 1;
        }
        // A sensible restored size: the source window's pre-snap floating size if
        // it has one, else its current rect size, clamped to a floor.
        let size = w
            .prev
            .map(|r| r.size())
            .unwrap_or_else(|| w.rect.size())
            .max(egui::vec2(MIN_W, MIN_H));
        let new_id = self.next;
        self.next += 1;
        self.z += 1;
        // Anchor the new window so the grabbed title sits roughly under the cursor.
        let origin = egui::pos2(local_pos.x - size.x * 0.5, local_pos.y - TITLE_H * 0.5);
        self.windows.push(Win {
            id: new_id,
            tabs: vec![tab],
            active: 0,
            rect: egui::Rect::from_min_size(origin, size),
            z: self.z,
            minimized: false,
            prev: None,
        });
        self.focus(new_id);
        Some(new_id)
    }

    fn toggle_last(&mut self) {
        if let Some(prev) = self.last_focused {
            if self.windows.iter().any(|w| w.id == prev && !w.minimized) {
                self.focus(prev);
            }
        }
    }

    /// `Tab`: advance the focused window's active tab by `+1`/`-1`. If the focused
    /// window is *not* a stack (len-1) and `forward`, fall back to the last-focused
    /// window toggle (the pre-tabs `Tab` behaviour). `Shift+Tab` on a non-stack
    /// does nothing (there is no "previous tab" to go to).
    fn cycle_tab(&mut self, forward: bool) {
        let Some(id) = self.focused else { return };
        let Some(w) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        let n = w.tabs.len();
        if n <= 1 {
            if forward {
                self.toggle_last();
            }
            return;
        }
        w.active = if forward {
            (w.active + 1) % n
        } else {
            (w.active + n - 1) % n
        };
        self.focus(id);
    }

    /// Recursively pump every PTY in this manager's tree — used to keep an entire
    /// *inactive project tab* (whole child manager) alive while it is not rendered.
    /// Mirrors `Content::keepalive` but reaches every tab of every window, since an
    /// un-rendered manager's show loop (which normally pumps the active tab) never
    /// runs this frame.
    fn keepalive(&mut self) {
        for w in &mut self.windows {
            for t in &mut w.tabs {
                t.content.keepalive();
            }
        }
    }

    /// tmux-style zoom: render the window full-area on top. The tree and other
    /// windows are untouched; un-zoom restores instantly. A floating window's
    /// rect round-trips via `prev`.
    fn toggle_zoom(&mut self, id: WinId) {
        if self.zoomed == Some(id) {
            self.zoomed = None;
            if !self.tree.contains(id) {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                    if let Some(pr) = w.prev.take() {
                        w.rect = pr;
                    }
                }
            }
        } else {
            if !self.tree.contains(id) {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                    w.prev = Some(w.rect);
                }
            }
            self.zoomed = Some(id);
        }
        self.focus(id);
    }

    /// Move the focused window within the tiled layer. Tiled: swap with the
    /// geometric neighbor leaf in that direction; with no neighbor, re-insert at
    /// the area edge as a full row/column. Floating: enter the tree at that edge.
    fn move_dir(&mut self, d: Dir) {
        let Some(id) = self.focused else { return };
        if self.tree.contains(id) {
            let local = egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area);
            let placements = self.tree.layout(local, SNAP_GAP);
            let Some(from) = placements.iter().find(|(w, _)| *w == id).map(|(_, r)| r.center())
            else {
                return;
            };
            let mut best: Option<(WinId, f32)> = None;
            for (w, r) in placements.iter().filter(|(w, _)| *w != id) {
                let c = r.center();
                let (along, cross) = match d {
                    Dir::Left => (from.x - c.x, (c.y - from.y).abs()),
                    Dir::Right => (c.x - from.x, (c.y - from.y).abs()),
                    Dir::Up => (from.y - c.y, (c.x - from.x).abs()),
                    Dir::Down => (c.y - from.y, (c.x - from.x).abs()),
                };
                if along <= 1.0 {
                    continue;
                }
                let score = along + cross * 2.0;
                if best.map_or(true, |(_, b)| score < b) {
                    best = Some((*w, score));
                }
            }
            match best {
                Some((n, _)) => {
                    self.tree.swap(id, n);
                }
                None => {
                    self.tree.remove(id);
                    self.tree.insert_root(id, d);
                }
            }
        } else {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                if w.prev.is_none() {
                    w.prev = Some(w.rect);
                }
            }
            self.tree.insert_root(id, d);
        }
        self.focus(id);
    }

    /// Split: create a new terminal next to the focused window in the tree.
    fn split_dir(&mut self, d: Dir, ctx: &egui::Context) {
        let src = self.focused;
        let Some(new_id) = self.add_terminal(Shell::PowerShell, ctx) else {
            return;
        };
        self.place_split(src, new_id, d);
    }

    /// The pure placement half of [`split_dir`] (no PTY/spawn), testable without
    /// a real `Session`. A floating (or absent) source first enters the tree so
    /// `Alt+WASD` always yields the two-pane result the user expects.
    fn place_split(&mut self, src: Option<WinId>, new_id: WinId, d: Dir) {
        let anchor = match src.filter(|s| *s != new_id) {
            Some(s) if self.tree.contains(s) => Some(s),
            Some(s) => {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == s) {
                    if w.prev.is_none() {
                        w.prev = Some(w.rect);
                    }
                }
                self.tree.insert_root(s, Dir::Right);
                Some(s)
            }
            None => None,
        };
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == new_id) {
            if w.prev.is_none() {
                w.prev = Some(w.rect);
            }
        }
        match anchor {
            Some(a) => {
                self.tree.insert_split(a, new_id, d);
            }
            None => self.tree.insert_root(new_id, d),
        }
        self.focus(new_id);
    }

    /// Toggle the focused window between tiled and floating. Un-tiling restores
    /// the remembered floating rect; re-tiling enters the tree where the window
    /// currently sits (the leaf under its center, split along its longer axis).
    fn toggle_float(&mut self) {
        let Some(id) = self.focused else { return };
        if self.tree.contains(id) {
            self.detach(id);
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.rect = w.prev.take().unwrap_or(egui::Rect::from_min_size(
                    egui::pos2(60.0, 60.0),
                    egui::vec2(580.0, 380.0),
                ));
            }
        } else {
            let (center, rect) = match self.windows.iter().find(|w| w.id == id) {
                Some(w) => (w.rect.center(), w.rect),
                None => return,
            };
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.prev = Some(rect);
            }
            let local = egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area);
            match self.tree.hit_leaf(center, local, SNAP_GAP) {
                Some((leaf, r)) => {
                    let side = if r.width() >= r.height() { Dir::Right } else { Dir::Down };
                    self.tree.insert_split(leaf, id, side);
                }
                None => self.tree.insert_root(id, Dir::Right),
            }
        }
        self.focus(id);
    }

    /// Move focus to the nearest window in direction `d`, by geometry on local
    /// rects: among windows whose center lies in the requested half-plane, pick
    /// the one minimizing (dominant-axis distance, then cross-axis distance).
    fn focus_dir(&mut self, d: Dir) {
        let Some(cur) = self.focused else {
            // No focus yet: focus the top-most visible window.
            if let Some(id) = self
                .windows
                .iter()
                .filter(|w| !w.minimized)
                .max_by_key(|w| w.z)
                .map(|w| w.id)
            {
                self.focus(id);
            }
            return;
        };
        let Some(from) = self
            .windows
            .iter()
            .find(|w| w.id == cur)
            .map(|w| w.rect.center())
        else {
            return;
        };

        let mut best: Option<(WinId, f32, f32)> = None;
        for w in self.windows.iter().filter(|w| !w.minimized && w.id != cur) {
            let c = w.rect.center();
            let dx = c.x - from.x;
            let dy = c.y - from.y;
            let (along, cross) = match d {
                Dir::Left => (-dx, dy.abs()),
                Dir::Right => (dx, dy.abs()),
                Dir::Up => (-dy, dx.abs()),
                Dir::Down => (dy, dx.abs()),
            };
            // Must lie meaningfully in the requested direction.
            if along <= 1.0 {
                continue;
            }
            // Prefer candidates roughly in line (cross small) and nearer (along
            // small): rank by along + a cross penalty so a window directly in the
            // direction beats one far off-axis.
            let score = along + cross * 2.0;
            if best.map_or(true, |(_, b, _)| score < b) {
                best = Some((w.id, score, cross));
            }
        }
        if let Some((id, _, _)) = best {
            self.focus(id);
        }
    }

    /// Hit-test the pointer (screen coords) against the windows in `order`
    /// (back-to-front draw order), returning the index of the *top-most* window —
    /// other than `src` — whose **titlebar** contains the pointer. Dropping a
    /// dragged window's title onto another window's titlebar tabs (merges) it onto
    /// that window's stack. Requiring the *titlebar* (not the whole body) makes
    /// merge a deliberate gesture, so ordinary repositioning that happens to
    /// overlap another window does not accidentally merge. Skips `src` so a window
    /// can never be merged onto itself.
    fn merge_target_at(
        &self,
        src: WinId,
        p: egui::Pos2,
        area: egui::Rect,
        order: &[usize],
    ) -> Option<usize> {
        // `order` is back-to-front; iterate in reverse for top-most-first.
        for &j in order.iter().rev() {
            let w = &self.windows[j];
            if w.id == src || w.minimized {
                continue;
            }
            let scr = w.rect.translate(area.min.to_vec2());
            let titlebar = egui::Rect::from_min_size(scr.min, egui::vec2(scr.width(), TITLE_H));
            if titlebar.contains(p) {
                return Some(j);
            }
        }
        None
    }

    /// Append an `exited (code)` marker to terminals whose process ended. Runs
    /// over every tab (not just visible ones) so background agents update too.
    /// Entry point is the desktop manager (gated in `show`); project managers
    /// are reached through the `Content::Project` recursion below.
    fn refresh_exit_titles(&mut self) {
        let chat = Rc::clone(&self.chat);
        for w in &mut self.windows {
            let wid = w.id;
            for t in &mut w.tabs {
                match &mut t.content {
                    Content::Terminal(s) => {
                        if let Some(code) = s.exit_to_note() {
                            if t.chat_member {
                                // Name must be read BEFORE the marker is appended.
                                chat.borrow_mut().sys(
                                    crate::chat::ChatKind::Exited,
                                    &term_tag(wid),
                                    display_name(&t.title),
                                );
                            }
                            t.title.push_str(&format!("  ·  exited ({code})"));
                        }
                    }
                    Content::Project(wm) => wm.refresh_exit_titles(),
                    Content::Chat(_) => {} // no process, no exit marker
                }
            }
        }
    }

    /// Returns whether any window in this manager was interacted with this frame.
    /// The parent uses this to propagate focus upward: clicking a sub-window in a
    /// background project bubbles up and switches the desktop to that project.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        area: egui::Rect,
        active: bool,
        base: egui::Id,
    ) -> bool {
        // Record the area so keyboard-driven zoom/snap can commit to a sensible
        // rect before the next render refits it.
        self.last_area = area.size();

        if self.desktop {
            self.refresh_exit_titles();
        }

        // Fill the chat viewer's crew rows / title chip before any window
        // draws. Project managers refresh their own viewer; on the desktop
        // manager this scans windows once and finds no viewer.
        self.refresh_chat_view();

        self.pump_commands(ui, active);

        ui.painter_at(area)
            .rect_filled(area, egui::CornerRadius::ZERO, DESK_BG);

        let focused = self.focused;
        let asz = area.size();
        let mut order: Vec<usize> = (0..self.windows.len())
            .filter(|&i| !self.windows[i].minimized)
            .collect();
        order.sort_by_key(|&i| self.windows[i].z);

        let placements: std::collections::HashMap<WinId, egui::Rect> = self
            .tree
            .layout(egui::Rect::from_min_size(egui::Pos2::ZERO, asz), SNAP_GAP)
            .into_iter()
            .collect();
        // zoomed window renders last (on top of the tiles)
        if let Some(zid) = self.zoomed {
            if let Some(pos) = order.iter().position(|&i| self.windows[i].id == zid) {
                let v = order.remove(pos);
                order.push(v);
            }
        }

        let mut acts: Vec<Act> = vec![];
        // overlay rect (screen coords) for the snap zone of the window being dragged
        let mut snap_overlay: Option<egui::Rect> = None;
        // index (into self.windows) of a window the dragged title is hovering as a
        // merge (tab) target; painted with a highlight to telegraph the drop.
        let mut merge_hint: Option<usize> = None;

        for &i in &order {
            let id = self.windows[i].id;
            // While the directory picker is open, no window is active — this stops the
            // focused terminal from also consuming the keystrokes meant for the picker.
            // While renaming, no window is active so the typed title doesn't also
            // leak into the focused terminal (which reads raw input events).
            let is_focus = focused == Some(id)
                && active
                && self.picker.is_none()
                && self.renaming.is_none()
                && self.settings.is_none();
            let is_project = self.windows[i].is_project();
            let is_renaming = self.renaming == Some(id);
            // Keep backgrounded tabs (everything but the active tab) alive: their
            // PTYs are drained / device queries answered even though they are not
            // drawn this frame. The active tab is pumped by its own render below.
            self.windows[i].keepalive_inactive();

            // Re-fit to the (possibly resized) area every frame: the zoomed window
            // takes the full area, tiled windows take their rect from the layout
            // tree, floating windows clamp back in.
            let is_tiled = placements.contains_key(&id);
            {
                let zoomed = self.zoomed;
                let w = &mut self.windows[i];
                if zoomed == Some(w.id) {
                    w.rect = egui::Rect::from_min_size(egui::Pos2::ZERO, asz).shrink(SNAP_GAP);
                } else if let Some(r) = placements.get(&w.id) {
                    w.rect = *r;
                } else {
                    clamp(&mut w.rect, asz);
                }
            }
            let mut scr = self.windows[i].rect.translate(area.min.to_vec2());
            // Projects reserve extra right-side room for the "+" new-project button.
            let ctl_w = if is_project { 116.0 } else { 88.0 };

            // --- title drag (interact first, then we know final position) ---
            let drag_rect = egui::Rect::from_min_size(
                scr.min,
                egui::vec2((scr.width() - ctl_w).max(0.0), TITLE_H),
            );
            let dr = ui.interact(
                drag_rect,
                base.with((id, "drag")),
                egui::Sense::click_and_drag(),
            );
            if dr.drag_started() || dr.clicked() {
                acts.push(Act::Focus(id));
            }
            if dr.double_clicked() {
                // Double-clicking the name edits it inline; elsewhere on the bar
                // still maximizes/restores.
                let title_w = ui
                    .painter()
                    .layout_no_wrap(
                        self.windows[i].title().to_string(),
                        egui::FontId::proportional(12.5),
                        TEXT,
                    )
                    .size()
                    .x;
                let name_rect =
                    egui::Rect::from_min_size(scr.min, egui::vec2(title_w + 22.0, TITLE_H));
                let on_name = dr
                    .interact_pointer_pos()
                    .is_some_and(|p| name_rect.contains(p));
                if on_name {
                    self.renaming = Some(id);
                    self.rename_buf = self.windows[i].title().to_string();
                    self.rename_focus = true;
                } else {
                    acts.push(Act::Max(id));
                }
            }
            if dr.dragged() {
                let popped = self.tree.contains(id) || self.zoomed == Some(id);
                if popped {
                    self.detach(id);
                }
                {
                    let w = &mut self.windows[i];
                    // Dragging tears a tiled/zoomed window out to floating. Like
                    // double-click/restore, it returns to its pre-tile size; we re-anchor
                    // the restored rect under the cursor so the title stays grabbed.
                    if popped {
                        if let (Some(pr), Some(p)) = (w.prev.take(), ui.ctx().pointer_latest_pos())
                        {
                            let local = p - area.min.to_vec2();
                            let frac = if w.rect.width() > 0.0 {
                                ((local.x - w.rect.min.x) / w.rect.width()).clamp(0.0, 1.0)
                            } else {
                                0.5
                            };
                            w.rect = egui::Rect::from_min_size(
                                egui::pos2(local.x - frac * pr.width(), local.y - TITLE_H * 0.5),
                                pr.size(),
                            );
                        }
                    }
                    w.rect = w.rect.translate(dr.drag_delta());
                    clamp(&mut w.rect, asz);
                }
                scr = self.windows[i].rect.translate(area.min.to_vec2());

                // --- merge target detection: is the pointer over another window? ---
                // Dropping a window's title onto another window tabs it onto that
                // window's stack. While hovering a merge target we suppress the snap
                // overlay and instead highlight the target (handled at paint time).
                let pointer = ui.ctx().pointer_latest_pos();
                let over_target = pointer.and_then(|p| self.merge_target_at(id, p, area, &order));
                if let Some(tgt) = over_target {
                    merge_hint = Some(tgt);
                } else if let Some(p) = pointer {
                    // Tree drop hint: leaf edges split, leaf centers tab-merge,
                    // area edge bands split the root. Painted like the old snap overlay.
                    if let Some((_, hint)) = self.tree.drop_target(p, area, SNAP_GAP) {
                        snap_overlay = Some(hint);
                    }
                }
            }
            if dr.drag_stopped() {
                let pointer = ui.ctx().pointer_latest_pos();
                // A drop onto another window's titlebar merges (tabs) onto it and wins
                // over the tree drop: the dragged window is consumed entirely.
                let merge_dst = pointer.and_then(|p| self.merge_target_at(id, p, area, &order));
                if let Some(dst_i) = merge_dst {
                    let dst = self.windows[dst_i].id;
                    acts.push(Act::Merge { src: id, dst });
                } else if let Some(p) = pointer {
                    if let Some((target, _)) = self.tree.drop_target(p, area, SNAP_GAP) {
                        match target {
                            crate::layout::DropTarget::Tab(dst) => {
                                acts.push(Act::Merge { src: id, dst });
                            }
                            crate::layout::DropTarget::Split(t, side) => {
                                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                                    if w.prev.is_none() {
                                        w.prev = Some(w.rect);
                                    }
                                }
                                self.tree.insert_split(t, id, side);
                            }
                            crate::layout::DropTarget::Root(side) => {
                                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                                    if w.prev.is_none() {
                                        w.prev = Some(w.rect);
                                    }
                                }
                                self.tree.insert_root(id, side);
                            }
                        }
                        // Rect refits from the tree next frame (one frame at the drop
                        // position — invisible at 60fps; intentionally no immediate set).
                    }
                }
            }

            let title_rect = egui::Rect::from_min_size(scr.min, egui::vec2(scr.width(), TITLE_H));
            let content_rect = egui::Rect::from_min_max(
                egui::pos2(scr.min.x + 1.0, scr.min.y + TITLE_H),
                egui::pos2(scr.max.x - 1.0, scr.max.y - 1.0),
            );

            // --- paint window ---
            // Tiled/zoomed windows square their corners so they tile flush to
            // the area edges and to each other (rounded corners would leave gaps).
            let cr = if is_tiled || self.zoomed == Some(id) {
                egui::CornerRadius::ZERO
            } else {
                egui::CornerRadius::same(6)
            };
            let p = ui.painter_at(scr.intersect(area));
            p.rect_filled(scr, cr, WIN_BG);
            let (tbg, tbg_focus) = if is_project {
                (PROJ_TITLE_BG, PROJ_TITLE_BG_FOCUS)
            } else {
                (TITLE_BG, TITLE_BG_FOCUS)
            };
            p.rect_filled(title_rect, cr, if is_focus { tbg_focus } else { tbg });
            // Right edge of the title/tab area; the project shell chips anchor here
            // so they never overlap a multi-tab bar. Set by each titlebar branch.
            let title_end_x;
            if is_renaming {
                // Field box centered in the titlebar; `vertical_align(Center)` lets
                // egui center the text within it, so no pixel-fudging is needed.
                let te_h = TITLE_H - 8.0;
                let te_rect = egui::Rect::from_min_size(
                    egui::pos2(scr.min.x + 8.0, scr.min.y + (TITLE_H - te_h) * 0.5),
                    egui::vec2((scr.width() - ctl_w - 14.0).max(40.0), te_h),
                );
                title_end_x = te_rect.max.x + 8.0;
                // Theme the field to the dark/amber titlebar instead of egui's
                // default light TextEdit: dark inset fill + amber edit-mode border.
                p.rect_filled(te_rect, egui::CornerRadius::same(3), WIN_BG);
                p.rect_stroke(
                    te_rect,
                    egui::CornerRadius::same(3),
                    egui::Stroke::new(1.0, BORDER_FOCUS),
                    egui::StrokeKind::Inside,
                );
                ui.visuals_mut().selection.bg_fill =
                    egui::Color32::from_rgba_unmultiplied(231, 169, 63, 90);
                let resp = ui.put(
                    te_rect,
                    egui::TextEdit::singleline(&mut self.rename_buf)
                        .id(base.with((id, "rename")))
                        .font(egui::FontId::proportional(12.5))
                        .text_color(TEXT)
                        .vertical_align(egui::Align::Center)
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::symmetric(6, 0))
                        .desired_width(te_rect.width()),
                );
                if self.rename_focus {
                    resp.request_focus();
                    self.rename_focus = false;
                }
                // Escape cancels; Enter or clicking away (lost focus) commits.
                if ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                    self.renaming = None;
                } else if resp.lost_focus() {
                    let t = self.rename_buf.trim().to_string();
                    if !t.is_empty() {
                        let a = self.windows[i].active;
                        self.windows[i].tabs[a].title = t;
                    }
                    self.renaming = None;
                }
            } else if self.windows[i].tabs.len() > 1 {
                // --- tab bar (multi-tab stacks only) ---
                // Drawn inside the titlebar (respecting TITLE_H): one chip per tab,
                // active highlighted, each with a small close affordance. Chips are
                // registered after the window-drag rect so they win pointer priority;
                // dragging a chip off the bar detaches it (untab).
                let ntabs = self.windows[i].tabs.len();
                let tab_font = egui::FontId::proportional(11.5);
                let chip_h = TITLE_H - 6.0;
                let cy = scr.min.y + 3.0;
                let mut cx = scr.min.x + 6.0;
                let avail_end = scr.max.x - ctl_w;
                for ti in 0..ntabs {
                    let is_active_tab = self.windows[i].active == ti;
                    let label = self.windows[i].tabs[ti].title.clone();
                    let tw = ui
                        .painter()
                        .layout_no_wrap(label.clone(), tab_font.clone(), TEXT)
                        .size()
                        .x
                        .min(120.0);
                    let close_w = 16.0;
                    let chip_w = tw + 12.0 + close_w;
                    if cx + chip_w > avail_end && ti > 0 {
                        // Out of room: stop drawing further chips (rare; many tabs on
                        // a narrow window). The active tab is always within the first
                        // few, and cycling still reaches the rest.
                        break;
                    }
                    let chip =
                        egui::Rect::from_min_size(egui::pos2(cx, cy), egui::vec2(chip_w, chip_h));
                    let chip_resp = ui.interact(
                        chip,
                        base.with((id, "tab", ti)),
                        egui::Sense::click_and_drag(),
                    );
                    let bg = if is_active_tab {
                        if is_focus { TITLE_BG_FOCUS } else { TITLE_BG }
                    } else if chip_resp.hovered() {
                        egui::Color32::from_rgb(50, 45, 35)
                    } else {
                        egui::Color32::from_rgb(38, 34, 27)
                    };
                    p.rect_filled(chip, egui::CornerRadius::same(4), bg);
                    if is_active_tab {
                        p.rect_stroke(
                            chip,
                            egui::CornerRadius::same(4),
                            egui::Stroke::new(1.0, BORDER_FOCUS),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let txt_col = if is_active_tab && is_focus { TEXT } else { DIM };
                    p.text(
                        egui::pos2(cx + 7.0, cy + chip_h / 2.0),
                        egui::Align2::LEFT_CENTER,
                        &label,
                        tab_font.clone(),
                        txt_col,
                    );
                    // per-tab close affordance (small ×)
                    let xr = egui::Rect::from_min_size(
                        egui::pos2(cx + chip_w - close_w, cy),
                        egui::vec2(close_w, chip_h),
                    );
                    let xresp = ui.interact(xr, base.with((id, "tabx", ti)), egui::Sense::click());
                    let xc = xr.center();
                    let xs = 3.0;
                    let xcol = if xresp.hovered() {
                        egui::Color32::from_rgb(220, 120, 100)
                    } else {
                        txt_col
                    };
                    let xstroke = egui::Stroke::new(1.2, xcol);
                    let pp = ui.painter();
                    pp.line_segment(
                        [
                            egui::pos2(xc.x - xs, xc.y - xs),
                            egui::pos2(xc.x + xs, xc.y + xs),
                        ],
                        xstroke,
                    );
                    pp.line_segment(
                        [
                            egui::pos2(xc.x - xs, xc.y + xs),
                            egui::pos2(xc.x + xs, xc.y - xs),
                        ],
                        xstroke,
                    );
                    if xresp.clicked() {
                        acts.push(Act::CloseTab(id, ti));
                    } else if chip_resp.clicked() {
                        acts.push(Act::SetTab(id, ti));
                    } else if chip_resp.dragged() {
                        // Live drag-out: the instant the pointer leaves the tab bar,
                        // detach the tab into its own floating window and hand the
                        // drag to that window (`grab`) so it pops to floating size and
                        // follows the cursor immediately — no wait for release.
                        if let Some(dp) = ui.ctx().pointer_latest_pos() {
                            if tab_drag_off(dp, scr) {
                                let local = dp - area.min.to_vec2();
                                acts.push(Act::Untab {
                                    id,
                                    idx: ti,
                                    pos: egui::pos2(local.x, local.y),
                                    grab: true,
                                });
                            }
                        }
                    } else if chip_resp.drag_stopped() {
                        // Released without ever crossing off the bar (e.g. a tiny
                        // flick the live path never caught): off → detach in place,
                        // else just activate the tab.
                        if let Some(dp) = ui.ctx().pointer_latest_pos() {
                            if tab_drag_off(dp, scr) {
                                let local = dp - area.min.to_vec2();
                                acts.push(Act::Untab {
                                    id,
                                    idx: ti,
                                    pos: egui::pos2(local.x, local.y),
                                    grab: false,
                                });
                            } else {
                                acts.push(Act::SetTab(id, ti));
                            }
                        }
                    }
                    cx += chip_w + 4.0;
                }
                title_end_x = cx + 6.0;
            } else {
                let tw = ui
                    .painter()
                    .layout_no_wrap(
                        self.windows[i].title().to_string(),
                        egui::FontId::proportional(12.5),
                        TEXT,
                    )
                    .size()
                    .x;
                p.text(
                    egui::pos2(scr.min.x + 11.0, scr.min.y + TITLE_H / 2.0),
                    egui::Align2::LEFT_CENTER,
                    self.windows[i].title(),
                    egui::FontId::proportional(12.5),
                    if is_focus { TEXT } else { DIM },
                );
                title_end_x = scr.min.x + 11.0 + tw + 14.0;
            }

            // --- dispatch keys (project headers only) ---
            // Compact "PS · CMD · SH" stamped after the title: clicking one spawns
            // a terminal of that shell *into this project*. Lives here (not the
            // global bar) so the target site is unambiguous — the window you click.
            if is_project {
                let kh = TITLE_H - 10.0;
                let ky = scr.min.y + 5.0;
                let mut kx = title_end_x;
                let key_font = egui::FontId::proportional(10.5);
                for (label, shell) in [
                    ("PS", Shell::PowerShell),
                    ("CMD", Shell::Cmd),
                    ("SH", Shell::Bash),
                ] {
                    let tw = ui
                        .painter()
                        .layout_no_wrap(label.to_owned(), key_font.clone(), TEXT)
                        .size()
                        .x;
                    let kw = tw + 12.0;
                    // keep keys from colliding with the window controls on narrow windows
                    if kx + kw > scr.max.x - ctl_w {
                        break;
                    }
                    let r = egui::Rect::from_min_size(egui::pos2(kx, ky), egui::vec2(kw, kh));
                    let kresp =
                        ui.interact(r, base.with((id, "disp", label)), egui::Sense::click());
                    let kbg = if kresp.hovered() {
                        egui::Color32::from_rgb(72, 82, 76)
                    } else {
                        egui::Color32::from_rgb(45, 51, 48)
                    };
                    ui.painter()
                        .rect_filled(r, egui::CornerRadius::same(3), kbg);
                    ui.painter().text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        key_font.clone(),
                        if is_focus { TEXT } else { DIM },
                    );
                    if kresp.clicked() {
                        acts.push(Act::AddTerm(id, shell));
                    }
                    kx += kw + 5.0;
                }
            }

            // --- window controls ---
            let by = scr.min.y + 3.0;
            let bh = TITLE_H - 6.0;
            let mut bx = scr.max.x - 4.0 - 22.0;
            for (role, danger) in [("close", true), ("max", false), ("min", false)] {
                let r = egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(22.0, bh));
                let resp = ui.interact(r, base.with((id, role)), egui::Sense::click());
                let bg = if resp.hovered() {
                    if danger {
                        egui::Color32::from_rgb(120, 45, 36)
                    } else {
                        egui::Color32::from_rgb(72, 64, 50)
                    }
                } else {
                    egui::Color32::TRANSPARENT
                };
                ui.painter().rect_filled(r, egui::CornerRadius::same(4), bg);
                // Icons are drawn as vector strokes (not font glyphs) so all three
                // share one optical center, size, and weight regardless of font.
                let c = r.center();
                let s = 4.0; // icon half-extent
                let stroke = egui::Stroke::new(1.4, if is_focus { TEXT } else { DIM });
                let p = ui.painter();
                match role {
                    "min" => {
                        p.line_segment(
                            [egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)],
                            stroke,
                        );
                    }
                    "max" => {
                        p.rect_stroke(
                            egui::Rect::from_center_size(c, egui::vec2(s * 2.0, s * 2.0)),
                            egui::CornerRadius::same(1),
                            stroke,
                            egui::StrokeKind::Inside,
                        );
                    }
                    _ => {
                        p.line_segment(
                            [egui::pos2(c.x - s, c.y - s), egui::pos2(c.x + s, c.y + s)],
                            stroke,
                        );
                        p.line_segment(
                            [egui::pos2(c.x - s, c.y + s), egui::pos2(c.x + s, c.y - s)],
                            stroke,
                        );
                    }
                }
                if resp.clicked() {
                    acts.push(match role {
                        "close" => Act::Close(id),
                        "max" => Act::Max(id),
                        _ => Act::Min(id),
                    });
                }
                bx -= 25.0;
            }

            // --- new-project button (project titlebars only) ---
            // Sits just left of the window controls; spawns a sibling project on
            // the desktop. Replaces the old global "+ project" header button.
            if is_project {
                let r = egui::Rect::from_min_size(egui::pos2(bx - 4.0, by), egui::vec2(22.0, bh));
                let resp = ui.interact(r, base.with((id, "addproj")), egui::Sense::click());
                let bg = if resp.hovered() {
                    egui::Color32::from_rgb(72, 64, 50)
                } else {
                    egui::Color32::TRANSPARENT
                };
                ui.painter().rect_filled(r, egui::CornerRadius::same(4), bg);
                let c = r.center();
                let s = 4.0;
                let stroke = egui::Stroke::new(1.4, if is_focus { TEXT } else { DIM });
                let p = ui.painter();
                p.line_segment([egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)], stroke);
                p.line_segment([egui::pos2(c.x, c.y - s), egui::pos2(c.x, c.y + s)], stroke);
                if resp.clicked() {
                    acts.push(Act::OpenProjectPicker);
                }
            }

            // --- content ---
            // Terminals need click_and_drag (for text selection); projects only
            // sense clicks so drags pass through to their own sub-windows.
            let sense = if is_project {
                egui::Sense::click()
            } else {
                egui::Sense::click_and_drag()
            };
            let cresp = ui.interact(content_rect, base.with((id, "content")), sense);
            if cresp.clicked() {
                acts.push(Act::Focus(id));
            }
            let child_interacted =
                self.windows[i]
                    .active_content()
                    .show(ui, content_rect, is_focus, base, id, &cresp);
            if child_interacted {
                // A sub-window inside this project was clicked: raise this project
                // to focus so the keyboard cascade reaches it. This also makes
                // `acts` non-empty, propagating the interaction further up.
                acts.push(Act::Focus(id));
            }

            // --- border + resize ---
            let border_col = if is_focus {
                if is_project {
                    PROJ_BORDER_FOCUS
                } else {
                    BORDER_FOCUS
                }
            } else {
                BORDER
            };
            ui.painter_at(area).rect_stroke(
                scr,
                cr,
                egui::Stroke::new(BORDER_W, border_col),
                egui::StrokeKind::Inside,
            );
            // --- resize: 8 invisible bands around the frame (4 edges + 4 corners) ---
            // Registered last so they take pointer priority over content/title in the
            // thin RESIZE_BAND frame. Floating windows resize freely on any edge; a
            // tiled window's interior edge (shared with a neighbour) drags the tree
            // divider so the tiles resize together, while outer edges are inert
            // (tear-out lives on the header drag). Zoomed windows don't resize.
            let bnd = RESIZE_BAND;
            let (x0, y0, x1, y1) = (scr.min.x, scr.min.y, scr.max.x, scr.max.y);
            type Ci = egui::CursorIcon;
            // (key, rect, left, right, top, bottom, cursor)
            let handles: [(&str, egui::Rect, bool, bool, bool, bool, Ci); 8] = [
                (
                    "w",
                    egui::Rect::from_min_max(
                        egui::pos2(x0, y0 + bnd),
                        egui::pos2(x0 + bnd, y1 - bnd),
                    ),
                    true,
                    false,
                    false,
                    false,
                    Ci::ResizeWest,
                ),
                (
                    "e",
                    egui::Rect::from_min_max(
                        egui::pos2(x1 - bnd, y0 + bnd),
                        egui::pos2(x1, y1 - bnd),
                    ),
                    false,
                    true,
                    false,
                    false,
                    Ci::ResizeEast,
                ),
                (
                    "n",
                    egui::Rect::from_min_max(
                        egui::pos2(x0 + bnd, y0),
                        egui::pos2(x1 - bnd, y0 + bnd),
                    ),
                    false,
                    false,
                    true,
                    false,
                    Ci::ResizeNorth,
                ),
                (
                    "s",
                    egui::Rect::from_min_max(
                        egui::pos2(x0 + bnd, y1 - bnd),
                        egui::pos2(x1 - bnd, y1),
                    ),
                    false,
                    false,
                    false,
                    true,
                    Ci::ResizeSouth,
                ),
                (
                    "nw",
                    egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x0 + bnd, y0 + bnd)),
                    true,
                    false,
                    true,
                    false,
                    Ci::ResizeNorthWest,
                ),
                (
                    "ne",
                    egui::Rect::from_min_max(egui::pos2(x1 - bnd, y0), egui::pos2(x1, y0 + bnd)),
                    false,
                    true,
                    true,
                    false,
                    Ci::ResizeNorthEast,
                ),
                (
                    "sw",
                    egui::Rect::from_min_max(egui::pos2(x0, y1 - bnd), egui::pos2(x0 + bnd, y1)),
                    true,
                    false,
                    false,
                    true,
                    Ci::ResizeSouthWest,
                ),
                (
                    "se",
                    egui::Rect::from_min_max(egui::pos2(x1 - bnd, y1 - bnd), egui::pos2(x1, y1)),
                    false,
                    true,
                    false,
                    true,
                    Ci::ResizeSouthEast,
                ),
            ];
            for (key, hr, hl, hrr, ht, hb, cursor) in handles {
                let resp = ui.interact(hr, base.with((id, "rsz", key)), egui::Sense::drag());
                if resp.hovered() || resp.dragged() {
                    // Only advertise a resize that can actually happen: tiled
                    // windows resize on interior dividers only; zoomed never.
                    let usable = if self.zoomed == Some(id) {
                        false
                    } else if self.tree.contains(id) {
                        (hl && self.tree.has_divider(id, Dir::Left))
                            || (hrr && self.tree.has_divider(id, Dir::Right))
                            || (ht && self.tree.has_divider(id, Dir::Up))
                            || (hb && self.tree.has_divider(id, Dir::Down))
                    } else {
                        true
                    };
                    if usable {
                        ui.ctx().set_cursor_icon(cursor);
                    }
                }
                if resp.drag_started() {
                    acts.push(Act::Focus(id));
                }
                if !resp.dragged() {
                    continue;
                }
                let d = resp.drag_delta();
                if self.zoomed == Some(id) {
                    continue; // zoomed windows render full-area; resizing is meaningless
                }
                if self.tree.contains(id) {
                    // Tiled: each edge maps to the divider it shares with a neighbour
                    // (resize_edge no-ops on outer edges). Corners drive both axes.
                    let local = egui::Rect::from_min_size(egui::Pos2::ZERO, asz);
                    if hl {
                        self.tree.resize_edge(id, Dir::Left, d.x, local, SNAP_GAP);
                    }
                    if hrr {
                        self.tree.resize_edge(id, Dir::Right, d.x, local, SNAP_GAP);
                    }
                    if ht {
                        self.tree.resize_edge(id, Dir::Up, d.y, local, SNAP_GAP);
                    }
                    if hb {
                        self.tree.resize_edge(id, Dir::Down, d.y, local, SNAP_GAP);
                    }
                } else {
                    resize_floating(&mut self.windows[i].rect, d, hl, hrr, ht, hb, asz);
                }
            }
        }

        self.paint_drag_overlays(ui, area, snap_overlay, merge_hint);
        self.paint_taskbar(ui, area, base, &mut acts);

        // Any Act means a window in this manager was interacted with this frame.
        // Captured before the apply loop consumes `acts`, returned at the end so
        // the parent can bubble focus upward through arbitrary nesting depth.
        let interacted = !acts.is_empty();

        let ctx = ui.ctx().clone();
        self.apply_acts(acts, asz, base, &ctx);
        // After the acts, not before: the same click that recorded a crew-row
        // hit also pushed Act::Focus(chat window) via cresp — draining last is
        // the fixed order that lets the member, not the viewer, end up focused.
        self.drain_chat_clicks();
        self.drain_chat_posts();
        self.show_modals(ui, area, &ctx);

        interacted
    }

    /// Leader / command mode: only the root desktop runs it, and only while it is
    /// the active (keyboard-owning) manager and no modal is up. Resolved and
    /// dispatched *before* the render recursion so command chords are drained from
    /// egui input and never reach a terminal's read_input.
    fn pump_commands(&mut self, ui: &mut egui::Ui, active: bool) {
        if self.desktop
            && active
            && self.picker.is_none()
            && self.renaming.is_none()
            && self.settings.is_none()
            // Any focused text field (chat input, rename) owns the keyboard — leader stays dormant.
            && ui.ctx().memory(|m| m.focused().is_none())
        {
            if let Some(cmd) = self.pump_leader(ui) {
                self.dispatch(cmd, ui);
            }
        }
    }

    /// Overlays painted above all windows while a title drag is in flight: the
    /// amber snap-zone preview, and (mutually exclusive) the merge/tab drop hint
    /// highlighting the window the pointer is over.
    fn paint_drag_overlays(
        &self,
        ui: &egui::Ui,
        area: egui::Rect,
        snap_overlay: Option<egui::Rect>,
        merge_hint: Option<usize>,
    ) {
        // --- snap overlay (amber), painted above all windows while dragging ---
        if let Some(ov) = snap_overlay {
            let p = ui.painter_at(area);
            let r = ov.intersect(area);
            p.rect_filled(r, egui::CornerRadius::same(8), SNAP_FILL);
            p.rect_stroke(
                r,
                egui::CornerRadius::same(8),
                egui::Stroke::new(1.5, SNAP_STROKE),
                egui::StrokeKind::Inside,
            );
        }

        // --- merge (tab) drop hint: highlight the target window while hovering ---
        if let Some(j) = merge_hint {
            let r = self.windows[j]
                .rect
                .translate(area.min.to_vec2())
                .intersect(area);
            let p = ui.painter_at(area);
            p.rect_filled(r, egui::CornerRadius::same(8), SNAP_FILL);
            p.rect_stroke(
                r,
                egui::CornerRadius::same(8),
                egui::Stroke::new(2.0, SNAP_STROKE),
                egui::StrokeKind::Inside,
            );
        }
    }

    /// Bottom-left taskbar of minimized windows; clicking a chip restores it.
    fn paint_taskbar(
        &self,
        ui: &mut egui::Ui,
        area: egui::Rect,
        base: egui::Id,
        acts: &mut Vec<Act>,
    ) {
        let mins: Vec<(WinId, String)> = self
            .windows
            .iter()
            .filter(|w| w.minimized)
            .map(|w| (w.id, w.title().to_string()))
            .collect();
        if mins.is_empty() {
            return;
        }
        let mut tx = area.min.x + 8.0;
        let ty = area.max.y - 30.0;
        for (id, title) in mins {
            let r = egui::Rect::from_min_size(egui::pos2(tx, ty), egui::vec2(160.0, 24.0));
            let resp = ui.interact(r, base.with((id, "task")), egui::Sense::click());
            ui.painter().rect_filled(
                r,
                egui::CornerRadius::same(5),
                egui::Color32::from_rgb(42, 38, 29),
            );
            ui.painter().text(
                egui::pos2(tx + 9.0, ty + 12.0),
                egui::Align2::LEFT_CENTER,
                &title,
                egui::FontId::proportional(11.5),
                DIM,
            );
            if resp.clicked() {
                acts.push(Act::Restore(id));
            }
            tx += 168.0;
        }
    }

    /// Deferred window mutations collected during render, applied after the render
    /// borrow on `self.windows` is released so we never remove/retab a window
    /// mid-loop and invalidate the draw order.
    fn apply_acts(&mut self, acts: Vec<Act>, _asz: egui::Vec2, base: egui::Id, ctx: &egui::Context) {
        for a in acts {
            match a {
                Act::Focus(id) => self.focus(id),
                Act::AddTerm(id, shell) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        if let Content::Project(wm) = w.active_content() {
                            let anchor = wm.focused;
                            if let Some(nid) = wm.add_terminal(shell, ctx) {
                                wm.tile_new(nid, anchor);
                            }
                        }
                    }
                    self.focus(id);
                }
                Act::OpenProjectPicker => {
                    self.picker = Some(DirPicker::new(self.picker_start()));
                }
                // The titlebar close control closes the *active tab* — which closes
                // the whole window only when it was the last tab.
                Act::Close(id) => self.close_active_tab(id),
                Act::SetTab(id, idx) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        if idx < w.tabs.len() {
                            w.active = idx;
                        }
                    }
                    self.focus(id);
                }
                Act::CloseTab(id, idx) => self.close_tab(id, idx),
                Act::Merge { src, dst } => self.merge_windows(src, dst),
                Act::Untab { id, idx, pos, grab } => {
                    if let Some(new_id) = self.untab(id, idx, pos) {
                        if grab {
                            // Hand the live pointer drag to the new window's title so
                            // the detached window keeps following the cursor this
                            // gesture (egui reports it dragged next frame).
                            ctx.set_dragged_id(base.with((new_id, "drag")));
                        }
                    }
                }
                Act::Min(id) => {
                    self.detach(id);
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        w.minimized = true;
                    }
                    if self.focused == Some(id) {
                        self.focused = None;
                    }
                }
                Act::Restore(id) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        w.minimized = false;
                    }
                    self.focus(id);
                }
                Act::Max(id) => self.toggle_zoom(id),
            }
        }
    }

    /// Desktop-level modal overlays drawn last, on top of everything: the dir
    /// picker, the keybindings editor, and the leader cue / help cheat-sheet.
    fn show_modals(&mut self, ui: &mut egui::Ui, area: egui::Rect, ctx: &egui::Context) {
        if let Some(mut picker) = self.picker.take() {
            match picker.show(ui) {
                Outcome::Pending => self.picker = Some(picker),
                Outcome::Cancelled => {}
                Outcome::Accepted(path) => {
                    let anchor = self.focused;
                    let nid = self.add_project(Shell::PowerShell, path, ctx);
                    self.tile_new(nid, anchor);
                }
            }
        }

        // --- keybindings editor modal (desktop only) ---
        // The editor reads input itself; afterwards we swallow every keyboard
        // event for the frame so nothing the editor didn't consume can leak to a
        // terminal — the same capture discipline as the picker / help overlay.
        if let Some(mut settings) = self.settings.take() {
            let outcome = settings.show(ui, &mut self.keymap);
            match outcome {
                SettingsOutcome::Close => { /* drop it: closed */ }
                SettingsOutcome::Changed => {
                    if let Err(e) = self.keymap.save() {
                        settings.set_save_error(e);
                    }
                    self.settings = Some(settings);
                }
                SettingsOutcome::Pending => self.settings = Some(settings),
            }
            self.swallow_input(ui);
        }

        // --- leader visual cue + help overlay (desktop only) ---
        if self.desktop {
            if self.armed {
                self.paint_armed_pill(ui, area);
            }
            if self.show_help {
                self.paint_help(ui, area);
            }
        }
    }

    /// A small amber pill in the bottom-right while command mode is armed, so the
    /// leader press is visibly acknowledged.
    fn paint_armed_pill(&self, ui: &egui::Ui, area: egui::Rect) {
        let text = format!("PREFIX  {}", self.keymap.leader.pretty());
        let text = text.as_str();
        let font = egui::FontId::monospace(11.5);
        let p = ui.painter_at(area);
        let galley = p.layout_no_wrap(text.to_string(), font.clone(), egui::Color32::BLACK);
        let pad = egui::vec2(10.0, 5.0);
        let size = galley.size() + pad * 2.0;
        let min = egui::pos2(area.max.x - size.x - 12.0, area.max.y - size.y - 12.0);
        let r = egui::Rect::from_min_size(min, size);
        p.rect_filled(r, egui::CornerRadius::same(6), BORDER_FOCUS);
        p.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            text,
            font,
            egui::Color32::from_rgb(25, 23, 19),
        );
    }

    /// Read-only bindings cheat sheet. Mirrors the dirpicker modal pattern: dim
    /// the desktop, draw a centered panel. Dismissed by any key (handled in
    /// `pump_leader`). Rows are built from the **live** keymap so hand-edits and
    /// in-app rebinds are reflected here, not a stale hardcoded list.
    fn paint_help(&self, ui: &mut egui::Ui, area: egui::Rect) {
        use crate::keymap::{Command, Group};
        ui.painter_at(area)
            .rect_filled(area, 0.0, egui::Color32::from_black_alpha(170));

        // (key, value). Empty value = section header; empty both = spacer.
        let mut rows: Vec<(String, String)> = Vec::new();
        rows.push((
            "Leader".into(),
            format!("{}  (then a command)", self.keymap.leader.pretty()),
        ));
        for &g in Group::ALL {
            rows.push((String::new(), String::new()));
            rows.push((format!("{} (after leader)", g.title()), String::new()));
            for &cmd in Command::ALL {
                if cmd.group() != g {
                    continue;
                }
                let chord = self
                    .keymap
                    .chord_for(cmd)
                    .map(|c| c.pretty())
                    .unwrap_or_else(|| "—".into());
                rows.push((format!("  {chord}"), cmd.label().to_string()));
            }
        }
        rows.push((String::new(), String::new()));
        rows.push((
            "  Drag".into(),
            "drag a header \u{2014} leaf edges split, centers stack as tabs, screen edges make a column".into(),
        ));
        rows.push((String::new(), String::new()));
        rows.push((
            "  Edit".into(),
            format!(
                "{} opens the editor  ·  any key closes",
                self.keymap
                    .chord_for(Command::OpenSettings)
                    .map(|c| c.pretty())
                    .unwrap_or_else(|| "—".into())
            ),
        ));

        let title_font = egui::FontId::proportional(15.0);
        let key_font = egui::FontId::monospace(12.5);
        let val_font = egui::FontId::proportional(12.5);
        let line_h = 19.0;
        let pad = 22.0;
        let key_col_w = 190.0;
        let panel_w = 470.0_f32;
        let panel_h = pad * 2.0 + 30.0 + rows.len() as f32 * line_h;
        let center = area.center();
        let panel = egui::Rect::from_center_size(center, egui::vec2(panel_w, panel_h));

        let p = ui.painter_at(area);
        p.rect_filled(panel, egui::CornerRadius::same(8), WIN_BG);
        p.rect_stroke(
            panel,
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.0, BORDER_FOCUS),
            egui::StrokeKind::Inside,
        );

        let mut y = panel.min.y + pad;
        p.text(
            egui::pos2(panel.min.x + pad, y),
            egui::Align2::LEFT_TOP,
            "Keyboard bindings",
            title_font,
            BORDER_FOCUS,
        );
        y += 30.0;
        for (k, v) in &rows {
            // Section headers (non-empty key, empty value) render emphasized.
            if v.is_empty() {
                if !k.is_empty() {
                    p.text(
                        egui::pos2(panel.min.x + pad, y),
                        egui::Align2::LEFT_TOP,
                        k,
                        val_font.clone(),
                        TEXT,
                    );
                }
            } else {
                p.text(
                    egui::pos2(panel.min.x + pad, y),
                    egui::Align2::LEFT_TOP,
                    k,
                    key_font.clone(),
                    BORDER_FOCUS,
                );
                p.text(
                    egui::pos2(panel.min.x + pad + key_col_w, y),
                    egui::Align2::LEFT_TOP,
                    v,
                    val_font.clone(),
                    DIM,
                );
            }
            y += line_h;
        }
    }
}

// Apply a resize drag to the affected edges of a floating window's rect, holding
// a minimum size and keeping every edge inside the area.
fn resize_floating(
    rect: &mut egui::Rect,
    d: egui::Vec2,
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
    area: egui::Vec2,
) {
    let mut nr = *rect;
    if left {
        nr.min.x = (nr.min.x + d.x).max(0.0).min(nr.max.x - MIN_W);
    }
    if right {
        nr.max.x = (nr.max.x + d.x).min(area.x).max(nr.min.x + MIN_W);
    }
    if top {
        nr.min.y = (nr.min.y + d.y).max(0.0).min(nr.max.y - MIN_H);
    }
    if bottom {
        nr.max.y = (nr.max.y + d.y).min(area.y).max(nr.min.y + MIN_H);
    }
    *rect = nr;
}

fn clamp(rect: &mut egui::Rect, area: egui::Vec2) {
    let w = rect.width().min(area.x);
    let h = rect.height().min(area.y);
    let x = rect.min.x.clamp(0.0, (area.x - w).max(0.0));
    let y = rect.min.y.clamp(0.0, (area.y - h).max(0.0));
    *rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h));
}

/// A tab's display name for chat purposes: the title minus the one-shot
/// exit marker `refresh_exit_titles` appends.
fn display_name(title: &str) -> &str {
    title.split("  ·  exited").next().unwrap_or(title).trim()
}

/// Parse a "t4"-style terminal id.
fn term_id(spec: &str) -> Result<WinId, String> {
    spec.strip_prefix('t')
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("bad terminal id: {spec}"))
}

/// Inverse of `term_id`: render a WinId as the chat identity string.
fn term_tag(id: WinId) -> String {
    format!("t{id}")
}

/// One dim line injected into a dispatched terminal at spawn, so the pane is
/// never blank while a silent worker (`claude -p`) runs. Truncated so a long
/// task prompt can't flood the pane.
fn dispatch_banner(argv: &[String]) -> String {
    let full = argv.join(" ");
    // 15-char prefix + 60 + "… ──" = 79 chars: flood control only. The final
    // fit to the pane's real width happens in Session::resize, which defers
    // the note past the spawn-time 80-col placeholder grid.
    if full.chars().count() > 60 {
        let head: String = full.chars().take(60).collect();
        format!("── dispatched: {head}… ──")
    } else {
        format!("── dispatched: {full} ──")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A cheap stub window content: an empty nested manager. Avoids spawning a PTY
    // (which would need an egui context), while exercising every tab op — they are
    // agnostic to whether content is a terminal or a project.
    fn stub_content() -> Content {
        Content::Project(Box::new(WindowManager::new()))
    }

    // Push a single-tab window with the given title; returns its id.
    fn push(wm: &mut WindowManager, title: &str) -> WinId {
        let id = wm.next;
        wm.next += 1;
        wm.z += 1;
        wm.windows.push(Win {
            id,
            tabs: vec![Tab {
                title: title.to_string(),
                content: stub_content(),
                chat_member: false,
            }],
            active: 0,
            rect: egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(400.0, 300.0)),
            z: wm.z,
            minimized: false,
            prev: None,
        });
        wm.focused = Some(id);
        id
    }

    // --- agent-dispatch drain semantics (handle_ctrl) ---

    // A dispatch message targeting the focused project, plus the receiver the
    // pipe server would be holding. `sent` backdates the server-side timestamp.
    fn dispatch_msg(
        sent: std::time::Instant,
    ) -> (
        crate::control::CtrlMsg,
        std::sync::mpsc::Receiver<crate::control::OpenReply>,
    ) {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let req = crate::control::OpenRequest {
            cmd: "open".into(),
            project: None,
            cwd: None,
            title: Some("agent · test".into()),
            command: vec!["cmd.exe".into(), "/c".into(), "exit 0".into()],
        };
        (crate::control::CtrlMsg::Open(req, rtx, sent), rrx)
    }

    fn project_terminal_count(wm: &WindowManager) -> usize {
        let pid = wm.focused.expect("a focused project");
        let win = wm.windows.iter().find(|w| w.id == pid).unwrap();
        let Content::Project(child) = &win.tabs[win.active].content else {
            panic!("focused window is not a project")
        };
        child.windows.len()
    }

    #[test]
    fn dispatch_banner_shows_command_and_truncates_long_prompts() {
        let argv: Vec<String> = ["claude", "-p", "task"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(dispatch_banner(&argv), "── dispatched: claude -p task ──");
        // A 500-char prompt must not flood the pane — and truncation must not
        // split a multi-byte char (the title convention uses "·" freely).
        let long: Vec<String> = vec!["claude".into(), "-p".into(), "é".repeat(500)];
        let b = dispatch_banner(&long);
        assert!(
            // The grid is 80 cols until the first render; the banner must not wrap.
            b.chars().count() <= 80,
            "banner too long: {} chars",
            b.chars().count()
        );
        assert!(
            b.ends_with("… ──"),
            "truncated banner ends with ellipsis: {b:?}"
        );
    }

    #[test]
    fn fresh_dispatch_spawns_and_replies() {
        let mut wm = WindowManager::new();
        push(&mut wm, "proj");
        let ctx = egui::Context::default();
        let (msg, rrx) = dispatch_msg(std::time::Instant::now());
        wm.handle_ctrl(msg, &ctx);
        let reply = rrx.try_recv().expect("reply must be sent");
        assert!(reply.ok);
        // The protocol promises "tN"/"pN"-formatted ids (epic § protocol).
        assert!(reply.terminal.is_some_and(|t| t.starts_with('t')));
        assert!(reply.project.is_some_and(|p| p.starts_with('p')));
        assert_eq!(project_terminal_count(&wm), 1);
    }

    #[test]
    fn stale_dispatch_is_dropped_without_spawning() {
        let mut wm = WindowManager::new();
        push(&mut wm, "proj");
        let ctx = egui::Context::default();
        // The pipe server gave up on this request REPLY_TIMEOUT ago and told the
        // client "foreman did not respond"; executing it now would open a
        // terminal the dispatcher believes failed (a retry then duplicates it).
        let sent = std::time::Instant::now()
            - (crate::control::REPLY_TIMEOUT + std::time::Duration::from_secs(1));
        let (msg, rrx) = dispatch_msg(sent);
        wm.handle_ctrl(msg, &ctx);
        assert!(rrx.try_recv().is_err(), "no reply for an abandoned request");
        assert_eq!(
            project_terminal_count(&wm),
            0,
            "stale request must not spawn"
        );
    }

    #[test]
    fn orphaned_reply_undoes_the_spawn() {
        let mut wm = WindowManager::new();
        push(&mut wm, "proj");
        let ctx = egui::Context::default();
        let (msg, rrx) = dispatch_msg(std::time::Instant::now());
        // Server timed out between our age check and the reply: receiver gone.
        drop(rrx);
        wm.handle_ctrl(msg, &ctx);
        assert_eq!(
            project_terminal_count(&wm),
            0,
            "client was told the dispatch failed; the terminal must not survive"
        );
    }

    #[test]
    fn len1_window_has_no_tab_bar_and_title_is_the_tab_title() {
        let mut wm = WindowManager::new();
        let id = push(&mut wm, "alpha");
        let w = wm.windows.iter().find(|w| w.id == id).unwrap();
        assert_eq!(w.tabs.len(), 1);
        assert_eq!(w.title(), "alpha");
    }

    #[test]
    fn merge_appends_source_tab_and_removes_source() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        assert_eq!(wm.windows.len(), 2);

        wm.merge_windows(a, b); // drop A onto B
        assert_eq!(wm.windows.len(), 1, "source window removed");
        let merged = &wm.windows[0];
        assert_eq!(merged.id, b, "destination survives");
        assert_eq!(merged.tabs.len(), 2, "source tab appended");
        // The just-dropped tab becomes active and is the merge target's focus.
        assert_eq!(merged.tabs[merged.active].title, "A");
        assert_eq!(wm.focused, Some(b));
    }

    #[test]
    fn merge_onto_self_is_noop() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        wm.merge_windows(a, a);
        assert_eq!(wm.windows.len(), 1);
        assert_eq!(wm.windows[0].tabs.len(), 1);
    }

    #[test]
    fn closing_last_tab_removes_the_window() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        wm.close_tab(a, 0);
        assert!(wm.windows.is_empty(), "single-tab close removes the window");
    }

    #[test]
    fn closing_one_of_many_tabs_keeps_window_and_clamps_active() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        wm.merge_windows(b, a); // A now has tabs [A, B], active = 1 (B)
        assert_eq!(wm.windows[0].tabs.len(), 2);
        assert_eq!(wm.windows[0].active, 1);

        wm.close_tab(a, 1); // close the active (last) tab
        let w = &wm.windows[0];
        assert_eq!(w.tabs.len(), 1);
        assert_eq!(w.active, 0, "active clamps to a live tab");
        assert_eq!(w.tabs[0].title, "A");
    }

    #[test]
    fn cycle_tab_wraps_forward_and_back() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        let c = push(&mut wm, "C");
        wm.merge_windows(b, a);
        wm.merge_windows(c, a); // A: [A, B, C]
        let id = a;
        wm.focus(id);
        // set active to 0 deterministically
        wm.windows.iter_mut().find(|w| w.id == id).unwrap().active = 0;

        wm.cycle_tab(true);
        assert_eq!(wm.windows.iter().find(|w| w.id == id).unwrap().active, 1);
        wm.cycle_tab(true);
        assert_eq!(wm.windows.iter().find(|w| w.id == id).unwrap().active, 2);
        wm.cycle_tab(true);
        assert_eq!(
            wm.windows.iter().find(|w| w.id == id).unwrap().active,
            0,
            "wraps"
        );
        wm.cycle_tab(false);
        assert_eq!(
            wm.windows.iter().find(|w| w.id == id).unwrap().active,
            2,
            "back wraps"
        );
    }

    #[test]
    fn cycle_tab_on_len1_falls_back_to_last_focused() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        // Focus A then B so last_focused = A while focused = B (both len-1).
        wm.focus(a);
        wm.focus(b);
        assert_eq!(wm.focused, Some(b));
        assert_eq!(wm.last_focused, Some(a));
        wm.cycle_tab(true); // not a stack → toggle to last focused
        assert_eq!(wm.focused, Some(a));
    }

    #[test]
    fn untab_detaches_into_new_floating_window() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        wm.merge_windows(b, a); // A: [A, B]
        assert_eq!(wm.windows.len(), 1);

        wm.untab(a, 1, egui::pos2(500.0, 400.0)); // pull B out
        assert_eq!(wm.windows.len(), 2, "a new window appeared");
        let src = wm.windows.iter().find(|w| w.id == a).unwrap();
        assert_eq!(src.tabs.len(), 1, "source lost the detached tab");
        assert_eq!(src.tabs[0].title, "A");
        // The new window holds exactly the detached tab and is focused.
        let new = wm.windows.iter().max_by_key(|w| w.id).unwrap();
        assert_eq!(new.tabs.len(), 1);
        assert_eq!(new.tabs[0].title, "B");
        assert_eq!(wm.focused, Some(new.id));
    }

    #[test]
    fn untab_on_len1_is_noop() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        wm.untab(a, 0, egui::pos2(500.0, 400.0));
        assert_eq!(wm.windows.len(), 1, "single-tab window is not detachable");
    }

    // --- Tree-based split/move/float (drives place_split with stub windows so no
    // real PTY/Session is spawned; split_dir only adds the spawn + id capture). ---

    #[test]
    fn split_from_floating_source_tiles_both_panes() {
        // floating focused src + place_split(Some(src), new, Right) → both in tree,
        // leaves == [src, new].
        let mut wm = WindowManager::new();
        let src = push(&mut wm, "src");
        let new = push(&mut wm, "new");
        // src is floating (not in tree); new is also floating.
        assert!(!wm.tree.contains(src));
        wm.focus(src);

        wm.place_split(Some(src), new, Dir::Right);

        assert!(wm.tree.contains(src), "src entered the tree");
        assert!(wm.tree.contains(new), "new entered the tree");
        assert_eq!(wm.tree.leaves(), vec![src, new]);
        assert_eq!(wm.focused, Some(new), "new is focused");
    }

    #[test]
    fn split_from_tiled_source_splits_that_leaf() {
        // src already in tree; place_split(Some(src), new, Down) → leaves [src, new],
        // root is a vertical split.
        let mut wm = WindowManager::new();
        let src = push(&mut wm, "src");
        let new = push(&mut wm, "new");
        wm.tree.insert_root(src, Dir::Right); // src is tiled

        wm.place_split(Some(src), new, Dir::Down);

        assert_eq!(wm.tree.leaves(), vec![src, new]);
        assert!(
            matches!(
                wm.tree.root,
                Some(crate::layout::Node::Split {
                    dir: crate::layout::SplitDir::V,
                    ..
                })
            ),
            "root should be a vertical split"
        );
        assert_eq!(wm.focused, Some(new), "new is focused");
    }

    #[test]
    fn move_dir_swaps_with_the_neighbor_and_edges_out() {
        // two tiles [a, b] side by side; focus a; move_dir(Right) → leaves [b, a];
        // move_dir(Right) again (no neighbor to the right) → still 2 leaves, a is rightmost.
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        wm.last_area = egui::vec2(1000.0, 800.0);
        // Build [a | b] layout: a on the left, b on the right.
        wm.tree.insert_root(a, Dir::Right);
        wm.tree.insert_root(b, Dir::Right);
        wm.focus(a);

        wm.move_dir(Dir::Right); // a swaps with b → [b, a]
        assert_eq!(wm.tree.leaves(), vec![b, a], "a moved right past b");
        assert_eq!(wm.focused, Some(a));

        wm.move_dir(Dir::Right); // a is already rightmost; re-inserts at right edge
        let leaves = wm.tree.leaves();
        assert_eq!(leaves.len(), 2, "still 2 leaves");
        assert_eq!(*leaves.last().unwrap(), a, "a remains rightmost");
    }

    #[test]
    fn move_dir_on_a_floating_window_enters_the_tree_at_that_edge() {
        // tiled a; floating b focused; move_dir(Left) → tree.contains(b), leaves == [b, a]
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        wm.last_area = egui::vec2(1000.0, 800.0);
        wm.tree.insert_root(a, Dir::Right); // a is tiled
        // b is floating (not in tree)
        assert!(!wm.tree.contains(b));
        wm.focus(b);

        wm.move_dir(Dir::Left);

        assert!(wm.tree.contains(b), "floating b entered the tree");
        let leaves = wm.tree.leaves();
        assert_eq!(leaves, vec![b, a], "b is at the left edge, a to the right");
    }

    #[test]
    fn toggle_float_roundtrips_tree_membership_and_rect() {
        // tiled a focused: toggle_float → !tree.contains(a), rect restored from prev;
        // toggle_float again → tree.contains(a).
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        wm.last_area = egui::vec2(1000.0, 800.0);
        wm.tree.insert_root(a, Dir::Right); // a is tiled
        wm.focus(a);

        // First toggle: tiled → floating, rect restored.
        wm.toggle_float();
        assert!(!wm.tree.contains(a), "a detached from tree");
        let rect_after = wm.windows.iter().find(|w| w.id == a).unwrap().rect;
        // prev was None before (tree-managed windows don't set prev), so falls back
        // to the default floating rect — just assert we got something reasonable.
        assert!(rect_after.width() > 0.0 && rect_after.height() > 0.0);

        // Second toggle: floating → tiled again.
        wm.toggle_float();
        assert!(wm.tree.contains(a), "a re-entered the tree");
    }

    #[test]
    fn place_split_with_no_source_becomes_the_root_tile() {
        // empty tree, place_split(None, n, Down) → leaves == [n]
        let mut wm = WindowManager::new();
        let n = push(&mut wm, "N");

        wm.place_split(None, n, Dir::Down);

        assert_eq!(wm.tree.leaves(), vec![n]);
        assert_eq!(wm.focused, Some(n));
    }

    fn mgr_with_project(id_focused: bool) -> WindowManager {
        let mut m = WindowManager::new();
        let (id, rect) = m.next_slot(egui::vec2(100.0, 100.0));
        let mut child = WindowManager::new();
        child.tag = Some(format!("p{id}"));
        m.push_win(id, "proj".into(), rect, Content::Project(Box::new(child)));
        if !id_focused {
            m.focused = None;
        }
        m
    }

    #[test]
    fn resolve_project_by_id_and_focus() {
        let m = mgr_with_project(true);
        assert_eq!(m.resolve_project(Some("p1")), Ok(1));
        assert_eq!(m.resolve_project(None), Ok(1)); // focused project
        assert!(m.resolve_project(Some("p9")).is_err());
        assert!(m.resolve_project(Some("zzz")).is_err());
        let unfocused = mgr_with_project(false);
        assert!(unfocused.resolve_project(None).is_err());
    }

    #[test]
    fn term_env_carries_ids() {
        let mut child = WindowManager::new();
        child.tag = Some("p3".into());
        let env = child.term_env(7);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("FOREMAN").as_deref(), Some("1"));
        assert_eq!(get("FOREMAN_PROJECT_ID").as_deref(), Some("p3"));
        assert_eq!(get("FOREMAN_TERMINAL_ID").as_deref(), Some("t7"));
        assert!(get("FOREMAN_EXE").is_some());

        // Desktop (untagged) managers must not claim a project id.
        let desktop = WindowManager::new();
        let env = desktop.term_env(1);
        assert!(env.iter().all(|(n, _)| n != "FOREMAN_PROJECT_ID"));
    }

    // --- group chat: membership, post, broadcast, history ---

    fn pause_argv() -> Vec<String> {
        // stays alive until stdin sees a key; exits cleanly when the PTY drops
        vec!["cmd.exe".into(), "/c".into(), "pause".into()]
    }

    #[test]
    fn dispatched_terminals_auto_join_chat() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let t = wm.add_terminal_cmd(&argv, None, None, &ctx).unwrap();
        let w = wm.windows.iter().find(|w| w.id == t).unwrap();
        assert!(w.tabs[w.active].chat_member);
    }

    #[test]
    fn dispatch_emits_a_joined_entry() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let t = wm
            .add_terminal_cmd(&argv, None, Some("worker A"), &ctx)
            .unwrap();
        let log = wm.chat.borrow();
        let m = log.msgs().last().expect("no joined entry");
        assert_eq!(m.kind, crate::chat::ChatKind::Joined);
        assert_eq!(m.from, format!("t{t}"));
        assert_eq!(m.name, "worker A");
    }

    #[test]
    fn first_post_emits_joined_before_the_post() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let t = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        // simulate a hand-opened terminal: not yet a member
        let w = wm.windows.iter_mut().find(|w| w.id == t).unwrap();
        w.tabs[w.active].chat_member = false;
        wm.chat_post(t, "hello", &[]).unwrap();
        let log = wm.chat.borrow();
        let kinds: Vec<_> = log.msgs().iter().map(|m| m.kind).collect();
        // dispatch auto-join from add_terminal_cmd, then the simulated
        // un-join means: Joined (dispatch), Joined (first post), Post
        assert_eq!(
            &kinds[kinds.len() - 3..],
            &[
                crate::chat::ChatKind::Joined, // dispatch auto-join
                crate::chat::ChatKind::Joined, // first post joins
                crate::chat::ChatKind::Post
            ]
        );
        drop(log);
        // second post: member already — no second Joined
        wm.chat_post(t, "again", &[]).unwrap();
        let log = wm.chat.borrow();
        assert_eq!(log.msgs().last().unwrap().kind, crate::chat::ChatKind::Post);
        let joins = log
            .msgs()
            .iter()
            .filter(|m| m.kind == crate::chat::ChatKind::Joined && m.from == format!("t{t}"))
            .count();
        assert_eq!(
            joins, 2,
            "one from dispatch, one from first post — not three"
        );
    }

    #[test]
    fn member_exit_emits_an_exited_entry_nonmember_does_not() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let member = wm
            .add_terminal_cmd(&argv, None, Some("worker A"), &ctx)
            .unwrap();
        let outsider = wm
            .add_terminal_cmd(&argv, None, Some("plain"), &ctx)
            .unwrap();
        let w = wm.windows.iter_mut().find(|w| w.id == outsider).unwrap();
        w.tabs[w.active].chat_member = false;
        // wait for both `cmd /c exit 0` children to end — pumping keepalive()
        // each pass, or the startup DSR query leaves cmd.exe hung forever
        // (the documented trap; same pattern as the broadcast tests above)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut done = 0;
            for id in [member, outsider] {
                let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
                let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                    panic!()
                };
                s.keepalive();
                if s.exited().is_some() {
                    done += 1;
                }
            }
            if done == 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "children never exited"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        wm.refresh_exit_titles();
        let log = wm.chat.borrow();
        let exits: Vec<_> = log
            .msgs()
            .iter()
            .filter(|m| m.kind == crate::chat::ChatKind::Exited)
            .collect();
        assert_eq!(exits.len(), 1, "only the member's exit is recorded");
        assert_eq!(exits[0].from, format!("t{member}"));
        assert_eq!(
            exits[0].name, "worker A",
            "name captured before the exit marker lands"
        );
    }

    #[test]
    fn chat_post_validates_joins_and_frames() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let t = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        // simulate a hand-opened (non-dispatched) terminal
        {
            let w = wm.windows.iter_mut().find(|w| w.id == t).unwrap();
            w.tabs[w.active].chat_member = false;
        }

        assert!(wm.chat_post(t, "", &[]).is_err(), "empty message rejected");
        assert!(
            wm.chat_post(999, "hi", &[]).is_err(),
            "unknown sender rejected"
        );
        let framed = wm.chat_post(t, "hello room", &[]).unwrap().0;
        // seq 3: dispatch Joined (1), first-post Joined (2), then the post —
        // system entries share the seq space but stay out of --history
        assert_eq!(framed, format!("[chat p1 #3] t{t}: hello room"));
        let w = wm.windows.iter().find(|w| w.id == t).unwrap();
        assert!(
            w.tabs[w.active].chat_member,
            "posting joins the sender's active tab"
        );
        assert_eq!(wm.chat_history(10), vec![format!("#3 t{t}: hello room")]);
    }

    #[test]
    fn open_chat_window_is_a_singleton() {
        let mut wm = WindowManager::new();
        wm.open_chat_window();
        let chat_wins = |wm: &WindowManager| {
            wm.windows
                .iter()
                .filter(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Chat(_))))
                .count()
        };
        assert_eq!(chat_wins(&wm), 1);
        let first = wm.windows.last().unwrap().id;
        // focus something else, then reopen: focuses, does not duplicate
        wm.focused = None;
        wm.open_chat_window();
        assert_eq!(chat_wins(&wm), 1);
        assert_eq!(wm.focused, Some(first));
    }

    #[test]
    fn open_chat_window_resurfaces_minimized_or_buried_viewer() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.open_chat_window();
        let id = wm.windows.last().unwrap().id;

        // (a) minimized viewer: reopening must unminimize, not just focus.
        {
            let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            w.minimized = true;
        }
        wm.focused = None;
        wm.open_chat_window();
        let w = wm.windows.iter().find(|w| w.id == id).unwrap();
        assert!(!w.minimized, "reopen must unminimize the viewer");
        assert_eq!(wm.focused, Some(id));

        // (b) chat tab buried behind a merged terminal tab: reopening must
        // re-activate the chat tab, not raise the window showing the terminal.
        {
            let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            let shell = Session::spawn_argv(&pause_argv(), None, &[], ctx.clone()).unwrap();
            w.tabs.push(Tab {
                title: "shell".into(),
                content: Content::Terminal(shell),
                chat_member: false,
            });
            w.active = 1; // terminal in front, chat behind
        }
        wm.focused = None;
        wm.open_chat_window();
        let w = wm.windows.iter().find(|w| w.id == id).unwrap();
        assert!(
            matches!(w.tabs[w.active].content, Content::Chat(_)),
            "reopen must re-activate the chat tab"
        );
        assert_eq!(wm.focused, Some(id));
    }

    #[test]
    fn refresh_chat_view_builds_rows_and_title_chip() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::vec2(800.0, 600.0);
        let a = wm
            .add_terminal_cmd(&pause_argv(), None, Some("worker A"), &ctx)
            .unwrap();
        let b = wm
            .add_terminal_cmd(&pause_argv(), None, Some("plain"), &ctx)
            .unwrap();
        let w = wm.windows.iter_mut().find(|w| w.id == b).unwrap();
        w.tabs[w.active].chat_member = false;
        wm.open_chat_window();
        wm.refresh_chat_view();
        let view_win = wm
            .windows
            .iter()
            .find(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Chat(_))))
            .unwrap();
        let tab = view_win
            .tabs
            .iter()
            .find(|t| matches!(t.content, Content::Chat(_)))
            .unwrap();
        assert_eq!(
            tab.title, "chat · 1 live",
            "the you-row must not inflate the live count"
        );
        let Content::Chat(v) = &tab.content else {
            panic!()
        };
        assert_eq!(v.crew.len(), 2, "the member + the human pane identity");
        // index by id, not position, for the member assertions
        let m = v
            .crew
            .iter()
            .find(|r| r.id == format!("t{a}"))
            .expect("member row missing");
        assert_eq!(m.name, "worker A");
        assert!(!m.exited);
        assert!(m.last.is_some(), "joined entry counts as heard");
        // the you-row sits AFTER the live members (here: index 1, one live member)
        assert_eq!(v.crew[1].id, "you");
        assert_eq!(v.crew[1].name, "you");
        assert!(!v.crew[1].exited);
        assert_eq!(
            v.crew[1].win, 0,
            "human row has no window — click must be a no-op"
        );
    }

    #[test]
    fn chat_click_focuses_the_member_window_and_tab() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::vec2(800.0, 600.0);
        let t = wm
            .add_terminal_cmd(&pause_argv(), None, Some("worker A"), &ctx)
            .unwrap();
        wm.open_chat_window();
        let chat_id = wm.focused.expect("open focuses the viewer");
        // simulate the render arm recording a click on worker A's row
        for w in &mut wm.windows {
            for tab in &mut w.tabs {
                if let Content::Chat(v) = &mut tab.content {
                    v.click = Some((t, 0));
                }
            }
        }
        wm.drain_chat_clicks();
        assert_eq!(wm.focused, Some(t), "click focused the member");
        assert_ne!(wm.focused, Some(chat_id));
        // stale target: must not panic or change focus
        for w in &mut wm.windows {
            for tab in &mut w.tabs {
                if let Content::Chat(v) = &mut tab.content {
                    v.click = Some((9999, 0));
                }
            }
        }
        wm.drain_chat_clicks();
        assert_eq!(wm.focused, Some(t), "stale click is a no-op");
        // stale tab in a live window: also a silent no-op (focus must stay put)
        wm.open_chat_window(); // refocuses the singleton viewer
        let chat_id = wm.focused.expect("viewer focused");
        for w in &mut wm.windows {
            for tab in &mut w.tabs {
                if let Content::Chat(v) = &mut tab.content {
                    v.click = Some((t, 5));
                }
            }
        }
        wm.drain_chat_clicks();
        assert_eq!(
            wm.focused,
            Some(chat_id),
            "stale tab index is a silent no-op"
        );
    }

    #[test]
    fn human_post_appends_with_reserved_id_and_broadcasts_to_all_members() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::vec2(800.0, 600.0);
        // both members run `cmd /c pause`: ANY stdin byte makes them exit
        let a = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let b = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        wm.open_chat_window();
        // simulate the input line submitting
        for w in &mut wm.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    v.pending_post = Some("go".to_string());
                }
            }
        }
        wm.drain_chat_posts();
        let framed = {
            let log = wm.chat.borrow();
            let m = log
                .msgs()
                .iter()
                .rfind(|m| m.kind == crate::chat::ChatKind::Post)
                .expect("post missing");
            assert_eq!(m.from, "you");
            assert_eq!(m.name, "you");
            let framed = m.frame("p1");
            assert!(framed.starts_with(&format!("[chat p1 #{}] you: go", m.seq)));
            framed
        };
        // BOTH members exit — the human excludes nobody. Bytes injected before a
        // child's startup DSR scan resolves get eaten (the documented trap; see
        // chat_broadcast_hits_members_only_excluding_sender), so pump every session
        // and RE-SEND the broadcast each iteration until both stdins have seen it —
        // deterministic instead of racing spawn latency.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            wm.chat_broadcast(None, &framed, None);
            let mut done = 0;
            for id in [a, b] {
                let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
                let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                    panic!()
                };
                if s.exited().is_some() {
                    done += 1;
                }
            }
            if done == 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a member never got the post"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn empty_or_blank_human_post_is_a_noop() {
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::vec2(800.0, 600.0);
        wm.open_chat_window();
        for w in &mut wm.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    v.pending_post = Some("   ".to_string());
                }
            }
        }
        wm.drain_chat_posts();
        assert_eq!(wm.chat.borrow().msgs().len(), 0);
    }

    #[test]
    fn leader_stays_dormant_while_a_widget_holds_focus() {
        let mut wm = WindowManager::new();
        // not as_desktop(): that loads the user's keybindings file from disk
        wm.desktop = true;
        let leader = wm.keymap.leader;
        let field = egui::Id::new("some-text-field");
        let ctx = egui::Context::default();
        let leader_event = || egui::Event::Key {
            key: leader.key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: leader.ctrl,
                shift: leader.shift,
                alt: leader.alt,
                ..Default::default()
            },
        };
        // frame 1: a widget holds keyboard focus — the leader chord must NOT arm
        let mut input = egui::RawInput::default();
        input.events.push(leader_event());
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ctx.memory_mut(|m| m.request_focus(field));
                wm.pump_commands(ui, true);
            });
        });
        assert!(
            !wm.armed,
            "leader must stay dormant while a field has focus"
        );
        // frame 2: focus released — the same chord arms (positive control)
        let mut input = egui::RawInput::default();
        input.events.push(leader_event());
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ctx.memory_mut(|m| m.surrender_focus(field));
                wm.pump_commands(ui, true);
            });
        });
        assert!(wm.armed, "without focus the leader must arm");
    }

    #[test]
    fn chat_view_watermark_moves_on_focus_loss_only() {
        let log = Rc::new(RefCell::new(crate::chat::ChatLog::new()));
        log.borrow_mut().post("t1", "a", "before-open");
        let mut v = crate::chat::ChatView::new(Rc::clone(&log));
        assert_eq!(
            v.last_seen, 1,
            "creation watermark = current tail (backlog pre-dates the window open)"
        );
        v.on_frame(true); // focused
        log.borrow_mut().post("t1", "a", "while-focused");
        v.on_frame(true);
        assert_eq!(v.last_seen, 1, "watermark holds while focused");
        v.on_frame(false); // focus left
        assert_eq!(
            v.last_seen, 2,
            "watermark catches up on the focus-loss edge"
        );
        log.borrow_mut().post("t1", "a", "while-unfocused");
        v.on_frame(false);
        assert_eq!(
            v.last_seen, 2,
            "unfocused arrivals stay above the watermark"
        );
    }

    #[test]
    fn chat_broadcast_hits_members_only_excluding_sender() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        // all three run `cmd /c pause`: receiving ANY stdin byte makes them exit
        let sender = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let member = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let outsider = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        {
            let w = wm.windows.iter_mut().find(|w| w.id == outsider).unwrap();
            w.tabs[w.active].chat_member = false;
        }

        let framed = wm.chat_post(sender, "go", &[]).unwrap().0;

        // Pump every session each iteration: keepalive() answers the startup
        // DSR (the documented trap — bytes injected before a child's DSR scan
        // resolves get eaten by the scan, see terminal.rs's
        // inject_input_reaches_child_stdin). wm.rs has no public grid access
        // to wait for "prompt rendered", so instead of a timing-based sleep
        // the broadcast is re-sent until the member's stdin sees it. That is
        // deterministic and still proves the membership filter: every send
        // skips the sender and the non-member, who are pumped too — so a
        // wrongful injection into them WOULD make them exit below.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            wm.chat_broadcast(Some(sender), &framed, None);
            // positive signal: the member exits because bytes hit its stdin
            let w = wm.windows.iter_mut().find(|w| w.id == member).unwrap();
            let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                panic!()
            };
            if s.exited().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "member never received the broadcast"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // sender and non-member saw nothing: still alive after the member
        // exited (kept pumped so an erroneous injection would surface).
        let grace = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < grace {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        for (id, who) in [(sender, "sender"), (outsider, "non-member")] {
            let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                panic!()
            };
            assert!(s.exited().is_none(), "{who} must not be injected");
        }
    }

    // --- Task 4: chat verb end-to-end (handle_ctrl + chat_dispatch) ---

    /// Desktop with one project (p1) containing two member terminals.
    fn chat_fixture(ctx: &egui::Context) -> (WindowManager, WinId, WinId) {
        let mut child = WindowManager::new();
        child.tag = Some("p1".to_string());
        let a = child
            .add_terminal_cmd(&pause_argv(), None, None, ctx)
            .unwrap();
        let b = child
            .add_terminal_cmd(&pause_argv(), None, None, ctx)
            .unwrap();
        let mut d = WindowManager::new().as_desktop();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        d.push_win(1, "proj".into(), rect, Content::Project(Box::new(child)));
        (d, a, b)
    }

    fn chat_req(
        from: WinId,
        text: Option<&str>,
        history: Option<usize>,
    ) -> crate::control::ChatRequest {
        crate::control::ChatRequest {
            cmd: "chat".into(),
            project: Some("p1".into()),
            from: Some(format!("t{from}")),
            to: Vec::new(),
            text: text.map(str::to_string),
            history,
            re: None,
        }
    }

    #[test]
    fn chat_post_replies_ok_then_broadcasts() {
        let ctx = egui::Context::default();
        let (mut d, a, b) = chat_fixture(&ctx);
        // Pre-pump all sessions so any startup DSR scans are resolved before
        // handle_ctrl fires its one-shot broadcast.
        let deadline_pump = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline_pump {
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            if let Content::Project(child) = &mut win.tabs[win.active].content {
                for w in child.windows.iter_mut() {
                    if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                        s.keepalive();
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let (rtx, rrx) = std::sync::mpsc::channel();
        d.handle_ctrl(
            crate::control::CtrlMsg::Chat(
                chat_req(a, Some("go"), None),
                rtx,
                std::time::Instant::now(),
            ),
            &ctx,
        );
        assert!(rrx.try_recv().expect("no reply").ok);
        // end-to-end: member b runs `cmd /c pause` and exits when bytes arrive
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &mut win.tabs[win.active].content else {
                panic!()
            };
            // Keep pumping so the broadcast bytes actually flush through.
            for w in child.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            let w = child.windows.iter_mut().find(|w| w.id == b).unwrap();
            let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                panic!()
            };
            if s.exited().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "member never received the post"
            );
            // If the one-shot broadcast may have been eaten by the DSR scan,
            // re-send via another handle_ctrl call (plan deviation: noted in report).
            let (rtx2, _rrx2) = std::sync::mpsc::channel();
            d.handle_ctrl(
                crate::control::CtrlMsg::Chat(
                    chat_req(a, Some("go"), None),
                    rtx2,
                    std::time::Instant::now(),
                ),
                &ctx,
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn chat_history_replies_lines_and_does_not_join() {
        let ctx = egui::Context::default();
        let (mut d, a, b) = chat_fixture(&ctx);
        // seed one message
        let (rtx, rrx) = std::sync::mpsc::channel();
        d.handle_ctrl(
            crate::control::CtrlMsg::Chat(
                chat_req(a, Some("hi"), None),
                rtx,
                std::time::Instant::now(),
            ),
            &ctx,
        );
        rrx.try_recv().expect("post reply");
        // make b a known terminal that is NOT a room member, so the
        // does-not-join assertion below actually has something to catch
        {
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &mut win.tabs[win.active].content else {
                panic!()
            };
            let w = child.windows.iter_mut().find(|w| w.id == b).unwrap();
            w.tabs[w.active].chat_member = false;
        }
        // history from a non-member: replies, does not error, does not join
        let (rtx, rrx) = std::sync::mpsc::channel();
        d.handle_ctrl(
            crate::control::CtrlMsg::Chat(
                chat_req(b, None, Some(10)),
                rtx,
                std::time::Instant::now(),
            ),
            &ctx,
        );
        let r = rrx.try_recv().expect("no history reply");
        assert!(r.ok);
        assert_eq!(r.history.as_deref().map(|h| h.len()), Some(1));
        let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
        let Content::Project(child) = &mut win.tabs[win.active].content else {
            panic!()
        };
        let w = child.windows.iter().find(|w| w.id == b).unwrap();
        assert!(
            !w.tabs[w.active].chat_member,
            "reading history must not join the room"
        );
    }

    #[test]
    fn chat_history_works_without_from_and_post_without_from_errors() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        // seed one post so history has a line
        let (rtx, rrx) = std::sync::mpsc::channel();
        d.handle_ctrl(
            crate::control::CtrlMsg::Chat(
                chat_req(a, Some("hi"), None),
                rtx,
                std::time::Instant::now(),
            ),
            &ctx,
        );
        assert!(rrx.try_recv().expect("post reply").ok);
        let snapshot = |d: &WindowManager| {
            let win = d.windows.iter().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &win.tabs[win.active].content else {
                panic!()
            };
            let members: Vec<bool> = child
                .windows
                .iter()
                .flat_map(|w| w.tabs.iter().map(|t| t.chat_member))
                .collect();
            (child.chat.borrow().msgs().len(), members)
        };
        let before = snapshot(&d);
        // history with from: None — must succeed (any caller may read)
        let mut req = chat_req(a, None, Some(5));
        req.from = None;
        let (rtx, rrx) = std::sync::mpsc::channel();
        d.handle_ctrl(
            crate::control::CtrlMsg::Chat(req, rtx, std::time::Instant::now()),
            &ctx,
        );
        let r = rrx.try_recv().expect("no history reply");
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.history.as_deref().map(|h| h.len()), Some(1));
        // post with from: None — refused loudly, nothing mutated
        let mut req = chat_req(a, Some("hi"), None);
        req.from = None;
        let (rtx, rrx) = std::sync::mpsc::channel();
        d.handle_ctrl(
            crate::control::CtrlMsg::Chat(req, rtx, std::time::Instant::now()),
            &ctx,
        );
        let r = rrx.try_recv().expect("no post reply");
        assert!(!r.ok);
        let e = r.error.unwrap();
        assert!(
            e.contains("sender") && e.contains("FOREMAN_TERMINAL_ID"),
            "{e}"
        );
        assert_eq!(
            snapshot(&d),
            before,
            "failed from-less post must not append or change membership"
        );
    }

    #[test]
    fn stale_chat_request_is_dropped_without_reply() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let (rtx, rrx) = std::sync::mpsc::channel();
        let stale = std::time::Instant::now() - crate::control::REPLY_TIMEOUT;
        d.handle_ctrl(
            crate::control::CtrlMsg::Chat(chat_req(a, Some("late"), None), rtx, stale),
            &ctx,
        );
        assert!(
            rrx.try_recv().is_err(),
            "stale request must be dropped unanswered (client already saw a timeout)"
        );
    }

    // --- status verb: project/terminal listing over the control pipe ---

    // A status message plus the receiver the pipe server would be holding.
    fn status_msg(
        project: Option<&str>,
        sent: std::time::Instant,
    ) -> (
        crate::control::CtrlMsg,
        std::sync::mpsc::Receiver<crate::control::OpenReply>,
    ) {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let req = crate::control::StatusRequest {
            cmd: "status".into(),
            project: project.map(str::to_string),
        };
        (crate::control::CtrlMsg::Status(req, rtx, sent), rrx)
    }

    #[test]
    fn status_lists_projects_terminals_and_membership() {
        let ctx = egui::Context::default();
        let (mut d, a, b) = chat_fixture(&ctx);
        let (msg, rrx) = status_msg(None, std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no status reply");
        assert!(r.ok, "{:?}", r.error);
        let lines = r.history.expect("status rides the history field");
        assert_eq!(lines.len(), 3, "project header + two terminals: {lines:?}");
        assert!(lines[0].starts_with("p1  proj"), "{}", lines[0]);
        for (line, id) in [(&lines[1], a), (&lines[2], b)] {
            assert!(
                line.starts_with(&format!("  t{id}  running  chat")),
                "{line}"
            );
        }
    }

    #[test]
    fn status_reports_instantly_exited_terminal_with_code() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 7".to_string(),
        ];
        let t = {
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &mut win.tabs[win.active].content else {
                panic!()
            };
            child.add_terminal_cmd(&argv, None, None, &ctx).unwrap()
        };
        // Poll with fresh Status requests until the child process has died
        // and status reports the code — pumping keepalive so the startup DSR
        // query can't park cmd.exe (the documented trap), and refreshing exit
        // titles so the title-stamp path is exercised (display_name must
        // strip it from the status line).
        let want = format!("  t{t}  exited(7)  chat  agent · cmd.exe");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            {
                let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
                let Content::Project(child) = &mut win.tabs[win.active].content else {
                    panic!()
                };
                for w in child.windows.iter_mut() {
                    for tab in w.tabs.iter_mut() {
                        if let Content::Terminal(s) = &mut tab.content {
                            s.keepalive();
                        }
                    }
                }
            }
            d.refresh_exit_titles();
            let (msg, rrx) = status_msg(None, std::time::Instant::now());
            d.handle_ctrl(msg, &ctx);
            let r = rrx.try_recv().expect("no status reply");
            assert!(r.ok, "{:?}", r.error);
            let lines = r.history.expect("lines");
            let line = lines
                .iter()
                .find(|l| l.starts_with(&format!("  t{t}  ")))
                .expect("the worker's line");
            if *line == want {
                // status asked the live process, and the title stamp
                // ("  ·  exited (7)") never leaks into the listing
                assert!(!line.contains("·  exited ("), "{line}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "status never reported exited(7); last line: {line}"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    #[test]
    fn status_filters_by_project_and_rejects_unknown() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        // second project (p2) with one terminal of its own
        let mut child2 = WindowManager::new();
        child2.tag = Some("p2".to_string());
        child2
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        d.push_win(2, "other".into(), rect, Content::Project(Box::new(child2)));

        // --project p1 lists only p1's header + its two terminals
        let (msg, rrx) = status_msg(Some("p1"), std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no status reply");
        assert!(r.ok, "{:?}", r.error);
        let lines = r.history.expect("lines");
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("p1  "), "{}", lines[0]);
        assert!(
            lines.iter().all(|l| !l.starts_with("p2")),
            "p2 must be filtered out: {lines:?}"
        );

        // unknown project is an error, not an empty list
        let (msg, rrx) = status_msg(Some("p99"), std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no error reply");
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("no such project: p99"));

        // bare status on an empty desktop says so (ok, not an error)
        let mut empty = WindowManager::new().as_desktop();
        let (msg, rrx) = status_msg(None, std::time::Instant::now());
        empty.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no reply");
        assert!(r.ok);
        assert_eq!(r.history.as_deref(), Some(&["no projects".to_string()][..]));
    }

    #[test]
    fn stale_status_is_dropped() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        let stale = std::time::Instant::now()
            - (crate::control::REPLY_TIMEOUT + std::time::Duration::from_secs(1));
        let (msg, rrx) = status_msg(None, stale);
        d.handle_ctrl(msg, &ctx);
        assert!(
            rrx.try_recv().is_err(),
            "stale request must be dropped unanswered (client already saw a timeout)"
        );
    }

    // --- close verb: validated, reply-before-close terminal teardown ---

    // A close message plus the receiver the pipe server would be holding.
    fn close_msg(
        project: Option<&str>,
        terminals: &[&str],
        sent: std::time::Instant,
    ) -> (
        crate::control::CtrlMsg,
        std::sync::mpsc::Receiver<crate::control::OpenReply>,
    ) {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let req = crate::control::CloseRequest {
            cmd: "close".into(),
            project: project.map(str::to_string),
            terminals: terminals.iter().map(|t| t.to_string()).collect(),
        };
        (crate::control::CtrlMsg::Close(req, rtx, sent), rrx)
    }

    // Does project p1's child manager still hold window `id`?
    fn child_has_win(d: &WindowManager, id: WinId) -> bool {
        let win = d.windows.iter().find(|w| w.id == 1).unwrap();
        let Content::Project(child) = &win.tabs[win.active].content else {
            panic!()
        };
        child.windows.iter().any(|w| w.id == id)
    }

    #[test]
    fn close_closes_listed_terminals_and_replies_project() {
        let ctx = egui::Context::default();
        let (mut d, a, b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let (msg, rrx) = close_msg(Some("p1"), &[&ta], std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no close reply");
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.project.as_deref(), Some("p1"));
        assert!(!child_has_win(&d, a), "closed terminal must be gone");
        assert!(child_has_win(&d, b), "unlisted terminal must survive");
    }

    #[test]
    fn close_unknown_terminal_fails_whole_request_and_closes_nothing() {
        let ctx = egui::Context::default();
        let (mut d, a, b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let (msg, rrx) = close_msg(Some("p1"), &[&ta, "t99"], std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no close reply");
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("no such terminal: t99"));
        // atomic: the valid id must NOT have been closed
        assert!(child_has_win(&d, a), "valid id must survive a failed batch");
        assert!(child_has_win(&d, b));
    }

    #[test]
    fn close_refuses_non_terminal_window() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        // open the chat viewer inside the project's child manager
        let viewer = {
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &mut win.tabs[win.active].content else {
                panic!()
            };
            child.open_chat_window();
            child.windows.last().unwrap().id
        };
        let tv = format!("t{viewer}");
        let (msg, rrx) = close_msg(Some("p1"), &[&tv], std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no close reply");
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("not a terminal"));
        assert!(child_has_win(&d, viewer), "the viewer must survive");
    }

    #[test]
    fn close_skips_execution_when_reply_orphaned() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let (msg, rrx) = close_msg(Some("p1"), &[&ta], std::time::Instant::now());
        // server timed out between the age check and the reply: receiver gone
        drop(rrx);
        d.handle_ctrl(msg, &ctx);
        assert!(
            child_has_win(&d, a),
            "client was told foreman didn't respond; the close must be skipped"
        );
    }

    #[test]
    fn stale_close_is_dropped() {
        let ctx = egui::Context::default();
        let (mut d, a, b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let stale = std::time::Instant::now()
            - (crate::control::REPLY_TIMEOUT + std::time::Duration::from_secs(1));
        let (msg, rrx) = close_msg(Some("p1"), &[&ta], stale);
        d.handle_ctrl(msg, &ctx);
        assert!(
            rrx.try_recv().is_err(),
            "stale request must be dropped unanswered"
        );
        assert!(child_has_win(&d, a), "stale request must not close");
        assert!(child_has_win(&d, b));
    }

    #[test]
    fn close_exited_terminal_succeeds() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let t = {
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &mut win.tabs[win.active].content else {
                panic!()
            };
            child.add_terminal_cmd(&argv, None, None, &ctx).unwrap()
        };
        // pump until the child process has actually exited (the DSR trap:
        // cmd.exe hangs on its startup query until keepalive answers it)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &mut win.tabs[win.active].content else {
                panic!()
            };
            let w = child.windows.iter_mut().find(|w| w.id == t).unwrap();
            let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                panic!()
            };
            s.keepalive();
            if s.exited().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "worker never exited");
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let tt = format!("t{t}");
        let (msg, rrx) = close_msg(Some("p1"), &[&tt], std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no close reply");
        assert!(r.ok, "{:?}", r.error);
        assert!(!child_has_win(&d, t), "exited terminal must close cleanly");
    }

    #[test]
    fn chat_request_with_both_or_neither_is_rejected() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        for req in [chat_req(a, Some("x"), Some(5)), chat_req(a, None, None)] {
            let (rtx, rrx) = std::sync::mpsc::channel();
            d.handle_ctrl(
                crate::control::CtrlMsg::Chat(req, rtx, std::time::Instant::now()),
                &ctx,
            );
            let r = rrx.try_recv().expect("shape errors must still reply");
            assert!(!r.ok);
            assert!(r.error.unwrap().contains("exactly one"), "wrong error");
        }
    }

    #[test]
    fn chat_broadcast_reaches_background_member_tab_not_foreground_shell() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let sender = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let host = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        // simulate a tab-merge: host window gains a foreground NON-member shell
        // tab; the dispatched member terminal stays behind it as a background tab
        {
            let w = wm.windows.iter_mut().find(|w| w.id == host).unwrap();
            let shell = Session::spawn_argv(&pause_argv(), None, &[], ctx.clone()).unwrap();
            w.tabs.push(Tab {
                title: "shell".into(),
                content: Content::Terminal(shell),
                chat_member: false,
            });
            w.active = 1; // shell in front, member behind
        }
        let framed = wm.chat_post(sender, "go", &[]).unwrap().0;

        // Same re-broadcast + pump-everything pattern as
        // chat_broadcast_hits_members_only_excluding_sender (the DSR trap):
        // every tab of every window is kept pumped, so a wrongful injection
        // into the foreground shell would make it exit below.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for w in wm.windows.iter_mut() {
                for t in w.tabs.iter_mut() {
                    if let Content::Terminal(s) = &mut t.content {
                        s.keepalive();
                    }
                }
            }
            wm.chat_broadcast(Some(sender), &framed, None);
            // positive signal: the background member tab exits
            let w = wm.windows.iter_mut().find(|w| w.id == host).unwrap();
            let Content::Terminal(s) = &mut w.tabs[0].content else {
                panic!()
            };
            if s.exited().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background member tab never received the broadcast"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // foreground non-member shell saw nothing: still alive after grace
        let grace = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < grace {
            for w in wm.windows.iter_mut() {
                for t in w.tabs.iter_mut() {
                    if let Content::Terminal(s) = &mut t.content {
                        s.keepalive();
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let w = wm.windows.iter_mut().find(|w| w.id == host).unwrap();
        let Content::Terminal(s) = &mut w.tabs[1].content else {
            panic!()
        };
        assert!(
            s.exited().is_none(),
            "foreground non-member shell must not be injected"
        );
    }

    // --- chat @-mentions v2: targeted delivery + validation ---

    #[test]
    fn chat_targeted_broadcast_hits_only_the_target() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        // all run `cmd /c pause`: any stdin byte makes them exit
        let sender = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let target = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let bystander = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        // bystander IS a member — only the target filter may exclude it
        let framed = wm.chat_post(sender, "go", &[]).unwrap().0;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            wm.chat_broadcast(Some(sender), &framed, Some(&[target]));
            let w = wm.windows.iter_mut().find(|w| w.id == target).unwrap();
            let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                panic!()
            };
            if s.exited().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "target never received the bytes"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // member bystander + sender saw nothing (kept pumped so a wrongful
        // injection would surface), and Some(&[]) injects nobody at all
        wm.chat_broadcast(Some(sender), &framed, Some(&[]));
        let grace = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < grace {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        for (id, who) in [(sender, "sender"), (bystander, "member bystander")] {
            let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                panic!()
            };
            assert!(s.exited().is_none(), "{who} must not be injected");
        }
    }

    #[test]
    fn targeted_post_validates_all_or_nothing_before_any_mutation() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let sender = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let member = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let outsider = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        {
            let w = wm.windows.iter_mut().find(|w| w.id == outsider).unwrap();
            w.tabs[w.active].chat_member = false;
        }
        let len_before = wm.chat.borrow().msgs().len();

        // unknown id — names it; one bad target fails a multi-target post entirely
        let e = wm
            .chat_post(sender, "go", &[term_tag(member), "t99".into()])
            .unwrap_err();
        assert!(e.contains("no such terminal: t99"), "{e}");
        // self-mention
        let e = wm.chat_post(sender, "go", &[term_tag(sender)]).unwrap_err();
        assert!(e.contains("cannot mention yourself"), "{e}");
        // non-member
        let e = wm
            .chat_post(sender, "go", &[term_tag(outsider)])
            .unwrap_err();
        assert!(e.contains("is not a chat member"), "{e}");
        // nothing appended by any failed post
        assert_eq!(wm.chat.borrow().msgs().len(), len_before);
        // inline mentions count too: a leading @ with a bad id fails the post
        let e = wm.chat_post(sender, "@t99 go", &[]).unwrap_err();
        assert!(e.contains("no such terminal: t99"), "{e}");
    }

    #[test]
    fn failed_targeted_post_does_not_join_the_sender() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let sender = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        {
            // make the sender a NON-member so a successful post would join it
            let w = wm.windows.iter_mut().find(|w| w.id == sender).unwrap();
            w.tabs[w.active].chat_member = false;
        }
        // dispatch already logged one Joined sysline; a failed post adds nothing
        let len_before = wm.chat.borrow().msgs().len();
        let _ = wm.chat_post(sender, "go", &["t99".into()]).unwrap_err();
        let w = wm.windows.iter().find(|w| w.id == sender).unwrap();
        assert!(!w.tabs[w.active].chat_member, "failed post must not join");
        assert_eq!(
            wm.chat.borrow().msgs().len(),
            len_before,
            "no Joined sysline either"
        );
    }

    #[test]
    fn targeted_post_resolves_targets_and_frames_the_arrow() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let sender = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let member = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();

        // flags first, then inline, deduped; `you` resolves to no terminal
        let (framed, targets) = wm
            .chat_post(sender, "@you go", &[term_tag(member)])
            .unwrap();
        let mtag = term_tag(member);
        let stag = term_tag(sender);
        assert!(
            framed.contains(&format!("{stag}→{mtag},you: @you go")),
            "{framed}"
        );
        assert_eq!(targets, Some(vec![member]));
        // pure-@you: Some(empty) — targeted, deliver to nobody
        let (framed, targets) = wm.chat_post(sender, "@you need eyes", &[]).unwrap();
        assert!(
            framed.contains(&format!("{stag}→you: @you need eyes")),
            "{framed}"
        );
        assert_eq!(targets, Some(vec![]));
        // untargeted: None — broadcast
        let (_, targets) = wm.chat_post(sender, "plain", &[]).unwrap();
        assert_eq!(targets, None);
    }

    #[test]
    fn targeting_an_exited_member_errors() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let sender = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let victim = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        // kill the victim by injecting a byte (pause exits on any stdin), pumping
        // through the DSR window like the broadcast tests
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            {
                let w = wm.windows.iter_mut().find(|w| w.id == victim).unwrap();
                let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                    panic!()
                };
                s.inject_input("x");
                if s.exited().is_some() {
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline, "victim never exited");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let e = wm.chat_post(sender, "go", &[term_tag(victim)]).unwrap_err();
        assert!(e.contains("has exited"), "{e}");
    }

    #[test]
    fn human_mention_narrows_delivery_or_falls_back_to_prose() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let member = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        let mtag = term_tag(member);

        // valid mention: targeted, arrow-framed under the reserved sender
        let (framed, targets) = wm
            .chat_post_human(&format!("@{mtag} check the diff"))
            .unwrap();
        assert!(
            framed.contains(&format!("you→{mtag}: @{mtag} check the diff")),
            "{framed}"
        );
        assert_eq!(targets, Some(vec![member]));
        assert_eq!(
            wm.chat.borrow().msgs().last().unwrap().to,
            vec![mtag.clone()]
        );

        // unknown id: prose fallback — broadcast, text intact, no error (spec §7)
        let (framed, targets) = wm.chat_post_human("@t99 anyone?").unwrap();
        assert!(framed.contains("you: @t99 anyone?"), "{framed}");
        assert_eq!(targets, None);
        assert!(wm.chat.borrow().msgs().last().unwrap().to.is_empty());

        // @you from the human is a self-mention: same fallback
        let (framed, targets) = wm.chat_post_human("@you hello").unwrap();
        assert!(framed.contains("you: @you hello"), "{framed}");
        assert_eq!(targets, None);
    }

    #[test]
    fn zoom_overlays_without_touching_the_tree_or_floating_rect() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "tiled");
        wm.tree.insert_root(a, Dir::Right);
        wm.toggle_zoom(a);
        assert_eq!(wm.zoomed, Some(a));
        assert!(wm.tree.contains(a)); // tree untouched
        wm.toggle_zoom(a);
        assert_eq!(wm.zoomed, None);
        // floating window: rect must survive a zoom round-trip
        let b = push(&mut wm, "float");
        let before = wm.windows.iter().find(|w| w.id == b).unwrap().rect;
        wm.toggle_zoom(b);
        wm.toggle_zoom(b);
        let after = wm.windows.iter().find(|w| w.id == b).unwrap().rect;
        assert_eq!(before, after);
    }

    #[test]
    fn tile_new_splits_the_focused_leaf_else_roots() {
        let mut wm = WindowManager::new();
        wm.last_area = egui::vec2(1000.0, 800.0);
        let a = push(&mut wm, "A");
        // no tiled anchor + empty tree → sole root leaf
        wm.tile_new(a, None);
        assert_eq!(wm.tree.leaves(), vec![a]);
        // tiled anchor → new window splits the anchor's slot
        let b = push(&mut wm, "B");
        wm.tile_new(b, Some(a));
        assert_eq!(wm.tree.leaves().len(), 2);
        assert!(wm.tree.contains(b));
        // anchor not tiled (floating) and tree non-empty → enters at root level
        let c = push(&mut wm, "C");
        let d = push(&mut wm, "D"); // d stays floating, used as a non-tiled anchor
        wm.tile_new(c, Some(d));
        assert!(wm.tree.contains(c));
        assert!(!wm.tree.contains(d));
    }

    #[test]
    fn closing_a_tiled_window_collapses_its_slot() {
        let mut wm = WindowManager::new();
        wm.last_area = egui::vec2(1000.0, 800.0);
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        wm.tree.insert_root(a, Dir::Right);
        wm.tree.insert_root(b, Dir::Right);
        wm.close(a);
        assert_eq!(wm.tree.leaves(), vec![b]);
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, wm.last_area);
        let p = wm.tree.layout(local, 8.0);
        assert!((p[0].1.width() - (1000.0 - 16.0)).abs() < 0.5, "b expanded to full inner width");
    }
}
