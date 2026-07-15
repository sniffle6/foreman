//! Bounded scrollback search — pure model over alacritty's `RegexSearch`.
//!
//! One shared line + wall-time budget per UI-frame tick. Query changes rebuild
//! immediately; content/resize changes wait for a quiescence window that slides
//! only when a *new* generation is observed. Counting walks from oldest history;
//! seeks wrap exactly once and stop after a full traversal.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexSearch};
use eframe::egui;

/// Max lines scanned across seek + count + visible work in one `tick`.
pub const SCAN_LINE_BUDGET: usize = 1000;
/// Soft wall-time budget per tick (dense regexes stop early).
pub const TICK_TIME_BUDGET_MS: u64 = 4;
/// Cap reported match count; display as `100000+` only after one more hit.
pub const COUNT_CAP: usize = 100_000;
/// Quiescence before auto-restarting a scan after continuous output (ms).
pub const OUTPUT_QUIESCE_MS: u64 = 80;

/// UI phase of the search bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchPhase {
    Closed,
    /// Search field focused; typing edits the query as a regex (smart-case).
    Editing,
    /// Query confirmed; `n`/`N`/Enter navigate without mutating the query.
    Navigating,
}

/// One match span in buffer coordinates (inclusive).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchSpan {
    pub start: Point,
    pub end: Point,
}

impl MatchSpan {
    pub fn from_match(m: &Match) -> Self {
        Self {
            start: *m.start(),
            end: *m.end(),
        }
    }

    /// Lexicographic order in buffer space (line then column).
    pub fn cmp_pos(a: Point, b: Point) -> std::cmp::Ordering {
        (a.line.0, a.column.0).cmp(&(b.line.0, b.column.0))
    }

    pub fn same_as(&self, other: &MatchSpan) -> bool {
        self.start == other.start && self.end == other.end
    }
}

/// Cache identity: content/resize invalidates match points; scroll only
/// rebuilds visible ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchGen {
    pub content_gen: u64,
    pub cols: usize,
    pub rows: usize,
}

/// View the terminal adapter paints / binds to the search bar.
#[derive(Clone, Debug)]
pub struct SearchView {
    pub phase: SearchPhase,
    pub query: String,
    /// Focused match index (1-based for display), if any.
    pub current: Option<usize>,
    /// Total matches found so far (capped).
    pub total: usize,
    pub total_capped: bool,
    pub invalid_regex: bool,
    pub scanning: bool,
    pub focused: Option<MatchSpan>,
    /// Matches intersecting the current viewport (buffer coords).
    pub visible: Vec<MatchSpan>,
}

impl Default for SearchView {
    fn default() -> Self {
        Self {
            phase: SearchPhase::Closed,
            query: String::new(),
            current: None,
            total: 0,
            total_capped: false,
            invalid_regex: false,
            scanning: false,
            focused: None,
            visible: Vec::new(),
        }
    }
}

/// Commands the adapter may issue after reading egui events.
#[derive(Clone, Debug)]
pub enum SearchCmd {
    Open,
    Close,
    SetQuery(String),
    Confirm,
    Next,
    Prev,
    /// Re-enter editing while keeping the query.
    Edit,
}

/// In-progress bounded search (focus or next/prev) that may span many ticks.
struct PendingSeek {
    dir: Direction,
    origin: Point,
    /// True after we wrapped once past the buffer edge.
    wrapped: bool,
    /// Lines of buffer already walked this seek (cap = total_lines).
    lines_walked: usize,
}

/// Internal scan cursor / engine state.
struct Engine {
    regex: RegexSearch,
    /// Next origin for the full-buffer count walk (always starts at topmost).
    scan_origin: Point,
    scan_done: bool,
    /// Matches counted so far (not stored). Count stops at COUNT_CAP+1 so we
    /// can prove a `+` suffix only after an extra hit beyond the display cap.
    count: usize,
    capped: bool,
    /// Focused match ordinal (1-based) and span.
    focused_ord: Option<usize>,
    focused: Option<MatchSpan>,
    /// Pending next/prev/initial focus that continues across ticks.
    pending: Option<PendingSeek>,
    /// Matches found at/before the focused start while counting (for ordinal).
    ord_reconcile_done: bool,
}

/// Per-session search controller.
pub struct SearchState {
    phase: SearchPhase,
    query: String,
    engine: Option<Engine>,
    invalid_regex: bool,
    cached_gen: Option<SearchGen>,
    /// Generation currently waiting out quiescence (slides only when it changes).
    pending_content_gen: Option<SearchGen>,
    /// Last display_offset used for visible rebuild.
    last_offset: Option<usize>,
    /// When content_gen changed while open, wait until this Instant to rescan.
    rescan_after: Option<std::time::Instant>,
    /// Visible matches for the last tick (buffer coords).
    visible_cache: Vec<MatchSpan>,
    /// Nav request deferred until the single per-frame tick.
    pending_nav: Option<Direction>,
    /// Force rebuild on next tick (query change / Confirm).
    force_rebuild: bool,
    /// Instrumentation: lines scanned in the last tick (tests).
    pub last_tick_lines: usize,
    /// Instrumentation: number of scan restarts.
    pub scan_restarts: u32,
    /// Whether the last tick still has work (scan / seek / deadline).
    pub needs_repaint: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            phase: SearchPhase::Closed,
            query: String::new(),
            engine: None,
            invalid_regex: false,
            cached_gen: None,
            pending_content_gen: None,
            last_offset: None,
            rescan_after: None,
            visible_cache: Vec::new(),
            pending_nav: None,
            force_rebuild: false,
            last_tick_lines: 0,
            scan_restarts: 0,
            needs_repaint: false,
        }
    }
}

