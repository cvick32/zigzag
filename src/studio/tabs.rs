//! Document tabs: several files open at once, one strip of tabs over the
//! canvas, as in VectorCraft and Illustrator.
//!
//! The document in front lives in `Studio`'s own fields (`doc`, `design`,
//! `undo`, `view`…), so everything else in the studio works on it unchanged.
//! The others are parked in [`Tab`]s. Switching swaps the two: the one in
//! front goes into its slot and the chosen one comes out of its own, leaving
//! a small husk in the slot of whichever is in front.
//!
//! Closing a tab with unsaved changes asks first. Quitting doesn't need to:
//! every tab comes back next time, unsaved changes and all (see `session.rs`).

use crate::app::{Snap, Studio};
use crate::canvas::View;
use crate::icons::{self, Icon};
use crate::theme::pal_ui;
use eframe::egui::{self, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};
use std::collections::HashSet;
use std::path::Path;
use zigzag::checks::Issue;
use zigzag::design::{Design, PlanCache};
use zigzag::document::Document;

/// The height of the tab strip: a panel header's, so the strip and the
/// docks' first headers line up.
pub const STRIP_H: f32 = crate::dock::HEADER_H;
/// A tab's width bounds.
const TAB_MIN: f32 = 96.0;
const TAB_MAX: f32 = 220.0;
/// The » and + buttons.
const BUTTON_W: f32 = 30.0;

/// Whether a tab's × comes before its title (macOS) or after it.
const CLOSE_BEFORE_TITLE: bool = cfg!(target_os = "macos");

/// A document that isn't in front: everything that belongs to it.
pub struct Tab {
    pub(crate) doc: Document,
    stable: Document,
    design: Design,
    issues: Vec<Issue>,
    names: Vec<String>,
    dirty: bool,
    plan_ms: f64,
    pub(crate) modified: bool,
    pub(crate) selected: Option<usize>,
    multi: Vec<usize>,
    isolated: Vec<String>,
    expanded: HashSet<String>,
    density_report: Option<zigzag::document::DensityReport>,
    pub(crate) view: View,
    fit_pending: bool,
    undo: Vec<Document>,
    redo: Vec<Document>,
    player: Option<usize>,
    plan_cache: PlanCache,
    /// The document as last written to its recovery file.
    pub(crate) saved: Option<(u64, Snap)>,
    /// Which recovery file is its (see `session::recovery_file`).
    pub(crate) rec: u32,
}

impl Tab {
    fn blank(rec: u32) -> Tab {
        let doc = Document::empty();
        Tab {
            stable: doc.clone(),
            doc,
            design: Design::default(),
            issues: Vec::new(),
            names: Vec::new(),
            dirty: true,
            plan_ms: 0.0,
            modified: false,
            selected: None,
            multi: Vec::new(),
            isolated: Vec::new(),
            expanded: HashSet::new(),
            density_report: None,
            view: View::default(),
            fit_pending: true,
            undo: Vec::new(),
            redo: Vec::new(),
            player: None,
            plan_cache: PlanCache::default(),
            saved: None,
            rec,
        }
    }

    /// The selected object's index.
    pub(crate) fn sel_index(&self) -> Option<usize> {
        self.selected
            .and_then(|uid| self.doc.shapes.iter().position(|s| s.uid == uid))
    }
}

/// Something done to the tabs from the strip, applied at the start of the
/// next frame (not halfway through one, where the frame's edit tracking
/// would take the switch for an edit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabOp {
    Switch(usize),
    Close(usize),
    CloseOthers(usize),
    New,
    Move { from: usize, to: usize },
}

pub struct Tabs {
    /// One per open document, in order. The one at `active` is a husk:
    /// that document is in `Studio`'s own fields.
    pub(crate) list: Vec<Tab>,
    pub(crate) active: usize,
    /// The next recovery file number.
    pub(crate) next_rec: u32,
    pub(crate) pending: Option<TabOp>,
    /// Asking whether to save the tab in front before closing it.
    pub(crate) asking: bool,
    /// A tab being dragged to another place: (its index, the pointer's x
    /// offset from the tab's left edge).
    drag: Option<(usize, f32)>,
}

