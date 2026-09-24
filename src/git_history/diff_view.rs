//! The Diff window: a reusable per-Project side-by-side view of one file's
//! change at one commit, loaded on a cancellable worker.
use super::diff::{self, Block, Diff, Doc, Kind, Notice, Row};
use super::file_tree::display_path;
use super::git;
use eframe::egui;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

/// Which pair of file versions a target compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// `commit` against its first parent (Git History).
    #[default]
    Commit,
    /// HEAD against the index (Git Changes).
    Staged,
    /// The index against the working copy; an unmerged file against HEAD.
    Unstaged,
    /// A file Git does not track, shown whole as added.
    Untracked,
}

/// One file's change: at one commit, or in the working tree. Built by the
/// details pane or the Git Changes window; persisted in `ContentSnap::GitDiff`,
/// so it carries no live state.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffTarget {
    pub stage: Stage,
    /// Empty unless `stage` is `Commit`.
    pub commit: String,
    /// First parent; `None` for a root commit.
    pub parent: Option<String>,
    /// `A M D R C T`, as in the details pane.
    pub status: char,
    /// Source path for renames and copies.
    pub old_path: Option<String>,
    pub path: String,
    pub merge: bool,
}

fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl DiffTarget {
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
    fn versus(&self) -> String {
        let short = |h: &str| h.get(..7).unwrap_or(h).to_owned();
        match (self.stage, self.status) {
            (Stage::Commit, _) => {}
            (Stage::Staged, 'A') => return "Staged · new file".into(),
            (Stage::Staged, 'D') => return "Staged · deleted file".into(),
            (Stage::Staged, _) => return "Staged vs HEAD".into(),
            (Stage::Unstaged, 'U') => return "Working copy vs HEAD · conflict".into(),
            (Stage::Unstaged, 'D') => return "Deleted from working copy".into(),
            (Stage::Unstaged, _) => return "Working copy vs index".into(),
            (Stage::Untracked, _) => return "Untracked · new file".into(),
        }
        match (self.status, &self.parent) {
            ('A', _) | (_, None) => format!("{} · new file", short(&self.commit)),
            ('D', _) => format!("{} · deleted file", short(&self.commit)),
            (_, Some(p)) if self.merge => {
                format!("{} vs {} (first parent)", short(&self.commit), short(p))
            }
            (_, Some(p)) => format!("{} vs {}", short(&self.commit), short(p)),
        }
    }
    /// Git arguments for this change. A/D/M use a pathspec-limited diff-tree
    /// (a submodule shows its gitlink header). T/R/C use the blob form, which
    /// pairs renamed/copied paths exactly; `load` resolves a T pair to bare
    /// blob ids so the mode change does not split it into delete + add.
    fn args(&self) -> Result<Vec<String>, String> {
        let mut args: Vec<String> = Vec::new();
        let flags = [
            "--no-ext-diff".to_owned(),
            "--no-textconv".into(),
            "--no-color".into(),
            format!("-U{}", diff::CONTEXT_LINES),
        ];
        let path = || ["--".to_owned(), self.path.clone()];
        match (self.stage, self.status) {
            (Stage::Commit, _) => {}
            // `:path` names the index blob; `load` resolves both specs to ids.
            (Stage::Staged, 'T' | 'R' | 'C') => {
                let old = self.old_path.as_deref().unwrap_or(&self.path);
                args.push("diff".into());
                args.extend(flags);
                args.extend([format!("HEAD:{old}"), format!(":{}", self.path)]);
                return Ok(args);
            }
            (Stage::Staged, _) => {
                args.extend(["diff".into(), "--cached".into()]);
                args.extend(flags);
                args.extend(path());
                return Ok(args);
            }
            (Stage::Unstaged, status) => {
                args.push("diff".into());
                args.extend(flags);
                // A conflicted index has no single stage to compare against.
                if status == 'U' {
                    args.push("HEAD".into());
                }
                args.extend(path());
                return Ok(args);
            }
            (Stage::Untracked, _) => return Err("Untracked files are not read through Git".into()),
        }
        if !is_object_id(&self.commit) || self.parent.as_deref().is_some_and(|p| !is_object_id(p)) {
            return Err("Invalid commit id".into());
        }
        match (self.status, &self.parent) {
            ('T' | 'R' | 'C', Some(parent)) => {
                let old = self.old_path.as_deref().unwrap_or(&self.path);
                args.push("diff".into());
                args.extend(flags);
                args.push(format!("{parent}:{old}"));
                args.push(format!("{}:{}", self.commit, self.path));
            }
            ('A' | 'D' | 'M', Some(parent)) => {
                args.extend(["diff-tree".into(), "-p".into()]);
                args.extend(flags);
                args.extend([
                    parent.clone(),
                    self.commit.clone(),
                    "--".into(),
                    self.path.clone(),
                ]);
            }
            (_, None) => {
                args.extend(["diff-tree".into(), "-p".into(), "--root".into()]);
                args.extend(flags);
                args.extend([self.commit.clone(), "--".into(), self.path.clone()]);
            }
            _ => return Err(format!("Unsupported change type {}", self.status)),
        }
        Ok(args)
    }
}

/// Worker-only: run the diff read and parse it.
pub(super) fn load(
    cwd: &Path,
    target: &DiffTarget,
    cancel: &Arc<AtomicBool>,
) -> Result<Diff, String> {
    if target.stage == Stage::Untracked {
        return untracked(cwd, &target.path);
    }
    let mut args = target.args()?;
    let blob_pair = match target.stage {
        Stage::Commit => target.status == 'T' && target.parent.is_some(),
        Stage::Staged => matches!(target.status, 'T' | 'R' | 'C'),
        _ => false,
    };
    if blob_pair {
        // git 2.39 still splits a `rev:path` pair whose modes differ into a
        // delete plus an add. Bare blob ids carry no mode: one content hunk.
        let specs = args.split_off(args.len() - 2);
        args.extend(blob_ids(cwd, &specs, cancel)?);
    }
    match run(cwd, &args, cancel, 16 << 20) {
        Ok(bytes) => diff::parse(&bytes),
        Err(git::GitError::TooLarge) => Ok(Diff::Notice(Notice::TooLarge)),
        Err(e) => Err(message(e)),
    }
}

