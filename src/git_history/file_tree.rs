//! The status-colored changed-file tree shared by the commit details pane and
//! the Git Changes window: pure row building off-thread, virtualized painting,
//! multi-select, optional per-row checkboxes, and a context-menu seam whose
//! items the owning view builds.
use eframe::egui;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::Range;

#[derive(Debug, PartialEq)]
pub(super) struct ChangedFile {
    pub(super) status: char,
    pub(super) path: String,
    pub(super) previous: Option<String>,
}

pub(super) struct TreeRow {
    pub(super) label: String,
    pub(super) depth: usize,
    pub(super) file: Option<usize>,
    /// Row index just past this row's subtree.
    pub(super) end: usize,
    pub(super) collapsed: bool,
    pub(super) file_count: usize,
    /// A top-level group heading ("Staged", "Changes", ...), not a directory.
    pub(super) section: bool,
    /// Stable identity (section + directory path) so a re-read keeps collapse state.
    pub(super) key: String,
    /// Draws a checkbox: the row sits in a checkable section.
    pub(super) check: bool,
}

#[derive(Default)]
struct Directory {
    dirs: BTreeMap<String, Directory>,
    files: BTreeMap<String, usize>,
}
impl Directory {
    fn insert(&mut self, path: &str, index: usize) {
        let mut parts = path.split('/').peekable();
        let mut dir = self;
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                dir = dir.dirs.entry(part.into()).or_default();
            } else {
                dir.files.insert(part.into(), index);
            }
        }
    }
    fn flatten(self, depth: usize, key: &str, rows: &mut Vec<TreeRow>) -> usize {
        let mut count = self.files.len();
        for (mut label, mut dir) in self.dirs {
            // Fold single-child directory chains into one row ("a/b/c").
            while dir.files.is_empty() && dir.dirs.len() == 1 {
                let (child, inner) = dir.dirs.pop_first().unwrap();
                label = format!("{label}/{child}");
                dir = inner;
            }
            let key = format!("{key}/{label}");
            let index = rows.len();
            rows.push(TreeRow {
                label,
                depth,
                file: None,
                end: 0,
                collapsed: false,
                file_count: 0,
                section: false,
                key: key.clone(),
                check: false,
            });
            let descendants = dir.flatten(depth + 1, &key, rows);
            count += descendants;
            rows[index].end = rows.len();
            rows[index].file_count = descendants;
        }
        for (label, file) in self.files {
            rows.push(TreeRow {
                key: format!("{key}/{label}"),
                label,
                depth,
                file: Some(file),
                end: rows.len() + 1,
                collapsed: false,
                file_count: 1,
                section: false,
                check: false,
            });
        }
        count
    }
}

/// A section: its heading, its files (indices run through the sections in
/// order), and whether its rows carry checkboxes.
struct Group {
    name: String,
    files: Range<usize>,
    check: bool,
}

/// Rows for `groups`: a heading each (empty groups skipped), over a directory
/// tree, or with `flat` over the bare files by name.
fn group_rows(files: &[ChangedFile], groups: &[Group], flat: bool) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    for group in groups.iter().filter(|g| !g.files.is_empty()) {
        let index = rows.len();
        rows.push(TreeRow {
            label: group.name.clone(),
            depth: 0,
            file: None,
            end: 0,
            collapsed: false,
            file_count: group.files.len(),
            section: true,
            key: group.name.clone(),
            check: false,
        });
        if flat {
            let mut sorted: Vec<usize> = group.files.clone().collect();
            // By name, as the rows read; the directory breaks ties.
            let name = |f: usize| {
                let path = files[f].path.as_str();
                (path.rsplit('/').next().unwrap_or(path), path)
            };
            sorted.sort_by(|&a, &b| name(a).cmp(&name(b)));
            for f in sorted {
                let path = &files[f].path;
                rows.push(TreeRow {
                    label: path.rsplit('/').next().unwrap_or(path).to_owned(),
                    depth: 1,
                    file: Some(f),
                    end: rows.len() + 1,
                    collapsed: false,
                    file_count: 1,
                    section: false,
                    key: format!("{}/{path}", group.name),
                    check: false,
                });
            }
        } else {
            let mut root = Directory::default();
            for f in group.files.clone() {
                root.insert(&files[f].path, f);
            }
            root.flatten(1, &group.name, &mut rows);
        }
        rows[index].end = rows.len();
        for row in &mut rows[index..] {
            row.check = group.check;
        }
    }
    rows
}