impl SearchState {
    pub fn is_open(&self) -> bool {
        self.phase != SearchPhase::Closed
    }

    pub fn phase(&self) -> SearchPhase {
        self.phase.clone()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query_str(&mut self, q: String) {
        self.apply(SearchCmd::SetQuery(q));
    }

    pub fn apply(&mut self, cmd: SearchCmd) {
        match cmd {
            SearchCmd::Open => {
                self.phase = SearchPhase::Editing;
            }
            SearchCmd::Close => {
                self.phase = SearchPhase::Closed;
                self.visible_cache.clear();
                self.pending_nav = None;
                self.force_rebuild = false;
                self.rescan_after = None;
                self.pending_content_gen = None;
                self.needs_repaint = false;
            }
            SearchCmd::SetQuery(q) => {
                if q != self.query {
                    self.query = q;
                    self.engine = None;
                    self.invalid_regex = false;
                    // Query invalidation is immediate — not subject to output
                    // quiescence. Leave cached_gen alone so content debounce
                    // state is not confused with a "missing gen".
                    self.visible_cache.clear();
                    self.force_rebuild = true;
                    self.rescan_after = None;
                    self.pending_content_gen = None;
                    self.pending_nav = None;
                }
                self.phase = SearchPhase::Editing;
            }
            SearchCmd::Confirm => {
                if self.phase == SearchPhase::Editing {
                    self.phase = SearchPhase::Navigating;
                    // Confirm forces a rescan even if content is mid-quiesce.
                    if self.engine.is_none() {
                        self.force_rebuild = true;
                    }
                }
            }
            SearchCmd::Edit => {
                if self.phase == SearchPhase::Navigating {
                    self.phase = SearchPhase::Editing;
                }
            }
            SearchCmd::Next => {
                if self.phase != SearchPhase::Closed {
                    self.phase = SearchPhase::Navigating;
                    self.pending_nav = Some(Direction::Right);
                }
            }
            SearchCmd::Prev => {
                if self.phase != SearchPhase::Closed {
                    self.phase = SearchPhase::Navigating;
                    self.pending_nav = Some(Direction::Left);
                }
            }
        }
    }

    /// Drive a bounded scan / navigation step. At most one call per UI frame.
    /// `now` for quiescence and the shared wall-time budget.
    pub fn tick<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &mut Term<T>,
        search_gen: SearchGen,
        display_offset: usize,
        now: std::time::Instant,
    ) {
        self.last_tick_lines = 0;
        self.needs_repaint = false;
        if self.phase == SearchPhase::Closed {
            self.visible_cache.clear();
            return;
        }
        if self.query.is_empty() {
            self.engine = None;
            self.invalid_regex = false;
            self.cached_gen = Some(search_gen);
            self.visible_cache.clear();
            self.force_rebuild = false;
            self.pending_nav = None;
            return;
        }

        let force = self.force_rebuild;
        self.force_rebuild = false;
        let nav = self.pending_nav.take();

        // --- Content / resize invalidation with non-sliding quiescence ---
        let gen_changed = self.cached_gen.map(|g| g != search_gen).unwrap_or(false);
        if gen_changed && !force {
            // Drop stale coords immediately.
            if let Some(eng) = self.engine.as_mut() {
                eng.focused = None;
                eng.focused_ord = None;
                eng.pending = None;
                eng.scan_done = false;
                eng.count = 0;
                eng.capped = false;
                eng.ord_reconcile_done = false;
            }
            self.visible_cache.clear();
            // Slide the deadline only when a *new* generation is observed.
            if self.pending_content_gen != Some(search_gen) {
                self.pending_content_gen = Some(search_gen);
                self.rescan_after = Some(now + std::time::Duration::from_millis(OUTPUT_QUIESCE_MS));
            }
            if nav.is_none() {
                if let Some(deadline) = self.rescan_after {
                    if now < deadline {
                        self.last_offset = Some(display_offset);
                        self.needs_repaint = true;
                        return;
                    }
                }
            }
            // Deadline reached (or nav forces): rebuild below.
        }

        // Rebuild when forced, engine missing, or content gen just quiesced.
        if force || self.engine.is_none() || gen_changed {
            self.rebuild_engine(term, search_gen);
            self.rescan_after = None;
            self.pending_content_gen = None;
        }

        // Explicit nav starts (or restarts) a pending seek.
        if let Some(dir) = nav {
            self.start_nav(term, dir);
        }

        let tick_deadline = now + std::time::Duration::from_millis(TICK_TIME_BUDGET_MS);
        let mut budget = SCAN_LINE_BUDGET;

        // Continue pending seek within shared budget.
        self.continue_pending(term, &mut budget, tick_deadline);

        // Full-buffer count from the oldest history, chunked.
        self.scan_chunk(term, &mut budget, tick_deadline);

        // Visible matches for current viewport (uses remaining budget).
        let offset_changed = self.last_offset != Some(display_offset);
        if offset_changed || self.visible_cache.is_empty() || force || gen_changed {
            self.rebuild_visible(term, display_offset, &mut budget, tick_deadline);
        }
        self.last_offset = Some(display_offset);
        self.cached_gen = Some(search_gen);

        // Repaint while work remains.
        let scanning = self
            .engine
            .as_ref()
            .map(|e| (!e.scan_done && !e.capped) || e.pending.is_some())
            .unwrap_or(false);
        self.needs_repaint = scanning || self.rescan_after.is_some();
    }

