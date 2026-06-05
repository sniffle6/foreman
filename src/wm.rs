use crate::dirpicker::{DirPicker, Outcome};
use crate::keymap::{Chord, Command, Keymap};
use crate::settings::{Outcome as SettingsOutcome, SettingsView};
use crate::terminal::{Session, Shell};
use eframe::egui;
use std::path::PathBuf;

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

const TITLE_H: f32 = 26.0;

// Leader (prefix) key — tmux-style. After it is pressed the next chord is a
// *command* (consumed, never sent to the PTY). The leader is now data-driven:
// it lives in `Keymap::leader` (default `Ctrl+b`), loaded from
// `%APPDATA%\foreman\keybindings.json` and overridable per user.

const RESIZE_BAND: f32 = 6.0; // thickness of the invisible edge/corner resize hit-zones
const MIN_W: f32 = 240.0; // smallest a floating window may be dragged to
const MIN_H: f32 = 140.0;
const MIN_TILE: f32 = 120.0; // smallest a tiled pane may shrink to when dragging a split

// snap overlay (amber, matches BORDER_FOCUS / web mockup --needs #e7a93f)
const SNAP_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(231, 169, 63, 33); // ~13% alpha
const SNAP_STROKE: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);
const SNAP_GAP: f32 = 0.0; // inset of zones from the area edge; 0 = windows tile edge-to-edge
const TOP_HOLD: f64 = 0.4; // hold in the top zone this long (s) → escalate to maximize
const GROW_LEAD: f64 = 0.25; // overlay grows top-half → full over the last GROW_LEAD secs

// A cardinal direction for directional focus / snap commands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}
impl Dir {
    // The snap zone a directional snap maps to (half-screen tiling).
    fn zone(self) -> Zone {
        match self {
            Dir::Left => Zone::Left,
            Dir::Right => Zone::Right,
            Dir::Up => Zone::Top,
            Dir::Down => Zone::Bottom,
        }
    }

    // The opposite direction — used by split to place the source pane facing the
    // newcomer (Left↔Right, Up↔Down).
    fn opposite(self) -> Dir {
        match self {
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Zone {
    Max,
    Left,
    Right,
    Top,
    Bottom,
    Tl,
    Tr,
    Bl,
    Br,
}

// pointer position as a fraction of the area → target zone (ports `detectZone`).
fn detect_zone(fx: f32, fy: f32) -> Option<Zone> {
    const T: f32 = 0.085; // edge band
    const C: f32 = 0.22; // corner band on the cross axis
    if fy < T {
        if fx < C {
            return Some(Zone::Tl);
        }
        if fx > 1.0 - C {
            return Some(Zone::Tr);
        }
        return Some(Zone::Top); // holding here escalates to Max (see resolve_zone)
    }
    if fy > 1.0 - T {
        if fx < C {
            return Some(Zone::Bl);
        }
        if fx > 1.0 - C {
            return Some(Zone::Br);
        }
        return Some(Zone::Bottom);
    }
    if fx < T {
        return Some(Zone::Left);
    }
    if fx > 1.0 - T {
        return Some(Zone::Right);
    }
    None
}

// target rect for a zone in LOCAL coords, given the manager's split ratios.
// `split` is the fractional position (0..1) of the vertical (x) and horizontal
// (y) tiling dividers, so adjacent snapped windows share a movable edge. With
// split = (0.5, 0.5) this reduces to the old fixed half/quarter tiling.
fn zone_rect(zone: Zone, area: egui::Vec2, split: egui::Vec2) -> egui::Rect {
    let g = SNAP_GAP;
    let (w, h) = (area.x, area.y);
    let divx = w * split.x; // vertical divider x
    let divy = h * split.y; // horizontal divider y
    let lx = g; // left column x
    let lw = (divx - g * 1.5).max(1.0); // left column width
    let rx = divx + g * 0.5; // right column x
    let rw = (w - divx - g * 1.5).max(1.0);
    let ty = g; // top row y
    let th = (divy - g * 1.5).max(1.0);
    let by = divy + g * 0.5; // bottom row y
    let bh = (h - divy - g * 1.5).max(1.0);
    let fw = (w - g * 2.0).max(1.0);
    let fh = (h - g * 2.0).max(1.0);
    let (x, y, sw, sh) = match zone {
        Zone::Max => (g, g, fw, fh),
        Zone::Top => (g, ty, fw, th),
        Zone::Bottom => (g, by, fw, bh),
        Zone::Left => (lx, g, lw, fh),
        Zone::Right => (rx, g, rw, fh),
        Zone::Tl => (lx, ty, lw, th),
        Zone::Tr => (rx, ty, rw, th),
        Zone::Bl => (lx, by, lw, bh),
        Zone::Br => (rx, by, rw, bh),
    };
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(sw, sh))
}

// Which edges of a snapped zone are interior — i.e. shared with a neighbouring
// tile and thus draggable via the split divider: (left, right, top, bottom).
fn interior_edges(zone: Zone) -> (bool, bool, bool, bool) {
    match zone {
        Zone::Left => (false, true, false, false),
        Zone::Right => (true, false, false, false),
        Zone::Top => (false, false, false, true),
        Zone::Bottom => (false, false, true, false),
        Zone::Tl => (false, true, false, true),
        Zone::Tr => (true, false, false, true),
        Zone::Bl => (false, true, true, false),
        Zone::Br => (true, false, true, false),
        Zone::Max => (false, false, false, false),
    }
}

fn lerp_rect(a: egui::Rect, b: egui::Rect, t: f32) -> egui::Rect {
    egui::Rect::from_min_max(a.min + (b.min - a.min) * t, a.max + (b.max - a.max) * t)
}

// Given the raw drag zone and how long it's been held, return the zone to commit
// plus the overlay rect to preview (local coords). The Top zone escalates to Max
// after TOP_HOLD seconds, the overlay growing top-half → full over GROW_LEAD.
fn resolve_zone(
    raw: Option<Zone>,
    held: f64,
    asz: egui::Vec2,
    split: egui::Vec2,
) -> (Option<Zone>, Option<egui::Rect>) {
    match raw {
        Some(Zone::Top) => {
            let p = (((held - (TOP_HOLD - GROW_LEAD)) / GROW_LEAD) as f32).clamp(0.0, 1.0);
            let ov = lerp_rect(zone_rect(Zone::Top, asz, split), zone_rect(Zone::Max, asz, split), p);
            let committed = if held >= TOP_HOLD { Zone::Max } else { Zone::Top };
            (Some(committed), Some(ov))
        }
        Some(z) => (Some(z), Some(zone_rect(z, asz, split))),
        None => (None, None),
    }
}

pub enum Content {
    Terminal(Session),
    /// A project window is a sandbox hosting its own nested WindowManager.
    Project(Box<WindowManager>),
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
        }
    }
}