#[derive(Default)]
pub(super) struct FileTree {
    pub(super) files: Vec<ChangedFile>,
    pub(super) rows: Vec<TreeRow>,
    pub(super) visible: Vec<usize>,
    /// Selected file indices. Private so `picked` stays in step.
    selected: BTreeSet<usize>,
    /// Per row, how many selected files sit at or before it: a directory is
    /// selected when every file under it is, answered in O(1) while painting.
    picked: Vec<u32>,
    /// The row the keyboard moves from and the menu opens at.
    pub(super) cursor: Option<usize>,
    /// Where a Shift range starts: the last plain or Ctrl click.
    anchor: Option<usize>,
    /// Last frame's scroll offset and viewport height, and whether the next
    /// frame must scroll the cursor into view (it may not be painted yet).
    scroll: f32,
    viewport: f32,
    reveal: bool,
    /// Where the open context menu is anchored.
    menu_at: egui::Pos2,
    /// Sections, when built by `grouped`; empty for a plain tree.
    groups: Vec<Group>,
    /// Files listed bare under their section instead of in directories.
    pub(super) flat: bool,
    /// Checked files, and per row how many sit at or before it (as `picked`).
    checked: BTreeSet<usize>,
    ticked: Vec<u32>,
}
impl FileTree {
    /// One directory tree over all `files`.
    pub(super) fn new(files: Vec<ChangedFile>) -> Self {
        let mut root = Directory::default();
        for (index, file) in files.iter().enumerate() {
            root.insert(&file.path, index);
        }
        let mut rows = Vec::new();
        root.flatten(0, "", &mut rows);
        Self::from_rows(files, rows)
    }
    /// A collapsible heading per non-empty section, each over its own tree.
    /// File indices run through the sections in order.
    #[cfg(test)]
    pub(super) fn sections(sections: Vec<(&str, Vec<ChangedFile>)>) -> Self {
        Self::grouped(
            sections.into_iter().map(|(n, f)| (n, f, false)).collect(),
            false,
        )
    }
    /// `sections`, with a checkbox flag per section, in directories or `flat`.
    pub(super) fn grouped(sections: Vec<(&str, Vec<ChangedFile>, bool)>, flat: bool) -> Self {
        let mut files = Vec::new();
        let mut groups = Vec::new();
        for (name, section, check) in sections {
            let start = files.len();
            files.extend(section);
            groups.push(Group {
                name: name.into(),
                files: start..files.len(),
                check,
            });
        }
        let rows = group_rows(&files, &groups, flat);
        let mut tree = Self::from_rows(files, rows);
        tree.groups = groups;
        tree.flat = flat;
        tree
    }
    /// Switch between directories and a flat list. Collapse state, the
    /// selection, the checks, cursor and anchor all carry over.
    pub(super) fn set_flat(&mut self, flat: bool) {
        if flat == self.flat || self.groups.is_empty() {
            return;
        }
        let collapsed = self.collapsed_keys();
        let key = |row: Option<usize>| row.map(|r| self.rows[r].key.clone());
        let (cursor, anchor) = (key(self.cursor), key(self.anchor));
        self.flat = flat;
        self.rows = group_rows(&self.files, &self.groups, flat);
        let find =
            |key: Option<String>| key.and_then(|k| self.rows.iter().position(|r| r.key == k));
        self.cursor = find(cursor);
        self.anchor = find(anchor);
        self.collapse(&collapsed);
        self.recount();
    }
    /// Expand All / Collapse All: every folder and section.
    pub(super) fn set_all_collapsed(&mut self, collapsed: bool) {
        for row in &mut self.rows {
            row.collapsed = collapsed && row.file.is_none();
        }
        self.rebuild_visible();
    }
    fn from_rows(files: Vec<ChangedFile>, rows: Vec<TreeRow>) -> Self {
        let visible = (0..rows.len()).collect();
        let mut tree = Self {
            files,
            rows,
            visible,
            ..Default::default()
        };
        tree.recount();
        tree
    }
    pub(super) fn rebuild_visible(&mut self) {
        self.visible.clear();
        let mut i = 0;
        while i < self.rows.len() {
            self.visible.push(i);
            i = if self.rows[i].collapsed {
                self.rows[i].end
            } else {
                i + 1
            };
        }
    }
    pub(super) fn collapsed_keys(&self) -> HashSet<String> {
        self.rows
            .iter()
            .filter(|r| r.collapsed)
            .map(|r| r.key.clone())
            .collect()
    }
    pub(super) fn collapse(&mut self, keys: &HashSet<String>) {
        for row in &mut self.rows {
            row.collapsed = row.file.is_none() && keys.contains(&row.key);
        }
        self.rebuild_visible();
    }

    /// The selected files, in file order.
    pub(super) fn selected(&self) -> Vec<usize> {
        self.selected.iter().copied().collect()
    }
    /// Replace the selection with `files`.
    pub(super) fn select_files(&mut self, files: impl IntoIterator<Item = usize>) {
        self.selected = files.into_iter().collect();
        self.recount();
    }
    fn recount(&mut self) {
        let prefix = |set: &BTreeSet<usize>| {
            let mut n = 0;
            std::iter::once(0)
                .chain(self.rows.iter().map(|row| {
                    n += row.file.is_some_and(|f| set.contains(&f)) as u32;
                    n
                }))
                .collect()
        };
        self.picked = prefix(&self.selected);
        self.ticked = prefix(&self.checked);
    }

