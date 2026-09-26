//! Read-only Git history: a demand-driven Git stream, pure lane layout, and virtualized native rows.
use eframe::egui;
mod changes;
mod details;
mod diff;
mod diff_view;
mod file_tree;
mod git;
mod scope;
pub use changes::ChangesView;
pub use diff_view::{DiffTarget, DiffView, Stage};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

const BATCH: usize = 512;
const ROW_H: f32 = 28.0;
const LANE_W: f32 = 16.0;
/// Unscaled timeline column widths: the most an author name may take, the
/// spare room over which a metadata column fades out as the subject nears
/// it, the gap after the subject, the tighter gap between name and date, and
/// the subject room a lane graph wider than the pane leaves before the
/// timeline scrolls sideways.
const AUTHOR_W: f32 = 140.0;
const FADE_W: f32 = 32.0;
/// Seconds the subject column takes to slide back left after a wide
/// stretch of graph scrolls off screen.
const EASE_S: f32 = 0.15;
const COL_GAP: f32 = 12.0;
const NAME_GAP: f32 = 6.0;
const MIN_SUBJECT_W: f32 = 120.0;
/// Branch counts above this show a filter field in the scope dropdown.
const FILTER_MIN: usize = 8;
/// Unscaled height of the scope dropdown's menu, fixed from its first frame.
/// The combo's scroll area auto-shrinks to the smaller of last frame's room
/// and this frame's content, so a menu that opens on a spinner and then
/// fills with branches stays spinner-sized and clips every branch row.
const MENU_H: f32 = 360.0;
const COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(116, 176, 164),
    egui::Color32::from_rgb(231, 169, 63),
    egui::Color32::from_rgb(123, 164, 218),
    egui::Color32::from_rgb(205, 133, 161),
    egui::Color32::from_rgb(162, 173, 104),
    egui::Color32::from_rgb(174, 150, 208),
];

#[derive(Debug)]
struct Commit {
    hash: String,
    parents: Vec<String>,
    refs: String,
    author: String,
    date: String,
    subject: String,
}
#[derive(Debug, PartialEq)]
struct Edge {
    from: usize,
    to: usize,
    color: usize,
}
#[derive(Debug)]
struct Row {
    commit: Commit,
    lane: usize,
    color: usize,
    incoming: Vec<(usize, usize)>,
    outgoing: Vec<Edge>,
    width: usize,
}
#[derive(Default)]
struct Graph {
    lanes: Vec<(String, usize)>,
    next_color: usize,
}
impl Graph {
    // Before/after frontiers name commits still to be visited. Persistent lanes
    // keep their color when compacted; each parent has exactly one frontier slot.
    fn push(&mut self, commit: Commit) -> Row {
        let existing = self.lanes.iter().position(|(h, _)| h == &commit.hash);
        let lane = existing.unwrap_or_else(|| {
            let lane = self.lanes.len();
            self.lanes.push((commit.hash.clone(), self.next_color));
            self.next_color += 1;
            lane
        });
        let color = self.lanes[lane].1;
        let incoming = self
            .lanes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != lane || existing.is_some())
            .map(|(i, (_, c))| (i, *c))
            .collect();
        let before = self.lanes.clone();
        self.lanes.remove(lane);
        for (i, parent) in commit.parents.iter().enumerate() {
            if !self.lanes.iter().any(|(h, _)| h == parent) {
                let c = if i == 0 {
                    color
                } else {
                    let c = self.next_color;
                    self.next_color += 1;
                    c
                };
                self.lanes
                    .insert((lane + i).min(self.lanes.len()), (parent.clone(), c));
            }
        }
        let mut outgoing = Vec::new();
        for (from, (hash, c)) in before.iter().enumerate() {
            if from != lane {
                let to = self.lanes.iter().position(|(h, _)| h == hash).unwrap();
                outgoing.push(Edge {
                    from,
                    to,
                    color: *c,
                });
            }
        }
        for parent in &commit.parents {
            let to = self.lanes.iter().position(|(h, _)| h == parent).unwrap();
            outgoing.push(Edge {
                from: lane,
                to,
                color: self.lanes[to].1,
            });
        }
        Row {
            width: before.len().max(self.lanes.len()),
            commit,
            lane,
            color,
            incoming,
            outgoing,
        }
    }
}

fn read_commit(reader: &mut impl BufRead) -> Result<Option<Commit>, String> {
    let mut fields = Vec::with_capacity(6);
    for i in 0..6 {
        let mut bytes = Vec::new();
        let n = reader
            .take(1024 * 1024)
            .read_until(0, &mut bytes)
            .map_err(|e| e.to_string())?;
        if n == 0 && i == 0 {
            return Ok(None);
        }
        if bytes.pop() != Some(0) {
            return Err("Git returned an incomplete or oversized history record".into());
        }
        fields.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    let mut f = fields.into_iter();
    Ok(Some(Commit {
        hash: f.next().unwrap(),
        parents: f
            .next()
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        refs: f.next().unwrap(),
        author: f.next().unwrap(),
        date: f.next().unwrap(),
        subject: f.next().unwrap(),
    }))
}

struct Page {
    rows: Vec<Row>,
    end: bool,
    error: Option<String>,
    /// The scope as the worker resolved it; set on a stream's first page.
    resolved: Option<scope::Resolved>,
}
struct Stream {
    next: mpsc::SyncSender<()>,
    pages: mpsc::Receiver<Page>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl Stream {
    fn start(cwd: PathBuf, scope: scope::Scope, ctx: egui::Context) -> Self {
        let (next, requests) = mpsc::sync_channel(1);
        let (tx, pages) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let result = stream_history(cwd, scope, &requests, &tx, &stop, &ctx);
            if let Err(error) = result {
                let _ = tx.send(Page {
                    rows: Vec::new(),
                    end: true,
                    error: Some(error),
                    resolved: None,
                });
                ctx.request_repaint();
            }
        });
        Self {
            next,
            pages,
            cancel,
        }
    }
}

fn stream_history(
    cwd: PathBuf,
    scope: scope::Scope,
    requests: &mpsc::Receiver<()>,
    tx: &mpsc::SyncSender<Page>,
    cancel: &Arc<AtomicBool>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let resolved = scope::resolve(&cwd, scope, cancel)?;
    if resolved.revisions.is_empty() {
        // An unborn HEAD: nothing to walk, and `git log` would fail on it.
        let _ = tx.send(Page {
            rows: Vec::new(),
            end: true,
            error: None,
            resolved: Some(resolved),
        });
        ctx.request_repaint();
        return Ok(());
    }
    let revisions = resolved.revisions.clone();
    let mut args = vec![
        "log",
        "--topo-order",
        "--decorate=short",
        "--no-color",
        "--no-patch",
        "--encoding=UTF-8",
        "--no-show-signature",
        "-z",
        "--format=%H%x00%P%x00%D%x00%an%x00%as%x00%s",
    ];
    args.extend(revisions.iter().map(String::as_str));
    args.push("--");
    let (stdout, exit) = git::spawn(&cwd, &args, cancel, None).map_err(|e| e.to_string())?;
    let mut resolved = Some(resolved);
    let mut reader = BufReader::new(stdout);
    let mut exit = Some(exit);
    let mut graph = Graph::default();
    let result = (|| {
        while requests.recv().is_ok() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let mut rows = Vec::with_capacity(BATCH);
            let mut end = false;
            for _ in 0..BATCH {
                match read_commit(&mut reader)? {
                    Some(commit) => rows.push(graph.push(commit)),
                    None => {
                        end = true;
                        break;
                    }
                }
            }
            let error = if end {
                match exit.take().expect("the stream ends once").finish() {
                    Ok(()) => None,
                    Err(git::GitError::Failed(stderr)) if stderr.is_empty() => {
                        Some("Git history could not be read".into())
                    }
                    Err(e) => Some(e.to_string()),
                }
            } else {
                None
            };
            if tx
                .send(Page {
                    rows,
                    end,
                    error,
                    resolved: resolved.take(),
                })
                .is_err()
            {
                break;
            }
            ctx.request_repaint();
            if end {
                break;
            }
        }
        Ok(())
    })();
    cancel.store(true, Ordering::Relaxed);
    result
}