    fn rebuild_engine<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &Term<T>,
        search_gen: SearchGen,
    ) {
        self.scan_restarts += 1;
        self.visible_cache.clear();
        match RegexSearch::new(&self.query) {
            Ok(regex) => {
                self.invalid_regex = false;
                let count_origin = top_left(term);
                let focus_origin = viewport_origin(term);
                self.engine = Some(Engine {
                    regex,
                    scan_origin: count_origin,
                    scan_done: false,
                    count: 0,
                    capped: false,
                    focused_ord: None,
                    focused: None,
                    pending: Some(PendingSeek {
                        dir: Direction::Right,
                        origin: focus_origin,
                        wrapped: false,
                        lines_walked: 0,
                    }),
                    ord_reconcile_done: false,
                });
                self.cached_gen = Some(search_gen);
            }
            Err(_) => {
                self.invalid_regex = true;
                self.engine = None;
            }
        }
    }

    fn start_nav<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &Term<T>,
        dir: Direction,
    ) {
        let Some(eng) = self.engine.as_mut() else {
            return;
        };
        let origin = match dir {
            Direction::Right => eng
                .focused
                .as_ref()
                .map(|f| point_after(term, f.end))
                .unwrap_or_else(|| viewport_origin(term)),
            Direction::Left => eng
                .focused
                .as_ref()
                .map(|f| point_before(term, f.start))
                .unwrap_or_else(|| viewport_origin(term)),
        };
        eng.pending = Some(PendingSeek {
            dir,
            origin,
            wrapped: false,
            lines_walked: 0,
        });
    }

    fn continue_pending<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &mut Term<T>,
        budget: &mut usize,
        tick_deadline: std::time::Instant,
    ) {
        if *budget == 0 || std::time::Instant::now() >= tick_deadline {
            return;
        }
        let total_lines = term.total_lines().max(1);
        let Some(eng) = self.engine.as_mut() else {
            return;
        };
        let Some(mut seek) = eng.pending.take() else {
            return;
        };

        let chunk = (*budget).min(SCAN_LINE_BUDGET);
        if chunk == 0 {
            eng.pending = Some(seek);
            return;
        }

        // alacritty's search_next may return a match *before* the origin when
        // the true next hit lies outside the max_lines window (unwrap_or on the
        // first hit in the window). Reject non-progressing hits and advance.
        let hit = term
            .search_next(
                &mut eng.regex,
                seek.origin,
                seek.dir,
                Side::Left,
                Some(chunk),
            )
            .map(|m| MatchSpan::from_match(&m))
            .filter(|span| match_progresses(span, seek.origin, seek.dir));

        if let Some(span) = hit {
            // Provisional ordinal until the count walk reconciles.
            match seek.dir {
                Direction::Right => {
                    if seek.wrapped {
                        eng.focused_ord = Some(1);
                    } else if let Some(ord) = eng.focused_ord.as_mut() {
                        *ord = ord.saturating_add(1);
                    } else {
                        eng.focused_ord = Some(1);
                    }
                }
                Direction::Left => {
                    if seek.wrapped {
                        eng.focused_ord = Some(eng.count.max(1));
                    } else if let Some(ord) = eng.focused_ord.as_mut() {
                        *ord = (*ord).saturating_sub(1).max(1);
                    } else {
                        eng.focused_ord = Some(1);
                    }
                }
            }
            eng.focused = Some(span.clone());
            eng.ord_reconcile_done = false;
            term.scroll_to_point(span.start);
            // Charge the full chunk (search_next may stop early on a hit).
            *budget = budget.saturating_sub(chunk);
            self.last_tick_lines = self.last_tick_lines.saturating_add(chunk);
            return;
        }

        // No usable match in this window — advance origin, detect wrap *before*
        // clamp. Charge only the lines actually remaining to the edge so a
        // short buffer cannot exhaust `lines_walked` before the wrap runs.
        let to_edge = lines_to_edge(term, seek.origin, seek.dir);
        let stepped = chunk.min(to_edge.max(1));
        *budget = budget.saturating_sub(stepped);
        self.last_tick_lines = self.last_tick_lines.saturating_add(stepped);
        seek.lines_walked = seek.lines_walked.saturating_add(stepped);

        match try_advance_origin(term, seek.origin, seek.dir, stepped) {
            Advance::Point(p) => {
                seek.origin = p;
                if seek.lines_walked >= total_lines {
                    eng.pending = None;
                } else {
                    eng.pending = Some(seek);
                }
            }
            Advance::CrossedEdge => {
                if seek.wrapped {
                    // Already wrapped once — stop without repeating the edge.
                    eng.pending = None;
                    return;
                }
                seek.wrapped = true;
                seek.origin = match seek.dir {
                    Direction::Right => top_left(term),
                    Direction::Left => bottom_right(term),
                };
                // Allow one full post-wrap traversal up to total_lines.
                if seek.lines_walked >= total_lines * 2 {
                    eng.pending = None;
                } else {
                    eng.pending = Some(seek);
                }
            }
        }
    }

    fn scan_chunk<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &Term<T>,
        budget: &mut usize,
        tick_deadline: std::time::Instant,
    ) {
        if *budget == 0 || std::time::Instant::now() >= tick_deadline {
            return;
        }
        let Some(eng) = self.engine.as_mut() else {
            return;
        };
        if eng.scan_done || eng.capped {
            return;
        }

        let chunk = *budget;
        let start = eng.scan_origin;
        let end_line = (start.line + chunk as i32).min(term.bottommost_line());
        let end = Point::new(end_line, term.last_column());
        let focused = eng.focused.clone();
        let iter = alacritty_terminal::term::search::RegexIter::new(
            start,
            end,
            Direction::Right,
            term,
            &mut eng.regex,
        );
        // Cap interior iterations so a dense pattern cannot explode inside one
        // line-chunk (shared budget also enforced via wall clock).
        let mut interior = 0usize;
        const INTERIOR_MATCH_CAP: usize = 50_000;
        for m in iter {
            if std::time::Instant::now() >= tick_deadline {
                // Leave scan_origin so we resume here next tick.
                eng.scan_origin = *m.start();
                let used = chunk; // conservative
                *budget = budget.saturating_sub(used);
                self.last_tick_lines = self.last_tick_lines.saturating_add(used);
                return;
            }
            eng.count = eng.count.saturating_add(1);
            interior += 1;
            // Reconcile focused ordinal as we pass matches from oldest.
            if let Some(ref f) = focused {
                let span = MatchSpan::from_match(&m);
                if span.same_as(f) {
                    eng.focused_ord = Some(eng.count.min(COUNT_CAP));
                    eng.ord_reconcile_done = true;
                }
            }
            // Prove "+" only after COUNT_CAP + 1 hits.
            if eng.count > COUNT_CAP {
                eng.capped = true;
                eng.count = COUNT_CAP;
                eng.scan_done = true;
                break;
            }
            if interior >= INTERIOR_MATCH_CAP {
                eng.scan_origin = point_after(term, *m.end());
                let used = ((end_line - start.line).0 as usize)
                    .saturating_add(1)
                    .min(chunk);
                *budget = budget.saturating_sub(used);
                self.last_tick_lines = self.last_tick_lines.saturating_add(used);
                return;
            }
        }
        let used = ((end_line - start.line).0 as usize)
            .saturating_add(1)
            .min(chunk);
        *budget = budget.saturating_sub(used);
        self.last_tick_lines = self.last_tick_lines.saturating_add(used);
        if eng.capped || end_line >= term.bottommost_line() {
            eng.scan_done = true;
        } else {
            eng.scan_origin = Point::new(end_line + 1, Column(0));
        }
    }

    fn rebuild_visible<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &Term<T>,
        display_offset: usize,
        budget: &mut usize,
        tick_deadline: std::time::Instant,
    ) {
        let Some(eng) = self.engine.as_mut() else {
            self.visible_cache.clear();
            return;
        };
        // Visible range is at most screen_lines; charge a small fixed cost.
        let vis_cost = term.screen_lines().min(*budget);
        if vis_cost == 0 {
            return;
        }
        let top = viewport_top_line(display_offset);
        let bot = top + term.screen_lines() as i32 - 1;
        // Expand one line past the edges so chunk-edge / wrap matches that
        // only partially intersect the viewport still appear.
        let start_line = Line(top - 1).max(term.topmost_line());
        let end_line = Line(bot + 1).min(term.bottommost_line());
        let start = Point::new(start_line, Column(0));
        let end = Point::new(end_line, term.last_column());
        let mut visible = Vec::new();
        let iter = alacritty_terminal::term::search::RegexIter::new(
            start,
            end,
            Direction::Right,
            term,
            &mut eng.regex,
        );
        for m in iter {
            if std::time::Instant::now() >= tick_deadline {
                break;
            }
            let span = MatchSpan::from_match(&m);
            // Keep if any part intersects the viewport rows.
            if span.end.line.0 >= top && span.start.line.0 <= bot {
                visible.push(span);
            }
            if visible.len() > 500 {
                break;
            }
        }
        *budget = budget.saturating_sub(vis_cost);
        self.last_tick_lines = self.last_tick_lines.saturating_add(vis_cost);
        self.visible_cache = visible;
    }

    pub fn view(&self) -> SearchView {
        if self.phase == SearchPhase::Closed {
            return SearchView::default();
        }
        let (total, total_capped, focused, focused_ord, scanning) = match &self.engine {
            Some(e) => {
                let pending = e.pending.is_some();
                let total = if e.capped {
                    COUNT_CAP
                } else {
                    e.count.max(e.focused_ord.unwrap_or(0))
                };
                (
                    total,
                    e.capped,
                    e.focused.clone(),
                    e.focused_ord,
                    (!e.scan_done && !e.capped) || pending || self.rescan_after.is_some(),
                )
            }
            None => (
                0,
                false,
                None,
                None,
                self.force_rebuild || self.rescan_after.is_some(),
            ),
        };
        SearchView {
            phase: self.phase.clone(),
            query: self.query.clone(),
            current: focused_ord,
            total,
            total_capped,
            invalid_regex: self.invalid_regex,
            scanning,
            focused,
            visible: self.visible_cache.clone(),
        }
    }
}