/// An untracked file as an all-added diff, read straight from disk: `git diff
/// --no-index` exits 1 on any difference, which the helper reports as failure.
fn untracked(cwd: &Path, path: &str) -> Result<Diff, String> {
    if path.ends_with('/') {
        // `status` lists a nested repository as its directory.
        return Ok(Diff::Notice(Notice::Submodule));
    }
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("Invalid path".into());
    }
    let full = cwd.join(relative);
    let unreadable = |e: std::io::Error| format!("Cannot read {}: {e}", display_path(path));
    if std::fs::metadata(&full).map_err(unreadable)?.len() > UNTRACKED_CAP {
        return Ok(Diff::Notice(Notice::TooLarge));
    }
    diff::parse(&as_added(&std::fs::read(&full).map_err(unreadable)?))
}
const UNTRACKED_CAP: u64 = 16 << 20;

/// Unified-diff text adding `bytes` as a whole new file. Binary uses Git's
/// heuristic: a NUL in the first 8000 bytes.
fn as_added(bytes: &[u8]) -> Vec<u8> {
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return b"Binary files /dev/null and b/file differ\n".to_vec();
    }
    if bytes.is_empty() {
        return Vec::new();
    }
    let body = bytes.strip_suffix(b"\n");
    let lines = body.unwrap_or(bytes).split(|b| *b == b'\n');
    let count = lines.clone().count();
    let mut out = format!("--- /dev/null\n+++ b/file\n@@ -0,0 +1,{count} @@\n").into_bytes();
    out.reserve(bytes.len() + count);
    for line in lines {
        out.push(b'+');
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    if body.is_none() {
        out.extend_from_slice(b"\\ No newline at end of file\n");
    }
    out
}

fn run(
    cwd: &Path,
    args: &[String],
    cancel: &Arc<AtomicBool>,
    cap: usize,
) -> Result<Vec<u8>, git::GitError> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    git::output(cwd, &args, cancel, cap, Duration::from_secs(30))
}

fn message(e: git::GitError) -> String {
    match e {
        git::GitError::Failed(stderr) if stderr.is_empty() => "Git could not read this diff".into(),
        e => e.to_string(),
    }
}

/// Resolve `rev:path` specs to their object ids.
fn blob_ids(cwd: &Path, specs: &[String], cancel: &Arc<AtomicBool>) -> Result<Vec<String>, String> {
    let mut args = vec!["rev-parse".to_owned()];
    args.extend_from_slice(specs);
    let bytes = run(cwd, &args, cancel, 4096).map_err(message)?;
    let ids: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_owned)
        .collect();
    if ids.len() != specs.len() || !ids.iter().all(|id| is_object_id(id)) {
        return Err("Git could not read this diff".into());
    }
    Ok(ids)
}

const ROW_H: f32 = 18.0;
const GUTTER_W: f32 = 14.0;
const STRIP_W: f32 = 8.0;
const HBAR_H: f32 = 6.0;
/// Narrowest a text side may be dragged, in chars.
const MIN_SIDE_CHARS: f32 = 12.0;

struct Request {
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<Diff, String>>,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Scroll and navigation state; reset on every retarget.
#[derive(Default)]
struct Nav {
    /// Salts the ScrollArea id so a new target starts at the top.
    generation: u64,
    /// Jump to the first difference once the document arrives.
    first: bool,
    scroll_y: f32,
    hx: f32,
    /// Block the last prev/next landed on, valid while the offset is still
    /// `jumped_to`; any other scroll re-derives from the anchor row.
    current: Option<usize>,
    jumped_to: Option<f32>,
}

pub struct DiffView {
    cwd: Option<PathBuf>,
    target: Option<DiffTarget>,
    request: Option<Request>,
    result: Option<Result<Diff, String>>,
    nav: Nav,
    /// Old side's share of the text width. A fraction, not px, so it survives
    /// both retargets and window resizes.
    split: f32,
    scale: f32,
    #[cfg(test)]
    drawn: Range<usize>,
    /// Screen x of the old/new divider last frame.
    #[cfg(test)]
    divider_x: f32,
}
impl Drop for DiffView {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Next/previous block. While `current` is valid it steps from there;
/// otherwise the first block past (or last block before) the anchor row.
fn step_block(
    blocks: &[Block],
    current: Option<usize>,
    anchor: usize,
    forward: bool,
) -> Option<usize> {
    match (current, forward) {
        (Some(c), true) => (c + 1 < blocks.len()).then_some(c + 1),
        (Some(c), false) => c.checked_sub(1),
        (None, true) => blocks.iter().position(|b| b.rows.start > anchor),
        (None, false) => blocks.iter().rposition(|b| b.rows.start < anchor),
    }
}

/// Old and new text widths: `split` of `avail`, each at least `min` (less
/// only when `avail` cannot fit two minimums, then an even split).
fn side_widths(avail: f32, split: f32, min: f32) -> (f32, f32) {
    let min = min.min(avail / 2.0).max(0.0);
    let old = (avail * split).clamp(min, (avail - min).max(min));
    (old, (avail - old).max(0.0))
}

/// Byte range of chars `skip..skip + take` of `text`, on char boundaries.
/// Bounds per-frame layout to what fits in the column, however long the line.
fn visible(text: &str, skip: usize, take: usize) -> Range<usize> {
    let mut ends = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()));
    let start = ends.nth(skip).unwrap_or(text.len());
    if take == 0 {
        return start..start;
    }
    let end = ends.nth(take - 1).unwrap_or(text.len());
    start..end
}

/// `visible` for a parsed cell. ASCII text (char count = byte length) maps
/// columns straight to bytes, so a far horizontal scroll into a huge line
/// costs nothing; other text walks chars.
fn visible_cell(cell: &diff::Cell, skip: usize, take: usize) -> Range<usize> {
    let len = cell.text.len();
    if cell.chars == len {
        let start = skip.min(len);
        return start..start.saturating_add(take).min(len);
    }
    visible(&cell.text, skip, take)
}

