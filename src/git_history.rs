//! Read-only Git history: a demand-driven Git stream, pure lane layout, and virtualized native rows.
use eframe::egui;
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
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args([
            "--no-pager",
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
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("Cannot start Git: {e}"))?;
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut stderr = child.stderr.take().unwrap();
    let mut errors = Some(std::thread::spawn(move || {
        let mut text = Vec::new();
        let mut buf = [0; 4096];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            let keep = n.min(16384usize.saturating_sub(text.len()));
            text.extend_from_slice(&buf[..keep]);
        }
        String::from_utf8_lossy(&text).trim().to_owned()
    }));
    // A separate owner can interrupt a blocked pipe read when a view closes or
    // refreshes. The GUI never kills/waits on a child, and idle streams sleep.
    let stop = cancel.clone();
    let (status_tx, status_rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            if stop.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let _ = status_tx.send(Ok(status));
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = status_tx.send(Err(e));
                    return;
                }
            }
        }
        let _ = status_tx.send(child.wait());
    });
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
                let status = status_rx
                    .recv()
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
                let stderr = errors.take().unwrap().join().unwrap_or_default();
                (!status.success()).then(|| {
                    if stderr.is_empty() {
                        "Git history could not be read".into()
                    } else {
                        stderr
                    }
                })
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

pub struct HistoryView {
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
    #[cfg(test)]
    drawn: std::ops::Range<usize>,
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
            #[cfg(test)]
            drawn: 0..0,
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
        // Follow the theme font size (Appearance / Ctrl+Scroll), like the board.
        let s = crate::terminal::font_size(ui.ctx()) / crate::config::DEFAULT_FONT_SIZE;
        let rescroll = (s != self.scale).then(|| self.scroll_y * s / self.scale);
        self.scale = s;
        ui.painter().rect_filled(rect, 0.0, th.bg);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base)
                .max_rect(rect.shrink(8.0 * s))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        child.spacing_mut().button_padding *= s;
        child.spacing_mut().interact_size *= s;
        for font in child.style_mut().text_styles.values_mut() {
            font.size *= s;
        }
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
            refresh = ui.button("Refresh").clicked();
        });
        if refresh {
            let cwd = self.cwd.clone();
            let generation = self.generation + 1;
            // Retire potentially large cached histories off the GUI thread.
            let old = std::mem::replace(self, Self::new(cwd));
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
        let (row_h, lane_w) = (ROW_H * s, LANE_W * s);
        let graph_w = (self.width as f32 + 1.0) * lane_w;
        let total_w = (graph_w + 720.0 * s).max(child.available_width() - 12.0 * s);
        let author_x = total_w - 230.0 * s;
        let date_x = total_w - 90.0 * s;
        let font = egui::FontId::proportional(13.0 * s);
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
                    ui.allocate_exact_size(egui::vec2(total_w, row_h), egui::Sense::hover());
                let painter = ui.painter_at(r.intersect(ui.clip_rect()));
                if i % 2 == 0 {
                    painter.rect_filled(r, 0.0, th.sel_bg.gamma_multiply(0.22));
                }
                if response.hovered() {
                    painter.rect_filled(r, 0.0, th.sel_bg);
                }
                let x = |lane: usize| r.left() + (lane as f32 + 1.0) * lane_w;
                for (lane, color) in &row.incoming {
                    painter.line_segment(
                        [
                            egui::pos2(x(*lane), r.top()),
                            egui::pos2(x(*lane), r.center().y),
                        ],
                        egui::Stroke::new(1.7 * s, COLORS[*color % COLORS.len()]),
                    );
                }
                for edge in &row.outgoing {
                    painter.line_segment(
                        [
                            egui::pos2(x(edge.from), r.center().y),
                            egui::pos2(x(edge.to), r.bottom()),
                        ],
                        egui::Stroke::new(1.7 * s, COLORS[edge.color % COLORS.len()]),
                    );
                }
                painter.circle_filled(
                    egui::pos2(x(row.lane), r.center().y),
                    4.0 * s,
                    COLORS[row.color % COLORS.len()],
                );
                let text_rect = egui::Rect::from_min_max(
                    egui::pos2(r.left() + graph_w, r.top()),
                    egui::pos2(r.left() + author_x - 12.0 * s, r.bottom()),
                );
                let p = painter.with_clip_rect(text_rect.intersect(ui.clip_rect()));
                let mut left = text_rect.left();
                if !row.commit.refs.is_empty() {
                    let galley = p.layout_no_wrap(row.commit.refs.clone(), font.clone(), COLORS[0]);
                    let w = galley.size().x.min((text_rect.width() * 0.5).max(0.0));
                    let chip = egui::Rect::from_min_size(
                        egui::pos2(left, r.top() + 4.0 * s),
                        egui::vec2(w + 10.0 * s, 20.0 * s),
                    );
                    p.rect_filled(chip, 3.0 * s, th.sel_bg);
                    p.with_clip_rect(chip.intersect(text_rect).intersect(ui.clip_rect()))
                        .galley(chip.min + egui::vec2(5.0, 2.0) * s, galley, COLORS[0]);
                    left += w + 18.0 * s;
                }
                p.text(
                    egui::pos2(left, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    &row.commit.subject,
                    font.clone(),
                    th.text,
                );
                let author_rect = egui::Rect::from_min_max(
                    egui::pos2(r.left() + author_x, r.top()),
                    egui::pos2(r.left() + date_x - 10.0 * s, r.bottom()),
                );
                painter
                    .with_clip_rect(author_rect.intersect(ui.clip_rect()))
                    .text(
                        author_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        &row.commit.author,
                        font.clone(),
                        th.dim,
                    );
                painter.text(
                    egui::pos2(r.left() + date_x, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    &row.commit.date,
                    font.clone(),
                    th.dim,
                );
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
    fn git(dir: &std::path::Path, args: &[&str]) -> String {
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
            view.drawn.len()
        };
        let base = rows_at(crate::config::DEFAULT_FONT_SIZE);
        let zoomed = rows_at(crate::config::DEFAULT_FONT_SIZE * 2.0);
        assert_eq!(view.scale, 2.0);
        assert!(zoomed * 2 <= base + 2, "base {base}, zoomed {zoomed}");
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