enum Advance {
    Point(Point),
    CrossedEdge,
}

/// True when `span` is strictly a step in `dir` from `origin` (accepts equal
/// start for Right so a match beginning at the origin still counts).
fn match_progresses(span: &MatchSpan, origin: Point, dir: Direction) -> bool {
    match dir {
        Direction::Right => MatchSpan::cmp_pos(span.start, origin) != std::cmp::Ordering::Less,
        Direction::Left => {
            // Side::Left compares on start; accept starts at or before origin.
            MatchSpan::cmp_pos(span.start, origin) != std::cmp::Ordering::Greater
        }
    }
}

/// Lines remaining from `origin` to the buffer edge in `dir` (at least 0).
fn lines_to_edge<T>(term: &Term<T>, origin: Point, dir: Direction) -> usize {
    match dir {
        Direction::Right => {
            let d = term.bottommost_line().0 - origin.line.0;
            if d < 0 { 0 } else { d as usize }
        }
        Direction::Left => {
            let d = origin.line.0 - term.topmost_line().0;
            if d < 0 { 0 } else { d as usize }
        }
    }
}

/// Advance origin by `lines` in `dir`. Returns `CrossedEdge` *before* clamping
/// so seekers can wrap exactly once.
fn try_advance_origin<T>(term: &Term<T>, origin: Point, dir: Direction, lines: usize) -> Advance {
    match dir {
        Direction::Right => {
            let line = Line(origin.line.0 + lines as i32);
            if line > term.bottommost_line() {
                Advance::CrossedEdge
            } else {
                Advance::Point(Point::new(line, Column(0)))
            }
        }
        Direction::Left => {
            let line = Line(origin.line.0 - lines as i32);
            if line < term.topmost_line() {
                Advance::CrossedEdge
            } else {
                Advance::Point(Point::new(line, term.last_column()))
            }
        }
    }
}

