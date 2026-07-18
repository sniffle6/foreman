use crate::dirpicker::{DirPicker, Outcome};
use crate::keymap::{Chord, Command, Keymap};
use crate::settings::{Outcome as SettingsOutcome, SettingsView};
use crate::settings_menu::{MenuOutcome, SettingsMenu};
use crate::terminal::{Session, Shell};
use crate::theme::*;
use eframe::egui;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub type WinId = u64;

/// Result of [`WindowManager::apply_workspace`] / nested manager restore.
/// Used for startup logs and unit tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub projects_restored: usize,
    pub projects_skipped: usize,
}

impl ApplyReport {
    fn merge(&mut self, other: ApplyReport) {
        self.projects_restored += other.projects_restored;
        self.projects_skipped += other.projects_skipped;
    }
}

/// Reconstruct an egui rect from a workspace snapshot rect (local min + size).
fn rect_from_snap(r: &crate::workspace::RectSnap) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w, r.h))
}

// Quiescence window for `foreman send`: after writing input, wait this long
// with no new PTY bytes before replying so a following snapshot reads settled
// state. The default (absent an explicit `settle_ms` on the request) is
// `Settings::send_settle_ms`. MAX_SETTLE_MS is a hard cap on the total wait —
// defense in depth on top of `Settings::sanitize`'s 2000 clamp — and stays
// under control::REPLY_TIMEOUT (5s) so the pipe server's recv_timeout never
// fires before a settle reply lands.
const MAX_SETTLE_MS: u64 = 4000;

// One pending `foreman send` settle: the terminal to watch, the channel to
// answer, and the silence-timer state advanced each frame by `advance_settles`.
struct PendingSettle {
    pid: WinId,
    tid: WinId,
    reply: std::sync::mpsc::Sender<crate::control::OpenReply>,
    last_gen: u64,
    quiet_since: std::time::Instant,
    deadline: std::time::Instant,
    quiet_window: std::time::Duration,
}

/// One settle tick. If output arrived (gen changed) the quiet window restarts.
/// Returns (updated last_gen, updated quiet_since, done).
fn settle_tick(
    last_gen: u64,
    quiet_since: std::time::Instant,
    deadline: std::time::Instant,
    quiet_window: std::time::Duration,
    current_gen: u64,
    now: std::time::Instant,
) -> (u64, std::time::Instant, bool) {
    let (last_gen, quiet_since) = if current_gen != last_gen {
        (current_gen, now)
    } else {
        (last_gen, quiet_since)
    };
    let done = now.duration_since(quiet_since) >= quiet_window || now >= deadline;
    (last_gen, quiet_since, done)
}

const BORDER_W: f32 = 0.75; // uniform window border width; focus is shown by colour

const TITLE_H: f32 = 26.0;

// Leader (prefix) key — tmux-style. After it is pressed the next chord is a
// *command* (consumed, never sent to the PTY). The leader is now data-driven:
// it lives in `Keymap::leader` (default `Ctrl+b`), loaded from
// `%APPDATA%\foreman\keybindings.json` and overridable per user.

const RESIZE_BAND: f32 = 6.0; // thickness of the invisible edge/corner resize hit-zones
const MIN_W: f32 = 240.0; // smallest a floating window may be dragged to
const MIN_H: f32 = 140.0;

const SNAP_GAP: f32 = 0.0; // inset of zones from the area edge; 0 = windows tile edge-to-edge

// A cardinal direction for directional focus / snap commands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    /// The opposite cardinal (Left↔Right, Up↔Down).
    pub fn opposite(self) -> Self {
        match self {
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
        }
    }
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
    /// Desktop-level task-manager panel (project/tab list). At most one per
    /// desktop; non-closable / non-minimizable / non-tabbable.
    TaskManager(crate::panel::PanelView),
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
        app_modal: bool,
    ) -> bool {
        match self {
            Content::Terminal(s) => {
                s.show(ui, rect, active, resp);
                false
            }
            // Recurse: the project's content rect becomes the child manager's area.
            // The child only reads the keyboard if this project is itself active,
            // so `active` ANDs down the tree to exactly one leaf terminal.
            // `app_modal` threads the app-wide "a confirm is open" state down.
            Content::Project(wm) => {
                wm.show(ui, rect, active, base.with(("proj", win_id)), app_modal)
            }
            Content::Chat(view) => {
                // Paint lives in chat_view.rs; click/pending_post drained after
                // apply_acts (see ChatView::show docs).
                view.show(ui, rect, active, resp, base.with((win_id, "chat-input")));
                false
            }
            Content::TaskManager(view) => {
                view.show(ui, rect, base.with((win_id, "panel")));
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
            Content::TaskManager(_) => {}
        }
    }

    /// The tab icon for this content: the terminal's agent/shell logo, a folder
    /// for a project, and none for the chat viewer.
    fn icon_kind(&self) -> Option<crate::icons::IconKind> {
        match self {
            Content::Terminal(s) => Some(s.icon_kind()),
            Content::Project(_) => Some(crate::icons::IconKind::Folder),
            Content::Chat(_) => None,
            Content::TaskManager(_) => None,
        }
    }

    /// Whether this content is a terminal with a latched Bell. Projects do
    /// not bubble up on window chrome (the panel carries project-level Bell).
    fn bell_active(&self) -> bool {
        matches!(self, Content::Terminal(s) if s.bell_active())
    }
}

/// One entry in a window's tab-stack: a title and the content it shows. The
/// per-tab title lives here (a window no longer has a single title); the active
/// tab's title is what the titlebar renders and what rename targets.
pub struct Tab {
    pub title: String,
    pub content: Content,
    /// When true, [`WindowManager::refresh_auto_titles`] may replace `title`
    /// with an agent name once the Session detects one (hand-launched / landing
    /// agents in a default-named shell). Cleared by manual rename; never set
    /// for dispatch-spawned titles, projects, chat, or the sessions panel.
    auto_title: bool,
}

impl Tab {
    /// Fixed title (no auto-rename). Used for projects, chat, panel, dispatch.
    fn fixed(title: impl AsRef<str>, content: Content) -> Self {
        Self {
            title: title.as_ref().to_string(),
            content,
            auto_title: false,
        }
    }

    /// Default shell title — auto-renames when an agent is detected.
    fn shell_default(title: impl AsRef<str>, content: Content) -> Self {
        Self {
            title: title.as_ref().to_string(),
            content,
            auto_title: true,
        }
    }
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
    /// True while minimized if the window was tiled when minimized — restore
    /// re-enters the tree instead of leaving the window floating.
    pub min_from_tree: bool,
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
    /// Border pulse rule: the whole stack pulses while ANY of its tabs rings.
    fn bell_active(&self) -> bool {
        self.tabs.iter().any(|t| t.content.bell_active())
    }
    /// Task-manager panel window (any tab). Non-closable / non-minimizable /
    /// non-tabbable; excluded from `deserted` and `panel_model`.
    pub fn is_panel(&self) -> bool {
        self.tabs
            .iter()
            .any(|t| matches!(t.content, Content::TaskManager(_)))
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

#[derive(Clone)]
enum Act {
    Focus(WinId),
    Close(WinId),
    Min(WinId),
    Max(WinId),
    /// Toggle window between tiled and floating (the header toggle button).
    Float(WinId),
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
    /// Focus / restore a panel row target (project, child window, and/or tab).
    FocusPath(crate::panel::TargetPath),
    /// Minimize a panel row target.
    MinPath(crate::panel::TargetPath),
    /// Close a panel row target (routes through the close-confirm path).
    ClosePath(crate::panel::TargetPath),
}

/// What a validated chat request resolved to. Posting is split from injection
/// so the reply (the ack handle) is sent before the per-frame
/// `chat_delivery_sweep` injects the post (spec §3: reply-before-inject).
enum ChatOutcome {
    Posted {
        /// The posted message's seq — returned to the sender as its ack handle.
        seq: Option<u64>,
    },
    History(Vec<String>),
}

/// What a pending close would destroy once confirmed. Held in `pending_close`
/// while the modal is up; consumed by `resolve_pending`.
enum CloseTarget {
    /// The active tab of window `id` (titlebar `x`, leader close).
    ActiveTab(WinId),
    /// A specific tab index of window `id` (tab-bar `x`).
    Tab(WinId, usize),
    /// The whole app: set by the app-quit guard (Task 5).
    Quit,
}

/// An in-flight close awaiting confirmation: the doomed target plus the modal
/// view rendering the running-process list.
struct PendingClose {
    target: CloseTarget,
    view: crate::confirm::ConfirmClose,
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
    /// This project's chat room (a harmless empty room at desktop level —
    /// `tick` no-ops with no members). Shared with the viewer window
    /// (`Content::Chat`), hence the Rc<RefCell<…>>.
    pub chat: Rc<RefCell<crate::chat::ChatRoom>>,
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
    /// Opened from the settings menu's Keybindings pane; stacks on top of it.
    keymap_editor: Option<SettingsView>,
    /// When `Some`, the settings menu modal is open (desktop only). The primary
    /// settings surface; the keybindings editor opens on top of it.
    menu: Option<SettingsMenu>,
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
    /// The window whose in-flight header drag started tiled/zoomed (tear-out).
    /// Such a drag keeps the tree drop hints without a modifier; a drag that
    /// started floating is a free move unless Shift is held.
    drag_from_tree: Option<WinId>,
    /// Pending `foreman send` settle entries, serviced each frame by
    /// `advance_settles` so the GUI never blocks waiting for a terminal to quiet.
    pending_settles: Vec<PendingSettle>,
    /// When `Some`, this manager's close-confirm modal is open: a close (or the
    /// app quit) is waiting on the user. Holds the target and the modal view.
    /// At most one is pending *per manager*; the app-wide "only one anywhere"
    /// rule is enforced through `app_modal`, which also holds the app alive.
    pending_close: Option<PendingClose>,
    /// Set true (on the desktop) once a quit confirm is accepted, so the app-quit
    /// guard in main.rs can drive the actual OS close.
    quit_confirmed: bool,
    /// True this frame when a confirm modal is open ANYWHERE in the app (this
    /// manager or any nested project). A confirm is globally modal: while it is
    /// set every terminal's keyboard is frozen (via `is_focus`) and no second
    /// confirm may open (checked in the close funnels). Recomputed each frame at
    /// the top of `show` from the threaded `app_modal` arg + `any_pending_close`.
    app_modal: bool,
    /// Project opens since the last drain: (cwd, injected command if any).
    /// Pushed by add_project / add_project_with_command; the app drains it each
    /// frame to record recents. The engine never learns what a "recent" is —
    /// it only reports (spec: open-drain seam).
    opened: Vec<(PathBuf, Option<String>)>,
    /// Structural workspace change since last poll (layout/open/close/focus/…).
    /// Desktop `poll_workspace_dirty` ORs this with nested project children.
    workspace_dirty: bool,
    /// Version string to show as the panel's update chip (None = hidden).
    update_chip: Option<String>,
    /// Latched when the user clicks the chip; App drains it each frame.
    update_clicked: bool,
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
            chat: Rc::new(RefCell::new(crate::chat::ChatRoom::new())),
            picker: None,
            renaming: None,
            rename_buf: String::new(),
            rename_focus: false,
            desktop: false,
            armed: false,
            show_help: false,
            keymap_editor: None,
            menu: None,
            last_focused: None,
            last_area: egui::vec2(0.0, 0.0),
            keymap: Keymap::default(),
            tree: Default::default(),
            zoomed: None,
            drag_from_tree: None,
            pending_settles: Vec::new(),
            pending_close: None,
            quit_confirmed: false,
            app_modal: false,
            opened: Vec::new(),
            workspace_dirty: false,
            update_chip: None,
            update_clicked: false,
        }
    }

    /// Mark this manager as the root desktop: it runs the leader state machine,
    /// and load the user's key bindings (merged over the in-code defaults).
    pub fn as_desktop(mut self) -> Self {
        self.desktop = true;
        self.keymap = Keymap::load();
        self
    }

    /// Cold-snapshot this manager (and nested projects) for workspace persistence.
    ///
    /// Capture rules:
    /// - Live `WinId` is stored as `SnapId` (identity). Apply still allocates
    ///   fresh runtime ids and remaps.
    /// - Windows whose every tab is `TaskManager` are omitted entirely; mixed
    ///   TaskManager tabs (should not happen) are skipped per-tab.
    /// - `windows` is ordered by ascending `z` (low index = back, high = front).
    /// - Layout tree leaves for omitted windows are dropped (splits collapse).
    /// - `focused` / `last_focused` / `zoomed` are kept only when the id remains.
    pub fn capture_manager(&self) -> crate::workspace::ManagerSnap {
        use crate::workspace::{
            ContentSnap, ManagerSnap, TabSnap, WinSnap, rect_to_snap, shell_to_str, tree_from_snap,
            tree_to_snap,
        };
        use std::collections::HashSet;

        // Back → front: sort by ascending z before filtering.
        let mut ordered: Vec<&Win> = self.windows.iter().collect();
        ordered.sort_by_key(|w| w.z);

        let mut windows: Vec<WinSnap> = Vec::with_capacity(ordered.len());
        for w in ordered {
            let mut tabs: Vec<TabSnap> = Vec::new();
            let mut new_active = 0usize;
            let mut found_active = false;
            for (i, t) in w.tabs.iter().enumerate() {
                if matches!(t.content, Content::TaskManager(_)) {
                    continue;
                }
                if i == w.active {
                    new_active = tabs.len();
                    found_active = true;
                }
                let content = match &t.content {
                    Content::Terminal(s) => ContentSnap::Terminal {
                        shell: shell_to_str(s.shell).into(),
                    },
                    Content::Chat(_) => ContentSnap::Chat,
                    Content::Project(child) => ContentSnap::Project {
                        child: child.capture_manager(),
                    },
                    Content::TaskManager(_) => unreachable!("filtered above"),
                };
                tabs.push(TabSnap {
                    title: t.title.clone(),
                    content,
                });
            }
            if tabs.is_empty() {
                // TaskManager-only window (desktop panel): omit entirely.
                continue;
            }
            if !found_active {
                new_active = 0;
            }
            if new_active >= tabs.len() {
                new_active = tabs.len() - 1;
            }
            windows.push(WinSnap {
                id: w.id, // SnapId == live WinId at capture time
                active: new_active,
                tabs,
                minimized: w.minimized,
                min_from_tree: w.min_from_tree,
                rect: rect_to_snap(w.rect),
                prev: w.prev.map(rect_to_snap),
            });
        }

        let included: HashSet<WinId> = windows.iter().map(|w| w.id).collect();
        // Identity map for included leaves; drop leaves for omitted windows
        // (panel) via tree_from_snap collapse, then re-encode as NodeSnap.
        let identity = |id: WinId| id;
        let raw = tree_to_snap(&self.tree, &identity);
        let filtered = tree_from_snap(raw.as_ref(), &|id| {
            if included.contains(&id) {
                Some(id)
            } else {
                None
            }
        });
        let tree = tree_to_snap(&filtered, &identity);

        let keep = |id: Option<WinId>| id.filter(|i| included.contains(i));

        ManagerSnap {
            cwd: self.cwd.clone(),
            focused: keep(self.focused),
            last_focused: keep(self.last_focused),
            zoomed: keep(self.zoomed),
            windows,
            tree,
        }
    }

    /// Snapshot the full desktop document (`version` + this manager as `desktop`).
    pub fn capture_workspace(&self) -> crate::workspace::WorkspaceSnapshot {
        crate::workspace::WorkspaceSnapshot {
            version: crate::workspace::WORKSPACE_VERSION,
            desktop: self.capture_manager(),
        }
    }

    /// Flag that the workspace layout (or nested project layout) changed.
    pub fn mark_workspace_dirty(&mut self) {
        self.workspace_dirty = true;
    }

    /// Consume this manager's local dirty bit (does not recurse).
    pub fn take_workspace_dirty(&mut self) -> bool {
        let d = self.workspace_dirty;
        self.workspace_dirty = false;
        d
    }

