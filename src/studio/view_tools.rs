//! Canvas helpers: mm grid, needle points, measuring, the pen tool for drawing
//! shapes and manual stitches, sewing-order tools and the worksheet.

use crate::app::{HOOPS, SPM, Studio, Tool};
use crate::canvas::View;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use zigzag::design::Design;
use zigzag::geom::P;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PenMode {
    /// Closed → filled shape, open → running line.
    #[default]
    Shape,
    /// Every click is a needle point.
    Manual,
}

/// An object's start or end point (see `Params::start`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    Start,
    End,
}

pub struct ViewTools {
    pub grid: bool,
    /// View › Density Map and View › Sewing Order.
    pub density: bool,
    pub order_labels: bool,
    /// The next click on the canvas places this marker on the selected
    /// object (the Stitch panel's Start and End buttons).
    pub marker_pick: Option<Marker>,
    /// A marker being dragged along the selected object's outline.
    pub marker_drag: Option<Marker>,
    /// Snap to other objects and the hoop while dragging (⌘U).
    pub smart_guides: bool,
    pub points: bool,
    /// View › Draw Stitches on GPU (off: egui's painter draws them).
    pub gpu_stitches: bool,
    pub measure: Option<(P, P)>,
    /// A Measure drag is under way.
    pub measuring: bool,
    /// The path being drawn with the pen.
    pub pen: Vec<zigzag::bezier::Anchor>,
    /// What the pen's mouse press is doing.
    pub pen_press: Option<crate::pen::PenPress>,
    /// The path being drawn when the press began, and where (a segment
    /// being bent is reshaped from these).
    pub pen_base: Vec<zigzag::bezier::Anchor>,
    pub pen_press_at: P,
    /// The press has moved far enough to be a drag.
    pub pen_moved: bool,
    /// Pointer position last frame (for Space-moving a point).
    pub pen_pointer: Option<P>,
    /// Continuing a selected path: (object uid, subpath).
    pub pen_continue: Option<(usize, usize)>,
    pub pen_mode: PenMode,
    /// Which tool of the Pen's group the tool strip shows (Pen or Anchor Point).
    pub pen_slot: Tool,
    /// Which tool of the Brush's group the tool strip shows.
    pub brush_slot: Tool,
    pub stitch_view: crate::canvas::StitchView,
    /// Thread chart shown in the Threads panel.
    pub brand: crate::threads::Brand,
    /// The Threads panel lists threads in sewing order (else by color).
    pub thread_order: bool,
}

impl Default for ViewTools {
    fn default() -> Self {
        ViewTools {
            grid: false,
            density: false,
            order_labels: false,
            marker_pick: None,
            marker_drag: None,
            smart_guides: true,
            points: false,
            gpu_stitches: true,
            measure: None,
            measuring: false,
            pen: Vec::new(),
            pen_press: None,
            pen_base: Vec::new(),
            pen_press_at: P::default(),
            pen_moved: false,
            pen_pointer: None,
            pen_continue: None,
            pen_mode: PenMode::default(),
            pen_slot: Tool::Pen,
            brush_slot: Tool::Brush,
            stitch_view: Default::default(),
            brand: Default::default(),
            thread_order: true,
        }
    }
}

/// The grid at this zoom, in design units: its minor step and its
/// labelled major step, at nice steps of the current unit (see
/// [`crate::units::Unit::grid`]).
pub fn grid_steps(view: &View) -> (f64, f64) {
    let u = crate::units::current();
    let (minor, major) = u.grid(view.scale as f64);
    (u.to_units(minor), u.to_units(major))
}

/// Grid lines every minor step, stronger every major one (in the current
/// unit, from the hoop's center). Their labels go on top of the design
/// ([`draw_grid_labels`]).
pub fn draw_grid(painter: &egui::Painter, rect: Rect, view: &View) {
    let (step, major) = grid_steps(view);
    let minor_c = Color32::from_black_alpha(22);
    let major_c = Color32::from_black_alpha(50);
    for (v, s, horizontal) in grid_lines(rect, view, step) {
        let c = if on_step(v, major) { major_c } else { minor_c };
        let ends = if horizontal {
            [Pos2::new(rect.left(), s), Pos2::new(rect.right(), s)]
        } else {
            [Pos2::new(s, rect.top()), Pos2::new(s, rect.bottom())]
        };
        painter.line_segment(ends, Stroke::new(1.0, c));
    }
}

