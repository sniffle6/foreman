//! **Create Layout…**: a Project-scoped editor that sends one description
//! and the Project's bounded window inventory to the one-shot provider,
//! shows the single candidate it proposes as a labeled diagram, and applies
//! it (Apply), saves then applies it (Save & Apply…), or drops it (Cancel).
//! Spec: `docs/superpowers/specs/2026-10-16-project-layouts-design.md`
//! (Create Layout, Generation boundary). Feature doc: `docs/project-layouts.md`.
//!
//! Single-shot: no history. Running again supersedes the pending request
//! (its receiver is dropped, so its reply can never publish) and replaces
//! the candidate. A request and a candidate are tied to the arrangement
//! revision they were made at; a structural change discards them ("Windows
//! changed — run again"), a resize does not. A candidate may add new
//! windows: the preview shows them, marked new, and only Apply opens them.
//! The editor lives in this
//! Project's picker state, so closing the Project or switching away from it
//! drops the request with the editor. Applying goes through the same
//! deferred, validated path as a saved layout, in its strict form
//! (`LayoutRequest::Candidate`).

use super::layout_picker::{Notice, PickerAction, Requested, kind_glyph};
use super::layouts::{LayoutApplyError, LayoutRequest};
use super::*;
use crate::layout_library::{OnConflict, StoreError};
use crate::layout_prompt::{self as lp, Entry, Inventory, Reply};
use crate::project_layout::{self as pl, ItemRef, LayoutDefinition, LayoutItemId, WindowState};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};

const EDITOR_W: f32 = 440.0;
const PREVIEW_H: f32 = 168.0;
const EXAMPLE: &str = "Large coding terminal on the left, two other terminals stacked in the \
                       middle, and chat on the right.";

/// One open editor. Runtime-only.
#[derive(Default)]
pub(super) struct CreateEditor {
    pub(super) description: String,
    /// Focus the description on the next frame.
    focus: bool,
    pending: Option<Pending>,
    pub(super) candidate: Option<Candidate>,
    pub(super) status: Option<Status>,
    pub(super) naming: Option<Naming>,
    /// Test seam: the sender a real request thread would own.
    #[cfg(test)]
    pub(super) test_tx: Option<mpsc::SyncSender<Result<Reply, String>>>,
}

impl CreateEditor {
    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

/// The one in-flight request of this Project.
struct Pending {
    rx: Receiver<Result<Reply, String>>,
    revision: u64,
    inventory: Inventory,
    icons: HashMap<LayoutItemId, crate::icons::IconKind>,
    program: &'static str,
}

/// The validated proposal. `entries` are the labels that were sent, then
/// one "New …" entry per added window.
pub(super) struct Candidate {
    pub(super) def: LayoutDefinition,
    pub(super) message: String,
    pub(super) revision: u64,
    /// New windows Apply opens; in `def`, never live yet.
    pub(super) added: Vec<ItemRef>,
    entries: Vec<Entry>,
    icons: HashMap<LayoutItemId, crate::icons::IconKind>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Status {
    Error(String),
    /// The windows changed under a request or candidate.
    Stale,
    /// The description needs Content that is not open.
    Missing {
        text: String,
        message: String,
    },
    /// The proposal is the arrangement the windows already have.
    Unchanged {
        message: String,
    },
}

/// Save & Apply's name row.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Naming {
    pub(super) buf: String,
    /// Another layout holds the name: `(id, name)`. Submit replaces it.
    pub(super) conflict: Option<(String, String)>,
    pub(super) error: Option<String>,
    focus: bool,
}

/// Everything the editor's controls can ask for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum CreateAction {
    /// Send the description (again). Supersedes a pending request.
    Run,
    Apply,
    /// Ask for a name, then save and apply.
    SaveAndApply,
    SubmitName,
    EditName,
    /// Leave the name row; the candidate stays.
    BackFromName,
    Cancel,
}

impl WindowManager {
    pub(super) fn open_create_editor(&mut self) {
        self.layout_ui.menu_open = false;
        self.layout_ui.dialog = None;
        self.layout_ui.notice = None;
        self.layout_ui.create = Some(CreateEditor {
            focus: true,
            ..Default::default()
        });
    }

    pub(super) fn run_create_action(
        &mut self,
        action: CreateAction,
        dir: &std::path::Path,
        ctx: &egui::Context,
    ) {
        if self.layout_ui.create.is_none() {
            return;
        }
        match action {
            CreateAction::Run => self.run_create_request(ctx),
            CreateAction::Apply => {
                let Some((def, added, revision)) = self.candidate_in_hand() else {
                    return;
                };
                match self.check_candidate(&def, &added, revision) {
                    Ok(()) => {
                        self.layout_ui.requested = Some(Requested::Create { saved: None });
                        self.request_layout(LayoutRequest::Candidate {
                            def,
                            added,
                            select: false,
                            revision,
                        });
                        self.layout_ui.create = None;
                        ctx.request_repaint();
                    }
                    Err(e) => self.create_failed(e),
                }
            }
            CreateAction::SaveAndApply => {
                if let Some(ed) = &mut self.layout_ui.create
                    && ed.candidate.is_some()
                {
                    ed.naming = Some(Naming {
                        focus: true,
                        ..Default::default()
                    });
                }
            }
            CreateAction::SubmitName => self.save_and_apply(dir, ctx),
            CreateAction::EditName => {
                if let Some(Naming {
                    conflict, error, ..
                }) = self
                    .layout_ui
                    .create
                    .as_mut()
                    .and_then(|e| e.naming.as_mut())
                {
                    *conflict = None;
                    *error = None;
                }
            }
            CreateAction::BackFromName => {
                if let Some(ed) = &mut self.layout_ui.create {
                    ed.naming = None;
                }
            }
            CreateAction::Cancel => self.layout_ui.create = None,
        }
    }

    fn candidate_in_hand(&self) -> Option<(LayoutDefinition, Vec<LayoutItemId>, u64)> {
        let c = self.layout_ui.create.as_ref()?.candidate.as_ref()?;
        let added = c.added.iter().map(|a| a.id).collect();
        Some((c.def.clone(), added, c.revision))
    }

    /// A candidate could not be used. Stale drops it; anything else keeps
    /// it and says why.
    fn create_failed(&mut self, e: LayoutApplyError) {
        let Some(ed) = &mut self.layout_ui.create else {
            return;
        };
        if e == LayoutApplyError::Stale {
            ed.candidate = None;
            ed.naming = None;
            ed.status = Some(Status::Stale);
        } else if let Some(naming) = &mut ed.naming {
            naming.error = Some(e.to_string());
        } else {
            ed.status = Some(Status::Error(e.to_string()));
        }
    }