    /// True if this manager or any nested project child is dirty; clears all.
    pub fn poll_workspace_dirty(&mut self) -> bool {
        let mut dirty = self.take_workspace_dirty();
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Project(child) = &mut t.content {
                    dirty |= child.poll_workspace_dirty();
                }
            }
        }
        dirty
    }

    /// Rebuild this manager (and nested projects) from a cold workspace snapshot.
    ///
    /// Clears windows/tree/focus on this manager, remaps snapshot ids to fresh
    /// `WinId`s, spawns shells for terminal tabs, and restores the layout tree.
    /// Does **not** call `add_project` (no default terminal, no recents drain)
    /// and does **not** create a TaskManager panel — callers run `ensure_panel`
    /// after apply.
    pub fn apply_workspace(
        &mut self,
        snap: &crate::workspace::WorkspaceSnapshot,
        ctx: &egui::Context,
    ) -> ApplyReport {
        self.apply_manager(&snap.desktop, ctx)
    }

    /// Apply one `ManagerSnap` into `self` (desktop or nested project child).
    fn apply_manager(
        &mut self,
        snap: &crate::workspace::ManagerSnap,
        ctx: &egui::Context,
    ) -> ApplyReport {
        use crate::workspace::{ContentSnap, SnapId, shell_from_str, tree_from_snap};
        use std::collections::HashMap;

        // Replace live structure; do not reset `next`/`z` so re-apply on a
        // long-lived manager never reuses ids that may still be referenced.
        self.windows.clear();
        self.tree = Default::default();
        self.focused = None;
        self.last_focused = None;
        self.zoomed = None;

        let mut report = ApplyReport::default();
        let mut snap_id_to_win: HashMap<SnapId, WinId> = HashMap::new();

        for win_snap in &snap.windows {
            // Provisional id used for term env / project tags; only consumed
            // if at least one tab materializes.
            let provisional_id = self.next;
            let mut tabs: Vec<Tab> = Vec::new();
            let mut new_active = 0usize;
            let mut found_active = false;

            for (i, tab_snap) in win_snap.tabs.iter().enumerate() {
                let content = match &tab_snap.content {
                    ContentSnap::Terminal { shell } => {
                        let shell = shell_from_str(shell);
                        let env = self.term_env(provisional_id);
                        match Session::spawn(shell, self.cwd.as_deref(), &env, ctx.clone()) {
                            Ok(mut s) => {
                                s.set_term_id(provisional_id);
                                Content::Terminal(s)
                            }
                            Err(e) => {
                                eprintln!("foreman: restore terminal spawn failed: {e}");
                                continue;
                            }
                        }
                    }
                    ContentSnap::Chat => {
                        Content::Chat(crate::chat::ChatView::new(Rc::clone(&self.chat)))
                    }
                    ContentSnap::Project { child } => {
                        if child.cwd.as_ref().is_none_or(|p| !p.is_dir()) {
                            report.projects_skipped += 1;
                            continue;
                        }
                        let mut nested = WindowManager::new();
                        nested.cwd = child.cwd.clone();
                        // Tag before nested apply so child terminals get
                        // FOREMAN_PROJECT_ID (id == provisional, finalized below).
                        nested.tag = Some(format!("p{provisional_id}"));
                        let nested_rep = nested.apply_manager(child, ctx);
                        report.merge(nested_rep);
                        report.projects_restored += 1;
                        Content::Project(Box::new(nested))
                    }
                };
                if i == win_snap.active {
                    new_active = tabs.len();
                    found_active = true;
                }
                // Terminals whose title is still a managed default (shell or
                // prior agent auto-name) keep auto_title so a re-detected agent
                // renames them. Custom user names stay fixed.
                let tab = match &content {
                    Content::Terminal(_) if title_is_auto_managed(&tab_snap.title) => {
                        Tab::shell_default(tab_snap.title.clone(), content)
                    }
                    _ => Tab::fixed(tab_snap.title.clone(), content),
                };
                tabs.push(tab);
            }

            if tabs.is_empty() {
                continue;
            }
            if !found_active {
                new_active = 0;
            }
            if new_active >= tabs.len() {
                new_active = tabs.len() - 1;
            }

            let id = self.next;
            self.next += 1;
            self.z += 1;
            debug_assert_eq!(id, provisional_id);
            snap_id_to_win.insert(win_snap.id, id);

            self.windows.push(Win {
                id,
                tabs,
                active: new_active,
                rect: rect_from_snap(&win_snap.rect),
                z: self.z,
                minimized: win_snap.minimized,
                min_from_tree: win_snap.min_from_tree,
                prev: win_snap.prev.as_ref().map(rect_from_snap),
            });
        }

        self.tree = tree_from_snap(snap.tree.as_ref(), &|sid| snap_id_to_win.get(&sid).copied());
        let map = |id: Option<SnapId>| id.and_then(|s| snap_id_to_win.get(&s).copied());
        self.focused = map(snap.focused);
        self.last_focused = map(snap.last_focused);
        self.zoomed = map(snap.zoomed);

        report
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

    fn push_win(&mut self, id: WinId, tab: Tab, rect: egui::Rect) {
        self.windows.push(Win {
            id,
            tabs: vec![tab],
            active: 0,
            rect,
            z: self.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
        self.focused = Some(id);
    }

    /// Spawn a bare terminal window with no placement — the caller decides how
    /// it enters (default placement via `tile_new`, a directional split via
    /// `place_split`, or left floating). Returns the new window's id, or `None`
    /// if the PTY failed to spawn (the caller treats that as a no-op).
    fn spawn_terminal_win(&mut self, shell: Shell, ctx: &egui::Context) -> Option<WinId> {
        let env = self.term_env(self.next);
        let mut s = Session::spawn(shell, self.cwd.as_deref(), &env, ctx.clone()).ok()?;
        let (id, rect) = self.next_slot(egui::vec2(580.0, 380.0));
        s.set_term_id(id); // stable Member id == the FOREMAN_TERMINAL_ID just baked in
        self.push_win(
            id,
            Tab::shell_default(
                format!("{}  ·  #{}", shell.label(), id),
                Content::Terminal(s),
            ),
            rect,
        );
        self.mark_workspace_dirty();
        Some(id)
    }

    /// Spawn a terminal into this manager with default placement: tiles next
    /// to the previously-focused window (`tile_new`), then — when
    /// `Settings::new_windows_float` is on — pops it back out to floating
    /// through the same path `Command::TermFloat` uses, so the float-rect
    /// math lives in one place. Returns the new window's id, or `None` if the
    /// PTY failed to spawn.
    pub fn add_terminal(&mut self, shell: Shell, ctx: &egui::Context) -> Option<WinId> {
        let anchor = self.focused;
        let id = self.spawn_terminal_win(shell, ctx)?;
        self.tile_new(id, anchor);
        if crate::config::live(ctx).new_windows_float {
            self.toggle_float_for(id);
        }
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
                let side = if r.width() >= r.height() {
                    Dir::Right
                } else {
                    Dir::Down
                };
                self.tree.insert_split(a, id, side);
                // Splitting against (or near) the panel must not leave it at
                // insert_split's 50/50.
                self.repin_panel();
            }
            None => {
                // Keep the task-manager panel on its remembered dock edge when
                // present: insert the new window on the opposite side rather
                // than forcing the panel back to the right rail.
                if let Some(pid) = self.panel_id().filter(|p| self.tree.contains(*p)) {
                    self.insert_beside_panel(id, pid);
                } else {
                    self.tree.insert_root(id, Dir::Right);
                }
            }
        }
    }

    /// Add a new project window. It starts as a sandbox containing one terminal.
    /// TODO(status line): show repo / branch on the project titlebar.
    pub fn add_project(&mut self, shell: Shell, cwd: PathBuf, ctx: &egui::Context) -> WinId {
        self.opened.push((cwd.clone(), None));
        let (id, rect) = self.next_slot(egui::vec2(720.0, 480.0));
        let title = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("project {}", id));
        let mut child = WindowManager::new();
        child.tag = Some(format!("p{}", id));
        child.cwd = Some(cwd);
        // Raw spawn, not `add_terminal`: a fresh project's sole terminal must
        // always tile as the root — it's not subject to `new_windows_float`.
        if let Some(tid) = child.spawn_terminal_win(shell, ctx) {
            child.tile_new(tid, None);
        }
        self.push_win(
            id,
            Tab::fixed(title, Content::Project(Box::new(child))),
            rect,
        );
        self.mark_workspace_dirty();
        id
    }

    /// Drain project opens recorded since the last call (most callers: the app,
    /// once per frame, to feed the recents list).
    pub fn take_opened(&mut self) -> Vec<(PathBuf, Option<String>)> {
        std::mem::take(&mut self.opened)
    }

    /// Add a project with the default shell, then type `command` into its fresh
    /// terminal (queued until the shell is ready). Used to launch an agent
    /// inside a normal shell, so quitting the agent drops back to a prompt
    /// rather than closing the pane.
    pub fn add_project_with_command(
        &mut self,
        cwd: PathBuf,
        command: &str,
        ctx: &egui::Context,
    ) -> WinId {
        self.opened.push((cwd.clone(), Some(command.to_string())));
        let (id, rect) = self.next_slot(egui::vec2(720.0, 480.0));
        let title = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("project {}", id));
        let mut child = WindowManager::new();
        child.tag = Some(format!("p{}", id));
        child.cwd = Some(cwd);
        // Raw spawn, not `add_terminal`: a fresh project's sole terminal must
        // always tile as the root — it's not subject to `new_windows_float`.
        if let Some(tid) =
            child.spawn_terminal_win(crate::config::live(ctx).default_shell.to_shell(), ctx)
        {
            child.tile_new(tid, None);
            if let Some(w) = child.windows.iter_mut().find(|w| w.id == tid) {
                if let Some(Content::Terminal(s)) = w.tabs.get_mut(w.active).map(|t| &mut t.content)
                {
                    s.inject_input(command);
                }
            }
        }
        self.push_win(
            id,
            Tab::fixed(title, Content::Project(Box::new(child))),
            rect,
        );
        self.mark_workspace_dirty();
        id
    }

    /// Env injected into every PTY this manager spawns (spec: agent-dispatch).
    fn term_env(&self, term_id: WinId) -> Vec<(String, String)> {
        let mut v = vec![
            ("FOREMAN".to_string(), "1".to_string()),
            ("FOREMAN_TERMINAL_ID".to_string(), format!("t{term_id}")),
            // Advertise our real capabilities so cross-platform TUIs enable 24-bit
            // color (Codex's input box, etc.). foreman renders truecolor.
            ("COLORTERM".to_string(), "truecolor".to_string()),
            ("TERM".to_string(), "xterm-256color".to_string()),
            // Kitty graphics: the narrowest signal that makes agents (Codex
            // pets) pick the kitty protocol. TERM stays truthful — we implement
            // the graphics subset in src/graphics.rs, not all of kitty.
            ("KITTY_WINDOW_ID".to_string(), "1".to_string()),
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
                            history: Some(lines),
                            ..Default::default()
                        });
                    }
                    // Unlike open's spawn-undo, a post whose reply channel died
                    // STAYS in the log (spec §3: append-only; the room is the
                    // log, not the audience) — only the injection is skipped.
                    // A retrying client may therefore duplicate a history line;
                    // accepted v1.
                    Ok(ChatOutcome::Posted { seq }) => {
                        // Reply-before-inject (spec §3): the ack handle returns
                        // now; chat_delivery_sweep injects the post into each
                        // ready member on a later frame.
                        let ok = OpenReply {
                            ok: true,
                            seq,
                            ..Default::default()
                        };
                        let _ = reply.send(ok);
                        ctx.request_repaint(); // log changed (viewer + pending delivery)
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
                        history: Some(lines),
                        ..Default::default()
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
                            project: Some(format!("p{pid}")),
                            ..Default::default()
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
            CtrlMsg::Send(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                match self.send_dispatch(&req) {
                    Err(e) => {
                        let _ = reply.send(OpenReply::err(e));
                    }
                    Ok((pid, tid)) => {
                        let settle = req
                            .settle_ms
                            .unwrap_or(crate::config::live(ctx).send_settle_ms);
                        if settle == 0 {
                            // Fire-and-forget: reply immediately, no settle wait.
                            let _ = reply.send(OpenReply {
                                ok: true,
                                ..Default::default()
                            });
                        } else {
                            // Defer the reply: park a PendingSettle that
                            // advance_settles drains once the terminal quiets
                            // (cross-frame so the GUI never blocks).
                            let now = std::time::Instant::now();
                            let quiet_window =
                                std::time::Duration::from_millis(settle.min(MAX_SETTLE_MS));
                            let cur_gen = self.session_gen(pid, tid).unwrap_or(0);
                            self.pending_settles.push(PendingSettle {
                                pid,
                                tid,
                                reply,
                                last_gen: cur_gen,
                                quiet_since: now,
                                deadline: now + std::time::Duration::from_millis(MAX_SETTLE_MS),
                                quiet_window,
                            });
                        }
                        ctx.request_repaint();
                    }
                }
            }
            CtrlMsg::Snapshot(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                let _ = reply.send(match self.snapshot_dispatch(&req) {
                    Ok((lines, cells, cursor)) => OpenReply {
                        ok: true,
                        history: Some(lines),
                        cells,
                        cursor,
                        ..Default::default()
                    },
                    Err(e) => OpenReply::err(e),
                });
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
                ..Default::default()
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
        let child = self.project_child_mut(pid)?;
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
        let child = self.project_child_mut(pid)?;
        match (&req.text, req.history) {
            (None, Some(n)) => Ok(ChatOutcome::History(child.chat_history(n))),
            (Some(text), None) => {
                // history reads are anonymous; a post must name its sender
                let from = req
                    .from
                    .as_deref()
                    .ok_or("posting requires a sender (FOREMAN_TERMINAL_ID)")?;
                let seq = child.chat_post(from, text, &req.to, req.re)?;
                Ok(ChatOutcome::Posted { seq: Some(seq) })
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
            // Clone the room Rc before the terminal loop so membership reads
            // don't borrow `child` a second time while it is borrowed mutably.
            let room = Rc::clone(&child.chat);
            for win in child.windows.iter_mut() {
                for tab in win.tabs.iter_mut() {
                    let Content::Terminal(s) = &mut tab.content else {
                        continue;
                    };
                    let tag = term_tag(s.term_id());
                    let state = match s.exited() {
                        Some(code) => format!("exited({code})"),
                        None => "running".into(),
                    };
                    let member = if room.borrow().is_member(&tag) {
                        "chat"
                    } else {
                        "-"
                    };
                    lines.push(format!(
                        "  {}  {}  {}  {}",
                        tag,
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
        let child = self.project_child(pid)?;
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

    /// Resolve a `(project, terminal)` pair to their `WinId`s.
    /// `project` uses the existing `resolve_project` logic; `terminal` is
    /// validated to exist in that project's child manager and to have at
    /// least one `Content::Terminal` tab.
    fn resolve_terminal(
        &self,
        project: Option<&str>,
        terminal: &str,
    ) -> Result<(WinId, WinId), String> {
        let pid = self.resolve_project(project)?;
        let tid = term_id(terminal)?;
        let child = self.project_child(pid)?;
        let tw = child
            .windows
            .iter()
            .find(|w| w.id == tid)
            .ok_or_else(|| format!("no such terminal: {terminal}"))?;
        if !tw
            .tabs
            .iter()
            .any(|t| matches!(t.content, Content::Terminal(_)))
        {
            return Err(format!("not a terminal: {terminal}"));
        }
        Ok((pid, tid))
    }

    /// The desktop→project hop shared by the control executors: the child
    /// `WindowManager` inside project `pid`'s ACTIVE tab. Callers pass a
    /// freshly `resolve_project`-ed pid, so a missing window is a logic
    /// error (`expect`) and a non-project active tab is unreachable.
    fn project_child(&self, pid: WinId) -> Result<&WindowManager, String> {
        let win = self.windows.iter().find(|w| w.id == pid).expect("resolved");
        match &win.tabs[win.active].content {
            Content::Project(child) => Ok(child),
            _ => Err("not a project".into()), // unreachable after resolve
        }
    }

    /// Mutable sibling of [`Self::project_child`].
    fn project_child_mut(&mut self, pid: WinId) -> Result<&mut WindowManager, String> {
        let win = self
            .windows
            .iter_mut()
            .find(|w| w.id == pid)
            .expect("resolved");
        match &mut win.tabs[win.active].content {
            Content::Project(child) => Ok(child),
            _ => Err("not a project".into()), // unreachable after resolve
        }
    }

    /// Get a mutable reference to the `Session` for the given (pid, tid).
    /// Tab choice is `terminal_tab_idx` (active-tab-preferred). Uses
    /// immutable checks first to find the tab index, then takes a single
    /// mutable borrow — satisfying the borrow checker without unsafe.
    fn session_mut(
        &mut self,
        pid: WinId,
        tid: WinId,
    ) -> Result<&mut crate::terminal::Session, String> {
        // Immutable pass: find which tab index holds a terminal.
        let tab_idx = {
            let child = self.project_child(pid)?;
            let tw = child
                .windows
                .iter()
                .find(|w| w.id == tid)
                .ok_or_else(|| format!("no such terminal: t{tid}"))?;
            terminal_tab_idx(tw).ok_or_else(|| format!("no terminal tab in t{tid}"))?
        };
        // Mutable pass: take the borrow with the known index.
        let child = self.project_child_mut(pid)?;
        let tw = child
            .windows
            .iter_mut()
            .find(|w| w.id == tid)
            .ok_or_else(|| format!("no such terminal: t{tid}"))?;
        let Content::Terminal(s) = &mut tw.tabs[tab_idx].content else {
            return Err(format!("tab {tab_idx} is not a terminal"));
        };
        Ok(s)
    }

    /// Read-only `output_gen` of the terminal at (pid, tid). Tab choice is
    /// `terminal_tab_idx`, same as `session_mut` — but this walk takes
    /// `&self` and degrades to `None` instead of erroring: the settle
    /// machinery polls freshness while iterating the pending list, and the
    /// project/terminal may have closed since the request was parked (so no
    /// `project_child`, which expects a live pid).
    fn session_gen(&self, pid: WinId, tid: WinId) -> Option<u64> {
        let win = self.windows.iter().find(|w| w.id == pid)?;
        let Content::Project(child) = &win.tabs[win.active].content else {
            return None;
        };
        let tw = child.windows.iter().find(|w| w.id == tid)?;
        let idx = terminal_tab_idx(tw)?;
        let Content::Terminal(s) = &tw.tabs[idx].content else {
            return None;
        };
        Some(s.output_gen())
    }

    /// Drive every pending settle one tick. Called each frame after `show()`
    /// (so sessions have already pumped this frame). For each entry: if the
    /// terminal is gone, reply ok and drop; otherwise `settle_tick` decides
    /// whether the silence window elapsed (or the deadline passed) — when done,
    /// reply ok and drop, else keep with the updated silence state.
    pub fn advance_settles(&mut self, now: std::time::Instant) {
        if self.pending_settles.is_empty() {
            return;
        }
        use crate::control::OpenReply;
        let ok_reply = || OpenReply {
            ok: true,
            ..Default::default()
        };
        // mem::take so we can call &self methods (session_gen) while mutating
        // the list — the established borrow pattern in this file.
        let mut settles = std::mem::take(&mut self.pending_settles);
        settles.retain_mut(|ps| {
            let current_gen = match self.session_gen(ps.pid, ps.tid) {
                None => {
                    let _ = ps.reply.send(ok_reply());
                    return false;
                }
                Some(g) => g,
            };
            let (new_gen, new_qs, done) = settle_tick(
                ps.last_gen,
                ps.quiet_since,
                ps.deadline,
                ps.quiet_window,
                current_gen,
                now,
            );
            ps.last_gen = new_gen;
            ps.quiet_since = new_qs;
            if done {
                let _ = ps.reply.send(ok_reply());
                false
            } else {
                true
            }
        });
        self.pending_settles = settles;
    }

    fn send_dispatch(
        &mut self,
        req: &crate::control::SendRequest,
    ) -> Result<(WinId, WinId), String> {
        let terminal = req.terminal.as_deref().ok_or("send: missing terminal")?;
        let (pid, tid) = self.resolve_terminal(req.project.as_deref(), terminal)?;
        // Read the term mode BEFORE mutably borrowing the session for feed.
        let mode = {
            let child = self.project_child(pid)?;
            let tw = child
                .windows
                .iter()
                .find(|w| w.id == tid)
                .expect("resolved");
            let idx = terminal_tab_idx(tw).ok_or_else(|| format!("no terminal tab in t{tid}"))?;
            let Content::Terminal(s) = &tw.tabs[idx].content else {
                return Err("not a terminal tab".into());
            };
            s.term_mode()
        };
        // Same encoder as the live keyboard.
        let key_bytes = crate::inspect::parse_keys(&req.keys, mode)?;
        let session = self.session_mut(pid, tid)?;
        if let Some(text) = &req.text {
            session.feed_text(text);
        }
        if !key_bytes.is_empty() {
            session.feed(&key_bytes);
        }
        Ok((pid, tid))
    }

    #[allow(clippy::type_complexity)]
    fn snapshot_dispatch(
        &mut self,
        req: &crate::control::SnapshotRequest,
    ) -> Result<
        (
            Vec<String>,
            Option<Vec<Vec<crate::inspect::CellData>>>,
            Option<crate::inspect::CursorInfo>,
        ),
        String,
    > {
        let terminal = req
            .terminal
            .as_deref()
            .ok_or("snapshot: missing terminal")?;
        let (pid, tid) = self.resolve_terminal(req.project.as_deref(), terminal)?;
        let session = self.session_mut(pid, tid)?;
        // One pump for the whole reply — chaining snapshot_text/cells/cursor_info
        // would pump per field and can stitch gens under active output.
        Ok(session.snapshot_all(req.attrs, req.cursor))
    }

    /// Close terminal `tid` inside project `pid` (the dispatch undo path).
    fn close_terminal(&mut self, pid: WinId, tid: WinId) {
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == pid) {
            if let Content::Project(child) = &mut win.tabs[win.active].content {
                child.close(tid);
                child.mark_workspace_dirty();
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
        s.set_term_id(id); // stable Member id == the FOREMAN_TERMINAL_ID just baked in
        let title = title
            .map(str::to_string)
            .unwrap_or_else(|| format!("agent · {}", argv[0]));
        // push_win focuses the new window; a dispatched agent must never yank
        // the keyboard out from under the user mid-keystroke (fire-and-watch:
        // the new terminal is to LOOK at, not type into). Keep focus where it
        // was; the window still spawns on top visually (z from next_slot).
        let prev_focus = self.focused;
        // Explicit dispatch title — never auto-renamed (already intentional).
        self.push_win(id, Tab::fixed(title, Content::Terminal(s)), rect);
        self.tile_new(id, prev_focus);
        // Dispatched agents auto-join the project chat room (spec §2) — the
        // room appends the Joined line; the transcript records it.
        if let Some(w) = self.windows.iter().find(|w| w.id == id) {
            // (`title` was moved into push_win — read it back off the window)
            self.chat
                .borrow_mut()
                .join(&term_tag(id), display_name(w.title()));
        } else {
            debug_assert!(false, "just-pushed window {id} missing");
        }
        self.focused = prev_focus;
        self.mark_workspace_dirty();
        Ok(id)
    }

    /// Open (or focus) this project's chat viewer — singleton per project
    /// (spec §4). Closing it later doesn't touch the log; the room is the log.
    fn open_chat_window(&mut self) {
        if let Some((win, tab)) = self.windows.iter().find_map(|w| {
            w.tabs
                .iter()
                .position(|t| matches!(t.content, Content::Chat(_)))
                .map(|i| (w.id, i))
        }) {
            self.surface_target(crate::panel::TargetPath {
                project: win,
                ptab: None,
                window: None,
                tab: Some(tab),
            });
            return;
        }
        let (id, rect) = self.next_slot(egui::vec2(420.0, 320.0));
        self.push_win(
            id,
            Tab::fixed(
                "chat",
                Content::Chat(crate::chat::ChatView::new(Rc::clone(&self.chat))),
            ),
            rect,
        );
        self.mark_workspace_dirty();
    }

    /// Apply crew-board clicks recorded during the draw (content cannot
    /// mutate sibling windows mid-loop). The recorded value is a member id
    /// (`tN`); re-resolve the live terminal holding it and focus its window +
    /// tab. Stale targets (closed windows, merged-away tabs, the human seat)
    /// are dropped silently — same staleness family as terminal-id resolution.
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
        let Some(id) = req else { return };
        // Find the window + active-relative tab whose terminal's Member id matches.
        let mut hit = None;
        for w in &self.windows {
            for (i, t) in w.tabs.iter().enumerate() {
                if let Content::Terminal(s) = &t.content
                    && term_tag(s.term_id()) == id
                {
                    hit = Some((w.id, i));
                    break;
                }
            }
            if hit.is_some() {
                break;
            }
        }
        if let Some((win, tab)) = hit {
            // Local-level surface: this manager owns both viewer and terminal.
            self.surface_target(crate::panel::TargetPath {
                project: win,
                ptab: None,
                window: None,
                tab: Some(tab),
            });
        }
        // else: no live terminal for that id (human seat, or closed) — no-op.
    }

    /// Make the addressed target visible and focused (write seam): restore the
    /// window (re-tiling it when it was minimized out of the tree), restore a
    /// nested child if addressed, switch its active tab, and run the focus
    /// cascade. Stale ids no-op silently.
    ///
    /// Dual shape: with `window: None`, `project` is a window in *this* manager
    /// (crew board / project row). With `window: Some`, `project` is a desktop
    /// window whose nested project contains that child.
    pub fn surface_target(&mut self, path: crate::panel::TargetPath) {
        let Some(pidx) = self.windows.iter().position(|w| w.id == path.project) else {
            return;
        };
        self.unminimize(path.project);
        // Zoom is render-order only, so a zoomed *other* window keeps painting
        // full-area over the target we are about to focus — surfacing it must
        // drop that zoom or focus lands on a window the user cannot see. A click
        // on the zoomed window itself keeps its zoom.
        if self.zoomed.is_some_and(|z| z != path.project) {
            self.unzoom();
        }

        match path.window {
            None => {
                if let Some(t) = path.tab {
                    if t < self.windows[pidx].tabs.len() {
                        self.windows[pidx].active = t;
                    }
                }
            }
            Some(wid) => {
                let Some(pi) = self.owning_project_tab(pidx, wid, path.ptab) else {
                    return;
                };
                self.windows[pidx].active = pi;
                if let Content::Project(inner) = &mut self.windows[pidx].tabs[pi].content {
                    if inner.zoomed.is_some_and(|z| z != wid) {
                        inner.unzoom();
                    }
                    inner.unminimize(wid);
                    if let Some(cw) = inner.windows.iter_mut().find(|w| w.id == wid) {
                        if let Some(t) = path.tab {
                            if t < cw.tabs.len() {
                                cw.active = t;
                            }
                        }
                    }
                    inner.focus(wid);
                }
            }
        }
        self.focus(path.project);
    }

    /// Panel-row click policy (taskbar-style): if the path is already the
    /// focused, *visible* target, minimize it; otherwise surface/focus it.
    ///
    /// "Visible" is evaluated after the un-zoom rule: a focused window covered
    /// by a zoomed sibling is not considered already-surfaced, so the click
    /// clears the overlay instead of minimizing a window the user never saw.
    fn toggle_surface_target(&mut self, path: crate::panel::TargetPath) {
        if self.is_already_surfaced(path) {
            self.apply_min_path(path);
        } else {
            self.surface_target(path);
        }
    }

    /// True when `path` is the current focus cascade leaf and not covered by a
    /// zoomed sibling (or minimized). Used only by the panel click toggle.
    fn is_already_surfaced(&self, path: crate::panel::TargetPath) -> bool {
        let Some(pidx) = self.windows.iter().position(|w| w.id == path.project) else {
            return false;
        };
        let w = &self.windows[pidx];
        if w.minimized || self.focused != Some(path.project) {
            return false;
        }
        // Covered by a zoomed sibling at this level → not visible.
        if self.zoomed.is_some_and(|z| z != path.project) {
            return false;
        }
        match path.window {
            None => match path.tab {
                Some(t) => w.active == t,
                None => true,
            },
            Some(wid) => {
                let Some(pi) = self.owning_project_tab(pidx, wid, path.ptab) else {
                    return false;
                };
                if w.active != pi {
                    return false;
                }
                let Content::Project(inner) = &w.tabs[pi].content else {
                    return false;
                };
                if inner.focused != Some(wid) {
                    return false;
                }
                // Covered by a zoomed sibling inside the project → not visible.
                if inner.zoomed.is_some_and(|z| z != wid) {
                    return false;
                }
                let Some(cw) = inner.windows.iter().find(|c| c.id == wid) else {
                    return false;
                };
                if cw.minimized {
                    return false;
                }
                match path.tab {
                    Some(t) => cw.active == t,
                    None => true,
                }
            }
        }
    }

    /// Pure snapshot of the whole tree for the task-manager panel (read seam).
    /// One `ProjectEntry` per `Content::Project` tab; the panel window itself is
    /// skipped. Cheap; rebuilt each frame by the desktop `show`.
    pub fn panel_model(&self) -> crate::panel::PanelModel {
        use crate::panel::*;
        let mut projects = Vec::new();
        for w in &self.windows {
            if w.is_panel() {
                continue;
            }
            for (pi, pt) in w.tabs.iter().enumerate() {
                let Content::Project(inner) = &pt.content else {
                    continue;
                };
                let ppath = TargetPath {
                    project: w.id,
                    ptab: None,
                    window: None,
                    tab: Some(pi),
                };
                let pfocused = self.focused == Some(w.id) && w.active == pi;
                let mut tabs = Vec::new();
                for cw in &inner.windows {
                    for (ti, t) in cw.tabs.iter().enumerate() {
                        let kind = match &t.content {
                            Content::Terminal(s) => RowKind::Terminal(s.icon_kind()),
                            Content::Chat(_) => RowKind::Chat,
                            // Nested project content is not a product path today; tests
                            // use empty Project stubs as PTY-free tab stand-ins.
                            Content::Project(_) => {
                                RowKind::Terminal(crate::icons::IconKind::Folder)
                            }
                            Content::TaskManager(_) => continue,
                        };
                        tabs.push(TabEntry {
                            path: TargetPath {
                                project: w.id,
                                ptab: Some(pi),
                                window: Some(cw.id),
                                tab: Some(ti),
                            },
                            title: t.title.clone(),
                            kind,
                            minimized: cw.minimized,
                            active_tab: cw.active == ti,
                            focused: pfocused && inner.focused == Some(cw.id) && cw.active == ti,
                            exited: match &t.content {
                                Content::Terminal(s) => s.has_exited(),
                                _ => false,
                            },
                            bell: t.content.bell_active(),
                        });
                    }
                }
                projects.push(ProjectEntry {
                    path: ppath,
                    title: pt.title.clone(),
                    minimized: w.minimized,
                    focused: pfocused,
                    bell: tabs.iter().any(|t| t.bell),
                    tabs,
                });
            }
        }
        PanelModel {
            projects,
            update: self.update_chip.clone(),
        }
    }

    /// Version string to show as the panel's update chip (None = hidden).
    pub fn set_update_chip(&mut self, v: Option<String>) {
        self.update_chip = v;
    }

    /// Latched when the user clicks the chip; App drains it each frame.
    pub fn take_update_click(&mut self) -> bool {
        std::mem::take(&mut self.update_clicked)
    }

    /// Desktop-only, idempotent: create the task-manager panel as a docked
    /// root split if none exists. `dock` is the edge the panel occupies
    /// (`Right` default; `Down` for the bottom strip).
    pub fn ensure_panel(&mut self, collapsed: bool, expanded_width: f32, dock: Dir) {
        if self.windows.iter().any(|w| w.is_panel()) {
            return;
        }
        let id = self.next;
        self.next += 1;
        self.z += 1;
        let prev_focus = self.focused;
        self.windows.push(Win {
            id,
            tabs: vec![Tab::fixed(
                "Sessions",
                Content::TaskManager(crate::panel::PanelView::with_dock(
                    collapsed,
                    expanded_width,
                    dock,
                )),
            )],
            active: 0,
            rect: egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(crate::panel::PANEL_W, 400.0),
            ),
            z: self.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
        // Don't steal focus from an existing project.
        self.focused = prev_focus;
        self.tree.insert_root(id, dock);
        if let Some(area_w) = Some(self.last_area.x).filter(|&w| w > 1.0) {
            self.apply_panel_ratio(area_w);
        }
    }

    fn panel_id(&self) -> Option<WinId> {
        self.windows.iter().find(|w| w.is_panel()).map(|w| w.id)
    }

    /// Live collapse / expanded extent / dock prefs for settings persistence.
    pub fn panel_prefs(&self) -> Option<(bool, f32, Dir)> {
        for w in &self.windows {
            for t in &w.tabs {
                if let Content::TaskManager(v) = &t.content {
                    return Some((v.collapsed, v.expanded_width, v.dock));
                }
            }
        }
        None
    }

    fn panel_dock(&self) -> Dir {
        self.panel_prefs().map(|(_, _, d)| d).unwrap_or(Dir::Right)
    }

    /// Insert `id` on the opposite side of the panel's remembered dock edge so
    /// the panel stays put (bottom stays bottom, right stays right).
    ///
    /// `insert_split` always starts at 50/50 — re-pin the panel to its
    /// remembered rail/expanded extent so minimize-all → restore (or
    /// `tile_new` against a sole panel) does not randomly resize the panel.
    fn insert_beside_panel(&mut self, id: WinId, pid: WinId) {
        let side = self.panel_dock().opposite();
        if !self.tree.insert_split(pid, id, side) {
            self.tree.insert_root(id, side);
        }
        self.repin_panel();
    }

    /// After any tree structure change, refresh the panel's dock edge from
    /// dividers and re-apply its remembered extent. Tree inserts always start
    /// 50/50; without this, moving the Sessions panel (or splitting against
    /// it) would randomly resize it.
    fn repin_panel(&mut self) {
        if !self.desktop || self.panel_id().is_none() {
            return;
        }
        self.sync_panel_dock_from_layout();
        let area_w = self.last_area.x;
        if area_w > 1.0 {
            self.apply_panel_ratio(area_w);
        }
    }

    fn toggle_panel(&mut self) {
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::TaskManager(v) = &mut t.content {
                    v.collapsed = !v.collapsed;
                }
            }
        }
        let area_w = self.last_area.x;
        if area_w > 1.0 {
            self.apply_panel_ratio(area_w);
        }
    }

    /// Size the panel leaf: rail extent when collapsed, else expanded_width
    /// ("expanded extent along the dock axis"). The constrained axis is
    /// whichever one `set_leaf_extent` can actually pin: try H first (right/
    /// left dock — today's behavior), fall back to V (bottom/top dock). A
    /// panel with dividers on both axes stays width-pinned. Extent is capped
    /// by [`crate::panel::max_expanded`] so a bottom strip can't eat half the
    /// landing.
    fn apply_panel_ratio(&mut self, area_w: f32) {
        let Some(pid) = self.panel_id() else {
            return;
        };
        let (collapsed, expanded_width, dock) =
            self.panel_prefs()
                .unwrap_or((false, crate::panel::PANEL_W, Dir::Right));
        let axis_len = match dock {
            Dir::Left | Dir::Right => area_w,
            Dir::Up | Dir::Down => self.last_area.y.max(1.0),
        };
        let max = crate::panel::max_expanded(dock, axis_len);
        // Keep stored preference inside the hard cap (e.g. after a dock flip
        // or a stale settings value from before the cap existed).
        if !collapsed && expanded_width > max + 0.5 {
            for win in &mut self.windows {
                for t in &mut win.tabs {
                    if let Content::TaskManager(v) = &mut t.content {
                        v.expanded_width = max;
                    }
                }
            }
        }
        let target_w = if collapsed {
            crate::panel::RAIL_W
        } else {
            expanded_width.min(max)
        }
        .clamp(crate::panel::RAIL_W, max);

        if self.tree.contains(pid) {
            let local = egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(area_w, self.last_area.y.max(1.0)),
            );
            use crate::layout::SplitDir;
            if !self
                .tree
                .set_leaf_extent(pid, SplitDir::H, target_w, local, SNAP_GAP)
            {
                self.tree
                    .set_leaf_extent(pid, SplitDir::V, target_w, local, SNAP_GAP);
            }
        } else if let Some(w) = self.windows.iter_mut().find(|w| w.id == pid) {
            // Floating panel: resize width only.
            let h = w.rect.height();
            w.rect.set_width(target_w);
            w.rect.set_height(h);
        }
    }

    /// Drain panel-row interactions recorded during the draw into deferred Acts.
    fn drain_panel_acts(&mut self, acts: &mut Vec<Act>) {
        let mut click = None;
        let mut hover = None;
        let mut toggle = false;
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::TaskManager(v) = &mut t.content {
                    if let Some(p) = v.click.take() {
                        click = Some(p);
                    }
                    if let Some(h) = v.hover_act.take() {
                        hover = Some(h);
                    }
                    if v.toggle_collapse {
                        v.toggle_collapse = false;
                        toggle = true;
                    }
                    if v.update_click {
                        v.update_click = false;
                        self.update_clicked = true;
                    }
                }
            }
        }
        if let Some(p) = click {
            acts.push(Act::FocusPath(p));
        }
        if let Some((p, b)) = hover {
            acts.push(match b {
                crate::panel::PanelBtn::Min => Act::MinPath(p),
                crate::panel::PanelBtn::Close => Act::ClosePath(p),
            });
        }
        if toggle {
            self.toggle_panel();
        }
    }

    fn apply_min_path(&mut self, p: crate::panel::TargetPath) {
        match p.window {
            None => self.minimize(p.project),
            Some(wid) => {
                let Some(pidx) = self.windows.iter().position(|w| w.id == p.project) else {
                    return;
                };
                // Activate the project tab that owns this child, then minimize.
                if let Some(pi) = self.owning_project_tab(pidx, wid, p.ptab) {
                    self.windows[pidx].active = pi;
                    if let Content::Project(inner) = &mut self.windows[pidx].tabs[pi].content {
                        inner.minimize(wid);
                    }
                }
            }
        }
    }

    fn apply_close_path(&mut self, p: crate::panel::TargetPath) {
        match p.window {
            None => match p.tab {
                Some(t) => self.request_close_tab(p.project, t),
                None => self.request_close_active_tab(p.project),
            },
            Some(wid) => {
                let Some(pidx) = self.windows.iter().position(|w| w.id == p.project) else {
                    return;
                };
                let Some(pi) = self.owning_project_tab(pidx, wid, p.ptab) else {
                    return;
                };
                self.windows[pidx].active = pi;
                // Route through the nested manager's confirm path.
                if let Content::Project(inner) = &mut self.windows[pidx].tabs[pi].content {
                    match p.tab {
                        Some(t) => inner.request_close_tab(wid, t),
                        None => {
                            if let Some(cw) = inner.windows.iter().find(|w| w.id == wid) {
                                let t = cw.active;
                                inner.request_close_tab(wid, t);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Index of the project tab on `self.windows[pidx]` that owns child window
    /// `wid`. Prefers the path's recorded `ptab`: nested managers number child
    /// windows independently (each starts at 1), so when projects are tabbed a
    /// bare child-id scan always resolves to the FIRST project tab. The scan
    /// remains as a fallback for stale paths (tab reordered since the model
    /// snapshot).
    fn owning_project_tab(&self, pidx: usize, wid: WinId, ptab: Option<usize>) -> Option<usize> {
        let owns = |t: &Tab| matches!(&t.content, Content::Project(inner) if inner.windows.iter().any(|cw| cw.id == wid));
        if let Some(pi) = ptab {
            if self.windows[pidx].tabs.get(pi).is_some_and(|t| owns(t)) {
                return Some(pi);
            }
        }
        self.windows[pidx].tabs.iter().position(|t| owns(t))
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
            // Append only; the per-frame chat_tick delivers it to each ready
            // member next frame (catch-up replay closes the spawn-time drop).
            let _ = self.chat_post_human(&text);
        }
    }

    /// Post into this project's chat on behalf of a terminal. `from` is the
    /// sender's `tN` id string (`req.from`, required non-empty). Requires a live
    /// terminal whose stable Member id matches `from` (else errors), then hands
    /// the post to the room — which owns validation (all-or-nothing targets),
    /// join-on-first-post, and append. Returns the posted seq (the sender's ack
    /// handle). The room auto-joins the sender only on a *successful* post, so a
    /// failed post mutates nothing. A new member's name lands as its id and is
    /// refreshed to the live tab title by the next `chat_tick`. All mutation
    /// lives in the frozen [`ChatRoom`]; nothing is injected here (the per-frame
    /// `chat_tick` delivers, spec §3: reply-before-inject).
    fn chat_post(
        &mut self,
        from: &str,
        text: &str,
        to: &[String],
        re: Option<u64>,
    ) -> Result<u64, String> {
        // The sender must be a live terminal (history reads are anonymous, but
        // a post names its origin). Resolve by stable Member id.
        let exists = self.windows.iter().any(|w| {
            w.tabs.iter().any(
                |t| matches!(&t.content, Content::Terminal(s) if term_tag(s.term_id()) == from),
            )
        });
        if !exists {
            return Err(format!("no such terminal: {from}"));
        }
        self.chat.borrow_mut().post(from, text, to, re)
    }

    /// Append a post from the chat pane's input line. The room owns the lenient
    /// human policy: a bad mention demotes to a plain broadcast instead of
    /// erroring (the input line has no error seat). Append only — `chat_tick`
    /// delivers next frame. Returns the new seq, or `None` for blank input.
    fn chat_post_human(&mut self, text: &str) -> Option<u64> {
        self.chat.borrow_mut().post_human(text)
    }

    /// Per-frame presence reconcile + catch-up delivery (chat handshake
    /// contract). Walks the whole manager tree; in each project manager it
    /// gathers the live members (id/name/ready/exited), hands them to the
    /// room's [`ChatRoom::tick`] (which reconciles exits, refreshes names, and
    /// returns the per-member outbox), then injects each delivered line into
    /// its terminal. `tick`'s ready-gating + cursor is the whole DSR point: a
    /// post appended while a member was still answering its startup device-
    /// status query lands on the first frame the member is ready, exactly once.
    /// Call once after the top-level `show()`, so every session pumped this
    /// frame reports its current `ready()` state.
    pub fn chat_tick(&mut self) {
        // First pass: recurse into projects and collect this manager's live
        // members (the `&mut s` for exited()/ready() forces a borrow we drop
        // before touching the room).
        let mut live = Vec::new();
        for w in self.windows.iter_mut() {
            for tab in w.tabs.iter_mut() {
                match &mut tab.content {
                    Content::Project(child) => child.chat_tick(), // each room owns its log
                    Content::Terminal(s) => live.push(crate::chat::LiveMember {
                        id: term_tag(s.term_id()),
                        name: display_name(&tab.title).to_string(),
                        ready: s.ready(),
                        exited: s.exited().is_some(),
                    }),
                    Content::Chat(_) | Content::TaskManager(_) => {} // not members
                }
            }
        }
        // Desktop manager (no tag) hosts only projects; with no members the
        // tick would no-op anyway, but skip the room borrow entirely.
        let project = self.tag.as_deref().unwrap_or("p?").to_string();
        // Reconcile + outbox, borrow dropped before injection.
        let room = self.chat.clone();
        let deliveries = room.borrow_mut().tick(&project, &live);
        // Second pass: inject each delivery into its terminal. No room borrow
        // is held here, and no session borrow spans the room borrow above.
        for d in &deliveries {
            for w in self.windows.iter_mut() {
                let mut hit = false;
                for tab in w.tabs.iter_mut() {
                    if let Content::Terminal(s) = &mut tab.content
                        && term_tag(s.term_id()) == d.id
                    {
                        for line in &d.lines {
                            s.inject_input(line);
                        }
                        hit = true;
                        break;
                    }
                }
                if hit {
                    break;
                }
            }
        }
    }

    /// Last `n` chat lines (the `--history` verb; reading does not join).
    fn chat_history(&self, n: usize) -> Vec<String> {
        self.chat.borrow().history(n)
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

    /// Back-to-front draw order. Floating windows form a strict upper layer:
    /// a float always paints above every tiled window, no matter how recently
    /// the tile was focused — `z` orders windows only *within* each layer.
    /// Minimized windows are skipped. (Zoom is layered on top separately by
    /// `show`.)
    fn draw_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.windows.len())
            .filter(|&i| !self.windows[i].minimized)
            .collect();
        order.sort_by_key(|&i| {
            let w = &self.windows[i];
            (!self.tree.contains(w.id), w.z)
        });
        order
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
                    self.request_close_active_tab(id);
                }
            }
            Command::LastProject => self.toggle_last(),
            Command::NewProject => {
                self.picker = Some(DirPicker::new(self.picker_start()));
            }
            Command::Help => self.show_help = true,
            Command::OpenSettings => self.open_settings(),
            Command::ToggleTaskManager => self.toggle_panel(),

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
                                child.request_close_active_tab(id);
                            }
                        }
                        Command::Rename => child.begin_rename(),
                        Command::NewTerm => {
                            // `add_terminal` handles default placement (and
                            // `new_windows_float`) itself.
                            child.add_terminal(
                                crate::config::live(&ctx).default_shell.to_shell(),
                                &ctx,
                            );
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
        // Keyboard commands bypass `apply_acts`; over-mark is fine (extra saves).
        match cmd {
            Command::Help | Command::OpenSettings | Command::NewProject => {}
            _ => self.mark_workspace_dirty(),
        }
    }

    /// Open the settings menu modal (desktop only). Closes the read-only help
    /// overlay if it was up, so the two modals never stack. The keybindings
    /// editor is reached from within the menu's Keybindings pane.
    fn open_settings(&mut self) {
        self.show_help = false;
        self.menu = Some(SettingsMenu::new());
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
        if self.windows.iter().any(|w| w.id == id && w.is_panel()) {
            return;
        }
        self.detach(id);
        self.windows.retain(|w| w.id != id);
        if self.focused == Some(id) {
            self.focused = self.last_focused.take();
        }
        if self.last_focused == Some(id) {
            self.last_focused = None;
        }
        self.mark_workspace_dirty();
        // End an in-flight rename of this window: the rename editor lives in
        // the (now gone) header, and a dangling `renaming` blocks focus for
        // EVERY window until restart.
        if self.renaming == Some(id) {
            self.renaming = None;
        }
    }

    /// Minimize a window (listed in the task-manager panel). Like `close`, this
    /// ends an in-flight rename of the window — its header (and the rename
    /// editor in it) stops rendering, and a dangling `renaming` blocks focus
    /// for EVERY window.
    fn minimize(&mut self, id: WinId) {
        if self.windows.iter().any(|w| w.id == id && w.is_panel()) {
            return;
        }
        // Capture dock while the tree still has the panel's sibling dividers —
        // after the last project detaches the panel is a sole leaf and the
        // dock edge is no longer observable from the tree.
        self.sync_panel_dock_from_layout();
        let was_tiled = self.tree.contains(id);
        self.detach(id);
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.minimized = true;
            w.min_from_tree = was_tiled;
        }
        if self.focused == Some(id) {
            self.focused = None;
        }
        if self.renaming == Some(id) {
            self.renaming = None;
        }
    }

    /// Clear `minimized` and, when the window was tiled at minimize time,
    /// re-enter the tree at the leaf under its old center (longer-axis split).
    /// Best effort — the tree may have changed shape since; falls back to the
    /// panel-aware root insert `tile_new` uses (honouring the panel's remembered
    /// dock edge). No-op on unknown ids.
    fn unminimize(&mut self, id: WinId) {
        let Some(w) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        let retile = w.minimized && std::mem::take(&mut w.min_from_tree);
        w.minimized = false;
        if !retile || self.tree.contains(id) {
            return;
        }
        let center = w.rect.center();
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area);
        let panel = self.panel_id().filter(|p| self.tree.contains(*p));
        match self.tree.hit_leaf(center, local, SNAP_GAP) {
            Some((leaf, r)) if Some(leaf) != panel => {
                let side = if r.width() >= r.height() {
                    Dir::Right
                } else {
                    Dir::Down
                };
                self.tree.insert_split(leaf, id, side);
                self.repin_panel();
            }
            _ => match panel {
                // Old center hit the panel leaf (or nothing): keep the panel on
                // its remembered dock edge, same as tile_new.
                Some(pid) => self.insert_beside_panel(id, pid),
                None => self.tree.insert_root(id, Dir::Right),
            },
        }
    }

    /// True when this manager has no real windows left and no modal is open (the
    /// picker could still create a project; an open settings editor must not
    /// be yanked out from under the user; a pending close-confirm must hold the
    /// app alive until answered). The task-manager panel alone does not count.
    /// On the desktop this means "closing the last project": `main.rs` quits
    /// the app when it turns true (and landing is off), the way a terminal
    /// emulator exits with its last tab. Minimized projects still count as
    /// "something exists" — use [`Self::should_show_landing`] for the empty
    /// *visible* desktop.
    pub fn deserted(&self) -> bool {
        self.windows.iter().all(|w| w.is_panel())
            && self.picker.is_none()
            && self.keymap_editor.is_none()
            && self.menu.is_none()
            && self.pending_close.is_none()
    }

    /// True when at least one non-panel window is not minimized (a project the
    /// user can see). The task-manager panel never counts.
    pub fn has_visible_project(&self) -> bool {
        self.windows.iter().any(|w| !w.is_panel() && !w.minimized)
    }

    /// Empty *visible* desktop: no non-minimized projects, and no modal that
    /// owns the keyboard. Used to show the landing in the content area while
    /// the Sessions panel stays docked at its remembered size (including when
    /// every project is merely minimized).
    pub fn should_show_landing(&self) -> bool {
        !self.has_visible_project()
            && self.picker.is_none()
            && self.keymap_editor.is_none()
            && self.menu.is_none()
            && self.pending_close.is_none()
    }

    /// Local-space strip for the panel when it is the sole tree leaf (no
    /// sibling to pin against). Honours remembered dock + collapsed/expanded
    /// extent so the panel does not inflate to fill the desktop.
    fn panel_strip_local(&self, asz: egui::Vec2) -> Option<egui::Rect> {
        let (collapsed, width, dock) = self.panel_prefs()?;
        let axis_len = match dock {
            Dir::Left | Dir::Right => asz.x,
            Dir::Up | Dir::Down => asz.y,
        };
        let max = crate::panel::max_expanded(dock, axis_len);
        let extent = if collapsed {
            crate::panel::RAIL_W
        } else {
            width.min(max)
        }
        .clamp(crate::panel::RAIL_W, max);
        let full = egui::Rect::from_min_size(egui::Pos2::ZERO, asz);
        Some(match dock {
            Dir::Right => {
                egui::Rect::from_min_max(egui::pos2(full.max.x - extent, full.min.y), full.max)
            }
            Dir::Left => {
                egui::Rect::from_min_max(full.min, egui::pos2(full.min.x + extent, full.max.y))
            }
            Dir::Down => {
                egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - extent), full.max)
            }
            Dir::Up => {
                egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, full.min.y + extent))
            }
        })
    }

    /// Screen-space content rect beside/above the docked panel strip — where
    /// the landing paints when no project is visible. Falls back to the full
    /// `area` if there is no panel.
    pub fn landing_content_rect(&self, area: egui::Rect) -> egui::Rect {
        let Some(local) = self.panel_strip_local(area.size()) else {
            return area;
        };
        let panel = local.translate(area.min.to_vec2());
        match self.panel_dock() {
            Dir::Right => egui::Rect::from_min_max(area.min, egui::pos2(panel.min.x, area.max.y)),
            Dir::Left => egui::Rect::from_min_max(egui::pos2(panel.max.x, area.min.y), area.max),
            Dir::Down => egui::Rect::from_min_max(area.min, egui::pos2(area.max.x, panel.min.y)),
            Dir::Up => egui::Rect::from_min_max(egui::pos2(area.min.x, panel.max.y), area.max),
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

    /// True if a confirm modal is open in this manager or any nested project.
    /// The desktop calls this over the whole tree; the answer becomes `app_modal`
    /// and is threaded back down so every level knows a dialog is up somewhere.
    fn any_pending_close(&self) -> bool {
        if self.pending_close.is_some() {
            return true;
        }
        self.windows.iter().any(|w| {
            w.tabs
                .iter()
                .any(|t| matches!(&t.content, Content::Project(wm) if wm.any_pending_close()))
        })
    }

    /// True while an overlay that owns the keyboard is up: an existing
    /// close-confirm (app-wide via `app_modal`), the dir picker, the settings
    /// editor, or an in-progress rename. A new close-confirm must not open on top
    /// of one — otherwise two overlays render at once and fight over one keypress.
    fn overlay_blocks_close(&self) -> bool {
        self.app_modal
            || self.pending_close.is_some()
            || self.picker.is_some()
            || self.keymap_editor.is_some()
            || self.menu.is_some()
            || self.renaming.is_some()
    }

    /// Close the active tab of `id`, or open the confirm modal if it has running
    /// subprocesses. No-op if any overlay is already up.
    fn request_close_active_tab(&mut self, id: WinId) {
        if self.overlay_blocks_close() {
            return;
        }
        let Some(w) = self.windows.iter().find(|w| w.id == id) else {
            return;
        };
        let tab = &w.tabs[w.active];
        let is_project = matches!(tab.content, Content::Project(_));
        // Fresh scan at the instant of the request so a child spawned inside the
        // throttle window is seen before we decide whether to warn (or close now).
        crate::proc::refresh_now();
        let groups = groups_in_tab(tab);
        if groups.is_empty() {
            self.close_active_tab(id);
            return;
        }
        self.pending_close = Some(PendingClose {
            target: CloseTarget::ActiveTab(id),
            view: build_confirm(is_project, groups),
        });
    }

    /// Same, for a specific tab index (tab-bar X).
    fn request_close_tab(&mut self, id: WinId, idx: usize) {
        if self.overlay_blocks_close() {
            return;
        }
        let Some(w) = self.windows.iter().find(|w| w.id == id) else {
            return;
        };
        let Some(tab) = w.tabs.get(idx) else {
            return;
        };
        let is_project = matches!(tab.content, Content::Project(_));
        crate::proc::refresh_now();
        let groups = groups_in_tab(tab);
        if groups.is_empty() {
            self.close_tab(id, idx);
            return;
        }
        self.pending_close = Some(PendingClose {
            target: CloseTarget::Tab(id, idx),
            view: build_confirm(is_project, groups),
        });
    }

    /// Apply a modal outcome to the pending close. Pure decision, split from the
    /// egui render so it is unit-tested without a UI context.
    fn resolve_pending(&mut self, outcome: crate::confirm::ConfirmOutcome) {
        let Some(pending) = self.pending_close.take() else {
            return;
        };
        match outcome {
            crate::confirm::ConfirmOutcome::Pending => self.pending_close = Some(pending),
            crate::confirm::ConfirmOutcome::Cancelled => {}
            crate::confirm::ConfirmOutcome::Confirmed => {
                match pending.target {
                    CloseTarget::ActiveTab(id) => self.close_active_tab(id),
                    CloseTarget::Tab(id, idx) => self.close_tab(id, idx),
                    CloseTarget::Quit => self.quit_confirmed = true,
                }
                // Close (and quit-accept) are structural; empty desktop after
                // last-project close should persist as empty on next launch.
                self.mark_workspace_dirty();
            }
        }
    }

    /// (title, root_pid) for every terminal tab in THIS manager whose shell
    /// reported a pid. Pure tree read — the testable surface for grouping.
    fn terminal_shells(&self) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        for w in &self.windows {
            for t in &w.tabs {
                if let Content::Terminal(s) = &t.content {
                    // Skip a shell we've already seen exit: its root_pid can be
                    // recycled by the OS, which would list unrelated processes.
                    if !s.has_exited() {
                        if let Some(pid) = s.root_pid() {
                            out.push((t.title.clone(), pid));
                        }
                    }
                }
            }
        }
        out
    }

    /// One group per terminal tab in THIS manager that has running processes
    /// (label = the tab title, empties skipped). Used for project-close.
    fn terminal_groups(&self) -> Vec<crate::confirm::ProcGroup> {
        self.terminal_shells()
            .into_iter()
            .filter_map(|(label, pid)| {
                let procs = crate::proc::top_children(pid);
                (!procs.is_empty()).then(|| crate::confirm::ProcGroup {
                    label,
                    scope: None,
                    procs,
                })
            })
            .collect()
    }

    /// One group per project tab in THIS (desktop) manager that has running
    /// processes: label = project title, scope = "N terminals", procs = the
    /// project's per-terminal rows flattened. Used by the quit guard. Like
    /// `groups_in_tab`'s project path, this reads the project's *direct* terminals
    /// (nested projects can't occur today — projects are only created at the
    /// desktop).
    fn project_groups(&self) -> Vec<crate::confirm::ProcGroup> {
        let mut out = Vec::new();
        for w in &self.windows {
            for t in &w.tabs {
                if let Content::Project(wm) = &t.content {
                    // One scan drives BOTH the rows and the "N terminals" count, so
                    // the count can never disagree with the list shown.
                    let inner = wm.terminal_groups();
                    if inner.is_empty() {
                        continue;
                    }
                    let n = inner.len();
                    let procs: Vec<crate::proc::ProcInfo> =
                        inner.into_iter().flat_map(|g| g.procs).collect();
                    out.push(crate::confirm::ProcGroup {
                        label: t.title.clone(),
                        scope: Some(format!("{n} terminal{}", if n == 1 { "" } else { "s" })),
                        procs,
                    });
                }
            }
        }
        out
    }

    /// Open the quit confirm if any subprocess is running anywhere; return true
    /// when it did (caller should cancel the OS close). False → nothing running,
    /// let the app quit.
    pub fn begin_quit_confirm(&mut self) -> bool {
        if self.overlay_blocks_close() {
            return true; // an overlay is already up (a confirm, picker, or settings)
        }
        crate::proc::refresh_now();
        let groups = self.project_groups();
        if groups.is_empty() {
            return false;
        }
        let k = groups.len();
        let view = crate::confirm::ConfirmClose::new(
            "quit foreman?",
            running_lead(
                top_count(&groups),
                background_count(&groups),
                Some((k, "project")),
            ),
            "quit anyway",
            groups,
        );
        self.pending_close = Some(PendingClose {
            target: CloseTarget::Quit,
            view,
        });
        true
    }

    /// True once, when the quit confirm was accepted. Resets on read.
    pub fn take_quit_confirmed(&mut self) -> bool {
        std::mem::take(&mut self.quit_confirmed)
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
            min_from_tree: false,
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
            self.unzoom();
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

    /// Drop any zoom, restoring a floating window's pre-zoom rect (the per-frame
    /// re-fit overwrites `rect` while zoomed). No-op when nothing is zoomed, and
    /// unlike `toggle_zoom` it does not move focus.
    fn unzoom(&mut self) {
        let Some(id) = self.zoomed.take() else {
            return;
        };
        if !self.tree.contains(id) {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                if let Some(pr) = w.prev.take() {
                    w.rect = pr;
                }
            }
        }
    }

    /// Move the focused window within the tiled layer. Tiled: swap with the
    /// geometric neighbor leaf in that direction; with no neighbor, re-insert at
    /// the area edge as a full row/column. Floating: enter the tree at that edge.
    fn move_dir(&mut self, d: Dir) {
        let Some(id) = self.focused else { return };
        if self.tree.contains(id) {
            let local = egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area);
            let placements = self.tree.layout(local, SNAP_GAP);
            let Some(from) = placements
                .iter()
                .find(|(w, _)| *w == id)
                .map(|(_, r)| r.center())
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
        // Swap / edge re-insert / float-enter all reshuffle ratios; keep the
        // Sessions panel on its remembered extent (and refresh dock edge).
        self.repin_panel();
        self.focus(id);
    }

    /// Split: create a new terminal next to the focused window in the tree.
    fn split_dir(&mut self, d: Dir, ctx: &egui::Context) {
        let src = self.focused;
        // Raw spawn, not `add_terminal`: an explicit directional split always
        // tiles in that direction — it's not subject to `new_windows_float`.
        let Some(new_id) =
            self.spawn_terminal_win(crate::config::live(ctx).default_shell.to_shell(), ctx)
        else {
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
        self.repin_panel();
        self.focus(new_id);
    }

    /// Toggle the focused window between tiled and floating (leader F / Ctrl+F).
    fn toggle_float(&mut self) {
        if let Some(id) = self.focused {
            self.toggle_float_for(id);
        }
    }

    /// Toggle `id` between tiled and floating. Un-tiling restores the remembered
    /// floating rect; re-tiling enters the tree where the window currently sits
    /// (the leaf under its center, split along its longer axis). Focuses `id`.
    fn toggle_float_for(&mut self, id: WinId) {
        if self.tree.contains(id) {
            self.detach(id);
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.rect = w.prev.take().unwrap_or(egui::Rect::from_min_size(
                    egui::pos2(60.0, 60.0),
                    egui::vec2(580.0, 380.0),
                ));
            }
            // Detaching the panel (or a sibling) leaves a new sole/neighbor
            // geometry — re-pin if the panel remains tiled.
            self.repin_panel();
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
                    let side = if r.width() >= r.height() {
                        Dir::Right
                    } else {
                        Dir::Down
                    };
                    self.tree.insert_split(leaf, id, side);
                }
                None => self.tree.insert_root(id, Dir::Right),
            }
            self.repin_panel();
        }
        self.focus(id);
    }

    /// Move focus to the nearest window in direction `d`, by geometry on local
    /// rects: among windows whose center lies in the requested half-plane, pick
    /// the one minimizing (dominant-axis distance, then cross-axis distance).
    fn focus_dir(&mut self, d: Dir) {
        let Some(cur) = self.focused else {
            // No focus yet: focus the top-most visible window. Draw order is
            // layered (floats above tiles), so "topmost" is its last entry —
            // NOT the max raw `z`, which a buried tile can hold.
            if let Some(id) = self.draw_order().last().map(|&i| self.windows[i].id) {
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
            if w.id == src || w.minimized || w.is_panel() {
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
        for w in &mut self.windows {
            for t in &mut w.tabs {
                match &mut t.content {
                    Content::Terminal(s) => {
                        if let Some(code) = s.exit_to_note() {
                            // The chat Exited line is emitted by chat_tick's
                            // presence reconcile, not here — this only stamps
                            // the one-shot title marker.
                            t.title.push_str(&format!("  ·  exited ({code})"));
                        }
                    }
                    Content::Project(wm) => wm.refresh_exit_titles(),
                    Content::Chat(_) | Content::TaskManager(_) => {} // no process
                }
            }
        }
    }

    /// When a shell tab still carries its default title (`auto_title`) and its
    /// Session now resolves to an agent icon, rename the tab to
    /// `"Claude  ·  #3"` (etc.). Manual renames and dispatch titles opt out.
    /// Agent exit leaves the name (v1). Runs over every tab, including
    /// background ones; recurses into projects like `refresh_exit_titles`.
    fn refresh_auto_titles(&mut self) {
        for w in &mut self.windows {
            for t in &mut w.tabs {
                match &mut t.content {
                    Content::Terminal(s) => {
                        if let Some(new) =
                            auto_agent_title(&t.title, t.auto_title, s.icon_kind(), s.term_id())
                        {
                            t.title = new;
                        }
                    }
                    Content::Project(wm) => wm.refresh_auto_titles(),
                    Content::Chat(_) | Content::TaskManager(_) => {}
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
        // True when a confirm modal is already open in an ancestor manager. The
        // desktop is passed `false` and folds in its own whole-tree scan; the
        // result flows back down so every level sees the app-wide state.
        app_modal: bool,
    ) -> bool {
        // Record the area so keyboard-driven zoom/snap can commit to a sensible
        // rect before the next render refits it.
        let prev_area_w = self.last_area.x;
        self.last_area = area.size();

        // A confirm dialog anywhere in the app is globally modal: freeze this
        // manager's keyboard (leader + terminals, via `is_focus`) so only the
        // dialog reads Enter/Esc, and block a second dialog from opening. The
        // desktop's scan reaches every nested project; the flag threads down.
        self.app_modal = app_modal || self.any_pending_close();
        let live = active && !self.app_modal;

        if self.desktop {
            self.refresh_exit_titles();
            self.refresh_auto_titles();
            // First real area (or large area change): size the panel leaf.
            // While collapsed, re-pin every frame so divider drags (from the
            // panel's edge or a neighbour's) spring back to the rail width.
            let panel_collapsed = self.panel_prefs().is_some_and(|(c, _, _)| c);
            if area.width() > 1.0
                && (panel_collapsed
                    || prev_area_w < 1.0
                    || (area.width() - prev_area_w).abs() > 80.0)
            {
                self.apply_panel_ratio(area.width());
            }
            // Stash the read-model snapshot into the panel before painting.
            let model = self.panel_model();
            for w in &mut self.windows {
                for t in &mut w.tabs {
                    if let Content::TaskManager(v) = &mut t.content {
                        v.model = model.clone();
                    }
                }
            }
        }

        self.pump_commands(ui, live);

        ui.painter_at(area)
            .rect_filled(area, egui::CornerRadius::ZERO, DESK_BG);

        let focused = self.focused;
        let asz = area.size();
        // Minimized windows never enter draw_order(), but their PTYs still
        // need to answer DSR/CPR and drain output. Pump every tab headlessly;
        // Content::keepalive recurses through minimized project windows.
        for w in self.windows.iter_mut().filter(|w| w.minimized) {
            w.keepalive_inactive();
            w.active_content().keepalive();
        }
        let mut order = self.draw_order();

        let mut placements: std::collections::HashMap<WinId, egui::Rect> = self
            .tree
            .layout(egui::Rect::from_min_size(egui::Pos2::ZERO, asz), SNAP_GAP)
            .into_iter()
            .collect();
        // Sole panel leaf would otherwise fill the desktop and blow
        // `expanded_width` via sync. Pin it to the remembered dock strip so
        // size survives minimize-all (and the landing can occupy the rest).
        if self.desktop {
            if let Some(pid) = self.panel_id().filter(|p| self.tree.contains(*p)) {
                let only_panel = self.tree.leaves() == [pid];
                if only_panel {
                    if let Some(strip) = self.panel_strip_local(asz) {
                        placements.insert(pid, strip);
                    }
                }
            }
        }
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

        // While the directory picker/renaming/keymap editor/menu is open, no
        // window is "live" — this stops the focused terminal from consuming
        // keystrokes meant for the overlay, and (below) stops a hover under
        // the overlay from stealing focus either.
        let no_modal = live
            && self.picker.is_none()
            && self.renaming.is_none()
            && self.keymap_editor.is_none()
            && self.menu.is_none();
        // Focus-follows-mouse fires only on genuine pointer movement (not a
        // still cursor under a pane that just re-laid-out) and never while a
        // button is held (title drag, text selection, divider resize).
        let follow_mouse = no_modal
            && crate::config::live(ui.ctx()).focus_follows_mouse
            && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO && !i.pointer.any_down());

        for &i in &order {
            let id = self.windows[i].id;
            let is_focus = focused == Some(id) && no_modal;
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

            // A lone, tiled, single-tab non-project window draws no chrome of its
            // own: the parent frame is its only frame (tmux-style sole pane). Its
            // content fills the whole area — no titlebar, controls, or border — and
            // it can't be dragged or torn out (there is nothing to tear it from).
            // This is what lets a project's tab/header flow straight into a single
            // terminal with no redundant inner frame between them.
            let is_panel = self.windows[i].is_panel();
            let bare = is_tiled
                && self.windows.len() == 1
                && self.windows[i].tabs.len() == 1
                && !is_project
                && !is_panel;
            if bare {
                ui.painter_at(scr.intersect(area))
                    .rect_filled(scr, egui::CornerRadius::ZERO, BG);
                let cresp = ui.interact(
                    scr,
                    base.with((id, "content")),
                    egui::Sense::click_and_drag(),
                );
                if cresp.clicked() {
                    acts.push(Act::Focus(id));
                }
                let child_interacted = self.windows[i].active_content().show(
                    ui,
                    scr,
                    is_focus,
                    base,
                    id,
                    &cresp,
                    self.app_modal,
                );
                if child_interacted {
                    acts.push(Act::Focus(id));
                }
                // Bare sole pane has no border or chips — the Bell falls back
                // to an inset ring on the content rect (the only surface that
                // doesn't invent chrome).
                if crate::terminal::bell_enabled(ui.ctx()) && self.windows[i].bell_active() {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(30));
                    ui.painter_at(scr.intersect(area)).rect_stroke(
                        scr.shrink(1.0),
                        egui::CornerRadius::ZERO,
                        egui::Stroke::new(
                            2.0,
                            bell_pulse(
                                ui.input(|inp| inp.time),
                                crate::config::live(ui.ctx()).bell_period as f64,
                            ),
                        ),
                        egui::StrokeKind::Inside,
                    );
                }
                continue;
            }

            // Right-side control zone width (panel collapse / project ✕+⋯ /
            // four terminal buttons); the drag strip and header_layout's
            // fence both derive from the same policy. Panel used to inherit
            // the terminal's 113px reserve and needed to be quite wide before
            // the title drag strip was usable — collapse is only ~28px.
            let ctl_w = header_ctl_w(is_project, is_panel);

            // Collapsed horizontal (bottom/top-docked) panel: the whole window
            // is a 36px strip. The header band is suppressed — the rail owns
            // the full rect and the expand toggle lives inside it — so the
            // drag strip collapses to nothing too. Orientation is derived
            // per-frame from the rect: wider than tall = horizontal.
            let panel_h_collapsed = is_panel
                && scr.width() > scr.height()
                && matches!(
                    &self.windows[i].tabs[self.windows[i].active].content,
                    Content::TaskManager(v) if v.collapsed
                );

            // --- title drag (interact first, then we know final position) ---
            let drag_rect = egui::Rect::from_min_size(
                scr.min,
                egui::vec2(
                    (scr.width() - ctl_w).max(0.0),
                    if panel_h_collapsed { 0.0 } else { TITLE_H },
                ),
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
                    self.drag_from_tree = Some(id);
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
                self.mark_workspace_dirty();
                scr = self.windows[i].rect.translate(area.min.to_vec2());

                // --- merge target detection: is the pointer over another window? ---
                // Dropping a window's title onto another window tabs it onto that
                // window's stack. While hovering a merge target we suppress the snap
                // overlay and instead highlight the target (handled at paint time).
                // Drop semantics are gated on drag origin: a tear-out (started
                // tiled/zoomed) keeps its hints; a drag that started floating is
                // a pure move unless Shift opts in. Checked live each frame so
                // pressing/releasing Shift mid-drag lights hints up and down.
                let snap_ok =
                    self.drag_from_tree == Some(id) || ui.input(|inp| inp.modifiers.shift);
                let pointer = ui.ctx().pointer_latest_pos();
                let over_target = if snap_ok {
                    pointer.and_then(|p| self.merge_target_at(id, p, area, &order))
                } else {
                    None
                };
                if let Some(tgt) = over_target {
                    merge_hint = Some(tgt);
                } else if snap_ok {
                    if let Some(p) = pointer {
                        // Tree drop hint: leaf edges split, leaf centers tab-merge,
                        // area edge bands split the root. Painted like the old snap overlay.
                        if let Some((_, hint)) = self.tree.drop_target(p, area, SNAP_GAP) {
                            snap_overlay = Some(hint);
                        }
                    }
                }
            }
            if dr.drag_stopped() {
                // Drag origin decides drop rights (Shift overrides for floating
                // drags). take() clears the flag at end-of-gesture either way.
                let snap_ok =
                    self.drag_from_tree.take() == Some(id) || ui.input(|inp| inp.modifiers.shift);
                // Without drop rights the pointer counts as nowhere: no merge, no
                // tree insert — the floating window simply stays where dropped.
                let pointer = ui.ctx().pointer_latest_pos().filter(|_| snap_ok);
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
                                // Panel drop (or drop against the panel) must
                                // keep the remembered Sessions extent, not 50/50.
                                self.repin_panel();
                                self.mark_workspace_dirty();
                            }
                            crate::layout::DropTarget::Root(side) => {
                                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                                    if w.prev.is_none() {
                                        w.prev = Some(w.rect);
                                    }
                                }
                                self.tree.insert_root(id, side);
                                self.repin_panel();
                                self.mark_workspace_dirty();
                            }
                        }
                        // Rect refits from the tree next frame (one frame at the drop
                        // position — invisible at 60fps; intentionally no immediate set).
                    }
                }
            }

            // A collapsed horizontal panel hands its full rect to the rail
            // (no reserved header band); everything else starts below TITLE_H.
            let content_rect = if panel_h_collapsed {
                scr
            } else {
                egui::Rect::from_min_max(
                    egui::pos2(scr.min.x + 1.0, scr.min.y + TITLE_H),
                    egui::pos2(scr.max.x - 1.0, scr.max.y - 1.0),
                )
            };

            // Every non-bare window paints its chrome unconditionally (the
            // bare sole-pane path above is the only chrome-less window): a
            // quiet header on a reserved band, surface-colored so it blends
            // with the content below (user iteration 2026-07-02). The old
            // hover-reveal + fade machinery is gone with it.
            // The band is reserved at BOTH levels now that headers are
            // permanent: content starts below TITLE_H, so the PTY grid gives
            // up a row instead of hiding one under the header.
            let content_paint = content_rect;

            // --- paint window ---
            // Tiled/zoomed windows square their corners so they tile flush to
            // the area edges and to each other (rounded corners would leave gaps).
            let cr = if is_tiled || self.zoomed == Some(id) {
                egui::CornerRadius::ZERO
            } else {
                egui::CornerRadius::same(6)
            };
            let p = ui.painter_at(scr.intersect(area));
            // Every window body paints the terminal surface color: the
            // reserved (unfilled) title bands at both levels must blend into
            // the content below them, or they read as header bars even with
            // no fill.
            p.rect_filled(scr, cr, BG);

            // --- content ---
            // Painted BEFORE the header so the hover-revealed header overlays
            // it. Terminals need click_and_drag (for text selection); projects
            // only sense clicks so drags pass through to their own sub-windows.
            let sense = if is_project {
                egui::Sense::click()
            } else {
                egui::Sense::click_and_drag()
            };
            let cresp = ui.interact(content_rect, base.with((id, "content")), sense);
            if cresp.clicked() {
                acts.push(Act::Focus(id));
            } else if follow_mouse && cresp.hovered() && !is_focus {
                acts.push(Act::Focus(id));
            }
            let child_interacted = self.windows[i].active_content().show(
                ui,
                content_paint,
                is_focus,
                base,
                id,
                &cresp,
                self.app_modal,
            );
            if child_interacted {
                // A sub-window inside this project was clicked: raise this project
                // to focus so the keyboard cascade reaches it. This also makes
                // `acts` non-empty, propagating the interaction further up.
                acts.push(Act::Focus(id));
            }

            {
                // Measure text caller-side (fonts are impure), then let the
                // pure header_layout place every header rect: chips, the
                // clamped `+`, and the control row. The layout variant
                // mirrors the branch below by construction.
                let tab_font = egui::FontId::proportional(11.5);
                let title_font = egui::FontId::proportional(12.5);
                let tab_measures: Vec<TabMeasure>;
                let spec = if is_renaming {
                    HeaderSpec::Rename
                } else if self.windows[i].tabs.len() > 1 {
                    tab_measures = self.windows[i]
                        .tabs
                        .iter()
                        .map(|t| TabMeasure {
                            label_w: ui
                                .painter()
                                .layout_no_wrap(t.title.clone(), tab_font.clone(), TEXT)
                                .size()
                                .x,
                            has_icon: t.content.icon_kind().is_some(),
                        })
                        .collect();
                    HeaderSpec::Tabs(&tab_measures)
                } else {
                    HeaderSpec::Title {
                        title_w: ui
                            .painter()
                            .layout_no_wrap(
                                self.windows[i].title().to_string(),
                                title_font.clone(),
                                TEXT,
                            )
                            .size()
                            .x,
                        has_icon: self.windows[i].tabs[self.windows[i].active]
                            .content
                            .icon_kind()
                            .is_some(),
                    }
                };
                let hl = header_layout(scr, is_project, is_panel, spec);

                if let HeaderContentLayout::Rename { field } = &hl.content {
                    // Field box centered in the titlebar; `vertical_align(Center)` lets
                    // egui center the text within it, so no pixel-fudging is needed.
                    let te_rect = *field;
                    // Theme the field to the dark/amber titlebar instead of egui's
                    // default light TextEdit: dark inset fill + amber edit-mode border.
                    p.rect_filled(te_rect, egui::CornerRadius::same(3), WIN_BG);
                    p.rect_stroke(
                        te_rect,
                        egui::CornerRadius::same(3),
                        egui::Stroke::new(1.0, BORDER_FOCUS),
                        egui::StrokeKind::Inside,
                    );
                    ui.visuals_mut().selection.bg_fill = SELECTION_TEXT_BG;
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
                            // Manual rename permanently opts out of auto-title.
                            self.windows[i].tabs[a].auto_title = false;
                            self.mark_workspace_dirty();
                        }
                        self.renaming = None;
                    }
                } else if let HeaderContentLayout::Tabs { chips } = &hl.content {
                    // --- tab bar (multi-tab stacks only) ---
                    // One pre-packed chip per visible tab: the geometry (chip
                    // metrics, the control-zone fence, truncation) lives in
                    // header_layout. Chips are registered after the window-drag
                    // rect so they win pointer priority; dragging a chip off
                    // the bar detaches it (untab).
                    let bell_gate = crate::terminal::bell_enabled(ui.ctx());
                    let bell_col = bell_pulse(
                        ui.input(|inp| inp.time),
                        crate::config::live(ui.ctx()).bell_period as f64,
                    );
                    for ch in chips {
                        let ti = ch.idx;
                        let chip = ch.rect;
                        let is_active_tab = self.windows[i].active == ti;
                        let label = self.windows[i].tabs[ti].title.clone();
                        let icon = self.windows[i].tabs[ti].content.icon_kind();
                        let chip_resp = ui.interact(
                            chip,
                            base.with((id, "tab", ti)),
                            egui::Sense::click_and_drag(),
                        );
                        // Active tab matches the content directly below it so the chip
                        // reads as joined to that area (classic browser tabs). Both
                        // terminal tabs and project tabs flow into terminal content (a
                        // lone subwindow renders bare, so the project's content top *is*
                        // the terminal). Inactive tabs sit lighter (hover lighter still)
                        // for an obvious active/inactive contrast.
                        let bg = if is_active_tab {
                            BG
                        } else if chip_resp.hovered() {
                            egui::Color32::from_rgb(50, 45, 35)
                        } else {
                            egui::Color32::from_rgb(38, 34, 27)
                        };
                        // Rounded on top, flat on the bottom so the active tab reads as
                        // joined to the content area below it (classic browser tabs).
                        let radius = egui::CornerRadius {
                            nw: 6,
                            ne: 6,
                            sw: 0,
                            se: 0,
                        };
                        p.rect_filled(chip, radius, bg);
                        // Project tabs get the focus-amber selection border (the same
                        // colour as a focused subwindow's border) on three sides only —
                        // the bottom edge is left open so the tab's colour flows straight
                        // into the content below it. Terminal tabs need no border; their
                        // bg-match into the content reads on its own.
                        if is_active_tab && is_project {
                            let tab_border = if is_focus { BORDER_FOCUS } else { BORDER };
                            p.rect_stroke(
                                chip,
                                radius,
                                egui::Stroke::new(BORDER_W, tab_border),
                                egui::StrokeKind::Inside,
                            );
                            // Paint over the bottom edge with the tab bg, leaving an open
                            // bottom so the colour continues into the header beneath.
                            let open = egui::Rect::from_min_max(
                                egui::pos2(chip.min.x, chip.max.y - 1.0),
                                egui::pos2(chip.max.x, chip.max.y),
                            );
                            p.rect_filled(open, egui::CornerRadius::ZERO, bg);
                        }
                        // Bell: only the ringing session's chip pulses (the
                        // whole-stack border pulse is painted at the frame).
                        if bell_gate && self.windows[i].tabs[ti].content.bell_active() {
                            p.rect_stroke(
                                chip,
                                radius,
                                egui::Stroke::new(BORDER_W, bell_col),
                                egui::StrokeKind::Inside,
                            );
                        }
                        let txt_col = if is_active_tab { TEXT } else { DIM };
                        // Leading icon: agent logo / shell glyph / project folder.
                        if let (Some(kind), Some(icon_rect)) = (icon, ch.icon) {
                            let px = (icon_rect.width() * ui.ctx().pixels_per_point())
                                .round()
                                .max(1.0) as u32;
                            let tex = crate::icons::texture(ui.ctx(), kind, px);
                            let tint = if is_active_tab {
                                kind.tint()
                            } else {
                                kind.tint().gamma_multiply(0.55)
                            };
                            p.image(
                                tex.id(),
                                icon_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                tint,
                            );
                        }
                        p.text(
                            ch.label_pos,
                            egui::Align2::LEFT_CENTER,
                            &label,
                            tab_font.clone(),
                            txt_col,
                        );
                        // Close affordance — shown only on the active or hovered tab
                        // (browser-style), and interactable only when shown.
                        let show_x = is_active_tab || chip_resp.hovered();
                        let xr = ch.close;
                        let xresp = if show_x {
                            let r =
                                ui.interact(xr, base.with((id, "tabx", ti)), egui::Sense::click());
                            let xc = xr.center();
                            let xs = 3.0;
                            let xcol = if r.hovered() {
                                egui::Color32::from_rgb(220, 120, 100)
                            } else {
                                txt_col
                            };
                            let xstroke = egui::Stroke::new(1.2, xcol);
                            p.line_segment(
                                [
                                    egui::pos2(xc.x - xs, xc.y - xs),
                                    egui::pos2(xc.x + xs, xc.y + xs),
                                ],
                                xstroke,
                            );
                            p.line_segment(
                                [
                                    egui::pos2(xc.x - xs, xc.y + xs),
                                    egui::pos2(xc.x + xs, xc.y - xs),
                                ],
                                xstroke,
                            );
                            Some(r)
                        } else {
                            None
                        };
                        if xresp.is_some_and(|r| r.clicked()) {
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
                    }
                } else if let HeaderContentLayout::Title { icon, text_pos } = &hl.content {
                    // Leading icon, mirroring the tab chips so a single-window header
                    // reads consistently (agent logo / shell glyph / project folder).
                    if let (Some(kind), Some(icon_rect)) = (
                        self.windows[i].tabs[self.windows[i].active]
                            .content
                            .icon_kind(),
                        *icon,
                    ) {
                        let px = (icon_rect.width() * ui.ctx().pixels_per_point())
                            .round()
                            .max(1.0) as u32;
                        let tex = crate::icons::texture(ui.ctx(), kind, px);
                        let tint = if is_focus {
                            kind.tint()
                        } else {
                            kind.tint().gamma_multiply(0.55)
                        };
                        p.image(
                            tex.id(),
                            icon_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            tint,
                        );
                    }
                    // Collapsed panel rail: 36px can't fit a label — a clipped
                    // "Sessions" reads as garbage, so the header shows only
                    // the expand toggle.
                    let collapsed_panel = matches!(
                        &self.windows[i].tabs[self.windows[i].active].content,
                        Content::TaskManager(v) if v.collapsed
                    );
                    if !collapsed_panel {
                        p.text(
                            *text_pos,
                            egui::Align2::LEFT_CENTER,
                            self.windows[i].title(),
                            title_font.clone(),
                            if is_focus { TEXT } else { DIM },
                        );
                    }
                }

                // --- window controls ---
                // Panel: collapse only (non-closable / non-minimizable).
                // Projects: close + overflow (quiet chrome). Terminals: four buttons.
                let mut ovf_rect = egui::Rect::NOTHING;
                if panel_h_collapsed {
                    // No header band at all: the rail (PanelView) owns the
                    // whole strip; its expand toggle lives inside the rail.
                } else if is_panel {
                    let br = egui::Rect::from_center_size(
                        egui::pos2(scr.max.x - 14.0, scr.min.y + TITLE_H * 0.5),
                        egui::vec2(22.0, 22.0),
                    );
                    let resp =
                        ui.interact(br, base.with((id, "panel-collapse")), egui::Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(
                            br,
                            egui::CornerRadius::same(4),
                            egui::Color32::from_rgb(72, 64, 50),
                        );
                    }
                    let collapsed = matches!(
                        &self.windows[i].tabs[self.windows[i].active].content,
                        Content::TaskManager(v) if v.collapsed
                    );
                    // Vertical (right-docked) panel: chevrons point at the
                    // right edge as before. Expanded horizontal: collapse
                    // points at the docked edge — `scr` vs the desktop area
                    // says whether that's the top or the bottom. Up/down are
                    // vector strokes: the default fonts have no U+2303/U+2304.
                    let col = if is_focus { TEXT } else { DIM };
                    if !collapsed && scr.width() > scr.height() {
                        let up = scr.center().y < area.center().y;
                        crate::panel::paint_chevron(ui.painter(), br.center(), up, col);
                    } else {
                        ui.painter().text(
                            br.center(),
                            egui::Align2::CENTER_CENTER,
                            if collapsed { "«" } else { "»" },
                            egui::FontId::proportional(13.0),
                            col,
                        );
                    }
                    if resp.clicked() {
                        let active = self.windows[i].active;
                        if let Content::TaskManager(v) = &mut self.windows[i].tabs[active].content {
                            v.toggle_collapse = true;
                        }
                    }
                } else {
                    for &(role, r) in &hl.controls {
                        if role == CtlRole::Ovf {
                            ovf_rect = r;
                        }
                        let resp =
                            ui.interact(r, base.with((id, role.id_str())), egui::Sense::click());
                        let bg = if resp.hovered() {
                            if role.danger() {
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
                            CtlRole::Min => {
                                p.line_segment(
                                    [egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)],
                                    stroke,
                                );
                            }
                            CtlRole::Max => {
                                p.rect_stroke(
                                    egui::Rect::from_center_size(c, egui::vec2(s * 2.0, s * 2.0)),
                                    egui::CornerRadius::same(1),
                                    stroke,
                                    egui::StrokeKind::Inside,
                                );
                            }
                            CtlRole::Float => {
                                if is_tiled {
                                    // In the tree: 2×2 grid. Click pops it out to floating.
                                    p.rect_stroke(
                                        egui::Rect::from_center_size(
                                            c,
                                            egui::vec2(s * 2.0, s * 2.0),
                                        ),
                                        egui::CornerRadius::same(1),
                                        stroke,
                                        egui::StrokeKind::Inside,
                                    );
                                    p.line_segment(
                                        [egui::pos2(c.x, c.y - s), egui::pos2(c.x, c.y + s)],
                                        stroke,
                                    );
                                    p.line_segment(
                                        [egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)],
                                        stroke,
                                    );
                                } else {
                                    // Floating: two offset squares. Click tiles it
                                    // (enters at the leaf under the window's center).
                                    let q = s * 0.8;
                                    let o = 1.5;
                                    p.rect_stroke(
                                        egui::Rect::from_center_size(
                                            egui::pos2(c.x + o, c.y - o),
                                            egui::vec2(q * 2.0, q * 2.0),
                                        ),
                                        egui::CornerRadius::same(1),
                                        stroke,
                                        egui::StrokeKind::Inside,
                                    );
                                    p.rect_stroke(
                                        egui::Rect::from_center_size(
                                            egui::pos2(c.x - o, c.y + o),
                                            egui::vec2(q * 2.0, q * 2.0),
                                        ),
                                        egui::CornerRadius::same(1),
                                        stroke,
                                        egui::StrokeKind::Inside,
                                    );
                                }
                            }
                            CtlRole::Ovf => {
                                for dx in [-4.0f32, 0.0, 4.0] {
                                    p.circle_filled(
                                        egui::pos2(c.x + dx, c.y),
                                        1.2,
                                        if is_focus { TEXT } else { DIM },
                                    );
                                }
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
                            match role {
                                CtlRole::Close => acts.push(Act::Close(id)),
                                CtlRole::Max => acts.push(Act::Max(id)),
                                CtlRole::Float => acts.push(Act::Float(id)),
                                CtlRole::Ovf => {} // hover opens the menu; click is inert
                                CtlRole::Min => acts.push(Act::Min(id)),
                            }
                        }
                    }
                } // !is_panel

                // --- project header menus (hover-opened) ---
                if is_project {
                    // The + after the name: create/open actions.
                    if let Some(pr) = hl.plus {
                        let presp = ui.interact(pr, base.with((id, "plus")), egui::Sense::click());
                        let pbg = if presp.hovered() {
                            egui::Color32::from_rgb(72, 64, 50)
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        ui.painter()
                            .rect_filled(pr, egui::CornerRadius::same(4), pbg);
                        let c = pr.center();
                        let s = 4.0;
                        let stroke = egui::Stroke::new(1.4, if is_focus { TEXT } else { DIM });
                        let p = ui.painter();
                        p.line_segment(
                            [egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)],
                            stroke,
                        );
                        p.line_segment(
                            [egui::pos2(c.x, c.y - s), egui::pos2(c.x, c.y + s)],
                            stroke,
                        );
                        if let Some(act) = hover_menu(
                            ui,
                            base.with((id, "plusmenu")),
                            pr,
                            area,
                            &[
                                ("New project", Act::OpenProjectPicker),
                                ("New PS terminal", Act::AddTerm(id, Shell::PowerShell)),
                                ("New CMD terminal", Act::AddTerm(id, Shell::Cmd)),
                                ("New SH terminal", Act::AddTerm(id, Shell::Bash)),
                            ],
                            false,
                        ) {
                            acts.push(act);
                        }
                    }
                    // The ⋯ on the right: window controls for the project.
                    let float_label = if is_tiled { "Float" } else { "Tile" };
                    if let Some(act) = hover_menu(
                        ui,
                        base.with((id, "ovfmenu")),
                        ovf_rect,
                        area,
                        &[
                            (float_label, Act::Float(id)),
                            ("Minimize", Act::Min(id)),
                            ("Maximize", Act::Max(id)),
                        ],
                        true,
                    ) {
                        acts.push(act);
                    }
                }
            } // end header chrome

            // --- border + resize ---
            // Bell: while ANY tab in this stack rings, the border breathes caret
            // amber — attention routing that outranks the focus color until the
            // ringing session gains focus. The repaint_after drives the breathe
            // animation past the idle 100ms cadence.
            let bell_on = crate::terminal::bell_enabled(ui.ctx()) && self.windows[i].bell_active();
            let border_col = if bell_on {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(30));
                bell_pulse(
                    ui.input(|inp| inp.time),
                    crate::config::live(ui.ctx()).bell_period as f64,
                )
            } else if is_focus {
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
            // A collapsed panel is pinned to the rail width: no resize
            // affordance (any drag would spring back on the next frame).
            let pinned = self.windows[i]
                .tabs
                .iter()
                .any(|t| matches!(&t.content, Content::TaskManager(v) if v.collapsed));
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
                    let usable = if pinned || self.zoomed == Some(id) {
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
                if pinned || self.zoomed == Some(id) {
                    continue; // zoomed renders full-area; a pinned panel springs back
                }
                if self.tree.contains(id) {
                    // Tiled: each edge maps to the divider it shares with a neighbour
                    // (resize_edge no-ops on outer edges). Corners drive both axes.
                    // The panel's pinned extent lives below MIN_RATIO, so drags
                    // touching its divider use the panel's own pixel floor —
                    // otherwise an expanded panel can grow but never shrink back.
                    let local = egui::Rect::from_min_size(egui::Pos2::ZERO, asz);
                    let soft = self
                        .panel_id()
                        .filter(|p| self.tree.contains(*p))
                        .map(|p| (p, crate::panel::PANEL_MIN_EXPANDED));
                    for (on, edge, delta) in [
                        (hl, Dir::Left, d.x),
                        (hrr, Dir::Right, d.x),
                        (ht, Dir::Up, d.y),
                        (hb, Dir::Down, d.y),
                    ] {
                        if !on {
                            continue;
                        }
                        match soft {
                            Some(s) => {
                                self.tree
                                    .resize_edge_soft_min(id, edge, delta, local, SNAP_GAP, s);
                            }
                            None => {
                                self.tree.resize_edge(id, edge, delta, local, SNAP_GAP);
                            }
                        }
                    }
                } else {
                    resize_floating(&mut self.windows[i].rect, d, hl, hrr, ht, hb, asz);
                }
                self.mark_workspace_dirty();
            }
        }

        self.paint_drag_overlays(ui, area, snap_overlay, merge_hint);
        // Chip taskbar removed — minimized windows restore via the panel.

        let ctx = ui.ctx().clone();
        // Panel drains join the same act list before apply (focus/min/close paths).
        if self.desktop {
            self.drain_panel_acts(&mut acts);
        }
        // Any Act means a window in this manager was interacted with this frame.
        // Captured before the apply loop consumes `acts`, returned at the end so
        // the parent can bubble focus upward through arbitrary nesting depth.
        let interacted = !acts.is_empty();
        self.apply_acts(acts, asz, base, &ctx);
        // After the acts, not before: the same click that recorded a crew-row
        // hit also pushed Act::Focus(chat window) via cresp — draining last is
        // the fixed order that lets the member, not the viewer, end up focused.
        self.drain_chat_clicks();
        self.drain_chat_posts();
        // Remember expanded panel extent + dock edge from the live tree.
        if self.desktop {
            self.sync_panel_width_from_layout();
            self.sync_panel_dock_from_layout();
        }
        self.show_modals(ui, area, &ctx);

        interacted
    }

    /// Re-derive the panel's dock edge from tree dividers while it has a
    /// sibling. No-op when the panel is the sole leaf (no dividers) — the last
    /// known dock is kept so minimize-all → restore does not force Right.
    fn sync_panel_dock_from_layout(&mut self) {
        let Some(pid) = self.panel_id() else {
            return;
        };
        if !self.tree.contains(pid) {
            return;
        }
        let Some(rect) = self.windows.iter().find(|w| w.id == pid).map(|w| w.rect) else {
            return;
        };
        let left = self.tree.has_divider(pid, Dir::Left);
        let right = self.tree.has_divider(pid, Dir::Right);
        let up = self.tree.has_divider(pid, Dir::Up);
        let down = self.tree.has_divider(pid, Dir::Down);
        // Prefer the axis matching the leaf's aspect (wide → top/bottom dock),
        // but fall through so a one-frame rect lag after drop still works.
        let dock = if rect.width() > rect.height() {
            if up && !down {
                Some(Dir::Down)
            } else if down && !up {
                Some(Dir::Up)
            } else if left && !right {
                Some(Dir::Right)
            } else if right && !left {
                Some(Dir::Left)
            } else {
                None
            }
        } else if left && !right {
            Some(Dir::Right)
        } else if right && !left {
            Some(Dir::Left)
        } else if up && !down {
            Some(Dir::Down)
        } else if down && !up {
            Some(Dir::Up)
        } else {
            None
        };
        if let Some(d) = dock {
            for win in &mut self.windows {
                for t in &mut win.tabs {
                    if let Content::TaskManager(v) = &mut t.content {
                        v.dock = d;
                    }
                }
            }
        }
    }

    /// After layout, store the panel's tiled extent along its dock axis into
    /// `expanded_width` when expanded so divider drags persist. A horizontal
    /// (bottom/top-docked) panel pinned only by a V divider persists its
    /// height; anything width-pinnable persists its width, as before. (The
    /// persisted `panel_width` setting means "expanded extent along the dock
    /// axis" — the key is deliberately not renamed.)
    fn sync_panel_width_from_layout(&mut self) {
        let Some(pid) = self.panel_id() else {
            return;
        };
        if !self.tree.contains(pid) {
            return;
        }
        let Some(rect) = self.windows.iter().find(|w| w.id == pid).map(|w| w.rect) else {
            return;
        };
        // Prefer the remembered dock axis: sole-leaf strip layout has no
        // dividers, and a bottom strip is *wide* so the old width>height
        // heuristic would wrongly persist the full desktop width.
        let dock = self.panel_dock();
        let w = match dock {
            Dir::Left | Dir::Right => rect.width(),
            Dir::Up | Dir::Down => rect.height(),
        };
        if w < 1.0 {
            return;
        }
        let axis_len = match dock {
            Dir::Left | Dir::Right => self.last_area.x.max(1.0),
            Dir::Up | Dir::Down => self.last_area.y.max(1.0),
        };
        let max = crate::panel::max_expanded(dock, axis_len);
        for win in &mut self.windows {
            for t in &mut win.tabs {
                if let Content::TaskManager(v) = &mut t.content {
                    if !v.collapsed && (w - v.expanded_width).abs() > 0.5 {
                        v.expanded_width = w.clamp(crate::panel::PANEL_MIN_EXPANDED, max);
                    }
                }
            }
        }
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
            && self.keymap_editor.is_none()
            && self.menu.is_none()
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

    /// Deferred window mutations collected during render, applied after the render
    /// borrow on `self.windows` is released so we never remove/retab a window
    /// mid-loop and invalidate the draw order.
    fn apply_acts(
        &mut self,
        acts: Vec<Act>,
        _asz: egui::Vec2,
        base: egui::Id,
        ctx: &egui::Context,
    ) {
        // A modal overlay (a close-confirm anywhere via `app_modal`, the dir
        // picker, or the settings editor) is modal for the MOUSE too, not just the
        // keyboard: drop every background window act while one is up. Otherwise the
        // user could retarget the doomed tab (switch/merge → confirm kills the
        // wrong one), hide the dialog (minimize → app-wide keyboard freeze with no
        // visible modal), or stack a second overlay on top. These fields still hold
        // the pre-open state the frame a modal OPENS, so that opening act applies.
        if self.app_modal
            || self.picker.is_some()
            || self.keymap_editor.is_some()
            || self.menu.is_some()
        {
            return;
        }
        if acts.is_empty() {
            return;
        }
        for a in acts {
            match a {
                Act::Focus(id) => self.focus(id),
                Act::AddTerm(id, shell) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        if let Content::Project(wm) = w.active_content() {
                            // `add_terminal` handles default placement (and
                            // `new_windows_float`) itself.
                            wm.add_terminal(shell, ctx);
                        }
                    }
                    self.focus(id);
                }
                Act::OpenProjectPicker => {
                    self.picker = Some(DirPicker::new(self.picker_start()));
                }
                // The titlebar close control closes the *active tab* — which closes
                // the whole window only when it was the last tab.
                Act::Close(id) => self.request_close_active_tab(id),
                Act::SetTab(id, idx) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        if idx < w.tabs.len() {
                            w.active = idx;
                        }
                    }
                    self.focus(id);
                }
                Act::CloseTab(id, idx) => self.request_close_tab(id, idx),
                Act::Merge { src, dst } => {
                    let panelish =
                        |id: WinId| self.windows.iter().any(|w| w.id == id && w.is_panel());
                    if panelish(src) || panelish(dst) {
                        // Panel is non-tabbable.
                    } else {
                        self.merge_windows(src, dst);
                    }
                }
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
                Act::Min(id) => self.minimize(id),
                Act::Max(id) => self.toggle_zoom(id),
                Act::Float(id) => self.toggle_float_for(id),
                Act::FocusPath(p) => self.toggle_surface_target(p),
                Act::MinPath(p) => self.apply_min_path(p),
                Act::ClosePath(p) => self.apply_close_path(p),
            }
        }
        // Any applied act can change focus/layout/tabs; over-dirty is intentional.
        self.mark_workspace_dirty();
    }

    /// Desktop-level modal overlays drawn last, on top of everything: the dir
    /// picker, the settings menu, the keybindings editor (stacked over the menu),
    /// and the leader cue / help cheat-sheet.
    fn show_modals(&mut self, ui: &mut egui::Ui, area: egui::Rect, ctx: &egui::Context) {
        if let Some(mut picker) = self.picker.take() {
            match picker.show_modal(ui) {
                // show_modal clamps ↓ on the last row (never emits PassedEnd);
                // keep the Pending arm defensive in case that changes.
                Outcome::Pending | Outcome::PassedEnd => self.picker = Some(picker),
                Outcome::Cancelled => {}
                Outcome::Accepted(path) => {
                    let anchor = self.focused;
                    let nid = self.add_project(
                        crate::config::live(ctx).default_shell.to_shell(),
                        path,
                        ctx,
                    );
                    self.tile_new(nid, anchor);
                }
            }
        }

        // --- settings menu modal (desktop only) ---
        // Suspended while the keybindings editor is stacked on top: that editor
        // owns the keyboard and paints its own dim backdrop over the menu, so
        // drawing the menu here would double-handle this frame's input. The menu
        // edits a clone of the live settings and republishes it through ctx data
        // (config::seed_live) so the App's read-back sees the change and debounces
        // the save — the same channel the font-size zoom publishes through.
        if self.keymap_editor.is_none() {
            if let Some(mut menu) = self.menu.take() {
                let mut live_settings = (*crate::config::live(ui.ctx())).clone();
                match menu.show(ui, &mut live_settings) {
                    MenuOutcome::Close => { /* drop it: closed */ }
                    MenuOutcome::OpenKeybindings => {
                        self.keymap_editor = Some(SettingsView::new());
                        self.menu = Some(menu);
                    }
                    MenuOutcome::Changed => {
                        crate::config::seed_live(ui.ctx(), &live_settings);
                        self.menu = Some(menu);
                    }
                    MenuOutcome::Pending => self.menu = Some(menu),
                }
                self.swallow_input(ui);
            }
        }

        // --- keybindings editor modal (desktop only), stacked over the menu ---
        // The editor reads input itself; afterwards we swallow every keyboard
        // event for the frame so nothing the editor didn't consume can leak to a
        // terminal — the same capture discipline as the picker / help overlay.
        if let Some(mut editor) = self.keymap_editor.take() {
            let outcome = editor.show(ui, &mut self.keymap);
            match outcome {
                SettingsOutcome::Close => { /* drop it: closed */ }
                SettingsOutcome::Changed => {
                    if let Err(e) = self.keymap.save() {
                        editor.set_save_error(e);
                    }
                    self.keymap_editor = Some(editor);
                }
                SettingsOutcome::Pending => self.keymap_editor = Some(editor),
            }
            self.swallow_input(ui);
        }

        // --- close-confirm modal (runs at every level: a terminal close renders
        // over its project's rect, a project/quit close over the desktop). The
        // decision lives in `resolve_pending`; this only renders + routes it.
        if let Some(mut pending) = self.pending_close.take() {
            let outcome = pending.view.show(ui, area);
            self.pending_close = Some(pending);
            self.resolve_pending(outcome);
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
            "tiled: tear out, hints place it \u{2014} floating: free move".into(),
        ));
        rows.push((
            "  Shift+Drag".into(),
            "floating window shows drop hints and snaps into the tree".into(),
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

/// Hover-opened popup menu anchored to a header button. Paints rows in a
/// Foreground `Area` and returns the clicked item's `Act`. Items carry their
/// action with their label so callers can't mispair them (no positional
/// index to keep in sync). Opens when the pointer rides the anchor (no
/// click), closes on item click, Escape, or the pointer leaving the
/// anchor+panel region (4 px slop bridges the gap). The open flag is
/// transient egui memory — per-frame UI state, never model state.
/// `align_right` right-aligns the panel to the anchor's right edge.
fn hover_menu(
    ui: &mut egui::Ui,
    menu_id: egui::Id,
    anchor: egui::Rect,
    area: egui::Rect,
    items: &[(&str, Act)],
    align_right: bool,
) -> Option<Act> {
    let open_id = menu_id.with("open");
    let was_open = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(open_id))
        .unwrap_or(false);
    // Layer-aware open test: an occluded header can't pop a menu through a
    // floating window above it.
    let open = was_open || ui.rect_contains_pointer(anchor);
    if !open {
        return None;
    }
    let font = egui::FontId::proportional(12.0);
    let row_h = 22.0;
    let pad = 10.0;
    let w = items
        .iter()
        .map(|(l, _)| {
            ui.painter()
                .layout_no_wrap((*l).to_owned(), font.clone(), TEXT)
                .size()
                .x
        })
        .fold(0.0f32, f32::max)
        + pad * 2.0;
    let panel_h = row_h * items.len() as f32 + 8.0;
    // Below the anchor; flip above it at the bottom desktop edge.
    let below = anchor.bottom() + 2.0;
    let oy = if below + panel_h > area.max.y {
        (anchor.top() - 2.0 - panel_h).max(area.min.y)
    } else {
        below
    };
    let ox = if align_right {
        (anchor.right() - w).max(area.min.x)
    } else {
        anchor.left().min(area.max.x - w).max(area.min.x)
    };
    let panel = egui::Rect::from_min_size(egui::pos2(ox, oy), egui::vec2(w, panel_h));
    let mut clicked = None;
    egui::Area::new(menu_id)
        .order(egui::Order::Foreground)
        .fixed_pos(panel.min)
        .show(ui.ctx(), |mui| {
            let mp = mui.painter();
            mp.rect_filled(panel, egui::CornerRadius::same(4), TITLE_BG);
            mp.rect_stroke(
                panel,
                egui::CornerRadius::same(4),
                egui::Stroke::new(1.0, BORDER),
                egui::StrokeKind::Inside,
            );
            for (ri, (label, act)) in items.iter().enumerate() {
                let rr = egui::Rect::from_min_size(
                    egui::pos2(panel.min.x, panel.min.y + 4.0 + row_h * ri as f32),
                    egui::vec2(w, row_h),
                );
                let rresp = mui.interact(rr, menu_id.with(("item", ri)), egui::Sense::click());
                if rresp.hovered() {
                    mui.painter().rect_filled(rr, 0.0, TITLE_BG_FOCUS);
                }
                mui.painter().text(
                    egui::pos2(rr.min.x + pad, rr.center().y),
                    egui::Align2::LEFT_CENTER,
                    *label,
                    font.clone(),
                    TEXT,
                );
                if rresp.clicked() {
                    clicked = Some(act.clone());
                }
            }
        });
    // Stay open only while the pointer rides the anchor or panel. Geometric
    // containment is safe here: the panel is topmost Foreground.
    let ptr_near = ui
        .ctx()
        .pointer_latest_pos()
        .is_some_and(|p| anchor.union(panel).expand(4.0).contains(p));
    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let stay = ptr_near && !esc && clicked.is_none();
    ui.ctx().data_mut(|d| d.insert_temp(open_id, stay));
    clicked
}

// --- header layout (pure) --------------------------------------------------
// All rect math for the TITLE_H band: chip packing, the project `+` clamp,
// and the control row. Pure over caller-measured label widths (fonts are
// impure), so the left-content/control-zone fence is unit-tested instead of
// comment-enforced. Pixel-identical to the pre-seam inline math; legacy
// quirks (chip 0 may overflow the fence on absurdly narrow windows; the
// rename field's 40pt floor may underlap controls) are contract-documented,
// not fixed — changing pixels is out of scope for a seam extraction.

/// Width of the reserved right-hand control zone. The one home for the
/// control-reserve policy — the title drag strip and the fence both derive
/// from it.
/// - Panel: collapse chevron only (~28).
/// - Project: ✕+⋯ (54).
/// - Terminal: four buttons (113).
const fn header_ctl_w(is_project: bool, is_panel: bool) -> f32 {
    if is_panel {
        28.0
    } else if is_project {
        54.0
    } else {
        113.0
    }
}

/// Caller-measured tab label (raw galley width — the module applies the
/// 120pt clamp) plus whether the tab has a leading icon.
#[derive(Clone, Copy)]
struct TabMeasure {
    label_w: f32,
    has_icon: bool,
}

/// What occupies the left of the band. Mirrors the paint code's three-way
/// branch one to one; the layout variant always matches the spec variant.
enum HeaderSpec<'a> {
    Rename,
    Tabs(&'a [TabMeasure]),
    Title { title_w: f32, has_icon: bool },
}

#[derive(Clone, Copy, PartialEq)]
enum CtlRole {
    Close,
    Max,
    Min,
    Float,
    Ovf,
}

impl CtlRole {
    fn danger(self) -> bool {
        matches!(self, CtlRole::Close)
    }
    /// Stable egui Id salt — matches the pre-seam string literals so widget
    /// identity survives the refactor.
    fn id_str(self) -> &'static str {
        match self {
            CtlRole::Close => "close",
            CtlRole::Max => "max",
            CtlRole::Min => "min",
            CtlRole::Float => "float",
            CtlRole::Ovf => "ovf",
        }
    }
}