impl DiffView {
    pub fn new(cwd: Option<PathBuf>) -> Self {
        Self {
            cwd,
            target: None,
            request: None,
            result: None,
            nav: Nav::default(),
            split: 0.5,
            scale: 1.0,
            #[cfg(test)]
            drawn: 0..0,
            #[cfg(test)]
            divider_x: 0.0,
        }
    }
    pub fn target(&self) -> Option<&DiffTarget> {
        self.target.as_ref()
    }
    /// Show `target`. The load starts lazily on the next `show`, so restore can
    /// call this without an egui context.
    pub fn retarget(&mut self, target: DiffTarget) {
        // The working tree moves under a fixed target: clicking it again re-reads.
        let failed = matches!(self.result, Some(Err(_)));
        let same = self.target.as_ref() == Some(&target) && target.stage == Stage::Commit;
        if same && !failed {
            return;
        }
        self.clear();
        self.target = Some(target);
        self.nav = Nav {
            generation: self.nav.generation + 1,
            first: true,
            ..Nav::default()
        };
    }
    /// Cancel any read and free a possibly large document off the GUI thread.
    fn clear(&mut self) {
        let request = self.request.take();
        // Cancel now, not when the background drop runs: the worker must stop
        // before a new request starts.
        if let Some(request) = &request {
            request.cancel.store(true, Ordering::Relaxed);
        }
        let result = self.result.take();
        if request.is_some() || result.is_some() {
            std::thread::spawn(move || {
                drop(request);
                drop(result);
            });
        }
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if self.result.is_some() {
            return;
        }
        let Some(target) = &self.target else { return };
        let Some(request) = &self.request else {
            let Some(cwd) = self.cwd.clone() else {
                self.result = Some(Err("This project has no directory".into()));
                return;
            };
            let (tx, receiver) = mpsc::sync_channel(1);
            let cancel = Arc::new(AtomicBool::new(false));
            let stop = cancel.clone();
            let target = target.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = load(&cwd, &target, &stop);
                if !stop.load(Ordering::Relaxed) {
                    let _ = tx.send(result);
                    ctx.request_repaint();
                }
            });
            self.request = Some(Request { cancel, receiver });
            return;
        };
        match request.receiver.try_recv() {
            Ok(result) => self.result = Some(result),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.result = Some(Err("Diff worker stopped".into()))
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, active: bool, base: egui::Id) {
        self.poll(ui.ctx());
        let th = crate::theme::live(ui.ctx());
        let zoom = crate::view_scale::ViewScale::from_ctx(ui.ctx());
        let s = zoom.factor();
        let rescale = (s != self.scale).then(|| self.nav.scroll_y * s / self.scale);
        self.scale = s;
        ui.painter().rect_filled(rect, 0.0, th.bg);
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base)
                .max_rect(rect.shrink(zoom.px(8.0)))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        zoom.apply(&mut ui);
        let Some(target) = &self.target else {
            ui.colored_label(th.dim, "Select a file in Git History.");
            return;
        };
        let doc = match &self.result {
            None => {
                header(&mut ui, target, None, &th);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading diff…");
                });
                return;
            }
            Some(Err(error)) => {
                header(&mut ui, target, None, &th);
                ui.colored_label(th.dim, error.as_str());
                return;
            }
            Some(Ok(Diff::Notice(notice))) => {
                header(&mut ui, target, None, &th);
                ui.colored_label(th.dim, notice.text());
                return;
            }
            Some(Ok(Diff::Doc(doc))) => doc,
        };
        let step = header(&mut ui, target, Some(doc.blocks.len()), &th);
        #[cfg(test)]
        let drawn = &mut self.drawn;
        #[cfg(not(test))]
        let drawn = &mut (0..0);
        let _divider_x = body(
            &mut ui,
            doc,
            &mut self.nav,
            &mut self.split,
            active,
            step,
            s,
            rescale,
            base,
            &th,
            drawn,
        );
        #[cfg(test)]
        {
            self.divider_x = _divider_x;
        }
    }
}

/// Path, versus label, difference count and prev/next. Returns a button step.
fn header(
    ui: &mut egui::Ui,
    target: &DiffTarget,
    blocks: Option<usize>,
    th: &crate::theme::Theme,
) -> Option<bool> {
    let mut step = None;
    ui.horizontal(|ui| {
        let path = match &target.old_path {
            Some(old) if old != &target.path => {
                format!("{} → {}", display_path(old), display_path(&target.path))
            }
            _ => display_path(&target.path),
        };
        ui.label(egui::RichText::new(path).color(th.text).strong());
        ui.label(egui::RichText::new(target.versus()).color(th.dim));
        if let Some(n) = blocks {
            let count = if n == 1 {
                "1 difference".to_owned()
            } else {
                format!("{n} differences")
            };
            ui.label(egui::RichText::new(count).color(th.dim));
            if ui
                .add_enabled(n > 0, egui::Button::new("▲"))
                .on_hover_text("Previous difference (Shift+F7)")
                .clicked()
            {
                step = Some(false);
            }
            if ui
                .add_enabled(n > 0, egui::Button::new("▼"))
                .on_hover_text("Next difference (F7)")
                .clicked()
            {
                step = Some(true);
            }
        }
    });
    step
}

/// The diff's own band palette. Deliberately NOT `file_tree::status_color`:
/// that is the file-tree legend and is being restyled separately (card zfw4je).
const REMOVED: egui::Color32 = egui::Color32::from_rgb(224, 118, 113);
const ADDED: egui::Color32 = egui::Color32::from_rgb(116, 190, 140);
const MODIFIED: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);

fn kind_color(kind: Kind) -> egui::Color32 {
    match kind {
        Kind::Removed => REMOVED,
        Kind::Added => ADDED,
        Kind::Modified | Kind::Same => MODIFIED,
    }
}

/// Column x-offsets from a row's left edge.
struct Cols {
    num_w: f32,
    old_w: f32,
    new_w: f32,
    gutter_w: f32,
    char_w: f32,
}
impl Cols {
    fn old_num(&self) -> f32 {
        0.0
    }
    fn old_text(&self) -> f32 {
        self.num_w
    }
    fn gutter(&self) -> f32 {
        self.num_w + self.old_w
    }
    fn new_num(&self) -> f32 {
        self.gutter() + self.gutter_w
    }
    fn new_text(&self) -> f32 {
        self.new_num() + self.num_w
    }
}

