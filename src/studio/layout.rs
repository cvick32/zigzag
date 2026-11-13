//! Object layout in the studio: multi-select, drag to move, scale/rotate
//! handles, numeric position and size, mirror, duplicate, copy/paste, nudge.
//! Every finished gesture is one undo step written into the SVG.
//!
//! The gestures follow Illustrator: corner handles scale freely and Shift
//! keeps proportions, Option scales from the center, Shift constrains moves
//! to 45°, Option-drag moves a copy, just outside a corner rotates, smart
//! guides snap to other objects and the hoop, and ⌘D repeats the last
//! transform (Transform Again).

use crate::app::{Studio, shape_bounds};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Stroke, Vec2};
use zigzag::document::{Document, Fragment};
use zigzag::geom::{
    Affine, Bounds, IDENTITY, P, affine_mul, rotate_about, scale_about, translation,
};
use zigzag::svg::Geometry;

/// Grab handles on the selection box. Scale handles carry which sides move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    /// (-1, 0, 1) per axis: the side being dragged.
    Scale(i8, i8),
    Rotate,
}

#[derive(Clone, Debug)]
pub enum DragMode {
    Move,
    Scale { handle: (i8, i8), bounds: Bounds },
    Rotate { center: P },
}

/// A canvas gesture in progress, previewed live and committed on release.
pub struct Drag {
    pub mode: DragMode,
    /// Pointer position where the drag started (design units).
    pub start: P,
    /// The document before the gesture.
    pub base: Document,
    pub indices: Vec<usize>,
    pub m: Affine,
    /// The selection's bounds when the gesture started.
    pub bounds: Bounds,
    /// What smart guides snap to: other objects' bounds, and the hoop.
    pub targets: Vec<Bounds>,
    /// Smart guide lines to draw (design units).
    pub guides: Vec<(P, P)>,
    /// Option is held during a move: release drops a copy.
    pub copy: bool,
}

/// Modifier keys during a canvas gesture.
#[derive(Clone, Copy, Debug, Default)]
pub struct DragMods {
    /// Keep proportions (scale), 45° steps (move, rotate).
    pub shift: bool,
    /// Scale from the center; move a copy.
    pub alt: bool,
    /// Snap to smart guides.
    pub snap: bool,
    /// Snap distance (design units).
    pub tol: f64,
}

/// The last transform, for Transform Again (⌘D).
#[derive(Clone, Debug)]
pub struct LastTransform {
    pub m: Affine,
    /// Selection bounds it was applied to; replays follow the selection.
    pub from: Bounds,
    /// It made a copy (Option-drag, Duplicate).
    pub copy: bool,
}

/// Paste offset so a duplicate doesn't land exactly on its original (units).
pub const DUPLICATE_OFFSET: f64 = 30.0;

/// Room left on each side when fitting artwork to the hoop (mm).
pub const HOOP_MARGIN_MM: f64 = 2.0;

#[derive(Default, Clone)]
pub struct LayoutState {
    pub multi: Vec<usize>,
    pub clipboard: Vec<Fragment>,
    /// A copy's SVG, waiting to go on the system clipboard (it needs the
    /// egui context, so it's written at the next frame).
    pub system_clipboard: Option<String>,
    /// Numeric fields being edited: x, y, w, h (mm).
    pub fields: Option<[f64; 4]>,
    pub lock_aspect: bool,
    pub keep_stitch_density: bool,
    pub density_report: Option<zigzag::document::DensityReport>,
    pub rotate_by: f64,
    /// Pastes since the last copy, to cascade them.
    pub pastes: usize,
    /// Groups shown open in the Objects panel.
    pub expanded: std::collections::HashSet<String>,
    /// Groups entered by double-clicking (Illustrator's isolation mode),
    /// outermost first. Clicks select inside the innermost one.
    pub isolated: Vec<String>,
    /// For Transform Again.
    pub last: Option<LastTransform>,
    /// The Offset Path window, when open: the distance (mm).
    pub offset_dialog: Option<f64>,
    /// The Round Corners window, when open.
    pub corner_dialog: Option<CornerDialog>,
    /// The Simplify window, when open.
    pub simplify_dialog: Option<crate::path_tools::SimplifyDialog>,
    /// The Clean Up window, when open.
    pub cleanup_dialog: Option<crate::path_tools::CleanUpDialog>,
    /// The Test Swatches window, when open.
    pub swatch_dialog: Option<crate::swatches::SwatchDialog>,
    /// Edit › Copy Stitch Settings.
    pub settings_clip: Option<zigzag::params::Params>,
    /// The Transform panel's reference point: (-1, 0, 1) per axis, as in
    /// Illustrator's 9-square locator. X/Y show this point; W/H scale about it.
    pub reference: (i8, i8),
}

/// The Round Corners window: radius (mm), kind, and the document before
/// the preview (Cancel puts it back; OK makes the change one undo step).
#[derive(Clone)]
pub struct CornerDialog {
    pub radius: f64,
    pub kind: zigzag::bezier::CornerKind,
    pub base: Document,
    pub indices: Vec<usize>,
    pub only: Vec<zigzag::bezier::AnchorRef>,
    pub shown: Option<(f64, zigzag::bezier::CornerKind)>,
    pub cut: usize,
}

/// Snap `vals` (positions on one axis that move together) to the nearest
/// target line within `tol`: (shift to apply, the line snapped to, target).
fn snap_axis(
    vals: [f64; 3],
    targets: &[Bounds],
    x_axis: bool,
    tol: f64,
) -> Option<(f64, f64, usize)> {
    let mut best: Option<(f64, f64, usize)> = None;
    for (k, t) in targets.iter().enumerate() {
        let lines = if x_axis {
            [t.min.x, t.center().x, t.max.x]
        } else {
            [t.min.y, t.center().y, t.max.y]
        };
        for l in lines {
            for v in vals {
                let d = l - v;
                if d.abs() <= tol && best.is_none_or(|(b, _, _)| d.abs() < b.abs()) {
                    best = Some((d, l, k));
                }
            }
        }
    }
    best
}

/// A guide line through `at` on one axis, spanning `a` and `b`.
fn guide(at: f64, x_axis: bool, a: &Bounds, b: &Bounds) -> (P, P) {
    if x_axis {
        let (lo, hi) = (a.min.y.min(b.min.y), a.max.y.max(b.max.y));
        (P::new(at, lo), P::new(at, hi))
    } else {
        let (lo, hi) = (a.min.x.min(b.min.x), a.max.x.max(b.max.x));
        (P::new(lo, at), P::new(hi, at))
    }
}

fn moved(b: &Bounds, m: Affine) -> Bounds {
    let mut out = Bounds::empty();
    for p in [
        b.min,
        b.max,
        P::new(b.min.x, b.max.y),
        P::new(b.max.x, b.min.y),
    ] {
        out.add(zigzag::geom::affine_apply(m, p));
    }
    out
}

/// The Transform panel's reference point of `b`.
pub fn reference_point(b: &Bounds, r: (i8, i8)) -> P {
    let pick = |lo: f64, hi: f64, k: i8| match k {
        -1 => lo,
        1 => hi,
        _ => (lo + hi) / 2.0,
    };
    P::new(pick(b.min.x, b.max.x, r.0), pick(b.min.y, b.max.y, r.1))
}