    /// Capture the bounded inventory, then start one provider request in
    /// the background. Replaces any pending request and the candidate.
    fn run_create_request(&mut self, ctx: &egui::Context) {
        let Some(description) = self
            .layout_ui
            .create
            .as_ref()
            .map(|e| e.description.trim().to_string())
            .filter(|d| !d.is_empty())
        else {
            return;
        };
        let prepared = self.layout_area().and_then(|area| {
            let (items, current) = self.capture_arrangement(area);
            if items.is_empty() {
                return Ok(None);
            }
            Inventory::new(
                &items,
                |id| self.layout_item_title(id).unwrap_or_default(),
                current,
                pl::Size::new(area.x, area.y),
            )
            .map(Some)
            .map_err(LayoutApplyError::Invalid)
        });
        let icons = self.layout_icons();
        let revision = self.layout_revision();
        let settings = crate::config::live(ctx);
        let provider = settings.title_provider;
        let model: String = settings.title_model.trim().chars().take(128).collect();
        // Take a provider slot before anything is dropped, so a refused
        // re-run keeps the candidate it already has.
        #[cfg(not(test))]
        let slot = match &prepared {
            Ok(Some(_)) => match lp::acquire(&lp::IN_FLIGHT, lp::MAX_IN_FLIGHT) {
                Some(slot) => Some(slot),
                None => {
                    if let Some(ed) = &mut self.layout_ui.create {
                        ed.status = Some(Status::Error(
                            "Earlier layout requests are still finishing (a canceled or \
                             superseded request runs until it answers or times out). Try \
                             again in a moment."
                                .into(),
                        ));
                    }
                    return;
                }
            },
            _ => None,
        };
        let Some(ed) = &mut self.layout_ui.create else {
            return;
        };
        // Superseded: dropping the receiver means its reply cannot publish.
        ed.pending = None;
        ed.candidate = None;
        ed.naming = None;
        ed.status = None;
        let inventory = match prepared {
            Ok(Some(inventory)) => inventory,
            Ok(None) => {
                ed.status = Some(Status::Error(
                    "This Project has no windows to arrange. Create arranges the windows that \
                     are open."
                        .into(),
                ));
                return;
            }
            Err(e) => {
                ed.status = Some(Status::Error(e.to_string()));
                return;
            }
        };
        let (tx, rx) = mpsc::sync_channel(1);
        #[cfg(not(test))]
        {
            let slot = slot.expect("acquired above for a non-empty inventory");
            let request = inventory.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _slot = slot;
                let reply = lp::generate(provider, &model, &request, &description);
                let _ = tx.send(reply);
                ctx.request_repaint();
            });
        }
        #[cfg(test)]
        {
            let _ = (provider, model, description);
            ed.test_tx = Some(tx);
        }
        ed.pending = Some(Pending {
            rx,
            revision,
            inventory,
            icons,
            program: provider.label(),
        });
    }

    /// Each frame the editor is open: drop a request or candidate the
    /// windows moved under, then take a finished reply.
    pub(super) fn poll_create(&mut self) {
        let revision = self.layout_revision();
        let Some(ed) = &mut self.layout_ui.create else {
            return;
        };
        if ed.pending.as_ref().is_some_and(|p| p.revision != revision) {
            ed.pending = None;
            ed.status = Some(Status::Stale);
        }
        if ed
            .candidate
            .as_ref()
            .is_some_and(|c| c.revision != revision)
        {
            ed.candidate = None;
            ed.naming = None;
            ed.status = Some(Status::Stale);
        }
        let Some(pending) = &ed.pending else {
            return;
        };
        let reply = match pending.rx.try_recv() {
            Ok(reply) => reply,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("The layout provider stopped unexpectedly.".into())
            }
        };
        let pending = ed.pending.take().expect("checked above");
        match reply {
            Ok(Reply::Candidate {
                message,
                def,
                added,
            }) => {
                if added.is_empty() && changes_nothing(&pending.inventory.current, &def.arrangement)
                {
                    ed.status = Some(Status::Unchanged { message });
                    return;
                }
                let mut entries = pending.inventory.entries;
                entries.extend(added.iter().map(|&item| Entry {
                    item,
                    label: format!("New {}", lp::kind_name(item.kind).to_lowercase()),
                }));
                ed.candidate = Some(Candidate {
                    def,
                    message,
                    revision: pending.revision,
                    added,
                    entries,
                    icons: pending.icons,
                });
            }
            Ok(Reply::Missing { message, kinds }) => {
                ed.status = Some(Status::Missing {
                    text: lp::missing_text(&kinds),
                    message,
                });
            }
            Err(e) => ed.status = Some(Status::Error(e)),
        }
    }

    /// Save & Apply: check the candidate against the live windows, write
    /// it, and only then queue the Apply. A failed write keeps the editor
    /// and the candidate, and moves nothing.
    fn save_and_apply(&mut self, dir: &std::path::Path, ctx: &egui::Context) {
        let Some((def, added, revision)) = self.candidate_in_hand() else {
            return;
        };
        let Some(naming) = self
            .layout_ui
            .create
            .as_ref()
            .and_then(|e| e.naming.clone())
        else {
            return;
        };
        if let Err(e) = self.check_candidate(&def, &added, revision) {
            self.create_failed(e);
            return;
        }
        let on_conflict = if naming.conflict.is_some() {
            OnConflict::Replace
        } else {
            OnConflict::Reject
        };
        let written = self.layout_store().and_then(|store| {
            store.create(
                dir,
                &naming.buf,
                def.items.clone(),
                def.arrangement.clone(),
                on_conflict,
            )
        });
        match written {
            Ok(saved) => {
                self.refresh_layouts();
                self.layout_ui.requested = Some(Requested::Create {
                    saved: Some(saved.name.clone()),
                });
                self.request_layout(LayoutRequest::Candidate {
                    def: saved,
                    added,
                    select: true,
                    revision,
                });
                self.layout_ui.create = None;
                ctx.request_repaint();
            }
            Err(e) => {
                if let Some(n) = self
                    .layout_ui
                    .create
                    .as_mut()
                    .and_then(|e| e.naming.as_mut())
                {
                    match e {
                        StoreError::NameTaken { id, name } => {
                            n.conflict = Some((id, name));
                            n.error = None;
                        }
                        e => n.error = Some(e.to_string()),
                    }
                }
            }
        }
    }

    /// The outcome notice of a Create Apply.
    pub(super) fn create_outcome_notice(
        saved: Option<String>,
        outcome: Result<(), LayoutApplyError>,
        now: f64,
    ) -> Notice {
        match (outcome, saved) {
            (Ok(()), Some(name)) => Notice::Saved {
                text: format!("Saved \u{201c}{name}\u{201d} and applied it."),
                until: now + super::layout_picker::SAVED_NOTICE_SECS,
            },
            (Ok(()), None) => Notice::Saved {
                text: "Applied. It stays Custom until you save it.".into(),
                until: now + super::layout_picker::SAVED_NOTICE_SECS,
            },
            (Err(LayoutApplyError::Stale), Some(name)) => Notice::Error(format!(
                "Saved \u{201c}{name}\u{201d}, but the windows changed before it applied. \
                 Choose it from Layout \u{25be} to apply it."
            )),
            (Err(e), _) => Notice::Error(e.to_string()),
        }
    }

    /// Tab icons of the live items, for the editor's list and preview.
    fn layout_icons(&self) -> HashMap<LayoutItemId, crate::icons::IconKind> {
        self.windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .filter_map(|t| t.content.icon_kind().map(|k| (t.layout_id(), k)))
            .collect()
    }

    /// The editor window. Clicks become `actions`; Escape backs out of the
    /// name row, then cancels. A click elsewhere in the Project does not
    /// close it: a request may still be running.
    pub(super) fn show_create_editor(
        &mut self,
        ui: &mut egui::Ui,
        th: &crate::theme::Theme,
        project: egui::Rect,
        screen: egui::Rect,
        id: egui::Id,
        actions: &mut Vec<PickerAction>,
    ) {
        self.poll_create();
        let provider = crate::config::live(ui.ctx()).title_provider;
        let program = self
            .layout_ui
            .create
            .as_ref()
            .and_then(|e| e.pending.as_ref())
            .map_or(provider.label(), |p| p.program);
        // What would be sent, as the user should see it.
        let listing: Vec<(ItemRef, String, &'static str)> = self
            .layout_area()
            .ok()
            .map(|area| {
                let (items, current) = self.capture_arrangement(area);
                let place: HashMap<LayoutItemId, &'static str> = current
                    .windows
                    .iter()
                    .flat_map(|w| {
                        let tag = match w.state {
                            WindowState::Tiled if w.tabs.len() > 1 => "tab",
                            WindowState::Tiled => "",
                            WindowState::Floating { .. } => "floating",
                            WindowState::Minimized { .. } => "minimized",
                        };
                        w.tabs.iter().map(move |t| (*t, tag))
                    })
                    .collect();
                items
                    .iter()
                    .map(|i| {
                        let label = lp::clean_label(
                            &self.layout_item_title(i.id).unwrap_or_default(),
                            i.kind,
                        );
                        (*i, label, place.get(&i.id).copied().unwrap_or(""))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let live_icons = self.layout_icons();
        let ctx = ui.ctx().clone();
        let Some(ed) = &mut self.layout_ui.create else {
            return;
        };
        let naming_open = ed.naming.is_some();
        let small = egui::FontId::proportional(11.0);
        let field_id = id.with("create-description");
        let name_id = id.with("create-name");
        // Enter submits only from the name field (the description takes
        // newlines); Ctrl+Enter runs only while no name is being typed, so
        // it can never drop a candidate mid-save.
        let name_focused = ctx.memory(|m| m.has_focus(name_id));
        let (enter, escape, run_key) = ui.input_mut(|i| {
            (
                naming_open
                    && name_focused
                    && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                !naming_open && i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter),
            )
        });
        egui::Window::new("Create layout")
            .id(id.with("create"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(project.center())
            .constrain_to(screen)
            .show(&ctx, |dui| {
                dui.set_width(EDITOR_W);
                dui.label(egui::RichText::new("Create layout").strong().color(th.text));
                dui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "Describe how to arrange this Project's windows. Create only \
                             moves the windows that are open: it never opens, closes, or \
                             runs anything.",
                        )
                        .color(th.dim)
                        .size(11.5),
                    )
                    .wrap(),
                );
                dui.add_space(2.0);
                dui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "Sent to {program} (Settings: session-title provider): your \
                             description and the window labels below. No terminal output, \
                             chat, or files."
                        ))
                        .color(th.caret)
                        .size(11.0),
                    )
                    .wrap(),
                );
                dui.add_space(4.0);
                dui.label(
                    egui::RichText::new(format!("Windows in this Project ({})", listing.len()))
                        .color(th.dim)
                        .font(small.clone()),
                );
                egui::Frame::NONE
                    .fill(th.win_bg)
                    .stroke(egui::Stroke::new(1.0, th.border))
                    .corner_radius(egui::CornerRadius::same(3))
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(dui, |lui| {
                        lui.set_width(EDITOR_W - 12.0);
                        egui::ScrollArea::vertical()
                            .id_salt(id.with("create-list"))
                            .max_height(88.0)
                            .auto_shrink([false, true])
                            .show(lui, |lui| {
                                if listing.is_empty() {
                                    lui.label(
                                        egui::RichText::new("No windows are open.")
                                            .color(th.dim)
                                            .font(small.clone()),
                                    );
                                }
                                for (item, label, tag) in &listing {
                                    let (row, _) = lui.allocate_exact_size(
                                        egui::vec2(lui.available_width(), 18.0),
                                        egui::Sense::hover(),
                                    );
                                    let p = lui.painter();
                                    let badge = egui::Rect::from_center_size(
                                        egui::pos2(row.min.x + 8.0, row.center().y),
                                        egui::vec2(14.0, 14.0),
                                    );
                                    paint_kind_badge(p, badge, item.kind, live_icons.get(&item.id), th);
                                    let right = format!(
                                        "{}{}{}",
                                        lp::kind_name(item.kind),
                                        if tag.is_empty() { "" } else { " \u{b7} " },
                                        tag
                                    );
                                    let rg = p.layout_no_wrap(right, small.clone(), th.dim);
                                    let rx = row.max.x - rg.size().x - 2.0;
                                    p.galley(
                                        egui::pos2(rx, row.center().y - rg.size().y / 2.0),
                                        rg,
                                        th.dim,
                                    );
                                    let lg = p.layout_no_wrap(
                                        label.clone(),
                                        egui::FontId::proportional(12.0),
                                        th.text,
                                    );
                                    p.with_clip_rect(egui::Rect::from_min_max(
                                        egui::pos2(badge.max.x + 6.0, row.min.y),
                                        egui::pos2(rx - 8.0, row.max.y),
                                    ))
                                    .galley(
                                        egui::pos2(
                                            badge.max.x + 6.0,
                                            row.center().y - lg.size().y / 2.0,
                                        ),
                                        lg,
                                        th.text,
                                    );
                                }
                            });
                    });
                dui.add_space(6.0);
                let resp = dui.add(
                    egui::TextEdit::multiline(&mut ed.description)
                        .id(field_id)
                        .hint_text(EXAMPLE)
                        .char_limit(lp::MAX_DESCRIPTION_CHARS)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut ed.focus) {
                    resp.request_focus();
                }
                let can_run =
                    !ed.description.trim().is_empty() && !listing.is_empty() && !naming_open;
                dui.horizontal(|dui| {
                    let label = if ed.candidate.is_some() || ed.status.is_some() || ed.busy() {
                        "run again"
                    } else {
                        "run"
                    };
                    let run = dui
                        .add_enabled(
                            can_run,
                            egui::Button::new(egui::RichText::new(label).color(th.caret).strong()),
                        )
                        .on_hover_text("Ctrl+Enter. One request: the new proposal replaces the old one.");
                    if run.clicked() || (run_key && can_run) {
                        actions.push(PickerAction::Create(CreateAction::Run));
                    }
                    if ed.busy() {
                        dui.add(egui::Spinner::new().size(12.0));
                        dui.label(
                            egui::RichText::new(format!("Asking {program}\u{2026}"))
                                .color(th.dim)
                                .font(small.clone()),
                        );
                    }
                });
                match &ed.status {
                    Some(Status::Stale) => {
                        dui.label(
                            egui::RichText::new(LayoutApplyError::Stale.to_string()).color(th.caret),
                        );
                    }
                    Some(Status::Error(e)) => {
                        dui.add(egui::Label::new(egui::RichText::new(e.as_str()).color(th.danger)).wrap());
                    }
                    Some(Status::Unchanged { message }) => {
                        dui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    "That is already how the windows are arranged. \
                                     Describe a change, then run again.",
                                )
                                .color(th.caret),
                            )
                            .wrap(),
                        );
                        dui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("{program}: {message}"))
                                    .color(th.dim)
                                    .size(11.5),
                            )
                            .wrap(),
                        );
                    }
                    Some(Status::Missing { text, message }) => {
                        dui.add(egui::Label::new(egui::RichText::new(text.as_str()).color(th.caret)).wrap());
                        dui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("{program}: {message}"))
                                    .color(th.dim)
                                    .size(11.5),
                            )
                            .wrap(),
                        );
                    }
                    None => {}
                }
                if let Some(c) = &ed.candidate {
                    dui.add_space(4.0);
                    dui.label(egui::RichText::new("Proposal").color(th.dim).font(small.clone()));
                    dui.add(
                        egui::Label::new(egui::RichText::new(c.message.as_str()).color(th.text).size(11.5))
                            .wrap(),
                    );
                    let (rect, _) = dui.allocate_exact_size(
                        egui::vec2(EDITOR_W, PREVIEW_H),
                        egui::Sense::hover(),
                    );
                    paint_candidate(dui.painter(), c, rect, th);
                    if !c.added.is_empty() {
                        let names: Vec<&str> =
                            c.added.iter().map(|a| lp::kind_name(a.kind)).collect();
                        dui.add(
                            egui::Label::new(
                                egui::RichText::new(format!(
                                    "Apply opens {} new: {}",
                                    if c.added.len() == 1 { "one window".to_string() } else { format!("{} windows", c.added.len()) },
                                    names.join(", ")
                                ))
                                .color(th.caret)
                                .size(11.0),
                            )
                            .wrap(),
                        );
                    }
                    let minimized = minimized_labels(c);
                    if !minimized.is_empty() {
                        dui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("Minimized: {}", minimized.join(", ")))
                                    .color(th.dim)
                                    .size(11.0),
                            )
                            .wrap(),
                        );
                    }
                }
                dui.add_space(6.0);
                if let Some(naming) = &mut ed.naming {
                    dui.label(
                        egui::RichText::new("Name the layout. It is saved for this Project, then applied.")
                            .color(th.dim)
                            .size(11.5),
                    );
                    let resp = dui.add(
                        egui::TextEdit::singleline(&mut naming.buf)
                            .id(name_id)
                            .hint_text("Layout name")
                            .char_limit(200)
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut naming.focus) {
                        resp.request_focus();
                    }
                    if resp.changed() {
                        actions.push(PickerAction::Create(CreateAction::EditName));
                    }
                    if let Some((_, name)) = &naming.conflict {
                        dui.add(
                            egui::Label::new(
                                egui::RichText::new(format!(
                                    "A layout named \u{201c}{name}\u{201d} already exists in this \
                                     Project. Replace it, or type another name."
                                ))
                                .color(th.caret),
                            )
                            .wrap(),
                        );
                    }
                    if let Some(e) = &naming.error {
                        dui.add(egui::Label::new(egui::RichText::new(e.as_str()).color(th.danger)).wrap());
                    }
                    let primary = if naming.conflict.is_some() {
                        "replace & apply"
                    } else {
                        "save & apply"
                    };
                    dui.horizontal(|dui| {
                        if dui.button("back").clicked() {
                            actions.push(PickerAction::Create(CreateAction::BackFromName));
                        }
                        if dui
                            .button(egui::RichText::new(primary).color(th.caret).strong())
                            .clicked()
                        {
                            actions.push(PickerAction::Create(CreateAction::SubmitName));
                        }
                    });
                } else {
                    let has = ed.candidate.is_some();
                    dui.horizontal(|dui| {
                        if dui.button("cancel").clicked() {
                            actions.push(PickerAction::Create(CreateAction::Cancel));
                        }
                        if dui
                            .add_enabled(has, egui::Button::new(egui::RichText::new("apply").color(th.caret).strong()))
                            .on_hover_text("Arrange the windows like this. The picker shows Custom until you save.")
                            .clicked()
                        {
                            actions.push(PickerAction::Create(CreateAction::Apply));
                        }
                        if dui
                            .add_enabled(has, egui::Button::new("save & apply\u{2026}"))
                            .on_hover_text("Name it, save it for this Project, then apply it.")
                            .clicked()
                        {
                            actions.push(PickerAction::Create(CreateAction::SaveAndApply));
                        }
                    });
                }
            });
        if actions.is_empty() {
            if escape {
                actions.push(PickerAction::Create(if naming_open {
                    CreateAction::BackFromName
                } else {
                    CreateAction::Cancel
                }));
            } else if enter {
                actions.push(PickerAction::Create(CreateAction::SubmitName));
            }
        }
        if self
            .layout_ui
            .create
            .as_ref()
            .is_some_and(CreateEditor::busy)
        {
            // The reply wakes the frame too; this is the spinner's cadence.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
impl WindowManager {
    /// Answer the pending Create request with provider text, parsed against
    /// the inventory that request sent, as the request thread would.
    pub(super) fn answer_create_for_test(&mut self, raw: &serde_json::Value) {
        let ed = self.layout_ui.create.as_mut().expect("editor open");
        let inv = &ed.pending.as_ref().expect("a pending request").inventory;
        let reply = lp::parse_reply(&raw.to_string(), inv);
        ed.test_tx
            .take()
            .expect("request sender")
            .send(reply)
            .unwrap();
    }
}

/// Whether a proposal would leave the windows as they are. Stricter than
/// `same_arrangement`, which ignores the front tab for Modified: "show the
/// chat first" is a real change.
fn changes_nothing(current: &pl::Arrangement, proposed: &pl::Arrangement) -> bool {
    let fronts = |a: &pl::Arrangement| {
        let mut ids: Vec<LayoutItemId> = a
            .windows
            .iter()
            .filter_map(|w| w.tabs.get(w.active).copied())
            .collect();
        ids.sort_unstable();
        ids
    };
    pl::same_arrangement(current, proposed) && fronts(current) == fronts(proposed)
}

/// The labels of the candidate's minimized items, shown under the diagram.
fn minimized_labels(c: &Candidate) -> Vec<String> {
    c.def
        .arrangement
        .windows
        .iter()
        .filter(|w| matches!(w.state, WindowState::Minimized { .. }))
        .flat_map(|w| w.tabs.iter())
        .map(|id| label_of(c, *id))
        .collect()
}

fn label_of(c: &Candidate, id: LayoutItemId) -> String {
    c.entries
        .iter()
        .find(|e| e.item.id == id)
        .map_or_else(String::new, |e| e.label.clone())
}

/// An agent/shell/folder icon when the tab has one, else the kind's letter
/// in a small badge.
fn paint_kind_badge(
    p: &egui::Painter,
    rect: egui::Rect,
    kind: pl::ItemKind,
    icon: Option<&crate::icons::IconKind>,
    th: &crate::theme::Theme,
) {
    if let Some(icon) = icon {
        let px = (rect.width() * p.ctx().pixels_per_point()).round().max(1.0) as u32;
        let tex = crate::icons::texture(p.ctx(), *icon, px);
        p.image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            icon.tint(),
        );
        return;
    }
    p.rect_filled(rect, egui::CornerRadius::same(3), th.sel_bg);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        kind_glyph(kind),
        egui::FontId::proportional(9.0),
        th.text,
    );
}