impl Default for Tabs {
    fn default() -> Self {
        Tabs {
            list: vec![Tab::blank(0)],
            active: 0,
            next_rec: 1,
            pending: None,
            asking: false,
            drag: None,
        }
    }
}

/// What the strip shows for one tab.
#[derive(Clone, Debug)]
pub struct TabInfo {
    pub name: String,
    pub modified: bool,
    pub path: Option<std::path::PathBuf>,
}

fn display_name(doc: &Document) -> String {
    if doc.name.is_empty() {
        "Untitled".into()
    } else {
        doc.name.clone()
    }
}

impl Studio {
    pub fn tab_count(&self) -> usize {
        self.tabs.list.len()
    }

    pub fn active_tab(&self) -> usize {
        self.tabs.active
    }

    /// Every tab's name, unsaved state and file, in order.
    pub fn tab_infos(&self) -> Vec<TabInfo> {
        (0..self.tab_count())
            .map(|i| {
                if i == self.tabs.active {
                    TabInfo {
                        name: display_name(&self.doc),
                        modified: self.modified,
                        path: self.doc.path.clone(),
                    }
                } else {
                    let t = &self.tabs.list[i];
                    TabInfo {
                        name: display_name(&t.doc),
                        modified: t.modified,
                        path: t.doc.path.clone(),
                    }
                }
            })
            .collect()
    }

    /// The tab with `path` open, if one has it.
    pub fn tab_with_path(&self, path: &Path) -> Option<usize> {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let want = canon(path);
        self.tab_infos().iter().position(|t| {
            t.path
                .as_deref()
                .is_some_and(|p| p == path || canon(p) == want)
        })
    }

    /// The tab in front has nothing in it worth keeping: no file, no
    /// objects, no history.
    pub fn tab_is_blank(&self) -> bool {
        self.doc.path.is_none()
            && !self.modified
            && self.doc.shapes.is_empty()
            && self.undo.is_empty()
    }

    /// Swap the document in front with tab `i`'s.
    fn swap_live(&mut self, i: usize) {
        use std::mem::swap;
        let t = &mut self.tabs.list[i];
        swap(&mut self.doc, &mut t.doc);
        swap(&mut self.stable, &mut t.stable);
        swap(&mut self.design, &mut t.design);
        swap(&mut self.issues, &mut t.issues);
        swap(&mut self.names, &mut t.names);
        swap(&mut self.dirty, &mut t.dirty);
        swap(&mut self.plan_ms, &mut t.plan_ms);
        swap(&mut self.modified, &mut t.modified);
        swap(&mut self.selected, &mut t.selected);
        swap(&mut self.layout.multi, &mut t.multi);
        swap(&mut self.layout.isolated, &mut t.isolated);
        swap(&mut self.layout.expanded, &mut t.expanded);
        swap(&mut self.layout.density_report, &mut t.density_report);
        swap(&mut self.view, &mut t.view);
        swap(&mut self.fit_pending, &mut t.fit_pending);
        swap(&mut self.undo, &mut t.undo);
        swap(&mut self.redo, &mut t.redo);
        swap(&mut self.player, &mut t.player);
        swap(&mut self.plan_cache, &mut t.plan_cache);
        self.session.swap_doc(&mut t.saved, &mut t.rec);
    }