/// Illustrator's reference point locator: nine squares, the chosen one
/// filled. Returns a newly clicked point.
fn reference_locator(ui: &mut egui::Ui, current: (i8, i8)) -> Option<(i8, i8)> {
    let p = crate::theme::pal_ui(ui);
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(22.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Reference point"));
    let resp =
        resp.on_hover_text("Reference point: X and Y show this point; resizing keeps it in place");
    let painter = ui.painter();
    let cell = |k: i8| (k + 1) as f32 * 8.0 + 3.0;
    let mut picked = None;
    painter.rect_stroke(
        r.shrink(3.0),
        1.0,
        Stroke::new(1.0, p.tertiary),
        egui::StrokeKind::Inside,
    );
    for x in [-1i8, 0, 1] {
        for y in [-1i8, 0, 1] {
            let c = r.min + Vec2::new(cell(x), cell(y));
            let on = (x, y) == current;
            let sq = Rect::from_center_size(c, Vec2::splat(if on { 5.0 } else { 4.0 }));
            if on {
                painter.rect_filled(sq, 0.5, p.accent);
            } else {
                painter.rect_filled(sq, 0.5, p.control);
                painter.rect_stroke(
                    sq,
                    0.5,
                    Stroke::new(1.0, p.secondary),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let k = |v: f32| ((v - 11.0) / 8.0).round().clamp(-1.0, 1.0) as i8;
        picked = Some((k(pos.x - r.min.x), k(pos.y - r.min.y)));
    }
    picked.filter(|&v| v != current)
}

/// The transform the panel applies: scale about the reference point to
/// `w`×`h`, then move the reference point to `at` (all design units).
pub fn panel_transform(b: &Bounds, r: (i8, i8), at: P, w: f64, h: f64) -> Affine {
    let rp = reference_point(b, r);
    let sx = if b.width() > 0.0 { w / b.width() } else { 1.0 };
    let sy = if b.height() > 0.0 {
        h / b.height()
    } else {
        1.0
    };
    affine_mul(translation(at - rp), scale_about(rp, sx, sy))
}

impl Studio {
    /// Objects in group `g`, in sewing order.
    pub fn group_members(&self, g: &str) -> Vec<usize> {
        (0..self.doc.shapes.len())
            .filter(|&i| self.doc.shapes[i].groups.iter().any(|x| x == g))
            .collect()
    }

    /// The entered group, if any.
    pub fn scope(&self) -> Option<&str> {
        self.layout.isolated.last().map(String::as_str)
    }

    /// Whether object `i` is inside the entered group (always, if none).
    pub fn in_scope(&self, i: usize) -> bool {
        match self.scope() {
            None => i < self.doc.shapes.len(),
            Some(g) => self
                .doc
                .shapes
                .get(i)
                .is_some_and(|s| s.groups.iter().any(|x| x == g)),
        }
    }

    /// The group a click on object `i` selects: its outermost group inside
    /// the entered group (or at the top level). None: the object stands alone.
    pub fn click_group(&self, i: usize) -> Option<String> {
        let groups = &self.doc.shapes.get(i)?.groups;
        let start = match self.scope() {
            None => 0,
            Some(g) => groups.iter().position(|x| x == g)? + 1,
        };
        // A group of one (Illustrator often wraps each object, and names it
        // with stitch settings) is just the object.
        groups[start..]
            .iter()
            .find(|g| self.group_members(g).len() > 1)
            .cloned()
    }

    /// Select object `i` the way a click does: its whole group, unless
    /// `direct` (⌘-click). `add` (Shift) adds to or removes from the
    /// selection. Clicking outside the entered group leaves it.
    pub fn click_select(&mut self, i: usize, add: bool, direct: bool) {
        if !self.in_scope(i) {
            self.exit_isolation();
        }
        let pick = match (direct, self.click_group(i)) {
            (false, Some(g)) => self.group_members(&g),
            _ => vec![i],
        };
        if !add {
            self.set_selection(&pick);
            return;
        }
        let mut sel = self.sel_indices();
        if pick.iter().all(|j| sel.contains(j)) {
            sel.retain(|j| !pick.contains(j));
        } else {
            sel.extend(
                pick.into_iter()
                    .filter(|j| !sel.contains(j))
                    .collect::<Vec<_>>(),
            );
        }
        self.set_selection(&sel);
    }

    /// Double-click on object `i`: go one level into its group, so clicks
    /// and drags work on the pieces inside, and select the piece clicked.
    pub fn enter_group(&mut self, i: usize) {
        if !self.in_scope(i) {
            self.exit_isolation();
        }
        if let Some(g) = self.click_group(i) {
            self.layout.expanded.insert(g.clone());
            self.status = format!(
                "Editing group \"{}\". Press Esc or double-click empty canvas to leave it.",
                Self::group_label(&g)
            );
            self.layout.isolated.push(g);
        }
        self.click_select(i, false, false);
    }

    /// Leave the innermost entered group, selecting it. False if none.
    pub fn exit_group(&mut self) -> bool {
        let Some(g) = self.layout.isolated.pop() else {
            return false;
        };
        let members = self.group_members(&g);
        self.set_selection(&members);
        match self.scope() {
            Some(outer) => self.status = format!("Editing group \"{}\".", Self::group_label(outer)),
            None => self.clear_group_status(),
        }
        true
    }

    /// Leave every entered group.
    pub fn exit_isolation(&mut self) {
        if !self.layout.isolated.is_empty() {
            self.layout.isolated.clear();
            self.clear_group_status();
        }
    }

    fn clear_group_status(&mut self) {
        if self.status.starts_with("Editing group") {
            self.status.clear();
        }
    }

    /// Drop entered groups that no longer exist (deleted, undone) and leave
    /// isolation when the selection moved outside it (e.g. from the Objects
    /// panel).
    pub fn tidy_isolation(&mut self) {
        if self.layout.isolated.is_empty() {
            return;
        }
        if let Some(k) = self
            .layout
            .isolated
            .iter()
            .position(|g| !self.doc.shapes.iter().any(|s| s.groups.contains(g)))
        {
            self.layout.isolated.truncate(k);
        }
        if self.sel_indices().iter().any(|&i| !self.in_scope(i)) {
            self.exit_isolation();
        }
    }

    /// Readable name for a group id.
    pub fn group_label(g: &str) -> String {
        let words = zigzag::svg::name_words(g);
        if words.is_empty() {
            "Group".into()
        } else {
            words.join(" ")
        }
    }

    /// Every selected object (primary first, then the rest), as indices.
    pub fn sel_indices(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self.sel_index().into_iter().collect();
        if self.layout.multi.is_empty() {
            return out;
        }
        // One pass over the objects (a Select All of a traced design is
        // hundreds of them), not one per selected object.
        let at: std::collections::HashMap<usize, usize> = self
            .doc
            .shapes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.uid, i))
            .collect();
        let mut seen: std::collections::HashSet<usize> = out.iter().copied().collect();
        for uid in &self.layout.multi {
            if let Some(&i) = at.get(uid)
                && seen.insert(i)
            {
                out.push(i);
            }
        }
        out
    }

    pub fn set_selection(&mut self, indices: &[usize]) {
        self.select_index(indices.first().copied());
        self.layout.multi = indices
            .iter()
            .skip(1)
            .filter_map(|&i| self.doc.shapes.get(i))
            .map(|s| s.uid)
            .collect();
    }

    /// Shift-click: add or remove an object from the selection.
    pub fn toggle_selected(&mut self, i: usize) {
        let mut sel = self.sel_indices();
        if let Some(k) = sel.iter().position(|&j| j == i) {
            sel.remove(k);
        } else {
            sel.push(i);
        }
        self.set_selection(&sel);
    }

    pub fn selection_bounds(&self) -> Bounds {
        let mut b = Bounds::empty();
        for i in self.sel_indices() {
            let sb = shape_bounds(&self.doc.shapes[i]);
            if !sb.is_empty() {
                b.add(sb.min);
                b.add(sb.max);
            }
        }
        b
    }

    /// Bounds of objects as sewn: stroke widths included.
    pub fn art_bounds(&self, idx: &[usize]) -> Bounds {
        let mut b = Bounds::empty();
        for &i in idx {
            let s = &self.doc.shapes[i];
            let sb = shape_bounds(s);
            if sb.is_empty() {
                continue;
            }
            let r = match &s.geometry {
                Geometry::Stroke { width, .. } => width / 2.0,
                Geometry::Fill { .. } => 0.0,
            };
            b.add(sb.min - P::new(r, r));
            b.add(sb.max + P::new(r, r));
        }
        b
    }

    /// Scale the selection (everything, if nothing is selected) to fill the
    /// hoop, less a small margin, and center it. With `only_shrink`, artwork
    /// that already fits is left alone. Returns the scale applied.
    pub fn fit_to_hoop(&mut self, only_shrink: bool) -> Option<f64> {
        let had_sel = !self.sel_indices().is_empty();
        if !had_sel {
            let all: Vec<usize> = (0..self.doc.shapes.len()).collect();
            self.set_selection(&all);
        }
        let idx = self.sel_indices();
        let b = self.art_bounds(&idx);
        let (hw, hh) = self.hoop_dims();
        let room = |mm: f64| (mm - 2.0 * HOOP_MARGIN_MM) * 10.0;
        let s = (room(hw) / b.width().max(1e-9)).min(room(hh) / b.height().max(1e-9));
        let done = |me: &mut Studio| {
            if !had_sel {
                me.set_selection(&[]);
            }
        };
        if b.is_empty() || !s.is_finite() || (only_shrink && s >= 1.0) {
            done(self);
            return None;
        }
        let c = b.center();
        let m = affine_mul(translation(P::default() - c), scale_about(c, s, s));
        let ok = self.transform_selected(m, "Fitted");
        done(self);
        if !ok {
            return None;
        }
        self.status = format!(
            "Scaled {} to {:.0}% ({}) to fit the {} hoop.",
            if had_sel {
                "the selection"
            } else {
                "the design"
            },
            s * 100.0,
            crate::units::fmt_size(b.width() * s, b.height() * s),
            crate::units::fmt_size(hw * 10.0, hh * 10.0),
        );
        Some(s)
    }

    /// Transform the selection by `m` (design units) as one undo step.
    pub fn transform_selected(&mut self, m: Affine, what: &str) -> bool {
        let idx = self.sel_indices();
        if idx.is_empty() || m == IDENTITY {
            return false;
        }
        let uids: Vec<usize> = idx.iter().map(|&i| self.doc.shapes[i].uid).collect();
        let from = self.selection_bounds();
        // A rigid move/rotation/reflection needs no reconstruction. Check both
        // column lengths and their dot product, including non-uniform scaling.
        let resized = (m[0] * m[0] + m[1] * m[1] - 1.0).abs() > 1e-6
            || (m[2] * m[2] + m[3] * m[3] - 1.0).abs() > 1e-6
            || (m[0] * m[2] + m[1] * m[3]).abs() > 1e-6;
        let keep_density = self.layout.keep_stitch_density && resized;
        let mut report = None;
        match self.structural(|d| {
            if keep_density {
                report = Some(d.transform_keep_density(&idx, m)?);
                Ok(())
            } else {
                d.transform(&idx, m)
            }
        }) {
            Ok(()) => {
                self.status = format!("{what} {}.", self.selection_label(idx.len()));
                if let Some(r) = &report {
                    self.status.push_str(&format!(
                        " Reconstructed {} fill(s), {} satin(s), {} repeated run(s). {} uncertain object(s)/span(s) scaled without preserving density; review Transform panel.",
                        r.fills, r.satins, r.repeated_runs, r.warnings.len()
                    ));
                }
                if resized {
                    self.layout.density_report = report;
                }
                self.restore_selection(&uids);
                self.layout.last = Some(LastTransform {
                    m,
                    from,
                    copy: false,
                });
                true
            }
            Err(e) => {
                self.status = format!("Couldn't {}: {e:#}", what.to_lowercase());
                false
            }
        }
    }

    pub(crate) fn restore_selection(&mut self, uids: &[usize]) {
        let idx: Vec<usize> = uids
            .iter()
            .filter_map(|u| self.doc.shapes.iter().position(|s| s.uid == *u))
            .collect();
        self.set_selection(&idx);
    }

    fn selection_label(&self, n: usize) -> String {
        match (n, self.sel_index()) {
            (1, Some(i)) => self
                .names
                .get(i)
                .cloned()
                .unwrap_or_else(|| "object".into()),
            _ => format!("{n} objects"),
        }
    }

    pub fn duplicate_selected(&mut self) {
        self.copy_selection_by(P::new(DUPLICATE_OFFSET, DUPLICATE_OFFSET), "Duplicated");
    }

    /// Copy the selection, shifted by `d` (design units), and select the copy.
    pub fn copy_selection_by(&mut self, d: P, what: &str) -> bool {
        let idx = self.sel_indices();
        if idx.is_empty() {
            return false;
        }
        let from = self.selection_bounds();
        let label = self.selection_label(idx.len());
        let mut new = Vec::new();
        match self.structural(|doc| {
            new = doc.duplicate(&idx, d)?;
            Ok(())
        }) {
            Ok(()) => {
                self.status = format!("{what} {label}.");
                self.set_selection(&new);
                self.layout.last = Some(LastTransform {
                    m: translation(d),
                    from,
                    copy: true,
                });
                true
            }
            Err(e) => {
                self.status = format!("Couldn't duplicate: {e:#}");
                false
            }
        }
    }

    /// Combine the selection with a Pathfinder operation.
    pub fn pathfinder_selected(&mut self, op: zigzag::boolean::PathOp) {
        let idx = self.sel_indices();
        let n = idx.len();
        let mut new = 0;
        match self.structural(|d| {
            new = d.pathfinder(&idx, op)?;
            Ok(())
        }) {
            Ok(()) => {
                self.set_selection(&[new]);
                self.status = format!("{op:?}: {n} objects into one shape.");
            }
            Err(e) => self.status = format!("Couldn't combine them: {e:#}"),
        }
    }

    pub fn offset_selected(&mut self, mm: f64) {
        let idx = self.sel_indices();
        let mut new = Vec::new();
        match self.structural(|d| {
            new = d.offset_path(&idx, mm * 10.0)?;
            Ok(())
        }) {
            Ok(()) => {
                self.set_selection(&new);
                self.status = format!("Offset by {mm:+.1} mm (the new shape is behind).");
            }
            Err(e) => self.status = format!("Couldn't offset: {e:#}"),
        }
    }

    /// Object › Path › Release Compound Path: the new group opens in the
    /// Objects list so its pieces show.
    pub fn release_compound_selected(&mut self) {
        let idx = self.sel_indices();
        let mut new = Vec::new();
        let mut groups = Vec::new();
        match self.structural(|d| {
            (new, groups) = d.release_compound(&idx)?;
            Ok(())
        }) {
            Ok(()) => {
                for g in groups {
                    self.layout.expanded.insert(g);
                }
                self.set_selection(&new);
                self.status = format!(
                    "Released into {} objects, grouped. Each one has its own stitch settings now.",
                    new.len()
                );
            }
            Err(e) => self.status = format!("Couldn't release: {e:#}"),
        }
    }

    pub fn outline_stroke_selected(&mut self) {
        let idx = self.sel_indices();
        let mut new = Vec::new();
        match self.structural(|d| {
            new = d.outline_stroke(&idx)?;
            Ok(())
        }) {
            Ok(()) => {
                self.set_selection(&new);
                self.status = format!("Outlined {} line(s) into shapes.", new.len());
            }
            Err(e) => self.status = format!("Couldn't outline: {e:#}"),
        }
    }

    /// Open Round Corners for the selection (or, in Direct Selection, the
    /// selected points of the object).
    pub fn open_corner_dialog(&mut self) {
        let indices = self.sel_indices();
        if indices.is_empty() {
            return;
        }
        let only = if self.tool == crate::app::Tool::Direct && indices.len() == 1 {
            self.path_edit.selected.clone()
        } else {
            Vec::new()
        };
        self.layout.corner_dialog = Some(CornerDialog {
            radius: 2.0,
            kind: Default::default(),
            base: self.doc.clone(),
            indices,
            only,
            shown: None,
            cut: 0,
        });
    }

    /// The Round Corners window, previewing on the canvas as you go.
    pub fn corner_window(&mut self, ctx: &egui::Context) {
        use zigzag::bezier::CornerKind;
        let Some(mut d) = self.layout.corner_dialog.take() else {
            return;
        };
        let open = true;
        let (mut ok, mut cancel) = (false, false);
        // Modal: the preview lives in the document, so nothing else may edit
        // it until OK or Cancel (the canvas, shortcuts, menus and agent calls
        // wait). No dimming: the preview is the point.
        use crate::dialog;
        let id = egui::Id::unique("round-corners");
        dialog::show(ctx, id, dialog::Place::Top, |ui| {
            dialog::heading(ui, "Round Corners");
            crate::theme::form(ui, "corner-form", |f| {
                f.row("Radius", |ui, l| {
                    ui.add(crate::units::drag_mm(&mut d.radius, 0.0..=50.0, 0.05))
                        .labelled_by(l);
                });
                f.row("Corners", |ui, _| {
                    crate::theme::segmented(
                        ui,
                        "corner-kind",
                        &mut d.kind,
                        &[
                            (CornerKind::Round, "Round"),
                            (CornerKind::Inverted, "Inverted"),
                            (CornerKind::Chamfer, "Chamfer"),
                        ],
                    );
                });
            });
            crate::theme::caption(
                ui,
                match (d.cut, d.only.is_empty()) {
                    (0, _) => "No sharp corners to round here.".to_string(),
                    (n, true) => format!("{n} corners. Each rounds as far as its sides allow."),
                    (n, false) => {
                        format!("{n} selected corners. Each rounds as far as its sides allow.")
                    }
                },
            );
            dialog::buttons(ui, |ui| {
                ok = dialog::primary(ui, "OK", true).clicked()
                    || ui.input(|i| i.key_pressed(Key::Enter));
                cancel = dialog::secondary(ui, "Cancel").clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape));
            });
        });
        // Preview: the corners cut from the document as it was.
        if d.shown != Some((d.radius, d.kind)) {
            let mut doc = d.base.clone();
            d.cut = doc
                .round_corners(&d.indices, d.radius * 10.0, d.kind, &d.only)
                .unwrap_or(0);
            self.doc = doc;
            self.dirty = true;
            d.shown = Some((d.radius, d.kind));
        }
        let uids: Vec<usize> = d
            .indices
            .iter()
            .filter_map(|&i| d.base.shapes.get(i))
            .map(|s| s.uid)
            .collect();
        if ok {
            let after = std::mem::replace(&mut self.doc, d.base.clone());
            if d.cut > 0 && d.radius > 0.0 {
                let _ = self.structural(|doc| {
                    *doc = after;
                    Ok(())
                });
                self.status = format!("Rounded {} corners.", d.cut);
            }
            self.path_edit.selected.clear();
            self.restore_selection(&uids);
            self.dirty = true;
        } else if cancel || !open {
            self.doc = d.base;
            self.restore_selection(&uids);
            self.dirty = true;
        } else {
            self.layout.corner_dialog = Some(d);
        }
    }

    /// The Offset Path window.
    pub fn offset_window(&mut self, ctx: &egui::Context) {
        let Some(mut mm) = self.layout.offset_dialog else {
            return;
        };
        let mut open = true;
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new("Offset Path")
            .title_frame(crate::theme::title_frame(ctx))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 90.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Offset");
                    let r = ui.add(
                        crate::units::drag_mm(&mut mm, -20.0..=20.0, 0.1),
                    );
                    r.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::SpinButton, true, "Offset distance")
                    });
                });
                crate::theme::caption(
                    ui,
                    "A new shape around each selected object (inside, if negative), sewn just before it.",
                );
                ui.add_space(crate::theme::space::S);
                // A button row (right-aligned), only as tall as its buttons.
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 24.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                    let accent = crate::theme::pal_ui(ui).accent;
                    ok = ui
                        .add(egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE)).fill(accent))
                        .clicked()
                        || ui.input(|i| i.key_pressed(Key::Enter));
                    cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
                });
            });
        self.layout.offset_dialog = Some(mm);
        if ok {
            self.layout.offset_dialog = None;
            if mm.abs() >= 0.05 {
                self.offset_selected(mm);
            }
        } else if cancel || !open {
            self.layout.offset_dialog = None;
        }
    }

    /// Repeat the last transform on the selection (Illustrator's Transform
    /// Again, ⌘D): after Option-dragging a copy, each ⌘D makes another one
    /// the same distance on. With nothing to repeat it duplicates.
    pub fn transform_again(&mut self) {
        if self.sel_indices().is_empty() {
            return;
        }
        let Some(last) = self.layout.last.clone() else {
            self.duplicate_selected();
            return;
        };
        // Follow the selection: the same transform, relative to where it is now.
        let d = self.selection_bounds().center() - last.from.center();
        let m = affine_mul(
            translation(d),
            affine_mul(last.m, translation(P::default() - d)),
        );
        if last.copy {
            self.copy_selection_by(P::new(m[4], m[5]), "Copied");
        } else {
            self.transform_selected(m, "Transformed again:");
        }
    }

    pub fn copy_selected(&mut self) {
        let idx = self.sel_indices();
        if idx.is_empty() {
            return;
        }
        let mut doc = self.doc.clone();
        match doc.copy(&idx) {
            Ok(frags) => {
                self.status = format!("Copied {}.", self.selection_label(idx.len()));
                self.layout.system_clipboard = Some(doc.fragments_svg(&frags));
                self.layout.clipboard = frags;
                self.layout.pastes = 0;
            }
            Err(e) => self.status = format!("Couldn't copy: {e:#}"),
        }
    }

    /// Put a pending copy on the system clipboard.
    pub fn flush_system_clipboard(&mut self, ctx: &egui::Context) {
        if let Some(svg) = self.layout.system_clipboard.take() {
            ctx.copy_text(svg);
        }
    }

    pub fn paste(&mut self) {
        if self.layout.clipboard.is_empty() {
            return;
        }
        let frags = self.layout.clipboard.clone();
        self.layout.pastes += 1;
        let off = DUPLICATE_OFFSET * self.layout.pastes as f64;
        let mut new = Vec::new();
        match self.structural(|d| {
            new = d.paste(&frags, P::new(off, off))?;
            Ok(())
        }) {
            Ok(()) => {
                self.status = format!("Pasted {} object(s).", new.len());
                self.set_selection(&new);
            }
            Err(e) => self.status = format!("Couldn't paste: {e:#}"),
        }
    }

    pub fn delete_selection(&mut self) {
        let idx = self.sel_indices();
        if idx.is_empty() {
            return;
        }
        let label = self.selection_label(idx.len());
        match self.structural(|d| d.delete_many(&idx)) {
            Ok(()) => self.status = format!("Deleted {label}."),
            Err(e) => self.status = format!("Couldn't delete: {e:#}"),
        }
        self.set_selection(&[]);
    }

    /// Keyboard layout shortcuts. Call before panels draw.
    pub fn layout_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (copy, paste, nudge) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            let step = if i.modifiers.shift { 50.0 } else { 5.0 };
            let mut d = P::default();
            for (k, v) in [
                (Key::ArrowLeft, P::new(-step, 0.0)),
                (Key::ArrowRight, P::new(step, 0.0)),
                (Key::ArrowUp, P::new(0.0, -step)),
                (Key::ArrowDown, P::new(0.0, step)),
            ] {
                if i.key_pressed(k) && !cmd {
                    d = d + v;
                }
            }
            (
                cmd && i.key_pressed(Key::C)
                    || i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                cmd && i.key_pressed(Key::V)
                    || i.events
                        .iter()
                        .any(|e| matches!(e, egui::Event::Paste(_) | egui::Event::PasteImage(_))),
                d,
            )
        });
        if copy {
            self.copy_selected();
        }
        if paste {
            self.paste();
        }
        if nudge.len() > 0.0 {
            self.transform_selected(translation(nudge), "Nudged");
        }
    }

    // ------------------------------------------------------------ canvas

    /// Screen rect of the selection box (with a little margin).
    pub fn selection_screen_rect(&self, rect: Rect) -> Option<Rect> {
        let b = self.selection_bounds();
        if b.is_empty() {
            return None;
        }
        let a = self.view.to_screen(rect, b.min);
        let c = self.view.to_screen(rect, b.max);
        Some(Rect::from_two_pos(a, c).expand(4.0))
    }

    fn handle_positions(r: Rect) -> Vec<(Handle, Pos2)> {
        let mut v = Vec::new();
        for sx in [-1i8, 0, 1] {
            for sy in [-1i8, 0, 1] {
                if sx == 0 && sy == 0 {
                    continue;
                }
                let x = match sx {
                    -1 => r.left(),
                    0 => r.center().x,
                    _ => r.right(),
                };
                let y = match sy {
                    -1 => r.top(),
                    0 => r.center().y,
                    _ => r.bottom(),
                };
                v.push((Handle::Scale(sx, sy), Pos2::new(x, y)));
            }
        }
        v
    }

    /// The handle under `pos`: a scale handle, or (as in Illustrator) the
    /// rotate zone just outside a corner of the selection box.
    pub fn handle_at(&self, rect: Rect, pos: Pos2) -> Option<Handle> {
        let r = self.selection_screen_rect(rect)?;
        if r.width() < 6.0 && r.height() < 6.0 {
            return None;
        }
        if let Some(h) = Self::handle_positions(r)
            .into_iter()
            .find(|(_, p)| p.distance(pos) <= 7.0)
            .map(|(h, _)| h)
        {
            return Some(h);
        }
        let near_corner = [
            r.left_top(),
            r.right_top(),
            r.left_bottom(),
            r.right_bottom(),
        ]
        .iter()
        .any(|c| c.distance(pos) <= 22.0);
        (near_corner && !r.contains(pos)).then_some(Handle::Rotate)
    }

    pub fn draw_selection_box(&self, painter: &egui::Painter, rect: Rect) {
        let Some(r) = self.selection_screen_rect(rect) else {
            return;
        };
        let accent = Color32::from_rgb(40, 140, 255);
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(1.0, accent.gamma_multiply(0.7)),
            egui::StrokeKind::Middle,
        );
        for (_, p) in Self::handle_positions(r) {
            let sq = Rect::from_center_size(p, Vec2::splat(8.0));
            painter.rect_filled(sq, 1.0, Color32::WHITE);
            painter.rect_stroke(sq, 1.0, Stroke::new(1.2, accent), egui::StrokeKind::Middle);
        }
    }

    /// Smart guides for the gesture in progress.
    pub fn draw_guides(&self, painter: &egui::Painter, rect: Rect) {
        let Some(d) = &self.drag else { return };
        let magenta = Color32::from_rgb(236, 0, 140);
        for (a, b) in &d.guides {
            let (a, b) = (self.view.to_screen(rect, *a), self.view.to_screen(rect, *b));
            painter.line_segment([a, b], Stroke::new(1.0, magenta));
            for q in [a, b] {
                painter.line_segment(
                    [q - Vec2::new(3.0, 3.0), q + Vec2::new(3.0, 3.0)],
                    Stroke::new(1.0, magenta),
                );
                painter.line_segment(
                    [q + Vec2::new(-3.0, 3.0), q + Vec2::new(3.0, -3.0)],
                    Stroke::new(1.0, magenta),
                );
            }
        }
    }

    pub fn handle_cursor(h: Handle) -> egui::CursorIcon {
        match h {
            Handle::Rotate => egui::CursorIcon::Alias,
            Handle::Scale(0, _) => egui::CursorIcon::ResizeVertical,
            Handle::Scale(_, 0) => egui::CursorIcon::ResizeHorizontal,
            Handle::Scale(x, y) if x == y => egui::CursorIcon::ResizeNwSe,
            Handle::Scale(..) => egui::CursorIcon::ResizeNeSw,
        }
    }

    /// Start a move/scale/rotate gesture at design point `p`.
    pub fn begin_drag(&mut self, handle: Option<Handle>, p: P) {
        let indices = self.sel_indices();
        if indices.is_empty() {
            return;
        }
        let b = self.selection_bounds();
        let mode = match handle {
            Some(Handle::Rotate) => DragMode::Rotate { center: b.center() },
            Some(Handle::Scale(x, y)) => DragMode::Scale {
                handle: (x, y),
                bounds: b,
            },
            None => DragMode::Move,
        };
        // Smart guide targets: every other object, and the hoop (its edges
        // and center lines).
        let (hw, hh) = self.hoop_dims();
        let mut targets = vec![Bounds {
            min: P::new(-hw * 5.0, -hh * 5.0),
            max: P::new(hw * 5.0, hh * 5.0),
        }];
        targets.extend(
            (0..self.doc.shapes.len())
                .filter(|i| !indices.contains(i))
                .map(|i| self.art_bounds(&[i]))
                .filter(|t| !t.is_empty()),
        );
        self.drag = Some(Drag {
            mode,
            start: p,
            base: self.doc.clone(),
            indices,
            m: IDENTITY,
            bounds: b,
            targets,
            guides: Vec::new(),
            copy: false,
        });
    }

    /// Update the live preview for pointer at `p`.
    pub fn update_drag(&mut self, p: P, mods: DragMods) {
        let lock = self.layout.lock_aspect;
        let Some(drag) = &mut self.drag else { return };
        let mut guides = Vec::new();
        let b0 = drag.bounds;
        let m = match &drag.mode {
            DragMode::Move => {
                let mut d = p - drag.start;
                // Shift: the nearest 45° direction.
                let (mut free_x, mut free_y) = (true, true);
                if mods.shift && d.len() > 0.0 {
                    let a = (d.y.atan2(d.x) / std::f64::consts::FRAC_PI_4).round()
                        * std::f64::consts::FRAC_PI_4;
                    let u = P::new(a.cos(), a.sin());
                    d = u * d.dot(u);
                    if u.x.abs() < 1e-9 {
                        d.x = 0.0;
                        free_x = false;
                    } else if u.y.abs() < 1e-9 {
                        d.y = 0.0;
                        free_y = false;
                    } else {
                        (free_x, free_y) = (false, false);
                    }
                }
                if mods.snap {
                    let c = b0.center();
                    if free_x
                        && let Some((dx, _, _)) = snap_axis(
                            [b0.min.x + d.x, c.x + d.x, b0.max.x + d.x],
                            &drag.targets,
                            true,
                            mods.tol,
                        )
                    {
                        d.x += dx;
                    }
                    if free_y
                        && let Some((dy, _, _)) = snap_axis(
                            [b0.min.y + d.y, c.y + d.y, b0.max.y + d.y],
                            &drag.targets,
                            false,
                            mods.tol,
                        )
                    {
                        d.y += dy;
                    }
                }
                translation(d)
            }
            DragMode::Scale {
                handle: (hx, hy),
                bounds: b,
            } => {
                let side = |h: i8, lo: f64, hi: f64, far: bool| match (h, far) {
                    (-1, false) | (1, true) => hi,
                    (1, false) | (-1, true) => lo,
                    _ => (lo + hi) / 2.0,
                };
                let c = b.center();
                // Option: from the center. Otherwise the opposite side stays put.
                let anchor = if mods.alt {
                    c
                } else {
                    P::new(
                        side(*hx, b.min.x, b.max.x, false),
                        side(*hy, b.min.y, b.max.y, false),
                    )
                };
                let grab = P::new(
                    side(*hx, b.min.x, b.max.x, true),
                    side(*hy, b.min.y, b.max.y, true),
                );
                let mut q = p;
                if mods.snap {
                    if *hx != 0
                        && let Some((dx, _, _)) = snap_axis([q.x; 3], &drag.targets, true, mods.tol)
                    {
                        q.x += dx;
                    }
                    if *hy != 0
                        && let Some((dy, _, _)) =
                            snap_axis([q.y; 3], &drag.targets, false, mods.tol)
                    {
                        q.y += dy;
                    }
                }
                // Dragging past the far side flips, as in Illustrator.
                let keep = |r: f64| {
                    if r.abs() < 0.02 {
                        0.02 * if r < 0.0 { -1.0 } else { 1.0 }
                    } else {
                        r
                    }
                };
                let ratio = |a: f64, g: f64, v: f64| {
                    if (g - a).abs() > 1e-6 {
                        keep((v - a) / (g - a))
                    } else {
                        1.0
                    }
                };
                let mut sx = if *hx == 0 {
                    1.0
                } else {
                    ratio(anchor.x, grab.x, q.x)
                };
                let mut sy = if *hy == 0 {
                    1.0
                } else {
                    ratio(anchor.y, grab.y, q.y)
                };
                if mods.shift || lock {
                    // Keep proportions.
                    if *hx != 0 && *hy != 0 {
                        let g = grab - anchor;
                        let s = keep((q - anchor).dot(g) / g.dot(g));
                        (sx, sy) = (s, s);
                    } else if *hx != 0 {
                        sy = sx.abs();
                    } else {
                        sx = sy.abs();
                    }
                }
                scale_about(anchor, sx, sy)
            }
            DragMode::Rotate { center } => {
                let a0 = (drag.start - *center).y.atan2((drag.start - *center).x);
                let a1 = (p - *center).y.atan2((p - *center).x);
                let mut deg = (a1 - a0).to_degrees();
                deg = if mods.shift {
                    (deg / 45.0).round() * 45.0
                } else {
                    deg.round()
                };
                rotate_about(*center, deg)
            }
        };
        // Guides: every line the moved selection now sits on.
        if mods.snap && !matches!(drag.mode, DragMode::Rotate { .. }) {
            let nb = moved(&b0, m);
            let nc = nb.center();
            for t in &drag.targets {
                let tc = t.center();
                for (lines, mine) in [([t.min.x, tc.x, t.max.x], [nb.min.x, nc.x, nb.max.x])] {
                    for l in lines {
                        if mine.iter().any(|v| (v - l).abs() < 0.05) {
                            guides.push(guide(l, true, &nb, t));
                        }
                    }
                }
                for l in [t.min.y, tc.y, t.max.y] {
                    if [nb.min.y, nc.y, nb.max.y]
                        .iter()
                        .any(|v| (v - l).abs() < 0.05)
                    {
                        guides.push(guide(l, false, &nb, t));
                    }
                }
            }
        }
        drag.m = m;
        drag.guides = guides;
        drag.copy = mods.alt && matches!(drag.mode, DragMode::Move);
        // The outlines follow the pointer (only the moved shapes' points
        // are rewritten, from the document as it was when the gesture
        // began); the stitches are drawn through the transform (see
        // `DrawOptions::moving`) and planned once, when the gesture ends.
        self.doc
            .preview_transform_from(&drag.base, &drag.indices, m);
    }

    /// Finish the gesture: one undo step with the transform written to the SVG.
    pub fn end_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        if drag.copy {
            // Option-drag: the original stays, a copy lands where you let go.
            self.doc = drag.base;
            if drag.m[4].abs() + drag.m[5].abs() >= 0.5 {
                self.copy_selection_by(P::new(drag.m[4], drag.m[5]), "Copied");
            }
            return;
        }
        self.doc = drag.base;
        let tiny = (drag.m[4].abs() + drag.m[5].abs()) < 0.5
            && (drag.m[0] - 1.0).abs() < 1e-3
            && (drag.m[3] - 1.0).abs() < 1e-3
            && drag.m[1].abs() < 1e-3
            && drag.m[2].abs() < 1e-3;
        if tiny {
            return;
        }
        let what = match drag.mode {
            DragMode::Move => "Moved",
            DragMode::Scale { .. } => "Resized",
            DragMode::Rotate { .. } => "Rotated",
        };
        if self.transform_selected(drag.m, what) && self.planner.slow {
            // A big design's new plan comes from the worker a moment
            // later: until then the stitches as planned follow the
            // objects, rather than jumping back to where they were.
            self.shift_planned(&drag.indices, drag.m);
        }
    }

    pub fn drag_label(&self) -> Option<String> {
        let d = self.drag.as_ref()?;
        Some(match &d.mode {
            DragMode::Move => {
                let u = crate::units::current();
                let sign = |v: f64| {
                    let n = u.number(u.amount(v));
                    if n.starts_with('-') {
                        n
                    } else {
                        format!("+{n}")
                    }
                };
                format!("{}, {} {}", sign(d.m[4]), sign(d.m[5]), u.suffix())
            }
            DragMode::Scale { bounds, .. } => crate::units::fmt_size(
                bounds.width() * d.m[0].abs(),
                bounds.height() * d.m[3].abs(),
            ),
            DragMode::Rotate { .. } => format!("{:.0}°", d.m[1].atan2(d.m[0]).to_degrees()),
        })
    }

    // ------------------------------------------------------------ panel

    /// Position, size, rotation and mirror for the selection (Transform panel).
    pub fn layout_panel(&mut self, ui: &mut egui::Ui) {
        use crate::icons::Icon;
        use crate::theme::{form, icon_button};
        let b = self.selection_bounds();
        if b.is_empty() {
            return;
        }
        ui.checkbox(&mut self.layout.keep_stitch_density, "Keep stitch density")
            .on_hover_text("Experimental: rebuild recognizable fills, satin sections, and repeated running stitches before resizing. Uncertain spans resize normally and are flagged below. Undo restores the original stitches.");
        if self.layout.keep_stitch_density {
            ui.small("Experimental. Uncertain spans resize normally and are flagged below.");
        }
        if let Some(report) = &self.layout.density_report {
            ui.label(format!(
                "Last resize: {} fill(s), {} satin(s), {} repeated run(s) reconstructed",
                report.fills, report.satins, report.repeated_runs
            ));
            let mut select = None;
            if !report.warnings.is_empty() {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Density NOT preserved for these objects/spans:",
                );
                egui::ScrollArea::vertical()
                    .id_salt("density_warnings")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for warning in &report.warnings {
                            if ui
                                .small_button(&warning.label)
                                .on_hover_text(&warning.reason)
                                .clicked()
                            {
                                select = Some(warning.uid);
                            }
                        }
                    });
            }
            if let Some(uid) = select {
                self.restore_selection(&[uid]);
                return;
            }
        }
        let refp = self.layout.reference;
        let rp = reference_point(&b, refp);
        let now = [
            rp.x / 10.0,
            rp.y / 10.0,
            b.width() / 10.0,
            b.height() / 10.0,
        ];
        let mut f = self.layout.fields.unwrap_or(now);
        let mut commit: Option<[f64; 4]> = None;
        let mut interacting = false;
        let lock = self.layout.lock_aspect;
        let mut toggle_lock = false;
        let field = |ui: &mut egui::Ui, label: &str, v: &mut f64, min: f64| {
            let r = ui.add(crate::units::drag_mm(v, min..=500.0, 0.1).prefix(format!("{label}  ")));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, label));
            (
                r.changed(),
                r.dragged() || r.has_focus(),
                r.drag_stopped() || r.lost_focus(),
            )
        };
        let c = b.center();
        let mut new_ref: Option<(i8, i8)> = None;
        let mut path_op: Option<zigzag::boolean::PathOp> = None;
        let mut rotate: Option<f64> = None;
        let mut flip: Option<(f64, f64)> = None;
        let mut center = false;
        form(ui, "transform-form", |fm| {
            let (x, y) = fm.row("Position", |ui, _| {
                if let Some(r) = reference_locator(ui, refp) {
                    new_ref = Some(r);
                }
                (
                    field(ui, "X", &mut f[0], -500.0),
                    field(ui, "Y", &mut f[1], -500.0),
                )
            });
            let (old_w, old_h) = (f[2], f[3]);
            let (w, h) = fm.row("Size", |ui, _| {
                // Line W/H up under X/Y (the reference point sits before X).
                ui.add_space(22.0 + ui.spacing().item_spacing.x);
                let w = field(ui, "W", &mut f[2], 0.5);
                let h = field(ui, "H", &mut f[3], 0.5);
                if icon_button(
                    ui,
                    Icon::Link,
                    if lock {
                        "Proportions locked"
                    } else {
                        "Keep proportions"
                    },
                    lock,
                    22.0,
                )
                .clicked()
                {
                    toggle_lock = true;
                }
                (w, h)
            });
            if lock {
                if w.0 && old_w > 0.0 {
                    f[3] = old_h * f[2] / old_w;
                } else if h.0 && old_h > 0.0 {
                    f[2] = old_w * f[3] / old_h;
                }
            }
            for (changed, active, done) in [x, y, w, h] {
                interacting |= active;
                if done || (changed && !active) {
                    commit = Some(f);
                }
            }
            fm.row("Rotate", |ui, _| {
                let r = ui.add(
                    egui::DragValue::new(&mut self.layout.rotate_by)
                        .speed(1.0)
                        .range(-180.0..=180.0)
                        .suffix("°"),
                );
                r.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::SpinButton, true, "Rotation angle")
                });
                if (r.lost_focus() || ui.add(egui::Button::new("Apply").small()).clicked())
                    && self.layout.rotate_by != 0.0
                {
                    rotate = Some(self.layout.rotate_by);
                }
                if icon_button(ui, Icon::RotateLeft, "Rotate Left 90°", false, 22.0).clicked() {
                    rotate = Some(-90.0);
                }
                if icon_button(ui, Icon::RotateRight, "Rotate Right 90°", false, 22.0).clicked() {
                    rotate = Some(90.0);
                }
            });
            fm.row("Flip", |ui, _| {
                if icon_button(ui, Icon::FlipH, "Flip Horizontal", false, 22.0).clicked() {
                    flip = Some((-1.0, 1.0));
                }
                if icon_button(ui, Icon::FlipV, "Flip Vertical", false, 22.0).clicked() {
                    flip = Some((1.0, -1.0));
                }
                ui.add_space(8.0);
                if icon_button(ui, Icon::Target, "Center in Hoop", false, 22.0).clicked() {
                    center = true;
                }
                if icon_button(ui, Icon::ZoomFit, "Fit to Hoop", false, 22.0).clicked() {
                    self.fit_to_hoop(false);
                }
                if icon_button(ui, Icon::Duplicate, "Duplicate", false, 22.0).clicked() {
                    self.duplicate_selected();
                }
            });
            if self.sel_indices().len() >= 2 {
                fm.row("Pathfinder", |ui, _| {
                    use zigzag::boolean::PathOp;
                    for (op, icon, label) in [
                        (PathOp::Unite, Icon::PathUnite, "Unite"),
                        (PathOp::MinusFront, Icon::PathMinusFront, "Minus Front"),
                        (PathOp::Intersect, Icon::PathIntersect, "Intersect"),
                        (PathOp::Exclude, Icon::PathExclude, "Exclude"),
                    ] {
                        if icon_button(ui, icon, label, false, 22.0)
                            .on_hover_text(label)
                            .clicked()
                        {
                            path_op = Some(op);
                        }
                    }
                });
            }
        });
        if let Some(op) = path_op {
            self.pathfinder_selected(op);
            return;
        }
        if toggle_lock {
            self.layout.lock_aspect = !self.layout.lock_aspect;
        }
        if let Some(r) = new_ref {
            self.layout.reference = r;
            self.layout.fields = None;
            return;
        }
        self.layout.fields = if interacting { Some(f) } else { None };
        if let Some(t) = commit {
            let m = panel_transform(
                &b,
                refp,
                P::new(t[0] * 10.0, t[1] * 10.0),
                t[2] * 10.0,
                t[3] * 10.0,
            );
            let resized = (m[0] - 1.0).abs() > 1e-4 || (m[3] - 1.0).abs() > 1e-4;
            if resized || (t[0] - now[0]).abs() > 1e-3 || (t[1] - now[1]).abs() > 1e-3 {
                self.transform_selected(m, if resized { "Resized" } else { "Moved" });
            }
            self.layout.fields = None;
        }
        if let Some(deg) = rotate {
            self.transform_selected(rotate_about(c, deg), "Rotated");
        }
        if let Some((sx, sy)) = flip {
            self.transform_selected(scale_about(c, sx, sy), "Flipped");
        }
        if center {
            self.transform_selected(translation(P::default() - c), "Centered");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::path::PathBuf;

    fn studio() -> Harness<'static, Studio> {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/badge.svg"));
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 1000.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(Some(path), None),
            );
        h.run_steps(4);
        h
    }

    #[test]
    fn density_checkbox_reconstructs_and_undo_restores_manual_stitches() {
        use zigzag::params::Kind;
        let mut s = Studio::with_file(None, None);
        let mut path = String::from("M0,0 ");
        for i in 0..30 {
            path.push_str(&format!("L0,{} L3,{} ", i as f64 * 0.4, i as f64 * 0.4));
        }
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="30mm" height="30mm" viewBox="0 0 30 30"><path id="column_manual" d="{path}" fill="none" stroke="red"/><path id="unknown_manual" d="M10 0 L10 3 L10 6" fill="none" stroke="blue"/></svg>"#
        );
        s.doc = Document::from_svg(&svg, "density", None).unwrap();
        s.set_selection(&[0, 1]);
        let original = s.doc.to_svg().unwrap();
        // Render the actual Transform panel, without unrelated dock state.
        let mut h = Harness::builder()
            .with_size(Vec2::new(400.0, 800.0))
            .build_ui_state(|ui, s: &mut Studio| s.layout_panel(ui), s);
        h.run_steps(2);
        assert!(!h.state().layout.keep_stitch_density);
        h.get_by_label("Keep stitch density").click();
        h.run_steps(2);
        assert!(h.state().layout.keep_stitch_density);
        assert!(
            h.state_mut()
                .transform_selected(scale_about(P::default(), 2.0, 2.0), "Resized")
        );
        h.run_steps(2);
        assert_eq!(h.state().doc.shapes[0].params.kind, Some(Kind::Manual));
        assert!(
            h.state()
                .doc
                .to_svg()
                .unwrap()
                .contains("data-zigzag-density-source=")
        );
        assert_eq!(
            h.state()
                .layout
                .density_report
                .as_ref()
                .unwrap()
                .warnings
                .len(),
            1
        );
        h.get_by_label("unknown_manual").click();
        h.run_steps(2);
        assert_eq!(h.state().sel_index(), Some(1));
        assert!(h.state_mut().apply_history(crate::app::History::Undo));
        assert_eq!(h.state().doc.to_svg().unwrap(), original);
        assert!(h.state().layout.density_report.is_none());
        assert!(h.state_mut().apply_history(crate::app::History::Redo));
        assert!(
            h.state()
                .doc
                .to_svg()
                .unwrap()
                .contains("data-zigzag-density-source=")
        );
    }

    #[test]
    fn density_checkbox_supports_downloaded_alower_and_undo_redo() {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/alower.pes"));
        let mut s = Studio::with_file(Some(path), None);
        s.set_selection(&[0]);
        let original = s.doc.to_svg().unwrap();
        let mut h = Harness::builder()
            .with_size(Vec2::new(400.0, 800.0))
            .build_ui_state(|ui, s: &mut Studio| s.layout_panel(ui), s);
        h.run_steps(2);
        h.get_by_label("Keep stitch density").click();
        h.run_steps(2);
        assert!(
            h.state_mut()
                .transform_selected(scale_about(P::default(), 2.0, 2.0), "Resized")
        );
        h.run_steps(2);
        let report = h.state().layout.density_report.as_ref().unwrap();
        assert_eq!(report.satins, 2);
        assert!(report.warnings.is_empty());
        let rebuilt = h.state().doc.to_svg().unwrap();
        assert!(rebuilt.contains("data-zigzag-density-source="));
        assert!(h.state_mut().apply_history(crate::app::History::Undo));
        assert_eq!(h.state().doc.to_svg().unwrap(), original);
        assert!(h.state_mut().apply_history(crate::app::History::Redo));
        assert_eq!(h.state().doc.to_svg().unwrap(), rebuilt);
    }

    fn index_of(h: &Harness<'_, Studio>, name: &str) -> usize {
        h.state().names.iter().position(|n| n == name).unwrap()
    }

    fn drag(h: &mut Harness<'_, Studio>, from: Pos2, to: Pos2) {
        h.hover_at(from);
        h.run_steps(1);
        h.drag_at(from);
        h.run_steps(1);
        for k in 1..=4 {
            h.hover_at(from + (to - from) * (k as f32 / 4.0));
            h.run_steps(1);
        }
        h.drop_at(to);
        h.run_steps(3);
    }

    /// The stitches of a dropped object, while a big design's new plan is
    /// still being made on the worker: where the object is, not where it was.
    #[test]
    fn dropped_stitches_follow_the_object_while_the_worker_plans() {
        let mut h = studio();
        // (A startup plan slow enough for the worker, on a busy machine,
        // would answer in the middle of this and reset `slow`: let it.)
        for _ in 0..400 {
            if !h.state().planner.pending {
                break;
            }
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let sun = index_of(&h, "sun");
        let c = shape_bounds(&h.state().doc.shapes[sun]).center();
        let rect = h.state().canvas_rect;
        let at = h.state().view.to_screen(rect, c);
        let scale = h.state().view.scale;
        // The sun's stitches before.
        let block_of = |h: &Harness<'_, Studio>, i: usize| {
            h.state()
                .design
                .block_shapes
                .iter()
                .position(|v| v == &[i])
                .expect("the sun has a block of its own")
        };
        // (Its center: a fresh plan may well start at another stitch.)
        let first_pt = |h: &Harness<'_, Studio>, i: usize| {
            let mut b = Bounds::empty();
            for r in &h.state().design.blocks[block_of(h, i)].runs {
                r.pts.iter().for_each(|&p| b.add(p));
            }
            b.center()
        };
        let p0 = first_pt(&h, sun);
        h.hover_at(at);
        h.run_steps(1);
        h.drag_at(at);
        h.run_steps(1);
        h.hover_at(at + Vec2::new(10.0 * scale, 0.0));
        h.run_steps(1);
        // A big design: the drop's plan goes to the worker.
        h.state_mut().planner.slow = true;
        let planned = h.state().replans;
        h.drop_at(at + Vec2::new(10.0 * scale, 0.0));
        // From the drop on, frame after frame, the stitches are where
        // the object is: the old plan's, moved along, while the worker
        // plans; the new plan's once it answers.
        let mut answered = false;
        for _ in 0..400 {
            h.run_steps(1);
            let sun = index_of(&h, "sun");
            let p1 = first_pt(&h, sun);
            assert!(
                (p1.x - p0.x - 100.0).abs() < 2.0 && (p1.y - p0.y).abs() < 2.0,
                "stitches 10 mm right with the object (worker pending: {}): {p0:?} → {p1:?}",
                h.state().planner.pending
            );
            // Planned again (on the worker, or here if it turned out quick).
            let st = h.state();
            if st.replans > planned && !st.dirty && !st.planner.pending {
                answered = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(answered, "the worker answered");
    }

    #[test]
    fn dragging_an_object_moves_it_as_one_undo_step() {
        let mut h = studio();
        let sun = index_of(&h, "sun");
        let c = shape_bounds(&h.state().doc.shapes[sun]).center();
        let rect = h.state().canvas_rect;
        let at = h.state().view.to_screen(rect, c);
        let scale = h.state().view.scale;
        drag(&mut h, at, at + Vec2::new(10.0 * scale, 0.0));
        let sun = index_of(&h, "sun");
        let c2 = shape_bounds(&h.state().doc.shapes[sun]).center();
        assert!(
            (c2.x - c.x - 100.0).abs() < 2.0,
            "moved 10 mm right: {} → {}",
            c.x,
            c2.x
        );
        assert!((c2.y - c.y).abs() < 2.0);
        assert_eq!(h.state().undo.len(), 1);
        assert!(
            h.state()
                .doc
                .to_svg()
                .unwrap()
                .contains("transform=\"matrix(")
        );
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Z);
        h.run_steps(3);
        let c3 = shape_bounds(&h.state().doc.shapes[index_of(&h, "sun")]).center();
        assert!((c3.x - c.x).abs() < 0.5);
    }

    use super::{DragMods, panel_transform, reference_point};

    fn mods(shift: bool, alt: bool, snap: bool) -> DragMods {
        DragMods {
            shift,
            alt,
            snap,
            tol: 10.0,
        }
    }

    /// A whole gesture through the same calls the canvas makes.
    fn gesture(h: &mut Harness<'_, Studio>, handle: Option<Handle>, from: P, to: P, m: DragMods) {
        h.state_mut().begin_drag(handle, from);
        h.state_mut().update_drag(to, m);
        h.state_mut().end_drag();
        h.run_steps(2);
    }

    fn select(h: &mut Harness<'_, Studio>, name: &str) -> Bounds {
        let i = index_of(h, name);
        h.state_mut().set_selection(&[i]);
        h.run_steps(1);
        h.state().selection_bounds()
    }

    #[test]
    fn corner_handle_scales_freely_and_shift_keeps_proportions() {
        let mut h = studio();
        let b = select(&mut h, "sun");
        let grab = b.max;
        gesture(
            &mut h,
            Some(Handle::Scale(1, 1)),
            grab,
            grab + P::new(b.width() * 0.5, 0.0),
            mods(false, false, false),
        );
        let b2 = h.state().selection_bounds();
        assert_eq!(
            h.state().undo.len(),
            1,
            "one step, even though the fill angle shears"
        );
        assert!((b2.width() / b.width() - 1.5).abs() < 0.02, "wider");
        assert!((b2.height() / b.height() - 1.0).abs() < 0.02, "same height");
        assert!(
            (b2.min.x - b.min.x).abs() < 0.5,
            "opposite corner stays put"
        );

        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Z);
        h.run_steps(2);
        gesture(
            &mut h,
            Some(Handle::Scale(1, 1)),
            grab,
            grab + P::new(b.width() * 0.5, 0.0),
            mods(true, false, false),
        );
        let b3 = h.state().selection_bounds();
        let (kx, ky) = (b3.width() / b.width(), b3.height() / b.height());
        assert!(
            kx > 1.1 && (kx - ky).abs() < 0.02,
            "Shift: proportional {kx} {ky}"
        );
    }

    #[test]
    fn option_scales_from_the_center() {
        let mut h = studio();
        let b = select(&mut h, "sun");
        gesture(
            &mut h,
            Some(Handle::Scale(1, 0)),
            P::new(b.max.x, b.center().y),
            P::new(b.max.x + b.width() * 0.5, b.center().y),
            mods(false, true, false),
        );
        let b2 = h.state().selection_bounds();
        assert!(
            (b2.center().x - b.center().x).abs() < 0.5,
            "center stays put"
        );
        assert!(
            (b2.width() / b.width() - 2.0).abs() < 0.02,
            "both sides moved"
        );
    }

    #[test]
    fn option_drag_leaves_a_copy_and_transform_again_repeats_it() {
        let mut h = studio();
        let n = h.state().doc.shapes.len();
        let b = select(&mut h, "sun");
        let c = b.center();
        gesture(
            &mut h,
            None,
            c,
            c + P::new(120.0, 0.0),
            mods(false, true, false),
        );
        assert_eq!(h.state().doc.shapes.len(), n + 1, "a copy");
        let orig = index_of(&h, "sun");
        let ob = shape_bounds(&h.state().doc.shapes[orig]);
        assert!(
            (ob.center().x - c.x).abs() < 0.5,
            "the original didn't move"
        );
        let copy = h.state().sel_index().unwrap();
        assert_ne!(copy, orig);
        let cb = shape_bounds(&h.state().doc.shapes[copy]);
        assert!((cb.center().x - c.x - 120.0).abs() < 0.5);
        assert_eq!(h.state().undo.len(), 1);

        // ⌘D: another copy, the same distance on.
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::D);
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n + 2);
        let third = shape_bounds(&h.state().doc.shapes[h.state().sel_index().unwrap()]);
        assert!(
            (third.center().x - c.x - 240.0).abs() < 0.5,
            "{}",
            third.center().x - c.x
        );

        // After a plain transform, ⌘D repeats it.
        h.state_mut()
            .transform_selected(zigzag::geom::rotate_about(third.center(), 30.0), "Rotated");
        h.run_steps(1);
        let before = h.state().selection_bounds();
        let steps = h.state().undo.len();
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::D);
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n + 2, "no new copy");
        assert_eq!(h.state().undo.len(), steps + 1, "{}", h.state().status);
        assert!(h.state().status.starts_with("Transformed again"));
        let after = h.state().selection_bounds();
        assert!(
            (after.center() - before.center()).len() < 0.5,
            "rotated in place again"
        );
    }

    #[test]
    fn shift_constrains_moves_to_45_degrees() {
        let mut h = studio();
        let b = select(&mut h, "sun");
        let c = b.center();
        gesture(
            &mut h,
            None,
            c,
            c + P::new(100.0, 12.0),
            mods(true, false, false),
        );
        let b2 = h.state().selection_bounds();
        assert!((b2.center().y - c.y).abs() < 0.01, "level");
        gesture(
            &mut h,
            None,
            b2.center(),
            b2.center() + P::new(50.0, 46.0),
            mods(true, false, false),
        );
        let b3 = h.state().selection_bounds();
        let d = b3.center() - b2.center();
        assert!((d.x - d.y).abs() < 0.01 && d.x > 40.0, "diagonal {d:?}");
    }

    #[test]
    fn smart_guides_snap_to_the_hoop_and_other_objects() {
        let mut h = studio();
        let b = select(&mut h, "sun");
        let c = b.center();
        // Drop the center 0.7 mm right of the hoop's center line: it snaps.
        let target = P::new(7.0, c.y);
        h.state_mut().begin_drag(None, c);
        h.state_mut().update_drag(target, mods(false, false, true));
        assert!(
            !h.state().drag.as_ref().unwrap().guides.is_empty(),
            "a guide shows"
        );
        h.state_mut().end_drag();
        h.run_steps(2);
        let snapped = h.state().selection_bounds().center().x;
        assert!(
            snapped.abs() < 1e-3,
            "snapped to the hoop center: {snapped}"
        );

        // With smart guides off, it lands where you let go.
        let b = h.state().selection_bounds();
        gesture(
            &mut h,
            None,
            b.center(),
            b.center() + P::new(7.0, 0.0),
            mods(false, false, false),
        );
        assert!((h.state().selection_bounds().center().x - 7.0).abs() < 1e-3);
    }

    #[test]
    fn rotate_zone_sits_just_outside_the_corners() {
        let mut h = studio();
        select(&mut h, "sun");
        let rect = h.state().canvas_rect;
        let r = h.state().selection_screen_rect(rect).unwrap();
        assert_eq!(
            h.state()
                .handle_at(rect, r.right_top() + Vec2::new(12.0, -10.0)),
            Some(Handle::Rotate)
        );
        assert_eq!(
            h.state().handle_at(rect, r.right_top()),
            Some(Handle::Scale(1, -1))
        );
        assert_eq!(h.state().handle_at(rect, r.center()), None);
        // Shift snaps rotation to 45°.
        let b = h.state().selection_bounds();
        let c = b.center();
        let from = c + P::new(100.0, 0.0);
        let to = c + P::new(100.0, 80.0); // ~38.7°
        h.state_mut().begin_drag(Some(Handle::Rotate), from);
        h.state_mut().update_drag(to, mods(true, false, false));
        let m = h.state().drag.as_ref().unwrap().m;
        assert!((m[1].atan2(m[0]).to_degrees() - 45.0).abs() < 1e-6);
        h.state_mut().end_drag();
    }

    #[test]
    fn pathfinder_buttons_and_offset_path() {
        let mut h = studio();
        let n = h.state().doc.shapes.len();
        let sun = index_of(&h, "sun");
        let disc = index_of(&h, "disc");
        h.state_mut().set_selection(&[sun, disc]);
        h.run_steps(2);
        h.get_by_label("Minus Front").click();
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n - 1, "{}", h.state().status);
        assert_eq!(h.state().sel_indices().len(), 1);
        assert_eq!(h.state().undo.len(), 1);
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Z);
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n);

        // Offset Path: a window for the distance, then a new shape.
        let sun = index_of(&h, "sun");
        h.state_mut().set_selection(&[sun]);
        h.state_mut()
            .run_action(crate::commands::Action::OffsetPath);
        h.run_steps(2);
        h.get_by_label("OK").click();
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n + 1, "{}", h.state().status);
        assert!(h.state().layout.offset_dialog.is_none());
    }

    #[test]
    fn reference_point_sets_what_x_y_mean_and_what_stays_put() {
        let b = Bounds {
            min: P::new(-100.0, -50.0),
            max: P::new(100.0, 50.0),
        };
        assert_eq!(reference_point(&b, (-1, -1)), P::new(-100.0, -50.0));
        assert_eq!(reference_point(&b, (0, 0)), P::new(0.0, 0.0));
        // Twice as wide about the top-left: the top-left stays.
        let m = panel_transform(&b, (-1, -1), P::new(-100.0, -50.0), 400.0, 100.0);
        let tl = zigzag::geom::affine_apply(m, b.min);
        let br = zigzag::geom::affine_apply(m, b.max);
        assert!((tl - b.min).len() < 1e-9);
        assert!((br - P::new(300.0, 50.0)).len() < 1e-9);
        // Moving the reference point moves the box.
        let m = panel_transform(&b, (1, 1), P::new(0.0, 0.0), 200.0, 100.0);
        assert!((zigzag::geom::affine_apply(m, b.max)).len() < 1e-9);
    }

    #[test]
    fn space_drag_pans_and_a_tap_plays() {
        let mut h = studio();
        let c0 = h.state().view.center;
        let mid = h.state().canvas_rect.center();
        h.key_down(Key::Space);
        h.run_steps(1);
        drag(&mut h, mid, mid + Vec2::new(80.0, 0.0));
        h.key_up(Key::Space);
        h.run_steps(2);
        assert!((h.state().view.center - c0).len() > 1.0, "panned");
        assert!(!h.state().playing, "a pan doesn't play");
        assert_eq!(h.state().undo.len(), 0, "nothing moved");
        h.key_down(Key::Space);
        h.run_steps(1);
        h.key_up(Key::Space);
        h.run_steps(2);
        assert!(h.state().playing, "a tap plays");
    }

    #[test]
    fn corner_handle_drag_on_canvas() {
        let mut h = studio();
        let sun = index_of(&h, "sun");
        h.state_mut().select_index(Some(sun));
        h.run_steps(2);
        let b = shape_bounds(&h.state().doc.shapes[sun]);
        let r = h
            .state()
            .selection_screen_rect(h.state().canvas_rect)
            .unwrap();
        drag(
            &mut h,
            r.right_bottom(),
            r.right_bottom() + (r.right_bottom() - r.left_top()) * 0.5,
        );
        let sun = index_of(&h, "sun");
        let b2 = shape_bounds(&h.state().doc.shapes[sun]);
        let (kx, ky) = (b2.width() / b.width(), b2.height() / b.height());
        assert!(kx > 1.3 && ky > 1.3, "{kx} {ky}");
        assert!(
            (b2.min.x - b.min.x).abs() < 1.0,
            "top-left corner stays put"
        );
    }

    #[test]
    fn duplicate_copy_paste_and_nudge() {
        let mut h = studio();
        let n = h.state().doc.shapes.len();
        let sun = index_of(&h, "sun");
        h.state_mut().select_index(Some(sun));
        h.run_steps(1);
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::D);
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n + 1);
        let copy = h.state().sel_index().unwrap();
        assert_eq!(h.state().names[copy], "sun (2)");
        let before = shape_bounds(&h.state().doc.shapes[copy]).center();
        h.key_press(Key::ArrowRight);
        h.run_steps(3);
        let copy = h.state().sel_index().unwrap();
        let after = shape_bounds(&h.state().doc.shapes[copy]).center();
        assert!((after.x - before.x - 5.0).abs() < 0.2, "nudged 0.5 mm");

        h.state_mut().copy_selected();
        h.state_mut().paste();
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n + 2);
    }

    #[test]
    fn a_copy_goes_on_the_system_clipboard_so_the_os_sends_a_paste() {
        let mut h = studio();
        let n = h.state().doc.shapes.len();
        let sun = index_of(&h, "sun");
        h.state_mut().select_index(Some(sun));
        h.run_steps(1);
        h.state_mut().copy_selected();
        let svg = h.state().layout.system_clipboard.clone().expect("waiting");
        let copied = zigzag::document::Document::from_svg(&svg, "clip", None).unwrap();
        assert_eq!(copied.shapes.len(), 1, "the clipboard SVG holds the copy");
        h.run_steps(1);
        assert!(
            h.state().layout.system_clipboard.is_none(),
            "handed to egui"
        );
        // With an image on the OS clipboard, Cmd+V arrives as an image paste.
        let image = egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]);
        h.event(egui::Event::PasteImage(std::sync::Arc::new(image)));
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n + 1);
    }

    #[test]
    fn shift_click_multi_select_and_mirror_buttons() {
        let mut h = studio();
        let sun = index_of(&h, "sun");
        let swoosh = index_of(&h, "swoosh");
        h.state_mut().set_selection(&[sun, swoosh]);
        h.run_steps(2);
        assert_eq!(h.state().sel_indices().len(), 2);
        let b = h.state().selection_bounds();
        h.get_by_label("Flip Horizontal").click();
        h.run_steps(3);
        let b2 = h.state().selection_bounds();
        assert!((b2.center().x - b.center().x).abs() < 1.0 && (b2.width() - b.width()).abs() < 1.0);
        assert_eq!(
            h.state().sel_indices().len(),
            2,
            "selection survives the edit"
        );
        assert_eq!(h.state().undo.len(), 1);
    }
}