/// Intents from the Git History or Git Changes window that change sibling
/// windows; drained by `WindowManager::drain_history_acts` after the draw pass.
pub enum HistoryAct {
    OpenDiff(DiffTarget),
}

pub struct HistoryView {
    /// Recorded during `show`; the owning manager drains them after the draw.
    pub acts: Vec<HistoryAct>,
    details: details::DetailsView,
    cwd: Option<PathBuf>,
    stream: Option<Stream>,
    pages: Vec<Vec<Row>>,
    count: usize,
    pending: bool,
    end: bool,
    error: Option<String>,
    /// Graph room shown, in lanes: the widest row on screen, eased down.
    lanes: f32,
    shrink_rate: f32,
    generation: u64,
    /// Theme font size / default, applied to every px dimension below.
    scale: f32,
    /// Last vertical scroll offset, so a zoom keeps the same rows in view.
    scroll_y: f32,
    /// Dragged details pane width in unscaled px; `None` until the first drag.
    /// Clamped when drawn but only written on drag, so a shrink-then-regrow of
    /// the window restores it. Not persisted across restart.
    details_w: Option<f32>,
    /// What the timeline walks. Survives Refresh; a new window starts on
    /// `Current`. Updated from the worker, which may fall back to `Current`.
    scope: scope::Scope,
    /// The resolved scope's name for the header; `None` until the first page.
    label: Option<String>,
    /// The dropdown's branch list, read on a worker each time it opens.
    branches: Option<Result<scope::Branches, String>>,
    branch_rx: Option<mpsc::Receiver<Result<scope::Branches, String>>>,
    /// Whether the dropdown was open last frame, to spot it opening.
    popup_open: bool,
    filter: String,
    #[cfg(test)]
    drawn: std::ops::Range<usize>,
    #[cfg(test)]
    drawn_details_w: f32,
    #[cfg(test)]
    drawn_graph_w: f32,
    #[cfg(test)]
    header_button_h: f32,
    #[cfg(test)]
    refresh_btn: egui::Rect,
    #[cfg(test)]
    scope_rows: Vec<(String, egui::Rect)>,
    #[cfg(test)]
    scope_btn: egui::Rect,
    #[cfg(test)]
    filter_rect: Option<egui::Rect>,
}
impl Drop for HistoryView {
    fn drop(&mut self) {
        // Closing a large history must not free every row on the GUI thread.
        let stream = self.stream.take();
        if let Some(stream) = &stream {
            stream.cancel.store(true, Ordering::Relaxed);
        }
        let pages = std::mem::take(&mut self.pages);
        if stream.is_some() || !pages.is_empty() {
            std::thread::spawn(move || {
                drop(stream);
                drop(pages);
            });
        }
    }
}
impl HistoryView {
    pub fn new(cwd: Option<PathBuf>) -> Self {
        Self {
            acts: Vec::new(),
            details: details::DetailsView::default(),
            cwd,
            stream: None,
            pages: Vec::new(),
            count: 0,
            pending: false,
            end: false,
            error: None,
            lanes: 1.0,
            shrink_rate: 0.0,
            generation: 0,
            scale: 1.0,
            scroll_y: 0.0,
            details_w: None,
            scope: scope::Scope::default(),
            label: None,
            branches: None,
            branch_rx: None,
            popup_open: false,
            filter: String::new(),
            #[cfg(test)]
            drawn: 0..0,
            #[cfg(test)]
            drawn_details_w: 0.0,
            #[cfg(test)]
            drawn_graph_w: 0.0,
            #[cfg(test)]
            header_button_h: 0.0,
            #[cfg(test)]
            refresh_btn: egui::Rect::NOTHING,
            #[cfg(test)]
            scope_rows: Vec::new(),
            #[cfg(test)]
            scope_btn: egui::Rect::NOTHING,
            #[cfg(test)]
            filter_rect: None,
        }
    }
    fn request(&mut self) {
        if !self.pending
            && !self.end
            && let Some(stream) = &self.stream
        {
            if stream.next.try_send(()).is_ok() {
                self.pending = true;
            }
        }
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if self.stream.is_none() && !self.end {
            if let Some(cwd) = &self.cwd {
                self.stream = Some(Stream::start(cwd.clone(), self.scope.clone(), ctx.clone()));
                self.request();
            } else {
                self.error = Some("This project has no directory".into());
                self.end = true;
            }
        }
        if let Some(stream) = &self.stream {
            match stream.pages.try_recv() {
                Ok(page) => {
                    self.pending = false;
                    self.end = page.end;
                    self.error = page.error;
                    if let Some(resolved) = page.resolved {
                        self.scope = resolved.scope;
                        self.label = Some(resolved.label);
                    }
                    self.count += page.rows.len();
                    if !page.rows.is_empty() {
                        self.pages.push(page.rows);
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) if !self.end => {
                    self.pending = false;
                    self.end = true;
                    self.error = Some("Git history worker stopped".into());
                }
                _ => {}
            }
        }
        if let Some(rx) = &self.branch_rx
            && let Ok(branches) = rx.try_recv()
        {
            self.branches = Some(branches);
            self.branch_rx = None;
        }
    }
    /// Start over on `scope`. Retires the old rows off the GUI thread and
    /// keeps the details pane width. A scope change keeps the selected
    /// commit (it is still a valid commit); Refresh clears it.
    fn restart(&mut self, ctx: &egui::Context, scope: scope::Scope, keep_selection: bool) {
        let cwd = self.cwd.clone();
        let generation = self.generation + 1;
        let mut old = std::mem::replace(self, Self::new(cwd));
        self.details_w = old.details_w;
        self.scope = scope;
        // The old name stays until the worker resolves the new scope.
        self.label = old.label.take();
        if keep_selection {
            std::mem::swap(&mut self.details, &mut old.details);
        }
        if let Some(stream) = &old.stream {
            stream.cancel.store(true, Ordering::Relaxed);
        }
        std::thread::spawn(move || drop(old));
        self.generation = generation;
        ctx.request_repaint();
    }

    fn load_branches(&mut self, ctx: &egui::Context) {
        self.filter.clear();
        self.branch_rx = None;
        let Some(cwd) = self.cwd.clone() else {
            self.branches = Some(Err("This project has no directory".into()));
            return;
        };
        self.branches = None;
        let (tx, rx) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(scope::branches(&cwd, &Arc::new(AtomicBool::new(false))));
            ctx.request_repaint();
        });
        self.branch_rx = Some(rx);
    }

    /// The dropdown's rows; returns the scope the human clicked.
    fn scope_menu(&mut self, ui: &mut egui::Ui) -> Option<scope::Scope> {
        use scope::Scope;
        let th = crate::theme::live(ui.ctx());
        let s = crate::view_scale::ViewScale::from_ctx(ui.ctx()).factor();
        ui.set_min_width(220.0 * s);
        ui.set_min_height(MENU_H * s);
        #[cfg(test)]
        {
            self.scope_rows.clear();
            self.filter_rect = None;
        }
        let mut pick = None;
        let current = match &self.branches {
            Some(Ok(b)) => scope::current_row(b.head.as_deref(), b.upstream.as_deref()),
            _ => "Current branch".to_string(),
        };
        for (scope, text) in [
            (Scope::Current, current),
            (Scope::Local, "Local branches".to_string()),
            (Scope::All, "All".to_string()),
        ] {
            let response = menu_row(ui, self.scope == scope, &text);
            #[cfg(test)]
            self.scope_rows.push((text.clone(), response.rect));
            if response.clicked() {
                pick = Some(scope);
            }
        }
        ui.separator();
        let branches = self.branches.take();
        match &branches {
            None => {
                ui.spinner();
            }
            Some(Err(error)) => {
                ui.colored_label(th.dim, error);
            }
            Some(Ok(b)) => {
                if b.local.len() + b.cards.len() + b.remote.len() > FILTER_MIN {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("filter…")
                            .desired_width(f32::INFINITY),
                    );
                    #[cfg(test)]
                    {
                        self.filter_rect = Some(field.rect);
                    }
                    #[cfg(not(test))]
                    let _ = field;
                }
                let needle = self.filter.to_lowercase();
                for (heading, refs) in [
                    ("LOCAL", &b.local),
                    ("CARDS", &b.cards),
                    ("REMOTE", &b.remote),
                ] {
                    let shown: Vec<&String> = refs
                        .iter()
                        .filter(|r| scope::short(r).to_lowercase().contains(&needle))
                        .collect();
                    if shown.is_empty() {
                        continue;
                    }
                    ui.label(egui::RichText::new(heading).small().color(th.dim));
                    for r in shown {
                        let scope = Scope::Branch(r.clone());
                        let response = menu_row(ui, self.scope == scope, scope::short(r));
                        #[cfg(test)]
                        self.scope_rows
                            .push((scope::short(r).to_string(), response.rect));
                        if response.clicked() {
                            pick = Some(scope);
                        }
                    }
                }
            }
        }
        self.branches = branches;
        if pick.is_some() {
            ui.close();
        }
        pick
    }
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        self.poll(ui.ctx());
        let th = crate::theme::live(ui.ctx());
        let zoom = crate::view_scale::ViewScale::from_ctx(ui.ctx());
        let s = zoom.factor();
        let rescroll = (s != self.scale).then(|| self.scroll_y * s / self.scale);
        self.scale = s;
        ui.painter().rect_filled(rect, 0.0, th.bg);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base)
                .max_rect(rect.shrink(zoom.px(8.0)))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        zoom.apply(&mut child);
        let mut refresh = false;
        let mut pick = None;
        child.horizontal(|ui| {
            let label = self.label.clone().unwrap_or_else(|| "…".into());
            let combo = egui::ComboBox::from_id_salt(base.with("scope"))
                .selected_text(egui::RichText::new(label).color(th.text).strong())
                // Clicks in the filter field must not close it; rows close it
                // themselves with `ui.close()`.
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                // The combo's own scroll area holds the whole list; a second
                // `ScrollArea` nested inside it collapses to a sliver.
                .height(MENU_H * s)
                .popup_style(zoom.popup_style())
                .show_ui(ui, |ui| self.scope_menu(ui));
            #[cfg(test)]
            {
                self.scope_btn = combo.response.rect;
            }
            let open = combo.inner.is_some();
            if open && !self.popup_open {
                self.load_branches(ui.ctx());
            }
            self.popup_open = open;
            pick = combo.inner.flatten();
            ui.label(
                egui::RichText::new(format!(
                    "{} commits{}",
                    self.count,
                    if self.end { "" } else { " loaded" }
                ))
                .color(th.dim),
            );
            if self.pending {
                ui.spinner();
            }
            let button = ui.button("Refresh");
            #[cfg(test)]
            {
                self.header_button_h = button.rect.height();
                self.refresh_btn = button.rect;
            }
            refresh = button.clicked();
        });
        if refresh {
            self.restart(child.ctx(), self.scope.clone(), false);
            return;
        }
        if let Some(scope) = pick
            && scope != self.scope
        {
            self.restart(child.ctx(), scope, true);
            return;
        }
        if let Some(error) = &self.error {
            child.colored_label(th.dim, error);
        }
        if self.end && self.count == 0 && self.error.is_none() {
            child.label("No commits yet.");
            return;
        }
        let body = child.available_rect_before_wrap();
        // Window growth goes to the timeline; the details pane keeps its px width.
        let half_gap = 6.0 * s;
        let minimum = (220.0 * s).min(body.width() * 0.5);
        let clamp = |w: f32| w.clamp(minimum, (body.width() - minimum).max(minimum));
        let mut details_w = clamp(self.details_w.map_or(body.width() * 0.45, |w| w * s));
        let gap = egui::Rect::from_x_y_ranges(
            body.right() - details_w - half_gap..=body.right() - details_w + half_gap,
            body.y_range(),
        );
        let response = child
            .interact(gap, base.with("timeline-divider"), egui::Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            details_w = clamp(body.right() - pos.x);
            self.details_w = Some(details_w / s);
        }
        #[cfg(test)]
        {
            self.drawn_details_w = details_w;
        }
        let split = body.right() - details_w;
        let timeline =
            egui::Rect::from_min_max(body.min, egui::pos2(split - half_gap, body.bottom()));
        let detail_rect =
            egui::Rect::from_min_max(egui::pos2(split + half_gap, body.top()), body.max);
        child.painter().vline(
            split,
            body.y_range(),
            egui::Stroke::new(
                1.0,
                if response.hovered() || response.dragged() {
                    th.dim
                } else {
                    th.border
                },
            ),
        );
        let mut child = child.new_child(
            egui::UiBuilder::new()
                .id_salt("timeline")
                .max_rect(timeline),
        );
        child.set_clip_rect(timeline.intersect(ui.clip_rect()));
        let mut selected = None;
        let (row_h, lane_w) = (zoom.px(ROW_H), zoom.px(LANE_W));
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let avail_w = child.available_width() - 12.0 * s;
        let font = egui::FontId::proportional(13.0 * s);
        let date_w = child
            .painter()
            .layout_no_wrap("0000-00-00".into(), font.clone(), th.dim)
            .size()
            .x;
        let mut last = 0;
        child.spacing_mut().item_spacing.y = 0.0;
        let mut area = egui::ScrollArea::both()
            .id_salt((base, self.generation))
            .auto_shrink([false, false]);
        if let Some(y) = rescroll {
            area = area.vertical_scroll_offset(y);
        }
        let out = area.show_rows(&mut child, row_h, self.count, |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let target = range
                .clone()
                .map(|i| self.pages[i / BATCH][i % BATCH].width)
                .max()
                .unwrap_or(1) as f32;
            (self.lanes, self.shrink_rate) = ease_lanes(self.lanes, self.shrink_rate, target, dt);
            if self.shrink_rate > 0.0 {
                ui.ctx().request_repaint();
            }
            let graph_w = (self.lanes + 1.0) * lane_w;
            #[cfg(test)]
            {
                self.drawn_graph_w = graph_w;
            }
            // Only a lane graph wider than the pane scrolls sideways; the
            // metadata stays pinned to the visible edge either way.
            let total_w = (graph_w + MIN_SUBJECT_W * s + meta_w(date_w, s)).max(avail_w);
            ui.set_min_width(total_w);
            last = range.end;
            #[cfg(test)]
            {
                self.drawn = range.clone();
            }
            for i in range {
                let row = &self.pages[i / BATCH][i % BATCH];
                let (r, response) =
                    ui.allocate_exact_size(egui::vec2(total_w, row_h), egui::Sense::click());
                if response.clicked() {
                    selected = Some(row.commit.hash.clone());
                }
                let measure = |text: &str| {
                    ui.painter()
                        .layout_no_wrap(text.into(), font.clone(), th.text)
                        .size()
                        .x
                };
                // Lay the left side out first: the subject gets its natural
                // width and the metadata takes only what is left of the row.
                let visible_right = r.right().min(ui.clip_rect().right()) - 6.0 * s;
                let mut left = r.left() + graph_w;
                let chip_w = (!row.commit.refs.is_empty()).then(|| {
                    measure(&row.commit.refs).min(((visible_right - left) * 0.5).max(0.0))
                });
                if let Some(w) = chip_w {
                    left += w + 18.0 * s;
                }
                let subject_right = left + measure(&row.commit.subject);
                let author_w = measure(&row.commit.author).min(AUTHOR_W * s);
                let cols = columns(r, ui.clip_rect(), date_w, author_w, subject_right, s);
                let painter = ui.painter_at(r.intersect(ui.clip_rect()));
                if i % 2 == 0 {
                    painter.rect_filled(r, 0.0, th.sel_bg.gamma_multiply(0.22));
                }
                if response.hovered() || self.details.selected() == Some(row.commit.hash.as_str()) {
                    painter.rect_filled(r, 0.0, th.sel_bg);
                }
                // Graph, refs, and subject all clip before the metadata columns.
                let p = painter.with_clip_rect(cols.left.intersect(painter.clip_rect()));
                let x = |lane: usize| r.left() + (lane as f32 + 1.0) * lane_w;
                for (lane, color) in &row.incoming {
                    p.line_segment(
                        [
                            egui::pos2(x(*lane), r.top()),
                            egui::pos2(x(*lane), r.center().y),
                        ],
                        egui::Stroke::new(1.7 * s, COLORS[*color % COLORS.len()]),
                    );
                }
                for edge in &row.outgoing {
                    p.line_segment(
                        [
                            egui::pos2(x(edge.from), r.center().y),
                            egui::pos2(x(edge.to), r.bottom()),
                        ],
                        egui::Stroke::new(1.7 * s, COLORS[edge.color % COLORS.len()]),
                    );
                }
                p.circle_filled(
                    egui::pos2(x(row.lane), r.center().y),
                    4.0 * s,
                    COLORS[row.color % COLORS.len()],
                );
                if let Some(w) = chip_w {
                    let galley = p.layout_no_wrap(row.commit.refs.clone(), font.clone(), COLORS[0]);
                    let chip = egui::Rect::from_min_size(
                        egui::pos2(r.left() + graph_w, r.top() + 4.0 * s),
                        egui::vec2(w + 10.0 * s, 20.0 * s),
                    );
                    p.rect_filled(chip, 3.0 * s, th.sel_bg);
                    p.with_clip_rect(chip.intersect(p.clip_rect())).galley(
                        chip.min + egui::vec2(5.0, 2.0) * s,
                        galley,
                        COLORS[0],
                    );
                }
                let elided = |text: &str, width: f32, color| {
                    let mut job =
                        egui::text::LayoutJob::simple_singleline(text.into(), font.clone(), color);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
                    p.layout_job(job)
                };
                let subject = elided(&row.commit.subject, cols.left.right() - left, th.text);
                p.galley(
                    egui::pos2(left, r.center().y - subject.size().y / 2.0),
                    subject,
                    th.text,
                );
                if cols.author_alpha > 0.0 {
                    let color = th.dim.gamma_multiply(cols.author_alpha);
                    let author = elided(&row.commit.author, cols.author.width(), color);
                    painter
                        .with_clip_rect(cols.author.intersect(painter.clip_rect()))
                        .galley(
                            egui::pos2(
                                cols.author.right() - author.size().x,
                                r.center().y - author.size().y / 2.0,
                            ),
                            author,
                            color,
                        );
                }
                if cols.date_alpha > 0.0 {
                    painter.text(
                        egui::pos2(cols.date.right(), r.center().y),
                        egui::Align2::RIGHT_CENTER,
                        &row.commit.date,
                        font.clone(),
                        th.dim.gamma_multiply(cols.date_alpha),
                    );
                }
                response.on_hover_ui(|ui| {
                    ui.label(format!(
                        "{}\n{}\n{}\n{} · {}",
                        row.commit.hash,
                        row.commit.subject,
                        row.commit.refs,
                        row.commit.author,
                        row.commit.date
                    ));
                });
            }
        });
        self.scroll_y = out.state.offset.y;
        if last + 64 >= self.count {
            self.request();
        }
        if let Some(hash) = selected
            && let Some(cwd) = &self.cwd
        {
            self.details.select(cwd.clone(), hash, ui.ctx().clone());
        }
        self.details
            .show(ui, detail_rect, base.with(("details", self.generation)));
        if let Some(target) = self.details.take_open() {
            self.acts.push(HistoryAct::OpenDiff(target));
        }
    }
}