    /// The checked files, in file order.
    #[cfg(test)]
    pub(super) fn checked(&self) -> Vec<usize> {
        self.checked.iter().copied().collect()
    }
    /// Replace the checks with `files`.
    pub(super) fn set_checked(&mut self, files: impl IntoIterator<Item = usize>) {
        self.checked = files.into_iter().collect();
        self.recount();
    }
    /// A row's checkbox: all its files checked, none, or `None` for some.
    pub(super) fn row_checked(&self, row: usize) -> Option<bool> {
        let r = &self.rows[row];
        match (self.ticked[r.end] - self.ticked[row]) as usize {
            0 => Some(false),
            n if n == r.file_count => Some(true),
            _ => None,
        }
    }
    /// Tick `files` unless all already are; then untick them. Only files on
    /// checkable rows count. Returns what changed, for the owner.
    fn toggle_checks<A>(&mut self, files: Vec<usize>) -> Option<TreeEvent<A>> {
        let checkable: HashSet<usize> = self
            .rows
            .iter()
            .filter(|r| r.check)
            .filter_map(|r| r.file)
            .collect();
        let files: Vec<usize> = files
            .into_iter()
            .filter(|f| checkable.contains(f))
            .collect();
        if files.is_empty() {
            return None;
        }
        let on = !files.iter().all(|f| self.checked.contains(f));
        for &f in &files {
            if on {
                self.checked.insert(f);
            } else {
                self.checked.remove(&f);
            }
        }
        self.recount();
        Some(TreeEvent::Check { files, on })
    }
    fn files_under(&self, row: usize) -> impl Iterator<Item = usize> + '_ {
        self.rows[row..self.rows[row].end]
            .iter()
            .filter_map(|r| r.file)
    }
    /// A file row whose file is selected, or a directory or section whose
    /// every file is.
    pub(super) fn row_selected(&self, row: usize) -> bool {
        let r = &self.rows[row];
        r.file_count > 0 && (self.picked[r.end] - self.picked[row]) as usize == r.file_count
    }
    fn position(&self, row: usize) -> Option<usize> {
        self.visible.iter().position(|&i| i == row)
    }
    /// A click (or arrow step) on `row`. Plain: just the files under it.
    /// Ctrl: toggle them. Shift: the files from the anchor row to here,
    /// added to the selection with Ctrl+Shift.
    pub(super) fn click(&mut self, row: usize, modifiers: egui::Modifiers) {
        if modifiers.shift {
            let here = self.position(row).unwrap_or(0);
            let from = self.anchor.and_then(|a| self.position(a)).unwrap_or(here);
            if !modifiers.command {
                self.selected.clear();
            }
            // The files shown in the range, plus those hidden in collapsed
            // folders there. An open folder's own files are rows of the
            // range already; the rest of it is outside the range.
            for &r in &self.visible[from.min(here)..=from.max(here)] {
                let files: Vec<_> = if self.rows[r].collapsed {
                    self.files_under(r).collect()
                } else {
                    self.rows[r].file.into_iter().collect()
                };
                self.selected.extend(files);
            }
        } else if modifiers.command {
            let files: Vec<_> = self.files_under(row).collect();
            if self.row_selected(row) {
                for f in files {
                    self.selected.remove(&f);
                }
            } else {
                self.selected.extend(files);
            }
            self.anchor = Some(row);
        } else {
            self.selected = self.files_under(row).collect();
            self.anchor = Some(row);
        }
        self.cursor = Some(row);
        self.recount();
    }
    pub(super) fn select_all(&mut self) {
        self.select_files(0..self.files.len());
    }
    /// Up/Down: move the cursor one visible row, selecting what it lands on
    /// (Shift extends from the anchor instead).
    fn step(&mut self, down: bool, modifiers: egui::Modifiers) {
        let Some(last) = self.visible.len().checked_sub(1) else {
            return;
        };
        let next = match self.cursor.and_then(|c| self.position(c)) {
            Some(p) if down => (p + 1).min(last),
            Some(p) => p.saturating_sub(1),
            None if down => 0,
            None => last,
        };
        let shift = egui::Modifiers {
            shift: modifiers.shift,
            ..Default::default()
        };
        self.click(self.visible[next], shift);
        self.reveal = true;
    }
    fn toggle(&mut self, row: usize) {
        self.rows[row].collapsed = !self.rows[row].collapsed;
        self.rebuild_visible();
    }
    /// Carry the selection, cursor and anchor over from the tree this one
    /// replaces, by row key: section plus path, so a file staged since the
    /// last read is a different row.
    pub(super) fn keep_selection(&mut self, old: &FileTree) {
        let keys: HashSet<&str> = old
            .rows
            .iter()
            .filter(|r| r.file.is_some_and(|f| old.selected.contains(&f)))
            .map(|r| r.key.as_str())
            .collect();
        let key = |row: Option<usize>| row.map(|r| old.rows[r].key.as_str());
        let (cursor, anchor) = (key(old.cursor), key(old.anchor));
        self.selected.clear();
        for (i, row) in self.rows.iter().enumerate() {
            if let Some(f) = row.file
                && keys.contains(row.key.as_str())
            {
                self.selected.insert(f);
            }
            if cursor == Some(row.key.as_str()) {
                self.cursor = Some(i);
            }
            if anchor == Some(row.key.as_str()) {
                self.anchor = Some(i);
            }
        }
        self.recount();
    }
}

/// One context-menu entry, built by the owning view from the selection.
/// `action` is the owner's own type; the tree hands it back unread.
pub(super) struct MenuItem<A> {
    pub(super) action: A,
    pub(super) label: &'static str,
    pub(super) enabled: bool,
}

/// What the tree asks its owner to do this frame.
#[derive(Debug, PartialEq)]
pub(super) enum TreeEvent<A> {
    /// Open this file's diff: a double-click or Enter.
    Open(usize),
    /// The chosen menu item's action, for `files`.
    Act { action: A, files: Vec<usize> },
    /// A checkbox click or Space ticked (`on`) or unticked these files. The
    /// tree has already updated its own checks.
    Check { files: Vec<usize>, on: bool },
}