#[allow(clippy::too_many_arguments)]
fn body(
    ui: &mut egui::Ui,
    doc: &Doc,
    nav: &mut Nav,
    split: &mut f32,
    active: bool,
    mut step: Option<bool>,
    s: f32,
    rescale: Option<f32>,
    base: egui::Id,
    th: &crate::theme::Theme,
    drawn: &mut Range<usize>,
) -> f32 {
    let row_h = ROW_H * s;
    let font = egui::FontId::monospace(13.0 * s);
    let char_w = ui
        .painter()
        .layout_no_wrap("0".into(), font.clone(), egui::Color32::WHITE)
        .size()
        .x;
    let area = ui.available_rect_before_wrap();
    let strip =
        egui::Rect::from_min_max(egui::pos2(area.right() - STRIP_W * s, area.top()), area.max);
    let main = egui::Rect::from_min_max(
        area.min,
        egui::pos2(strip.left() - 2.0 * s, area.bottom() - HBAR_H * s),
    );
    let digits = doc.max_line.max(1).to_string().len() as f32;
    let num_w = (digits + 1.5) * char_w;
    let gutter_w = GUTTER_W * s;
    // Reserve the ScrollArea's vertical bar so the new side is not covered.
    let bar = ui.spacing().scroll.bar_width + ui.spacing().scroll.bar_outer_margin;
    let avail = (main.width() - bar - gutter_w - 2.0 * num_w).max(2.0 * char_w);
    let (old_w, new_w) = side_widths(avail, *split, MIN_SIDE_CHARS * char_w);
    let cols = Cols {
        num_w,
        old_w,
        new_w,
        gutter_w,
        char_w,
    };
    // One offset scrolls both sides; the narrower side must reach line ends.
    let side_w = old_w.min(new_w);
    let text_w = (doc.max_cols as f32 + 1.0) * char_w;
    let max_hx = (text_w - side_w).max(0.0);
    let view_h = main.height();

    if ui.rect_contains_pointer(main) {
        // egui already maps Shift+wheel to horizontal delta.
        nav.hx -= ui.input(|i| i.smooth_scroll_delta.x);
    }
    let mut target_y = rescale;
    if active {
        let page = (view_h / row_h).floor().max(1.0) * row_h;
        ui.input(|i| {
            let delta = if i.key_pressed(egui::Key::ArrowDown) {
                Some(row_h)
            } else if i.key_pressed(egui::Key::ArrowUp) {
                Some(-row_h)
            } else if i.key_pressed(egui::Key::PageDown) {
                Some(page)
            } else if i.key_pressed(egui::Key::PageUp) {
                Some(-page)
            } else {
                None
            };
            if let Some(d) = delta {
                target_y = Some(nav.scroll_y + d);
            }
            if i.key_pressed(egui::Key::Home) {
                target_y = Some(0.0);
            }
            if i.key_pressed(egui::Key::End) {
                target_y = Some(doc.rows.len() as f32 * row_h);
            }
            if i.key_pressed(egui::Key::F7) {
                step = Some(!i.modifiers.shift);
            }
        });
    }
    if nav.jumped_to.is_none_or(|y| (y - nav.scroll_y).abs() > 0.5) {
        nav.current = None;
    }
    let mut jumped = false;
    if std::mem::take(&mut nav.first) && !doc.blocks.is_empty() {
        nav.current = Some(0);
        target_y = Some(doc.blocks[0].rows.start as f32 * row_h - view_h / 3.0);
        jumped = true;
    } else if let Some(forward) = step {
        let anchor = ((nav.scroll_y + view_h / 3.0) / row_h).round() as usize;
        if let Some(k) = step_block(&doc.blocks, nav.current, anchor, forward) {
            nav.current = Some(k);
            target_y = Some(doc.blocks[k].rows.start as f32 * row_h - view_h / 3.0);
            jumped = true;
        }
    }

    // Marker strip, painted from last frame's offset; a click scrolls there.
    let total_h = (doc.rows.len().max(1) as f32) * row_h;
    let p = ui.painter_at(strip);
    p.rect_filled(strip, 0.0, th.border.gamma_multiply(0.4));
    let n = doc.rows.len().max(1) as f32;
    for b in &doc.blocks {
        let y0 = strip.top() + b.rows.start as f32 / n * strip.height();
        let y1 = (strip.top() + b.rows.end as f32 / n * strip.height()).max(y0 + 2.0);
        p.rect_filled(
            egui::Rect::from_x_y_ranges(strip.x_range(), y0..=y1),
            0.0,
            kind_color(b.kind),
        );
    }
    let vy0 = strip.top() + nav.scroll_y / total_h * strip.height();
    let vy1 = vy0 + (view_h / total_h).min(1.0) * strip.height();
    p.rect_stroke(
        egui::Rect::from_x_y_ranges(strip.x_range(), vy0..=vy1),
        0.0,
        egui::Stroke::new(1.0, th.dim),
        egui::StrokeKind::Inside,
    );
    let hit = ui.interact(
        strip,
        base.with("diff-strip"),
        egui::Sense::click_and_drag(),
    );
    if let Some(pos) = hit
        .interact_pointer_pos()
        .filter(|_| hit.clicked() || hit.dragged())
    {
        let f = ((pos.y - strip.top()) / strip.height()).clamp(0.0, 1.0);
        target_y = Some(f * total_h - view_h / 2.0);
    }

    // Horizontal bar under the text columns; drags both sides together.
    let track = egui::Rect::from_min_max(
        egui::pos2(main.left(), main.bottom()),
        egui::pos2(main.right(), area.bottom()),
    );
    if max_hx > 0.0 {
        let thumb_w = (track.width() * side_w / text_w).max(12.0 * s);
        let span = (track.width() - thumb_w).max(1.0);
        let drag = ui.interact(track, base.with("diff-hbar"), egui::Sense::drag());
        nav.hx += drag.drag_delta().x * max_hx / span;
        nav.hx = nav.hx.clamp(0.0, max_hx);
        let x = track.left() + nav.hx / max_hx * span;
        ui.painter_at(track).rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(x, track.top() + 1.0),
                egui::vec2(thumb_w, track.height() - 2.0),
            ),
            2.0 * s,
            th.dim.gamma_multiply(0.6),
        );
    }
    nav.hx = nav.hx.clamp(0.0, max_hx);

    let mut rows_ui = ui.new_child(egui::UiBuilder::new().id_salt("diff-rows").max_rect(main));
    rows_ui.set_clip_rect(main.intersect(ui.clip_rect()));
    rows_ui.spacing_mut().item_spacing.y = 0.0;
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt((base, nav.generation))
        .auto_shrink([false, false]);
    if let Some(y) = target_y {
        scroll = scroll.vertical_scroll_offset(y.max(0.0));
    }
    let hx = nav.hx;
    let out = scroll.show_rows(&mut rows_ui, row_h, doc.rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        *drawn = range.clone();
        for i in range {
            let Some(row) = doc.rows.get(i) else { break };
            let (r, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), row_h),
                egui::Sense::hover(),
            );
            let block = doc.blocks.partition_point(|b| b.rows.end <= i);
            let block = doc
                .blocks
                .get(block)
                .filter(|b| b.rows.contains(&i))
                .map(|b| b.kind);
            paint_row(ui.painter(), r, row, block, &cols, hx, &font, th);
        }
    });
    nav.scroll_y = out.state.offset.y;
    if jumped {
        nav.jumped_to = Some(nav.scroll_y);
    }

    // Old/new divider over the connector gutter. Registered after the rows so
    // it wins the hit test over the ScrollArea's own drag.
    let gutter_x = main.left() + cols.gutter();
    let handle = egui::Rect::from_x_y_ranges(gutter_x..=gutter_x + gutter_w, main.y_range());
    let response = ui
        .interact(
            handle,
            base.with("diff-divider"),
            egui::Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Drag to resize; double-click to even");
    let hot = response.hovered() || response.dragged();
    ui.painter_at(main).vline(
        handle.center().x,
        main.y_range(),
        egui::Stroke::new(1.0, if hot { th.dim } else { th.border }),
    );
    if response.double_clicked() {
        *split = 0.5;
        ui.ctx().request_repaint();
    } else if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let old = pos.x - gutter_w / 2.0 - main.left() - num_w;
        let (old, _) = side_widths(avail, old / avail, MIN_SIDE_CHARS * char_w);
        *split = old / avail;
        ui.ctx().request_repaint();
    }
    handle.center().x
}