/// One dropdown row: the theme's selectable label, with a check on the
/// current choice.
fn menu_row(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    ui.selectable_label(
        on,
        if on {
            format!("{text}  ✓")
        } else {
            text.to_owned()
        },
    )
}

/// Lanes of graph room to show this frame, and the shrink rate to carry to
/// the next. Growth is instant so a wider row never paints under text. A
/// shrink runs at the speed that closes the largest gap seen in `EASE_S`,
/// so the column glides back instead of jumping.
fn ease_lanes(shown: f32, rate: f32, target: f32, dt: f32) -> (f32, f32) {
    if target >= shown {
        return (target, 0.0);
    }
    let rate = rate.max((shown - target) / EASE_S);
    let next = (shown - rate * dt).max(target);
    (next, if next > target { rate } else { 0.0 })
}
fn meta_w(date_w: f32, s: f32) -> f32 {
    date_w + (AUTHOR_W + COL_GAP + NAME_GAP) * s
}

/// Screen-space columns of one timeline row, with each metadata column's
/// opacity. A column at alpha 0 is not painted.
#[derive(Debug)]
struct Columns {
    /// Clip for graph, refs, and subject.
    left: egui::Rect,
    author: egui::Rect,
    date: egui::Rect,
    author_alpha: f32,
    date_alpha: f32,
}