/// The major grid lines' values along the canvas's top and left edges,
/// and the unit in the corner where they meet.
pub fn draw_grid_labels(painter: &egui::Painter, rect: Rect, view: &View) {
    let u = crate::units::current();
    let major = grid_steps(view).1;
    let p = crate::theme::pal(painter.ctx());
    let font = egui::FontId::proportional(10.0);
    let tag = |at: Pos2, text: String| {
        let g = painter.layout_no_wrap(text, font.clone(), p.secondary);
        let r = Rect::from_min_size(at, g.size() + Vec2::new(6.0, 2.0));
        painter.rect_filled(r, 3.0, p.toolbar.gamma_multiply(0.85));
        painter.galley(r.min + Vec2::new(3.0, 1.0), g, p.secondary);
    };
    for (v, s, horizontal) in grid_lines(rect, view, major) {
        if horizontal && s > rect.top() + 18.0 {
            tag(Pos2::new(rect.left() + 2.0, s + 2.0), u.number(u.amount(v)));
        } else if !horizontal && s > rect.left() + 24.0 {
            tag(Pos2::new(s + 2.0, rect.top() + 2.0), u.number(u.amount(v)));
        }
    }
    tag(
        rect.left_top() + Vec2::new(2.0, 2.0),
        u.suffix().to_string(),
    );
}

/// Is `v` a whole number of `step`s?
fn on_step(v: f64, step: f64) -> bool {
    let k = v / step;
    (k - k.round()).abs() < 1e-6
}

/// The grid lines `step` apart across `rect`: (design value, screen
/// coordinate, horizontal). Counted from zero, so they fall on whole steps.
fn grid_lines(rect: Rect, view: &View, step: f64) -> Vec<(f64, f32, bool)> {
    let a = view.to_design(rect, rect.left_top());
    let b = view.to_design(rect, rect.right_bottom());
    let mut out = Vec::new();
    let mut k = (a.x / step).floor() as i64;
    while k as f64 * step <= b.x {
        let x = k as f64 * step;
        out.push((x, view.to_screen(rect, P::new(x, 0.0)).x, false));
        k += 1;
    }
    let mut k = (a.y / step).floor() as i64;
    while k as f64 * step <= b.y {
        let y = k as f64 * step;
        out.push((y, view.to_screen(rect, P::new(0.0, y)).y, true));
        k += 1;
    }
    out
}

/// Needle penetration points (only drawn when zoomed in enough to see them).
pub fn draw_points(painter: &egui::Painter, rect: Rect, view: &View, d: &Design, limit: usize) {
    let r = (view.scale * 0.12).clamp(1.0, 2.5);
    let mut left = limit;
    for blk in &d.blocks {
        let c = Color32::from_rgb(
            255 - blk.color[0] / 2,
            255 - blk.color[1] / 2,
            255 - blk.color[2] / 2,
        );
        for run in &blk.runs {
            for &p in run.pts.iter().take(left) {
                let s = view.to_screen(rect, p);
                if rect.contains(s) {
                    painter.circle_filled(s, r, c);
                }
            }
            left = left.saturating_sub(run.pts.len());
            if left == 0 {
                return;
            }
        }
    }
}

pub fn label(painter: &egui::Painter, at: Pos2, text: String) {
    let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), Color32::WHITE);
    let bg = Rect::from_min_size(at, galley.size() + Vec2::new(10.0, 6.0));
    painter.rect_filled(bg, 4.0, Color32::from_black_alpha(190));
    painter.galley(bg.min + Vec2::new(5.0, 3.0), galley, Color32::WHITE);
}

/// The start marker's green, the end marker's red.
fn marker_color(m: Marker, p: &crate::theme::Pal) -> Color32 {
    match m {
        Marker::Start => Color32::from_rgb(52, 199, 89),
        Marker::End => p.error,
    }
}

impl Studio {
    /// Where the selected object's markers are on screen.
    fn marker_positions(&self, rect: Rect) -> Vec<(Marker, Pos2)> {
        let Some(i) = self.sel_index() else {
            return Vec::new();
        };
        let s = &self.doc.shapes[i];
        [(Marker::Start, s.params.start), (Marker::End, s.params.end)]
            .into_iter()
            .filter_map(|(m, t)| {
                let p = zigzag::design::outline_at(&s.geometry, t?)?;
                Some((m, self.view.to_screen(rect, p)))
            })
            .collect()
    }