/// Draw the tree and run its selection gestures. `menu(files)` builds the
/// context menu for those files; an empty list means no menu. `keys`: this
/// tree reads the keyboard (its window is active); it still yields to any
/// focused egui widget, such as the commit message box. Checkboxes (rows in
/// a checkable section) toggle on click or, for the selection, on Space.
pub(super) fn show<A: Copy>(
    ui: &mut egui::Ui,
    tree: &mut FileTree,
    scale: f32,
    keys: bool,
    menu: &dyn Fn(&[usize]) -> Vec<MenuItem<A>>,
) -> Option<TreeEvent<A>> {
    ui.spacing_mut().item_spacing.y = 0.0;
    let row_height = 20.0 * scale;
    let th = crate::theme::live(ui.ctx());
    let font = egui::FontId::proportional(13.0 * scale);
    let folder = crate::icons::texture(
        ui.ctx(),
        crate::icons::IconKind::Folder,
        (14.0 * scale * ui.ctx().pixels_per_point()).ceil().max(1.0) as u32,
    );
    let menu_id = ui.id().with("file-menu");
    let menu_open = egui::Popup::is_id_open(ui.ctx(), menu_id);
    let mut event = None;
    // Where to open the menu this frame: the pointer, or the cursor row.
    let mut open_menu: Option<Option<egui::Pos2>> = None;
    if keys && !menu_open && ui.memory(|m| m.focused().is_none()) {
        let checks = tree.rows.iter().any(|r| r.check);
        for (key, modifiers) in take_keys(ui, checks) {
            match key {
                egui::Key::Space => event = tree.toggle_checks(tree.selected()),
                egui::Key::ArrowUp | egui::Key::ArrowDown => {
                    tree.step(key == egui::Key::ArrowDown, modifiers)
                }
                egui::Key::A => tree.select_all(),
                egui::Key::ArrowLeft | egui::Key::ArrowRight => {
                    if let Some(c) = tree.cursor
                        && tree.rows[c].file.is_none()
                        && tree.rows[c].collapsed == (key == egui::Key::ArrowRight)
                    {
                        tree.toggle(c);
                    }
                }
                egui::Key::Enter => match tree.cursor.map(|c| (c, tree.rows[c].file)) {
                    Some((_, Some(file))) => event = Some(TreeEvent::Open(file)),
                    Some((c, None)) => tree.toggle(c),
                    None => {}
                },
                // Shift+F10; egui-winit 0.34 drops the Menu key itself.
                _ => open_menu = Some(None),
            }
        }
    }
    // Keep the cursor row on screen after a keyboard move.
    let mut scroll = egui::ScrollArea::both()
        .id_salt("files")
        .auto_shrink([false, false]);
    if std::mem::take(&mut tree.reveal)
        && let Some(p) = tree.cursor.and_then(|c| tree.position(c))
    {
        let top = p as f32 * row_height;
        let offset = if top < tree.scroll {
            Some(top)
        } else if top + row_height > tree.scroll + tree.viewport {
            Some(top + row_height - tree.viewport)
        } else {
            None
        };
        if let Some(offset) = offset {
            scroll = scroll.vertical_scroll_offset(offset.max(0.0));
        }
    }
    let mut gesture = None;
    let mut cursor_rect = None;
    let out = scroll.show_rows(ui, row_height, tree.visible.len(), |ui, range| {
        for i in range {
            let index = tree.visible[i];
            let row = &tree.rows[index];
            let color = row
                .file
                .map(|f| status_color(tree.files[f].status))
                .unwrap_or(th.text);
            let label = ui
                .painter()
                .layout_no_wrap(display_path(&row.label), font.clone(), color);
            let indent = row.depth as f32 * 18.0 * scale;
            // A checkbox sits right of the disclosure strip and pushes the rest over.
            let boxed = if row.check { 18.0 * scale } else { 0.0 };
            // Sections have no icon, so their label sits where the icon would.
            let label_x = if row.section { 16.0 } else { 58.0 } * scale + boxed;
            // Flat: the file's directory, dim, after its name.
            let detail = row
                .file
                .filter(|_| tree.flat)
                .and_then(|f| tree.files[f].path.rsplit_once('/'))
                .map(|(dir, _)| {
                    ui.painter()
                        .layout_no_wrap(display_path(dir), font.clone(), th.dim)
                });
            let detail_w = detail.as_ref().map_or(0.0, |d| d.size().x + 8.0 * scale);
            let width = (indent + label_x + label.size().x + detail_w + 60.0 * scale)
                .max(ui.available_width());
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::click());
            if tree.cursor == Some(index) {
                cursor_rect = Some(rect);
            }
            let selected = tree.row_selected(index);
            if selected || response.hovered() {
                ui.painter().rect_filled(
                    rect,
                    2.0,
                    if selected {
                        th.sel_bg
                    } else {
                        th.sel_bg.gamma_multiply(0.45)
                    },
                );
            }
            let x = rect.left() + indent;
            let cy = rect.center().y;
            if tree.cursor == Some(index) && keys {
                ui.painter().rect_stroke(
                    rect.shrink(0.5),
                    2.0,
                    egui::Stroke::new(1.0, th.sel_bg.gamma_multiply(1.6)),
                    egui::StrokeKind::Inside,
                );
            }
            let painter = ui.painter();
            if row.check {
                let state = tree.row_checked(index);
                check_box(painter, egui::pos2(x + 22.0 * scale, cy), scale, state, &th);
            }
            // Reuse the Sessions folder asset; draw a folded page for files.
            // Neither icon nor disclosure relies on platform font glyphs.
            let center = egui::pos2(x + 26.0 * scale + boxed, cy);
            if row.file.is_none() {
                let arrow = egui::pos2(x + 7.0 * scale, cy);
                let points = if row.collapsed {
                    [(-2.0, -3.0), (2.0, 0.0), (-2.0, 3.0)]
                } else {
                    [(-3.0, -2.0), (3.0, -2.0), (0.0, 2.0)]
                };
                painter.add(egui::Shape::convex_polygon(
                    points
                        .into_iter()
                        .map(|(x, y)| arrow + egui::vec2(x, y) * scale)
                        .collect(),
                    th.dim,
                    egui::Stroke::NONE,
                ));
                if !row.section {
                    painter.image(
                        folder.id(),
                        egui::Rect::from_center_size(center, egui::vec2(14.0, 14.0) * scale),
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        crate::icons::IconKind::Folder.tint(),
                    );
                }
            } else {
                let point = |x, y| center + egui::vec2(x, y) * scale;
                let stroke = egui::Stroke::new(scale, th.dim);
                painter.add(egui::Shape::closed_line(
                    vec![
                        point(-5.0, -6.0),
                        point(1.0, -6.0),
                        point(5.0, -2.0),
                        point(5.0, 6.0),
                        point(-5.0, 6.0),
                    ],
                    stroke,
                ));
                painter.add(egui::Shape::line(
                    vec![point(1.0, -6.0), point(1.0, -2.0), point(5.0, -2.0)],
                    stroke,
                ));
                for y in [1.0, 3.5] {
                    painter.line_segment([point(-2.5, y), point(2.5, y)], stroke);
                }
            }
            if let Some(file_index) = row.file {
                let file = &tree.files[file_index];
                painter.text(
                    egui::pos2(x + 44.0 * scale + boxed, cy),
                    egui::Align2::CENTER_CENTER,
                    file.status,
                    font.clone(),
                    color,
                );
            } else {
                let count = match row.file_count {
                    1 => "1 file".to_owned(),
                    n => format!("{n} files"),
                };
                painter.text(
                    egui::pos2(x + label_x + label.size().x + 8.0 * scale, cy),
                    egui::Align2::LEFT_CENTER,
                    count,
                    font.clone(),
                    th.dim,
                );
            }
            let label_w = label.size().x;
            painter.galley(
                egui::pos2(x + label_x, cy - label.size().y * 0.5),
                label,
                color,
            );
            if let Some(detail) = detail {
                painter.galley(
                    egui::pos2(
                        x + label_x + label_w + 8.0 * scale,
                        cy - detail.size().y * 0.5,
                    ),
                    detail,
                    th.dim,
                );
            }
            // Both clicks of a double-click also report `clicked`: the first
            // selects (or toggles, on the arrow), the second opens or toggles.
            // Each click on a checkbox toggles it, and nothing else.
            let press_x = response.interact_pointer_pos().map(|p| p.x - x);
            let on_arrow = press_x.is_some_and(|px| px < 16.0 * scale);
            let on_box =
                row.check && press_x.is_some_and(|px| (14.0 * scale..30.0 * scale).contains(&px));
            let modifiers = ui.input(|i| i.modifiers);
            let pressed = if on_box && (response.clicked() || response.double_clicked()) {
                Some(Gesture::Check)
            } else if response.double_clicked() {
                Some(match row.file {
                    Some(file) => Gesture::Open(file),
                    None => Gesture::Toggle,
                })
            } else if response.clicked() {
                Some(if row.file.is_none() && on_arrow {
                    Gesture::Toggle
                } else {
                    Gesture::Select(modifiers)
                })
            } else if response.secondary_clicked() {
                Some(Gesture::Menu(response.interact_pointer_pos()))
            } else {
                None
            };
            if let Some(pressed) = pressed {
                gesture = Some((index, pressed));
            }
            if let Some(file_index) = row.file {
                let file = &tree.files[file_index];
                response.on_hover_text(match &file.previous {
                    Some(old) => format!(
                        "{}\n{} → {}",
                        status_name(file.status),
                        display_path(old),
                        display_path(&file.path)
                    ),
                    None => {
                        format!("{}\n{}", status_name(file.status), display_path(&file.path))
                    }
                });
            }
        }
    });
    tree.scroll = out.state.offset.y;
    tree.viewport = out.inner_rect.height();
    match gesture {
        Some((row, Gesture::Select(modifiers))) => tree.click(row, modifiers),
        Some((row, Gesture::Toggle)) => tree.toggle(row),
        Some((row, Gesture::Check)) => event = tree.toggle_checks(tree.files_under(row).collect()),
        Some((_, Gesture::Open(file))) => event = Some(TreeEvent::Open(file)),
        // Right-click keeps a selected group; anything else selects just it.
        Some((row, Gesture::Menu(at))) => {
            if tree.row_selected(row) {
                tree.cursor = Some(row);
            } else {
                tree.click(row, egui::Modifiers::NONE);
            }
            open_menu = Some(at);
        }
        None => {}
    }
    if let Some(at) = open_menu {
        let at = at
            .or(cursor_rect.map(|r| r.left_bottom() + egui::vec2(24.0 * scale, 0.0)))
            .unwrap_or(out.inner_rect.left_top());
        let files = tree.selected();
        if !files.is_empty() && !menu(&files).is_empty() {
            tree.menu_at = at;
            egui::Popup::open_id(ui.ctx(), menu_id);
        }
    }
    if egui::Popup::is_id_open(ui.ctx(), menu_id) {
        let files = tree.selected();
        let items = menu(&files);
        egui::Popup::new(
            menu_id,
            ui.ctx().clone(),
            egui::PopupAnchor::Position(tree.menu_at),
            ui.layer_id(),
        )
        .kind(egui::PopupKind::Menu)
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .style(egui::containers::menu::menu_style)
        .open_memory(None)
        .show(|ui| {
            for m in &items {
                if ui
                    .add_enabled(m.enabled, egui::Button::new(m.label))
                    .clicked()
                {
                    event = Some(TreeEvent::Act {
                        action: m.action,
                        files: files.clone(),
                    });
                }
            }
        });
    }
    event
}