    /// Before the document in front goes away: finish what's under way on
    /// it (typing, a preview, a drag) so nothing carries over to the next.
    fn leave_tab(&mut self) {
        self.finish_typing();
        self.cancel_previews();
        self.layout.offset_dialog = None;
        self.layout.cleanup_dialog = None;
        self.layout.swatch_dialog = None;
        self.shapes.dialog = None;
        self.lettering.edit = None;
        self.picker = None;
        self.drag = None;
        self.panning = false;
        self.gesture = Default::default();
        self.path_edit = Default::default();
        self.playing = false;
        self.pending_history = None;
        if self.editing {
            // A slider drag in progress: what it did so far is the edit.
            self.stable = self.doc.clone();
            self.editing = false;
        }
        // A plan still out on the worker is for this document: plan again
        // when it's back in front.
        if self.planner.pending {
            self.dirty = true;
        }
        self.planner.superseded();
    }

    /// After a document comes to the front.
    fn enter_tab(&mut self) {
        self.hovered = None;
        // A different plan on screen: what's kept by plan number (bounds,
        // density, the stitch meshes) is out of date.
        self.replans += 1;
        *self.satin_memo.borrow_mut() = None;
        self.agent.work.clear();
    }

    /// Bring tab `j` to the front.
    pub fn switch_tab(&mut self, j: usize) {
        if j == self.tabs.active || j >= self.tab_count() {
            return;
        }
        self.leave_tab();
        let a = self.tabs.active;
        self.swap_live(a);
        self.swap_live(j);
        self.tabs.active = j;
        self.enter_tab();
    }

    /// Open a new, empty tab after the one in front and bring it forward.
    pub fn new_tab(&mut self) {
        self.leave_tab();
        let a = self.tabs.active;
        self.swap_live(a);
        // What came out of the slot is the husk: replace it with the new,
        // empty document, and give the new slot a husk of its own.
        let rec = self.tabs.next_rec;
        self.tabs.next_rec += 1;
        let mut fresh = Tab::blank(rec);
        let at = a + 1;
        self.tabs.list.insert(at, Tab::blank(0));
        self.tabs.active = at;
        // (Swapping with `fresh` puts the empty document in front.)
        std::mem::swap(&mut self.tabs.list[at], &mut fresh);
        self.swap_live(at);
        self.enter_tab();
    }

    /// Before opening a document: use the tab in front if it's blank, else
    /// open a new one.
    pub fn make_room_for_document(&mut self) {
        if !self.tab_is_blank() {
            self.new_tab();
        }
    }

    /// Open `doc`: in a new tab (or the blank one in front). A file that's
    /// already open comes to the front instead, keeping its changes; the
    /// one in front is read again from disk only if it has none. False if
    /// `doc` wasn't used.
    pub fn open_document(&mut self, doc: Document) -> bool {
        if let Some(i) = doc.path.as_deref().and_then(|p| self.tab_with_path(p)) {
            if i != self.tabs.active {
                self.switch_tab(i);
                return false;
            }
            if self.modified {
                return false;
            }
        } else {
            self.make_room_for_document();
        }
        self.set_document(doc);
        true
    }

    /// Close tab `i`, asking first if it has unsaved changes.
    pub fn close_tab(&mut self, i: usize) {
        if i >= self.tab_count() {
            return;
        }
        let modified = if i == self.tabs.active {
            self.modified
        } else {
            self.tabs.list[i].modified
        };
        if modified {
            // Ask with it in front, so it's clear what's being closed.
            self.switch_tab(i);
            self.tabs.asking = true;
        } else {
            self.discard_tab(i);
        }
    }

    /// Close tab `i` without asking. The last tab closed leaves an empty
    /// document.
    pub fn discard_tab(&mut self, i: usize) {
        if i >= self.tab_count() {
            return;
        }
        let rec = if i == self.tabs.active {
            self.session.rec
        } else {
            self.tabs.list[i].rec
        };
        if self.tab_count() == 1 {
            self.leave_tab();
            self.set_document(Document::empty());
            self.session.forget_recovery(rec);
            self.status = "Closed.".into();
            return;
        }
        let name = self.tab_infos()[i].name.clone();
        if i == self.tabs.active {
            // The next tab to the right comes forward (the left one at the end).
            let next = if i + 1 < self.tab_count() {
                i + 1
            } else {
                i - 1
            };
            self.switch_tab(next);
        }
        self.tabs.list.remove(i);
        if self.tabs.active > i {
            self.tabs.active -= 1;
        }
        self.session.forget_recovery(rec);
        self.status = format!("Closed {name}.");
    }