/// One packed tab chip; all rects in `scr`'s space.
struct ChipLayout {
    /// Tab index — packing may stop early, so idx is load-bearing for
    /// Act::SetTab/CloseTab/Untab addressing.
    idx: usize,
    rect: egui::Rect,
    icon: Option<egui::Rect>,
    label_pos: egui::Pos2,
    close: egui::Rect,
}

enum HeaderContentLayout {
    Rename {
        field: egui::Rect,
    },
    Tabs {
        chips: Vec<ChipLayout>,
    },
    Title {
        icon: Option<egui::Rect>,
        text_pos: egui::Pos2,
    },
}

struct HeaderLayout {
    /// The fence: left content (chips past the first, the `+`) never
    /// crosses `avail_end = scr.max.x - header_ctl_w(kind)`. Read by the
    /// contract tests; the paint code never needs it because every output
    /// rect is already clamped against it.
    #[allow(dead_code)]
    avail_end: f32,
    content: HeaderContentLayout,
    /// The project `+`, pre-clamped against the fence. `Some` iff project
    /// and not renaming.
    plus: Option<egui::Rect>,
    /// Right-to-left: [Close, Ovf] (project) / [Close, Max, Min, Float]
    /// (terminal); 22pt buttons at 25pt pitch, rightmost ends at
    /// scr.max.x - 4.
    controls: Vec<(CtlRole, egui::Rect)>,
}