fn viewport_origin<T>(term: &Term<T>) -> Point {
    let top = term.grid().display_offset() as i32;
    Point::new(Line(-top), Column(0))
}

fn viewport_top_line(display_offset: usize) -> i32 {
    -(display_offset as i32)
}

fn top_left<T>(term: &Term<T>) -> Point {
    Point::new(term.topmost_line(), Column(0))
}

fn bottom_right<T>(term: &Term<T>) -> Point {
    Point::new(term.bottommost_line(), term.last_column())
}

fn point_after<T>(term: &Term<T>, p: Point) -> Point {
    let mut col = p.column.0 + 1;
    let mut line = p.line;
    if col > term.last_column().0 {
        col = 0;
        line = Line(line.0 + 1);
        if line > term.bottommost_line() {
            return bottom_right(term);
        }
    }
    Point::new(line, Column(col))
}

fn point_before<T>(term: &Term<T>, p: Point) -> Point {
    if p.column.0 > 0 {
        Point::new(p.line, Column(p.column.0 - 1))
    } else if p.line > term.topmost_line() {
        Point::new(Line(p.line.0 - 1), term.last_column())
    } else {
        top_left(term)
    }
}

/// Convert a buffer-space match span into an optional viewport range.
///
/// Inclusive on both ends. Clamps rows/cols to the viewport, so a match that
/// crosses the top or bottom edge still highlights the visible portion. Wide-
/// char spacers are covered when the match end column includes them (alacritty
/// `Match` ranges include the full matched cell span).
pub fn match_viewport_range(
    span: &MatchSpan,
    display_offset: usize,
    screen_lines: usize,
    columns: usize,
) -> Option<crate::frame::SelRange> {
    let off = display_offset as i32;
    let start_row = span.start.line.0 + off;
    let end_row = span.end.line.0 + off;
    if end_row < 0 || start_row >= screen_lines as i32 {
        return None;
    }
    let last_col = columns.saturating_sub(1);
    let start = if start_row < 0 {
        (0, 0)
    } else {
        (start_row as usize, span.start.column.0.min(last_col))
    };
    let end = if end_row >= screen_lines as i32 {
        (screen_lines.saturating_sub(1), last_col)
    } else {
        (end_row as usize, span.end.column.0.min(last_col))
    };
    Some(crate::frame::SelRange { start, end })
}

/// Geometry for the in-pane search bar (top-right overlay). Shared by paint and
/// mouse hit-testing so the bar excludes terminal selection/paste.
pub fn search_bar_rect(content: egui::Rect) -> egui::Rect {
    let bar_w = 320.0_f32.min(content.width() * 0.6);
    let bar_h = 30.0;
    let pad = 6.0;
    egui::Rect::from_min_size(
        egui::pos2(content.max.x - bar_w - pad, content.min.y + pad),
        egui::vec2(bar_w, bar_h),
    )
}