/// Pins the date to the right edge of the visible pane, however wide the row
/// is or wherever it is scrolled, with the author name right-aligned just
/// before it. The subject wins: metadata only uses room the subject's full
/// text leaves free (`subject_right` is where that text ends). Columns never
/// move or shrink; each fades out across the last `FADE_W` of spare room
/// before the subject would reach it, the name first and then the date.
/// Opacity is a function of width, not time, so dragging the pane fades
/// smoothly both ways. Once both are gone the subject gets the whole row and
/// elides at the visible edge.
fn columns(
    row: egui::Rect,
    visible: egui::Rect,
    date_w: f32,
    author_w: f32,
    subject_right: f32,
    s: f32,
) -> Columns {
    let right = row.right().min(visible.right()) - 6.0 * s;
    let date = egui::Rect::from_x_y_ranges(right - date_w..=right, row.y_range());
    let author_right = date.left() - NAME_GAP * s;
    let author = egui::Rect::from_x_y_ranges(author_right - author_w..=author_right, row.y_range());
    // Spare room between the subject's end (plus its gap) and a column.
    let fade = |column_left: f32| {
        ((column_left - (subject_right + COL_GAP * s)) / (FADE_W * s)).clamp(0.0, 1.0)
    };
    let date_alpha = fade(date.left());
    // The name never outlives the date beside it.
    let author_alpha = fade(author.left()).min(date_alpha);
    let left_right = if author_alpha > 0.0 {
        author.left() - COL_GAP * s
    } else if date_alpha > 0.0 {
        date.left() - COL_GAP * s
    } else {
        right
    };
    let left = egui::Rect::from_x_y_ranges(row.left()..=left_right.max(row.left()), row.y_range());
    Columns {
        left,
        author,
        date,
        author_alpha,
        date_alpha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn commit(hash: &str, parents: &[&str]) -> Commit {
        Commit {
            hash: hash.into(),
            parents: parents.iter().map(|s| s.to_string()).collect(),
            refs: String::new(),
            author: "Author".into(),
            date: "2026-09-23".into(),
            subject: hash.into(),
        }
    }
    #[test]
    fn clicking_rows_changes_selection_and_refresh_clears_it() {
        let mut view = HistoryView::new(Some(PathBuf::new()));
        view.end = true;
        let mut graph = Graph::default();
        view.pages.push(vec![
            graph.push(commit("first", &[])),
            graph.push(commit("second", &[])),
        ]);
        view.count = 2;
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let mut frame = |view: &mut HistoryView, events| {
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    events,
                    ..Default::default()
                },
                |ui| view.show(ui, rect, egui::Id::new("click")),
            );
        };
        frame(&mut view, vec![]);
        let click = |view: &mut HistoryView,
                     frame: &mut dyn FnMut(&mut HistoryView, Vec<egui::Event>),
                     pos| {
            frame(view, vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                frame(
                    view,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            }
        };
        click(&mut view, &mut frame, egui::pos2(250.0, 45.0));
        assert_eq!(view.details.selected(), Some("first"));
        click(&mut view, &mut frame, egui::pos2(250.0, 73.0));
        assert_eq!(view.details.selected(), Some("second"));
        let refresh = view.refresh_btn.center();
        click(&mut view, &mut frame, refresh);
        assert_eq!(view.details.selected(), None);
        assert_eq!(view.count, 0);
    }

    #[test]
    fn timeline_divider_width_survives_window_shrink() {
        let mut view = HistoryView::new(None);
        view.end = true;
        let mut graph = Graph::default();
        view.pages.push(vec![graph.push(commit("only", &[]))]);
        view.count = 1;
        let ctx = egui::Context::default();
        let frame = |view: &mut HistoryView, width: f32, events| {
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 600.0));
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    events,
                    ..Default::default()
                },
                |ui| view.show(ui, rect, egui::Id::new("split")),
            );
        };
        let press = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut view, 1000.0, vec![]);
        let before = view.drawn_details_w;
        // Body spans 8..992 inside the 8px margin.
        let start = egui::pos2(992.0 - before, 300.0);
        let end = start + egui::vec2(-100.0, 0.0);
        frame(&mut view, 1000.0, vec![egui::Event::PointerMoved(start)]);
        frame(&mut view, 1000.0, vec![press(start, true)]);
        frame(&mut view, 1000.0, vec![egui::Event::PointerMoved(end)]);
        frame(&mut view, 1000.0, vec![press(end, false)]);
        let dragged = view.drawn_details_w;
        assert!(
            (dragged - (before + 100.0)).abs() < 1.0,
            "{before} -> {dragged}"
        );
        frame(&mut view, 600.0, vec![]);
        assert!(view.drawn_details_w < dragged, "{}", view.drawn_details_w);
        frame(&mut view, 1000.0, vec![]);
        assert_eq!(view.drawn_details_w, dragged);
    }

    #[test]
    fn metadata_fades_as_the_subject_nears_it_and_never_moves() {
        let rect =
            |x0: f32, x1: f32| egui::Rect::from_min_max(egui::pos2(x0, 0.0), egui::pos2(x1, 28.0));
        let at = |subject_right: f32| {
            columns(
                rect(0.0, 500.0),
                rect(0.0, 500.0),
                80.0,
                40.0,
                subject_right,
                1.0,
            )
        };
        // Date 414..494, name 368..408. Short subject: both fully opaque,
        // the subject clip ending a gap before the name.
        let short = at(200.0);
        assert_eq!((short.date.right(), short.author.width()), (494.0, 40.0));
        assert_eq!(short.author.right(), short.date.left() - NAME_GAP);
        assert_eq!((short.author_alpha, short.date_alpha), (1.0, 1.0));
        assert_eq!(short.left.right(), short.author.left() - COL_GAP);
        // Subject halfway into the name's fade band: name half faded, date
        // untouched, and nothing moved.
        let half = at(368.0 - COL_GAP - FADE_W / 2.0);
        assert!((half.author_alpha - 0.5).abs() < 1e-4, "{half:?}");
        assert_eq!(half.date_alpha, 1.0);
        assert_eq!((half.author, half.date), (short.author, short.date));
        // Subject reaching the name: name gone, date fully shown, and the
        // subject clip now ends a gap before the date.
        let no_name = at(368.0 - COL_GAP);
        assert_eq!((no_name.author_alpha, no_name.date_alpha), (0.0, 1.0));
        assert_eq!(no_name.left.right(), no_name.date.left() - COL_GAP);
        // Subject halfway into the date's band: date half faded.
        let fading = at(414.0 - COL_GAP - FADE_W / 2.0);
        assert!((fading.date_alpha - 0.5).abs() < 1e-4, "{fading:?}");
        assert_eq!(fading.author_alpha, 0.0);
        // Subject past the date: all metadata gone, subject gets the row.
        let long = at(460.0);
        assert_eq!((long.author_alpha, long.date_alpha), (0.0, 0.0));
        assert_eq!(long.left.right(), 494.0);
        // Row scrolled wider than the pane: metadata follows the visible edge.
        let scrolled = columns(
            rect(-300.0, 1200.0),
            rect(0.0, 600.0),
            80.0,
            40.0,
            300.0,
            1.0,
        );
        assert_eq!(scrolled.date.right(), 594.0);
        // Zoom scales every gap and the fade band.
        let zoomed = columns(
            rect(0.0, 1000.0),
            rect(0.0, 1000.0),
            160.0,
            80.0,
            300.0,
            2.0,
        );
        assert_eq!(zoomed.author.right(), zoomed.date.left() - NAME_GAP * 2.0);
        assert_eq!(zoomed.date.right(), 988.0);
    }

    #[test]
    fn narrow_pane_keeps_subjects_whole_and_drops_metadata_first() {
        let mut view = HistoryView::new(None);
        view.end = true;
        let mut graph = Graph::default();
        let mut short = commit("short", &["long"]);
        short.subject = "short subject".into();
        let mut long = commit("long", &[]);
        long.subject = "a very long subject ".repeat(20);
        view.pages.push(vec![graph.push(short), graph.push(long)]);
        view.count = 2;
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 400.0));
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    ..Default::default()
                },
                |ui| view.show(ui, rect, egui::Id::new("narrow")),
            ));
        }
        // Body 8..692; timeline ends half a gap left of the divider.
        let timeline_right = 692.0 - view.drawn_details_w - 6.0;
        let mut texts = vec![];
        for clipped in &out.unwrap().shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                texts.push((
                    text.galley.text().to_owned(),
                    text.visual_bounding_rect(),
                    text.galley.elided,
                ));
            }
        }
        let all = |needle: &str| {
            texts
                .iter()
                .filter(|t| t.0.starts_with(needle))
                .collect::<Vec<_>>()
        };
        // Short subject: whole, with the name hugging the date at the edge.
        let (dates, authors) = (all("2026-09-23"), all("Author"));
        assert_eq!((dates.len(), authors.len()), (1, 1), "{texts:?}");
        let (date, author) = (dates[0], authors[0]);
        let subject = all("short subject")[0];
        assert!(!subject.2, "{subject:?}");
        assert!(
            date.1.right() <= timeline_right && date.1.right() > timeline_right - 20.0,
            "{date:?} vs {timeline_right}"
        );
        assert!(
            date.1.left() - author.1.right() <= NAME_GAP + 2.0,
            "name should hug the date: {author:?} {date:?}"
        );
        assert!(
            subject.1.right() < author.1.left(),
            "{subject:?} {author:?}"
        );
        // Long subject: its row paints no metadata and the subject runs to
        // the visible edge before eliding.
        let long = all("a very long")[0];
        assert!(long.2, "{long:?}");
        assert!(
            long.1.right() > timeline_right - 20.0 && long.1.right() <= timeline_right + 1.0,
            "{long:?} vs {timeline_right}"
        );
    }

    #[test]
    fn linear_history_keeps_one_color_and_ends_at_root() {
        let mut g = Graph::default();
        let a = g.push(commit("a", &["b"]));
        let b = g.push(commit("b", &["c"]));
        let c = g.push(commit("c", &[]));
        assert!(a.incoming.is_empty());
        assert_eq!(
            a.outgoing,
            vec![Edge {
                from: 0,
                to: 0,
                color: a.color
            }]
        );
        assert_eq!((a.color, a.lane), (b.color, b.lane));
        assert_eq!(c.incoming, vec![(0, a.color)]);
        assert!(c.outgoing.is_empty());
        assert!(g.lanes.is_empty());
    }
    #[test]
    fn merge_lanes_split_then_rejoin_without_duplicate_ancestors() {
        let mut g = Graph::default();
        let merge = g.push(commit("merge", &["left", "right"]));
        assert_eq!(merge.outgoing.len(), 2);
        assert_ne!(merge.outgoing[0].color, merge.outgoing[1].color);
        g.push(commit("left", &["root"]));
        let right = g.push(commit("right", &["root"]));
        assert_eq!(right.lane, 1);
        assert_eq!(g.lanes.len(), 1);
        assert_eq!(
            right
                .outgoing
                .iter()
                .map(|e| (e.from, e.to))
                .collect::<Vec<_>>(),
            [(0, 0), (1, 0)]
        );
        let root = g.push(commit("root", &[]));
        assert_eq!(root.lane, 0);
        assert!(g.lanes.is_empty());
    }
    #[test]
    fn octopus_and_disconnected_roots_preserve_frontier_edges() {
        let mut g = Graph::default();
        for (hash, parents) in [
            ("m", vec!["a", "b", "c"]),
            ("other", vec![]),
            ("a", vec!["r"]),
            ("b", vec!["r"]),
            ("c", vec!["r"]),
            ("r", vec![]),
        ] {
            let before = g.lanes.clone();
            let row = g.push(commit(hash, &parents));
            for (lane, (h, c)) in before.iter().enumerate().filter(|(_, (h, _))| h != hash) {
                let target = g.lanes.iter().position(|(s, _)| s == h).unwrap();
                assert!(row.outgoing.contains(&Edge {
                    from: lane,
                    to: target,
                    color: *c
                }));
            }
            let unique: std::collections::HashSet<_> = g.lanes.iter().map(|(h, _)| h).collect();
            assert_eq!(unique.len(), g.lanes.len());
        }
        assert!(g.lanes.is_empty());
    }
    #[test]
    fn parser_preserves_unicode_and_delimiter_like_subjects() {
        let bytes =
            "abc\0def ghi\0HEAD -> main, tag: v1\0Zoë\02026-09-23\0Subject | tabs\t and 日本語\0";
        let mut r = std::io::Cursor::new(bytes.as_bytes());
        let c = read_commit(&mut r).unwrap().unwrap();
        assert_eq!(c.parents, ["def", "ghi"]);
        assert_eq!(c.author, "Zoë");
        assert_eq!(c.subject, "Subject | tabs\t and 日本語");
        assert!(read_commit(&mut r).unwrap().is_none());
        assert!(read_commit(&mut std::io::Cursor::new(b"truncated")).is_err());
    }
    pub(super) fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().into()
    }
    #[test]
    fn worker_reads_real_merge_tags_detached_head_and_worktree_without_writes() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "History Test"]);
        git(dir, &["config", "user.email", "history@example.test"]);
        git(dir, &["commit", "--allow-empty", "-m", "Root"]);
        git(dir, &["checkout", "-b", "topic"]);
        git(dir, &["commit", "--allow-empty", "-m", "Topic"]);
        git(dir, &["checkout", "main"]);
        git(dir, &["commit", "--allow-empty", "-m", "Main"]);
        git(dir, &["merge", "--no-ff", "topic", "-m", "Merge"]);
        git(dir, &["tag", "-a", "v1", "-m", "Release"]);
        let work = dir.join("linked");
        git(
            dir,
            &["worktree", "add", "--detach", work.to_str().unwrap()],
        );
        git(&work, &["commit", "--allow-empty", "-m", "Detached"]);
        let before = git(&work, &["status", "--porcelain=v1"]);
        let stream = Stream::start(work.clone(), scope::Scope::All, egui::Context::default());
        stream.next.send(()).unwrap();
        let page = stream.pages.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(page.end);
        assert!(page.error.is_none(), "{:?}", page.error);
        assert_eq!(page.rows.len(), 5);
        assert_eq!(page.rows[0].commit.subject, "Detached");
        assert!(page.rows.iter().any(|r| r.commit.refs.contains("tag: v1")));
        assert!(page.rows.iter().any(|r| r.commit.parents.len() == 2));
        assert_eq!(git(&work, &["status", "--porcelain=v1"]), before);
    }
    #[test]
    fn empty_and_non_repository_results_are_distinct() {
        let repo = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let stream = Stream::start(repo.path().into(), scope::Scope::Current, ctx.clone());
        stream.next.send(()).unwrap();
        assert!(
            stream
                .pages
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .error
                .is_some()
        );
        git(repo.path(), &["init"]);
        let stream = Stream::start(repo.path().into(), scope::Scope::Current, ctx);
        stream.next.send(()).unwrap();
        let page = stream.pages.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(page.end && page.rows.is_empty() && page.error.is_none());
    }
    #[test]
    fn large_history_paints_only_viewport_rows_and_scrolls_to_old_commits() {
        let mut view = HistoryView::new(None);
        view.end = true;
        let mut graph = Graph::default();
        let start = std::time::Instant::now();
        for i in 0..100_000 {
            if i % BATCH == 0 {
                view.pages.push(Vec::with_capacity(BATCH));
            }
            view.pages
                .last_mut()
                .unwrap()
                .push(graph.push(commit(&i.to_string(), &[&(i + 1).to_string()])));
        }
        view.count = 100_000;
        let graph_time = start.elapsed();
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
        let start = std::time::Instant::now();
        for frame in 0..12 {
            let events = if frame == 3 {
                vec![
                    egui::Event::PointerMoved(egui::pos2(400.0, 300.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        phase: egui::TouchPhase::Move,
                        delta: egui::vec2(0.0, -50_000.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            } else {
                vec![]
            };
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    events,
                    ..Default::default()
                },
                |ui| view.show(ui, rect, egui::Id::new("large")),
            );
            assert!(view.drawn.len() < 30, "{:?}", view.drawn);
        }
        assert!(view.drawn.start > 100, "{:?}", view.drawn);
        eprintln!(
            "100k graph {:?}; 12 headless debug frames {:?}; final range {:?}",
            graph_time,
            start.elapsed(),
            view.drawn
        );
    }

    #[test]
    fn rows_follow_theme_font_size() {
        let mut view = HistoryView::new(None);
        view.end = true;
        let mut graph = Graph::default();
        view.pages.push(
            (0..BATCH)
                .map(|i| graph.push(commit(&i.to_string(), &[&(i + 1).to_string()])))
                .collect(),
        );
        view.count = BATCH;
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
        let mut rows_at = |px: f32| {
            crate::terminal::set_font_size(&ctx, px);
            for _ in 0..2 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(rect),
                        ..Default::default()
                    },
                    |ui| view.show(ui, rect, egui::Id::new("zoom")),
                );
            }
            (view.drawn.len(), view.header_button_h)
        };
        let base = rows_at(crate::config::DEFAULT_FONT_SIZE);
        let zoomed = rows_at(crate::config::DEFAULT_FONT_SIZE * 2.0);
        assert_eq!(view.scale, 2.0);
        assert!(
            zoomed.0 * 2 <= base.0 + 2,
            "base {base:?}, zoomed {zoomed:?}"
        );
        assert!(zoomed.1 >= base.1 * 1.9, "base {base:?}, zoomed {zoomed:?}");
    }

    #[test]
    fn demand_batches_keep_graph_continuity_and_stop_when_view_closes() {
        use std::io::Write;
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        let mut import = std::process::Command::new("git")
            .current_dir(repo.path())
            .args(["fast-import", "--quiet"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = import.stdin.take().unwrap();
        for i in 0..BATCH + 20 {
            writeln!(input, "commit refs/heads/main\ncommitter Test <test@example.test> {} +0000\ndata 7\nCommit!\n", 1_700_000_000+i).unwrap();
        }
        drop(input);
        assert!(import.wait().unwrap().success());
        let stream = Stream::start(
            repo.path().into(),
            scope::Scope::Current,
            egui::Context::default(),
        );
        stream.next.send(()).unwrap();
        let first = stream.pages.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(first.rows.len(), BATCH);
        assert!(!first.end);
        assert!(matches!(
            stream.pages.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        stream.next.send(()).unwrap();
        let second = stream.pages.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(second.rows.len(), 20);
        assert!(second.end);
        assert_eq!(first.rows.last().unwrap().color, second.rows[0].color);
        assert_eq!(
            first.rows.last().unwrap().commit.parents[0],
            second.rows[0].commit.hash
        );
        let cancel = stream.cancel.clone();
        drop(stream);
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn scopes_walk_only_their_refs() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "History Test"]);
        git(dir, &["config", "user.email", "history@example.test"]);
        git(dir, &["commit", "--allow-empty", "-m", "Root"]);
        git(dir, &["checkout", "-b", "topic"]);
        git(dir, &["commit", "--allow-empty", "-m", "Topic"]);
        git(dir, &["checkout", "main"]);
        git(dir, &["commit", "--allow-empty", "-m", "Main"]);
        // A foreign namespace and a stash: `--all` walked both.
        let tree = git(dir, &["rev-parse", "HEAD^{tree}"]);
        let private = git(dir, &["commit-tree", &tree, "-m", "Private"]);
        git(dir, &["update-ref", "refs/x/private", &private]);
        std::fs::write(dir.join("wip.txt"), "wip").unwrap();
        git(dir, &["add", "wip.txt"]);
        git(dir, &["stash"]);
        let read = |scope: scope::Scope| {
            let stream = Stream::start(dir.into(), scope, egui::Context::default());
            stream.next.send(()).unwrap();
            let page = stream.pages.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(page.error.is_none(), "{:?}", page.error);
            let subjects: Vec<String> =
                page.rows.iter().map(|r| r.commit.subject.clone()).collect();
            (
                subjects,
                page.resolved.expect("first page carries the scope").label,
            )
        };
        assert_eq!(
            read(scope::Scope::Current),
            (
                vec!["Main".to_string(), "Root".to_string()],
                "main".to_string()
            )
        );
        assert_eq!(
            read(scope::Scope::Branch("refs/heads/topic".into())),
            (
                vec!["Topic".to_string(), "Root".to_string()],
                "topic".to_string()
            )
        );
        for scope in [scope::Scope::Local, scope::Scope::All] {
            let (subjects, _) = read(scope.clone());
            assert!(
                subjects.contains(&"Topic".to_string()),
                "{scope:?}: {subjects:?}"
            );
            assert!(
                !subjects.contains(&"Private".to_string()),
                "{scope:?}: {subjects:?}"
            );
            assert!(
                !subjects
                    .iter()
                    .any(|s| s.starts_with("WIP on") || s.starts_with("index on")),
                "{scope:?} walked the stash: {subjects:?}"
            );
        }
    }

    fn frame(
        ctx: &egui::Context,
        view: &mut HistoryView,
        rect: egui::Rect,
        base: egui::Id,
        events: Vec<egui::Event>,
    ) {
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(rect),
                events,
                ..Default::default()
            },
            |ui| view.show(ui, rect, base),
        );
    }
    fn click_at(
        ctx: &egui::Context,
        view: &mut HistoryView,
        rect: egui::Rect,
        base: egui::Id,
        pos: egui::Pos2,
    ) {
        frame(ctx, view, rect, base, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            let press = egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(ctx, view, rect, base, vec![press]);
        }
    }
    /// Draw frames until `done` holds; workers answer between frames.
    fn settle(
        ctx: &egui::Context,
        view: &mut HistoryView,
        rect: egui::Rect,
        base: egui::Id,
        done: impl Fn(&HistoryView) -> bool,
    ) {
        let start = std::time::Instant::now();
        while !done(view) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "view never settled"
            );
            frame(ctx, view, rect, base, vec![]);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn scope_dropdown_picks_a_branch_and_keeps_the_selected_commit() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "History Test"]);
        git(dir, &["config", "user.email", "history@example.test"]);
        git(dir, &["commit", "--allow-empty", "-m", "Root"]);
        git(dir, &["checkout", "-b", "topic"]);
        git(dir, &["commit", "--allow-empty", "-m", "Topic"]);
        git(dir, &["checkout", "main"]);
        git(dir, &["commit", "--allow-empty", "-m", "Main"]);
        // More than FILTER_MIN branches, so the filter field shows.
        for i in 0..9 {
            git(dir, &["branch", &format!("card/c{i}")]);
        }
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let base = egui::Id::new("scope-pick");
        let mut view = HistoryView::new(Some(dir.into()));
        settle(&ctx, &mut view, rect, base, |v| v.end);
        assert_eq!(view.label.as_deref(), Some("main"));
        let main = view.pages[0][0].commit.hash.clone();
        view.details.select(dir.into(), main.clone(), ctx.clone());

        let btn = view.scope_btn.center();
        click_at(&ctx, &mut view, rect, base, btn);
        settle(&ctx, &mut view, rect, base, |v| {
            v.scope_rows.iter().any(|(n, _)| n == "topic")
        });
        // No upstream is configured, so the Current row is just the branch.
        assert_eq!(view.scope_rows[0].0, "main");
        // Clicking the filter field must not close the popup.
        let filter = view
            .filter_rect
            .expect("more than FILTER_MIN branches show the filter");
        click_at(&ctx, &mut view, rect, base, filter.center());
        frame(&ctx, &mut view, rect, base, vec![]);
        assert!(view.popup_open, "the filter click closed the popup");

        let topic = view
            .scope_rows
            .iter()
            .find(|(n, _)| n == "topic")
            .unwrap()
            .1;
        click_at(&ctx, &mut view, rect, base, topic.center());
        assert_eq!(view.scope, scope::Scope::Branch("refs/heads/topic".into()));
        assert_eq!(
            view.details.selected(),
            Some(main.as_str()),
            "details keep the commit"
        );
        settle(&ctx, &mut view, rect, base, |v| v.end);
        let subjects: Vec<&str> = view
            .pages
            .iter()
            .flatten()
            .map(|r| r.commit.subject.as_str())
            .collect();
        assert_eq!(subjects, ["Topic", "Root"]);
        assert_eq!(view.label.as_deref(), Some("topic"));
        assert!(!view.popup_open, "a pick closes the popup");
    }

    #[test]
    fn scope_dropdown_without_a_directory_still_offers_the_scopes() {
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let base = egui::Id::new("scope-none");
        let mut view = HistoryView::new(None);
        frame(&ctx, &mut view, rect, base, vec![]);
        let btn = view.scope_btn.center();
        click_at(&ctx, &mut view, rect, base, btn);
        frame(&ctx, &mut view, rect, base, vec![]);
        let names: Vec<&str> = view.scope_rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Current branch", "Local branches", "All"]);
        assert!(
            matches!(view.branches, Some(Err(_))),
            "no Git ran without a directory"
        );
    }

    #[test]
    fn text_column_grows_at_once_and_eases_back() {
        let dt = 1.0 / 60.0;
        assert_eq!(
            ease_lanes(2.0, 0.0, 6.0, dt),
            (6.0, 0.0),
            "growth is instant"
        );
        assert_eq!(ease_lanes(3.0, 0.0, 3.0, dt), (3.0, 0.0), "steady state");
        assert_eq!(
            ease_lanes(4.0, 9.0, 5.0, dt),
            (5.0, 0.0),
            "growth cancels a shrink"
        );
        let (mut lanes, mut rate, mut frames) = (6.0f32, 0.0f32, 0);
        while lanes > 2.0 {
            let (next, r) = ease_lanes(lanes, rate, 2.0, dt);
            assert!(next < lanes && next >= 2.0, "{lanes} -> {next}");
            (lanes, rate, frames) = (next, r, frames + 1);
            assert!(frames <= 10, "shrink took longer than EASE_S");
        }
        // 0.15 s at 60 fps is 9 frames; float rounding may add one.
        assert!((9..=10).contains(&frames), "{frames} frames");
        assert_eq!(rate, 0.0, "a finished shrink forgets its rate");
    }

    #[test]
    fn subjects_start_after_the_widest_graph_on_screen() {
        let mut view = HistoryView::new(None);
        view.end = true;
        let mut g = Graph::default();
        let mut rows = Vec::new();
        // 100 linear rows (1 lane), an octopus opening 6 lanes for ~60 rows,
        // then 200 linear rows again.
        for i in 0..100 {
            let parent = if i < 99 {
                format!("a{}", i + 1)
            } else {
                "m".into()
            };
            rows.push(g.push(commit(&format!("a{i}"), &[&parent])));
        }
        let heads: Vec<String> = (0..6).map(|k| format!("b{k}_0")).collect();
        let heads: Vec<&str> = heads.iter().map(String::as_str).collect();
        rows.push(g.push(commit("m", &heads)));
        for k in 0..6 {
            for j in 0..10 {
                let parent = if j < 9 {
                    format!("b{k}_{}", j + 1)
                } else {
                    "r".into()
                };
                rows.push(g.push(commit(&format!("b{k}_{j}"), &[&parent])));
            }
        }
        rows.push(g.push(commit("r", &["c0"])));
        for i in 0..200 {
            let parent = format!("c{}", i + 1);
            let parents: &[&str] = if i < 199 { &[&parent] } else { &[] };
            rows.push(g.push(commit(&format!("c{i}"), parents)));
        }
        view.count = rows.len();
        view.pages.push(rows);
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let base = egui::Id::new("column");
        let narrow = 2.0 * LANE_W;
        let wheel = |dy: f32| {
            vec![
                egui::Event::PointerMoved(egui::pos2(200.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    phase: egui::TouchPhase::Move,
                    delta: egui::vec2(0.0, dy),
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        frame(&ctx, &mut view, rect, base, vec![]);
        assert_eq!(view.drawn_graph_w, narrow, "top of history is one lane");
        // Scroll into the octopus stretch (row 101 onward).
        frame(&ctx, &mut view, rect, base, wheel(-104.0 * ROW_H));
        settle(&ctx, &mut view, rect, base, |v| v.drawn.start >= 100);
        frame(&ctx, &mut view, rect, base, vec![]);
        assert!(view.drawn.start < 150, "{:?}", view.drawn);
        assert_eq!(
            view.drawn_graph_w,
            7.0 * LANE_W,
            "six lanes on screen, instantly"
        );
        // Back to the top: the column glides back within the ease.
        frame(&ctx, &mut view, rect, base, wheel(200.0 * ROW_H));
        settle(&ctx, &mut view, rect, base, |v| v.drawn.start == 0);
        // Headless frames advance `stable_dt` by egui's default 1/60 s, so
        // 15 frames (0.25 s) outlast the 0.15 s ease.
        for _ in 0..15 {
            frame(&ctx, &mut view, rect, base, vec![]);
        }
        assert_eq!(view.drawn_graph_w, narrow);
    }
}
