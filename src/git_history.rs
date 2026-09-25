//! Read-only Git history: a demand-driven Git stream, pure lane layout, and virtualized native rows.
use eframe::egui;
mod changes;
mod details;
mod diff;
mod diff_view;
mod file_tree;
mod git;
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
/// least it is worth showing at, the gap after the subject, the tighter gap
/// between name and date, and the subject room a lane graph wider than the
/// pane leaves before the timeline scrolls sideways.
const AUTHOR_W: f32 = 140.0;
const MIN_AUTHOR_W: f32 = 24.0;
const COL_GAP: f32 = 12.0;
const NAME_GAP: f32 = 6.0;
const MIN_SUBJECT_W: f32 = 120.0;
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
    fn start(cwd: PathBuf, ctx: egui::Context) -> Self {
        let (next, requests) = mpsc::sync_channel(1);
        let (tx, pages) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let result = stream_history(cwd, &requests, &tx, &stop, &ctx);
            if let Err(error) = result {
                let _ = tx.send(Page {
                    rows: Vec::new(),
                    end: true,
                    error: Some(error),
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
    requests: &mpsc::Receiver<()>,
    tx: &mpsc::SyncSender<Page>,
    cancel: &Arc<AtomicBool>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let (stdout, exit) = git::spawn(
        &cwd,
        &[
            "log",
            "--all",
            "--topo-order",
            "--decorate=short",
            "--no-color",
            "--no-patch",
            "--encoding=UTF-8",
            "--no-show-signature",
            "-z",
            "--format=%H%x00%P%x00%D%x00%an%x00%as%x00%s",
            "--",
        ],
        cancel,
        None,
    )
    .map_err(|e| e.to_string())?;
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
            if tx.send(Page { rows, end, error }).is_err() {
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
    width: usize,
    generation: u64,
    /// Theme font size / default, applied to every px dimension below.
    scale: f32,
    /// Last vertical scroll offset, so a zoom keeps the same rows in view.
    scroll_y: f32,
    /// Dragged details pane width in unscaled px; `None` until the first drag.
    /// Clamped when drawn but only written on drag, so a shrink-then-regrow of
    /// the window restores it. Not persisted across restart.
    details_w: Option<f32>,
    #[cfg(test)]
    drawn: std::ops::Range<usize>,
    #[cfg(test)]
    drawn_details_w: f32,
    #[cfg(test)]
    header_button_h: f32,
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
            width: 1,
            generation: 0,
            scale: 1.0,
            scroll_y: 0.0,
            details_w: None,
            #[cfg(test)]
            drawn: 0..0,
            #[cfg(test)]
            drawn_details_w: 0.0,
            #[cfg(test)]
            header_button_h: 0.0,
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
                self.stream = Some(Stream::start(cwd.clone(), ctx.clone()));
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
                    self.width = self
                        .width
                        .max(page.rows.iter().map(|r| r.width).max().unwrap_or(1));
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
        child.horizontal(|ui| {
            ui.label(egui::RichText::new("All branches").color(th.text).strong());
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
            }
            refresh = button.clicked();
        });
        if refresh {
            let cwd = self.cwd.clone();
            let generation = self.generation + 1;
            let details_w = self.details_w;
            // Retire potentially large cached histories off the GUI thread.
            let old = std::mem::replace(self, Self::new(cwd));
            self.details_w = details_w;
            if let Some(stream) = &old.stream {
                stream.cancel.store(true, Ordering::Relaxed);
            }
            std::thread::spawn(move || drop(old));
            self.generation = generation;
            child.ctx().request_repaint();
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
        let graph_w = (self.width as f32 + 1.0) * lane_w;
        let font = egui::FontId::proportional(13.0 * s);
        let date_w = child
            .painter()
            .layout_no_wrap("0000-00-00".into(), font.clone(), th.dim)
            .size()
            .x;
        // Only a lane graph wider than the pane scrolls sideways; the
        // metadata stays pinned to the visible edge either way.
        let total_w = (graph_w + MIN_SUBJECT_W * s + meta_w(date_w, s))
            .max(child.available_width() - 12.0 * s);
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
                if cols.author.width() > 0.0 {
                    let author = elided(&row.commit.author, cols.author.width(), th.dim);
                    painter
                        .with_clip_rect(cols.author.intersect(painter.clip_rect()))
                        .galley(
                            egui::pos2(
                                cols.author.right() - author.size().x,
                                r.center().y - author.size().y / 2.0,
                            ),
                            author,
                            th.dim,
                        );
                }
                if cols.date.width() > 0.0 {
                    painter.text(
                        egui::pos2(cols.date.right(), r.center().y),
                        egui::Align2::RIGHT_CENTER,
                        &row.commit.date,
                        font.clone(),
                        th.dim,
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

fn meta_w(date_w: f32, s: f32) -> f32 {
    date_w + (AUTHOR_W + COL_GAP + NAME_GAP) * s
}

/// Screen-space columns of one timeline row. A hidden column is zero-width.
#[derive(Debug)]
struct Columns {
    /// Clip for graph, refs, and subject.
    left: egui::Rect,
    author: egui::Rect,
    date: egui::Rect,
}

/// Pins the date to the right edge of the visible pane, however wide the row
/// is or wherever it is scrolled, with the author name right-aligned just
/// before it. The subject wins: metadata only takes room the subject's full
/// text leaves free (`subject_right` is where that text ends). As the room
/// runs out the author name elides, then drops out, then the date drops out,
/// and only then is the subject itself elided at the row's visible edge.
fn columns(
    row: egui::Rect,
    visible: egui::Rect,
    date_w: f32,
    author_w: f32,
    subject_right: f32,
    s: f32,
) -> Columns {
    let right = row.right().min(visible.right()) - 6.0 * s;
    let hidden = egui::Rect::from_x_y_ranges(right..=right, row.y_range());
    // Room the metadata may use without covering any of the subject.
    let room = right - (subject_right + COL_GAP * s);
    if room < date_w {
        let left = egui::Rect::from_x_y_ranges(row.left()..=right.max(row.left()), row.y_range());
        return Columns {
            left,
            author: hidden,
            date: hidden,
        };
    }
    let date = egui::Rect::from_x_y_ranges(right - date_w..=right, row.y_range());
    let author_w = author_w.min(room - date_w - NAME_GAP * s);
    let (author, meta_left) = if author_w >= MIN_AUTHOR_W * s {
        let author_right = date.left() - NAME_GAP * s;
        let author =
            egui::Rect::from_x_y_ranges(author_right - author_w..=author_right, row.y_range());
        (author, author.left())
    } else {
        let at = date.left();
        (
            egui::Rect::from_x_y_ranges(at..=at, row.y_range()),
            date.left(),
        )
    };
    let left_right = (meta_left - COL_GAP * s).max(row.left());
    let left = egui::Rect::from_x_y_ranges(row.left()..=left_right, row.y_range());
    Columns { left, author, date }
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
        click(&mut view, &mut frame, egui::pos2(180.0, 18.0));
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
    fn subject_wins_and_metadata_takes_only_what_it_leaves() {
        let rect =
            |x0: f32, x1: f32| egui::Rect::from_min_max(egui::pos2(x0, 0.0), egui::pos2(x1, 28.0));
        // Short subject: date at the right edge, the name just before it at
        // its own width, the subject clip ending a gap before the name.
        let wide = columns(rect(0.0, 1000.0), rect(0.0, 1000.0), 80.0, 40.0, 300.0, 1.0);
        assert_eq!(wide.date.right(), 994.0);
        assert_eq!(wide.author.right(), wide.date.left() - NAME_GAP);
        assert_eq!(wide.author.width(), 40.0);
        assert_eq!(wide.left.right(), wide.author.left() - COL_GAP);
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
        assert!(scrolled.left.right() < scrolled.author.left());
        // Longer subject: the name elides so the subject stays whole.
        let squeezed = columns(rect(0.0, 500.0), rect(0.0, 500.0), 80.0, 100.0, 340.0, 1.0);
        assert!(squeezed.author.width() < 100.0 && squeezed.author.width() >= MIN_AUTHOR_W);
        assert!(squeezed.left.right() >= 340.0);
        // Longer still: the name drops out, the date stays.
        let no_name = columns(rect(0.0, 500.0), rect(0.0, 500.0), 80.0, 100.0, 390.0, 1.0);
        assert_eq!(no_name.author.width(), 0.0);
        assert_eq!(no_name.date.width(), 80.0);
        assert!(no_name.left.right() >= 390.0);
        // Subject reaches the date's room: all metadata goes, the subject
        // gets the whole visible row.
        let long = columns(rect(0.0, 500.0), rect(0.0, 500.0), 80.0, 40.0, 420.0, 1.0);
        assert_eq!((long.author.width(), long.date.width()), (0.0, 0.0));
        assert_eq!(long.left.right(), 494.0);
        // Zoom scales every gap.
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
        let stream = Stream::start(work.clone(), egui::Context::default());
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
        let stream = Stream::start(repo.path().into(), ctx.clone());
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
        let stream = Stream::start(repo.path().into(), ctx);
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
        let stream = Stream::start(repo.path().into(), egui::Context::default());
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
}