#[allow(clippy::too_many_arguments)]
fn paint_row(
    p: &egui::Painter,
    r: egui::Rect,
    row: &Row,
    block: Option<Kind>,
    cols: &Cols,
    hx: f32,
    font: &egui::FontId,
    th: &crate::theme::Theme,
) {
    let tint_for = |side_old: bool| match row.kind {
        Kind::Same => None,
        Kind::Modified => Some(kind_color(Kind::Modified)),
        Kind::Removed if side_old => Some(kind_color(Kind::Removed)),
        Kind::Added if !side_old => Some(kind_color(Kind::Added)),
        _ => None,
    };
    let x = |off: f32| r.left() + off;
    for (cell, num_x, text_x, side_w, old_side) in [
        (&row.old, cols.old_num(), cols.old_text(), cols.old_w, true),
        (&row.new, cols.new_num(), cols.new_text(), cols.new_w, false),
    ] {
        let side = egui::Rect::from_min_max(
            egui::pos2(x(num_x), r.top()),
            egui::pos2(x(text_x + side_w), r.bottom()),
        );
        let Some(cell) = cell else {
            if row.kind != Kind::Same {
                p.rect_filled(side, 0.0, th.dim.gamma_multiply(0.06));
            }
            continue;
        };
        let tint = tint_for(old_side);
        if let Some(t) = tint {
            p.rect_filled(side, 0.0, t.gamma_multiply(0.14));
        }
        p.text(
            egui::pos2(x(num_x + cols.num_w - cols.char_w * 0.75), r.center().y),
            egui::Align2::RIGHT_CENTER,
            cell.line.to_string(),
            font.clone(),
            th.dim,
        );
        let text_rect = egui::Rect::from_min_max(egui::pos2(x(text_x), r.top()), side.max);
        let tp = p.with_clip_rect(text_rect.intersect(p.clip_rect()));
        let x0 = text_rect.left() - hx;
        // Char offsets come from the parser: no per-frame scan of the line.
        let col_x = |chars: usize| x0 + chars as f32 * cols.char_w;
        if let (Some(t), Some(hot)) = (tint, &cell.hot_chars) {
            tp.rect_filled(
                egui::Rect::from_x_y_ranges(col_x(hot.start)..=col_x(hot.end), r.y_range()),
                0.0,
                t.gamma_multiply(0.35),
            );
        }
        let skip = (hx / cols.char_w) as usize;
        let take = (side_w / cols.char_w) as usize + 2;
        let vis = visible_cell(cell, skip, take);
        tp.text(
            egui::pos2(x0 + skip as f32 * cols.char_w, r.center().y),
            egui::Align2::LEFT_CENTER,
            &cell.text[vis],
            font.clone(),
            th.text,
        );
        if cell.no_eol {
            tp.text(
                egui::pos2(col_x(cell.chars) + cols.char_w, r.center().y),
                egui::Align2::LEFT_CENTER,
                "(no newline)",
                font.clone(),
                th.dim,
            );
        }
    }
    if let Some(k) = block {
        let g = egui::Rect::from_min_max(
            egui::pos2(x(cols.gutter()), r.top()),
            egui::pos2(x(cols.gutter() + cols.gutter_w), r.bottom()),
        );
        p.rect_filled(g, 0.0, kind_color(k).gamma_multiply(0.30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Diff Test"],
            &["config", "user.email", "diff@example.test"],
            &["config", "commit.gpgsign", "false"],
            &["config", "core.autocrlf", "false"],
        ] {
            git(repo.path(), args);
        }
        repo
    }
    fn lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }
    fn flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    fn target(dir: &Path, status: char, old: Option<&str>, path: &str) -> DiffTarget {
        DiffTarget {
            stage: Stage::Commit,
            commit: git(dir, &["rev-parse", "HEAD"]),
            parent: Some(git(dir, &["rev-parse", "HEAD^"])),
            status,
            old_path: old.map(Into::into),
            path: path.into(),
            merge: false,
        }
    }
    fn doc(r: Result<Diff, String>) -> diff::Doc {
        match r.unwrap() {
            Diff::Doc(d) => d,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn real_changes_of_every_status_load_as_side_by_side_rows() {
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join("m.txt"), lines(1000)).unwrap();
        std::fs::write(dir.join("d.txt"), "gone\n").unwrap();
        std::fs::write(dir.join("old.txt"), lines(50)).unwrap();
        std::fs::write(dir.join("same.txt"), "unchanged\n").unwrap();
        std::fs::write(dir.join("t.txt"), "plain\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);
        let root = DiffTarget {
            stage: Stage::Commit,
            commit: git(dir, &["rev-parse", "HEAD"]),
            parent: None,
            status: 'A',
            old_path: None,
            path: "d.txt".into(),
            merge: false,
        };
        let d = doc(load(dir, &root, &flag()));
        assert_eq!(d.rows.len(), 1);
        assert_eq!(d.rows[0].kind, diff::Kind::Added);

        // Edits 890 lines apart must still be one whole-file hunk (bounded -U).
        let m = lines(1000)
            .replace("line 10\n", "line ten\n")
            .replace("line 900\n", "line nine hundred\n");
        std::fs::write(dir.join("m.txt"), m).unwrap();
        std::fs::remove_file(dir.join("d.txt")).unwrap();
        std::fs::write(dir.join("a.txt"), "new\nfile\n").unwrap();
        git(dir, &["mv", "old.txt", "new.txt"]);
        std::fs::write(
            dir.join("new.txt"),
            lines(50).replace("line 25\n", "line 25!\n"),
        )
        .unwrap();
        git(dir, &["mv", "same.txt", "same2.txt"]);
        git(dir, &["add", "-A", "."]);
        // Type change without a real symlink: stage mode 120000 directly.
        std::fs::write(dir.join("link.tmp"), "link-target").unwrap();
        let blob = git(dir, &["hash-object", "-w", "link.tmp"]);
        std::fs::remove_file(dir.join("link.tmp")).unwrap();
        git(
            dir,
            &[
                "update-index",
                "--cacheinfo",
                &format!("120000,{blob},t.txt"),
            ],
        );
        git(dir, &["commit", "-m", "second"]);
        let before = git(dir, &["status", "--porcelain=v1"]);

        let m = doc(load(dir, &target(dir, 'M', None, "m.txt"), &flag()));
        assert_eq!(m.rows.len(), 1000);
        assert_eq!(m.blocks.len(), 2);
        assert_eq!(m.blocks[0].rows, 9..10);
        let row = &m.rows[9];
        assert_eq!(row.kind, diff::Kind::Modified);
        let new = row.new.as_ref().unwrap();
        assert_eq!(&new.text[new.hot.clone().unwrap()], "ten");

        let d = doc(load(dir, &target(dir, 'D', None, "d.txt"), &flag()));
        assert_eq!(d.rows[0].kind, diff::Kind::Removed);
        let a = doc(load(dir, &target(dir, 'A', None, "a.txt"), &flag()));
        assert_eq!(a.rows.len(), 2);
        let r = doc(load(
            dir,
            &target(dir, 'R', Some("old.txt"), "new.txt"),
            &flag(),
        ));
        assert_eq!(r.blocks.len(), 1);
        assert_eq!(r.blocks[0].rows, 24..25);
        let c = doc(load(
            dir,
            &target(dir, 'C', Some("old.txt"), "new.txt"),
            &flag(),
        ));
        assert_eq!(c.blocks, r.blocks);
        assert_eq!(
            load(
                dir,
                &target(dir, 'R', Some("same.txt"), "same2.txt"),
                &flag()
            )
            .unwrap(),
            Diff::Notice(Notice::Unchanged)
        );
        let t = doc(load(dir, &target(dir, 'T', None, "t.txt"), &flag()));
        assert_eq!(t.blocks.len(), 1);
        assert_eq!(
            git(dir, &["status", "--porcelain=v1"]),
            before,
            "diff reads must not write"
        );
    }

    #[test]
    fn files_over_the_line_cap_are_a_notice_not_a_partial_render() {
        let repo = repo();
        let dir = repo.path();
        let n = diff::CONTEXT_LINES + 2;
        std::fs::write(dir.join("big.txt"), lines(n)).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);
        // Only the last line changes, so the hunk would start at line
        // n - CONTEXT_LINES = 2: git cannot return the whole file as one hunk.
        // (A change at BOTH ends would be one complete hunk, correctly rendered.)
        let edited = lines(n).replace(&format!("line {n}\n"), "last\n");
        std::fs::write(dir.join("big.txt"), edited).unwrap();
        git(dir, &["commit", "-am", "edit ends"]);
        assert_eq!(
            load(dir, &target(dir, 'M', None, "big.txt"), &flag()).unwrap(),
            Diff::Notice(Notice::TooLarge)
        );
    }

    fn worktree(stage: Stage, status: char, old: Option<&str>, path: &str) -> DiffTarget {
        DiffTarget {
            stage,
            commit: String::new(),
            parent: None,
            status,
            old_path: old.map(Into::into),
            path: path.into(),
            merge: false,
        }
    }
    fn texts(d: &diff::Doc) -> Vec<(diff::Kind, Option<&str>, Option<&str>)> {
        d.rows
            .iter()
            .map(|r| {
                let old = r.old.as_ref().map(|c| c.text.as_str());
                (r.kind, old, r.new.as_ref().map(|c| c.text.as_str()))
            })
            .collect()
    }

    #[test]
    fn working_tree_stages_diff_the_right_pair_without_writes() {
        use diff::Kind::*;
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("old.txt"), lines(20)).unwrap();
        std::fs::write(dir.join("gone.txt"), "x\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);
        std::fs::write(dir.join("a.txt"), "one\nTWO\n").unwrap();
        git(dir, &["add", "a.txt"]);
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        git(dir, &["mv", "old.txt", "new.txt"]);
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        std::fs::write(dir.join("u.txt"), "hello\r\nworld").unwrap();
        std::fs::write(dir.join("bin.dat"), b"a\0b").unwrap();
        std::fs::write(dir.join("empty.txt"), "").unwrap();
        let before = git(dir, &["status", "--porcelain=v1"]);

        let staged = doc(load(
            dir,
            &worktree(Stage::Staged, 'M', None, "a.txt"),
            &flag(),
        ));
        assert_eq!(
            texts(&staged),
            [
                (Same, Some("one"), Some("one")),
                (Modified, Some("two"), Some("TWO"))
            ]
        );
        let unstaged = doc(load(
            dir,
            &worktree(Stage::Unstaged, 'M', None, "a.txt"),
            &flag(),
        ));
        assert_eq!(texts(&unstaged)[2], (Added, None, Some("three")));
        let renamed = worktree(Stage::Staged, 'R', Some("old.txt"), "new.txt");
        assert_eq!(
            load(dir, &renamed, &flag()).unwrap(),
            Diff::Notice(Notice::Unchanged)
        );
        let deleted = doc(load(
            dir,
            &worktree(Stage::Unstaged, 'D', None, "gone.txt"),
            &flag(),
        ));
        assert_eq!(texts(&deleted), [(Removed, Some("x"), None)]);
        let new = doc(load(
            dir,
            &worktree(Stage::Untracked, '?', None, "u.txt"),
            &flag(),
        ));
        assert_eq!(
            texts(&new),
            [(Added, None, Some("hello")), (Added, None, Some("world"))]
        );
        assert!(new.rows[1].new.as_ref().unwrap().no_eol);
        for (path, notice) in [
            ("bin.dat", Notice::Binary),
            ("empty.txt", Notice::Unchanged),
            ("nested/", Notice::Submodule),
        ] {
            let t = worktree(Stage::Untracked, '?', None, path);
            assert_eq!(load(dir, &t, &flag()).unwrap(), Diff::Notice(notice));
        }
        for path in ["../outside.txt", "missing.txt"] {
            assert!(load(dir, &worktree(Stage::Untracked, '?', None, path), &flag()).is_err());
        }
        assert_eq!(git(dir, &["status", "--porcelain=v1"]), before);
    }

    #[test]
    fn working_tree_targets_label_and_reread_on_retarget() {
        let conflict = worktree(Stage::Unstaged, 'U', None, "c.txt");
        assert!(conflict.args().unwrap().contains(&"HEAD".to_owned()));
        assert_eq!(conflict.versus(), "Working copy vs HEAD · conflict");
        assert_eq!(
            worktree(Stage::Staged, 'M', None, "f").versus(),
            "Staged vs HEAD"
        );
        assert_eq!(
            worktree(Stage::Unstaged, 'M', None, "f").versus(),
            "Working copy vs index"
        );
        assert_eq!(
            worktree(Stage::Untracked, '?', None, "f").versus(),
            "Untracked · new file"
        );
        let mut view = DiffView::new(Some(PathBuf::new()));
        let target = worktree(Stage::Unstaged, 'M', None, "f");
        view.retarget(target.clone());
        view.result = Some(Ok(Diff::Notice(Notice::Unchanged)));
        view.retarget(target);
        assert!(
            view.result.is_none(),
            "same working-tree target must re-read"
        );
    }

    #[test]
    fn missing_commits_and_bad_ids_are_errors_not_panics() {
        let repo = repo();
        let dir = repo.path();
        git(dir, &["commit", "--allow-empty", "-m", "root"]);
        let mut t = DiffTarget {
            stage: Stage::Commit,
            commit: "0".repeat(40),
            parent: Some("1".repeat(40)),
            status: 'M',
            old_path: None,
            path: "x.txt".into(),
            merge: false,
        };
        assert!(!load(dir, &t, &flag()).unwrap_err().is_empty());
        t.commit = "--output=pwned".into();
        assert_eq!(load(dir, &t, &flag()).unwrap_err(), "Invalid commit id");
        let not_repo = tempfile::tempdir().unwrap();
        t.commit = "0".repeat(40);
        assert!(load(not_repo.path(), &t, &flag()).is_err());
    }

    #[test]
    fn target_labels() {
        let t = DiffTarget {
            stage: Stage::Commit,
            commit: "abcdef0123".repeat(4),
            parent: Some("1234567890".repeat(4)),
            status: 'M',
            old_path: None,
            path: "src/dir/file.rs".into(),
            merge: true,
        };
        assert_eq!(t.file_name(), "file.rs");
        assert_eq!(t.versus(), "abcdef0 vs 1234567 (first parent)");
        let added = DiffTarget {
            status: 'A',
            parent: None,
            merge: false,
            ..t
        };
        assert_eq!(added.versus(), "abcdef0 · new file");
    }

    fn synthetic(n: usize, changed: &[usize]) -> diff::Doc {
        let mut s = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,{n} +1,{n} @@\n");
        for i in 1..=n {
            if changed.contains(&i) {
                s += &format!("-line {i}\n+LINE {i}\n");
            } else {
                s += &format!(" line {i}\n");
            }
        }
        match diff::parse(s.as_bytes()).unwrap() {
            Diff::Doc(d) => d,
            other => panic!("{other:?}"),
        }
    }
    fn loaded(doc: diff::Doc) -> DiffView {
        let mut view = DiffView::new(None);
        view.target = Some(DiffTarget {
            stage: Stage::Commit,
            commit: "a".repeat(40),
            parent: Some("b".repeat(40)),
            status: 'M',
            old_path: None,
            path: "f".into(),
            merge: false,
        });
        view.result = Some(Ok(Diff::Doc(doc)));
        view.nav.first = true; // what retarget would set
        view
    }
    fn run(ctx: &egui::Context, view: &mut DiffView, events: Vec<egui::Event>) {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        // egui reads held modifiers from RawInput, not from the key event;
        // the real backend sets both.
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                egui::Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(rect),
                modifiers,
                events,
                ..Default::default()
            },
            |ui| view.show(ui, rect, true, egui::Id::new("diff")),
        );
    }
    fn key(key: egui::Key, shift: bool) -> Vec<egui::Event> {
        let modifiers = if shift {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]
    }

    #[test]
    fn step_block_follows_current_then_falls_back_to_anchor() {
        let b = |r: std::ops::Range<usize>| diff::Block {
            rows: r,
            kind: diff::Kind::Modified,
        };
        let blocks = [b(2..3), b(10..12), b(40..41)];
        assert_eq!(step_block(&blocks, Some(0), 99, true), Some(1));
        assert_eq!(step_block(&blocks, Some(2), 0, true), None);
        assert_eq!(step_block(&blocks, Some(1), 0, false), Some(0));
        assert_eq!(step_block(&blocks, Some(0), 0, false), None);
        // Free scroll: anchor row decides.
        assert_eq!(step_block(&blocks, None, 10, true), Some(2));
        assert_eq!(step_block(&blocks, None, 10, false), Some(0));
        assert_eq!(step_block(&blocks, None, 0, true), Some(0));
        assert_eq!(step_block(&[], None, 0, true), None);
    }

    #[test]
    fn visible_slice_respects_char_boundaries() {
        let s = "héllo wörld";
        assert_eq!(&s[visible(s, 1, 2)], "él");
        assert_eq!(&s[visible(s, 7, 100)], "örld");
        assert_eq!(visible(s, 100, 5), s.len()..s.len());
        assert_eq!(visible(s, 2, 0), 3..3);
        let cjk = "日本語";
        assert_eq!(&cjk[visible(cjk, 1, 1)], "本");
    }

    #[test]
    fn visible_cell_matches_visible_for_ascii_and_multibyte() {
        let cell = |text: &str| diff::Cell {
            line: 1,
            text: text.into(),
            hot: None,
            hot_chars: None,
            chars: text.chars().count(),
            no_eol: false,
        };
        for text in ["", "plain ascii line", "héllo wörld", "日本語 text"] {
            let c = cell(text);
            for skip in [0, 1, 3, 7, 40] {
                for take in [0, 1, 2, 5, 100] {
                    assert_eq!(visible_cell(&c, skip, take), visible(text, skip, take));
                }
            }
        }
        let huge = cell(&"x".repeat(1_000_000));
        assert_eq!(visible_cell(&huge, 999_990, 120), 999_990..1_000_000);
        assert_eq!(
            visible_cell(&huge, usize::MAX, usize::MAX),
            1_000_000..1_000_000
        );
    }

    #[test]
    fn visible_slice_bounds_huge_lines() {
        let line = "x".repeat(1_000_000);
        let r = visible(&line, 500_000, 120);
        assert_eq!(r.len(), 120);
    }

    #[test]
    fn side_widths_split_and_respect_minimums() {
        assert_eq!(side_widths(800.0, 0.5, 100.0), (400.0, 400.0));
        assert_eq!(side_widths(800.0, 0.25, 100.0), (200.0, 600.0));
        assert_eq!(side_widths(800.0, 0.0, 100.0), (100.0, 700.0));
        assert_eq!(side_widths(800.0, 1.0, 100.0), (700.0, 100.0));
        // Too narrow for two minimums: even, whatever the split.
        assert_eq!(side_widths(150.0, 0.9, 100.0), (75.0, 75.0));
        assert_eq!(side_widths(0.0, 0.3, 100.0), (0.0, 0.0));
    }

    fn drag(ctx: &egui::Context, view: &mut DiffView, from: egui::Pos2, to: egui::Pos2) {
        let press = |pos, pressed| {
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        run(ctx, view, vec![egui::Event::PointerMoved(from)]);
        run(ctx, view, press(from, true));
        run(ctx, view, vec![egui::Event::PointerMoved(to)]);
        run(ctx, view, press(to, false));
        run(ctx, view, vec![]);
    }

    #[test]
    fn divider_drags_and_survives_retarget_and_resize() {
        let ctx = egui::Context::default();
        let mut view = loaded(synthetic(200, &[5]));
        run(&ctx, &mut view, vec![]);
        run(&ctx, &mut view, vec![]);
        let even = view.divider_x;
        let from = egui::pos2(even, 300.0);
        drag(&ctx, &mut view, from, from + egui::vec2(-200.0, 0.0));
        assert!(
            (view.divider_x - (even - 200.0)).abs() < 1.0,
            "{even} -> {}",
            view.divider_x
        );
        let split = view.split;
        assert!(split < 0.4, "{split}");
        // Dragging far past the edge stops at the minimum, not off-screen.
        let at = egui::pos2(view.divider_x, 300.0);
        drag(&ctx, &mut view, at, egui::pos2(-500.0, 300.0));
        assert!(
            view.split > 0.0 && view.divider_x > 100.0,
            "{}",
            view.divider_x
        );
        let at = egui::pos2(view.divider_x, 300.0);
        drag(&ctx, &mut view, at, from + egui::vec2(-200.0, 0.0));
        assert!((view.split - split).abs() < 0.01);
        // Switching files keeps the split; navigation still lands.
        let mut next = view.target.clone().unwrap();
        next.path = "g".into();
        view.retarget(next);
        view.result = Some(Ok(Diff::Doc(synthetic(5000, &[3000]))));
        run(&ctx, &mut view, vec![]);
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&2999), "{:?}", view.drawn);
        assert!((view.split - split).abs() < 0.01);
        // A window resize keeps the proportion rather than the px offset.
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 600.0));
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            },
            |ui| view.show(ui, rect, true, egui::Id::new("diff")),
        );
        assert!((view.split - split).abs() < 0.01);
        assert!(view.divider_x < 300.0, "{}", view.divider_x);
    }

    #[test]
    fn opens_on_first_difference_and_f7_walks_blocks() {
        let ctx = egui::Context::default();
        let mut view = loaded(synthetic(5000, &[100, 2000, 4000]));
        run(&ctx, &mut view, vec![]);
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&99), "{:?}", view.drawn);
        run(&ctx, &mut view, key(egui::Key::F7, false));
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&1999), "{:?}", view.drawn);
        run(&ctx, &mut view, key(egui::Key::F7, false));
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&3999), "{:?}", view.drawn);
        run(&ctx, &mut view, key(egui::Key::F7, true));
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&1999), "{:?}", view.drawn);
        run(&ctx, &mut view, key(egui::Key::Home, false));
        run(&ctx, &mut view, vec![]);
        assert_eq!(view.drawn.start, 0);
    }

    #[test]
    fn large_diff_paints_only_viewport_rows() {
        let ctx = egui::Context::default();
        let start = std::time::Instant::now();
        let mut view = loaded(synthetic(100_000, &[50_000]));
        let parse_time = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..12 {
            run(&ctx, &mut view, vec![]);
            assert!(view.drawn.len() < 60, "{:?}", view.drawn);
        }
        assert!(view.drawn.contains(&49_999), "{:?}", view.drawn);
        eprintln!(
            "100k-line parse {parse_time:?}; 12 headless debug frames {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn rows_follow_theme_font_size() {
        let ctx = egui::Context::default();
        let mut view = loaded(synthetic(2000, &[1]));
        let mut rows_at = |px: f32| {
            crate::terminal::set_font_size(&ctx, px);
            for _ in 0..2 {
                run(&ctx, &mut view, vec![]);
            }
            view.drawn.len()
        };
        let base = rows_at(crate::config::DEFAULT_FONT_SIZE);
        let zoomed = rows_at(crate::config::DEFAULT_FONT_SIZE * 2.0);
        assert!(zoomed * 2 <= base + 2, "base {base}, zoomed {zoomed}");
    }

    #[test]
    fn retarget_cancels_the_previous_request() {
        let mut view = DiffView::new(Some(PathBuf::new()));
        let (_tx, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        view.target = Some(DiffTarget {
            stage: Stage::Commit,
            commit: "a".repeat(40),
            parent: None,
            status: 'A',
            old_path: None,
            path: "old".into(),
            merge: false,
        });
        view.request = Some(Request {
            cancel: cancel.clone(),
            receiver,
        });
        let mut next = view.target.clone().unwrap();
        next.path = "new".into();
        view.retarget(next.clone());
        assert!(cancel.load(Ordering::Relaxed));
        assert!(view.request.is_none() && view.result.is_none());
        assert_eq!(view.target(), Some(&next));
        // Same target again is a no-op (no restart) unless the last load failed.
        view.retarget(next.clone());
        assert_eq!(view.nav.generation, 1);
        view.result = Some(Err("boom".into()));
        view.retarget(next);
        assert!(view.result.is_none());
        assert_eq!(view.nav.generation, 2);
    }
}