    /// Move tab `from` to `to` (both in the order as it is now).
    pub fn move_tab(&mut self, from: usize, to: usize) {
        let n = self.tab_count();
        if from >= n || to >= n || from == to {
            return;
        }
        let t = self.tabs.list.remove(from);
        self.tabs.list.insert(to, t);
        let a = self.tabs.active;
        self.tabs.active = if a == from {
            to
        } else if from < a && to >= a {
            a - 1
        } else if from > a && to <= a {
            a + 1
        } else {
            a
        };
    }

    /// Apply what the strip asked for last frame. Called at the start of a
    /// frame, with the keyboard's tab shortcuts.
    pub fn apply_tab_op(&mut self, ctx: &egui::Context) {
        // ⌃Tab / ⌃⇧Tab: the next and previous tab (as in Illustrator).
        let (next, prev) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::CTRL, Key::Tab),
                i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab),
            )
        });
        let n = self.tab_count();
        if self.pending_tab_op_none() && n > 1 {
            if next {
                self.tabs.pending = Some(TabOp::Switch((self.tabs.active + 1) % n));
            } else if prev {
                self.tabs.pending = Some(TabOp::Switch((self.tabs.active + n - 1) % n));
            }
        }
        let Some(op) = self.tabs.pending.take() else {
            return;
        };
        match op {
            TabOp::Switch(i) => self.switch_tab(i),
            TabOp::Close(i) => self.close_tab(i),
            TabOp::CloseOthers(i) => {
                // Unsaved ones stay (their changes are worth a question each).
                self.switch_tab(i);
                let mut k = self.tab_count();
                while k > 0 {
                    k -= 1;
                    if k != self.tabs.active && !self.tabs.list[k].modified {
                        self.discard_tab(k);
                    }
                }
                let left = self.tab_count() - 1;
                if left > 0 {
                    self.status = format!(
                        "Kept {left} {} with unsaved changes open.",
                        if left == 1 { "tab" } else { "tabs" }
                    );
                }
            }
            TabOp::New => {
                self.new_tab();
                self.status = "New document.".into();
            }
            TabOp::Move { from, to } => self.move_tab(from, to),
        }
        ctx.request_repaint();
    }

    fn pending_tab_op_none(&self) -> bool {
        self.tabs.pending.is_none()
    }

    /// The tab strip over the canvas.
    pub fn doc_tabs(&mut self, ui: &mut egui::Ui) {
        let p = pal_ui(ui);
        let strip = ui.max_rect();
        let painter = ui.painter().clone();
        painter.rect_filled(strip, 0.0, p.dock);
        let infos = self.tab_infos();
        let active = self.tabs.active;
        let font = egui::FontId::proportional(12.0);
        // Each tab sized to its name, within bounds.
        let galleys: Vec<_> = infos
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let color = if i == active { p.text } else { p.secondary };
                painter.layout_no_wrap(t.name.clone(), font.clone(), color)
            })
            .collect();
        let widths: Vec<f32> = galleys
            .iter()
            .map(|g| (g.size().x + 46.0).clamp(TAB_MIN, TAB_MAX))
            .collect();
        let room = strip.width() - BUTTON_W;
        let overflow = widths.iter().sum::<f32>() > room;
        let room = room - if overflow { BUTTON_W } else { 0.0 };
        let first = if overflow {
            first_tab_shown(&widths, active, room)
        } else {
            0
        };
        // The strip's bottom edge, broken under the tab in front.
        let base = strip.bottom() - 0.5;
        let mut x = strip.left();
        let mut shown: Vec<(usize, Rect)> = Vec::new();
        let mut op = None;
        let pointer = ui.input(|i| i.pointer.interact_pos());
        for (i, w) in widths.iter().enumerate().skip(first) {
            if x + w > strip.left() + room && i != active && !shown.is_empty() {
                break;
            }
            let r = Rect::from_min_size(Pos2::new(x, strip.top()), Vec2::new(*w, STRIP_H));
            shown.push((i, r));
            x += w;
        }
        let dragging = self.tabs.drag;
        for &(i, r) in &shown {
            let is_active = i == active;
            let id = ui.scope_id().with(("doc-tab", i));
            let resp = ui.interact(r, id, Sense::click_and_drag());
            let name = &infos[i].name;
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Tab, true, is_active, format!("{name} tab"))
            });
            // The tab being dragged is drawn under the pointer.
            let draw = match (dragging, pointer) {
                (Some((d, grab)), Some(pt)) if d == i => r.translate(Vec2::new(
                    (pt.x - grab - r.left())
                        .clamp(strip.left() - r.left(), strip.left() + room - r.right()),
                    0.0,
                )),
                _ => r,
            };
            if is_active {
                painter.rect_filled(draw, 0.0, p.desk);
            } else if resp.hovered() {
                painter.rect_filled(draw, 0.0, p.hover);
            }
            painter.line_segment(
                [draw.right_top(), draw.right_bottom()],
                Stroke::new(1.0, p.separator),
            );
            if !is_active {
                painter.line_segment(
                    [Pos2::new(draw.left(), base), Pos2::new(draw.right(), base)],
                    Stroke::new(1.0, p.separator),
                );
            }
            let (x_center, title_left) = if CLOSE_BEFORE_TITLE {
                (draw.left() + 15.0, draw.left() + 28.0)
            } else {
                (draw.right() - 15.0, draw.left() + 12.0)
            };
            let xr =
                Rect::from_center_size(Pos2::new(x_center, draw.center().y), Vec2::splat(16.0));
            let xresp = ui.interact(xr, ui.scope_id().with(("doc-tab-x", i)), Sense::click());
            let xresp = xresp.on_hover_text("Close Tab");
            xresp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Close {name}"))
            });
            // The × shows on the tab in front and under the pointer; an
            // unsaved tab shows a dot there otherwise (as macOS does).
            let hot = resp.hovered() || xresp.hovered() || is_active;
            if xresp.hovered() {
                painter.rect_filled(xr, 4.0, p.pressed);
            }
            if infos[i].modified && !(resp.hovered() || xresp.hovered()) {
                painter.circle_filled(
                    xr.center(),
                    3.5,
                    if is_active { p.text } else { p.secondary },
                );
            } else if hot {
                icons::paint(
                    &painter,
                    xr.shrink(3.0),
                    Icon::Close,
                    if xresp.hovered() { p.text } else { p.secondary },
                );
            }
            // The name, cut short with an ellipsis if the tab is too narrow.
            let title_w = if CLOSE_BEFORE_TITLE {
                draw.right() - 10.0 - title_left
            } else {
                xr.left() - 4.0 - title_left
            };
            let g = if galleys[i].size().x > title_w {
                let mut job = egui::text::LayoutJob::simple_singleline(
                    name.clone(),
                    font.clone(),
                    if is_active { p.text } else { p.secondary },
                );
                job.wrap = egui::text::TextWrapping::truncate_at_width(title_w);
                painter.layout_job(job)
            } else {
                galleys[i].clone()
            };
            painter.galley(
                Pos2::new(title_left, draw.center().y - g.size().y / 2.0),
                g,
                p.text,
            );
            let resp = match &infos[i].path {
                Some(path) => resp.on_hover_text(path.display().to_string()),
                None => resp,
            };
            if xresp.clicked() || resp.middle_clicked() {
                op = Some(TabOp::Close(i));
            } else if (resp.clicked() || resp.drag_started()) && !is_active {
                op = Some(TabOp::Switch(i));
            }
            if resp.drag_started()
                && let Some(pt) = pointer
            {
                self.tabs.drag = Some((i, pt.x - r.left()));
            }
            resp.context_menu(|ui| {
                if ui.button("Close Tab").clicked() {
                    op = Some(TabOp::Close(i));
                    ui.close();
                }
                if ui
                    .add_enabled(infos.len() > 1, egui::Button::new("Close Other Tabs"))
                    .clicked()
                {
                    op = Some(TabOp::CloseOthers(i));
                    ui.close();
                }
                if let Some(path) = &infos[i].path {
                    ui.separator();
                    if ui.button("Copy Path").clicked() {
                        ui.ctx().copy_text(path.display().to_string());
                        ui.close();
                    }
                }
            });
        }
        // Dragging a tab: dropped where the pointer is.
        if let Some((d, _)) = self.tabs.drag {
            let released = ui.input(|i| !i.pointer.primary_down());
            if released {
                self.tabs.drag = None;
                if let Some(pt) = pointer {
                    let to = shown
                        .iter()
                        .rev()
                        .find(|(_, r)| pt.x >= r.left())
                        .map(|(i, _)| *i)
                        .or(shown.first().map(|(i, _)| *i))
                        .unwrap_or(d);
                    if to != d {
                        op = Some(TabOp::Move { from: d, to });
                    }
                }
            } else {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
        }
        // The rest of the strip's bottom edge, after the tabs.
        let end = shown.last().map(|(_, r)| r.right()).unwrap_or(strip.left());
        painter.line_segment(
            [Pos2::new(end, base), Pos2::new(strip.right(), base)],
            Stroke::new(1.0, p.separator),
        );
        // + : a new document.
        let plus = Rect::from_min_size(Pos2::new(end, strip.top()), Vec2::new(BUTTON_W, STRIP_H))
            .shrink2(Vec2::new(3.0, 4.0));
        let presp = ui
            .interact(plus, ui.scope_id().with("doc-tab-new"), Sense::click())
            .on_hover_text("New Document (⌘N)");
        presp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "New Tab"));
        if presp.hovered() {
            painter.rect_filled(plus, 5.0, p.hover);
        }
        icons::paint(
            &painter,
            Rect::from_center_size(plus.center(), Vec2::splat(14.0)),
            Icon::Plus,
            p.secondary,
        );
        if presp.clicked() {
            op = Some(TabOp::New);
        }
        // » : every open document, when they don't all fit.
        if overflow {
            let more = Rect::from_min_max(
                Pos2::new(strip.right() - BUTTON_W, strip.top()),
                Pos2::new(strip.right(), strip.bottom()),
            )
            .shrink2(Vec2::new(3.0, 4.0));
            let mresp = ui
                .interact(more, ui.scope_id().with("doc-tabs-more"), Sense::click())
                .on_hover_text("Show All Open Documents");
            mresp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "All open documents")
            });
            if mresp.hovered() {
                painter.rect_filled(more, 5.0, p.hover);
            }
            icons::paint(
                &painter,
                Rect::from_center_size(more.center(), Vec2::splat(14.0)),
                Icon::ChevronsRight,
                p.secondary,
            );
            egui::Popup::menu(&mresp).show(|ui| {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        for (i, t) in infos.iter().enumerate() {
                            let label = format!("{}{}", if t.modified { "• " } else { "" }, t.name);
                            if ui.selectable_label(i == active, label).clicked() {
                                op = Some(TabOp::Switch(i));
                                ui.close();
                            }
                        }
                    });
            });
        }
        if op.is_some() {
            self.tabs.pending = op;
            ui.ctx().request_repaint();
        }
    }

    /// "Save changes to … before closing?" for the tab in front.
    pub fn close_prompt(&mut self, ctx: &egui::Context) {
        if !self.tabs.asking {
            return;
        }
        let name = display_name(&self.doc);
        let mut choice = None;
        use crate::dialog;
        let modal = dialog::show(
            ctx,
            egui::Id::unique("close-tab-prompt"),
            dialog::Place::Center,
            |ui| {
                dialog::heading(
                    ui,
                    &format!("Do you want to save the changes you made to “{name}”?"),
                );
                dialog::body(ui, "Your changes will be lost if you don't save them.");
                dialog::buttons(ui, |ui| {
                    if dialog::primary(ui, "Save", true).clicked() {
                        choice = Some(Close::Save);
                    }
                    if dialog::secondary(ui, "Cancel").clicked() {
                        choice = Some(Close::Cancel);
                    }
                    dialog::apart(ui);
                    if dialog::secondary(ui, "Don't Save").clicked() {
                        choice = Some(Close::Discard);
                    }
                });
            },
        );
        if modal.should_close() {
            choice = choice.or(Some(Close::Cancel));
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            choice = Some(Close::Save);
        }
        match choice {
            Some(Close::Cancel) => self.tabs.asking = false,
            Some(Close::Discard) => {
                self.tabs.asking = false;
                self.discard_tab(self.tabs.active);
            }
            Some(Close::Save) => {
                self.tabs.asking = false;
                self.run_action(crate::commands::Action::Save);
                // (Save As… canceled: it stays open.)
                if !self.modified {
                    self.discard_tab(self.tabs.active);
                }
            }
            None => {}
        }
    }
}

