//! The status-colored changed-file tree shared by the commit details pane and
//! the Git Changes window: pure row building off-thread, virtualized painting.
use eframe::egui;
use std::collections::{BTreeMap, HashSet};

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
            });
        }
        count
    }
}

#[derive(Default)]
pub(super) struct FileTree {
    pub(super) files: Vec<ChangedFile>,
    pub(super) rows: Vec<TreeRow>,
    pub(super) visible: Vec<usize>,
    pub(super) selected: Option<usize>,
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
    pub(super) fn sections(sections: Vec<(&str, Vec<ChangedFile>)>) -> Self {
        let mut files = Vec::new();
        let mut rows = Vec::new();
        for (name, section) in sections {
            if section.is_empty() {
                continue;
            }
            let mut root = Directory::default();
            for file in section {
                root.insert(&file.path, files.len());
                files.push(file);
            }
            let index = rows.len();
            rows.push(TreeRow {
                label: name.into(),
                depth: 0,
                file: None,
                end: 0,
                collapsed: false,
                file_count: 0,
                section: true,
                key: name.into(),
            });
            rows[index].file_count = root.flatten(1, name, &mut rows);
            rows[index].end = rows.len();
        }
        Self::from_rows(files, rows)
    }
    fn from_rows(files: Vec<ChangedFile>, rows: Vec<TreeRow>) -> Self {
        let visible = (0..rows.len()).collect();
        Self {
            files,
            rows,
            visible,
            selected: None,
        }
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
}

/// Draw the file tree; returns the file clicked this frame, already selected.
pub(super) fn show(ui: &mut egui::Ui, tree: &mut FileTree, scale: f32) -> Option<usize> {
    ui.spacing_mut().item_spacing.y = 0.0;
    let row_height = 20.0 * scale;
    let th = crate::theme::live(ui.ctx());
    let font = egui::FontId::proportional(13.0 * scale);
    let folder = crate::icons::texture(
        ui.ctx(),
        crate::icons::IconKind::Folder,
        (14.0 * scale * ui.ctx().pixels_per_point()).ceil().max(1.0) as u32,
    );
    let mut toggled = None;
    let mut clicked = None;
    egui::ScrollArea::both()
        .id_salt("files")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, tree.visible.len(), |ui, range| {
            for i in range {
                let index = tree.visible[i];
                let row = &tree.rows[index];
                let color = row
                    .file
                    .map(|f| status_color(tree.files[f].status))
                    .unwrap_or(th.text);
                let label =
                    ui.painter()
                        .layout_no_wrap(display_path(&row.label), font.clone(), color);
                let indent = row.depth as f32 * 18.0 * scale;
                // Sections have no icon, so their label sits where the icon would.
                let label_x = if row.section { 16.0 } else { 58.0 } * scale;
                let width =
                    (indent + label_x + label.size().x + 60.0 * scale).max(ui.available_width());
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::click());
                let selected = row.file.is_some() && tree.selected == row.file;
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
                let painter = ui.painter();
                // Reuse the Sessions folder asset; draw a folded page for files.
                // Neither icon nor disclosure relies on platform font glyphs.
                let center = egui::pos2(x + 26.0 * scale, cy);
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
                        egui::pos2(x + 44.0 * scale, cy),
                        egui::Align2::CENTER_CENTER,
                        file.status,
                        font.clone(),
                        color,
                    );
                    if response.clicked() {
                        tree.selected = Some(file_index);
                        clicked = Some(file_index);
                    }
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
                } else {
                    if response.clicked() {
                        toggled = Some(index);
                    }
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
                painter.galley(
                    egui::pos2(x + label_x, cy - label.size().y * 0.5),
                    label,
                    color,
                );
            }
        });
    if let Some(index) = toggled {
        tree.rows[index].collapsed = !tree.rows[index].collapsed;
        tree.rebuild_visible();
    }
    clicked
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