/// Pure rect math over numbers — no Ui, no Context, no fonts. Total:
/// degenerate `scr` yields degenerate rects, never a panic.
fn header_layout(
    scr: egui::Rect,
    is_project: bool,
    is_panel: bool,
    spec: HeaderSpec<'_>,
) -> HeaderLayout {
    let ctl_w = header_ctl_w(is_project, is_panel);
    let avail_end = scr.max.x - ctl_w;

    // (content, unclamped x where the `+` would anchor; None while renaming)
    let (content, plus_x) = match spec {
        HeaderSpec::Rename => {
            let te_h = TITLE_H - 8.0;
            let field = egui::Rect::from_min_size(
                egui::pos2(scr.min.x + 8.0, scr.min.y + (TITLE_H - te_h) * 0.5),
                egui::vec2((scr.width() - ctl_w - 14.0).max(40.0), te_h),
            );
            (HeaderContentLayout::Rename { field }, None)
        }
        HeaderSpec::Tabs(tabs) => {
            let chip_h = TITLE_H - 4.0;
            let cy = scr.min.y + 4.0;
            let mut cx = scr.min.x + 6.0;
            let icon_disp = 14.0;
            let icon_gap = 6.0;
            let left_pad = 8.0;
            let close_w = 16.0;
            let mut chips = Vec::new();
            for (idx, t) in tabs.iter().enumerate() {
                let tw = t.label_w.min(120.0);
                let icon_w = if t.has_icon {
                    icon_disp + icon_gap
                } else {
                    0.0
                };
                let chip_w = left_pad + icon_w + tw + 6.0 + close_w;
                if cx + chip_w > avail_end && idx > 0 {
                    // Out of room: stop packing (many tabs on a narrow
                    // window); cycling still reaches the hidden tabs.
                    break;
                }
                let rect =
                    egui::Rect::from_min_size(egui::pos2(cx, cy), egui::vec2(chip_w, chip_h));
                let icon = t.has_icon.then(|| {
                    egui::Rect::from_center_size(
                        egui::pos2(cx + left_pad + icon_disp / 2.0, cy + chip_h / 2.0),
                        egui::vec2(icon_disp, icon_disp),
                    )
                });
                let label_pos = egui::pos2(cx + left_pad + icon_w, cy + chip_h / 2.0);
                let close = egui::Rect::from_min_size(
                    egui::pos2(cx + chip_w - close_w, cy),
                    egui::vec2(close_w, chip_h),
                );
                chips.push(ChipLayout {
                    idx,
                    rect,
                    icon,
                    label_pos,
                    close,
                });
                cx += chip_w + 4.0;
            }
            (HeaderContentLayout::Tabs { chips }, Some(cx + 8.0))
        }
        HeaderSpec::Title { title_w, has_icon } => {
            let mut tx = scr.min.x + 11.0;
            let icon_disp = 14.0_f32;
            let icon = has_icon.then(|| {
                egui::Rect::from_center_size(
                    egui::pos2(tx + icon_disp / 2.0, scr.min.y + TITLE_H / 2.0),
                    egui::vec2(icon_disp, icon_disp),
                )
            });
            if has_icon {
                tx += icon_disp + 7.0;
            }
            let text_pos = egui::pos2(tx, scr.min.y + TITLE_H / 2.0);
            (
                HeaderContentLayout::Title { icon, text_pos },
                Some(tx + title_w + 14.0),
            )
        }
    };

    let plus = if is_project {
        plus_x.map(|x| {
            egui::Rect::from_center_size(
                egui::pos2(x.min(scr.max.x - ctl_w - 10.0), scr.min.y + TITLE_H / 2.0),
                egui::vec2(16.0, 16.0),
            )
        })
    } else {
        None
    };

    let roles: &[CtlRole] = if is_project {
        &[CtlRole::Close, CtlRole::Ovf]
    } else {
        &[CtlRole::Close, CtlRole::Max, CtlRole::Min, CtlRole::Float]
    };
    let by = scr.min.y + 3.0;
    let bh = TITLE_H - 6.0;
    let mut bx = scr.max.x - 4.0 - 22.0;
    let mut controls = Vec::with_capacity(roles.len());
    for &role in roles {
        controls.push((
            role,
            egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(22.0, bh)),
        ));
        bx -= 25.0;
    }

    HeaderLayout {
        avail_end,
        content,
        plus,
        controls,
    }
}