/// One entry in a window's tab-stack: a title and the content it shows. The
/// per-tab title lives here (a window no longer has a single title); the active
/// tab's title is what the titlebar renders and what rename targets.
pub struct Tab {
    pub title: String,
    pub content: Content,
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
    pub snap: Option<Zone>, // Some => tiled/maximized: refit to area each frame
    pub prev: Option<egui::Rect>, // floating rect to restore to when un-snapped
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
    Merge { src: WinId, dst: WinId },
    /// Detach tab `idx` of window `id` into a new floating window at `pos` (local).
    Untab { id: WinId, idx: usize, pos: egui::Pos2 },
}

pub struct WindowManager {
    pub windows: Vec<Win>,
    z: u64,
    focused: Option<WinId>,
    next: WinId,
    dwell_zone: Option<Zone>, // raw zone the drag is currently hovering
    dwell_start: f64,         // time (s) the current dwell_zone was entered
    // Fractional position (0..1) of the tiling dividers. Snapped windows lay out
    // from these, so dragging a shared edge moves the divider for every tile on it.
    split: egui::Vec2,
    /// Working directory new terminals in this manager spawn into. `None` on the
    /// desktop (process cwd); `Some` on a project, set when the project is created.
    cwd: Option<PathBuf>,
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
}

impl WindowManager {
    pub fn new() -> Self {
        Self {
            windows: vec![],
            z: 1,
            focused: None,
            next: 1,
            dwell_zone: None,
            dwell_start: 0.0,
            split: egui::vec2(0.5, 0.5),
            cwd: None,
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
            tabs: vec![Tab { title, content }],
            active: 0,
            rect,
            z: self.z,
            minimized: false,
            snap: None,
            prev: None,
        });
        self.focused = Some(id);
    }

    /// Spawn a terminal into this manager. Returns the new window's id, or `None`
    /// if the PTY failed to spawn (the caller treats that as a no-op).
    pub fn add_terminal(&mut self, shell: Shell, ctx: &egui::Context) -> Option<WinId> {
        let s = Session::spawn(shell, self.cwd.as_deref(), ctx.clone()).ok()?;
        let (id, rect) = self.next_slot(egui::vec2(580.0, 380.0));
        self.push_win(
            id,
            format!("{}  ·  #{}", shell.label(), id),
            rect,
            Content::Terminal(s),
        );
        Some(id)
    }

    /// Add a new project window. It starts as a sandbox containing one terminal.
    /// TODO(status line): show repo / branch on the project titlebar.
    pub fn add_project(&mut self, shell: Shell, cwd: PathBuf, ctx: &egui::Context) {
        let (id, rect) = self.next_slot(egui::vec2(720.0, 480.0));
        let title = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("project {}", id));
        let mut child = WindowManager::new();
        child.cwd = Some(cwd);
        child.add_terminal(shell, ctx);
        self.push_win(id, title, rect, Content::Project(Box::new(child)));
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
                i.events.iter().any(|e| Self::event_chord(e) == Some(leader))
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
        let asz_proj = self.last_area; // desktop area for project-level zoom
        match cmd {
            // ---- project (outer) level: act on the desktop ----
            Command::ProjFocus(d) => self.focus_dir(d),
            Command::ZoomProject => {
                if let Some(id) = self.focused {
                    self.toggle_zoom(id, asz_proj);
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
                    let asz = child.last_area;
                    match other {
                        Command::TermFocus(d) => child.focus_dir(d),
                        Command::TermSnap(d) => child.snap_dir(d),
                        Command::Split(d) => child.split_dir(d, &ctx),
                        Command::ZoomTerm => {
                            if let Some(id) = child.focused {
                                child.toggle_zoom(id, asz);
                            }
                        }
                        Command::CloseTerm => {
                            if let Some(id) = child.focused {
                                child.close_active_tab(id);
                            }
                        }
                        Command::Rename => child.begin_rename(),
                        Command::NewTerm => {
                            child.add_terminal(Shell::PowerShell, &ctx);
                        }
                        Command::LastTerm => child.toggle_last(),
                        Command::TabCycle => child.cycle_tab(true),
                        Command::TabPrev => child.cycle_tab(false),
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

    /// Remove an entire window (all of its tabs) and fix up focus.
    fn close(&mut self, id: WinId) {
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
    /// window restores a sensible floating size. If the source had only one tab,
    /// this is a no-op (dragging the sole tab just moves the window, handled by the
    /// normal title drag).
    fn untab(&mut self, id: WinId, idx: usize, local_pos: egui::Pos2) {
        let Some(w) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        if w.tabs.len() <= 1 || idx >= w.tabs.len() {
            return;
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
            snap: None,
            prev: None,
        });
        self.focus(new_id);
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

    /// Toggle maximize (zoom) for a window — mirrors the `Act::Max` handler.
    fn toggle_zoom(&mut self, id: WinId, asz: egui::Vec2) {
        let split = self.split;
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            if w.snap == Some(Zone::Max) {
                w.snap = None;
                if let Some(pr) = w.prev.take() {
                    w.rect = pr;
                }
            } else {
                w.prev = Some(w.rect);
                w.snap = Some(Zone::Max);
                if asz.x > 1.0 && asz.y > 1.0 {
                    w.rect = zone_rect(Zone::Max, asz, split);
                }
            }
        }
        self.focus(id);
    }

    /// Snap window `id` to `zone`, preserving its pre-snap floating rect in `prev`
    /// so an un-snap can restore it. Mirrors the mouse-drop snap path: only
    /// `snap`/`prev` are set; the show loop refits the rect to `zone_rect` next
    /// frame. A no-op if the window is already snapped to `zone`. This is the one
    /// place split and directional snap share.
    fn set_snap(&mut self, id: WinId, zone: Zone) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            if w.snap == Some(zone) {
                return;
            }
            if w.snap.is_none() {
                w.prev = Some(w.rect);
            }
            w.snap = Some(zone);
        }
    }

    /// Snap the focused window to a half-screen zone. The show loop refits the
    /// rect to `zone_rect` each frame, so we only set `snap`/`prev` here.
    fn snap_dir(&mut self, d: Dir) {
        let Some(id) = self.focused else { return };
        let zone = d.zone();
        // Toggling the same snap pops back to floating (handy to undo).
        let already = self
            .windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.snap == Some(zone))
            .unwrap_or(false);
        if already {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.snap = None;
                if let Some(pr) = w.prev.take() {
                    w.rect = pr;
                }
            }
        } else {
            self.set_snap(id, zone);
        }
        self.focus(id);
    }

    /// Phase 2 split: create a new terminal and place it in the pointed zone.
    ///
    /// - If a window is **already snapped to the target zone**, the newcomer is
    ///   *tabbed* onto it (reusing [`merge_windows`]) instead of stacking a second
    ///   window in the same zone.
    /// - Otherwise the new terminal is snapped to the target zone (reusing the
    ///   snap path via [`set_snap`]).
    /// - If the focused source window was **unsnapped (floating)**, it is also
    ///   snapped to the **opposite** zone for an instant two-pane split. An
    ///   already-snapped source is left untouched.
    ///
    /// No-ops cleanly if there is no source window or the PTY fails to spawn.
    fn split_dir(&mut self, d: Dir, ctx: &egui::Context) {
        let Some(src) = self.focused else { return };
        // Capture the source's snap state *before* creating the newcomer, since
        // source placement depends on whether it was floating.
        let Some(src_snap) = self.windows.iter().find(|w| w.id == src).map(|w| w.snap) else {
            return; // focused id no longer exists
        };
        // Spawn the newcomer. add_terminal focuses it and returns its id; bail on
        // a spawn failure without disturbing the existing layout.
        let Some(new_id) = self.add_terminal(Shell::PowerShell, ctx) else {
            return;
        };
        self.place_split(src, src_snap, new_id, d);
    }

    /// The pure placement half of [`split_dir`] (no PTY/spawn): given the source
    /// window, its pre-split snap state, and an already-created newcomer window,
    /// place the newcomer in the pointed zone — tabbing onto an existing occupant
    /// on collision — and snap an unsnapped source to the opposite zone. Split out
    /// so it is testable without spawning a real `Session`.
    fn place_split(&mut self, src: WinId, src_snap: Option<Zone>, new_id: WinId, d: Dir) {
        let zone = d.zone();
        // Collision: an existing window (not the source, not the newcomer) already
        // snapped to the target zone → the newcomer tabs onto it.
        let collision = self
            .windows
            .iter()
            .find(|w| w.id != src && w.id != new_id && w.snap == Some(zone))
            .map(|w| w.id);

        if let Some(dst) = collision {
            // Tab the fresh terminal onto the occupant (Phase 1 merge op).
            self.merge_windows(new_id, dst);
        } else {
            self.set_snap(new_id, zone);
            self.focus(new_id);
        }

        // Source placement: only when it was floating, snap it opposite so the
        // two panes face each other. An already-snapped source stays put.
        if src_snap.is_none() {
            self.set_snap(src, d.opposite().zone());
        }
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

    /// Returns whether any window in this manager was interacted with this frame.
    /// The parent uses this to propagate focus upward: clicking a sub-window in a
    /// background project bubbles up and switches the desktop to that project.
    pub fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, active: bool, base: egui::Id) -> bool {
        // Record the area so keyboard-driven zoom/snap can commit to a sensible
        // rect before the next render refits it.
        self.last_area = area.size();

        // Leader / command mode: only the root desktop runs it, and only while it
        // is the active (keyboard-owning) manager and no modal is up. Resolve and
        // dispatch *before* the render recursion so command chords are drained
        // from egui input and never reach a terminal's read_input.
        if self.desktop
            && active
            && self.picker.is_none()
            && self.renaming.is_none()
            && self.settings.is_none()
        {
            if let Some(cmd) = self.pump_leader(ui) {
                self.dispatch(cmd, ui);
            }
        }

        ui.painter_at(area)
            .rect_filled(area, egui::CornerRadius::ZERO, DESK_BG);

        let focused = self.focused;
        let asz = area.size();
        let mut order: Vec<usize> = (0..self.windows.len())
            .filter(|&i| !self.windows[i].minimized)
            .collect();
        order.sort_by_key(|&i| self.windows[i].z);

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

            // Re-fit to the (possibly resized) area every frame: snapped/maximized
            // windows recompute to the new size; floating windows clamp back in.
            // This also confines freshly-created windows whose default size is
            // bigger than the area (e.g. a terminal spawned inside a small project).
            {
                let w = &mut self.windows[i];
                match w.snap {
                    Some(z) => w.rect = zone_rect(z, asz, self.split),
                    None => clamp(&mut w.rect, asz),
                }
            }
            let mut scr = self.windows[i].rect.translate(area.min.to_vec2());
            // Projects reserve extra right-side room for the "+" new-project button.
            let ctl_w = if is_project { 116.0 } else { 88.0 };

            // --- title drag (interact first, then we know final position) ---
            let drag_rect =
                egui::Rect::from_min_size(scr.min, egui::vec2((scr.width() - ctl_w).max(0.0), TITLE_H));
            let dr = ui.interact(drag_rect, base.with((id, "drag")), egui::Sense::click_and_drag());
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
                let name_rect = egui::Rect::from_min_size(scr.min, egui::vec2(title_w + 22.0, TITLE_H));
                let on_name = dr.interact_pointer_pos().is_some_and(|p| name_rect.contains(p));
                if on_name {
                    self.renaming = Some(id);
                    self.rename_buf = self.windows[i].title().to_string();
                    self.rename_focus = true;
                } else {
                    acts.push(Act::Max(id));
                }
            }
            if dr.dragged() {
                {
                    let w = &mut self.windows[i];
                    // Dragging pops a snapped/maximized window back to floating. Like
                    // double-click/restore, it returns to its pre-snap size; we re-anchor
                    // the restored rect under the cursor so the title stays grabbed.
                    if w.snap.is_some() {
                        if let (Some(pr), Some(p)) =
                            (w.prev.take(), ui.ctx().pointer_latest_pos())
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
                        w.snap = None;
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
                let over_target =
                    pointer.and_then(|p| self.merge_target_at(id, p, area, &order));
                if let Some(tgt) = over_target {
                    merge_hint = Some(tgt);
                    self.dwell_zone = None;
                } else {
                    // --- snap: detect zone under the pointer (relative to area) ---
                    // The top zone escalates to maximize the longer you hold it; the
                    // overlay grows from top-half to full to telegraph the switch.
                    if let Some(p) = pointer {
                        let fx = (p.x - area.min.x) / asz.x;
                        let fy = (p.y - area.min.y) / asz.y;
                        let now = ui.input(|inp| inp.time);
                        let raw = detect_zone(fx, fy);
                        if raw != self.dwell_zone {
                            self.dwell_zone = raw;
                            self.dwell_start = now;
                        }
                        let held = now - self.dwell_start;
                        let (_committed, overlay) = resolve_zone(raw, held, asz, self.split);
                        if let Some(r) = overlay {
                            snap_overlay = Some(r.translate(area.min.to_vec2()));
                        }
                    }
                }
            }
            if dr.drag_stopped() {
                let pointer = ui.ctx().pointer_latest_pos();
                // A drop onto another window merges (tabs) onto it and wins over the
                // snap: the dragged window is consumed, so we skip snapping entirely.
                let merge_dst =
                    pointer.and_then(|p| self.merge_target_at(id, p, area, &order));
                if let Some(dst_i) = merge_dst {
                    let dst = self.windows[dst_i].id;
                    acts.push(Act::Merge { src: id, dst });
                } else if let Some(p) = pointer {
                    // commit the snap on release, applying the hold-to-maximize escalation
                    let fx = (p.x - area.min.x) / asz.x;
                    let fy = (p.y - area.min.y) / asz.y;
                    let now = ui.input(|inp| inp.time);
                    let raw = detect_zone(fx, fy);
                    let held = if raw == self.dwell_zone {
                        now - self.dwell_start
                    } else {
                        0.0
                    };
                    let (committed, _) = resolve_zone(raw, held, asz, self.split);
                    if let Some(zone) = committed {
                        let split = self.split;
                        let w = &mut self.windows[i];
                        // remember the free-floating rect so restore/un-snap works
                        w.prev = Some(w.rect);
                        w.snap = Some(zone);
                        w.rect = zone_rect(zone, asz, split);
                        scr = w.rect.translate(area.min.to_vec2());
                    }
                }
                self.dwell_zone = None;
            }

            let title_rect = egui::Rect::from_min_size(scr.min, egui::vec2(scr.width(), TITLE_H));
            let content_rect = egui::Rect::from_min_max(
                egui::pos2(scr.min.x + 1.0, scr.min.y + TITLE_H),
                egui::pos2(scr.max.x - 1.0, scr.max.y - 1.0),
            );

            // --- paint window ---
            // Snapped/maximized windows square their corners so they tile flush to
            // the area edges and to each other (rounded corners would leave gaps).
            let cr = if self.windows[i].snap.is_some() {
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
            p.rect_filled(
                title_rect,
                cr,
                if is_focus { tbg_focus } else { tbg },
            );
            if is_renaming {
                // Field box centered in the titlebar; `vertical_align(Center)` lets
                // egui center the text within it, so no pixel-fudging is needed.
                let te_h = TITLE_H - 8.0;
                let te_rect = egui::Rect::from_min_size(
                    egui::pos2(scr.min.x + 8.0, scr.min.y + (TITLE_H - te_h) * 0.5),
                    egui::vec2((scr.width() - ctl_w - 14.0).max(40.0), te_h),
                );
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
                    let chip = egui::Rect::from_min_size(
                        egui::pos2(cx, cy),
                        egui::vec2(chip_w, chip_h),
                    );
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
                        [egui::pos2(xc.x - xs, xc.y - xs), egui::pos2(xc.x + xs, xc.y + xs)],
                        xstroke,
                    );
                    pp.line_segment(
                        [egui::pos2(xc.x - xs, xc.y + xs), egui::pos2(xc.x + xs, xc.y - xs)],
                        xstroke,
                    );
                    if xresp.clicked() {
                        acts.push(Act::CloseTab(id, ti));
                    } else if chip_resp.clicked() {
                        acts.push(Act::SetTab(id, ti));
                    } else if chip_resp.drag_stopped() {
                        // Dragging a chip off the titlebar detaches it into its own
                        // floating window at the drop point (local coords).
                        if let Some(dp) = ui.ctx().pointer_latest_pos() {
                            let off = (dp.y - scr.min.y).abs() > TITLE_H * 1.5
                                || dp.x < scr.min.x
                                || dp.x > scr.max.x;
                            if off {
                                let local = dp - area.min.to_vec2();
                                acts.push(Act::Untab {
                                    id,
                                    idx: ti,
                                    pos: egui::pos2(local.x, local.y),
                                });
                            } else {
                                // Dropped back on the bar: just activate it.
                                acts.push(Act::SetTab(id, ti));
                            }
                        }
                    }
                    cx += chip_w + 4.0;
                }
            } else {
                p.text(
                    egui::pos2(scr.min.x + 11.0, scr.min.y + TITLE_H / 2.0),
                    egui::Align2::LEFT_CENTER,
                    self.windows[i].title(),
                    egui::FontId::proportional(12.5),
                    if is_focus { TEXT } else { DIM },
                );
            }

            // --- dispatch keys (project headers only) ---
            // Compact "PS · CMD · SH" stamped after the title: clicking one spawns
            // a terminal of that shell *into this project*. Lives here (not the
            // global bar) so the target site is unambiguous — the window you click.
            if is_project {
                let title_w = ui
                    .painter()
                    .layout_no_wrap(
                        self.windows[i].title().to_string(),
                        egui::FontId::proportional(12.5),
                        TEXT,
                    )
                    .size()
                    .x;
                let kh = TITLE_H - 10.0;
                let ky = scr.min.y + 5.0;
                let mut kx = scr.min.x + 11.0 + title_w + 14.0;
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
                    let kresp = ui.interact(r, base.with((id, "disp", label)), egui::Sense::click());
                    let kbg = if kresp.hovered() {
                        egui::Color32::from_rgb(72, 82, 76)
                    } else {
                        egui::Color32::from_rgb(45, 51, 48)
                    };
                    ui.painter().rect_filled(r, egui::CornerRadius::same(3), kbg);
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
            // snapped window's INTERIOR edge (shared with a neighbour) drags the tiling
            // divider so both tiles resize together, while an OUTER edge pops it back to
            // floating. Corners that touch any outer edge also pop to floating.
            let bnd = RESIZE_BAND;
            let (x0, y0, x1, y1) = (scr.min.x, scr.min.y, scr.max.x, scr.max.y);
            type Ci = egui::CursorIcon;
            // (key, rect, left, right, top, bottom, cursor)
            let handles: [(&str, egui::Rect, bool, bool, bool, bool, Ci); 8] = [
                ("w", egui::Rect::from_min_max(egui::pos2(x0, y0 + bnd), egui::pos2(x0 + bnd, y1 - bnd)), true, false, false, false, Ci::ResizeWest),
                ("e", egui::Rect::from_min_max(egui::pos2(x1 - bnd, y0 + bnd), egui::pos2(x1, y1 - bnd)), false, true, false, false, Ci::ResizeEast),
                ("n", egui::Rect::from_min_max(egui::pos2(x0 + bnd, y0), egui::pos2(x1 - bnd, y0 + bnd)), false, false, true, false, Ci::ResizeNorth),
                ("s", egui::Rect::from_min_max(egui::pos2(x0 + bnd, y1 - bnd), egui::pos2(x1 - bnd, y1)), false, false, false, true, Ci::ResizeSouth),
                ("nw", egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x0 + bnd, y0 + bnd)), true, false, true, false, Ci::ResizeNorthWest),
                ("ne", egui::Rect::from_min_max(egui::pos2(x1 - bnd, y0), egui::pos2(x1, y0 + bnd)), false, true, true, false, Ci::ResizeNorthEast),
                ("sw", egui::Rect::from_min_max(egui::pos2(x0, y1 - bnd), egui::pos2(x0 + bnd, y1)), true, false, false, true, Ci::ResizeSouthWest),
                ("se", egui::Rect::from_min_max(egui::pos2(x1 - bnd, y1 - bnd), egui::pos2(x1, y1)), false, true, false, true, Ci::ResizeSouthEast),
            ];
            for (key, hr, hl, hrr, ht, hb, cursor) in handles {
                let resp = ui.interact(hr, base.with((id, "rsz", key)), egui::Sense::drag());
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if resp.drag_started() {
                    acts.push(Act::Focus(id));
                }
                if !resp.dragged() {
                    continue;
                }
                let d = resp.drag_delta();
                match self.windows[i].snap {
                    Some(zone) => {
                        let (il, ir, it, ib) = interior_edges(zone);
                        let touches_outer =
                            (hl && !il) || (hrr && !ir) || (ht && !it) || (hb && !ib);
                        if touches_outer {
                            let w = &mut self.windows[i];
                            w.snap = None;
                            w.prev = None;
                            resize_floating(&mut w.rect, d, hl, hrr, ht, hb, asz);
                        } else {
                            // drag the shared divider(s); every tile on it refits next frame
                            if hl || hrr {
                                let lo = (MIN_TILE / asz.x).min(0.5);
                                self.split.x = (self.split.x + d.x / asz.x).clamp(lo, 1.0 - lo);
                            }
                            if ht || hb {
                                let lo = (MIN_TILE / asz.y).min(0.5);
                                self.split.y = (self.split.y + d.y / asz.y).clamp(lo, 1.0 - lo);
                            }
                        }
                    }
                    None => resize_floating(&mut self.windows[i].rect, d, hl, hrr, ht, hb, asz),
                }
            }
        }

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
            let r = self.windows[j].rect.translate(area.min.to_vec2()).intersect(area);
            let p = ui.painter_at(area);
            p.rect_filled(r, egui::CornerRadius::same(8), SNAP_FILL);
            p.rect_stroke(
                r,
                egui::CornerRadius::same(8),
                egui::Stroke::new(2.0, SNAP_STROKE),
                egui::StrokeKind::Inside,
            );
        }

        // --- taskbar (minimized) ---
        let mins: Vec<(WinId, String)> = self
            .windows
            .iter()
            .filter(|w| w.minimized)
            .map(|w| (w.id, w.title().to_string()))
            .collect();
        if !mins.is_empty() {
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

        // Any Act means a window in this manager was interacted with this frame.
        // Captured before the apply loop consumes `acts`, returned at the end so
        // the parent can bubble focus upward through arbitrary nesting depth.
        let interacted = !acts.is_empty();

        let ctx = ui.ctx().clone();
        for a in acts {
            match a {
                Act::Focus(id) => self.focus(id),
                Act::AddTerm(id, shell) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        if let Content::Project(wm) = w.active_content() {
                            wm.add_terminal(shell, &ctx);
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
                Act::Untab { id, idx, pos } => self.untab(id, idx, pos),
                Act::Min(id) => {
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
                Act::Max(id) => {
                    let split = self.split;
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        if w.snap == Some(Zone::Max) {
                            w.snap = None;
                            if let Some(pr) = w.prev.take() {
                                w.rect = pr;
                            }
                        } else {
                            w.prev = Some(w.rect);
                            w.snap = Some(Zone::Max);
                            w.rect = zone_rect(Zone::Max, asz, split);
                        }
                    }
                    self.focus(id);
                }
            }
        }

        if let Some(mut picker) = self.picker.take() {
            match picker.show(ui) {
                Outcome::Pending => self.picker = Some(picker),
                Outcome::Cancelled => {}
                Outcome::Accepted(path) => self.add_project(Shell::PowerShell, path, &ctx),
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

        interacted
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
        rows.push(("Leader".into(), format!("{}  (then a command)", self.keymap.leader.pretty())));
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
            }],
            active: 0,
            rect: egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(400.0, 300.0)),
            z: wm.z,
            minimized: false,
            snap: None,
            prev: None,
        });
        wm.focused = Some(id);
        id
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
        assert_eq!(wm.windows.iter().find(|w| w.id == id).unwrap().active, 0, "wraps");
        wm.cycle_tab(false);
        assert_eq!(wm.windows.iter().find(|w| w.id == id).unwrap().active, 2, "back wraps");
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

    // --- Phase 2: split placement (drives place_split with stub windows so no
    // real PTY/Session is spawned; split_dir only adds the spawn + id capture). ---

    #[test]
    fn dir_zone_and_opposite_mapping() {
        assert_eq!(Dir::Left.zone(), Zone::Left);
        assert_eq!(Dir::Right.zone(), Zone::Right);
        assert_eq!(Dir::Up.zone(), Zone::Top);
        assert_eq!(Dir::Down.zone(), Zone::Bottom);
        assert_eq!(Dir::Left.opposite(), Dir::Right);
        assert_eq!(Dir::Right.opposite(), Dir::Left);
        assert_eq!(Dir::Up.opposite(), Dir::Down);
        assert_eq!(Dir::Down.opposite(), Dir::Up);
    }

    #[test]
    fn split_from_floating_source_snaps_both_panes() {
        // Source is floating; Alt+D (Right) → newcomer right, source snaps left.
        let mut wm = WindowManager::new();
        let src = push(&mut wm, "src");
        let newcomer = push(&mut wm, "new"); // stands in for the freshly spawned terminal
        let src_snap = wm.windows.iter().find(|w| w.id == src).unwrap().snap;
        assert_eq!(src_snap, None, "source starts floating");

        wm.place_split(src, src_snap, newcomer, Dir::Right);

        let s = wm.windows.iter().find(|w| w.id == src).unwrap();
        let n = wm.windows.iter().find(|w| w.id == newcomer).unwrap();
        assert_eq!(n.snap, Some(Zone::Right), "newcomer snaps to pointed zone");
        assert_eq!(s.snap, Some(Zone::Left), "floating source snaps opposite");
        assert!(n.prev.is_some(), "newcomer keeps a pre-snap floating rect");
        assert!(s.prev.is_some(), "source keeps a pre-snap floating rect");
        assert_eq!(wm.focused, Some(newcomer), "newcomer is focused");
    }

    #[test]
    fn split_from_snapped_source_leaves_source_untouched() {
        // Source already snapped left; Alt+D → newcomer right, source stays left.
        let mut wm = WindowManager::new();
        let src = push(&mut wm, "src");
        wm.set_snap(src, Zone::Left);
        let src_prev = wm.windows.iter().find(|w| w.id == src).unwrap().prev;
        let newcomer = push(&mut wm, "new");
        let src_snap = wm.windows.iter().find(|w| w.id == src).unwrap().snap;

        wm.place_split(src, src_snap, newcomer, Dir::Right);

        let s = wm.windows.iter().find(|w| w.id == src).unwrap();
        let n = wm.windows.iter().find(|w| w.id == newcomer).unwrap();
        assert_eq!(s.snap, Some(Zone::Left), "snapped source is untouched");
        assert_eq!(s.prev, src_prev, "source prev rect unchanged");
        assert_eq!(n.snap, Some(Zone::Right), "newcomer snaps to pointed zone");
    }

    #[test]
    fn split_into_occupied_zone_tabs_onto_occupant() {
        // A window already snapped right; splitting right again must tab onto it,
        // not place a second window in the same zone.
        let mut wm = WindowManager::new();
        let src = push(&mut wm, "src");
        let occupant = push(&mut wm, "occ");
        wm.set_snap(occupant, Zone::Right);
        let newcomer = push(&mut wm, "new");
        let src_snap = wm.windows.iter().find(|w| w.id == src).unwrap().snap;
        let before = wm.windows.len();

        wm.place_split(src, src_snap, newcomer, Dir::Right);

        assert_eq!(wm.windows.len(), before - 1, "newcomer merged, not added");
        let occ = wm.windows.iter().find(|w| w.id == occupant).unwrap();
        assert_eq!(occ.snap, Some(Zone::Right), "occupant keeps its zone");
        assert_eq!(occ.tabs.len(), 2, "newcomer became a tab on the occupant");
        assert_eq!(occ.tabs[occ.active].title, "new", "merged tab is active");
        assert!(
            !wm.windows.iter().any(|w| w.id == newcomer),
            "newcomer window no longer exists on its own"
        );
        assert_eq!(wm.focused, Some(occupant), "occupant is focused after tab");
        // Floating source still gets the opposite zone.
        let s = wm.windows.iter().find(|w| w.id == src).unwrap();
        assert_eq!(s.snap, Some(Zone::Left), "floating source snaps opposite");
    }
}