/// A row's pointer gesture, applied after the rows are painted.
enum Gesture {
    Select(egui::Modifiers),
    Toggle,
    /// A click on the row's checkbox.
    Check,
    Open(usize),
    /// Right-click, at this position.
    Menu(Option<egui::Pos2>),
}

/// Drain the tree's key presses. Taken from the event list so nothing
/// behind the tree sees them too.
fn take_keys(ui: &egui::Ui, checks: bool) -> Vec<(egui::Key, egui::Modifiers)> {
    use egui::{Key, Modifiers};
    ui.ctx().input_mut(|i| {
        let mut keys = Vec::new();
        i.events.retain(|e| {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = e
            else {
                return true;
            };
            let ours = match key {
                Key::ArrowUp | Key::ArrowDown => {
                    *modifiers == Modifiers::NONE || *modifiers == Modifiers::SHIFT
                }
                Key::ArrowLeft | Key::ArrowRight | Key::Enter => *modifiers == Modifiers::NONE,
                Key::A => modifiers.command && !modifiers.shift && !modifiers.alt,
                Key::F10 => *modifiers == Modifiers::SHIFT,
                Key::Space => checks && *modifiers == Modifiers::NONE,
                _ => false,
            };
            if ours {
                keys.push((*key, *modifiers));
            }
            !ours
        });
        keys
    })
}

/// A row's checkbox, painted (the row's own response takes the click):
/// ticked, empty, or `None` for a folder whose files are partly ticked.
fn check_box(
    painter: &egui::Painter,
    center: egui::Pos2,
    scale: f32,
    state: Option<bool>,
    th: &crate::theme::Theme,
) {
    let rect = egui::Rect::from_center_size(center, egui::vec2(12.0, 12.0) * scale);
    let fill = if state == Some(false) {
        th.bg
    } else {
        th.sel_bg
    };
    painter.rect(
        rect,
        2.0 * scale,
        fill,
        egui::Stroke::new(scale, th.dim),
        egui::StrokeKind::Inside,
    );
    let point = |x, y| center + egui::vec2(x, y) * scale;
    let stroke = egui::Stroke::new(1.5 * scale, th.text);
    match state {
        Some(true) => {
            painter.add(egui::Shape::line(
                vec![point(-3.0, 0.0), point(-1.0, 2.5), point(3.5, -2.5)],
                stroke,
            ));
        }
        None => {
            painter.line_segment([point(-3.0, 0.0), point(3.0, 0.0)], stroke);
        }
        Some(false) => {}
    }
}