/// A tab's display name for chat purposes: the title minus the one-shot
/// exit marker `refresh_exit_titles` appends.
fn display_name(title: &str) -> &str {
    title.split("  ·  exited").next().unwrap_or(title).trim()
}

/// Titles we still own: stock shell defaults (`powershell  ·  #3`) and prior
/// agent auto-names (`Claude  ·  #3`). Custom renames and exit stamps are not.
fn title_is_auto_managed(title: &str) -> bool {
    if title.contains("  ·  exited") {
        return false;
    }
    let Some((label, num)) = title.split_once("  ·  #") else {
        return false;
    };
    if label.is_empty() || num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    matches!(
        label,
        "powershell" | "cmd" | "bash" | "Claude" | "Codex" | "Grok"
    )
}

/// Stock shell default only (`powershell  ·  #3`) — not agent names or custom.
fn title_is_shell_default(title: &str) -> bool {
    let Some((label, num)) = title.split_once("  ·  #") else {
        return false;
    };
    matches!(label, "powershell" | "cmd" | "bash")
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit())
}

/// Pure: if the tab is auto-title-eligible and `icon` is an agent, propose
/// `"{Agent}  ·  #{term_id}"`. Also renames when the live title is still a
/// stock shell default even if the flag was lost (workspace restore). See #6.
fn auto_agent_title(
    current: &str,
    auto_title: bool,
    icon: crate::icons::IconKind,
    term_id: u64,
) -> Option<String> {
    // Don't clobber the one-shot exit marker from `refresh_exit_titles`.
    if current.contains("  ·  exited") {
        return None;
    }
    let agent = icon.agent_label()?;
    let want = format!("{agent}  ·  #{term_id}");
    if current == want {
        return None;
    }
    // Eligible: shell-spawn flag / managed restore, OR title still stock shell
    // (covers older restores that forced Tab::fixed and cleared the flag).
    if !auto_title && !title_is_shell_default(current) {
        return None;
    }
    Some(want)
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

/// Active-tab-preferred terminal-tab pick: the active tab if it holds a
/// `Content::Terminal`, else the first terminal tab (`None` if the window
/// has no terminal tabs at all). This is the ONE place the policy lives —
/// every control executor that reads or feeds "the terminal in window `tN`"
/// must agree on which tab that means.
fn terminal_tab_idx(tw: &Win) -> Option<usize> {
    if matches!(tw.tabs[tw.active].content, Content::Terminal(_)) {
        Some(tw.active)
    } else {
        tw.tabs
            .iter()
            .position(|t| matches!(t.content, Content::Terminal(_)))
    }
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

/// Processes that closing this one tab would kill: a terminal → at most its own
/// group; a project → one group per terminal inside it; chat → none.
fn groups_in_tab(tab: &Tab) -> Vec<crate::confirm::ProcGroup> {
    match &tab.content {
        Content::Terminal(s) => {
            // A dead shell's root_pid may be recycled — don't scan it (see
            // terminal_shells). An exited terminal has nothing left to warn about.
            let procs = if s.has_exited() {
                Vec::new()
            } else {
                s.root_pid()
                    .map(crate::proc::top_children)
                    .unwrap_or_default()
            };
            if procs.is_empty() {
                Vec::new()
            } else {
                vec![crate::confirm::ProcGroup {
                    label: tab.title.clone(),
                    scope: None,
                    procs,
                }]
            }
        }
        Content::Project(wm) => wm.terminal_groups(),
        Content::Chat(_) | Content::TaskManager(_) => Vec::new(),
    }
}

/// Top-level processes across all groups (the rows the modal shows).
fn top_count(groups: &[crate::confirm::ProcGroup]) -> usize {
    groups.iter().map(|g| g.procs.len()).sum()
}

/// Descendants rolled up under those top-level processes (the "(+n)" totals).
fn background_count(groups: &[crate::confirm::ProcGroup]) -> usize {
    groups
        .iter()
        .flat_map(|g| &g.procs)
        .map(|p| p.background)
        .sum()
}

/// The lead line, e.g. "1 process (+16 background) still running:" or
/// "3 processes still running across 2 terminals:". The background clause is
/// dropped when nothing is rolled up; the "across" clause when it's a single
/// unit (a lone terminal/project reads fine without it).
fn running_lead(top: usize, bg: usize, across: Option<(usize, &str)>) -> String {
    let procs = if top == 1 { "process" } else { "processes" };
    let bg_clause = if bg > 0 {
        format!(" (+{bg} background)")
    } else {
        String::new()
    };
    let across_clause = match across {
        Some((k, unit)) if k > 1 => format!(" across {k} {unit}s"),
        _ => String::new(),
    };
    format!("{top} {procs}{bg_clause} still running{across_clause}:")
}

/// Compose the confirm copy for a pane/project close from the gathered groups.
fn build_confirm(
    is_project: bool,
    groups: Vec<crate::confirm::ProcGroup>,
) -> crate::confirm::ConfirmClose {
    let top = top_count(&groups);
    let bg = background_count(&groups);
    if is_project {
        crate::confirm::ConfirmClose::new(
            "close this project?",
            running_lead(top, bg, Some((groups.len(), "terminal"))),
            "close anyway",
            groups,
        )
    } else {
        crate::confirm::ConfirmClose::new(
            "close this terminal?",
            running_lead(top, bg, None),
            "close anyway",
            groups,
        )
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
            tabs: vec![Tab::fixed(title.to_string(), stub_content())],
            active: 0,
            rect: egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(400.0, 300.0)),
            z: wm.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
        wm.focused = Some(id);
        id
    }

    #[test]
    fn nested_mark_surfaces_on_desktop_poll() {
        let mut d = WindowManager::new().as_desktop();
        let _id = push(&mut d, "p");
        // mark dirty on nested project manager
        if let Content::Project(child) = &mut d.windows[0].tabs[0].content {
            child.mark_workspace_dirty();
        }
        assert!(d.poll_workspace_dirty());
        assert!(!d.poll_workspace_dirty(), "take clears");
    }

    #[test]
    fn capture_workspace_skips_panel_and_records_tree() {
        let mut d = WindowManager::new().as_desktop();
        // panel-like window
        let pid = {
            let id = d.next;
            d.next += 1;
            d.z += 1;
            d.windows.push(Win {
                id,
                tabs: vec![Tab::fixed(
                    "sessions",
                    Content::TaskManager(crate::panel::PanelView::new(
                        false,
                        crate::panel::PANEL_W,
                    )),
                )],
                active: 0,
                rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(260.0, 800.0)),
                z: d.z,
                minimized: false,
                min_from_tree: false,
                prev: None,
            });
            id
        };
        let a = push(&mut d, "proj-a");
        let b = push(&mut d, "proj-b");
        // Mark as projects with cwd (mutate stub content)
        for (id, cwd) in [(a, r"C:\a"), (b, r"C:\b")] {
            if let Some(w) = d.windows.iter_mut().find(|w| w.id == id) {
                if let Content::Project(child) = &mut w.tabs[0].content {
                    child.cwd = Some(std::path::PathBuf::from(cwd));
                }
            }
        }
        d.tree = crate::layout::LayoutTree {
            root: Some(crate::layout::Node::Split {
                dir: crate::layout::SplitDir::H,
                ratios: vec![0.4, 0.6],
                children: vec![crate::layout::Node::Leaf(a), crate::layout::Node::Leaf(b)],
            }),
        };
        // Panel not in tree for this test (or is — either way capture must omit panel win)
        d.focused = Some(b);

        let snap = d.capture_workspace();
        assert_eq!(snap.version, crate::workspace::WORKSPACE_VERSION);
        assert_eq!(snap.desktop.windows.len(), 2, "panel window omitted");
        assert!(snap.desktop.windows.iter().all(|w| w.id != pid));
        assert_eq!(snap.desktop.focused, Some(b));
        assert!(snap.desktop.tree.is_some());
        // Nested project cwds preserved
        let cwds: Vec<_> = snap
            .desktop
            .windows
            .iter()
            .filter_map(|w| match &w.tabs[0].content {
                crate::workspace::ContentSnap::Project { child } => child.cwd.clone(),
                _ => None,
            })
            .collect();
        assert!(cwds.iter().any(|p| p == std::path::Path::new(r"C:\a")));
        assert!(cwds.iter().any(|p| p == std::path::Path::new(r"C:\b")));
        // z-order: a pushed before b → a.z < b.z → a appears first (back)
        assert_eq!(snap.desktop.windows[0].id, a);
        assert_eq!(snap.desktop.windows[1].id, b);
    }

    #[test]
    fn capture_records_chat_tab() {
        let mut m = WindowManager::new();
        m.cwd = Some(std::path::PathBuf::from(r"C:\p"));
        let id = m.next;
        m.next += 1;
        m.z += 1;
        m.windows.push(Win {
            id,
            tabs: vec![Tab::fixed(
                "chat",
                Content::Chat(crate::chat::ChatView::new(std::rc::Rc::clone(&m.chat))),
            )],
            active: 0,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0)),
            z: m.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
        let snap = crate::workspace::capture_manager(&m);
        assert_eq!(snap.cwd.as_deref(), Some(std::path::Path::new(r"C:\p")));
        assert!(matches!(
            snap.windows[0].tabs[0].content,
            crate::workspace::ContentSnap::Chat
        ));
        assert_eq!(snap.windows[0].id, id);
    }

    #[test]
    fn apply_restores_project_cwd_and_one_shell() {
        use crate::workspace::{
            ContentSnap, ManagerSnap, NodeSnap, RectSnap, TabSnap, WORKSPACE_VERSION, WinSnap,
            WorkspaceSnapshot,
        };
        let dir = std::env::temp_dir().join(format!("foreman-ws-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let ctx = egui::Context::default();
        let snap = WorkspaceSnapshot {
            version: WORKSPACE_VERSION,
            desktop: ManagerSnap {
                cwd: None,
                focused: Some(1),
                last_focused: None,
                zoomed: None,
                windows: vec![WinSnap {
                    id: 1,
                    active: 0,
                    tabs: vec![TabSnap {
                        title: "proj".into(),
                        content: ContentSnap::Project {
                            child: ManagerSnap {
                                cwd: Some(dir.clone()),
                                focused: Some(2),
                                last_focused: None,
                                zoomed: None,
                                windows: vec![WinSnap {
                                    id: 2,
                                    active: 0,
                                    tabs: vec![TabSnap {
                                        title: "cmd".into(),
                                        content: ContentSnap::Terminal {
                                            shell: "cmd".into(),
                                        },
                                    }],
                                    minimized: false,
                                    min_from_tree: false,
                                    rect: RectSnap {
                                        x: 0.0,
                                        y: 0.0,
                                        w: 580.0,
                                        h: 380.0,
                                    },
                                    prev: None,
                                }],
                                tree: Some(NodeSnap::Leaf { id: 2 }),
                            },
                        },
                    }],
                    minimized: false,
                    min_from_tree: false,
                    rect: RectSnap {
                        x: 10.0,
                        y: 20.0,
                        w: 720.0,
                        h: 480.0,
                    },
                    prev: None,
                }],
                tree: Some(NodeSnap::Leaf { id: 1 }),
            },
        };
        let mut d = WindowManager::new().as_desktop();
        let rep = d.apply_workspace(&snap, &ctx);
        assert_eq!(rep.projects_restored, 1);
        assert_eq!(rep.projects_skipped, 0);

        let w = d
            .windows
            .iter()
            .find(|w| w.is_project())
            .expect("project window");
        assert_eq!(w.rect.min, egui::pos2(10.0, 20.0));
        assert!(d.tree.contains(w.id), "tree leaf remapped to runtime id");
        assert_eq!(d.focused, Some(w.id));
        match &w.tabs[0].content {
            Content::Project(child) => {
                assert_eq!(child.cwd.as_ref(), Some(&dir));
                assert_eq!(child.tag.as_deref(), Some(format!("p{}", w.id).as_str()));
                assert!(
                    child.windows.iter().any(|cw| {
                        cw.tabs
                            .iter()
                            .any(|t| matches!(t.content, Content::Terminal(_)))
                    }),
                    "expected at least one Terminal tab inside restored project"
                );
            }
            _ => panic!("expected Project content"),
        }
        // Apply must not pollute the recents open-drain.
        assert!(d.take_opened().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_skips_missing_project_dir() {
        use crate::workspace::{
            ContentSnap, ManagerSnap, NodeSnap, RectSnap, TabSnap, WORKSPACE_VERSION, WinSnap,
            WorkspaceSnapshot,
        };
        let ctx = egui::Context::default();
        let snap = WorkspaceSnapshot {
            version: WORKSPACE_VERSION,
            desktop: ManagerSnap {
                cwd: None,
                focused: Some(1),
                last_focused: None,
                zoomed: None,
                windows: vec![WinSnap {
                    id: 1,
                    active: 0,
                    tabs: vec![TabSnap {
                        title: "gone".into(),
                        content: ContentSnap::Project {
                            child: ManagerSnap {
                                cwd: Some(std::path::PathBuf::from(
                                    r"C:\foreman-ws-does-not-exist-xyz",
                                )),
                                focused: None,
                                last_focused: None,
                                zoomed: None,
                                windows: vec![],
                                tree: None,
                            },
                        },
                    }],
                    minimized: false,
                    min_from_tree: false,
                    rect: RectSnap {
                        x: 0.0,
                        y: 0.0,
                        w: 100.0,
                        h: 100.0,
                    },
                    prev: None,
                }],
                tree: Some(NodeSnap::Leaf { id: 1 }),
            },
        };
        let mut d = WindowManager::new().as_desktop();
        let rep = d.apply_workspace(&snap, &ctx);
        assert_eq!(rep.projects_restored, 0);
        assert!(rep.projects_skipped >= 1);
        assert!(d.windows.iter().all(|w| !w.is_project()));
        assert!(d.windows.is_empty(), "full skip leaves desktop empty");
        assert!(d.tree.root.is_none());
        assert!(d.take_opened().is_empty());
    }

    #[test]
    fn project_opens_land_in_the_drain_and_drain_empties() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new().as_desktop();
        wm.add_project(
            Shell::PowerShell,
            std::path::PathBuf::from("C:\\proj"),
            &ctx,
        );
        wm.add_project_with_command(std::path::PathBuf::from("C:\\agent"), "claude", &ctx);
        assert_eq!(
            wm.take_opened(),
            vec![
                (std::path::PathBuf::from("C:\\proj"), None),
                (
                    std::path::PathBuf::from("C:\\agent"),
                    Some("claude".to_string())
                ),
            ]
        );
        assert!(wm.take_opened().is_empty(), "take drains");
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

    // --- merge_target_at (titlebar hit-testing) -----------------------------
    // NOTE: `merge_target_at` returns the window *index* (`Some(usize)`) into
    // `self.windows`, not the `WinId`. `push` gives every window the same local
    // rect (20,20)+(400,300); an `area` anchored at the origin makes local ==
    // screen coords, so the titlebar band is y ∈ [20, 20+TITLE_H). `9999` is a
    // `WinId` no window owns, used when the drag source is irrelevant.

    // A screen area at the origin: window-local rects coincide with screen rects.
    fn origin_area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(2000.0, 2000.0))
    }
    // A point inside the shared titlebar band of a `push`ed window.
    fn titlebar_point() -> egui::Pos2 {
        egui::pos2(100.0, 20.0 + TITLE_H / 2.0)
    }

    #[test]
    fn merge_target_hits_the_titlebar_band() {
        let mut wm = WindowManager::new();
        let _a = push(&mut wm, "A"); // index 0
        assert_eq!(
            wm.merge_target_at(9999, titlebar_point(), origin_area(), &[0]),
            Some(0)
        );
    }

    #[test]
    fn merge_target_misses_the_body_below_the_titlebar() {
        let mut wm = WindowManager::new();
        let _a = push(&mut wm, "A"); // index 0
        // Inside the window body (rect y ∈ [20,320]) but below the titlebar band.
        let body = egui::pos2(100.0, 20.0 + TITLE_H + 50.0);
        assert_eq!(
            wm.merge_target_at(9999, body, origin_area(), &[0]),
            None,
            "only the titlebar band is a merge target, not the body"
        );
    }

    #[test]
    fn merge_target_prefers_topmost_in_z_order() {
        let mut wm = WindowManager::new();
        let _a = push(&mut wm, "A"); // index 0
        let _b = push(&mut wm, "B"); // index 1, overlapping A
        // `order` is back-to-front, so its last entry is the top-most window.
        assert_eq!(
            wm.merge_target_at(9999, titlebar_point(), origin_area(), &[0, 1]),
            Some(1),
            "top-most (last in back-to-front order) wins"
        );
        // Reverse the stacking: index 0 is now on top.
        assert_eq!(
            wm.merge_target_at(9999, titlebar_point(), origin_area(), &[1, 0]),
            Some(0),
            "the z-order slice, not the windows vec order, decides the winner"
        );
    }

    #[test]
    fn merge_target_skips_the_dragged_source() {
        let mut wm = WindowManager::new();
        let _a = push(&mut wm, "A"); // index 0
        let b = push(&mut wm, "B"); // index 1, top-most, overlapping A
        // `b` is on top but is the drag source, so the target is A beneath it.
        assert_eq!(
            wm.merge_target_at(b, titlebar_point(), origin_area(), &[0, 1]),
            Some(0)
        );
    }

    #[test]
    fn merge_target_skips_minimized_windows() {
        let mut wm = WindowManager::new();
        let _a = push(&mut wm, "A"); // index 0
        let _b = push(&mut wm, "B"); // index 1, top-most, overlapping A
        wm.windows[1].minimized = true;
        // The top-most window is minimized, so the pointer falls through to A.
        assert_eq!(
            wm.merge_target_at(9999, titlebar_point(), origin_area(), &[0, 1]),
            Some(0)
        );
    }

    #[test]
    fn minimized_window_keeps_its_active_terminal_alive() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        let id = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .expect("spawn failed");
        wm.windows
            .iter_mut()
            .find(|w| w.id == id)
            .unwrap()
            .minimized = true;
        assert!(wm.draw_order().is_empty());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            ));
            let _ = ctx.run_ui(input, |ui| {
                let area = ui.max_rect();
                wm.show(ui, area, true, egui::Id::new("minimized-keepalive"), false);
            });

            let w = wm.windows.iter().find(|w| w.id == id).unwrap();
            let Content::Terminal(session) = &w.tabs[w.active].content else {
                panic!("test window stopped being a terminal");
            };
            if session.ready() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "minimized terminal never answered its startup DSR"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn unminimize_retiles_a_window_minimized_from_the_tree() {
        let mut wm = WindowManager::new();
        wm.last_area = egui::vec2(1000.0, 600.0);
        let a = push(&mut wm, "a");
        let b = push(&mut wm, "b");
        wm.tree.insert_root(a, Dir::Right);
        wm.tree.insert_root(b, Dir::Right);
        // give the windows their live tiled rects, as layout would each frame
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, wm.last_area);
        for (w, r) in wm.tree.layout(local, 8.0) {
            wm.windows.iter_mut().find(|win| win.id == w).unwrap().rect = r;
        }
        wm.minimize(b);
        assert!(!wm.tree.contains(b));
        assert!(wm.windows.iter().find(|w| w.id == b).unwrap().minimized);

        wm.surface_target(crate::panel::TargetPath {
            project: b,
            ptab: None,
            window: None,
            tab: None,
        });
        let wb = wm.windows.iter().find(|w| w.id == b).unwrap();
        assert!(!wb.minimized);
        assert!(wm.tree.contains(b), "restore should re-enter the tree");
    }

    #[test]
    fn unminimize_leaves_a_floating_minimized_window_floating() {
        let mut wm = WindowManager::new();
        wm.last_area = egui::vec2(1000.0, 600.0);
        let a = push(&mut wm, "a"); // floating, never tiled
        wm.minimize(a);
        wm.surface_target(crate::panel::TargetPath {
            project: a,
            ptab: None,
            window: None,
            tab: None,
        });
        let wa = wm.windows.iter().find(|w| w.id == a).unwrap();
        assert!(!wa.minimized);
        assert!(!wm.tree.contains(a), "floating windows restore floating");
    }

    #[test]
    fn surface_target_disambiguates_tabbed_projects_with_colliding_child_ids() {
        let mut desk = WindowManager::new();
        let w = push(&mut desk, "a");
        // Two inner managers: each numbers its child windows from 1, so the
        // child ids collide — exactly what tabbing two projects produces.
        let mut inner_a = WindowManager::new();
        let ca = push(&mut inner_a, "term-a");
        let mut inner_b = WindowManager::new();
        let cb = push(&mut inner_b, "term-b");
        assert_eq!(ca, cb, "the collision under test");
        let win = desk.windows.iter_mut().find(|x| x.id == w).unwrap();
        win.tabs = vec![
            Tab::fixed("a", Content::Project(Box::new(inner_a))),
            Tab::fixed("b", Content::Project(Box::new(inner_b))),
        ];
        win.active = 0;

        desk.surface_target(crate::panel::TargetPath {
            project: w,
            ptab: Some(1),
            window: Some(cb),
            tab: Some(0),
        });
        assert_eq!(
            desk.windows.iter().find(|x| x.id == w).unwrap().active,
            1,
            "the second project tab owns the clicked terminal"
        );
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

    // A Member's id is its Session's stable spawn-time id, NOT the mutable Win id.
    // Untab allocates a new Win id; the detached terminal must keep its identity
    // so chat delivery/self-exclusion/targeting don't break under it.
    #[test]
    fn auto_agent_title_renames_default_shell_only() {
        use crate::icons::IconKind;
        // Agent detected + auto_title → rename, keep term id suffix.
        assert_eq!(
            auto_agent_title("powershell  ·  #3", true, IconKind::Claude, 3),
            Some("Claude  ·  #3".into())
        );
        assert_eq!(
            auto_agent_title("powershell  ·  #7", true, IconKind::Grok, 7),
            Some("Grok  ·  #7".into())
        );
        // Already agent-named: no-op.
        assert_eq!(
            auto_agent_title("Claude  ·  #3", true, IconKind::Claude, 3),
            None
        );
        // Agent switch while still managed: rename.
        assert_eq!(
            auto_agent_title("Claude  ·  #3", true, IconKind::Codex, 3),
            Some("Codex  ·  #3".into())
        );
        // Shell icon: leave the default alone.
        assert_eq!(
            auto_agent_title("powershell  ·  #3", true, IconKind::PowerShell, 3),
            None
        );
        // Manual/dispatch title: auto_title off, not a shell default.
        assert_eq!(
            auto_agent_title("my work", false, IconKind::Claude, 3),
            None
        );
        // Flag lost but title still stock shell default → still rename (restore).
        assert_eq!(
            auto_agent_title("powershell  ·  #3", false, IconKind::Claude, 3),
            Some("Claude  ·  #3".into())
        );
        // Exit stamp is sticky — don't overwrite.
        assert_eq!(
            auto_agent_title("Claude  ·  #3  ·  exited (0)", true, IconKind::Claude, 3),
            None
        );
        assert!(title_is_auto_managed("powershell  ·  #1"));
        assert!(title_is_auto_managed("Claude  ·  #2"));
        assert!(!title_is_auto_managed("my work"));
        assert!(!title_is_auto_managed("Claude  ·  #2  ·  exited (0)"));
    }

    #[test]
    fn refresh_auto_titles_renames_shell_tabs_when_agent_detected() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        let id = wm.add_terminal(Shell::Cmd, &ctx).expect("shell");
        // Default title from add_terminal (shell.label() is lowercase).
        assert_eq!(
            wm.windows.iter().find(|w| w.id == id).unwrap().title(),
            &format!("cmd  ·  #{id}")
        );
        // Simulate a hand-launched agent: OSC title stem is the agent binary.
        {
            let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            let Content::Terminal(s) = &mut w.tabs[0].content else {
                panic!("expected terminal");
            };
            s.set_osc_title_for_test(Some("claude".into()));
        }
        wm.refresh_auto_titles();
        assert_eq!(
            wm.windows.iter().find(|w| w.id == id).unwrap().title(),
            &format!("Claude  ·  #{id}")
        );
        // Manual rename opts out permanently.
        {
            let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            w.tabs[0].title = "my pane".into();
            w.tabs[0].auto_title = false;
        }
        {
            let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
            let Content::Terminal(s) = &mut w.tabs[0].content else {
                panic!("expected terminal");
            };
            s.set_osc_title_for_test(Some("codex".into()));
        }
        wm.refresh_auto_titles();
        assert_eq!(
            wm.windows.iter().find(|w| w.id == id).unwrap().title(),
            "my pane",
            "user rename must stick"
        );
    }

    #[test]
    fn dispatch_titles_are_not_auto_renamed() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        let id = wm
            .add_terminal_cmd(&["claude".into()], None, Some("agent · claude"), &ctx)
            .unwrap();
        // icon_kind is Claude from dispatch argv, but title stays the explicit one.
        wm.refresh_auto_titles();
        assert_eq!(
            wm.windows.iter().find(|w| w.id == id).unwrap().title(),
            "agent · claude"
        );
    }

    #[test]
    fn term_id_survives_untab() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let a = wm
            .add_terminal_cmd(&pause_argv(), None, Some("A"), &ctx)
            .unwrap();
        let b = wm
            .add_terminal_cmd(&pause_argv(), None, Some("B"), &ctx)
            .unwrap();

        let term_id_at = |wm: &WindowManager, win: WinId, tab: usize| -> u64 {
            let w = wm.windows.iter().find(|w| w.id == win).unwrap();
            match &w.tabs[tab].content {
                Content::Terminal(s) => s.term_id(),
                _ => panic!("not a terminal"),
            }
        };
        // spawn stamps the Session with the id that also became its Win id.
        assert_eq!(term_id_at(&wm, a, 0), a, "spawn stamps term_id");
        assert_eq!(term_id_at(&wm, b, 0), b);

        // Tab B onto A, then pull it back out: untab mints a NEW Win id.
        wm.merge_windows(b, a); // A: [A, B]
        let new_id = wm
            .untab(a, 1, egui::pos2(500.0, 400.0))
            .expect("untab detaches");
        assert_ne!(new_id, b, "the detached tab lives in a brand-new Win id");
        assert_eq!(
            term_id_at(&wm, new_id, 0),
            b,
            "but the Member id is stable — it does not follow the Win id"
        );
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
    fn new_terminal_tiles_by_default() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        let id = wm.add_terminal(Shell::Cmd, &ctx).expect("shell");
        assert!(wm.tree.contains(id), "must spawn tiled by default");
    }

    #[test]
    fn new_terminal_floats_when_setting_says_so() {
        let ctx = egui::Context::default();
        let mut s = crate::config::Settings::default();
        s.new_windows_float = true;
        crate::config::seed_live(&ctx, &s);
        let mut wm = WindowManager::new();
        let id = wm.add_terminal(Shell::Cmd, &ctx).expect("shell");
        assert!(!wm.tree.contains(id), "must spawn floating");
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
    fn toggle_float_for_targets_an_unfocused_window_and_focuses_it() {
        // The header button acts on the clicked window, not the focused one.
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        wm.last_area = egui::vec2(1000.0, 800.0);
        wm.tree.insert_root(a, Dir::Right); // a tiled, b floating
        wm.focus(b);

        wm.toggle_float_for(a);
        assert!(!wm.tree.contains(a), "a detached from tree");
        assert_eq!(wm.focused, Some(a), "toggle focuses the toggled window");

        wm.toggle_float_for(a);
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
        m.push_win(
            id,
            Tab::fixed("proj", Content::Project(Box::new(child))),
            rect,
        );
        if !id_focused {
            m.focused = None;
        }
        m
    }

    #[test]
    fn bell_latches_the_stack_until_cleared_and_reaches_the_panel() {
        let ctx = egui::Context::default();
        let mut m = WindowManager::new();
        let env: Vec<(String, String)> = vec![];
        let s1 = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
        let s2 = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        m.push_win(1, Tab::fixed("front", Content::Terminal(s1)), r);
        m.windows[0]
            .tabs
            .push(Tab::fixed("back", Content::Terminal(s2)));

        assert!(!m.windows[0].bell_active(), "fresh sessions must not ring");

        // Ring the background tab: the whole stack (border rule) rings, but
        // only that tab's content does (chip / panel-row rule).
        let Content::Terminal(s) = &m.windows[0].tabs[1].content else {
            panic!("expected terminal");
        };
        s.ring_bell_for_test();
        assert!(m.windows[0].bell_active());
        assert!(m.windows[0].tabs[1].content.bell_active());
        assert!(!m.windows[0].tabs[0].content.bell_active());

        // Sticky: only clearing (keyboard focus landed) ends it — there is
        // no self-expiry.
        s.clear_bell();
        assert!(!m.windows[0].bell_active());

        // Panel read seam: the ringing tab's row carries bell and the project
        // row bubbles it up (for the collapsed rail).
        s.ring_bell_for_test();
        let mut desk = WindowManager::new();
        desk.push_win(7, Tab::fixed("proj", Content::Project(Box::new(m))), r);
        let pm = desk.panel_model();
        assert!(pm.projects[0].bell, "project row must bubble the ring");
        let rows: Vec<bool> = pm.projects[0].tabs.iter().map(|t| t.bell).collect();
        assert_eq!(rows, vec![false, true], "only the ringing tab's row rings");
    }

    #[test]
    fn terminal_shells_lists_one_pair_per_terminal_tab() {
        let ctx = egui::Context::default();
        let mut m = WindowManager::new();
        let env: Vec<(String, String)> = vec![];
        let s1 = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
        let s2 = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        m.push_win(1, Tab::fixed("one", Content::Terminal(s1)), r);
        m.push_win(2, Tab::fixed("two", Content::Terminal(s2)), r);

        let shells = m.terminal_shells();
        assert_eq!(shells.len(), 2);
        let titles: Vec<&str> = shells.iter().map(|(t, _)| t.as_str()).collect();
        assert!(titles.contains(&"one") && titles.contains(&"two"));
        assert!(shells.iter().all(|(_, pid)| *pid != 0));
    }

    #[test]
    fn idle_terminals_produce_no_groups() {
        let ctx = egui::Context::default();
        let mut m = WindowManager::new();
        let env: Vec<(String, String)> = vec![];
        let s = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        m.push_win(1, Tab::fixed("idle", Content::Terminal(s)), r);
        // An idle cmd.exe has no non-plumbing descendants → no group to warn about.
        assert!(m.terminal_groups().is_empty());
        // groups_in_tab agrees: the idle terminal contributes nothing.
        assert!(groups_in_tab(&m.windows[0].tabs[0]).is_empty());
    }

    #[test]
    fn closing_the_last_project_deserts_the_desktop() {
        let mut m = mgr_with_project(true);
        assert!(!m.deserted());
        let id = m.windows[0].id;
        m.close(id);
        assert!(m.deserted());
    }

    #[test]
    fn an_open_picker_keeps_the_empty_desktop_alive() {
        let mut m = mgr_with_project(true);
        let id = m.windows[0].id;
        m.picker = Some(DirPicker::new(m.picker_start()));
        m.close(id);
        assert!(!m.deserted(), "picker may still create a project; no quit");
    }

    #[test]
    fn closing_or_minimizing_a_renaming_window_clears_the_rename() {
        // A dangling `renaming` blocks focus for EVERY window (is_focus
        // requires renaming.is_none()), freezing the app until restart.
        let mut m = WindowManager::new();
        let (a, ra) = m.next_slot(egui::vec2(100.0, 100.0));
        m.push_win(a, Tab::fixed("one", stub_content()), ra);
        let (b, rb) = m.next_slot(egui::vec2(100.0, 100.0));
        m.push_win(b, Tab::fixed("two", stub_content()), rb);

        m.focus(a);
        m.begin_rename();
        assert_eq!(m.renaming, Some(a));
        m.close(a);
        assert!(m.renaming.is_none(), "close left a dangling rename");

        m.focus(b);
        m.begin_rename();
        assert_eq!(m.renaming, Some(b));
        m.minimize(b);
        assert!(m.renaming.is_none(), "minimize left a dangling rename");
    }

    #[test]
    fn header_fence_holds_under_packing_pressure() {
        // Over-wide tabs at a spread of window widths, incl. degenerate:
        // chips past the first and the `+` must never cross the fence.
        for w in [90.0f32, 141.0, 213.7, 400.0, 1200.0] {
            let scr = egui::Rect::from_min_size(egui::pos2(80.0, 40.0), egui::vec2(w, 300.0));
            let tabs = vec![
                TabMeasure {
                    label_w: 300.0,
                    has_icon: true
                };
                6
            ];
            let hl = header_layout(scr, true, false, HeaderSpec::Tabs(&tabs));
            let HeaderContentLayout::Tabs { chips } = &hl.content else {
                panic!("layout variant must mirror spec variant");
            };
            assert!(!chips.is_empty(), "active tab must stay reachable (w={w})");
            for c in &chips[1..] {
                assert!(
                    c.rect.max.x <= hl.avail_end,
                    "chip {} crossed the fence (w={w})",
                    c.idx
                );
            }
            let plus = hl.plus.expect("projects get a +");
            assert!(plus.max.x <= hl.avail_end, "+ crossed the fence (w={w})");
            for (role, r) in &hl.controls {
                assert!(!plus.intersects(*r), "+ overlaps {} (w={w})", role.id_str());
            }
        }
    }

    #[test]
    fn header_layout_mirrors_spec_kind_and_order() {
        let scr = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(500.0, 300.0));

        // Terminal: four controls right-to-left at 25pt pitch, no +.
        let hl = header_layout(
            scr,
            false,
            false,
            HeaderSpec::Title {
                title_w: 80.0,
                has_icon: true,
            },
        );
        assert!(hl.plus.is_none(), "terminals have no +");
        let roles: Vec<&str> = hl.controls.iter().map(|(r, _)| r.id_str()).collect();
        assert_eq!(roles, ["close", "max", "min", "float"]);
        assert_eq!(hl.controls[0].1.max.x, scr.max.x - 4.0);
        assert_eq!(hl.controls[1].1.max.x, scr.max.x - 29.0);

        // Rename: field present, + suppressed even on a project, and the
        // documented 40pt width floor holds on an absurdly narrow window.
        let hl = header_layout(scr, true, false, HeaderSpec::Rename);
        assert!(matches!(hl.content, HeaderContentLayout::Rename { .. }));
        assert!(hl.plus.is_none(), "no + while renaming");
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(30.0, 100.0));
        let hl = header_layout(tiny, true, false, HeaderSpec::Rename);
        let HeaderContentLayout::Rename { field } = hl.content else {
            panic!();
        };
        assert_eq!(field.width(), 40.0);

        // A 5000pt title still clamps the + inside the fence.
        let hl = header_layout(
            scr,
            true,
            false,
            HeaderSpec::Title {
                title_w: 5000.0,
                has_icon: false,
            },
        );
        assert!(hl.plus.unwrap().max.x <= hl.avail_end);
    }

    #[test]
    fn panel_title_drag_strip_stays_wide_on_narrow_panel() {
        // Sessions only has a collapse chevron (~28px). The old terminal
        // control reserve (113) left almost no drag target until the panel
        // was wider than that.
        let narrow = egui::vec2(120.0, 400.0);
        let ctl = header_ctl_w(false, true);
        assert!(ctl <= 30.0, "panel reserve is collapse-only, got {ctl}");
        let drag_w = (narrow.x - ctl).max(0.0);
        assert!(
            drag_w >= 90.0,
            "narrow Sessions panel must still have a usable drag strip, got {drag_w}"
        );
        // Terminal policy must remain the wider reserve.
        assert!((header_ctl_w(false, false) - 113.0).abs() < 0.1);
        assert!((header_ctl_w(true, false) - 54.0).abs() < 0.1);
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

    // Count Joined/Exited syslines for a member id by reading the room's paint
    // blocks (the room exposes no raw msg list — blocks() is the read path).
    // Sys lines render as "— {name} ({id}) joined|exited —".
    fn sys_lines(wm: &WindowManager, id: &str, verb: &str) -> usize {
        wm.chat
            .borrow()
            .blocks(0, true)
            .iter()
            .filter(|b| {
                matches!(b, crate::chat::ChatBlock::Sys(s)
                    if s.contains(&format!("({id}) {verb}")))
            })
            .count()
    }

    #[test]
    fn dispatched_terminals_auto_join_chat() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "exit 0".to_string(),
        ];
        let t = wm.add_terminal_cmd(&argv, None, None, &ctx).unwrap();
        // membership now lives in the room, not the Tab.
        assert!(wm.chat.borrow().is_member(&term_tag(t)));
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
        // exactly one Joined sysline for this member, carrying its name.
        assert_eq!(sys_lines(&wm, &term_tag(t), "joined"), 1);
        assert!(
            wm.chat
                .borrow()
                .blocks(0, true)
                .iter()
                .any(|b| matches!(b, crate::chat::ChatBlock::Sys(s)
                if s.contains(&format!("worker A ({}) joined", term_tag(t)))))
        );
    }

    #[test]
    fn first_post_emits_joined_before_the_post() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let t = wm
            .add_terminal_cmd(&pause_argv(), None, None, &ctx)
            .unwrap();
        // dispatch auto-joined once; posting is idempotent on an existing
        // member — no second Joined line, just the Post.
        wm.chat_post(&term_tag(t), "hello", &[], None).unwrap();
        // history shows the post; blocks show the single join then the post.
        assert_eq!(sys_lines(&wm, &term_tag(t), "joined"), 1);
        assert_eq!(wm.chat_history(10), vec![format!("#2 t{t}: hello")]);
        // a second post adds no Joined line and a second history entry.
        wm.chat_post(&term_tag(t), "again", &[], None).unwrap();
        assert_eq!(
            sys_lines(&wm, &term_tag(t), "joined"),
            1,
            "one Joined from dispatch — posting never re-joins"
        );
        assert_eq!(
            wm.chat_history(10),
            vec![format!("#2 t{t}: hello"), format!("#3 t{t}: again")]
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
        // A hand-opened (non-dispatched) terminal: never joins. Push it as a
        // plain window so the room never registers it (it stays on `pause`).
        let outsider = {
            let mut s =
                Session::spawn_argv(&pause_argv(), None, &[], ctx.clone()).expect("spawn outsider");
            let (id, rect) = wm.next_slot(egui::vec2(580.0, 380.0));
            s.set_term_id(id);
            wm.push_win(id, Tab::fixed("plain", Content::Terminal(s)), rect);
            id
        };
        assert!(!wm.chat.borrow().is_member(&term_tag(outsider)));
        // wait for the `cmd /c exit 0` member child to end — pumping keepalive()
        // each pass, or the startup DSR query leaves cmd.exe hung forever
        // (the documented trap; same pattern as the broadcast tests above)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let w = wm.windows.iter_mut().find(|w| w.id == member).unwrap();
            let Content::Terminal(s) = &mut w.tabs[w.active].content else {
                panic!()
            };
            s.keepalive();
            if s.exited().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "child never exited");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // chat_tick's presence reconcile emits the Exited line now. Keep the
        // outsider's pwsh pumped so it never falsely looks gone.
        for w in wm.windows.iter_mut() {
            if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                s.keepalive();
            }
        }
        wm.chat_tick();
        // exactly one Exited line — the member's — and none for the non-member.
        assert_eq!(sys_lines(&wm, &term_tag(member), "exited"), 1);
        assert_eq!(sys_lines(&wm, &term_tag(outsider), "exited"), 0);
        assert!(
            wm.chat
                .borrow()
                .blocks(0, true)
                .iter()
                .any(|b| matches!(b, crate::chat::ChatBlock::Sys(s)
                if s.contains(&format!("worker A ({}) exited", term_tag(member)))))
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
        let tag = term_tag(t);

        // empty text is refused by the room; an unknown sender id has no live
        // terminal so chat_post errors before touching the room.
        assert!(
            wm.chat_post(&tag, "", &[], None).is_err(),
            "empty message rejected"
        );
        assert!(
            wm.chat_post("t999", "hi", &[], None).is_err(),
            "unknown sender rejected"
        );
        let seq = wm.chat_post(&tag, "hello room", &[], None).unwrap();
        // seq 2: dispatch Joined (1), then the post — the join is idempotent so
        // posting does not add a second Joined. System entries share the seq
        // space but stay out of --history.
        assert_eq!(seq, 2);
        assert!(wm.chat.borrow().is_member(&tag), "the sender is a member");
        assert_eq!(wm.chat_history(10), vec![format!("#2 t{t}: hello room")]);
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
            w.tabs.push(Tab::fixed("shell", Content::Terminal(shell)));
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

    // The viewer now PULLS crew rows from the room each draw — there is no
    // pushed `crew` field or title chip. This re-expresses the old
    // refresh_chat_view coverage against the room's crew() (the single source).
    #[test]
    fn room_crew_lists_member_then_human_seat() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::vec2(800.0, 600.0);
        let a = wm
            .add_terminal_cmd(&pause_argv(), None, Some("worker A"), &ctx)
            .unwrap();
        // a hand-opened (non-member) terminal: never registered with the room.
        let _b = {
            let mut s =
                Session::spawn_argv(&pause_argv(), None, &[], ctx.clone()).expect("spawn plain");
            let (id, rect) = wm.next_slot(egui::vec2(580.0, 380.0));
            s.set_term_id(id);
            wm.push_win(id, Tab::fixed("plain", Content::Terminal(s)), rect);
            id
        };
        let crew = wm.chat.borrow().crew(std::time::Instant::now());
        assert_eq!(crew.len(), 2, "the one member + the human pane identity");
        // the live member row carries its name and last-heard.
        let m = crew
            .iter()
            .find(|r| r.id == term_tag(a))
            .expect("member row missing");
        assert_eq!(m.name, "worker A");
        assert!(!m.exited);
        assert!(m.last.is_some(), "joined entry counts as heard");
        // the non-member terminal contributes no row.
        assert!(crew.iter().all(|r| r.id != term_tag(_b)));
        // the human seat sits AFTER the live members (index 1).
        assert_eq!(crew[1].id, "you");
        assert_eq!(crew[1].name, "you");
        assert!(!crew[1].exited);
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
        // The click now carries a member id (tN); record a click on worker A.
        let set_click = |wm: &mut WindowManager, id: Option<String>| {
            for w in &mut wm.windows {
                for tab in &mut w.tabs {
                    if let Content::Chat(v) = &mut tab.content {
                        v.click = id.clone();
                    }
                }
            }
        };
        set_click(&mut wm, Some(term_tag(t)));
        wm.drain_chat_clicks();
        assert_eq!(wm.focused, Some(t), "click focused the member");
        assert_ne!(wm.focused, Some(chat_id));
        // stale id with no live terminal: must not panic or change focus
        set_click(&mut wm, Some("t9999".to_string()));
        wm.drain_chat_clicks();
        assert_eq!(wm.focused, Some(t), "stale click is a no-op");
        // the human seat id resolves to no terminal: also a silent no-op
        wm.open_chat_window(); // refocuses the singleton viewer
        let chat_id = wm.focused.expect("viewer focused");
        set_click(&mut wm, Some("you".to_string()));
        wm.drain_chat_clicks();
        assert_eq!(
            wm.focused,
            Some(chat_id),
            "the human seat has no terminal — a silent no-op"
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
        {
            // the human post lands under the reserved `you` id (history is the
            // read path now; system entries are excluded from it).
            let hist = wm.chat_history(10);
            let last = hist.last().expect("post missing");
            assert!(
                last.ends_with(" you: go"),
                "human post framed under `you`: {last}"
            );
        }
        // BOTH members exit — the human is not a member, so its post is
        // addressed to every member. chat_tick only delivers to ready()
        // sessions, so pump every session each iteration (latching ready())
        // then tick until both stdins have seen it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            wm.chat_tick();
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
        assert_eq!(
            wm.chat.borrow().last_seq(),
            0,
            "blank input appends nothing"
        );
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
        let room = Rc::new(RefCell::new(crate::chat::ChatRoom::new()));
        // join t1 (Joined is #1), then a backlog post (#2) before the view opens.
        room.borrow_mut().join("t1", "a");
        room.borrow_mut()
            .post("t1", "before-open", &[], None)
            .unwrap();
        let mut v = crate::chat::ChatView::new(Rc::clone(&room));
        assert_eq!(
            v.last_seen, 2,
            "creation watermark = current tail (backlog pre-dates the window open)"
        );
        v.on_frame(true); // focused
        room.borrow_mut()
            .post("t1", "while-focused", &[], None)
            .unwrap(); // #3
        v.on_frame(true);
        assert_eq!(v.last_seen, 2, "watermark holds while focused");
        v.on_frame(false); // focus left
        assert_eq!(
            v.last_seen, 3,
            "watermark catches up on the focus-loss edge"
        );
        room.borrow_mut()
            .post("t1", "while-unfocused", &[], None)
            .unwrap(); // #4
        v.on_frame(false);
        assert_eq!(
            v.last_seen, 3,
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
        // outsider is hand-opened (pushed plainly): never registered with the
        // room, so the membership filter must skip it.
        let outsider = {
            let mut s =
                Session::spawn_argv(&pause_argv(), None, &[], ctx.clone()).expect("spawn outsider");
            let (id, rect) = wm.next_slot(egui::vec2(580.0, 380.0));
            s.set_term_id(id);
            wm.push_win(id, Tab::fixed("plain", Content::Terminal(s)), rect);
            id
        };
        assert!(!wm.chat.borrow().is_member(&term_tag(outsider)));

        wm.chat_post(&term_tag(sender), "go", &[], None).unwrap();

        // Pump every session each iteration: keepalive() answers the startup
        // DSR (the documented trap — bytes injected before a child's DSR scan
        // resolves get eaten by the scan, see terminal.rs's
        // inject_input_reaches_child_stdin). chat_tick only delivers to ready()
        // members and never the sender's own post, so pumping latches ready()
        // and the tick delivers. It still proves the membership filter: the
        // sender and the non-member are pumped too — so a wrongful injection
        // into them WOULD make them exit below.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            wm.chat_tick();
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
        d.push_win(
            1,
            Tab::fixed("proj", Content::Project(Box::new(child))),
            rect,
        );
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
        // end-to-end: member b runs `cmd /c pause` and exits when bytes arrive.
        // The post was appended once above; each iteration pumps the child
        // sessions (latching ready()) then chat_tick recurses into the project
        // and delivers. The cursor + ready-gating make a re-send unnecessary.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &mut win.tabs[win.active].content else {
                panic!()
            };
            // Keep pumping so each child session reports its current ready().
            for w in child.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            // Drop the &mut d.windows borrow before ticking, then re-borrow.
            d.chat_tick();
            let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &mut win.tabs[win.active].content else {
                panic!()
            };
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
        // The history arm is anonymous (it ignores `from`) and must never
        // append — capture the room's seq before and after to prove it.
        let seq_before = {
            let win = d.windows.iter().find(|w| w.id == 1).unwrap();
            let Content::Project(child) = &win.tabs[win.active].content else {
                panic!()
            };
            child.chat.borrow().last_seq()
        };
        // history keyed by b: replies, does not error, does not append/join
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
        let win = d.windows.iter().find(|w| w.id == 1).unwrap();
        let Content::Project(child) = &win.tabs[win.active].content else {
            panic!()
        };
        assert_eq!(
            child.chat.borrow().last_seq(),
            seq_before,
            "reading history must not append a Joined (or anything)"
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
            let room = child.chat.borrow();
            let members: Vec<bool> = child
                .windows
                .iter()
                .flat_map(|w| w.tabs.iter())
                .filter_map(|t| match &t.content {
                    Content::Terminal(s) => Some(room.is_member(&term_tag(s.term_id()))),
                    _ => None,
                })
                .collect();
            (room.last_seq(), members)
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
        d.push_win(
            2,
            Tab::fixed("other", Content::Project(Box::new(child2))),
            rect,
        );

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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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

    // --- send / snapshot verbs ---

    fn send_msg(
        project: Option<&str>,
        terminal: &str,
        text: &str,
        sent: std::time::Instant,
        settle_ms: Option<u64>,
    ) -> (
        crate::control::CtrlMsg,
        std::sync::mpsc::Receiver<crate::control::OpenReply>,
    ) {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let req = crate::control::SendRequest {
            cmd: "send".into(),
            project: project.map(str::to_string),
            terminal: Some(terminal.to_string()),
            text: Some(text.to_string()),
            keys: vec![],
            settle_ms,
        };
        (crate::control::CtrlMsg::Send(req, rtx, sent), rrx)
    }

    fn snapshot_msg(
        project: Option<&str>,
        terminal: &str,
        sent: std::time::Instant,
    ) -> (
        crate::control::CtrlMsg,
        std::sync::mpsc::Receiver<crate::control::OpenReply>,
    ) {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let req = crate::control::SnapshotRequest {
            cmd: "snapshot".into(),
            project: project.map(str::to_string),
            terminal: Some(terminal.to_string()),
            attrs: false,
            cursor: false,
        };
        (crate::control::CtrlMsg::Snapshot(req, rtx, sent), rrx)
    }

    #[test]
    fn send_replies_ok_for_valid_terminal() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let (msg, rrx) = send_msg(Some("p1"), &ta, "hello", std::time::Instant::now(), Some(0));
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no send reply");
        assert!(r.ok, "{:?}", r.error);
        assert_eq!(r.history, None); // send does not return a snapshot
    }

    #[test]
    fn send_with_settle_ms_zero_replies_immediately() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let (msg, rrx) = send_msg(Some("p1"), &ta, "x", std::time::Instant::now(), Some(0));
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("settle_ms=0 must reply immediately");
        assert!(r.ok, "{:?}", r.error);
    }

    #[test]
    fn send_with_settle_defers_then_replies_after_deadline() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        // Default settle (None → Settings::send_settle_ms, 120 by default):
        // no immediate reply — the request is parked on the pending list.
        let (msg, rrx) = send_msg(Some("p1"), &ta, "x", std::time::Instant::now(), None);
        d.handle_ctrl(msg, &ctx);
        assert!(
            rrx.try_recv().is_err(),
            "settle send must NOT reply synchronously"
        );
        // Advance past the MAX_SETTLE_MS deadline → the settle fires.
        let future = std::time::Instant::now() + std::time::Duration::from_millis(5000);
        d.advance_settles(future);
        let r = rrx
            .try_recv()
            .expect("settle must reply once the deadline passes");
        assert!(r.ok, "{:?}", r.error);
    }

    #[test]
    fn send_with_no_settle_ms_uses_the_configured_send_settle_default() {
        let ctx = egui::Context::default();
        let mut s = crate::config::Settings::default();
        s.send_settle_ms = 500;
        crate::config::seed_live(&ctx, &s);
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let sent = std::time::Instant::now();
        let (msg, rrx) = send_msg(Some("p1"), &ta, "x", sent, None);
        d.handle_ctrl(msg, &ctx);
        assert!(
            rrx.try_recv().is_err(),
            "settle send must NOT reply synchronously"
        );
        // Short of the configured 500ms default: still pending (proves the
        // settle used is 500, not the old hardcoded 120ms default).
        d.advance_settles(sent + std::time::Duration::from_millis(200));
        assert!(
            rrx.try_recv().is_err(),
            "must not fire before the configured send_settle_ms elapses"
        );
        // Past 500ms (still well under MAX_SETTLE_MS): the settle fires.
        d.advance_settles(sent + std::time::Duration::from_millis(600));
        let r = rrx
            .try_recv()
            .expect("settle must reply once send_settle_ms elapses");
        assert!(r.ok, "{:?}", r.error);
    }

    #[test]
    fn send_unknown_terminal_errors() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        let (msg, rrx) = send_msg(Some("p1"), "t99", "x", std::time::Instant::now(), Some(0));
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no send reply");
        assert!(!r.ok);
        assert!(
            r.error.as_deref().unwrap_or("").contains("t99"),
            "{:?}",
            r.error
        );
    }

    #[test]
    fn stale_send_is_dropped() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let stale = std::time::Instant::now()
            - (crate::control::REPLY_TIMEOUT + std::time::Duration::from_secs(1));
        let (msg, rrx) = send_msg(Some("p1"), &ta, "x", stale, Some(0));
        d.handle_ctrl(msg, &ctx);
        assert!(
            rrx.try_recv().is_err(),
            "stale send must be dropped unanswered"
        );
    }

    #[test]
    fn snapshot_replies_history_some_and_nonempty() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let (msg, rrx) = snapshot_msg(Some("p1"), &ta, std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no snapshot reply");
        assert!(r.ok, "{:?}", r.error);
        // snapshot always returns Some(lines) — even an idle terminal has rows
        assert!(r.history.is_some(), "snapshot must populate history field");
        assert!(
            !r.history.as_ref().unwrap().is_empty(),
            "snapshot rows must be non-empty"
        );
    }

    #[test]
    fn snapshot_attrs_and_cursor_opt_ins_populate_reply() {
        // --attrs / --cursor flow through handle_ctrl into the structured fields;
        // a default snapshot (no flags) leaves both None.
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");

        let make = |attrs: bool, cursor: bool| {
            let (rtx, rrx) = std::sync::mpsc::channel();
            let req = crate::control::SnapshotRequest {
                cmd: "snapshot".into(),
                project: Some("p1".into()),
                terminal: Some(ta.clone()),
                attrs,
                cursor,
            };
            (
                crate::control::CtrlMsg::Snapshot(req, rtx, std::time::Instant::now()),
                rrx,
            )
        };

        // default: neither field set
        let (msg, rrx) = make(false, false);
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no reply");
        assert!(r.ok);
        assert!(r.cells.is_none(), "no --attrs => no cells");
        assert!(r.cursor.is_none(), "no --cursor => no cursor");

        // both opt-ins
        let (msg, rrx) = make(true, true);
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no reply");
        assert!(r.ok);
        assert!(r.cells.is_some(), "--attrs must populate cells");
        assert!(r.cursor.is_some(), "--cursor must populate cursor");
    }

    #[test]
    fn snapshot_unknown_terminal_errors() {
        let ctx = egui::Context::default();
        let (mut d, _a, _b) = chat_fixture(&ctx);
        let (msg, rrx) = snapshot_msg(Some("p1"), "t99", std::time::Instant::now());
        d.handle_ctrl(msg, &ctx);
        let r = rrx.try_recv().expect("no snapshot reply");
        assert!(!r.ok);
        assert!(
            r.error.as_deref().unwrap_or("").contains("t99"),
            "{:?}",
            r.error
        );
    }

    #[test]
    fn stale_snapshot_is_dropped() {
        let ctx = egui::Context::default();
        let (mut d, a, _b) = chat_fixture(&ctx);
        let ta = format!("t{a}");
        let stale = std::time::Instant::now()
            - (crate::control::REPLY_TIMEOUT + std::time::Duration::from_secs(1));
        let (msg, rrx) = snapshot_msg(Some("p1"), &ta, stale);
        d.handle_ctrl(msg, &ctx);
        assert!(
            rrx.try_recv().is_err(),
            "stale snapshot must be dropped unanswered"
        );
    }

    // --- settle_tick pure logic ---

    #[test]
    fn settle_tick_not_done_within_window() {
        let t0 = std::time::Instant::now();
        let quiet_window = std::time::Duration::from_millis(120);
        let deadline = t0 + std::time::Duration::from_millis(4000);
        // gen unchanged, 50ms elapsed < 120ms window → not done
        let (g, qs, done) = super::settle_tick(
            5,
            t0,
            deadline,
            quiet_window,
            5, // current_gen == last_gen (no output)
            t0 + std::time::Duration::from_millis(50),
        );
        assert_eq!(g, 5, "gen unchanged");
        assert_eq!(qs, t0, "quiet_since unchanged");
        assert!(!done, "should not be done yet");
    }

    #[test]
    fn settle_tick_done_after_quiet_window() {
        let t0 = std::time::Instant::now();
        let quiet_window = std::time::Duration::from_millis(120);
        let deadline = t0 + std::time::Duration::from_millis(4000);
        // gen unchanged, 150ms elapsed > 120ms window → done
        let (g, qs, done) = super::settle_tick(
            5,
            t0,
            deadline,
            quiet_window,
            5,
            t0 + std::time::Duration::from_millis(150),
        );
        assert_eq!(g, 5);
        assert_eq!(qs, t0);
        assert!(done, "should be done after quiet window");
    }

    #[test]
    fn settle_tick_gen_change_resets_quiet_since() {
        let t0 = std::time::Instant::now();
        let quiet_window = std::time::Duration::from_millis(120);
        let deadline = t0 + std::time::Duration::from_millis(4000);
        // Gen changed at t0+150ms: quiet_since resets to now; not done even
        // though we are past the original quiet window measured from t0.
        let now = t0 + std::time::Duration::from_millis(150);
        let (g, qs, done) = super::settle_tick(
            5,
            t0,
            deadline,
            quiet_window,
            6, // current_gen != last_gen → output arrived
            now,
        );
        assert_eq!(g, 6, "gen must update to current");
        assert_eq!(qs, now, "quiet_since must reset to now");
        assert!(!done, "just received output, should not be done");
    }

    #[test]
    fn settle_tick_past_deadline_always_done() {
        let t0 = std::time::Instant::now();
        let quiet_window = std::time::Duration::from_millis(120);
        // deadline already in the past
        let deadline = t0 - std::time::Duration::from_millis(1);
        // Even if gen just changed, deadline overrules
        let now = t0;
        let (g, qs, done) = super::settle_tick(5, t0, deadline, quiet_window, 6, now);
        assert_eq!(g, 6);
        assert_eq!(qs, now);
        assert!(done, "past deadline must be done regardless of gen");
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
        // tab (its own distinct term_id, never joined); the dispatched member
        // terminal stays behind it as a background tab.
        {
            let shell_id = wm.next;
            wm.next += 1;
            let w = wm.windows.iter_mut().find(|w| w.id == host).unwrap();
            let mut shell = Session::spawn_argv(&pause_argv(), None, &[], ctx.clone()).unwrap();
            shell.set_term_id(shell_id);
            w.tabs.push(Tab::fixed("shell", Content::Terminal(shell)));
            w.active = 1; // shell in front, member behind
        }
        wm.chat_post(&term_tag(sender), "go", &[], None).unwrap();

        // Same tick + pump-everything pattern as
        // chat_broadcast_hits_members_only_excluding_sender (the DSR trap):
        // every tab of every window is kept pumped, so a wrongful injection
        // into the foreground shell would make it exit below.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            for w in wm.windows.iter_mut() {
                for t in w.tabs.iter_mut() {
                    if let Content::Terminal(s) = &mut t.content {
                        s.keepalive();
                    }
                }
            }
            wm.chat_tick();
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
        // bystander IS a member — only the post's target filter may exclude it.
        // Targeting now comes from the POST: address it to `target` alone.
        wm.chat_post(&term_tag(sender), "go", &[term_tag(target)], None)
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            wm.chat_tick();
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
        // injection would surface), and a pure @you post delivers to nobody
        wm.chat_post(&term_tag(sender), "@you go", &[], None)
            .unwrap();
        let grace = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < grace {
            for w in wm.windows.iter_mut() {
                if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                    s.keepalive();
                }
            }
            // tick on a ready frame: the @you post must deliver to nobody
            wm.chat_tick();
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
        // outsider is hand-opened (pushed plainly): a live terminal the room
        // never registered, so targeting it is an unknown-member error.
        let outsider = {
            let mut s =
                Session::spawn_argv(&pause_argv(), None, &[], ctx.clone()).expect("spawn outsider");
            let (id, rect) = wm.next_slot(egui::vec2(580.0, 380.0));
            s.set_term_id(id);
            wm.push_win(id, Tab::fixed("plain", Content::Terminal(s)), rect);
            id
        };
        let seq_before = wm.chat.borrow().last_seq();

        // unknown id — names it; one bad target fails a multi-target post entirely
        let e = wm
            .chat_post(
                &term_tag(sender),
                "go",
                &[term_tag(member), "t99".into()],
                None,
            )
            .unwrap_err();
        assert!(e.contains("unknown member t99"), "{e}");
        // self-mention (the room rejects targeting yourself)
        let e = wm
            .chat_post(&term_tag(sender), "go", &[term_tag(sender)], None)
            .unwrap_err();
        assert!(e.contains("cannot target yourself"), "{e}");
        // non-member: the outsider id is not registered with the room
        let e = wm
            .chat_post(&term_tag(sender), "go", &[term_tag(outsider)], None)
            .unwrap_err();
        assert!(
            e.contains(&format!("unknown member {}", term_tag(outsider))),
            "{e}"
        );
        // nothing appended by any failed post
        assert_eq!(wm.chat.borrow().last_seq(), seq_before);
        // inline mentions count too: a leading @ with a bad id fails the post
        let e = wm
            .chat_post(&term_tag(sender), "@t99 go", &[], None)
            .unwrap_err();
        assert!(e.contains("unknown member t99"), "{e}");
    }

    #[test]
    fn failed_targeted_post_does_not_append() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        // A hand-opened (non-dispatched) terminal: not yet a room member, so a
        // failed first post is the case that must not leave any trace.
        let sender = {
            let mut s =
                Session::spawn_argv(&pause_argv(), None, &[], ctx.clone()).expect("spawn sender");
            let (id, rect) = wm.next_slot(egui::vec2(580.0, 380.0));
            s.set_term_id(id);
            wm.push_win(id, Tab::fixed("plain", Content::Terminal(s)), rect);
            id
        };
        assert!(!wm.chat.borrow().is_member(&term_tag(sender)));
        let seq_before = wm.chat.borrow().last_seq();
        // a failing post (unknown target) must append nothing — the room
        // validates all-or-nothing before any mutation.
        let _ = wm
            .chat_post(&term_tag(sender), "go", &["t99".into()], None)
            .unwrap_err();
        assert!(
            !wm.chat.borrow().is_member(&term_tag(sender)),
            "failed post must not join"
        );
        assert_eq!(
            wm.chat.borrow().last_seq(),
            seq_before,
            "no Joined sysline or post appended"
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
        let mtag = term_tag(member);
        let stag = term_tag(sender);

        // flags first, then inline, deduped; `you` is a legal target. The
        // arrow framing now reads off the room's history line.
        wm.chat_post(&stag, "@you go", &[mtag.clone()], None)
            .unwrap();
        assert_eq!(
            wm.chat_history(1),
            vec![format!("#3 {stag}→{mtag},you: @you go")],
            "flag target precedes inline mention, deduped"
        );
        // pure-@you: targeted to the human seat alone
        wm.chat_post(&stag, "@you need eyes", &[], None).unwrap();
        assert_eq!(
            wm.chat_history(1),
            vec![format!("#4 {stag}→you: @you need eyes")]
        );
        // untargeted: broadcast (no arrow)
        wm.chat_post(&stag, "plain", &[], None).unwrap();
        assert_eq!(wm.chat_history(1), vec![format!("#5 {stag}: plain")]);
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
        // chat_tick's presence reconcile is what marks the victim exited in the
        // room; until then the room still believes it is alive.
        for w in wm.windows.iter_mut() {
            if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                s.keepalive();
            }
        }
        wm.chat_tick();
        let e = wm
            .chat_post(&term_tag(sender), "go", &[term_tag(victim)], None)
            .unwrap_err();
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

        // valid mention: targeted, arrow-rendered under the reserved sender.
        // The room owns the policy; the arrow reads off the history line.
        wm.chat_post_human(&format!("@{mtag} check the diff"))
            .expect("posted");
        assert_eq!(
            wm.chat_history(1),
            vec![format!("#2 you→{mtag}: @{mtag} check the diff")]
        );

        // unknown id: prose fallback — broadcast, text intact, no error (spec §7)
        wm.chat_post_human("@t99 anyone?").expect("posted");
        assert_eq!(wm.chat_history(1), vec!["#3 you: @t99 anyone?".to_string()]);

        // @you from the human is a self-mention: same fallback to broadcast
        wm.chat_post_human("@you hello").expect("posted");
        assert_eq!(wm.chat_history(1), vec!["#4 you: @you hello".to_string()]);
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
    fn floats_always_paint_above_tiled_windows() {
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "tiled-a");
        let b = push(&mut wm, "tiled-b");
        wm.tree.insert_root(a, Dir::Right);
        wm.tree.insert_root(b, Dir::Right);
        let f = push(&mut wm, "float-1");
        let g = push(&mut wm, "float-2");
        // Focusing a tiled window bumps its z past both floats…
        wm.focus(a);
        let ids: Vec<WinId> = wm.draw_order().iter().map(|&i| wm.windows[i].id).collect();
        // …but floats still paint last (top layer); z orders only within layers.
        assert_eq!(ids, vec![b, a, f, g]);
        // No-focus fallback lands on the true topmost window (float g), not
        // the max raw z (tile a holds a stale higher z).
        wm.focused = None;
        wm.focus_dir(Dir::Right);
        assert_eq!(wm.focused, Some(g));
        // Raise-on-focus reorders within the float layer only.
        wm.focus(f);
        let ids: Vec<WinId> = wm.draw_order().iter().map(|&i| wm.windows[i].id).collect();
        assert_eq!(ids, vec![b, a, g, f]);
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
        assert!(
            (p[0].1.width() - (1000.0 - 16.0)).abs() < 0.5,
            "b expanded to full inner width"
        );
    }

    #[test]
    fn resolve_confirmed_closes_the_target_window() {
        let mut m = WindowManager::new();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let child = WindowManager::new();
        m.push_win(7, Tab::fixed("proj", Content::Project(Box::new(child))), r);
        m.pending_close = Some(PendingClose {
            target: CloseTarget::ActiveTab(7),
            view: crate::confirm::ConfirmClose::new("t", "l", "close anyway", vec![]),
        });
        m.resolve_pending(crate::confirm::ConfirmOutcome::Confirmed);
        assert!(
            m.windows.iter().all(|w| w.id != 7),
            "window not closed on confirm"
        );
        assert!(m.pending_close.is_none());
    }

    #[test]
    fn resolve_cancelled_keeps_the_window() {
        let mut m = WindowManager::new();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let child = WindowManager::new();
        m.push_win(7, Tab::fixed("proj", Content::Project(Box::new(child))), r);
        m.pending_close = Some(PendingClose {
            target: CloseTarget::ActiveTab(7),
            view: crate::confirm::ConfirmClose::new("t", "l", "close anyway", vec![]),
        });
        m.resolve_pending(crate::confirm::ConfirmOutcome::Cancelled);
        assert!(
            m.windows.iter().any(|w| w.id == 7),
            "window closed on cancel"
        );
        assert!(m.pending_close.is_none());
    }

    #[test]
    fn deserted_is_false_while_a_close_is_pending() {
        let mut m = WindowManager::new().as_desktop();
        m.pending_close = Some(PendingClose {
            target: CloseTarget::Quit,
            view: crate::confirm::ConfirmClose::new("quit foreman?", "l", "quit anyway", vec![]),
        });
        assert!(!m.deserted(), "a pending confirm must hold the app alive");
    }

    #[test]
    fn build_confirm_wording_terminal_vs_project() {
        let proc = |pid: u32, bg: usize| crate::proc::ProcInfo {
            pid,
            name: "x.exe".into(),
            background: bg,
        };
        let g = |label: &str, procs: Vec<crate::proc::ProcInfo>| crate::confirm::ProcGroup {
            label: label.into(),
            scope: None,
            procs,
        };
        // Terminal: one top-level process with a rolled-up subtree.
        let term = build_confirm(false, vec![g("claude", vec![proc(1, 16)])]);
        assert_eq!(term.title(), "close this terminal?");
        assert!(
            term.lead()
                .contains("1 process (+16 background) still running"),
            "got: {}",
            term.lead()
        );
        // Project: two terminals → the "across" clause appears.
        let proj = build_confirm(
            true,
            vec![
                g("a", vec![proc(2, 0), proc(3, 0)]),
                g("b", vec![proc(4, 0)]),
            ],
        );
        assert_eq!(proj.title(), "close this project?");
        assert!(
            proj.lead().contains("across 2 terminals"),
            "got: {}",
            proj.lead()
        );
    }

    #[test]
    fn begin_quit_confirm_is_false_when_nothing_runs() {
        let mut m = WindowManager::new().as_desktop();
        assert!(
            !m.begin_quit_confirm(),
            "empty desktop should let the app quit"
        );
        assert!(m.pending_close.is_none());
    }

    #[test]
    fn take_quit_confirmed_reports_once_then_resets() {
        let mut m = WindowManager::new().as_desktop();
        m.quit_confirmed = true;
        assert!(m.take_quit_confirmed());
        assert!(
            !m.take_quit_confirmed(),
            "flag must reset after being taken"
        );
    }

    #[test]
    fn any_pending_close_sees_a_nested_modal() {
        let mut m = WindowManager::new().as_desktop();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        assert!(!m.any_pending_close(), "empty desktop has no modal");
        let mut child = WindowManager::new();
        child.pending_close = Some(PendingClose {
            target: CloseTarget::ActiveTab(1),
            view: crate::confirm::ConfirmClose::new("t", "l", "close anyway", vec![]),
        });
        m.push_win(7, Tab::fixed("proj", Content::Project(Box::new(child))), r);
        assert!(
            m.any_pending_close(),
            "a confirm inside a nested project must be visible app-wide"
        );
    }

    #[test]
    fn a_second_confirm_cannot_open_while_one_is_up() {
        // `app_modal` is the app-wide "a dialog is open somewhere" flag the
        // desktop threads down each frame. With it set, a close funnel elsewhere
        // must refuse — no second modal, and the target is left untouched.
        let mut m = WindowManager::new();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        m.push_win(
            7,
            Tab::fixed("proj", Content::Project(Box::new(WindowManager::new()))),
            r,
        );
        m.app_modal = true;
        m.request_close_active_tab(7);
        assert!(
            m.pending_close.is_none(),
            "guard must refuse to open a second confirm"
        );
        assert!(
            m.windows.iter().any(|w| w.id == 7),
            "the window must not close either while a dialog is up"
        );
    }

    // --- task-manager panel seams ---

    #[test]
    fn panel_model_groups_tabs_under_projects_with_state_flags() {
        let mut desk = WindowManager::new();
        let proj = push(&mut desk, "projA");
        let mut inner = WindowManager::new();
        let a = push(&mut inner, "termA");
        inner.windows[0].tabs.push(Tab::fixed(
            "termA2",
            Content::Chat(crate::chat::ChatView::new(std::rc::Rc::new(
                std::cell::RefCell::new(crate::chat::ChatRoom::new()),
            ))),
        ));
        let b = push(&mut inner, "termB");
        inner
            .windows
            .iter_mut()
            .find(|w| w.id == b)
            .unwrap()
            .minimized = true;
        inner.focus(a);
        desk.windows[0].tabs[0].content = Content::Project(Box::new(inner));
        desk.focus(proj);

        let m = desk.panel_model();
        assert_eq!(m.projects.len(), 1);
        let p = &m.projects[0];
        assert_eq!(p.title, "projA");
        assert!(p.focused && !p.minimized);
        assert_eq!(
            p.path,
            crate::panel::TargetPath {
                project: proj,
                ptab: None,
                window: None,
                tab: Some(0),
            }
        );
        assert_eq!(p.tabs.len(), 3);
        assert!(p.tabs[0].active_tab && !p.tabs[0].minimized);
        assert!(matches!(p.tabs[1].kind, crate::panel::RowKind::Chat));
        assert!(!p.tabs[1].active_tab);
        let bt = p.tabs.iter().find(|t| t.path.window == Some(b)).unwrap();
        assert!(bt.minimized);
    }

    #[test]
    fn panel_model_skips_the_panel_window_itself() {
        let mut desk = WindowManager::new();
        desk.ensure_panel(false, crate::panel::PANEL_W, Dir::Right);
        assert!(
            desk.panel_model().projects.is_empty(),
            "panel alone is not a project row"
        );
        let _ = push(&mut desk, "projA");
        let m = desk.panel_model();
        assert_eq!(m.projects.len(), 1);
        assert_eq!(m.projects[0].title, "projA");
        assert!(
            !m.projects.iter().any(|p| p.title == "Sessions"),
            "panel window must not appear as a project"
        );
    }

    #[test]
    fn surface_target_restores_and_focuses_across_levels() {
        let mut desk = WindowManager::new();
        let other = push(&mut desk, "other");
        let proj = push(&mut desk, "proj");
        let mut inner = WindowManager::new();
        let cw = push(&mut inner, "t1");
        inner.windows[0].tabs.push(Tab::fixed(
            "t2",
            Content::Chat(crate::chat::ChatView::new(std::rc::Rc::new(
                std::cell::RefCell::new(crate::chat::ChatRoom::new()),
            ))),
        ));
        inner.windows[0].minimized = true;
        desk.windows.iter_mut().find(|w| w.id == proj).unwrap().tabs[0].content =
            Content::Project(Box::new(inner));
        desk.windows
            .iter_mut()
            .find(|w| w.id == proj)
            .unwrap()
            .minimized = true;
        desk.focus(other);

        desk.surface_target(crate::panel::TargetPath {
            project: proj,
            ptab: None,
            window: Some(cw),
            tab: Some(1),
        });

        let pw = desk.windows.iter().find(|w| w.id == proj).unwrap();
        assert!(!pw.minimized, "project must be restored");
        assert_eq!(desk.focused, Some(proj), "project must take desktop focus");
        let Content::Project(inner) = &pw.tabs[0].content else {
            panic!()
        };
        let c = inner.windows.iter().find(|w| w.id == cw).unwrap();
        assert!(!c.minimized, "child window must be restored");
        assert_eq!(c.active, 1, "background tab must become active");
        assert_eq!(inner.focused, Some(cw), "child must take inner focus");
    }

    /// Builds a desktop with one project window holding an inner manager of two
    /// tiled child windows (a, b), `a` zoomed. Returns (desk, proj, a, b).
    fn zoomed_project(desk: &mut WindowManager) -> (WinId, WinId, WinId) {
        let proj = push(desk, "proj");
        let mut inner = WindowManager::new();
        inner.last_area = egui::vec2(800.0, 600.0);
        let a = push(&mut inner, "a");
        let b = push(&mut inner, "b");
        inner.tree.insert_root(a, Dir::Right);
        inner.tree.insert_root(b, Dir::Right);
        inner.toggle_zoom(a);
        assert_eq!(inner.zoomed, Some(a), "precondition: a is zoomed");
        desk.windows.iter_mut().find(|w| w.id == proj).unwrap().tabs[0].content =
            Content::Project(Box::new(inner));
        (proj, a, b)
    }

    fn inner_of(desk: &WindowManager, proj: WinId) -> &WindowManager {
        let pw = desk.windows.iter().find(|w| w.id == proj).unwrap();
        let Content::Project(inner) = &pw.tabs[0].content else {
            panic!("expected a project")
        };
        inner
    }

    #[test]
    fn surface_target_clears_a_sibling_zoom_inside_a_project() {
        // Zoom is render-order only: a zoomed sibling keeps painting full-area on
        // top, so focusing another terminal without un-zooming focuses a window
        // the user cannot see.
        let mut desk = WindowManager::new();
        let (proj, _a, b) = zoomed_project(&mut desk);

        desk.surface_target(crate::panel::TargetPath {
            project: proj,
            ptab: None,
            window: Some(b),
            tab: None,
        });

        let inner = inner_of(&desk, proj);
        assert_eq!(
            inner.zoomed, None,
            "a sibling's zoom must not cover the newly focused target"
        );
        assert_eq!(
            inner.focused,
            Some(b),
            "the clicked child takes inner focus"
        );
    }

    #[test]
    fn surface_target_keeps_the_zoom_when_the_target_is_the_zoomed_window() {
        let mut desk = WindowManager::new();
        let (proj, a, _b) = zoomed_project(&mut desk);

        desk.surface_target(crate::panel::TargetPath {
            project: proj,
            ptab: None,
            window: Some(a),
            tab: None,
        });

        let inner = inner_of(&desk, proj);
        assert_eq!(
            inner.zoomed,
            Some(a),
            "clicking the zoomed window's own row must not un-zoom it"
        );
    }

    #[test]
    fn surface_target_restores_a_floating_zoomed_siblings_rect() {
        // A floating window's rect is overwritten by the per-frame re-fit while
        // zoomed; un-zooming must round-trip it through `prev`, exactly as
        // toggle_zoom does.
        let mut desk = WindowManager::new();
        let proj = push(&mut desk, "proj");
        let mut inner = WindowManager::new();
        inner.last_area = egui::vec2(800.0, 600.0);
        let a = push(&mut inner, "a"); // floating: never tiled
        let b = push(&mut inner, "b");
        inner.tree.insert_root(b, Dir::Right);
        let home = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(300.0, 200.0));
        inner.windows.iter_mut().find(|w| w.id == a).unwrap().rect = home;
        inner.toggle_zoom(a);
        // what the per-frame re-fit does to the zoomed window
        inner.windows.iter_mut().find(|w| w.id == a).unwrap().rect =
            egui::Rect::from_min_size(egui::Pos2::ZERO, inner.last_area);
        desk.windows.iter_mut().find(|w| w.id == proj).unwrap().tabs[0].content =
            Content::Project(Box::new(inner));

        desk.surface_target(crate::panel::TargetPath {
            project: proj,
            ptab: None,
            window: Some(b),
            tab: None,
        });

        let inner = inner_of(&desk, proj);
        assert_eq!(inner.zoomed, None);
        let aw = inner.windows.iter().find(|w| w.id == a).unwrap();
        assert_eq!(
            aw.rect, home,
            "the un-zoomed floater returns to its own rect"
        );
    }

    #[test]
    fn surface_target_clears_a_zoomed_sibling_project_on_the_desktop() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1200.0, 800.0);
        let p1 = push(&mut desk, "p1");
        let p2 = push(&mut desk, "p2");
        desk.tree.insert_root(p1, Dir::Right);
        desk.tree.insert_root(p2, Dir::Right);
        desk.toggle_zoom(p1);

        desk.surface_target(crate::panel::TargetPath {
            project: p2,
            ptab: None,
            window: None,
            tab: None,
        });
        assert_eq!(
            desk.zoomed, None,
            "a zoomed project must not cover another project focused from the panel"
        );
        assert_eq!(desk.focused, Some(p2));

        desk.toggle_zoom(p2);
        desk.surface_target(crate::panel::TargetPath {
            project: p2,
            ptab: None,
            window: None,
            tab: None,
        });
        assert_eq!(
            desk.zoomed,
            Some(p2),
            "clicking the zoomed project's own row keeps the zoom"
        );
    }

    #[test]
    fn surface_target_is_a_noop_on_stale_paths() {
        let mut desk = WindowManager::new();
        let a = push(&mut desk, "a");
        desk.focus(a);
        desk.surface_target(crate::panel::TargetPath {
            project: 999,
            ptab: None,
            window: None,
            tab: None,
        });
        assert_eq!(desk.focused, Some(a), "stale path must change nothing");
    }

    fn apply_focus_path(desk: &mut WindowManager, path: crate::panel::TargetPath) {
        let ctx = egui::Context::default();
        desk.apply_acts(
            vec![Act::FocusPath(path)],
            egui::vec2(800.0, 600.0),
            egui::Id::new("panel-toggle"),
            &ctx,
        );
    }

    #[test]
    fn focus_path_minimizes_already_focused_project() {
        // Taskbar-style: click the focused project row → minimize.
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1200.0, 800.0);
        let p1 = push(&mut desk, "p1");
        let _p2 = push(&mut desk, "p2");
        desk.tree.insert_root(p1, Dir::Right);
        desk.focus(p1);

        apply_focus_path(
            &mut desk,
            crate::panel::TargetPath {
                project: p1,
                ptab: None,
                window: None,
                tab: Some(0),
            },
        );

        let w = desk.windows.iter().find(|w| w.id == p1).unwrap();
        assert!(
            w.minimized,
            "second click on the focused project minimizes it"
        );
        assert_ne!(
            desk.focused,
            Some(p1),
            "minimized project drops desktop focus"
        );
    }

    #[test]
    fn focus_path_surfaces_unfocused_project() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1200.0, 800.0);
        let p1 = push(&mut desk, "p1");
        let p2 = push(&mut desk, "p2");
        desk.tree.insert_root(p1, Dir::Right);
        desk.tree.insert_root(p2, Dir::Right);
        desk.focus(p1);

        apply_focus_path(
            &mut desk,
            crate::panel::TargetPath {
                project: p2,
                ptab: None,
                window: None,
                tab: Some(0),
            },
        );

        assert_eq!(desk.focused, Some(p2));
        assert!(
            !desk.windows.iter().find(|w| w.id == p2).unwrap().minimized,
            "unfocused project is focused, not minimized"
        );
    }

    #[test]
    fn focus_path_minimizes_already_focused_terminal() {
        let mut desk = WindowManager::new();
        let (proj, a, _b) = zoomed_project(&mut desk);
        // Drop zoom so `a` is simply the focused visible child.
        {
            let pw = desk.windows.iter_mut().find(|w| w.id == proj).unwrap();
            let Content::Project(inner) = &mut pw.tabs[0].content else {
                panic!()
            };
            inner.unzoom();
            inner.focus(a);
        }
        desk.focus(proj);

        apply_focus_path(
            &mut desk,
            crate::panel::TargetPath {
                project: proj,
                ptab: Some(0),
                window: Some(a),
                tab: Some(0),
            },
        );

        let inner = inner_of(&desk, proj);
        let aw = inner.windows.iter().find(|w| w.id == a).unwrap();
        assert!(
            aw.minimized,
            "second click on the focused terminal minimizes it"
        );
        assert_ne!(inner.focused, Some(a));
    }

    #[test]
    fn focus_path_surfaces_sibling_terminal() {
        let mut desk = WindowManager::new();
        let (proj, a, b) = zoomed_project(&mut desk);
        {
            let pw = desk.windows.iter_mut().find(|w| w.id == proj).unwrap();
            let Content::Project(inner) = &mut pw.tabs[0].content else {
                panic!()
            };
            inner.unzoom();
            inner.focus(a);
        }
        desk.focus(proj);

        apply_focus_path(
            &mut desk,
            crate::panel::TargetPath {
                project: proj,
                ptab: Some(0),
                window: Some(b),
                tab: Some(0),
            },
        );

        let inner = inner_of(&desk, proj);
        assert_eq!(inner.focused, Some(b));
        assert!(
            !inner.windows.iter().find(|w| w.id == b).unwrap().minimized,
            "sibling click focuses, does not minimize"
        );
    }

    #[test]
    fn focus_path_restores_minimized_instead_of_reminimizing() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1200.0, 800.0);
        let p1 = push(&mut desk, "p1");
        desk.tree.insert_root(p1, Dir::Right);
        desk.minimize(p1);
        assert!(desk.windows.iter().find(|w| w.id == p1).unwrap().minimized);

        apply_focus_path(
            &mut desk,
            crate::panel::TargetPath {
                project: p1,
                ptab: None,
                window: None,
                tab: Some(0),
            },
        );

        let w = desk.windows.iter().find(|w| w.id == p1).unwrap();
        assert!(!w.minimized, "clicking a minimized row restores it");
        assert_eq!(desk.focused, Some(p1));
    }

    #[test]
    fn focus_path_does_not_minimize_when_covered_by_sibling_zoom() {
        // #17 interaction: focused-but-covered must un-zoom, not minimize.
        // `zoomed_project` leaves `a` zoomed and focused; focus `b` under that
        // overlay, then panel-click `b` — user wants to *see* b, not hide it.
        let mut desk = WindowManager::new();
        let (proj, a, b) = zoomed_project(&mut desk);
        {
            let pw = desk.windows.iter_mut().find(|w| w.id == proj).unwrap();
            let Content::Project(inner) = &mut pw.tabs[0].content else {
                panic!()
            };
            // Keep a's zoom; move focus to the covered sibling.
            assert_eq!(inner.zoomed, Some(a));
            inner.focus(b);
        }
        desk.focus(proj);

        apply_focus_path(
            &mut desk,
            crate::panel::TargetPath {
                project: proj,
                ptab: Some(0),
                window: Some(b),
                tab: Some(0),
            },
        );

        let inner = inner_of(&desk, proj);
        assert_eq!(inner.zoomed, None, "sibling zoom must clear");
        assert_eq!(inner.focused, Some(b));
        assert!(
            !inner.windows.iter().find(|w| w.id == b).unwrap().minimized,
            "must not minimize a window the user could not see under zoom"
        );
    }

    #[test]
    fn focus_path_minimizes_zoomed_focused_window() {
        // The zoomed window *is* visible — second click still minimizes.
        // zoomed_project leaves focus on `b` (last push); pin focus on `a`.
        let mut desk = WindowManager::new();
        let (proj, a, _b) = zoomed_project(&mut desk);
        {
            let pw = desk.windows.iter_mut().find(|w| w.id == proj).unwrap();
            let Content::Project(inner) = &mut pw.tabs[0].content else {
                panic!()
            };
            inner.focus(a);
            assert_eq!(inner.zoomed, Some(a));
        }
        desk.focus(proj);

        apply_focus_path(
            &mut desk,
            crate::panel::TargetPath {
                project: proj,
                ptab: Some(0),
                window: Some(a),
                tab: Some(0),
            },
        );

        let inner = inner_of(&desk, proj);
        assert!(
            inner.windows.iter().find(|w| w.id == a).unwrap().minimized,
            "clicking the zoomed focused row minimizes it"
        );
        assert_eq!(inner.zoomed, None, "minimize detaches and clears zoom");
    }

    #[test]
    fn focus_path_switches_background_tab_instead_of_minimizing() {
        let mut desk = WindowManager::new();
        let proj = push(&mut desk, "proj");
        let mut inner = WindowManager::new();
        let cw = push(&mut inner, "t1");
        inner.windows[0].tabs.push(Tab::fixed(
            "t2",
            Content::Chat(crate::chat::ChatView::new(std::rc::Rc::new(
                std::cell::RefCell::new(crate::chat::ChatRoom::new()),
            ))),
        ));
        inner.windows[0].active = 0;
        desk.windows.iter_mut().find(|w| w.id == proj).unwrap().tabs[0].content =
            Content::Project(Box::new(inner));
        desk.focus(proj);

        apply_focus_path(
            &mut desk,
            crate::panel::TargetPath {
                project: proj,
                ptab: Some(0),
                window: Some(cw),
                tab: Some(1),
            },
        );

        let inner = inner_of(&desk, proj);
        let c = inner.windows.iter().find(|w| w.id == cw).unwrap();
        assert_eq!(c.active, 1, "background tab becomes active");
        assert!(!c.minimized, "tab switch is not a minimize");
        assert_eq!(inner.focused, Some(cw));
    }

    #[test]
    fn ensure_panel_is_idempotent_and_tiled_right() {
        let mut desk = WindowManager::new();
        desk.ensure_panel(false, crate::panel::PANEL_W, Dir::Right);
        desk.ensure_panel(false, crate::panel::PANEL_W, Dir::Right);
        let panels: Vec<_> = desk.windows.iter().filter(|w| w.is_panel()).collect();
        assert_eq!(panels.len(), 1);
        assert!(desk.tree.contains(panels[0].id), "panel starts tiled");
    }

    #[test]
    fn deserted_ignores_the_panel() {
        let mut desk = WindowManager::new();
        desk.ensure_panel(false, crate::panel::PANEL_W, Dir::Right);
        assert!(desk.deserted(), "a lone panel must not hold the app alive");
        let p = push(&mut desk, "proj");
        assert!(!desk.deserted());
        desk.close(p);
        assert!(desk.deserted());
    }

    #[test]
    fn panel_refuses_close_and_minimize() {
        let mut desk = WindowManager::new();
        desk.ensure_panel(false, crate::panel::PANEL_W, Dir::Right);
        let id = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        desk.close(id);
        desk.minimize(id);
        let w = desk.windows.iter().find(|w| w.id == id).unwrap();
        assert!(!w.minimized);
        assert_eq!(desk.windows.iter().filter(|w| w.is_panel()).count(), 1);
    }

    #[test]
    fn apply_panel_ratio_pins_width_for_a_right_docked_panel() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 300.0, Dir::Right);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        desk.tree.insert_root(999, Dir::Left); // [other | panel]
        desk.apply_panel_ratio(1000.0);
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, desk.last_area);
        let r = |t: &crate::layout::LayoutTree| {
            t.layout(local, SNAP_GAP)
                .into_iter()
                .find(|(w, _)| *w == pid)
                .unwrap()
                .1
        };
        let w = r(&desk.tree).width();
        assert!((w - 300.0).abs() < 0.5, "expanded got {w}");
        desk.toggle_panel();
        let w = r(&desk.tree).width();
        assert!((w - crate::panel::RAIL_W).abs() < 0.5, "collapsed got {w}");
    }

    #[test]
    fn apply_panel_ratio_pins_height_for_a_bottom_docked_panel() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 220.0, Dir::Down);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        // Re-dock by hand: another leaf on top, panel across the bottom.
        desk.tree = crate::layout::LayoutTree::default();
        desk.tree.insert_root(999, Dir::Right);
        desk.tree.insert_root(pid, Dir::Down);
        desk.apply_panel_ratio(1000.0);
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, desk.last_area);
        let r = |t: &crate::layout::LayoutTree| {
            t.layout(local, SNAP_GAP)
                .into_iter()
                .find(|(w, _)| *w == pid)
                .unwrap()
                .1
        };
        let h = r(&desk.tree).height();
        assert!((h - 220.0).abs() < 0.5, "expanded got {h}");
        desk.toggle_panel();
        let h = r(&desk.tree).height();
        assert!((h - crate::panel::RAIL_W).abs() < 0.5, "collapsed got {h}");
    }

    #[test]
    fn bottom_dock_caps_expanded_height_at_panel_max_edge() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1000.0, 800.0);
        // Stale/large height from before the cap (or a fat divider drag).
        desk.ensure_panel(false, 500.0, Dir::Down);
        assert!(
            desk.panel_prefs().unwrap().1 <= crate::panel::PANEL_MAX_EDGE + 0.5,
            "constructor should hard-cap stored width"
        );
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        desk.tree = crate::layout::LayoutTree::default();
        desk.tree.insert_root(999, Dir::Right);
        desk.tree.insert_root(pid, Dir::Down);
        // Force a bloated preference as if settings.json still had 500.
        for win in &mut desk.windows {
            for t in &mut win.tabs {
                if let Content::TaskManager(v) = &mut t.content {
                    v.expanded_width = 500.0;
                }
            }
        }
        desk.apply_panel_ratio(1000.0);
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, desk.last_area);
        let h = desk
            .tree
            .layout(local, SNAP_GAP)
            .into_iter()
            .find(|(w, _)| *w == pid)
            .unwrap()
            .1
            .height();
        assert!(
            (h - crate::panel::PANEL_MAX_EDGE).abs() < 0.5,
            "bottom dock must cap at PANEL_MAX_EDGE, got {h}"
        );
        assert!(
            desk.panel_prefs().unwrap().1 <= crate::panel::PANEL_MAX_EDGE + 0.5,
            "stored preference must be rewound to the cap"
        );

        // Sole-leaf strip (landing) must use the same cap.
        desk.tree = crate::layout::LayoutTree::default();
        desk.tree.insert_root(pid, Dir::Right);
        let strip = desk.panel_strip_local(desk.last_area).unwrap();
        assert!(
            (strip.height() - crate::panel::PANEL_MAX_EDGE).abs() < 0.5,
            "sole strip height got {}",
            strip.height()
        );
    }

    #[test]
    fn sync_panel_width_persists_height_for_a_horizontal_panel() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 300.0, Dir::Down);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        desk.tree = crate::layout::LayoutTree::default();
        desk.tree.insert_root(999, Dir::Right);
        desk.tree.insert_root(pid, Dir::Down);
        // Post-layout rect of a bottom-docked panel after a divider drag
        // (under PANEL_MAX_EDGE so the cap is not the thing under test).
        if let Some(w) = desk.windows.iter_mut().find(|w| w.id == pid) {
            w.rect = egui::Rect::from_min_size(egui::pos2(0.0, 580.0), egui::vec2(1000.0, 220.0));
        }
        desk.sync_panel_width_from_layout();
        assert_eq!(
            desk.panel_prefs().unwrap(),
            (false, 220.0, Dir::Down),
            "horizontal dock must persist height, not width"
        );
    }

    #[test]
    fn unminimize_keeps_a_bottom_docked_panel_on_the_bottom() {
        // Minimize every project while the Sessions panel is bottom-docked;
        // restoring must not shove the panel back to the right rail.
        let mut desk = WindowManager::new().as_desktop();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 200.0, Dir::Down);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        let proj = push(&mut desk, "proj");
        // Bottom-docked: project on top, panel across the bottom.
        desk.tree = crate::layout::LayoutTree::default();
        desk.tree.insert_root(proj, Dir::Right);
        desk.tree.insert_root(pid, Dir::Down);
        desk.apply_panel_ratio(1000.0);
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, desk.last_area);
        for (w, r) in desk.tree.layout(local, SNAP_GAP) {
            desk.windows
                .iter_mut()
                .find(|win| win.id == w)
                .unwrap()
                .rect = r;
        }
        desk.sync_panel_dock_from_layout();
        assert_eq!(desk.panel_dock(), Dir::Down);

        desk.minimize(proj);
        assert!(!desk.tree.contains(proj));
        assert!(desk.tree.contains(pid));
        assert_eq!(
            desk.panel_dock(),
            Dir::Down,
            "dock survives sole-leaf collapse"
        );

        desk.surface_target(crate::panel::TargetPath {
            project: proj,
            ptab: None,
            window: None,
            tab: None,
        });
        assert!(desk.tree.contains(proj));
        assert!(
            desk.tree.has_divider(pid, Dir::Up),
            "panel should still sit under a vertical split (bottom dock)"
        );
        assert!(
            !desk.tree.has_divider(pid, Dir::Left),
            "panel must not have been re-docked to the right rail"
        );
        assert_eq!(desk.panel_dock(), Dir::Down);
        // insert_split starts 50/50 — restore must re-pin the remembered height.
        let h = desk
            .tree
            .layout(local, SNAP_GAP)
            .into_iter()
            .find(|(w, _)| *w == pid)
            .unwrap()
            .1
            .height();
        assert!(
            (h - 200.0).abs() < 0.5,
            "panel height must stay 200 after first unminimize, got {h}"
        );
    }

    #[test]
    fn moving_panel_to_bottom_edge_keeps_remembered_extent() {
        // Tear the panel out of a right dock and re-root it at the bottom —
        // the same tree mutation a drag-drop to the area edge uses. Extent
        // must stay `expanded_width` (height once bottom-docked), not 50/50.
        let mut desk = WindowManager::new().as_desktop();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 200.0, Dir::Right);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        let proj = push(&mut desk, "proj");
        desk.tree = crate::layout::LayoutTree::default();
        desk.tree.insert_root(proj, Dir::Right);
        desk.tree.insert_root(pid, Dir::Right); // [proj | panel]
        desk.apply_panel_ratio(1000.0);

        desk.tree.remove(pid);
        desk.tree.insert_root(pid, Dir::Down);
        desk.repin_panel();

        assert_eq!(desk.panel_dock(), Dir::Down);
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, desk.last_area);
        let h = desk
            .tree
            .layout(local, SNAP_GAP)
            .into_iter()
            .find(|(w, _)| *w == pid)
            .unwrap()
            .1
            .height();
        assert!(
            (h - 200.0).abs() < 0.5,
            "bottom re-root must keep extent 200, got {h} (50/50 would be ~400)"
        );
    }

    #[test]
    fn swapping_panel_with_neighbor_re_pins_extent() {
        let mut desk = WindowManager::new().as_desktop();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 260.0, Dir::Right);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        let proj = push(&mut desk, "proj");
        desk.tree = crate::layout::LayoutTree::default();
        desk.tree.insert_root(proj, Dir::Right);
        desk.tree.insert_root(pid, Dir::Right);
        desk.apply_panel_ratio(1000.0);
        // Fake unequal ratios so a bare swap would give the panel ~700px.
        // (insert leaves 0.5/0.5; set_leaf_extent already made panel 260 —
        // swap with a fat sibling still needs re-pin after move_dir.)
        desk.focused = Some(pid);
        desk.move_dir(Dir::Left); // swap with proj → panel would take proj's fat slot
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, desk.last_area);
        let w = desk
            .tree
            .layout(local, SNAP_GAP)
            .into_iter()
            .find(|(wid, _)| *wid == pid)
            .unwrap()
            .1
            .width();
        assert!(
            (w - 260.0).abs() < 0.5,
            "after swap panel must re-pin to 260, got {w}"
        );
    }

    #[test]
    fn unminimize_keeps_right_docked_panel_width() {
        let mut desk = WindowManager::new().as_desktop();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 260.0, Dir::Right);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        let proj = push(&mut desk, "proj");
        desk.tree = crate::layout::LayoutTree::default();
        desk.tree.insert_root(proj, Dir::Right);
        desk.tree.insert_root(pid, Dir::Right); // [proj | panel]
        desk.apply_panel_ratio(1000.0);
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, desk.last_area);
        for (w, r) in desk.tree.layout(local, SNAP_GAP) {
            desk.windows
                .iter_mut()
                .find(|win| win.id == w)
                .unwrap()
                .rect = r;
        }
        let before = desk
            .tree
            .layout(local, SNAP_GAP)
            .into_iter()
            .find(|(w, _)| *w == pid)
            .unwrap()
            .1
            .width();
        assert!((before - 260.0).abs() < 0.5, "setup width {before}");

        desk.minimize(proj);
        desk.surface_target(crate::panel::TargetPath {
            project: proj,
            ptab: None,
            window: None,
            tab: None,
        });
        let after = desk
            .tree
            .layout(local, SNAP_GAP)
            .into_iter()
            .find(|(w, _)| *w == pid)
            .unwrap()
            .1
            .width();
        assert!(
            (after - 260.0).abs() < 0.5,
            "panel width must stay 260 after first unminimize, got {after} (50/50 would be ~500)"
        );
    }

    #[test]
    fn tile_new_beside_a_sole_bottom_panel_keeps_bottom_dock() {
        let mut desk = WindowManager::new().as_desktop();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 200.0, Dir::Down);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        let proj = push(&mut desk, "proj");
        desk.tile_new(proj, None);
        assert!(desk.tree.has_divider(pid, Dir::Up));
        assert!(!desk.tree.has_divider(pid, Dir::Left));
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, desk.last_area);
        let h = desk
            .tree
            .layout(local, SNAP_GAP)
            .into_iter()
            .find(|(w, _)| *w == pid)
            .unwrap()
            .1
            .height();
        assert!(
            (h - 200.0).abs() < 0.5,
            "tile_new against sole panel must re-pin height, got {h}"
        );
    }

    #[test]
    fn should_show_landing_when_all_projects_are_minimized() {
        let mut desk = WindowManager::new();
        desk.ensure_panel(false, crate::panel::PANEL_W, Dir::Right);
        assert!(
            desk.should_show_landing(),
            "panel alone is an empty visible desktop"
        );
        assert!(desk.deserted());

        let proj = push(&mut desk, "proj");
        assert!(!desk.should_show_landing());
        assert!(!desk.deserted());

        desk.minimize(proj);
        assert!(
            desk.should_show_landing(),
            "all-minimized must show landing"
        );
        assert!(
            !desk.deserted(),
            "minimized projects still exist — must not quit"
        );
        assert!(!desk.has_visible_project());
    }

    #[test]
    fn panel_strip_local_keeps_remembered_extent_on_sole_leaf() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 220.0, Dir::Down);
        let strip = desk.panel_strip_local(desk.last_area).unwrap();
        assert!(
            (strip.height() - 220.0).abs() < 0.5,
            "bottom strip height got {}",
            strip.height()
        );
        assert!(
            (strip.width() - 1000.0).abs() < 0.5,
            "bottom strip spans full width"
        );
        assert!((strip.min.y - (800.0 - 220.0)).abs() < 0.5);

        // Right dock: pin width, full height.
        if let Some(w) = desk.windows.iter_mut().find(|w| w.is_panel()) {
            if let Content::TaskManager(v) = &mut w.tabs[0].content {
                v.dock = Dir::Right;
                v.expanded_width = 260.0;
            }
        }
        let strip = desk.panel_strip_local(desk.last_area).unwrap();
        assert!((strip.width() - 260.0).abs() < 0.5);
        assert!((strip.height() - 800.0).abs() < 0.5);
    }

    #[test]
    fn landing_content_rect_leaves_panel_strip() {
        let mut desk = WindowManager::new();
        desk.ensure_panel(false, 200.0, Dir::Right);
        let area = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(1000.0, 800.0));
        let content = desk.landing_content_rect(area);
        assert!(
            (content.width() - 800.0).abs() < 0.5,
            "got {}",
            content.width()
        );
        assert!((content.max.x - (area.max.x - 200.0)).abs() < 0.5);
        assert_eq!(content.min, area.min);
        assert_eq!(content.max.y, area.max.y);
    }

    #[test]
    fn sync_panel_width_does_not_inflate_on_sole_bottom_strip() {
        let mut desk = WindowManager::new();
        desk.last_area = egui::vec2(1000.0, 800.0);
        desk.ensure_panel(false, 180.0, Dir::Down);
        let pid = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
        // Simulate the sole-leaf strip placement (wide × short).
        let strip = desk.panel_strip_local(desk.last_area).unwrap();
        if let Some(w) = desk.windows.iter_mut().find(|w| w.id == pid) {
            w.rect = strip;
        }
        desk.sync_panel_width_from_layout();
        assert_eq!(
            desk.panel_prefs().unwrap(),
            (false, 180.0, Dir::Down),
            "sole bottom strip must not adopt full desktop width as extent"
        );
    }

    #[test]
    fn a_modal_freezes_background_mouse_acts() {
        // While a confirm is up, clicking a sibling tab / dragging a merge / hitting
        // minimize must NOT take effect — otherwise Confirm could close the wrong
        // tab, or a minimize could hide the modal while the whole app stays frozen.
        let ctx = egui::Context::default();
        let mut m = WindowManager::new();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        m.push_win(
            1,
            Tab::fixed("a", Content::Project(Box::new(WindowManager::new()))),
            r,
        );
        m.push_win(
            2,
            Tab::fixed("b", Content::Project(Box::new(WindowManager::new()))),
            r,
        );
        m.focus(1);
        m.app_modal = true;
        m.apply_acts(
            vec![
                Act::Focus(2),
                Act::Min(1),
                Act::FocusPath(crate::panel::TargetPath {
                    project: 2,
                    ptab: None,
                    window: None,
                    tab: None,
                }),
                Act::MinPath(crate::panel::TargetPath {
                    project: 1,
                    ptab: None,
                    window: None,
                    tab: None,
                }),
            ],
            egui::vec2(0.0, 0.0),
            egui::Id::new("t"),
            &ctx,
        );
        assert_eq!(
            m.focused,
            Some(1),
            "focus change must be dropped under a modal"
        );
        assert!(
            !m.windows.iter().find(|w| w.id == 1).unwrap().minimized,
            "minimize must be dropped under a modal (no soft-lock)"
        );
        // Once the modal clears, the same act applies again.
        m.app_modal = false;
        m.apply_acts(
            vec![Act::Focus(2)],
            egui::vec2(0.0, 0.0),
            egui::Id::new("t"),
            &ctx,
        );
        assert_eq!(m.focused, Some(2));
    }

    #[test]
    fn a_close_confirm_wont_open_over_another_overlay() {
        // A rename (like the picker/settings) owns the keyboard — a close-confirm
        // must not stack on top, or the two overlays fight over one Enter/Esc.
        let mut m = WindowManager::new();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        m.push_win(
            7,
            Tab::fixed("proj", Content::Project(Box::new(WindowManager::new()))),
            r,
        );
        m.renaming = Some(7);
        m.request_close_active_tab(7);
        assert!(
            m.pending_close.is_none(),
            "must not stack a confirm over a rename"
        );
        assert!(
            m.windows.iter().any(|w| w.id == 7),
            "and must not close the window"
        );
    }
}