/// The candidate as a diagram with each window's icon and label: tiled
/// windows from the tree, floating ones on top. Arrangement data and the
/// labels that were sent only; nothing live is rendered.
fn paint_candidate(p: &egui::Painter, c: &Candidate, rect: egui::Rect, th: &crate::theme::Theme) {
    p.rect_filled(rect, egui::CornerRadius::same(3), th.desk_bg);
    p.rect_stroke(
        rect,
        egui::CornerRadius::same(3),
        egui::Stroke::new(1.0, th.border),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(4.0);
    for cell in super::layout_picker::diagram_cells(&c.def, inner) {
        let cell_rect = cell.rect.shrink(1.5);
        let Some(w) = c.def.arrangement.windows.get(cell.window) else {
            continue;
        };
        let active = w.tabs.get(w.active).or(w.tabs.first()).copied();
        if cell.floating {
            p.rect_filled(
                cell_rect.translate(egui::vec2(2.0, 2.0)),
                egui::CornerRadius::same(3),
                egui::Color32::from_black_alpha(90),
            );
        }
        let new = w.tabs.iter().any(|t| c.added.iter().any(|a| a.id == *t));
        p.rect_filled(cell_rect, egui::CornerRadius::same(3), th.win_bg);
        p.rect_stroke(
            cell_rect,
            egui::CornerRadius::same(3),
            match (new, cell.floating) {
                (true, _) => egui::Stroke::new(1.5, th.caret),
                (false, true) => egui::Stroke::new(1.0, th.text),
                (false, false) => egui::Stroke::new(1.0, th.border),
            },
            egui::StrokeKind::Inside,
        );
        let Some(active) = active else {
            continue;
        };
        if cell_rect.width() < 18.0 || cell_rect.height() < 14.0 {
            continue;
        }
        let kind = c
            .entries
            .iter()
            .find(|e| e.item.id == active)
            .map_or(pl::ItemKind::Terminal, |e| e.item.kind);
        let badge =
            egui::Rect::from_min_size(cell_rect.min + egui::vec2(4.0, 4.0), egui::vec2(12.0, 12.0));
        paint_kind_badge(p, badge, kind, c.icons.get(&active), th);
        let clip = egui::Rect::from_min_max(
            egui::pos2(badge.max.x + 4.0, cell_rect.min.y),
            egui::pos2(cell_rect.max.x - 3.0, cell_rect.max.y),
        );
        let label = p.layout_no_wrap(
            label_of(c, active),
            egui::FontId::proportional(11.0),
            th.text,
        );
        p.with_clip_rect(clip).galley(
            egui::pos2(clip.min.x, badge.center().y - label.size().y / 2.0),
            label,
            th.text,
        );
        if w.tabs.len() > 1 && cell_rect.height() >= 30.0 {
            let others: Vec<String> = w
                .tabs
                .iter()
                .filter(|t| **t != active)
                .map(|t| label_of(c, *t))
                .collect();
            let more = p.layout_no_wrap(
                format!("+ {}", others.join(", ")),
                egui::FontId::proportional(10.0),
                th.dim,
            );
            p.with_clip_rect(egui::Rect::from_min_max(
                egui::pos2(cell_rect.min.x + 4.0, cell_rect.min.y),
                egui::pos2(cell_rect.max.x - 3.0, cell_rect.max.y),
            ))
            .galley(
                egui::pos2(cell_rect.min.x + 6.0, badge.max.y + 3.0),
                more,
                th.dim,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_library::LayoutStore;
    use crate::project_layout::{Arrangement, ItemKind, LayoutSelection};

    const AREA: egui::Vec2 = egui::vec2(1000.0, 800.0);

    struct Fixture {
        _tmp: tempfile::TempDir,
        config: std::path::PathBuf,
        repo: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        Fixture {
            _tmp: tmp,
            config,
            repo,
        }
    }

    impl Fixture {
        fn store(&self) -> LayoutStore {
            LayoutStore::at(&self.config)
        }

        fn project(&self) -> WindowManager {
            let mut wm = WindowManager::new();
            wm.cwd = Some(self.repo.clone());
            wm.tag = Some("p1".into());
            wm.last_area = AREA;
            wm.layout_ui.store = Some(self.store());
            wm
        }
    }

    /// A cheap layout item: an empty nested manager (kind Project).
    fn stub() -> Content {
        Content::Project(Box::new(WindowManager::new()))
    }

    fn tile(wm: &mut WindowManager, title: &str) -> LayoutItemId {
        let anchor = wm.focused;
        let (id, rect) = wm.next_slot(egui::vec2(400.0, 300.0));
        let tab = Tab::fixed(title, stub());
        let item = tab.layout_id();
        wm.push_win(id, tab, rect);
        wm.tile_new(id, anchor);
        wm.focused = Some(id);
        item
    }

    fn act(wm: &mut WindowManager, action: CreateAction) {
        let ctx = egui::Context::default();
        wm.run_layout_action(PickerAction::Create(action), &ctx);
    }

    fn editor(wm: &WindowManager) -> &CreateEditor {
        wm.layout_ui.create.as_ref().expect("the editor is open")
    }

    fn open_and_run(
        wm: &mut WindowManager,
        description: &str,
    ) -> mpsc::SyncSender<Result<Reply, String>> {
        if wm.layout_ui.create.is_none() {
            let ctx = egui::Context::default();
            wm.run_layout_action(PickerAction::OpenCreate, &ctx);
        }
        wm.layout_ui.create.as_mut().unwrap().description = description.into();
        act(wm, CreateAction::Run);
        wm.layout_ui
            .create
            .as_mut()
            .unwrap()
            .test_tx
            .take()
            .expect("a request started")
    }

    fn arrangement(wm: &WindowManager) -> Arrangement {
        wm.capture_arrangement(wm.last_area).1
    }

    fn identities(wm: &WindowManager) -> Vec<LayoutItemId> {
        let mut ids: Vec<_> = wm
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter().map(|t| t.layout_id()))
            .collect();
        ids.sort();
        ids
    }

    /// What a provider would propose for three items: all in one stack,
    /// in reverse order, built against the editor's own pending inventory.
    fn stacked_reply(wm: &WindowManager) -> Reply {
        let inv = &editor(wm).pending.as_ref().expect("pending").inventory;
        let n = inv.entries.len();
        let tabs: Vec<usize> = (1..=n).rev().collect();
        let raw = serde_json::json!({
            "message": "Everything in one stack.",
            "windows": [{"tabs": tabs, "place": "tiled"}],
            "tree": {"window": 0}
        });
        lp::parse_reply(&raw.to_string(), inv).unwrap()
    }

    fn three(f: &Fixture) -> (WindowManager, [LayoutItemId; 3]) {
        let mut wm = f.project();
        let a = tile(&mut wm, "alpha");
        let b = tile(&mut wm, "beta");
        let c = tile(&mut wm, "gamma");
        wm.watch_layout_edits(); // first frame: measure the baseline
        (wm, [a, b, c])
    }

    #[test]
    fn a_reply_becomes_one_candidate_and_nothing_moves_until_apply() {
        let f = fixture();
        let (mut wm, [a, b, c]) = three(&f);
        let before = arrangement(&wm);
        let tx = open_and_run(&mut wm, "one stack");
        assert!(editor(&wm).busy());
        let inv = &editor(&wm).pending.as_ref().unwrap().inventory;
        let labels: Vec<&str> = inv.entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["alpha", "beta", "gamma"],
            "labels are what is sent"
        );
        tx.send(Ok(stacked_reply(&wm))).unwrap();
        wm.poll_create();
        let ed = editor(&wm);
        assert!(!ed.busy());
        let cand = ed.candidate.as_ref().expect("candidate");
        assert_eq!(cand.def.arrangement.windows[0].tabs, vec![c, b, a]);
        assert_eq!(
            arrangement(&wm),
            before,
            "live windows do not move until Apply"
        );
    }

    #[test]
    fn running_again_supersedes_the_pending_request_and_replaces_the_candidate() {
        let f = fixture();
        let (mut wm, _) = three(&f);
        let first = open_and_run(&mut wm, "one stack");
        let reply = stacked_reply(&wm);
        let second = open_and_run(&mut wm, "one stack, please");
        // The first request's reply has nowhere to land.
        assert!(
            first.send(Ok(reply.clone())).is_err(),
            "superseded receiver dropped"
        );
        wm.poll_create();
        assert!(editor(&wm).busy(), "still waiting on the second");
        second.send(Ok(reply)).unwrap();
        wm.poll_create();
        assert!(editor(&wm).candidate.is_some());
        // Running again drops the candidate at once.
        let _third = open_and_run(&mut wm, "something else");
        assert!(editor(&wm).candidate.is_none());
        assert!(editor(&wm).busy());
    }

    #[test]
    fn a_structural_change_discards_the_request_and_the_candidate_but_a_resize_does_not() {
        let f = fixture();
        let (mut wm, [a, ..]) = three(&f);
        // Pending request, then a structural edit: the reply never publishes.
        let tx = open_and_run(&mut wm, "one stack");
        let reply = stacked_reply(&wm);
        wm.minimize(win_id_of(&wm, a));
        wm.watch_layout_edits();
        let _ = tx.send(Ok(reply));
        wm.poll_create();
        assert_eq!(editor(&wm).status, Some(Status::Stale));
        assert!(editor(&wm).candidate.is_none() && !editor(&wm).busy());
        assert_eq!(
            LayoutApplyError::Stale.to_string(),
            "Windows changed \u{2014} run again."
        );

        // A candidate, then a resize: it survives.
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Ok(stacked_reply(&wm))).unwrap();
        wm.poll_create();
        assert!(editor(&wm).candidate.is_some());
        wm.last_area = egui::vec2(700.0, 500.0);
        wm.watch_layout_edits();
        wm.poll_create();
        assert!(editor(&wm).candidate.is_some(), "resize alone keeps it");
        assert_eq!(editor(&wm).status, None);

        // Then a structural edit (closing Content counts): discarded.
        let pid = win_id_of(&wm, a);
        wm.close(pid);
        wm.watch_layout_edits();
        wm.poll_create();
        assert!(editor(&wm).candidate.is_none());
        assert_eq!(editor(&wm).status, Some(Status::Stale));
    }

    #[test]
    fn apply_moves_existing_content_only_and_leaves_the_picker_on_custom() {
        let f = fixture();
        let (mut wm, [a, b, c]) = three(&f);
        wm.set_layout_selection(LayoutSelection {
            selected: Some("old".into()),
            modified: true,
        });
        let ids = identities(&wm);
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Ok(stacked_reply(&wm))).unwrap();
        wm.poll_create();
        act(&mut wm, CreateAction::Apply);
        assert!(wm.layout_ui.create.is_none(), "the editor closes");
        let ctx = egui::Context::default();
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert!(matches!(wm.layout_ui.notice, Some(Notice::Saved { .. })));
        assert_eq!(identities(&wm), ids, "nothing opened or closed");
        let now = arrangement(&wm);
        assert_eq!(now.windows.len(), 1);
        assert_eq!(now.windows[0].tabs, vec![c, b, a]);
        assert_eq!(wm.layout, LayoutSelection::default(), "Custom until saved");
        assert_eq!(
            wm.layout_revert_status(),
            super::super::layouts::RevertStatus::Ready
        );
        // Revert brings the three tiles back.
        wm.revert_layout().unwrap();
        assert_eq!(arrangement(&wm).windows.len(), 3);
        assert_eq!(identities(&wm), ids);
        assert!(
            f.store().project_layouts(&f.repo).unwrap().is_empty(),
            "Apply saves nothing"
        );
    }

    #[test]
    fn a_candidate_that_went_stale_before_its_frame_does_not_apply() {
        let f = fixture();
        let (mut wm, [a, ..]) = three(&f);
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Ok(stacked_reply(&wm))).unwrap();
        wm.poll_create();
        act(&mut wm, CreateAction::Apply);
        // A structural edit lands between the click and the next frame.
        wm.minimize(win_id_of(&wm, a));
        wm.watch_layout_edits();
        let before = arrangement(&wm);
        let ctx = egui::Context::default();
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert_eq!(arrangement(&wm), before, "nothing moved");
        assert_eq!(
            wm.layout_ui.notice,
            Some(Notice::Error("Windows changed \u{2014} run again.".into()))
        );
        assert_eq!(
            wm.layout_revert_status(),
            super::super::layouts::RevertStatus::Unavailable
        );
    }

    #[test]
    fn strict_apply_refuses_anything_but_an_exact_id_match() {
        let f = fixture();
        let (mut wm, [a, b, _c]) = three(&f);
        let rev = wm.layout_revision();
        let ctx = egui::Context::default();
        let before = arrangement(&wm);
        // A definition that omits one live item would set it aside.
        let def = LayoutDefinition {
            id: String::new(),
            name: String::new(),
            items: vec![
                ItemRef::new(a, ItemKind::Project),
                ItemRef::new(b, ItemKind::Project),
            ],
            arrangement: Arrangement {
                windows: vec![pl::WindowSpec::tiled(vec![a, b], 0)],
                tree: Some(pl::TreeSpec::Leaf { window: 0 }),
            },
        };
        assert_eq!(
            wm.check_candidate(&def, &[], rev),
            Err(LayoutApplyError::Stale)
        );
        assert_eq!(
            wm.apply_candidate(&def, &[], false, rev, &ctx).unwrap_err(),
            LayoutApplyError::Stale
        );
        // A definition with a slot no live item fills would open one.
        let mut extra = def.clone();
        extra.items.push(ItemRef::new(9999, ItemKind::Terminal));
        extra.arrangement.windows[0].tabs.push(9999);
        assert_eq!(
            wm.apply_candidate(&extra, &[], false, rev, &ctx)
                .unwrap_err(),
            LayoutApplyError::Stale
        );
        // The wrong revision is stale however good the definition.
        let (items, current) = wm.capture_arrangement(AREA);
        let same = LayoutDefinition {
            id: String::new(),
            name: String::new(),
            items,
            arrangement: current,
        };
        assert_eq!(
            wm.apply_candidate(&same, &[], false, rev + 1, &ctx)
                .unwrap_err(),
            LayoutApplyError::Stale
        );
        assert_eq!(arrangement(&wm), before, "nothing moved");
        assert_eq!(
            wm.windows.iter().map(|w| w.tabs.len()).sum::<usize>(),
            3,
            "nothing opened"
        );
        assert!(wm.apply_candidate(&same, &[], false, rev, &ctx).is_ok());
    }

    #[test]
    fn save_and_apply_persists_before_it_moves_anything() {
        let f = fixture();
        let (mut wm, [a, b, c]) = three(&f);
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Ok(stacked_reply(&wm))).unwrap();
        wm.poll_create();
        act(&mut wm, CreateAction::SaveAndApply);
        wm.layout_ui
            .create
            .as_mut()
            .unwrap()
            .naming
            .as_mut()
            .unwrap()
            .buf = "  Stacked ".into();
        act(&mut wm, CreateAction::SubmitName);
        assert!(wm.layout_ui.create.is_none());
        let saved = f.store().project_layouts(&f.repo).unwrap();
        assert_eq!(saved.len(), 1, "written before the Apply ran");
        assert_eq!(saved[0].name, "Stacked");
        assert_eq!(
            arrangement(&wm).windows.len(),
            3,
            "not moved yet: the Apply is deferred"
        );
        let ctx = egui::Context::default();
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert_eq!(arrangement(&wm).windows[0].tabs, vec![c, b, a]);
        assert_eq!(
            wm.layout,
            LayoutSelection {
                selected: Some(saved[0].id.clone()),
                modified: false
            },
            "Save & Apply selects the saved definition"
        );
        assert_eq!(
            wm.layout_ui
                .notice
                .as_ref()
                .map(|n| matches!(n, Notice::Saved { text, .. } if text.contains("Stacked"))),
            Some(true)
        );
    }

    #[test]
    fn a_failed_or_conflicting_save_keeps_the_candidate_and_moves_nothing() {
        let f = fixture();
        let (mut wm, _) = three(&f);
        // Another layout already holds the name.
        let (items, current) = wm.capture_arrangement(AREA);
        f.store()
            .create(&f.repo, "Stacked", items, current, OnConflict::Reject)
            .unwrap();
        let before = arrangement(&wm);
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Ok(stacked_reply(&wm))).unwrap();
        wm.poll_create();
        act(&mut wm, CreateAction::SaveAndApply);
        wm.layout_ui
            .create
            .as_mut()
            .unwrap()
            .naming
            .as_mut()
            .unwrap()
            .buf = "stacked".into();
        act(&mut wm, CreateAction::SubmitName);
        let ed = editor(&wm);
        assert!(ed.candidate.is_some(), "candidate kept");
        assert!(
            ed.naming.as_ref().unwrap().conflict.is_some(),
            "offers Replace"
        );
        assert!(!wm.has_pending_layout_request(), "no Apply queued");
        // An empty name is rejected by the store: kept again.
        act(&mut wm, CreateAction::EditName);
        wm.layout_ui
            .create
            .as_mut()
            .unwrap()
            .naming
            .as_mut()
            .unwrap()
            .buf = "   ".into();
        act(&mut wm, CreateAction::SubmitName);
        let ed = editor(&wm);
        assert!(ed.candidate.is_some());
        assert!(ed.naming.as_ref().unwrap().error.is_some());
        // A store that cannot be written (a future-version file).
        std::fs::write(
            f.config.join("project-layouts.json"),
            r#"{"version": 999, "projects": {}}"#,
        )
        .unwrap();
        wm.layout_ui
            .create
            .as_mut()
            .unwrap()
            .naming
            .as_mut()
            .unwrap()
            .buf = "Fresh".into();
        act(&mut wm, CreateAction::SubmitName);
        let ed = editor(&wm);
        assert!(ed.candidate.is_some());
        assert!(
            ed.naming.as_ref().unwrap().error.is_some(),
            "{:?}",
            ed.naming
        );
        assert!(!wm.has_pending_layout_request());
        assert_eq!(arrangement(&wm), before);
        // Back keeps the candidate; Cancel drops the editor and changes nothing.
        act(&mut wm, CreateAction::BackFromName);
        assert!(editor(&wm).naming.is_none() && editor(&wm).candidate.is_some());
        act(&mut wm, CreateAction::Cancel);
        assert!(wm.layout_ui.create.is_none());
        assert_eq!(arrangement(&wm), before);
    }

    #[test]
    fn a_save_whose_windows_changed_first_writes_nothing() {
        let f = fixture();
        let (mut wm, [a, ..]) = three(&f);
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Ok(stacked_reply(&wm))).unwrap();
        wm.poll_create();
        act(&mut wm, CreateAction::SaveAndApply);
        wm.layout_ui
            .create
            .as_mut()
            .unwrap()
            .naming
            .as_mut()
            .unwrap()
            .buf = "Stacked".into();
        // The revision moves without a frame in between.
        wm.minimize(win_id_of(&wm, a));
        wm.watch_layout_edits();
        act(&mut wm, CreateAction::SubmitName);
        assert!(
            f.store().project_layouts(&f.repo).unwrap().is_empty(),
            "validated before the write"
        );
        assert_eq!(editor(&wm).status, Some(Status::Stale));
    }

    #[test]
    fn provider_errors_missing_content_and_a_dead_worker_leave_windows_alone() {
        let f = fixture();
        let (mut wm, _) = three(&f);
        let before = arrangement(&wm);
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Err(lp::launch_error_text(
            "claude",
            crate::ai_oneshot::LaunchError::Unavailable,
        )))
        .unwrap();
        wm.poll_create();
        assert!(matches!(&editor(&wm).status, Some(Status::Error(e)) if e.contains("unavailable")));
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Err(lp::launch_error_text(
            "claude",
            crate::ai_oneshot::LaunchError::Timeout,
        )))
        .unwrap();
        wm.poll_create();
        assert!(matches!(&editor(&wm).status, Some(Status::Error(e)) if e.contains("too long")));
        let tx = open_and_run(&mut wm, "chat on the right");
        tx.send(Ok(Reply::Missing {
            message: "Open a chat first.".into(),
            kinds: vec![ItemKind::Chat],
        }))
        .unwrap();
        wm.poll_create();
        assert!(matches!(
            &editor(&wm).status,
            Some(Status::Missing { text, .. }) if text.contains("Chat")
        ));
        assert!(editor(&wm).candidate.is_none(), "nothing to apply");
        let tx = open_and_run(&mut wm, "one stack");
        drop(tx);
        wm.poll_create();
        assert!(matches!(&editor(&wm).status, Some(Status::Error(_))));
        act(&mut wm, CreateAction::Apply);
        assert!(
            !wm.has_pending_layout_request(),
            "no candidate: nothing queued"
        );
        assert_eq!(arrangement(&wm), before);
        assert_eq!(wm.windows.iter().map(|w| w.tabs.len()).sum::<usize>(), 3);
    }

    #[test]
    fn closing_the_editor_with_its_project_cancels_the_request_for_good() {
        let f = fixture();
        let (mut wm, _) = three(&f);
        let tx = open_and_run(&mut wm, "one stack");
        let reply = stacked_reply(&wm);
        assert!(
            wm.layout_ui.captures_keyboard(),
            "the editor owns the keyboard"
        );
        // Switching away (focus leaves, or the Project is not drawn).
        wm.close_layout_popups();
        assert!(wm.layout_ui.create.is_none());
        assert!(tx.send(Ok(reply)).is_err(), "the reply has nowhere to go");
        assert!(!wm.layout_ui.captures_keyboard());
        // Reopening starts empty: nothing resumes.
        let ctx = egui::Context::default();
        wm.run_layout_action(PickerAction::OpenCreate, &ctx);
        let ed = editor(&wm);
        assert!(ed.description.is_empty() && ed.candidate.is_none() && !ed.busy());
    }

    #[test]
    fn an_empty_project_or_description_sends_nothing() {
        let f = fixture();
        let mut wm = f.project();
        let ctx = egui::Context::default();
        wm.run_layout_action(PickerAction::OpenCreate, &ctx);
        act(&mut wm, CreateAction::Run);
        assert!(editor(&wm).test_tx.is_none(), "blank description");
        wm.layout_ui.create.as_mut().unwrap().description = "anything".into();
        act(&mut wm, CreateAction::Run);
        assert!(editor(&wm).test_tx.is_none());
        assert!(matches!(&editor(&wm).status, Some(Status::Error(e)) if e.contains("no windows")));
    }

    /// Draw `desk` in a `w`-wide area, as one real frame.
    fn desk_frame(desk: &mut WindowManager, ctx: &egui::Context, w: f32) {
        let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 600.0));
        let input = egui::RawInput {
            screen_rect: Some(area),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            desk.show(ui, area, true, egui::Id::new("create-narrow"), false);
        });
    }

    #[test]
    fn a_header_too_narrow_for_the_controls_closes_the_editor() {
        let f = fixture();
        let (inner, _) = three(&f);
        let ctx = egui::Context::default();
        let mut desk = WindowManager::new().as_desktop();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        desk.push_win(
            1,
            Tab::fixed("proj", Content::Project(Box::new(inner))),
            rect,
        );
        desk.focused = Some(1);
        desk_frame(&mut desk, &ctx, 1000.0);
        child(&mut desk).run_layout_action(PickerAction::OpenCreate, &ctx);
        desk_frame(&mut desk, &ctx, 1000.0);
        assert!(
            child(&mut desk).layout_ui.create.is_some(),
            "drawn: stays open"
        );
        // Narrower than the controls: no header can draw the editor, so it
        // must not linger, invisible, with the terminals frozen.
        if let Some(w) = desk.windows.iter_mut().find(|w| w.id == 1) {
            w.rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(120.0, 600.0));
        }
        desk_frame(&mut desk, &ctx, 1000.0);
        assert!(child(&mut desk).layout_ui.create.is_none());
        assert!(!child(&mut desk).layout_ui.captures_keyboard());
    }

    #[test]
    fn header_save_closes_the_editor_so_its_dialog_can_draw() {
        let f = fixture();
        let (mut wm, _) = three(&f);
        let _tx = open_and_run(&mut wm, "one stack");
        let ctx = egui::Context::default();
        wm.run_layout_action(PickerAction::Save, &ctx);
        assert!(wm.layout_ui.create.is_none(), "one popup at a time");
        assert!(
            wm.layout_ui.dialog.is_some(),
            "Save's name dialog is the one shown"
        );
        let _tx = open_and_run(&mut wm, "one stack");
        wm.run_layout_action(PickerAction::SaveAs, &ctx);
        assert!(wm.layout_ui.create.is_none());
    }

    #[test]
    fn strict_apply_refuses_a_candidate_that_leaves_out_a_minimized_item() {
        let f = fixture();
        let (mut wm, [a, b, c]) = three(&f);
        wm.minimize(win_id_of(&wm, c));
        wm.watch_layout_edits();
        let rev = wm.layout_revision();
        let def = LayoutDefinition {
            id: String::new(),
            name: String::new(),
            items: vec![
                ItemRef::new(a, ItemKind::Project),
                ItemRef::new(b, ItemKind::Project),
            ],
            arrangement: Arrangement {
                windows: vec![pl::WindowSpec::tiled(vec![a, b], 0)],
                tree: Some(pl::TreeSpec::Leaf { window: 0 }),
            },
        };
        assert_eq!(
            wm.check_candidate(&def, &[], rev),
            Err(LayoutApplyError::Stale)
        );
    }

    /// The three live stubs stacked on the left, a new Board and a new Chat
    /// tiled to their right.
    fn reply_with_new(wm: &WindowManager) -> Reply {
        let inv = &editor(wm).pending.as_ref().expect("pending").inventory;
        let raw = serde_json::json!({
            "message": "Stack left, board and chat right.",
            "new": [{"id": 4, "kind": "board"}, {"id": 5, "kind": "chat"}],
            "windows": [
                {"tabs": [1, 2, 3], "place": "tiled"},
                {"tabs": [4], "place": "tiled"},
                {"tabs": [5], "place": "tiled"}
            ],
            "tree": {"split": "row", "sizes": [2, 1, 1], "children": [
                {"window": 0}, {"window": 1}, {"window": 2}
            ]}
        });
        lp::parse_reply(&raw.to_string(), inv).unwrap()
    }

    fn kind_of(wm: &WindowManager, id: LayoutItemId) -> ItemKind {
        wm.layout_inventory()
            .iter()
            .find(|i| i.id == id)
            .expect("live")
            .kind
    }

    #[test]
    fn new_windows_show_in_the_preview_and_open_only_on_apply() {
        let f = fixture();
        let (mut wm, [a, b, c]) = three(&f);
        let ids = identities(&wm);
        let tx = open_and_run(&mut wm, "like Rider");
        tx.send(Ok(reply_with_new(&wm))).unwrap();
        wm.poll_create();
        let cand = editor(&wm).candidate.as_ref().expect("a candidate");
        let added: Vec<ItemKind> = cand.added.iter().map(|x| x.kind).collect();
        assert_eq!(added, vec![ItemKind::Board, ItemKind::Chat]);
        let labels: Vec<String> = cand.added.iter().map(|x| label_of(cand, x.id)).collect();
        assert_eq!(
            labels,
            vec!["New board", "New chat"],
            "the preview names them"
        );
        assert_eq!(identities(&wm), ids, "a reply opens nothing");
        act(&mut wm, CreateAction::Apply);
        assert_eq!(
            identities(&wm),
            ids,
            "nor does the click: the Apply is deferred"
        );
        let ctx = egui::Context::default();
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert!(
            matches!(wm.layout_ui.notice, Some(Notice::Saved { .. })),
            "{:?}",
            wm.layout_ui.notice
        );
        assert_eq!(wm.layout_inventory().len(), 5, "both new windows opened");
        let now = arrangement(&wm);
        assert_eq!(now.windows.len(), 3);
        assert_eq!(now.windows[0].tabs, vec![a, b, c]);
        assert_eq!(kind_of(&wm, now.windows[1].tabs[0]), ItemKind::Board);
        assert_eq!(kind_of(&wm, now.windows[2].tabs[0]), ItemKind::Chat);
        assert_eq!(wm.layout, LayoutSelection::default(), "Custom until saved");
        // Revert restores the old arrangement; what opened stays open.
        wm.revert_layout().unwrap();
        assert_eq!(wm.layout_inventory().len(), 5);
    }

    #[test]
    fn save_and_apply_saves_the_new_windows_and_opens_them() {
        let f = fixture();
        let (mut wm, _) = three(&f);
        let tx = open_and_run(&mut wm, "like Rider");
        tx.send(Ok(reply_with_new(&wm))).unwrap();
        wm.poll_create();
        act(&mut wm, CreateAction::SaveAndApply);
        wm.layout_ui
            .create
            .as_mut()
            .unwrap()
            .naming
            .as_mut()
            .unwrap()
            .buf = "Rider".into();
        act(&mut wm, CreateAction::SubmitName);
        let saved = f.store().project_layouts(&f.repo).unwrap();
        assert_eq!(saved.len(), 1);
        let kinds: Vec<ItemKind> = saved[0].items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds.iter().filter(|k| **k == ItemKind::Board).count(), 1);
        assert_eq!(kinds.iter().filter(|k| **k == ItemKind::Chat).count(), 1);
        assert_eq!(wm.layout_inventory().len(), 3, "saved first, opened later");
        let ctx = egui::Context::default();
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert_eq!(wm.layout_inventory().len(), 5);
        assert_eq!(
            wm.layout,
            LayoutSelection {
                selected: Some(saved[0].id.clone()),
                modified: false
            },
            "every slot filled: the saved layout, not Modified"
        );
    }

    #[test]
    fn a_new_window_that_fails_to_open_is_reported() {
        let f = fixture();
        let (mut wm, _) = three(&f);
        let tx = open_and_run(&mut wm, "like Rider");
        tx.send(Ok(reply_with_new(&wm))).unwrap();
        wm.poll_create();
        wm.layout_rt.fail_opens = true;
        act(&mut wm, CreateAction::Apply);
        let ctx = egui::Context::default();
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert_eq!(wm.layout_inventory().len(), 3, "nothing opened");
        assert!(
            matches!(&wm.layout_ui.notice, Some(Notice::Summary { title, .. }) if title == "Applied with changes"),
            "{:?}",
            wm.layout_ui.notice
        );
    }

    #[test]
    fn strict_apply_opens_exactly_the_candidates_new_windows() {
        let f = fixture();
        let (wm, [.., c]) = three(&f);
        let rev = wm.layout_revision();
        let (mut items, current) = wm.capture_arrangement(AREA);
        let fresh = c + 100;
        items.push(ItemRef::new(fresh, ItemKind::Board));
        let mut arrangement = current.clone();
        arrangement.windows[0].tabs.push(fresh);
        let def = LayoutDefinition {
            id: String::new(),
            name: String::new(),
            items,
            arrangement,
        };
        assert_eq!(wm.check_candidate(&def, &[fresh], rev), Ok(()));
        // The same slot not declared new: refused.
        assert_eq!(
            wm.check_candidate(&def, &[], rev),
            Err(LayoutApplyError::Stale)
        );
        // A declared window that no slot opens: refused.
        assert_eq!(
            wm.check_candidate(&def, &[fresh, fresh + 1], rev),
            Err(LayoutApplyError::Stale)
        );
        // An item the candidate placed that has since closed is never
        // reopened, even with the new window declared.
        let mut gone = def.clone();
        let closed = c + 50;
        gone.items.push(ItemRef::new(closed, ItemKind::Terminal));
        gone.arrangement.windows[0].tabs.push(closed);
        assert_eq!(
            wm.check_candidate(&gone, &[fresh], rev),
            Err(LayoutApplyError::Stale)
        );
    }

    #[test]
    fn a_proposal_that_only_changes_the_front_tab_is_offered() {
        let f = fixture();
        let (mut wm, [a, b, c]) = three(&f);
        // Put all three in one stack, c in front.
        let tx = open_and_run(&mut wm, "one stack");
        tx.send(Ok(stacked_reply(&wm))).unwrap();
        wm.poll_create();
        act(&mut wm, CreateAction::Apply);
        let ctx = egui::Context::default();
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert_eq!(arrangement(&wm).windows[0].tabs, vec![c, b, a]);
        // Same stack, b in front: a real change.
        let tx = open_and_run(&mut wm, "show beta first");
        let inv = editor(&wm).pending.as_ref().unwrap().inventory.clone();
        let mut def = LayoutDefinition {
            id: String::new(),
            name: String::new(),
            items: inv.items(),
            arrangement: inv.current.clone(),
        };
        def.arrangement.windows[0].active = 1;
        tx.send(Ok(Reply::Candidate {
            message: "Beta in front.".into(),
            def,
            added: Vec::new(),
        }))
        .unwrap();
        wm.poll_create();
        assert!(editor(&wm).candidate.is_some(), "{:?}", editor(&wm).status);
    }

    #[test]
    fn a_proposal_that_changes_nothing_is_not_offered() {
        let f = fixture();
        let (mut wm, _) = three(&f);
        let tx = open_and_run(&mut wm, "like Rider");
        let inv = editor(&wm).pending.as_ref().unwrap().inventory.clone();
        let def = LayoutDefinition {
            id: String::new(),
            name: String::new(),
            items: inv.items(),
            arrangement: inv.current.clone(),
        };
        tx.send(Ok(Reply::Candidate {
            message: "Kept it.".into(),
            def,
            added: Vec::new(),
        }))
        .unwrap();
        wm.poll_create();
        assert!(editor(&wm).candidate.is_none(), "nothing to apply or save");
        assert_eq!(
            editor(&wm).status,
            Some(Status::Unchanged {
                message: "Kept it.".into()
            })
        );
    }

    fn child(desk: &mut WindowManager) -> &mut WindowManager {
        desk.project_child_mut(1).expect("project")
    }

    fn win_id_of(wm: &WindowManager, item: LayoutItemId) -> WinId {
        wm.windows
            .iter()
            .find(|w| w.tabs.iter().any(|t| t.layout_id() == item))
            .unwrap()
            .id
    }
}