    /// The selected object's start and end markers: placed by a click after
    /// the Stitch panel's Start or End button, and dragged along the
    /// object's outline. True while the pointer is busy with one (the
    /// Select tool then leaves it alone).
    pub fn markers_canvas(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        rect: Rect,
    ) -> bool {
        let Some(i) = self.sel_index() else {
            self.view_tools.marker_pick = None;
            self.view_tools.marker_drag = None;
            return false;
        };
        let view = self.view;
        let at = |pos: Option<Pos2>, s: &zigzag::svg::Shape| {
            zigzag::design::outline_param(&s.geometry, view.to_design(rect, pos?))
        };
        if let Some(m) = self.view_tools.marker_pick {
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.view_tools.marker_pick = None;
                self.status = "Cancelled.".into();
                return true;
            }
            self.want_cursor(crate::cursors::Glyph::Cross(None));
            if resp.clicked() || resp.drag_started_by(egui::PointerButton::Primary) {
                let pos = ctx
                    .input(|i| i.pointer.press_origin())
                    .or(resp.interact_pointer_pos());
                let t = at(pos, &self.doc.shapes[i]);
                let pr = &mut self.doc.shapes[i].params;
                match m {
                    Marker::Start => pr.start = t,
                    Marker::End => pr.end = t,
                }
                self.view_tools.marker_pick = None;
                self.status = match m {
                    Marker::Start => "Sewing starts there. Drag the marker to move it.".into(),
                    Marker::End => "Sewing ends there. Drag the marker to move it.".into(),
                };
            }
            return true;
        }
        if self.tool != Tool::Select {
            return false;
        }
        let positions = self.marker_positions(rect);
        let hit = |pos: Option<Pos2>| {
            let pos = pos?;
            positions
                .iter()
                .find(|(_, s)| s.distance(pos) <= 8.0)
                .map(|(m, _)| *m)
        };
        if resp.drag_started_by(egui::PointerButton::Primary)
            && let Some(m) = hit(ctx.input(|i| i.pointer.press_origin()))
        {
            self.view_tools.marker_drag = Some(m);
        }
        if let Some(m) = self.view_tools.marker_drag {
            if resp.dragged_by(egui::PointerButton::Primary) {
                let t = at(resp.interact_pointer_pos(), &self.doc.shapes[i]);
                let pr = &mut self.doc.shapes[i].params;
                match m {
                    Marker::Start => pr.start = t.or(pr.start),
                    Marker::End => pr.end = t.or(pr.end),
                }
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                ctx.request_repaint();
            }
            if resp.drag_stopped() {
                self.view_tools.marker_drag = None;
            }
            return true;
        }
        if hit(resp.hover_pos()).is_some() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
            return true;
        }
        false
    }

    /// Draw the selected object's start and end markers (and, while one is
    /// being placed, where it would land).
    pub fn draw_markers(&self, painter: &egui::Painter, rect: Rect) {
        let p = crate::theme::pal(painter.ctx());
        let dot = |m: Marker, s: Pos2, ghost: bool| {
            let c = marker_color(m, &p);
            let c = if ghost { c.gamma_multiply(0.5) } else { c };
            painter.circle_filled(s, 5.5, Color32::WHITE);
            painter.circle_filled(s, 4.0, c);
            if m == Marker::End {
                // A square inside: "stop".
                painter.rect_filled(
                    Rect::from_center_size(s, Vec2::splat(3.0)),
                    0.0,
                    Color32::WHITE,
                );
            }
        };
        for (m, s) in self.marker_positions(rect) {
            dot(m, s, false);
        }
        if let Some(m) = self.view_tools.marker_pick
            && let Some(i) = self.sel_index()
            && let Some(pos) = painter.ctx().pointer_latest_pos()
            && rect.contains(pos)
        {
            let g = &self.doc.shapes[i].geometry;
            if let Some(t) = zigzag::design::outline_param(g, self.view.to_design(rect, pos))
                && let Some(q) = zigzag::design::outline_at(g, t)
            {
                dot(m, self.view.to_screen(rect, q), true);
                painter.ctx().request_repaint();
            }
        }
    }
}

/// What a measurement reads: distance, angle (degrees, counter-clockwise
/// from the right, as on a protractor) and the horizontal and vertical
/// distances, all in the current unit.
pub fn measure_text(a: P, b: P) -> String {
    let d = b - a;
    let u = crate::units::current();
    let ang = (-d.y).atan2(d.x).to_degrees() + 0.0;
    format!(
        "D  {}\nAngle  {:.1}°\nW  {}    H  {}",
        u.fmt(d.len()),
        ang,
        u.fmt(d.x.abs()),
        u.fmt(d.y.abs())
    )
}