/// Truncate a display string on a char boundary (never panic on emoji).
pub fn truncate_chars_end(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let skip = count - max_chars;
    let tail: String = s.chars().skip(skip).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::Processor;

    struct Dims {
        cols: usize,
        rows: usize,
    }
    impl Dimensions for Dims {
        fn total_lines(&self) -> usize {
            self.rows + 10_000
        }
        fn screen_lines(&self) -> usize {
            self.rows
        }
        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn term_with(text: &str, cols: usize, rows: usize) -> Term<VoidListener> {
        let mut term = Term::new(
            Config {
                scrolling_history: 10_000,
                ..Config::default()
            },
            &Dims { cols, rows },
            VoidListener,
        );
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, text.as_bytes());
        term
    }

    fn sgen(g: u64, cols: usize, rows: usize) -> SearchGen {
        SearchGen {
            content_gen: g,
            cols,
            rows,
        }
    }

    #[test]
    fn empty_query_does_no_work() {
        let mut term = term_with("hello\r\n", 40, 10);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery(String::new()));
        s.tick(&mut term, sgen(1, 40, 10), 0, std::time::Instant::now());
        assert!(!s.view().scanning);
        assert_eq!(s.view().total, 0);
        assert_eq!(s.last_tick_lines, 0);
        assert!(s.view().visible.is_empty());
    }

    #[test]
    fn invalid_regex_is_nonfatal() {
        let mut term = term_with("abc\r\n", 40, 10);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("(".into()));
        s.tick(&mut term, sgen(1, 40, 10), 0, std::time::Instant::now());
        assert!(s.view().invalid_regex);
        assert!(s.view().focused.is_none());
    }

    #[test]
    fn finds_match_in_scrollback() {
        let mut body = String::new();
        for i in 0..50 {
            body.push_str(&format!("line {i} unique_token_{i}\r\n"));
        }
        body.push_str("line live target_word here\r\n");
        let mut term = term_with(&body, 40, 8);
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(20));
        let off = term.grid().display_offset();
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("unique_token_5".into()));
        for _ in 0..40 {
            s.tick(
                &mut term,
                sgen(1, 40, 8),
                off,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            );
            if s.view().focused.is_some() {
                break;
            }
        }
        assert!(
            s.view().focused.is_some(),
            "must find scrollback match; total={} scanning={} restarts={} lines={}",
            s.view().total,
            s.view().scanning,
            s.scan_restarts,
            s.last_tick_lines
        );
    }

    #[test]
    fn smart_case_insensitive_without_uppercase() {
        let mut term = term_with("Hello WORLD\r\n", 40, 5);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("hello".into()));
        s.tick(&mut term, sgen(1, 40, 5), 0, std::time::Instant::now());
        assert!(s.view().focused.is_some());
    }

    #[test]
    fn tick_respects_shared_line_budget() {
        let mut body = String::new();
        for i in 0..3000 {
            body.push_str(&format!("row{i} x\r\n"));
        }
        let mut term = term_with(&body, 40, 10);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("x".into()));
        s.tick(&mut term, sgen(1, 40, 10), 0, std::time::Instant::now());
        assert!(
            s.last_tick_lines <= SCAN_LINE_BUDGET,
            "one tick must stay within one shared budget, got {}",
            s.last_tick_lines
        );
    }

    #[test]
    fn dense_pattern_stays_within_shared_budget() {
        let mut body = String::new();
        for i in 0..2000 {
            body.push_str(&format!("aaaaaaaaaa{i}\r\n"));
        }
        let mut term = term_with(&body, 40, 10);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("a".into()));
        s.tick(&mut term, sgen(1, 40, 10), 0, std::time::Instant::now());
        assert!(
            s.last_tick_lines <= SCAN_LINE_BUDGET,
            "dense pattern exceeded budget: {}",
            s.last_tick_lines
        );
    }

    #[test]
    fn continuous_output_does_not_restart_every_gen() {
        let mut term = term_with("alpha beta\r\n", 40, 5);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("alpha".into()));
        let t0 = std::time::Instant::now();
        s.tick(&mut term, sgen(1, 40, 5), 0, t0);
        let restarts = s.scan_restarts;
        for g in 2..10 {
            s.tick(&mut term, sgen(g, 40, 5), 0, t0);
        }
        assert_eq!(s.scan_restarts, restarts);
    }

    #[test]
    fn set_query_finds_on_ordinary_ticks_without_enter() {
        let mut body = String::new();
        for i in 0..30 {
            body.push_str(&format!("line {i}\r\n"));
        }
        body.push_str("needle_word here\r\n");
        let mut term = term_with(&body, 40, 10);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("needle_word".into()));
        // Ordinary production ticks only — no Confirm / force flag.
        let t0 = std::time::Instant::now();
        for _ in 0..30 {
            s.tick(
                &mut term,
                sgen(1, 40, 10),
                0,
                t0 + std::time::Duration::from_secs(1),
            );
            if s.view().focused.is_some() {
                break;
            }
        }
        assert!(
            s.view().focused.is_some(),
            "SetQuery must find results without Enter"
        );
    }

    #[test]
    fn content_gen_bump_rebuilds_after_quiescence_not_while_sliding() {
        let mut term = term_with("alpha beta gamma\r\n", 40, 5);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("alpha".into()));
        let t0 = std::time::Instant::now();
        s.tick(&mut term, sgen(1, 40, 5), 0, t0);
        let restarts = s.scan_restarts;
        // One gen bump at t0: should arm deadline, not rebuild yet.
        s.tick(&mut term, sgen(2, 40, 5), 0, t0);
        assert_eq!(s.scan_restarts, restarts, "must not rebuild mid-quiesce");
        // Same gen later still before deadline: still no rebuild.
        s.tick(
            &mut term,
            sgen(2, 40, 5),
            0,
            t0 + std::time::Duration::from_millis(40),
        );
        assert_eq!(s.scan_restarts, restarts);
        // Past deadline with stable gen: rebuild.
        s.tick(
            &mut term,
            sgen(2, 40, 5),
            0,
            t0 + std::time::Duration::from_millis(OUTPUT_QUIESCE_MS + 5),
        );
        assert!(
            s.scan_restarts > restarts,
            "stable gen past quiescence must rebuild"
        );
    }

    #[test]
    fn deep_history_finds_and_wraps_once_both_dirs() {
        let mut body = String::new();
        // >3000 lines so seek must span multiple chunks.
        for i in 0..3500 {
            if i == 100 {
                body.push_str("DEEP_MATCH_A\r\n");
            } else if i == 3200 {
                body.push_str("DEEP_MATCH_B\r\n");
            } else {
                body.push_str(&format!("pad_{i}\r\n"));
            }
        }
        let mut term = term_with(&body, 40, 10);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("DEEP_MATCH".into()));
        let t_far = std::time::Instant::now() + std::time::Duration::from_secs(10);
        // Drain until first focus + count finished (stable total).
        for _ in 0..80 {
            s.tick(&mut term, sgen(1, 40, 10), 0, t_far);
            let v = s.view();
            if v.focused.is_some() && !v.scanning && v.total >= 2 {
                break;
            }
        }
        let v = s.view();
        assert!(
            v.focused.is_some(),
            "initial focus missing; total={}",
            v.total
        );
        assert!(
            v.total >= 2,
            "expected both deep matches counted, total={}",
            v.total
        );
        let first = v.focused.clone().unwrap();
        // Next should find the other match (may take multiple ticks).
        s.apply(SearchCmd::Next);
        let mut second = None;
        for _ in 0..80 {
            s.tick(&mut term, sgen(1, 40, 10), 0, t_far);
            if let Some(f) = s.view().focused.clone() {
                if !f.same_as(&first) {
                    second = Some(f);
                    break;
                }
            }
        }
        assert!(
            second.is_some(),
            "n must find the other deep match; first={first:?} total={} pending scanning={}",
            s.view().total,
            s.view().scanning
        );
        // One more next wraps back to first.
        s.apply(SearchCmd::Next);
        let mut wrapped = false;
        for _ in 0..80 {
            s.tick(&mut term, sgen(1, 40, 10), 0, t_far);
            if let Some(f) = s.view().focused.clone() {
                if f.same_as(&first) {
                    wrapped = true;
                    break;
                }
            }
        }
        assert!(wrapped, "forward wrap must return to first match once");
        // Prev wraps the other way.
        s.apply(SearchCmd::Prev);
        let mut back = false;
        for _ in 0..80 {
            s.tick(&mut term, sgen(1, 40, 10), 0, t_far);
            if let Some(f) = s.view().focused.clone() {
                if second.as_ref().is_some_and(|sec| f.same_as(sec)) {
                    back = true;
                    break;
                }
            }
        }
        assert!(back, "backward nav must reach the other match");
    }

    #[test]
    fn next_wrap_keeps_exact_four_match_counter() {
        let mut term = term_with(
            "WRAP_HIT one\r\nWRAP_HIT two\r\nWRAP_HIT three\r\nWRAP_HIT four\r\n",
            40,
            8,
        );
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("WRAP_HIT".into()));
        let t_far = std::time::Instant::now() + std::time::Duration::from_secs(5);

        for _ in 0..10 {
            s.tick(&mut term, sgen(1, 40, 8), 0, t_far);
            let v = s.view();
            if !v.scanning && v.total == 4 {
                break;
            }
        }
        assert_eq!(s.view().current, Some(1));
        assert_eq!(s.view().total, 4);

        for expected in [2, 3, 4] {
            s.apply(SearchCmd::Next);
            s.tick(&mut term, sgen(1, 40, 8), 0, t_far);
            let v = s.view();
            assert_eq!(v.current, Some(expected));
            assert_eq!(v.total, 4);
        }

        s.apply(SearchCmd::Next);
        for _ in 0..3 {
            s.tick(&mut term, sgen(1, 40, 8), 0, t_far);
            if s.view().current != Some(4) {
                break;
            }
        }
        let wrapped = s.view();
        assert_eq!(wrapped.current, Some(1), "4/4 must wrap to 1/4");
        assert_eq!(wrapped.total, 4, "wrapping must not inflate the total");

        s.apply(SearchCmd::Next);
        s.tick(&mut term, sgen(1, 40, 8), 0, t_far);
        let after_wrap = s.view();
        assert_eq!(after_wrap.current, Some(2));
        assert_eq!(after_wrap.total, 4);
    }

    #[test]
    fn focused_ordinal_counts_matches_above_and_below() {
        // Three matches: viewport sits on the middle one after initial seek.
        let mut body = String::new();
        for i in 0..80 {
            if i == 10 || i == 40 || i == 70 {
                body.push_str("ORD_HIT\r\n");
            } else {
                body.push_str(&format!("zz{i}\r\n"));
            }
        }
        let mut term = term_with(&body, 40, 12);
        // Scroll so viewport is near the middle match.
        term.scroll_display(alacritty_terminal::grid::Scroll::Delta(35));
        let off = term.grid().display_offset();
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("ORD_HIT".into()));
        let t_far = std::time::Instant::now() + std::time::Duration::from_secs(5);
        for _ in 0..40 {
            s.tick(&mut term, sgen(1, 40, 12), off, t_far);
            let v = s.view();
            if v.focused.is_some() && !v.scanning && v.total >= 3 {
                break;
            }
        }
        let v = s.view();
        assert_eq!(v.total, 3, "must count all three");
        // Initial seek starts at viewport top → first hit at/after that is the
        // middle match (2 of 3). Count walk must reconcile exact ordinal, not
        // leave the provisional "1" from the seek path.
        assert_eq!(
            v.current,
            Some(2),
            "focused ordinal must be exact after count reconcile, got {:?}",
            v.current
        );
    }

    #[test]
    fn match_viewport_range_clips_bottom_edge() {
        let span = MatchSpan {
            start: Point::new(Line(8), Column(2)),
            end: Point::new(Line(12), Column(4)),
        };
        let r = match_viewport_range(&span, 0, 10, 20).unwrap();
        assert_eq!(r.start, (8, 2));
        assert_eq!(r.end, (9, 19)); // clamped to last viewport row/col
    }

    #[test]
    fn match_viewport_range_none_when_fully_above() {
        let span = MatchSpan {
            start: Point::new(Line(-20), Column(0)),
            end: Point::new(Line(-15), Column(5)),
        };
        assert!(match_viewport_range(&span, 0, 10, 40).is_none());
    }

    #[test]
    fn editing_phase_keeps_n_as_non_nav_until_confirm() {
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("x".into()));
        assert_eq!(s.phase(), SearchPhase::Editing);
        // Confirm transitions; Next while open forces Navigating.
        s.apply(SearchCmd::Confirm);
        assert_eq!(s.phase(), SearchPhase::Navigating);
        s.apply(SearchCmd::Edit);
        assert_eq!(s.phase(), SearchPhase::Editing);
        s.apply(SearchCmd::Next);
        assert_eq!(s.phase(), SearchPhase::Navigating);
    }

    #[test]
    fn search_bar_hit_is_disjoint_from_content_origin() {
        // Pure geometry: pointer at content top-left is never over the bar;
        // pointer at bar center is. Session routing uses this exclusion for
        // selection + secondary paste (see terminal.rs show path).
        let content = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let bar = search_bar_rect(content);
        assert!(!bar.contains(egui::pos2(10.0, 10.0)));
        assert!(bar.contains(bar.center()));
        assert!(bar.max.x <= content.max.x && bar.min.y >= content.min.y);
    }

    #[test]
    fn needs_repaint_false_when_scan_and_seek_idle() {
        let mut term = term_with("only_one_hit\r\n", 40, 5);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("only_one_hit".into()));
        let t_far = std::time::Instant::now() + std::time::Duration::from_secs(5);
        for _ in 0..20 {
            s.tick(&mut term, sgen(1, 40, 5), 0, t_far);
            if !s.needs_repaint && s.view().focused.is_some() {
                break;
            }
        }
        assert!(s.view().focused.is_some());
        assert!(
            !s.needs_repaint,
            "idle search must not request endless repaint"
        );
    }

    #[test]
    fn empty_query_clears_visible_cache() {
        let mut term = term_with("alpha\r\n", 40, 5);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("alpha".into()));
        s.tick(&mut term, sgen(1, 40, 5), 0, std::time::Instant::now());
        assert!(!s.view().visible.is_empty() || s.view().focused.is_some());
        s.apply(SearchCmd::SetQuery(String::new()));
        s.tick(&mut term, sgen(2, 40, 5), 0, std::time::Instant::now());
        assert!(s.view().visible.is_empty());
    }

    #[test]
    fn truncate_chars_end_is_utf8_safe() {
        let s = "🦀🚀🎉✨🔥🌈🎯";
        let t = truncate_chars_end(s, 3);
        assert!(t.starts_with('…'));
        assert_eq!(t.chars().count(), 4); // ellipsis + 3
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn match_viewport_range_culls_like_selection() {
        let span = MatchSpan {
            start: Point::new(Line(-2), Column(3)),
            end: Point::new(Line(1), Column(5)),
        };
        let r = match_viewport_range(&span, 0, 10, 20).unwrap();
        assert_eq!(r.start, (0, 0));
        assert_eq!(r.end, (1, 5));
    }

    #[test]
    fn match_viewport_range_covers_wide_span_cols() {
        // CJK-like span covering base + spacer columns 0..=1.
        let span = MatchSpan {
            start: Point::new(Line(0), Column(0)),
            end: Point::new(Line(0), Column(1)),
        };
        let r = match_viewport_range(&span, 0, 5, 40).unwrap();
        assert_eq!(r.start, (0, 0));
        assert_eq!(r.end, (0, 1));
    }

    #[test]
    fn initial_search_never_unbounded() {
        let mut body = String::new();
        for i in 0..2500 {
            body.push_str(&format!("zzz{i}\r\n"));
        }
        body.push_str("needle_here\r\n");
        let mut term = term_with(&body, 40, 10);
        let mut s = SearchState::default();
        s.apply(SearchCmd::Open);
        s.apply(SearchCmd::SetQuery("needle_here".into()));
        s.tick(&mut term, sgen(1, 40, 10), 0, std::time::Instant::now());
        assert!(
            s.last_tick_lines <= SCAN_LINE_BUDGET,
            "initial focus must be bounded, got {}",
            s.last_tick_lines
        );
    }

    #[test]
    fn try_advance_detects_edge_before_clamp() {
        let term = term_with("a\r\n", 40, 5);
        let origin = Point::new(term.bottommost_line(), Column(0));
        match try_advance_origin(&term, origin, Direction::Right, 100) {
            Advance::CrossedEdge => {}
            Advance::Point(p) => panic!("expected edge, got {p:?}"),
        }
        let top = Point::new(term.topmost_line(), Column(0));
        match try_advance_origin(&term, top, Direction::Left, 100) {
            Advance::CrossedEdge => {}
            Advance::Point(p) => panic!("expected edge, got {p:?}"),
        }
    }

    #[test]
    fn search_bar_rect_is_top_right_overlay() {
        let content = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(800.0, 600.0));
        let bar = search_bar_rect(content);
        assert!(bar.max.x <= content.max.x);
        assert!(bar.min.y >= content.min.y);
        assert!(bar.width() > 0.0 && bar.height() > 0.0);
        // Outside bar is not inside.
        assert!(!bar.contains(egui::pos2(content.min.x + 10.0, content.min.y + 10.0)));
    }
}