enum Close {
    Save,
    Discard,
    Cancel,
}

/// The first of tabs `widths` wide that the strip shows in `room`: the first
/// one, or a later one so that the `active` tab fits.
fn first_tab_shown(widths: &[f32], active: usize, room: f32) -> usize {
    let mut first = 0;
    while first < active && widths[first..=active].iter().sum::<f32>() > room {
        first += 1;
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::path::PathBuf;

    fn example(name: &str) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/")).join(name)
    }

    fn svg_file(dir: &str, name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tl-tabs-{dir}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let f = d.join(name);
        std::fs::write(
            &f,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="40mm" viewBox="0 0 40 40"><rect id="a" x="2" y="2" width="20" height="20" fill="#c0392b"/><circle id="b" cx="30" cy="30" r="6" fill="#2c3e50"/></svg>"##,
        )
        .unwrap();
        f
    }

    #[test]
    fn opening_files_opens_tabs_and_each_keeps_its_own_state() {
        let a = svg_file("own", "a.svg");
        let b = svg_file("own", "b.svg");
        let mut s = Studio::with_file(Some(a.clone()), None);
        assert_eq!(s.tab_count(), 1);
        // Edit A: delete an object, zoom in.
        s.structural(|d| d.delete(0)).unwrap();
        s.view.scale = 4.5;
        s.select_index(Some(0));
        let a_shapes = s.doc.shapes.len();

        s.load(&b);
        assert_eq!(s.tab_count(), 2, "B in a new tab");
        assert_eq!(s.active_tab(), 1);
        assert_eq!(s.doc.name, "b");
        assert!(!s.modified);
        assert!(s.undo.is_empty(), "B's own history");
        assert_eq!(s.sel_index(), None);

        s.switch_tab(0);
        assert_eq!(s.doc.name, "a");
        assert_eq!(s.doc.shapes.len(), a_shapes);
        assert!(s.modified, "A's changes are still there");
        assert_eq!(s.view.scale, 4.5);
        assert_eq!(s.sel_index(), Some(0));
        assert!(s.apply_history(crate::app::History::Undo), "and its undo");
        assert_eq!(s.doc.shapes.len(), a_shapes + 1);

        // Opening A again brings its tab forward; nothing new opens.
        s.switch_tab(1);
        s.load(&a);
        assert_eq!(s.tab_count(), 2);
        assert_eq!(s.active_tab(), 0);
        assert!(s.modified, "kept, not read again from disk");
    }

    #[test]
    fn a_blank_tab_is_used_for_the_next_file() {
        let a = svg_file("blank", "a.svg");
        let mut s = Studio::with_file(None, None);
        assert!(s.tab_is_blank());
        s.load(&a);
        assert_eq!(s.tab_count(), 1, "the empty document makes way");
        s.new_tab();
        assert_eq!(s.tab_count(), 2);
        assert!(s.tab_is_blank());
        assert_eq!(s.doc.shapes.len(), 0);
    }

    #[test]
    fn closing_asks_only_about_unsaved_changes() {
        let a = svg_file("close", "a.svg");
        let b = svg_file("close", "b.svg");
        let mut s = Studio::with_file(Some(a), None);
        s.load(&b);
        s.load(&example("badge.svg"));
        assert_eq!(s.tab_count(), 3);
        // Unchanged: closes at once; the tab to its right comes forward
        // (or the left one, at the end).
        s.close_tab(2);
        assert_eq!(s.tab_count(), 2);
        assert_eq!(s.doc.name, "b");
        // Unsaved: brought forward and asked about.
        s.switch_tab(0);
        s.structural(|d| d.delete(0)).unwrap();
        s.switch_tab(1);
        s.close_tab(0);
        assert_eq!(s.tab_count(), 2, "not closed yet");
        assert!(s.tabs.asking);
        assert_eq!(s.doc.name, "a", "the one asked about is in front");
        s.tabs.asking = false;
        s.discard_tab(0);
        assert_eq!(s.tab_count(), 1);
        assert_eq!(s.doc.name, "b");
        // The last tab closed leaves an empty document.
        s.close_tab(0);
        assert_eq!(s.tab_count(), 1);
        assert!(s.tab_is_blank());
    }

    #[test]
    fn tabs_move_and_the_one_in_front_follows() {
        let mut s = Studio::with_file(Some(example("badge.svg")), None);
        for n in ["a.svg", "b.svg", "c.svg"] {
            s.load(&svg_file("move", n));
        }
        // badge, a, b, c — c in front.
        s.switch_tab(1);
        s.move_tab(1, 3);
        let names: Vec<String> = s.tab_infos().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["badge", "b", "c", "a"]);
        assert_eq!(s.active_tab(), 3);
        assert_eq!(s.doc.name, "a");
        s.move_tab(0, 2);
        let names: Vec<String> = s.tab_infos().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["b", "c", "badge", "a"]);
        assert_eq!(s.doc.name, "a");
        s.switch_tab(2);
        assert_eq!(s.doc.name, "badge");
    }

    #[test]
    fn switching_shows_the_other_design_without_counting_as_an_edit() {
        let a = svg_file("edit", "a.svg");
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 900.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(Some(example("badge.svg")), None),
            );
        h.run_steps(3);
        let badge = h.state().design.stitch_count();
        h.state_mut().load(&a);
        h.run_steps(3);
        assert_ne!(h.state().design.stitch_count(), badge);
        // Click the badge's tab.
        h.get_by_label("badge tab").click();
        h.run_steps(3);
        assert_eq!(h.state().doc.name, "badge");
        assert_eq!(h.state().design.stitch_count(), badge, "its own stitches");
        assert!(!h.state().modified, "a switch isn't an edit");
        assert!(h.state().undo.is_empty());
        // ⌃Tab: the next one.
        h.key_press_modifiers(Modifiers::CTRL, Key::Tab);
        h.run_steps(3);
        assert_eq!(h.state().doc.name, "a");
        // Its ×.
        h.get_by_label("Close a").click();
        h.run_steps(3);
        assert_eq!(h.state().tab_count(), 1);
        assert_eq!(h.state().doc.name, "badge");
        // + opens an empty one.
        h.get_by_label("New Tab").click();
        h.run_steps(3);
        assert_eq!(h.state().tab_count(), 2);
        assert!(h.state().tab_is_blank());
    }
}