impl Studio {
    /// The Measure tool (I), as VectorCraft's: drag between two points to
    /// read the distance, angle, width and height. The ends snap to points,
    /// edges and the hoop (smart guides), or to the grid when it's showing;
    /// Shift keeps the line to 45° steps. The reading stays until the next
    /// drag (Escape clears it); the document isn't changed.
    fn measure_canvas(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
    ) {
        // Cyan, as VectorCraft's: apart from the fill angle's yellow arrow.
        let accent = Color32::from_rgb(40, 200, 255);
        self.want_cursor(crate::cursors::Glyph::Cross(Some(
            crate::icons::Icon::Measure,
        )));
        let dragging = resp.dragged_by(egui::PointerButton::Primary);
        self.snap_targets(None, dragging);
        let shift = ui.input(|i| i.modifiers.shift);
        if resp.drag_started_by(egui::PointerButton::Primary)
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            let a = self.snap_point(self.view.to_design(rect, p), None, false, &[]);
            self.view_tools.measure = Some((a, a));
            self.view_tools.measuring = true;
        }
        if dragging
            && self.view_tools.measuring
            && let (Some(p), Some((a, _))) = (resp.interact_pointer_pos(), self.view_tools.measure)
        {
            let b = self.snap_point(self.view.to_design(rect, p), Some(a), shift, &[]);
            self.view_tools.measure = Some((a, b));
        } else if !dragging {
            self.view_tools.measuring = false;
            // Show where a measurement would start.
            if let Some(p) = resp.hover_pos() {
                self.snap_point(self.view.to_design(rect, p), None, false, &[]);
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.view_tools.measure = None;
        }
        let Some((a, b)) = self.view_tools.measure else {
            return;
        };
        if (b - a).len() < 1e-9 {
            return;
        }
        let (sa, sb) = (self.view.to_screen(rect, a), self.view.to_screen(rect, b));
        let halo = Color32::from_black_alpha(90);
        painter.line_segment([sa, sb], Stroke::new(3.5, halo));
        painter.line_segment([sa, sb], Stroke::new(1.5, accent));
        for s in [sa, sb] {
            painter.circle_filled(s, 3.5, accent);
            painter.circle_stroke(s, 3.5, Stroke::new(1.0, halo));
        }
        // The reading beside the far end, kept on the canvas.
        let text = measure_text(a, b);
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), Color32::WHITE);
        let size = galley.size() + Vec2::new(12.0, 8.0);
        let mut at = sb + Vec2::new(12.0, 12.0);
        at.x = at.x.min(rect.right() - size.x - 4.0).max(rect.left() + 4.0);
        at.y = at.y.min(rect.bottom() - size.y - 4.0).max(rect.top() + 4.0);
        let bg = Rect::from_min_size(at, size);
        painter.rect_filled(bg, 5.0, Color32::from_black_alpha(200));
        painter.galley(bg.min + Vec2::new(6.0, 4.0), galley, Color32::WHITE);
    }

    /// Canvas input and drawing for the measure and pen tools.
    pub fn view_tools_canvas(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
    ) {
        match self.tool {
            Tool::Measure => self.measure_canvas(ui, resp, painter, rect),
            Tool::Text => self.type_canvas(ui, resp, rect),
            Tool::Shape => self.shape_canvas(ui, resp, painter, rect),
            Tool::Brush => self.brush_canvas(ui, resp, painter, rect),
            Tool::Smooth | Tool::PathEraser | Tool::Join => {
                self.cleanup_canvas(ui, resp, painter, rect)
            }
            Tool::Pen => {
                self.pen_canvas(ui, resp, painter, rect);
                // Pen options float over the canvas, like a control bar.
                let mut mode = self.view_tools.pen_mode;
                egui::Area::new(ui.make_persistent_id("pen-options"))
                    .pivot(egui::Align2::CENTER_TOP)
                    .fixed_pos(rect.center_top() + Vec2::new(0.0, crate::theme::space::M))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style())
                            .corner_radius(9)
                            .inner_margin(egui::Margin::same(crate::theme::space::XS as i8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                                    for (m, icon, name) in [
                                        (
                                            PenMode::Shape,
                                            crate::icons::Icon::PenPath,
                                            "Shape or Line",
                                        ),
                                        (
                                            PenMode::Manual,
                                            crate::icons::Icon::NeedlePoints,
                                            "Manual Stitches",
                                        ),
                                    ] {
                                        if crate::theme::icon_button(
                                            ui,
                                            icon,
                                            name,
                                            mode == m,
                                            26.0,
                                        )
                                        .clicked()
                                        {
                                            mode = m;
                                        }
                                    }
                                });
                            });
                    });
                self.view_tools.pen_mode = mode;
            }
            _ => {}
        }
    }

    /// Reorder to sew each color in as few passes as layering allows.
    pub fn group_colors(&mut self) {
        let before = self.design.blocks.len();
        let order = zigzag::design::grouped_by_color(&self.doc.shapes);
        if order.iter().enumerate().any(|(k, &i)| k != i) {
            // An edit like any other: one undo step, and unsaved.
            let _ = self.structural(|d| {
                let shapes = std::mem::take(&mut d.shapes);
                let mut slots: Vec<Option<zigzag::svg::Shape>> =
                    shapes.into_iter().map(Some).collect();
                d.shapes = order
                    .iter()
                    .map(|&i| slots[i].take().expect("permutation"))
                    .collect();
                Ok(())
            });
        }
        self.replan();
        let after = self.design.blocks.len();
        self.status = if after < before {
            format!(
                "Color changes: {} → {}.",
                before.saturating_sub(1),
                after.saturating_sub(1)
            )
        } else {
            "Already in the fewest color changes the layering allows.".into()
        };
    }

    pub fn move_selected_to(&mut self, first: bool) {
        let mut idx = self.sel_indices();
        if idx.is_empty() {
            return;
        }
        idx.sort_unstable();
        let uids: Vec<usize> = idx.iter().map(|&i| self.doc.shapes[i].uid).collect();
        let _ = self.structural(|d| {
            let mut moving = Vec::new();
            for &i in idx.iter().rev() {
                moving.push(d.shapes.remove(i));
            }
            moving.reverse();
            if first {
                d.shapes.splice(0..0, moving);
            } else {
                d.shapes.extend(moving);
            }
            Ok(())
        });
        let new: Vec<usize> = uids
            .iter()
            .filter_map(|u| self.doc.shapes.iter().position(|s| s.uid == *u))
            .collect();
        self.set_selection(&new);
    }

    pub fn worksheet_html(&self) -> anyhow::Result<String> {
        let h = HOOPS[self.doc.settings.hoop.min(HOOPS.len() - 1)];
        let info = zigzag::formats::worksheet::WorksheetInfo {
            title: if self.doc.name.is_empty() {
                "Design".into()
            } else {
                self.doc.name.clone()
            },
            fabric: self.doc.settings.fabric().name.to_string(),
            hoop: Some((h.1.to_string(), h.2, h.3)),
            background: self.doc.settings.cloth,
            names: self.names.clone(),
            spm: SPM,
        };
        zigzag::formats::worksheet::html(&self.design, &info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Key;
    use egui_kittest::Harness;
    use std::path::PathBuf;
    use zigzag::params::Kind;

    fn studio(file: Option<&str>) -> Harness<'static, Studio> {
        let path =
            file.map(|f| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/")).join(f));
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 860.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(path, None),
            );
        h.run_steps(4);
        h
    }

    fn click_at(h: &mut Harness<'_, Studio>, at: Pos2) {
        h.hover_at(at);
        h.run_steps(1);
        h.drag_at(at);
        h.run_steps(1);
        h.drop_at(at);
        h.run_steps(2);
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
        h.run_steps(2);
    }

    #[test]
    fn measure_reads_distance_angle_and_size_in_the_unit() {
        let mut h = studio(None);
        h.state_mut().view_tools.smart_guides = false;
        h.key_press(Key::I);
        h.run_steps(1);
        assert_eq!(h.state().tool, Tool::Measure);
        let c = h.state().canvas_rect.center();
        let s = h.state().view.scale; // points per mm
        drag(&mut h, c, c + Vec2::new(30.0 * s, -40.0 * s));
        let (a, b) = h.state().view_tools.measure.expect("measured");
        assert!(((b - a).len() - 500.0).abs() < 1.0, "{a:?} {b:?}");
        let text = measure_text(a, b);
        assert!(text.starts_with("D  50 mm"), "{text}");
        assert!(text.contains("Angle  53.1°"), "{text}");
        assert!(text.contains("W  30 mm    H  40 mm"), "{text}");
        // Shown in inches once they're chosen (and nothing in the document changed).
        h.state_mut()
            .run_action(crate::commands::Action::Units(crate::units::Unit::In));
        h.run_steps(1);
        assert!(measure_text(a, b).starts_with("D  1.969 in"));
        assert!(h.state().doc.shapes.is_empty());
        assert!(!h.state().modified);
        // It stays after the drag; Escape clears it.
        h.run_steps(2);
        assert!(h.state().view_tools.measure.is_some());
        h.key_press(Key::Escape);
        h.run_steps(1);
        assert!(h.state().view_tools.measure.is_none());
    }

    #[test]
    fn measure_snaps_to_the_grid_and_shift_keeps_it_level() {
        let mut h = studio(None);
        h.state_mut().view_tools.smart_guides = false;
        h.state_mut().view_tools.grid = true;
        h.state_mut()
            .run_action(crate::commands::Action::Units(crate::units::Unit::In));
        h.state_mut().set_tool(Tool::Measure);
        h.run_steps(1);
        let c = h.state().canvas_rect.center();
        drag(&mut h, c + Vec2::new(3.0, 2.0), c + Vec2::new(97.0, 41.0));
        let (a, b) = h.state().view_tools.measure.unwrap();
        let step = grid_steps(&h.state().view).0;
        for v in [a.x, a.y, b.x, b.y] {
            let k = v / step;
            assert!((k - k.round()).abs() < 1e-6, "{v} on a {step} grid");
        }
        // Shift: level, however the pointer wanders.
        h.state_mut().view_tools.grid = false;
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
        drag(&mut h, c, c + Vec2::new(120.0, 9.0));
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        let (a, b) = h.state().view_tools.measure.unwrap();
        assert!((a.y - b.y).abs() < 1e-6 && b.x > a.x, "{a:?} {b:?}");
    }

    #[test]
    fn grid_lines_fall_on_the_units_steps() {
        let view = crate::canvas::View {
            center: P::default(),
            scale: 8.0,
        };
        crate::units::set_current(crate::units::Unit::Mm);
        assert_eq!(grid_steps(&view), (10.0, 100.0)); // 1 mm, labels each cm
        crate::units::set_current(crate::units::Unit::In);
        let (minor, major) = grid_steps(&view);
        assert!((minor - 254.0 / 16.0).abs() < 1e-9 && (major - 127.0).abs() < 1e-9);
        crate::units::set_current(crate::units::Unit::Mm);
    }

    #[test]
    fn pen_tool_draws_a_filled_shape_and_a_line() {
        let mut h = studio(None);
        h.state_mut().tool = Tool::Pen;
        h.run_steps(1);
        let rect = h.state().canvas_rect;
        let c = rect.center();
        for p in [c, c + Vec2::new(80.0, 0.0), c + Vec2::new(40.0, 70.0), c] {
            h.hover_at(p);
            h.run_steps(1);
            h.drag_at(p);
            h.run_steps(1);
            h.drop_at(p);
            h.run_steps(2);
        }
        assert_eq!(
            h.state().doc.shapes.len(),
            1,
            "clicking the first point closes the shape"
        );
        assert_eq!(h.state().doc.shapes[0].params.kind, Some(Kind::Fill));
        assert!(h.state().design.stitch_count() > 50);

        for p in [c + Vec2::new(0.0, -100.0), c + Vec2::new(120.0, -100.0)] {
            h.hover_at(p);
            h.run_steps(1);
            h.drag_at(p);
            h.run_steps(1);
            h.drop_at(p);
            h.run_steps(2);
        }
        h.key_press(Key::Enter);
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), 2);
        assert_eq!(h.state().doc.shapes[1].params.kind, Some(Kind::Run));
        assert_eq!(h.state().undo.len(), 2);
    }

    #[test]
    fn command_y_switches_to_stitch_lines() {
        let mut h = studio(Some("badge.svg"));
        assert_eq!(
            h.state().view_tools.stitch_view,
            crate::canvas::StitchView::Thread
        );
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Y);
        h.run_steps(2);
        assert_eq!(
            h.state().view_tools.stitch_view,
            crate::canvas::StitchView::Lines
        );
    }

    #[test]
    fn hand_tool_pans_without_selecting() {
        let mut h = studio(Some("badge.svg"));
        h.state_mut().tool = Tool::Hand;
        h.run_steps(1);
        let before = h.state().view.center;
        let c = h.state().canvas_rect.center();
        h.hover_at(c);
        h.run_steps(1);
        h.drag_at(c);
        h.run_steps(1);
        for k in 1..=3 {
            h.hover_at(c + Vec2::new(30.0 * k as f32, 0.0));
            h.run_steps(1);
        }
        h.drop_at(c + Vec2::new(90.0, 0.0));
        h.run_steps(2);
        assert!(h.state().view.center.x < before.x - 1.0, "view moved");
        assert!(h.state().undo.is_empty(), "nothing was edited");
    }

    #[test]
    fn start_and_end_markers_are_placed_and_dragged() {
        use egui_kittest::kittest::Queryable;
        let mut h = studio(Some("badge.svg"));
        let sun = h.state().names.iter().position(|n| n == "sun").unwrap();
        h.state_mut().set_selection(&[sun]);
        h.run_steps(2);
        // The Stitch panel's Start button, then a click on the sun's left edge.
        h.get_by_label("Start").click();
        h.run_steps(1);
        assert_eq!(h.state().view_tools.marker_pick, Some(Marker::Start));
        let rect = h.state().canvas_rect;
        let b = crate::app::shape_bounds(&h.state().doc.shapes[sun]);
        let left = h
            .state()
            .view
            .to_screen(rect, P::new(b.min.x, b.center().y));
        click_at(&mut h, left);
        h.run_steps(2);
        let t = h.state().doc.shapes[sun].params.start.expect("placed");
        let at = zigzag::design::outline_at(&h.state().doc.shapes[sun].geometry, t).unwrap();
        assert!(at.dist(P::new(b.min.x, b.center().y)) < 5.0, "{at:?}");
        assert_eq!(h.state().view_tools.marker_pick, None);
        // Drag the marker round to the right edge.
        let right = h
            .state()
            .view
            .to_screen(rect, P::new(b.max.x, b.center().y));
        drag(&mut h, left, right);
        let t2 = h.state().doc.shapes[sun].params.start.unwrap();
        let at2 = zigzag::design::outline_at(&h.state().doc.shapes[sun].geometry, t2).unwrap();
        assert!(at2.dist(P::new(b.max.x, b.center().y)) < 5.0, "{at2:?}");
        assert!(h.state().modified);
        // Clear takes both away; the token round-trips through the SVG.
        h.get_by_label("End").click();
        h.run_steps(1);
        click_at(&mut h, left);
        h.run_steps(2);
        assert!(h.state().doc.shapes[sun].params.end.is_some());
        let svg = h.state().doc.to_svg().unwrap();
        assert!(svg.contains("st=") && svg.contains("en="), "{svg}");
        h.get_by_label("Clear").click();
        h.run_steps(1);
        let p = &h.state().doc.shapes[sun].params;
        assert!(p.start.is_none() && p.end.is_none());
    }

    #[test]
    fn trim_stop_and_connect_are_in_the_panels() {
        use egui_kittest::kittest::Queryable;
        let mut h = studio(Some("badge.svg"));
        let sun = h.state().names.iter().position(|n| n == "sun").unwrap();
        h.state_mut().set_selection(&[sun]);
        h.run_steps(2);
        h.get_by_label("Trim").click();
        h.get_by_label("Stop").click();
        h.run_steps(2);
        let p = &h.state().doc.shapes[sun].params;
        assert_eq!((p.trim, p.stop), (Some(true), Some(true)));
        let blocks = h.state().design.blocks.len();
        h.get_by_label("Stop").click();
        h.run_steps(2);
        assert_eq!(h.state().doc.shapes[sun].params.stop, None);
        assert!(h.state().design.blocks.len() <= blocks);
        // Connect gaps: kept in the document, and used by the plan.
        h.state_mut().doc.settings.connect_mm = 20.0;
        h.state_mut().replan();
        let svg = h.state().doc.to_svg().unwrap();
        assert!(svg.contains("connect=20"), "{svg}");
        assert_eq!(
            zigzag::document::Document::from_svg(&svg, "x", None)
                .unwrap()
                .settings
                .connect_mm,
            20.0
        );
    }

    #[test]
    fn the_connect_bar_counts_the_cuts_it_saves_and_shows_what_it_sews() {
        use egui_kittest::kittest::Queryable;
        let mut h = studio(None);
        // One object in two squares 3 mm apart, on bare cloth: a cut.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="20mm" viewBox="0 0 40 20"><path id="pair" d="M2,2H12V12H2Z M15,2H25V12H15Z" fill="#203040"/></svg>"##;
        let doc = zigzag::document::Document::from_svg(svg, "pair", None).unwrap();
        h.state_mut().set_document(doc);
        h.run_steps(3);
        assert_eq!(h.state().design.trim_count(), 1);
        assert!(
            h.query_by_label("1 cut: drag to sew across the short ones")
                .is_some()
        );
        assert!(h.state().bridges_peek.is_none());
        // The bar in hand: what it sews is lit up on the canvas.
        h.get_by_label("Connect gaps").hover();
        h.run_steps(2);
        assert!(h.state().bridges_peek.is_some());
        // Too short to reach across: nothing saved.
        h.state_mut().doc.settings.connect_mm = 1.0;
        h.state_mut().replan();
        h.run_steps(2);
        assert!(h.query_by_label("1 cut: none this short").is_some());
        assert!(h.state().design.bridges.is_empty());
        h.state_mut().doc.settings.connect_mm = 0.0;
        h.state_mut().replan();
        h.run_steps(2);
        // Dragged to its far end (20 mm): sewn across, and counted.
        let bar = h.get_by_label("Connect gaps").rect();
        let (from, to) = (
            Pos2::new(bar.left() + 2.0, bar.center().y),
            Pos2::new(bar.right() - 1.0, bar.center().y),
        );
        drag(&mut h, from, to);
        h.run_steps(3);
        h.state_mut().plan_now();
        h.run_steps(1);
        assert!(h.state().doc.settings.connect_mm > 15.0);
        assert_eq!(h.state().design.trim_count(), 0);
        assert_eq!(h.state().design.bridges.len(), 1);
        assert!(h.query_by_label("1 cut down to 0").is_some());
        // Kept in the file.
        assert!(h.state().doc.to_svg().unwrap().contains("connect="));
        // Undo takes the bridge away again.
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(3);
        h.state_mut().plan_now();
        h.run_steps(1);
        assert_eq!(h.state().doc.settings.connect_mm, 0.0);
        assert_eq!(h.state().design.trim_count(), 1);
    }

    #[test]
    fn density_map_and_sewing_order_toggle_from_the_view_menu() {
        let mut h = studio(Some("badge.svg"));
        assert!(!h.state().view_tools.density && !h.state().view_tools.order_labels);
        h.state_mut()
            .run_action(crate::commands::Action::ToggleDensity);
        h.state_mut()
            .run_action(crate::commands::Action::ToggleOrder);
        h.run_steps(2);
        assert!(h.state().view_tools.density && h.state().view_tools.order_labels);
        assert!(h.state().status.contains("per mm²"), "{}", h.state().status);
        // The map is worked out once per plan.
        let a = std::rc::Rc::as_ptr(&h.state().density_map());
        h.run_steps(2);
        assert_eq!(a, std::rc::Rc::as_ptr(&h.state().density_map()));
        h.state_mut().replan();
        assert_ne!(a, std::rc::Rc::as_ptr(&h.state().density_map()));
        assert_eq!(
            h.state()
                .action_checked(crate::commands::Action::ToggleDensity),
            Some(true)
        );
    }

    #[test]
    fn decorative_fills_and_satin_styles_from_the_panel() {
        use egui_kittest::kittest::Queryable;
        let mut h = studio(Some("badge.svg"));
        let disc = h.state().names.iter().position(|n| n == "disc").unwrap();
        let swoosh = h.state().names.iter().position(|n| n == "swoosh").unwrap();
        h.state_mut().set_selection(&[disc]);
        h.run_steps(2);
        h.state_mut().plan_now();
        let tatami = h.state().design.per_shape[disc];
        for (label, kind) in [
            ("Meander", Kind::Meander),
            ("Ripple", Kind::Ripple),
            ("Cross-stitch", Kind::Cross),
            ("Tartan", Kind::Tartan),
        ] {
            h.get_by_label(label).click();
            h.run_steps(3);
            // Slow plans may still be on the worker after these UI frames.
            h.state_mut().plan_now();
            assert_eq!(h.state().doc.shapes[disc].params.kind, Some(kind));
            let n = h.state().design.per_shape[disc];
            assert!(n > 0 && n != tatami, "{label}: {n}");
        }
        // Tartan: two threads from one object, shown in the Threads panel.
        let blocks = h.state().design.blocks.len();
        assert!(
            h.state()
                .design
                .block_shapes
                .iter()
                .filter(|b| b.contains(&disc))
                .count()
                >= 2,
            "{blocks}"
        );
        h.get_by_label("+ Stripe").click();
        h.run_steps(3);
        assert_eq!(
            h.state().doc.shapes[disc]
                .params
                .sett
                .as_ref()
                .map(|s| s.len()),
            Some(3)
        );
        // The satin's stitch styles and a second thread.
        h.state_mut().set_selection(&[swoosh]);
        h.run_steps(2);
        h.state_mut().replan();
        let plain = h.state().design.per_shape[swoosh];
        h.get_by_label("Zig-zag").click();
        h.run_steps(2);
        h.state_mut().replan();
        assert!(h.state().design.per_shape[swoosh] < plain);
        h.get_by_label("S-stitch").click();
        h.run_steps(2);
        h.state_mut().replan();
        assert!(h.state().design.per_shape[swoosh] > plain);
        let before = h.state().design.blocks.len();
        h.state_mut().doc.shapes[swoosh].params.colors = Some(vec![[200, 0, 0], [0, 0, 200]]);
        h.state_mut().replan();
        assert_eq!(h.state().design.blocks.len(), before + 1, "a second thread");
        // Everything round-trips through the SVG.
        let svg = h.state().doc.to_svg().unwrap();
        let back = zigzag::document::Document::from_svg(&svg, "x", None).unwrap();
        assert_eq!(back.shapes[disc].params.kind, Some(Kind::Tartan));
        assert_eq!(
            back.shapes[disc].params.sett.as_ref().map(|s| s.len()),
            Some(3)
        );
        assert_eq!(
            back.shapes[swoosh].params.colors.as_ref().map(|c| c.len()),
            Some(2)
        );
        assert_eq!(
            back.shapes[swoosh].params.style,
            Some(zigzag::stitch::satin::SatinStyle::SStitch)
        );
    }

    #[test]
    fn grouping_colors_and_worksheet() {
        let mut h = studio(Some("badge.svg"));
        h.state_mut().group_colors();
        h.run_steps(3);
        assert!(h.state().design.blocks.len() <= 4);
        let html = h.state().worksheet_html().unwrap();
        assert!(html.contains("<table>") && html.contains("disc"));
        h.state_mut().set_selection(&[0]);
        h.state_mut().move_selected_to(false);
        h.run_steps(3);
        assert_eq!(h.state().sel_index(), Some(h.state().doc.shapes.len() - 1));
    }
}