pub(super) fn display_path(path: &str) -> String {
    path.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
fn status_name(status: char) -> &'static str {
    match status {
        'A' => "Added",
        'D' => "Deleted",
        'R' => "Renamed",
        'C' => "Copied",
        'T' => "Type changed",
        'M' => "Modified",
        'U' => "Unmerged (conflict)",
        '?' => "Untracked",
        _ => "Other",
    }
}
pub(super) fn status_color(status: char) -> egui::Color32 {
    // JetBrains Darcula FILESTATUS colors, independent of graph lane colors.
    // Copies are additions; Git type changes use the modified-file color.
    match status {
        'A' | 'C' => egui::Color32::from_rgb(0x62, 0x97, 0x55),
        'D' => egui::Color32::from_rgb(0x6c, 0x6c, 0x6c),
        'R' => egui::Color32::from_rgb(0x3a, 0x84, 0x84),
        'M' | 'T' => egui::Color32::from_rgb(0x68, 0x97, 0xbb),
        // FILESTATUS_UNKNOWN and FILESTATUS_MERGED_WITH_CONFLICTS.
        '?' => egui::Color32::from_rgb(0xd1, 0x67, 0x5a),
        'U' => egui::Color32::from_rgb(0xd5, 0x75, 0x6c),
        _ => super::COLORS[3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(status: char, path: &str) -> ChangedFile {
        ChangedFile {
            status,
            path: path.into(),
            previous: None,
        }
    }

    /// Rows: 0 Staged, 1 src, 2 a.rs (f0), 3 Changes, 4 src, 5 a.rs (f1),
    /// 6 b.rs (f2), 7 c.rs (f3). Each 20px tall from y = 0.
    fn sectioned() -> FileTree {
        FileTree::sections(vec![
            ("Staged", vec![file('M', "src/a.rs")]),
            (
                "Changes",
                vec![file('M', "src/a.rs"), file('D', "b.rs"), file('A', "c.rs")],
            ),
        ])
    }

    struct Harness {
        ctx: egui::Context,
        tree: FileTree,
        /// The files each `menu` call was asked about.
        asked: std::cell::RefCell<Vec<Vec<usize>>>,
        ui_id: Option<egui::Id>,
    }
    impl Harness {
        fn new(tree: FileTree) -> Self {
            Self {
                ctx: egui::Context::default(),
                tree,
                asked: Default::default(),
                ui_id: None,
            }
        }
        fn frame(&mut self, events: Vec<egui::Event>) -> Option<TreeEvent<&'static str>> {
            self.frame_with(events, NONE)
        }
        /// One frame; `modifiers` held, as egui-winit reports them.
        fn frame_with(
            &mut self,
            events: Vec<egui::Event>,
            modifiers: egui::Modifiers,
        ) -> Option<TreeEvent<&'static str>> {
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 300.0));
            let mut out = None;
            let asked = &self.asked;
            let menu = |files: &[usize]| {
                asked.borrow_mut().push(files.to_vec());
                vec![
                    MenuItem {
                        action: "first",
                        label: "First",
                        enabled: true,
                    },
                    MenuItem {
                        action: "second",
                        label: "Second",
                        enabled: files.len() > 1,
                    },
                ]
            };
            let (tree, ui_id) = (&mut self.tree, &mut self.ui_id);
            let _ = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    events,
                    modifiers,
                    ..Default::default()
                },
                |ui| {
                    *ui_id = Some(ui.id());
                    out = show(ui, tree, 1.0, true, &menu);
                },
            );
            out
        }
        fn press(
            &mut self,
            pos: egui::Pos2,
            button: egui::PointerButton,
            modifiers: egui::Modifiers,
        ) -> Option<TreeEvent<&'static str>> {
            let mut out = self.frame_with(vec![egui::Event::PointerMoved(pos)], modifiers);
            for pressed in [true, false] {
                let event = egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                };
                out = out.or(self.frame_with(vec![event], modifiers));
            }
            out
        }
        /// A primary click on visible row `row`, on its label.
        fn click(
            &mut self,
            row: usize,
            modifiers: egui::Modifiers,
        ) -> Option<TreeEvent<&'static str>> {
            let pos = egui::pos2(200.0, 20.0 * row as f32 + 10.0);
            self.press(pos, egui::PointerButton::Primary, modifiers)
        }
        fn right_click(&mut self, row: usize) -> Option<TreeEvent<&'static str>> {
            let pos = egui::pos2(200.0, 20.0 * row as f32 + 10.0);
            self.press(pos, egui::PointerButton::Secondary, NONE)
        }
        fn key(
            &mut self,
            key: egui::Key,
            modifiers: egui::Modifiers,
        ) -> Option<TreeEvent<&'static str>> {
            let event = egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            };
            self.frame_with(vec![event], modifiers)
        }
        fn menu_rect(&self) -> Option<egui::Rect> {
            let id = self.ui_id.unwrap().with("file-menu");
            egui::Popup::is_id_open(&self.ctx, id)
                .then(|| self.ctx.memory(|m| m.area_rect(id)))
                .flatten()
        }
    }
    const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;
    const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;
    const NONE: egui::Modifiers = egui::Modifiers::NONE;

    #[test]
    fn click_selects_one_ctrl_toggles_and_shift_takes_the_range_from_the_anchor() {
        let mut h = Harness::new(sectioned());
        // A single click selects only: opening is a double-click.
        assert_eq!(h.click(5, NONE), None);
        assert_eq!(h.tree.selected(), [1]);
        h.click(6, NONE);
        assert_eq!(h.tree.selected(), [2]);
        h.click(7, CTRL);
        assert_eq!(h.tree.selected(), [2, 3]);
        h.click(6, CTRL);
        assert_eq!(h.tree.selected(), [3]);
        // The anchor is the last plain or Ctrl click (row 6). The range spans
        // the section boundary; the open Changes heading inside it does not
        // drag in c.rs below it.
        h.click(2, SHIFT);
        assert_eq!(h.tree.selected(), [0, 1, 2]);
        assert!(!h.tree.row_selected(3));
        // Shift again re-ranges from the same anchor instead of growing.
        h.click(7, SHIFT);
        assert_eq!(h.tree.selected(), [2, 3]);
        // Ctrl+Shift adds the range to what is already selected.
        h.click(0, NONE);
        h.click(5, CTRL);
        h.click(7, CTRL | SHIFT);
        assert_eq!(h.tree.selected(), [0, 1, 2, 3]);
        assert_eq!(h.tree.cursor, Some(7));
    }

    #[test]
    fn folder_and_section_rows_select_everything_under_them_and_the_arrow_toggles() {
        let mut h = Harness::new(sectioned());
        h.click(3, NONE);
        assert_eq!(h.tree.selected(), [1, 2, 3]);
        assert!(h.tree.row_selected(3) && h.tree.row_selected(4));
        assert!(!h.tree.row_selected(0));
        h.click(1, NONE);
        assert_eq!(h.tree.selected(), [0]);
        assert!(
            h.tree.row_selected(0),
            "a section whose files are all selected"
        );
        // The disclosure arrow collapses without touching the selection.
        h.press(egui::pos2(8.0, 70.0), egui::PointerButton::Primary, NONE);
        assert!(h.tree.rows[3].collapsed);
        assert_eq!(h.tree.selected(), [0]);
        // Ctrl on a collapsed section toggles its hidden files too.
        h.click(3, CTRL);
        assert_eq!(h.tree.selected(), [0, 1, 2, 3]);
        h.click(3, CTRL);
        assert_eq!(h.tree.selected(), [0]);
    }

    #[test]
    fn double_click_opens_and_double_click_on_a_folder_toggles_it() {
        let mut h = Harness::new(sectioned());
        assert_eq!(h.click(6, NONE), None);
        assert_eq!(h.click(6, NONE), Some(TreeEvent::Open(2)));
        assert_eq!(h.tree.selected(), [2]);
        // egui counts any quick third click as another double: start over.
        h.ctx = egui::Context::default();
        h.click(4, NONE);
        h.click(4, NONE);
        assert!(h.tree.rows[4].collapsed);
        assert_eq!(h.tree.selected(), [1]);
    }

    #[test]
    fn keyboard_moves_extends_selects_all_opens_and_collapses() {
        let mut h = Harness::new(sectioned());
        let down = egui::Key::ArrowDown;
        h.key(down, NONE);
        assert_eq!((h.tree.cursor, h.tree.selected()), (Some(0), vec![0]));
        h.click(5, NONE);
        h.key(down, NONE);
        assert_eq!((h.tree.cursor, h.tree.selected()), (Some(6), vec![2]));
        h.key(down, SHIFT);
        assert_eq!(h.tree.selected(), [2, 3]);
        // The last row stays put.
        h.key(down, SHIFT);
        assert_eq!(h.tree.cursor, Some(7));
        h.key(egui::Key::ArrowUp, SHIFT);
        h.key(egui::Key::ArrowUp, SHIFT);
        assert_eq!(h.tree.selected(), [1, 2]);
        assert_eq!(h.key(egui::Key::Enter, NONE), Some(TreeEvent::Open(1)));
        h.key(egui::Key::A, CTRL);
        assert_eq!(h.tree.selected(), [0, 1, 2, 3]);
        // Left collapses the folder under the cursor; Enter toggles it back.
        h.key(egui::Key::ArrowUp, NONE);
        assert_eq!(h.tree.cursor, Some(4));
        h.key(egui::Key::ArrowLeft, NONE);
        assert!(h.tree.rows[4].collapsed);
        assert_eq!(h.key(egui::Key::Enter, NONE), None);
        assert!(!h.tree.rows[4].collapsed);
    }

    #[test]
    fn keyboard_scrolls_the_cursor_into_view() {
        let files = (0..40).map(|i| file('M', &format!("f{i:02}.rs"))).collect();
        let mut h = Harness::new(FileTree::new(files));
        h.frame(vec![]);
        for _ in 0..30 {
            h.key(egui::Key::ArrowDown, NONE);
        }
        h.frame(vec![]);
        // Row 29's bottom edge (600px) sits at the viewport's bottom.
        assert_eq!(h.tree.cursor, Some(29));
        assert!(
            (h.tree.scroll + h.tree.viewport - 600.0).abs() < 1.0,
            "{} {}",
            h.tree.scroll,
            h.tree.viewport
        );
    }

    #[test]
    fn right_click_keeps_a_selected_group_and_otherwise_selects_just_the_row() {
        let mut h = Harness::new(sectioned());
        h.click(5, NONE);
        h.click(7, SHIFT);
        assert_eq!(h.right_click(6), None);
        assert_eq!(h.tree.selected(), [1, 2, 3]);
        assert_eq!(h.tree.cursor, Some(6));
        h.frame(vec![]);
        let rect = h.menu_rect().expect("menu open");
        assert_eq!(h.asked.borrow().last().unwrap(), &[1, 2, 3]);
        // The second item: enabled for a group.
        let pos = egui::pos2(rect.left() + 20.0, rect.bottom() - 12.0);
        assert_eq!(
            h.press(pos, egui::PointerButton::Primary, NONE),
            Some(TreeEvent::Act {
                action: "second",
                files: vec![1, 2, 3]
            })
        );
        h.frame(vec![]);
        assert!(h.menu_rect().is_none(), "choosing an item closes the menu");
        // An unselected row: just it.
        h.right_click(2);
        assert_eq!(h.tree.selected(), [0]);
        h.frame(vec![]);
        assert!(h.menu_rect().is_some());
        h.key(egui::Key::Escape, NONE);
        h.frame(vec![]);
        assert!(h.menu_rect().is_none());
        // Shift+F10 opens it at the cursor row.
        h.key(egui::Key::F10, SHIFT);
        h.frame(vec![]);
        let rect = h.menu_rect().expect("Shift+F10 opens the menu");
        assert!((rect.top() - 60.0).abs() < 1.0, "{rect:?}");
    }

    #[test]
    fn selection_cursor_and_anchor_survive_a_re_read_by_section_and_path() {
        let mut old = sectioned();
        old.click(5, NONE);
        old.click(7, SHIFT);
        assert_eq!(old.selected(), [1, 2, 3]);
        // b.rs was staged meanwhile, and 0.rs appeared.
        let mut new = FileTree::sections(vec![
            ("Staged", vec![file('M', "src/a.rs"), file('D', "b.rs")]),
            (
                "Changes",
                vec![file('M', "src/a.rs"), file('A', "c.rs"), file('A', "0.rs")],
            ),
        ]);
        new.keep_selection(&old);
        // Changes/src/a.rs and Changes/c.rs carry; b.rs moved sections.
        let paths: Vec<_> = new
            .selected()
            .iter()
            .map(|&f| new.files[f].path.as_str())
            .collect();
        assert_eq!(paths, ["src/a.rs", "c.rs"]);
        assert_eq!(new.rows[new.cursor.unwrap()].key, "Changes/c.rs");
        assert_eq!(new.rows[new.anchor.unwrap()].key, "Changes/src/a.rs");
        // Through the pointer: a Shift click still ranges from the carried anchor.
        let mut h = Harness::new(new);
        let row = h
            .tree
            .visible
            .iter()
            .position(|&r| h.tree.rows[r].key == "Changes/0.rs")
            .unwrap();
        h.click(row, SHIFT);
        // Anchor Changes/src/a.rs (f2) through 0.rs (f4), which sorts before c.rs.
        assert_eq!(h.tree.selected(), [2, 4]);
    }

    #[test]
    fn checkboxes_tick_folders_show_partial_and_space_toggles_the_selection() {
        // Rows: 0 Staged (no boxes), 1 src, 2 a.rs, 3 Changes, 4 src,
        // 5 a.rs (f1), 6 b.rs (f2), 7 c.rs (f3).
        let tree = FileTree::grouped(
            vec![
                ("Staged", vec![file('M', "src/a.rs")], false),
                (
                    "Changes",
                    vec![file('M', "src/a.rs"), file('D', "b.rs"), file('A', "c.rs")],
                    true,
                ),
            ],
            false,
        );
        let mut h = Harness::new(tree);
        let tick = |h: &mut Harness, row: usize| {
            let pos = egui::pos2(
                18.0 * h.tree.rows[row].depth as f32 + 22.0,
                20.0 * row as f32 + 10.0,
            );
            h.press(pos, egui::PointerButton::Primary, NONE)
        };
        // The folder's box ticks everything under it, and nothing else:
        // the selection stays as it was.
        assert_eq!(
            tick(&mut h, 4),
            Some(TreeEvent::Check {
                files: vec![1],
                on: true
            })
        );
        assert!(h.tree.selected().is_empty());
        assert_eq!(h.tree.row_checked(3), None, "Changes is partly ticked");
        assert_eq!(h.tree.row_checked(4), Some(true));
        // A section without boxes ignores the click as a box.
        tick(&mut h, 1);
        assert_eq!(h.tree.checked(), [1]);
        // Space ticks the selected checkable files; again unticks them.
        h.click(3, NONE);
        let space = h.key(egui::Key::Space, NONE);
        assert_eq!(
            space,
            Some(TreeEvent::Check {
                files: vec![1, 2, 3],
                on: true
            })
        );
        assert_eq!(h.tree.row_checked(3), Some(true));
        h.key(egui::Key::Space, NONE);
        assert!(h.tree.checked().is_empty());
        // Flat keeps the checks and the selection.
        h.tree.set_checked([2]);
        h.tree.set_flat(true);
        let labels: Vec<_> = h.tree.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Staged", "a.rs", "Changes", "a.rs", "b.rs", "c.rs"]
        );
        assert_eq!(h.tree.checked(), [2]);
        assert_eq!(h.tree.selected(), [1, 2, 3]);
    }

    #[test]
    fn file_statuses_use_darcula_colors_independently_of_graph_lanes() {
        for (statuses, rgb) in [
            ("AC", [0x62, 0x97, 0x55]),
            ("MT", [0x68, 0x97, 0xbb]),
            ("D", [0x6c, 0x6c, 0x6c]),
            ("R", [0x3a, 0x84, 0x84]),
            ("?", [0xd1, 0x67, 0x5a]),
            ("U", [0xd5, 0x75, 0x6c]),
        ] {
            for status in statuses.chars() {
                assert_eq!(
                    status_color(status),
                    egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2])
                );
            }
        }
    }

    #[test]
    fn single_child_directory_chains_fold_into_one_row() {
        let tree = FileTree::new(vec![
            file('M', ".foreman/tasks/a.json"),
            file('M', ".foreman/tasks/b.json"),
            file('A', "src/x/y.rs"),
            file('A', "src/z.rs"),
        ]);
        let labels: Vec<_> = tree.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                ".foreman/tasks",
                "a.json",
                "b.json",
                "src",
                "x",
                "y.rs",
                "z.rs"
            ]
        );
        assert_eq!(tree.rows[0].file_count, 2);
        assert_eq!(tree.rows[3].file_count, 2);
        assert_eq!(tree.rows[4].file_count, 1);
    }

    #[test]
    fn sections_head_their_own_trees_and_skip_empty_groups() {
        let mut tree = FileTree::sections(vec![
            ("Staged", vec![file('M', "src/a.rs")]),
            ("Conflicts", vec![]),
            ("Changes", vec![file('M', "src/a.rs"), file('D', "b.rs")]),
        ]);
        let rows: Vec<_> = tree
            .rows
            .iter()
            .map(|r| (r.label.as_str(), r.depth, r.section, r.file))
            .collect();
        assert_eq!(
            rows,
            [
                ("Staged", 0, true, None),
                ("src", 1, false, None),
                ("a.rs", 2, false, Some(0)),
                ("Changes", 0, true, None),
                ("src", 1, false, None),
                ("a.rs", 2, false, Some(1)),
                ("b.rs", 1, false, Some(2)),
            ]
        );
        assert_eq!(tree.rows[3].file_count, 2);
        assert_eq!(tree.rows[0].end, 3);
        // Same directory in two sections: distinct keys, so collapse is per section.
        tree.rows[1].collapsed = true;
        let keys = tree.collapsed_keys();
        let mut again = FileTree::sections(vec![
            ("Staged", vec![file('M', "src/a.rs")]),
            ("Changes", vec![file('M', "src/a.rs")]),
        ]);
        again.collapse(&keys);
        assert!(again.rows[1].collapsed);
        assert!(!again.rows[4].collapsed);
        assert_eq!(again.visible, [0, 1, 3, 4, 5]);
    }
}
