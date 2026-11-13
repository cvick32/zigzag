//! The Skitch window: sewing a design, the way a slicer's device page
//! follows a print. Before sending it's the send sheet: the machine (with
//! pairing help), the design, the threads to have ready, options and a
//! checklist. Once the design is on the machine it's the monitor: the
//! design with what's sewn so far and where the needle is, a timeline of
//! its threads to scrub the needle along while paused, progress and time
//! left, Start / Pause / Stop, which thread is on and which comes next,
//! and what to do at each point on the machine (Accept, Start, rethread)
//! or about an error.

use crate::app::Studio;
use crate::canvas::{self, DrawOptions, View};
use crate::icons::{self, Icon};
use crate::machine::{Cmd, Link, Snapshot, has_progress};
use crate::theme::{caption, icon_button, pal_ui, space};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use zigzag::design::Design;
use zigzag::machine::protocol::{self, Status};

/// Where in the job things are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Connect,
    Home,
    Send,
    Trace,
    Sew,
    Done,
}

const STEPS: [(Step, &str); 6] = [
    (Step::Connect, "Connect"),
    (Step::Home, "Home"),
    (Step::Send, "Send"),
    (Step::Trace, "Trial"),
    (Step::Sew, "Sew"),
    (Step::Done, "Done"),
];

/// A step's name, as the window shows it.
pub fn step_name(at: Step) -> &'static str {
    STEPS
        .iter()
        .find(|(s, _)| *s == at)
        .map(|(_, n)| *n)
        .unwrap_or("")
}

/// The step a snapshot is at (`ours`: the machine has the design we sent).
pub fn step(s: &Snapshot, ours: bool) -> Step {
    if s.link != Link::Connected {
        return Step::Connect;
    }
    let Some(st) = s.status else {
        return Step::Connect;
    };
    if st == Status::Initial || s.error == 0x72 {
        return Step::Home;
    }
    if !ours || s.upload.is_some() {
        return Step::Send;
    }
    match st {
        Status::Idle | Status::WaitingForMaskTrace | Status::MaskTracing => Step::Trace,
        Status::Complete => Step::Done,
        Status::MaskTraceComplete
        | Status::ReadyToSew
        | Status::Sewing
        | Status::WaitingForColorChange
        | Status::Paused
        | Status::Stopped
        | Status::Interrupted => Step::Sew,
        _ => Step::Trace,
    }
}

/// Green for done.
pub(crate) const GOOD: Color32 = Color32::from_rgb(52, 168, 83);

/// A button filled with the accent color.
fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let accent = pal_ui(ui).accent;
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(accent)
            .min_size(Vec2::new(0.0, 26.0)),
    )
}

/// The line under the picture: a figure, a detail, and (right) a time.
fn stats(ui: &mut egui::Ui, big: &str, detail: &str, right: &str) {
    let p = pal_ui(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        ui.label(RichText::new(big).size(13.0).strong());
        if !detail.is_empty() {
            ui.label(RichText::new(detail).size(12.0).color(p.secondary));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !right.is_empty() {
                ui.label(RichText::new(right).size(12.0).color(p.secondary));
            }
        });
    });
}

/// A section's heading, with a note at the right.
fn section_label(ui: &mut egui::Ui, text: &str, note: &str) {
    let p = pal_ui(ui);
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).size(12.5).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !note.is_empty() {
                ui.label(RichText::new(note).size(12.0).color(p.secondary));
            }
        });
    });
}

/// A transport button: icon and label, filled with the accent when it's
/// the thing to do.
fn transport(
    ui: &mut egui::Ui,
    icon: Icon,
    text: &str,
    enabled: bool,
    main: bool,
) -> egui::Response {
    let p = pal_ui(ui);
    let ink = match (enabled, main) {
        (false, _) => p.tertiary,
        (true, true) => Color32::WHITE,
        (true, false) => p.text,
    };
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), ink);
    let size = Vec2::new(galley.size().x + 40.0, 30.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (r, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    let fill = match (enabled, main) {
        (true, true) if resp.hovered() => p.accent.gamma_multiply(0.85),
        (true, true) => p.accent,
        (true, false) if resp.hovered() => p.pressed,
        _ => p.hover,
    };
    ui.painter().rect_filled(r, 7.0, fill);
    icons::paint(
        ui.painter(),
        Rect::from_center_size(Pos2::new(r.left() + 17.0, r.center().y), Vec2::splat(12.0)),
        icon,
        ink,
    );
    ui.painter().galley(
        Pos2::new(r.left() + 29.0, r.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    resp
}

/// "1. …" lines, wrapped.
fn numbered<S: AsRef<str>>(ui: &mut egui::Ui, items: &[S]) {
    let p = pal_ui(ui);
    for (k, t) in items.iter().enumerate() {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            ui.add_sized(
                [14.0, 16.0],
                egui::Label::new(
                    RichText::new(format!("{}.", k + 1))
                        .size(12.0)
                        .color(p.secondary),
                ),
            );
            ui.add(egui::Label::new(RichText::new(t.as_ref()).size(12.5).color(p.text)).wrap());
        });
    }
}

/// A card: a heading and what to do.
fn card(ui: &mut egui::Ui, tint: Option<Color32>, add: impl FnOnce(&mut egui::Ui)) {
    let p = pal_ui(ui);
    let fill = match tint {
        Some(c) => c.gamma_multiply(if p.dark { 0.16 } else { 0.10 }),
        None => p.hover,
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(8)
        .inner_margin(egui::Margin::same(space::S as i8 + 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = space::XS + 2.0;
            add(ui);
        });
}

fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(14.0).strong());
}

/// "3, 9 and 12".
fn join(v: &[usize]) -> String {
    let s: Vec<String> = v.iter().map(|k| k.to_string()).collect();
    match s.len() {
        0 => String::new(),
        1 => s[0].clone(),
        n => format!("{} and {}", s[..n - 1].join(", "), s[n - 1]),
    }
}

/// Thousands with commas.
fn n(v: usize) -> String {
    let s = v.to_string();
    let mut out = String::new();
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Minutes and seconds.
fn clock(secs: usize) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// The thread to use for a color: the nearest in the chosen chart.
pub struct Thread {
    pub color: [u8; 3],
    pub name: String,
}

/// Whether the needle can be moved along the design: the machine has it
/// and isn't moving. (A Skitch paused with its own Start/Stop button says
/// it's ready to sew, the same as before the first stitch: not paused.)
fn needle_movable(st: Status) -> bool {
    matches!(
        st,
        Status::Paused | Status::Interrupted | Status::Stopped | Status::ReadyToSew
    )
}

/// What the machine is doing, as one heading, for the status line.
fn headline(s: &Snapshot) -> String {
    match &s.link {
        Link::Connected => {
            let model = s
                .info
                .as_ref()
                .map(|i| i.model.trim().to_string())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "Skitch".into());
            match (s.upload, s.status) {
                (Some(f), _) => format!("{model} · Sending {:.0}%", f * 100.0),
                (_, Some(st)) => format!("{model} · {}", st.label()),
                _ => model,
            }
        }
        Link::Searching => "Looking for the Skitch…".into(),
        Link::Lost(_) => "Connection lost".into(),
        Link::NotPaired => "Not paired".into(),
        _ => "Not connected".into(),
    }
}

impl Studio {
    fn thread(&self, c: [u8; 3]) -> Thread {
        let brand = self.view_tools.brand;
        let (_, name, number) = crate::threads::nearest(brand, c);
        let b = if brand == crate::threads::Brand::Brother {
            "Brother"
        } else {
            "Janome"
        };
        Thread {
            color: c,
            name: format!("{b} {}", crate::threads::label(name, number)),
        }
    }

    /// The thread to put on: a big swatch, its name and a caption.
    fn thread_row(&self, ui: &mut egui::Ui, t: &Thread, note: &str) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
            ui.painter().rect_filled(
                r,
                5.0,
                Color32::from_rgb(t.color[0], t.color[1], t.color[2]),
            );
            ui.painter().rect_stroke(
                r,
                5.0,
                Stroke::new(1.0, Color32::from_black_alpha(50)),
                egui::StrokeKind::Inside,
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new(&t.name).size(13.0).strong());
                caption(ui, note);
            });
        });
    }

    /// The threads in sewing order: of what was sent, else of the design.
    fn job_threads(&self, s: &Snapshot) -> Vec<([u8; 3], usize)> {
        match self.machine.ours(s) {
            Some(sent) => sent
                .pen
                .blocks
                .iter()
                .map(|b| {
                    let c = sent
                        .design
                        .blocks
                        .get(b.block)
                        .map(|k| k.color)
                        .unwrap_or([0; 3]);
                    (c, b.end - b.start)
                })
                .collect(),
            None => self
                .design
                .blocks
                .iter()
                .map(|b| (b.color, b.runs.iter().map(|r| r.pts.len()).sum()))
                .filter(|b| b.1 > 0)
                .collect(),
        }
    }

    /// Which thread (color block) the machine is on: while it waits for a
    /// thread change, the one to change to.
    fn thread_index(&self, s: &Snapshot) -> Option<usize> {
        let sent = self.machine.ours(s)?;
        let k = s.progress.map(|p| p.current_stitch as usize).unwrap_or(0);
        sent.pen
            .thread_at(k, s.status == Some(Status::WaitingForColorChange))
    }

    /// The machine's stitch (a record of what was sent).
    fn at_stitch(&self, s: &Snapshot) -> usize {
        let total = self.machine.ours(s).map(|x| x.pen.stitches).unwrap_or(0);
        if s.status == Some(Status::Complete) {
            total
        } else {
            s.progress
                .map(|p| p.current_stitch as usize)
                .unwrap_or(0)
                .min(total)
        }
    }

    /// Open (or close) the Skitch window; opening connects if it isn't.
    pub fn toggle_machine_window(&mut self) {
        if self.machine.open {
            self.machine.open = false;
            return;
        }
        self.machine.open = true;
        let s = self.machine.snapshot();
        if matches!(s.link, Link::Off | Link::NotFound | Link::Lost(_)) && cfg!(feature = "ble") {
            let ctx = self.egui_ctx.clone().unwrap_or_default();
            // The simulated machine, once it's the one in use; else the Skitch.
            let sim = self.machine.sim.is_some();
            self.machine.connect(sim, &ctx);
        }
    }

    /// The Skitch window.
    pub fn machine_window(&mut self, ctx: &egui::Context) {
        if !self.machine.open {
            return;
        }
        let s = self.machine.snapshot();
        let mut open = true;
        let screen = ctx.content_rect();
        egui::Window::new("Skitch")
            .id(egui::Id::unique("skitch-window"))
            .title_frame(crate::theme::title_frame(ctx))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([780.0, 560.0])
            .min_width(620.0)
            .max_width(1100.0)
            .default_pos(Pos2::new(screen.right() - 800.0, 70.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = space::S;
                self.machine_header(ui, &s);
                crate::theme::hline(ui);
                ui.add_space(space::XS);
                ui.horizontal_top(|ui| {
                    let w = (ui.available_width() * 0.5).clamp(260.0, 420.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(w, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(w);
                            self.preview_column(ui, &s);
                        },
                    );
                    ui.add_space(space::M);
                    let side = ui.available_width();
                    ui.allocate_ui_with_layout(
                        Vec2::new(side, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(side);
                            egui::ScrollArea::vertical()
                                .id_salt("skitch-side")
                                .auto_shrink([false, true])
                                .max_height(ui.available_height().max(380.0))
                                .show(ui, |ui| {
                                    // Clear of the scroll bar.
                                    egui::Frame::NONE
                                        .inner_margin(egui::Margin {
                                            right: 12,
                                            ..Default::default()
                                        })
                                        .show(ui, |ui| {
                                            ui.spacing_mut().item_spacing.y = space::M;
                                            self.side_column(ui, &s);
                                        });
                                });
                        },
                    );
                });
            });
        if !open {
            self.machine.open = false;
        }
    }

    /// The window's top line: what the machine's doing, the steps, its menu.
    fn machine_header(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let p = pal_ui(ui);
        let ours = self.machine.ours(s).is_some();
        ui.horizontal(|ui| {
            let dot = match (&s.link, s.status) {
                (Link::Connected, Some(Status::Stopped)) => p.error,
                (
                    Link::Connected,
                    Some(Status::WaitingForColorChange | Status::Paused | Status::Interrupted),
                ) => p.warning,
                (Link::Connected, _) => GOOD,
                (Link::Searching, _) => p.warning,
                (Link::Lost(_), _) => p.error,
                _ => p.tertiary,
            };
            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, dot);
            ui.label(RichText::new(headline(s)).size(12.5));
            if s.link == Link::Searching || s.doing.is_some() {
                ui.add(egui::Spinner::new().size(12.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Minus, "Minimize", false, 24.0)
                    .on_hover_text("Minimize: it carries on, and the status bar shows how it's doing")
                    .clicked()
                {
                    self.machine.open = false;
                }
                let more = icon_button(ui, Icon::More, "Skitch Menu", false, 24.0);
                ui.add_space(space::S);
                stepper(ui, step(s, ours));
                egui::Popup::menu(&more).show(|ui| {
                    ui.set_min_width(220.0);
                    if s.link == Link::Connected {
                        if ui.button("Disconnect").clicked() {
                            self.machine.ask(Cmd::Disconnect, ui.ctx());
                        }
                    } else if s.link != Link::Searching && ui.button("Connect").clicked() {
                        self.machine.connect(false, ui.ctx());
                    }
                    if ui
                        .button("Connect to a Simulated Machine")
                        .on_hover_text(
                            "Try it without a Skitch: the simulated one does each step on the machine by itself",
                        )
                        .clicked()
                    {
                        self.machine.connect(true, ui.ctx());
                    }
                    ui.separator();
                    ui.add(egui::Checkbox::new(
                        &mut self.machine.follow,
                        "Show progress on the canvas",
                    ));
                    ui.add(egui::Checkbox::new(
                        &mut self.machine.trace_first,
                        "Trial run after sending",
                    ));
                    ui.add(egui::Checkbox::new(
                        &mut self.machine.auto_show,
                        "Show this when the machine needs you",
                    ));
                    if ours && s.link == Link::Connected {
                        ui.separator();
                        if ui.button("Send Again").clicked() {
                            let ctx = ui.ctx().clone();
                            self.send_design(&ctx);
                        }
                    }
                    ui.separator();
                    if ui
                        .button("Skitch How-To Videos")
                        .on_hover_text("Brother's videos: threading, the bobbin, the frame, a thread break, tension, cleaning")
                        .clicked()
                    {
                        open_url(HOW_TO);
                    }
                });
            });
        });
    }

    /// Left: the design, what's sewn, where the needle is; the threads'
    /// timeline under it.
    fn preview_column(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let p = pal_ui(ui);
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, w * 0.86), Sense::hover());
        ui.painter()
            .rect_filled(rect, 8.0, canvas::color32(self.doc.settings.cloth));
        let sent = self.machine.ours(s).cloned();
        let (design, b): (&Design, _) = match &sent {
            Some(x) => (&x.design, x.bounds),
            None => (&self.design, self.design_bounds()),
        };
        let inner = rect.shrink(16.0);
        let mut view = View::default();
        let sewing = sent.is_some() && s.status.is_some_and(has_progress);
        let mid = zigzag::geom::P::new((b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0);
        let painter = ui.painter_at(rect);
        if sewing {
            // Sewing: close in on the design.
            if !b.is_empty() {
                view.fit(inner, mid, b.width() / 10.0, b.height() / 10.0);
            }
        } else {
            // Before: the hoop, and where in it the design will sew.
            let centered = sent
                .as_ref()
                .map(|x| x.centered)
                .unwrap_or(self.machine.centered);
            let hoop = s
                .info
                .as_ref()
                .map(|i| i.max_width.min(i.max_height) as f64 / 10.0)
                .unwrap_or(100.0);
            let c = if centered && !b.is_empty() {
                mid
            } else {
                zigzag::geom::P::default()
            };
            view.fit(inner, c, hoop, hoop);
            let half = hoop * 5.0;
            let frame = Rect::from_two_pos(
                view.to_screen(rect, c - zigzag::geom::P::new(half, half)),
                view.to_screen(rect, c + zigzag::geom::P::new(half, half)),
            );
            let ink = Color32::from_black_alpha(70);
            painter.rect_stroke(frame, 6.0, Stroke::new(1.0, ink), egui::StrokeKind::Middle);
            // The frame's center marks.
            for (a, d) in [
                (frame.center_top(), Vec2::Y),
                (frame.center_bottom(), -Vec2::Y),
                (frame.left_center(), Vec2::X),
                (frame.right_center(), -Vec2::X),
            ] {
                painter.line_segment([a, a + d * 7.0], Stroke::new(1.0, ink));
            }
        }
        let opts = DrawOptions {
            show_jumps: false,
            limit: usize::MAX,
            thread_mm: 0.4,
            view: self.view_tools.stitch_view,
            background: self.doc.settings.cloth,
            moving: None,
            hidden: None,
        };
        let generation = sent
            .as_ref()
            .map(|x| u64::from_le_bytes(x.uuid[..8].try_into().unwrap_or([0; 8])))
            .unwrap_or(self.replans ^ 0x5eed);
        canvas::draw_design_cached(
            &painter,
            rect,
            &view,
            design,
            &opts,
            generation,
            &mut self.preview_mesh.borrow_mut(),
        );
        if let (true, Some(sent)) = (sewing, &sent) {
            // What's still to sew, faded; what's sewn over it.
            let c = self.doc.settings.cloth;
            painter.rect_filled(
                rect,
                8.0,
                Color32::from_rgba_unmultiplied(c[0], c[1], c[2], 190),
            );
            let at = self.machine.picking.unwrap_or(self.at_stitch(s));
            let sewn = sewn_of(sent, at);
            let needle = canvas::draw_design_cached(
                &painter,
                rect,
                &view,
                design,
                &DrawOptions {
                    limit: sewn.max(1),
                    ..opts.clone()
                },
                generation,
                &mut self.preview_sewn_mesh.borrow_mut(),
            );
            if let Some(n) = needle
                && sewn > 0
                && at < sent.pen.stitches
            {
                painter.circle_stroke(n, 7.0, Stroke::new(2.0, Color32::from_rgb(255, 60, 60)));
                painter.circle_filled(n, 2.5, Color32::from_rgb(255, 60, 60));
            }
        }
        if design.blocks.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Nothing to sew yet",
                egui::FontId::proportional(13.0),
                p.secondary,
            );
        }
        match (sent, s.upload) {
            (_, Some(f)) => {
                // Sending: the one bar fills as it goes.
                let (r, _) = ui.allocate_exact_size(Vec2::new(w, 14.0), Sense::hover());
                let bar = Rect::from_center_size(r.center(), Vec2::new(w, 6.0));
                ui.painter().rect_filled(bar, 3.0, p.hover);
                let done = Rect::from_min_max(
                    bar.min,
                    Pos2::new(bar.left() + bar.width() * f, bar.bottom()),
                );
                ui.painter().rect_filled(done, 3.0, p.accent);
                stats(ui, &format!("Sending {:.0}%", f * 100.0), "", "");
            }
            (Some(sent), None) => self.timeline(ui, s, &sent),
            (None, None) => {
                let b = self.design_bounds();
                stats(
                    ui,
                    &crate::units::fmt_size(b.width(), b.height()),
                    &format!("{} stitches", n(self.design.stitch_count())),
                    &format!(
                        "about {:.0} min",
                        (self.design.stitch_count() as f64 / 400.0).ceil()
                    ),
                );
            }
        }
    }

    /// The design's threads end to end, the needle's place on them; while
    /// paused, drag along it to move the needle.
    fn timeline(&mut self, ui: &mut egui::Ui, s: &Snapshot, sent: &crate::machine::Sent) {
        let p = pal_ui(ui);
        let total = sent.pen.stitches.max(1);
        let at = self.at_stitch(s);
        let movable = s.status.is_some_and(needle_movable);
        let w = ui.available_width();
        let (r, resp) = ui.allocate_exact_size(
            Vec2::new(w, 14.0),
            if movable {
                Sense::click_and_drag()
            } else {
                Sense::hover()
            },
        );
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Slider, movable, "Stitch timeline")
        });
        let bar = Rect::from_center_size(r.center(), Vec2::new(w, 6.0));
        let x_of = |k: usize| bar.left() + bar.width() * k as f32 / total as f32;
        let painter = ui.painter_at(bar);
        // Each thread its own color, faint where it's still to sew.
        let shown = self.machine.picking.unwrap_or(at);
        ui.painter().rect_filled(bar, 3.0, p.hover);
        for b in &sent.pen.blocks {
            let c = sent
                .design
                .blocks
                .get(b.block)
                .map(|k| k.color)
                .unwrap_or([0; 3]);
            let c = canvas::color32(c);
            let split = shown.clamp(b.start, b.end);
            let sewn = Rect::from_x_y_ranges(x_of(b.start)..=x_of(split), bar.y_range());
            let rest = Rect::from_x_y_ranges(x_of(split)..=x_of(b.end), bar.y_range());
            painter.rect_filled(sewn, 0.0, c);
            painter.rect_filled(rest, 0.0, c.gamma_multiply(0.3));
        }
        // Round the ends.
        ui.painter().rect_stroke(
            bar.expand(1.0),
            4.0,
            Stroke::new(2.0, p.dock),
            egui::StrokeKind::Inside,
        );
        let x = x_of(shown).clamp(bar.left() + 1.0, bar.right() - 1.0);
        ui.painter().line_segment(
            [Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
            Stroke::new(2.0, p.text),
        );
        let pick = |pos: Pos2| {
            (((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0) * total as f32).round() as usize
        };
        if movable {
            if let Some(pos) = resp.interact_pointer_pos()
                && (resp.dragged() || resp.clicked())
            {
                self.machine.picking = Some(pick(pos));
            }
            if (resp.drag_stopped() || resp.clicked())
                && let Some(k) = self.machine.picking.take()
            {
                self.machine
                    .ask(Cmd::SetStitch(k.min(u16::MAX as usize) as u16), ui.ctx());
            }
            if let Some(pos) = resp.hover_pos() {
                resp.clone()
                    .on_hover_text(format!("Move the needle to stitch {}", n(pick(pos))));
            }
        }
        let speed = s.pattern.map(|p| p.speed).filter(|v| *v > 0).unwrap_or(400) as f64;
        let left = ((total - shown.min(total)) as f64 * 60.0 / speed) as usize;
        stats(
            ui,
            &format!("{:.0}%", shown as f32 / total as f32 * 100.0),
            &format!("{} / {} stitches", n(shown), n(total)),
            &if shown < total {
                format!("{} left", clock(left))
            } else {
                String::new()
            },
        );
    }

    /// Right: what to do now, progress, controls, the threads.
    fn side_column(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let ours = self.machine.ours(s).is_some();
        let at = step(s, ours);
        // What went wrong comes first: it's what to sort out on the machine.
        if s.link == Link::Connected
            && let Some(help) = protocol::error_help(s.error)
            && !help.informational
        {
            error_card(ui, s.error, &help);
        }
        match at {
            Step::Connect => self.connect_card(ui, s),
            Step::Home => home_card(ui),
            Step::Send => self.send_card(ui, s),
            Step::Trace | Step::Sew | Step::Done => {
                self.guide_card(ui, s);
                self.controls(ui, s);
            }
        }
        if let Some(problem) = &s.problem {
            crate::theme::callout(ui, pal_ui(ui).error, Icon::Warning, problem);
        }
        self.threads_list(ui, s, at);
        if matches!(at, Step::Send | Step::Trace)
            || (at == Step::Sew && self.at_stitch(s) == 0 && s.status != Some(Status::Sewing))
        {
            self.checklist(ui, s);
        }
        if s.link == Link::Connected {
            self.details(ui, s);
        }
    }

    fn connect_card(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let p = pal_ui(ui);
        let ctx = ui.ctx().clone();
        let mut connect = false;
        let mut sim = false;
        let ble = cfg!(feature = "ble");
        match &s.link {
            Link::Searching => card(ui, None, |ui| {
                title(ui, "Looking for the Skitch…");
            }),
            Link::NotPaired => card(ui, Some(p.warning), |ui| {
                title(ui, "Pair the Skitch with this computer");
                numbered(
                    ui,
                    &[
                        "Hold the Skitch's Bluetooth button until it beeps.",
                        "Click Try Again straight away. If your computer asks to pair, click Connect or Pair.",
                    ],
                );
                ui.horizontal(|ui| {
                    connect = primary(ui, "Try Again").clicked();
                    if cfg!(target_os = "macos")
                        && ui
                            .button("Bluetooth Permission")
                            .on_hover_text("Check ZigZag is allowed to use Bluetooth")
                            .clicked()
                    {
                        open_bluetooth_settings();
                    }
                });
                if cfg!(target_os = "macos") {
                    caption(
                        ui,
                        "It won't show up in the Mac's Bluetooth settings: it pairs from here.",
                    );
                }
            }),
            Link::NotFound => card(ui, Some(p.warning), |ui| {
                title(ui, "No Skitch found");
                numbered(
                    ui,
                    &[
                        "Check it's on and close by.",
                        "Close Artspira: the Skitch talks to one app at a time.",
                        "First time? Hold its Bluetooth button until it beeps, then Try Again.",
                    ],
                );
                ui.horizontal(|ui| {
                    connect = primary(ui, "Try Again").clicked();
                    if cfg!(target_os = "macos")
                        && ui
                            .button("Bluetooth Permission")
                            .on_hover_text("Check ZigZag is allowed to use Bluetooth")
                            .clicked()
                    {
                        open_bluetooth_settings();
                    }
                });
            }),
            Link::Failed(msg) => card(ui, Some(p.error), |ui| {
                title(ui, "Couldn't connect");
                caption(ui, msg);
                ui.horizontal(|ui| {
                    connect = primary(ui, "Try Again").clicked();
                    if !ble {
                        sim = ui.button("Use a Simulated Machine").clicked();
                    }
                });
            }),
            Link::Lost(_) => card(ui, Some(p.error), |ui| {
                title(ui, "Lost the connection");
                caption(ui, "It keeps sewing; reconnect to follow it.");
                connect = primary(ui, "Reconnect").clicked();
            }),
            Link::Off | Link::Connected => card(ui, None, |ui| {
                title(ui, "Connect to your Skitch");
                numbered(
                    ui,
                    &[
                        "Turn the machine on.",
                        "Close Artspira: the Skitch talks to one app at a time.",
                    ],
                );
                ui.horizontal(|ui| {
                    connect = primary(ui, "Connect").clicked();
                    if !ble {
                        sim = ui.button("Use a Simulated Machine").clicked();
                    }
                });
            }),
        }
        if connect {
            let again_sim = s.sim && matches!(s.link, Link::Lost(_) | Link::Failed(_));
            self.machine.connect(again_sim || !ble, &ctx);
        }
        if sim {
            self.machine.connect(true, &ctx);
        }
    }

    /// Before sending: the design, the size check, options, Send.
    fn send_card(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let ctx = ui.ctx().clone();
        let p = pal_ui(ui);
        let total = self.design.stitch_count();
        let b = self.design_bounds();
        let (w, h) = (b.width() / 10.0, b.height() / 10.0);
        let max = s
            .info
            .as_ref()
            .map(|i| (i.max_width as f64 / 10.0, i.max_height as f64 / 10.0));
        // As placed, all of it has to land inside the machine's area.
        let outside = !self.machine.centered
            && max.is_some_and(|(mw, mh)| {
                b.min.x < -mw * 5.0
                    || b.max.x > mw * 5.0
                    || b.min.y < -mh * 5.0
                    || b.max.y > mh * 5.0
            });
        let too_big =
            max.is_some_and(|(mw, mh)| w > mw || h > mh) || self.design_over_hoop() || outside;
        let mut send = false;
        card(ui, None, |ui| {
            title(
                ui,
                if s.upload.is_some() {
                    "Sending…"
                } else {
                    "Send the design"
                },
            );
            if s.upload.is_some() {
                return;
            }
            if too_big {
                crate::theme::callout(
                    ui,
                    p.error,
                    Icon::Warning,
                    &match max {
                        Some((mw, mh)) if w > mw || h > mh => {
                            format!(
                                "Too big for this machine: it sews up to {}.",
                                crate::units::fmt_size(mw * 10.0, mh * 10.0)
                            )
                        }
                        _ if outside => {
                            "Part of it is outside the hoop: move it in, or choose Centered.".into()
                        }
                        _ => "Too big for the hoop.".into(),
                    },
                );
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("Where").size(12.5).color(p.secondary));
                crate::theme::segmented(
                    ui,
                    "skitch-placement",
                    &mut self.machine.centered,
                    &[(false, "As Placed"), (true, "Centered")],
                );
            });
            ui.checkbox(&mut self.machine.trace_first, "Trial run after sending")
                .on_hover_text(
                    "The needle goes around the design's outline without sewing, so you can check where it lands",
                );
            ui.add_enabled_ui(total > 0 && !too_big, |ui| {
                send = primary(ui, "Send Design").clicked();
            });
        });
        if send {
            self.send_design(&ctx);
        }
    }

    /// Encode the design and send it.
    pub fn send_design(&mut self, ctx: &egui::Context) {
        self.plan_now();
        let pen = zigzag::formats::pen::encode(&self.design);
        self.machine.send(&self.design, pen, ctx);
        self.keep_sent();
    }

    /// What the machine is doing and what to do about it.
    fn guide_card(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let p = pal_ui(ui);
        let threads = self.job_threads(s);
        let total = threads.len();
        let k = self.thread_index(s).unwrap_or(0);
        let started = self.at_stitch(s) > 0;
        let st = s.status.unwrap_or(Status::Idle);
        let note = |k: usize| format!("Thread {} of {total}", k + 1);
        let thread = |k: usize| threads.get(k).map(|t| self.thread(t.0));
        match st {
            Status::Idle => card(ui, None, |ui| {
                title(ui, "Do a trial run");
                caption(
                    ui,
                    "The needle goes around the outline without sewing, to check where it lands.",
                );
            }),
            Status::WaitingForMaskTrace => card(ui, Some(p.accent), |ui| {
                title(ui, "Press Accept on the machine");
                caption(ui, "To start the trial run.");
            }),
            Status::MaskTracing => card(ui, None, |ui| {
                title(ui, "Trial run…");
                caption(
                    ui,
                    "Check the needle clears the frame's magnets, seams and the brim.",
                );
            }),
            Status::MaskTraceComplete if !started => card(ui, None, |ui| {
                title(ui, "Put on the first thread");
                if let Some(t) = thread(0) {
                    self.thread_row(ui, &t, &note(0));
                }
            }),
            Status::ReadyToSew => card(ui, Some(p.accent), |ui| {
                title(ui, "Press Start on the machine");
                if started {
                    caption(ui, "To go back first, drag the bar or use Needle below.");
                }
                if let Some(t) = thread(k) {
                    self.thread_row(ui, &t, &note(k));
                }
            }),
            Status::WaitingForColorChange
                if k > 0 && threads.get(k).map(|t| t.0) == threads.get(k - 1).map(|t| t.0) =>
            {
                card(ui, Some(p.warning), |ui| {
                    title(ui, "Same thread again");
                    if let Some(t) = thread(k) {
                        self.thread_row(ui, &t, &note(k));
                    }
                    caption(ui, "Press Start on the machine.");
                })
            }
            Status::WaitingForColorChange => card(ui, Some(p.warning), |ui| {
                title(ui, "Change the thread");
                if let Some(t) = thread(k) {
                    self.thread_row(ui, &t, &note(k));
                }
                numbered(
                    ui,
                    &[
                        "Cut and pull out the old thread.",
                        "Thread this one, needle too.",
                        "Press Start on the machine.",
                    ],
                );
            }),
            Status::Sewing => card(ui, None, |ui| {
                if let Some(t) = thread(k) {
                    self.thread_row(ui, &t, &note(k));
                }
                if let Some(t) = thread(k + 1) {
                    caption(ui, format!("Next: {}", t.name));
                }
            }),
            Status::Stopped => card(ui, Some(p.error), |ui| {
                title(ui, "Stopped");
                caption(ui, "Clear the problem on the machine.");
            }),
            Status::Complete => card(ui, Some(GOOD), |ui| {
                title(ui, "Finished");
                numbered(
                    ui,
                    &[
                        "Take the hoop off and trim the jump threads.",
                        "Press Accept on the machine.",
                    ],
                );
            }),
            _ => card(ui, Some(p.warning), |ui| {
                // Paused or interrupted part-way.
                title(ui, "Paused");
                caption(ui, "Drag the bar to move the needle, then Resume.");
                if let Some((code, back)) = s.rolled_back
                    && let Some(h) = protocol::error_help(code)
                {
                    caption(
                        ui,
                        format!("Moved back {back} stitches ({}).", h.title.to_lowercase()),
                    );
                }
                if let Some(t) = thread(k) {
                    self.thread_row(ui, &t, &note(k));
                }
            }),
        }
    }

    /// Start / Resume, Pause, Stop, Trace: what the machine allows now.
    fn controls(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let ctx = ui.ctx().clone();
        let st = s.status.unwrap_or(Status::Idle);
        let started = self.at_stitch(s) > 0;
        let busy = s.doing.is_some();
        let can_start = matches!(
            st,
            Status::MaskTraceComplete | Status::Paused | Status::Interrupted | Status::Complete
        ) && !busy;
        let start_label = match st {
            Status::Paused | Status::Interrupted => "Resume",
            Status::Complete => "Sew Again",
            _ => "Start",
        };
        let can_trace = matches!(st, Status::Idle | Status::MaskTraceComplete)
            || (st == Status::ReadyToSew && !started);
        let can_stop = !matches!(
            st,
            Status::Sewing | Status::MaskTracing | Status::ReceivingData
        ) && !busy;
        let mut ask = None;
        let mut stop = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS + 2.0;
            if transport(ui, Icon::Play, start_label, can_start, can_start)
                .on_hover_text("Readies the machine: then press Start on it")
                .clicked()
            {
                ask = Some(Cmd::Sew);
            }
            transport(ui, Icon::Pause, "Pause", false, false).on_hover_text(
                "Pause with the Start/Stop button on the Skitch: it can't be paused over Bluetooth.",
            );
            let stop_btn = transport(ui, Icon::Stop, "Stop", can_stop, false).on_hover_text(
                if can_stop {
                    "End the job: the design comes off the machine"
                } else {
                    "Pause it with the Start/Stop button on the Skitch first."
                },
            );
            egui::Popup::from_toggle_button_response(&stop_btn)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    ui.set_min_width(220.0);
                    ui.label(RichText::new("End this job?").strong());
                    caption(
                        ui,
                        "The design comes off the machine. Send it again to start over.",
                    );
                    if ui.button("End Job").clicked() {
                        stop = true;
                        ui.close();
                    }
                });
            if can_trace
                && transport(
                    ui,
                    Icon::Target,
                    if st == Status::Idle {
                        "Trial Run"
                    } else {
                        "Trial Again"
                    },
                    true,
                    st == Status::Idle,
                )
                .clicked()
            {
                ask = Some(Cmd::Trace);
            }
        });
        // Fine needle moves while it's stopped.
        if needle_movable(st)
            && let Some(sent) = self.machine.ours(s).cloned()
        {
            let at = self.at_stitch(s) as i64;
            let max = sent.pen.stitches as i64;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                caption(ui, "Needle");
                for (icon, label, d) in [
                    (Icon::ChevronsLeft, "Back 100 stitches", -100),
                    (Icon::ChevronLeft, "Back 10 stitches", -10),
                    (Icon::Minus, "Back 1 stitch", -1),
                    (Icon::Plus, "On 1 stitch", 1),
                    (Icon::ChevronRight, "On 10 stitches", 10),
                    (Icon::ChevronsRight, "On 100 stitches", 100),
                ] {
                    if icon_button(ui, icon, label, false, 24.0)
                        .on_hover_text(label)
                        .clicked()
                    {
                        ask = Some(Cmd::SetStitch((at + d).clamp(0, max) as u16));
                    }
                }
                if let Some(b) = sent.pen.block_at(at as usize)
                    && ui
                        .small_button("Thread Start")
                        .on_hover_text("Back to where this thread began")
                        .clicked()
                {
                    ask = Some(Cmd::SetStitch(sent.pen.blocks[b].start as u16));
                }
            });
        }
        if let Some(c) = ask {
            self.machine.ask(c, &ctx);
        }
        if stop {
            self.machine.ask(Cmd::Delete, &ctx);
            self.machine.sent = None;
            self.forget_sent();
        }
    }

    /// The threads in sewing order, like a printer's filament slots: done,
    /// on now, to come; before sending, the ones to have ready.
    fn threads_list(&mut self, ui: &mut egui::Ui, s: &Snapshot, at: Step) {
        let p = pal_ui(ui);
        let threads = self.job_threads(s);
        if threads.is_empty() {
            return;
        }
        let ours = self.machine.ours(s).is_some();
        let sewing = ours && matches!(at, Step::Sew | Step::Done);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::XS + 2.0;
            section_label(
                ui,
                if sewing {
                    "Threads"
                } else {
                    "Threads to have ready"
                },
                "",
            );
            let complete = s.status == Some(Status::Complete);
            let cur = if sewing { self.thread_index(s) } else { None };
            let started = self.at_stitch(s) > 0 || s.status == Some(Status::Sewing);
            for (k, &(c, count)) in threads.iter().enumerate() {
                let done = complete || (started && cur.is_some_and(|cur| k < cur));
                let now = sewing && cur == Some(k) && !complete;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::XS + 2.0;
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    if done {
                        icons::paint(ui.painter(), r, Icon::Check, GOOD);
                    } else if now {
                        ui.painter().circle_filled(r.center(), 4.0, p.accent);
                    } else {
                        ui.painter().text(
                            r.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("{}", k + 1),
                            egui::FontId::proportional(11.0),
                            p.secondary,
                        );
                    }
                    crate::threads::swatch(ui, c, 14.0, false);
                    let t = self.thread(c);
                    let text = RichText::new(t.name).size(12.0);
                    ui.label(if now {
                        text.strong()
                    } else if done {
                        text.color(p.secondary)
                    } else {
                        text
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        caption(ui, n(count));
                    });
                });
            }
        });
    }

    /// The "before you sew" checklist.
    fn checklist(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let first = self
            .job_threads(s)
            .first()
            .map(|t| format!("Threaded with {}", self.thread(t.0).name))
            .unwrap_or_else(|| "Threaded with the first color".into());
        let items = [
            "Bobbin wound and in".to_string(),
            first,
            "Fabric hooped smooth and tight".into(),
            "Hoop on the machine".into(),
            "Needle up".into(),
        ];
        let done = self.machine.checked.iter().filter(|c| **c).count();
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::XS + 2.0;
            section_label(ui, "Before you sew", &format!("{done} of {}", items.len()));
            for (k, t) in items.iter().enumerate() {
                ui.checkbox(&mut self.machine.checked[k], RichText::new(t).size(12.0));
            }
        });
    }

    fn details(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let Some(i) = &s.info else { return };
        egui::CollapsingHeader::new(RichText::new("Machine details").size(12.5))
            .id_salt("machine-details")
            .show(ui, |ui| {
                crate::theme::form(ui, "machine-details-form", |f| {
                    let mut row = |label: &str, value: String| {
                        f.row(label, |ui, _| {
                            ui.label(RichText::new(value).size(12.0));
                        });
                    };
                    row("Model", i.model.trim().to_string());
                    row("Serial", i.serial.clone());
                    row("Firmware", i.software_version.clone());
                    row(
                        "Sewing area",
                        crate::units::fmt_size(i.max_width as f64, i.max_height as f64),
                    );
                    if let Some((since, ever)) = s.counts {
                        row("Stitches sewn", n(ever as usize));
                        row("Since service", n(since as usize));
                    }
                });
            });
        self.settings_probe(ui, s);
    }

    /// Reading the machine's settings bytes, to work out what they mean:
    /// read, change one setting in Artspira, read again; the
    /// bytes that changed are marked.
    fn settings_probe(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let p = pal_ui(ui);
        egui::CollapsingHeader::new(RichText::new("Settings bytes").size(12.5))
            .id_salt("settings-probe")
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = space::XS + 2.0;
                caption(
                    ui,
                    "To work out what these mean: read them, change one setting in Artspira (disconnect here first), reconnect and read again.",
                );
                let mut read = false;
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.machine.settings_label)
                            .hint_text("What you changed in Artspira")
                            .desired_width(ui.available_width() - 70.0),
                    );
                    read = ui
                        .add_enabled(s.doing.is_none(), egui::Button::new("Read"))
                        .clicked();
                });
                if read {
                    let label = std::mem::take(&mut self.machine.settings_label);
                    self.machine.ask(Cmd::ReadSettings(label), ui.ctx());
                }
                // Newest first; bytes that differ from the read before, in color.
                let reads = &s.settings;
                for k in (0..reads.len()).rev().take(8) {
                    let (label, bytes) = &reads[k];
                    let marks = k
                        .checked_sub(1)
                        .map(|j| zigzag::machine::capture::changed(&reads[j].1, bytes))
                        .unwrap_or_default();
                    ui.label(
                        RichText::new(if label.is_empty() { "(no label)" } else { label })
                            .size(11.5)
                            .color(p.secondary),
                    );
                    let mut job = egui::text::LayoutJob::default();
                    job.wrap.max_width = ui.available_width();
                    for (i, b) in bytes.iter().enumerate() {
                        let hot = marks.contains(&i);
                        job.append(
                            &format!("{b:02x}"),
                            if i == 0 { 0.0 } else { 5.0 },
                            egui::TextFormat {
                                font_id: egui::FontId::monospace(11.5),
                                color: if hot { Color32::WHITE } else { p.text },
                                background: if hot { p.accent } else { Color32::TRANSPARENT },
                                ..Default::default()
                            },
                        );
                    }
                    ui.label(job);
                    if !marks.is_empty() {
                        caption(ui, format!("Changed: byte {}", join(&marks)));
                    }
                }
                if let Some(path) = crate::machine::settings_log_path() {
                    caption(ui, format!("Kept in {}", path.display()));
                }
            });
    }

    /// The machine at the right of the status bar, always there: its icon,
    /// and (connected) what it's doing and how far, with a small bar. Click
    /// for the Skitch window. While the window's minimized this is how you
    /// follow it; a thread change, an error or the end bring the window
    /// back.
    pub fn machine_chip(&mut self, ui: &mut egui::Ui) {
        let s = self.machine.snapshot();
        self.machine_attention(&s);
        let p = pal_ui(ui);
        let attention = matches!(
            s.status,
            Some(
                Status::WaitingForColorChange
                    | Status::Paused
                    | Status::Interrupted
                    | Status::Stopped
                    | Status::WaitingForMaskTrace
                    | Status::ReadyToSew
            )
        ) || protocol::error_help(s.error).is_some_and(|h| !h.informational);
        let (ink, text, frac) = match &s.link {
            Link::Connected => {
                let frac = self.machine.ours(&s).and_then(|sent| {
                    let st = s.status?;
                    if st == Status::Complete {
                        return Some(1.0);
                    }
                    let at = s.progress?.current_stitch as f32;
                    has_progress(st).then(|| at / sent.pen.stitches.max(1) as f32)
                });
                let ink = match s.status {
                    Some(Status::Stopped) => p.error,
                    _ if attention => p.warning,
                    Some(Status::Sewing | Status::MaskTracing) => p.accent,
                    // Connected and idle, or done: green.
                    _ => GOOD,
                };
                let text = match (s.upload, s.status) {
                    (Some(f), _) => format!("Sending {:.0}%", f * 100.0),
                    (_, Some(Status::WaitingForColorChange)) => {
                        let next = self
                            .thread_index(&s)
                            .and_then(|k| self.job_threads(&s).get(k).map(|t| t.0))
                            .map(|c| self.thread(c).name)
                            .unwrap_or_default();
                        format!("Change thread: {}", next.trim_start_matches("Brother "))
                    }
                    (_, Some(st)) => match frac {
                        Some(f) if st != Status::Complete => {
                            format!("{} {:.0}%", st.label(), f * 100.0)
                        }
                        _ => st.label().to_string(),
                    },
                    _ => "Connected".into(),
                };
                (ink, Some(text), frac.or(s.upload))
            }
            Link::Searching => (p.secondary, Some("Connecting…".to_string()), None),
            Link::Lost(_) => (p.error, Some("Connection lost".to_string()), None),
            Link::NotPaired => (p.warning, Some("Not paired".to_string()), None),
            _ => (p.secondary, None, None),
        };
        let galley = text.map(|t| {
            ui.painter()
                .layout_no_wrap(t, egui::FontId::proportional(11.5), ink)
        });
        let bar_w = if frac.is_some() { 40.0 } else { 0.0 };
        // Connected: a green dot after the icon, like the MCP chip.
        let dot_w = if s.link == Link::Connected { 8.0 } else { 0.0 };
        let w = 26.0
            + dot_w
            + galley.as_ref().map(|g| g.size().x + 6.0).unwrap_or(0.0)
            + if bar_w > 0.0 { bar_w + 8.0 } else { 0.0 };
        let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Skitch status"));
        if resp.hovered() || self.machine.open {
            ui.painter().rect_filled(r, 5.0, p.hover);
        }
        // Waiting on you: a slow pulse behind it.
        if attention && !self.machine.open {
            let t = ui.input(|i| i.time) as f32;
            let a = 0.10 + 0.10 * (t * 3.0).sin().abs();
            ui.painter()
                .rect_filled(r, 5.0, p.warning.gamma_multiply(a));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        // Working: the needle goes up and down (and the bar shimmers), so
        // it reads as running at a glance.
        let working = s.link == Link::Connected
            && (s.upload.is_some()
                || matches!(s.status, Some(Status::Sewing | Status::MaskTracing)));
        let t = ui.input(|i| i.time) as f32;
        let icon_r =
            Rect::from_center_size(Pos2::new(r.left() + 13.0, r.center().y), Vec2::splat(14.0));
        if working && s.upload.is_none() {
            icons::paint(ui.painter(), icon_r, Icon::MachineFrame, ink);
            // From under the arm down to (almost) the bed and back.
            let rate = if s.status == Some(Status::MaskTracing) {
                1.2
            } else {
                2.6
            };
            let dip = 0.5 + 0.5 * (t * rate * std::f32::consts::TAU).sin();
            let at = |y: f32| icon_r.left_top() + Vec2::new(0.18, y) * icon_r.width();
            ui.painter().line_segment(
                [at(0.5), at(0.58 + 0.26 * dip)],
                Stroke::new((icon_r.width() / 11.0).clamp(1.1, 1.8), ink),
            );
        } else {
            icons::paint(ui.painter(), icon_r, Icon::Machine, ink);
        }
        if working {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
        if dot_w > 0.0 {
            ui.painter()
                .circle_filled(Pos2::new(r.left() + 27.0, r.center().y), 3.0, GOOD);
        }
        let mut x = r.left() + 26.0 + dot_w;
        if let Some(g) = galley {
            let gw = g.size().x;
            ui.painter()
                .galley(Pos2::new(x, r.center().y - g.size().y / 2.0), g, ink);
            x += gw + 8.0;
        }
        if let Some(f) = frac {
            let bar = Rect::from_min_size(Pos2::new(x, r.center().y - 2.0), Vec2::new(bar_w, 4.0));
            ui.painter().rect_filled(bar, 2.0, p.separator);
            let done = Rect::from_min_size(bar.min, Vec2::new(bar_w * f.clamp(0.0, 1.0), 4.0));
            ui.painter().rect_filled(done, 2.0, ink);
            if working && done.width() > 2.0 {
                // A soft highlight sliding along what's done.
                let u = (t * 0.8).fract();
                let cx = done.left() + done.width() * u;
                let glow = Rect::from_center_size(
                    Pos2::new(cx, done.center().y),
                    Vec2::new(8.0_f32.min(done.width()), 4.0),
                )
                .intersect(done);
                ui.painter()
                    .rect_filled(glow, 2.0, Color32::from_white_alpha(110));
            }
        }
        let tip = match &s.link {
            Link::Off => "Skitch: not connected. Click to connect.",
            _ if self.machine.open => "Minimize the Skitch window",
            _ => "Show the Skitch window",
        };
        if resp.on_hover_text(tip).clicked() {
            self.toggle_machine_window();
        }
    }

    /// Bring the window back when the machine starts needing you.
    fn machine_attention(&mut self, s: &Snapshot) {
        let st = s.status;
        let before = self.machine.last_status;
        self.machine.last_status = st;
        if before == st || !self.machine.auto_show || self.machine.open {
            return;
        }
        if self.machine.ours(s).is_some()
            && matches!(
                st,
                Some(
                    Status::WaitingForColorChange
                        | Status::Interrupted
                        | Status::Stopped
                        | Status::Complete
                )
            )
        {
            self.machine.open = true;
        }
    }

    /// The sent design and how many of its stitches are sewn, when the
    /// canvas should show the machine's progress.
    pub fn sewing_view(&self) -> Option<(crate::machine::Sent, usize)> {
        // Minimized, the canvas is yours to work on.
        if !self.machine.follow || !self.machine.open {
            return None;
        }
        let s = self.machine.snapshot();
        let sent = self.machine.ours(&s)?;
        let st = s.status?;
        if s.link != Link::Connected || !has_progress(st) {
            return None;
        }
        let at = self.machine.picking.unwrap_or(self.at_stitch(&s));
        Some((sent.clone(), sewn_of(sent, at)))
    }
}

/// Design stitches sewn once the machine is at record `k`.
fn sewn_of(sent: &crate::machine::Sent, k: usize) -> usize {
    if k >= sent.pen.stitches {
        return sent.design.stitch_count();
    }
    if k == 0 {
        return 0;
    }
    sent.pen
        .design_at
        .get(k.min(sent.pen.design_at.len()) - 1)
        .copied()
        .unwrap_or(0) as usize
}

/// The steps, small: done ones quiet, this one marked, the rest faint.
fn stepper(ui: &mut egui::Ui, at: Step) {
    let p = pal_ui(ui);
    let here = STEPS.iter().position(|(s, _)| *s == at).unwrap_or(0);
    let font = egui::FontId::proportional(11.0);
    let galleys: Vec<_> = STEPS
        .iter()
        .enumerate()
        .map(|(k, (_, label))| {
            let c = match k.cmp(&here) {
                std::cmp::Ordering::Less => p.secondary,
                std::cmp::Ordering::Equal => p.text,
                std::cmp::Ordering::Greater => p.tertiary,
            };
            ui.painter()
                .layout_no_wrap(label.to_string(), font.clone(), c)
        })
        .collect();
    const GAP: f32 = 18.0;
    const DOT: f32 = 10.0;
    let w: f32 =
        galleys.iter().map(|g| g.size().x).sum::<f32>() + GAP * (STEPS.len() - 1) as f32 + DOT;
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 16.0), Sense::hover());
    let mut x = r.left();
    for (k, g) in galleys.into_iter().enumerate() {
        let y = r.center().y;
        if k == here {
            ui.painter()
                .circle_filled(Pos2::new(x + 3.0, y), 3.0, p.accent);
            x += DOT;
        }
        let gw = g.size().x;
        ui.painter()
            .galley(Pos2::new(x, y - g.size().y / 2.0), g, p.text);
        x += gw;
        if k + 1 < STEPS.len() {
            ui.painter().hline(
                (x + 5.0)..=(x + GAP - 5.0),
                y,
                Stroke::new(1.0, if k < here { p.secondary } else { p.separator }),
            );
            x += GAP;
        }
    }
}

fn home_card(ui: &mut egui::Ui) {
    let help = protocol::error_help(0x72).expect("homing help");
    card(ui, None, |ui| {
        title(ui, "Home the machine");
        numbered(ui, help.steps);
    });
}

fn error_card(ui: &mut egui::Ui, code: u8, help: &protocol::ErrorHelp) {
    let p = pal_ui(ui);
    card(ui, Some(p.error), |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Warning, p.error);
            title(ui, help.title);
        });
        caption(ui, format!("{} (error 0x{code:02X})", help.description));
        numbered(ui, help.steps);
    });
}

/// Brother's how-to videos for the Skitch.
const HOW_TO: &str =
    "https://sewingcraft.brother.eu/en/inspiration-and-events/video-gallery/sewing-videos/pp1";

/// Open a web page in the browser.
fn open_url(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}

/// The Mac's Bluetooth privacy settings, where ZigZag is allowed (or
/// not) to use Bluetooth. (A BLE machine like the Skitch never shows in the
/// Bluetooth device list; it pairs when an app connects to it.)
fn open_bluetooth_settings() {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth")
            .spawn();
    }
}

/// Wash over what's still to sew, so what's sewn stands out.
pub fn fade_rect(painter: &egui::Painter, r: Rect, cloth: [u8; 3]) {
    painter.rect_filled(
        r,
        0.0,
        Color32::from_rgba_unmultiplied(cloth[0], cloth[1], cloth[2], 190),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use zigzag::machine::sim::{Sim, SimConfig};

    fn studio(sim: SimConfig) -> (Harness<'static, Studio>, Sim) {
        let path =
            std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/badge.svg"));
        let mut h = Harness::builder()
            .with_size(egui::Vec2::new(1400.0, 1300.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(Some(path), None),
            );
        let sim = Sim::new(sim);
        h.state_mut().machine.sim = Some(sim.clone());
        h.run_steps(3);
        (h, sim)
    }

    fn fast() -> SimConfig {
        SimConfig {
            stitches_per_s: 2500.0,
            person_s: 0.15,
            needs_homing: true,
        }
    }

    /// Run frames until `f` holds of the machine.
    fn until(h: &mut Harness<'_, Studio>, what: &str, f: impl Fn(&Snapshot) -> bool) {
        for _ in 0..1500 {
            h.run_steps(1);
            if f(&h.state().machine.snapshot()) {
                h.run_steps(2);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        panic!("never got to: {what} ({:?})", h.state().machine.snapshot());
    }

    fn status(st: Status) -> impl Fn(&Snapshot) -> bool {
        move |s| s.status == Some(st)
    }

    #[test]
    fn the_window_walks_through_a_whole_job_with_the_threads_to_use() {
        let (mut h, _sim) = studio(fast());
        h.state_mut().send_action();
        h.run_steps(2);
        assert!(h.state().machine.open, "Send opens the Skitch window");
        if cfg!(feature = "ble") {
            // (With Bluetooth built in the window offers no simulator; it
            // connects to the one set up here.)
            assert!(h.query_by_label("Use a Simulated Machine").is_none());
            let ctx = h.ctx.clone();
            h.state_mut().machine.connect(true, &ctx);
        } else {
            h.get_by_label("Use a Simulated Machine").click();
        }
        until(&mut h, "connected", |s| s.link == Link::Connected);
        // A machine just turned on wants homing first.
        until(&mut h, "homing", status(Status::Initial));
        assert!(h.query_by_label("Home the machine").is_some());
        until(&mut h, "homed", status(Status::Idle));
        // What to have ready, before sending.
        assert!(h.query_by_label("Threads to have ready").is_some());
        h.get_by_label("Send Design").click();
        // Sent, then the outline traced (after Accept on the machine).
        until(&mut h, "sent", |s| s.uuid.is_some() && s.upload.is_none());
        until(&mut h, "traced", status(Status::MaskTraceComplete));
        // Which thread to put on first.
        assert!(h.query_by_label("Put on the first thread").is_some());
        assert!(h.query_all_by_label("Brother Ultramarine (406)").count() > 0);
        h.get_by_label("Start").click();
        until(&mut h, "start on the machine", status(Status::ReadyToSew));
        assert!(h.query_by_label("Press Start on the machine").is_some());
        until(&mut h, "sewing", status(Status::Sewing));
        // Pause is the machine's own button.
        assert!(h.query_by_label("Pause").is_some());
        // The thread change: which one to put on.
        until(
            &mut h,
            "thread change",
            status(Status::WaitingForColorChange),
        );
        assert!(h.query_by_label("Change the thread").is_some());
        assert!(h.query_by_label("Thread 2 of 4").is_some());
        assert!(h.query_all_by_label("Brother Pumpkin (126)").count() > 0);
        // The canvas shows what's sewn: the first thread, all of it.
        let (sent, sewn) = h.state().sewing_view().expect("following the machine");
        let first: usize = sent.design.blocks[0].runs.iter().map(|r| r.pts.len()).sum();
        assert!(sewn >= first - 2 && sewn <= first, "{sewn} vs {first}");
        assert!(
            h.query_by_label("Skitch status").is_some(),
            "the status bar chip"
        );
        assert!(h.query_by_label("Stitch timeline").is_some());
        until(&mut h, "finished", status(Status::Complete));
        assert!(h.query_by_label("Finished").is_some());
        assert_eq!(
            h.state().sewing_view().map(|v| v.1),
            Some(
                h.state()
                    .machine
                    .sent
                    .as_ref()
                    .unwrap()
                    .design
                    .stitch_count()
            )
        );
    }

    #[test]
    fn a_thread_break_goes_back_over_the_gap_and_says_what_to_do() {
        let (mut h, sim) = studio(SimConfig {
            stitches_per_s: 150.0,
            person_s: 0.05,
            needs_homing: false,
        });
        let ctx = h.ctx.clone();
        h.state_mut().machine.open = true;
        h.state_mut().machine.connect(true, &ctx);
        until(&mut h, "connected", |s| {
            s.link == Link::Connected && s.status.is_some()
        });
        h.state_mut().send_design(&ctx);
        until(&mut h, "sent", |s| s.uuid.is_some() && s.upload.is_none());
        until(&mut h, "traced", status(Status::MaskTraceComplete));
        h.state_mut().machine.ask(crate::machine::Cmd::Sew, &ctx);
        until(&mut h, "sewing", |s| {
            s.status == Some(Status::Sewing) && s.progress.is_some_and(|p| p.current_stitch > 60)
        });
        sim.fail(0x60);
        until(&mut h, "rolled back", |s| s.rolled_back == Some((0x60, 6)));
        assert!(h.query_by_label("Upper thread").is_some());
        assert!(h.query_by_label("Rethread the top thread.").is_some());
        assert!(
            h.query_by_label("Moved back 6 stitches (upper thread).")
                .is_some()
        );
        // The needle went back six.
        let at = h
            .state()
            .machine
            .snapshot()
            .progress
            .unwrap()
            .current_stitch as usize;
        assert_eq!(at, sim.stitch());
        // Fixed on the machine: sew on.
        // Dragging along the timeline moves the needle.
        let bar = h.get_by_label("Stitch timeline").rect();
        let to = bar.left() + bar.width() * 0.05;
        let at = egui::pos2(to, bar.center().y);
        h.hover_at(at);
        h.run_steps(1);
        h.drag_at(at);
        h.run_steps(1);
        h.drop_at(at);
        h.run_steps(3);
        until(&mut h, "needle moved", |_| sim.stitch() < 200);
        sim.clear_error();
        h.get_by_label("Resume").click();
        until(&mut h, "sewing again", status(Status::Sewing));
    }

    #[test]
    fn paused_with_the_machines_own_button_the_needle_can_go_back() {
        let (mut h, sim) = studio(SimConfig {
            stitches_per_s: 150.0,
            person_s: 0.05,
            needs_homing: false,
        });
        let ctx = h.ctx.clone();
        h.state_mut().machine.open = true;
        h.state_mut().machine.connect(true, &ctx);
        until(&mut h, "connected", |s| {
            s.link == Link::Connected && s.status.is_some()
        });
        h.state_mut().send_design(&ctx);
        until(&mut h, "sent", |s| s.uuid.is_some() && s.upload.is_none());
        until(&mut h, "traced", status(Status::MaskTraceComplete));
        h.state_mut().machine.ask(crate::machine::Cmd::Sew, &ctx);
        until(&mut h, "sewing", |s| {
            s.status == Some(Status::Sewing) && s.progress.is_some_and(|p| p.current_stitch > 60)
        });
        // Nothing to move the needle with while it sews.
        assert!(h.query_by_label("Back 10 stitches").is_none());
        // Start/Stop on the machine: a Skitch says it's ready to sew, not
        // paused, part-way through the design.
        sim.press_start_stop();
        let at = sim.stitch();
        until(&mut h, "stopped where it was", |s| {
            s.status == Some(Status::ReadyToSew)
                && s.progress.is_some_and(|p| p.current_stitch as usize == at)
        });
        assert!(h.query_by_label("Press Start on the machine").is_some());
        h.get_by_label("Back 10 stitches").click();
        until(&mut h, "needle back ten", |_| sim.stitch() == at - 10);
        // The bar moves it too.
        let bar = h.get_by_label("Stitch timeline").rect();
        let to = egui::pos2(bar.left() + bar.width() * 0.01, bar.center().y);
        h.hover_at(to);
        h.run_steps(1);
        h.drag_at(to);
        h.run_steps(1);
        h.drop_at(to);
        h.run_steps(3);
        until(&mut h, "needle moved along the bar", |_| sim.stitch() < 60);
        // Start on the machine again: it sews on from there.
        sim.press_start_stop();
        until(&mut h, "sewing again", status(Status::Sewing));
    }

    #[test]
    fn an_unpaired_machine_gets_pairing_steps() {
        let (mut h, _) = studio(fast());
        h.state_mut().machine.open = true;
        h.state().machine.shared.lock().unwrap().link = Link::NotPaired;
        h.run_steps(2);
        assert!(
            h.query_by_label("Pair the Skitch with this computer")
                .is_some()
        );
        assert!(h.query_by_label("Try Again").is_some());
        h.state().machine.shared.lock().unwrap().link = Link::NotFound;
        h.run_steps(2);
        assert!(h.query_by_label("No Skitch found").is_some());
    }

    #[test]
    fn agents_can_send_and_follow_the_machine() {
        let (mut h, _) = studio(SimConfig {
            needs_homing: false,
            ..fast()
        });
        let r = h
            .state_mut()
            .run_tool("send_to_skitch", &serde_json::json!({}));
        assert!(r.is_error, "not connected yet");
        let ctx = h.ctx.clone();
        h.state_mut().machine.connect(true, &ctx);
        until(&mut h, "connected", |s| {
            s.link == Link::Connected && s.status.is_some()
        });
        let r = h
            .state_mut()
            .run_tool("send_to_skitch", &serde_json::json!({}));
        assert!(!r.is_error);
        until(&mut h, "sent", |s| s.uuid.is_some() && s.upload.is_none());
        let v = h.state().machine_json();
        assert_eq!(v["connection"], "connected");
        assert_eq!(v["has_our_design"], true);
        assert_eq!(v["step"], "Trial", "the trial run next");
        assert_eq!(v["progress"]["threads"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn minimized_it_carries_on_and_comes_back_for_a_thread_change() {
        let (mut h, _) = studio(SimConfig {
            stitches_per_s: 1500.0,
            needs_homing: false,
            ..fast()
        });
        // The machine's icon is always in the status bar; it opens the window.
        h.get_by_label("Skitch status").click();
        h.run_steps(2);
        assert!(h.state().machine.open);
        let ctx = h.ctx.clone();
        h.state_mut().machine.connect(true, &ctx);
        until(&mut h, "connected", |s| {
            s.link == Link::Connected && s.status.is_some()
        });
        h.state_mut().send_design(&ctx);
        until(&mut h, "traced", status(Status::MaskTraceComplete));
        h.get_by_label("Start").click();
        until(&mut h, "sewing", status(Status::Sewing));
        // Minimize: the canvas is the design again, to work on.
        assert!(h.state().sewing_view().is_some());
        h.get_by_label("Minimize").click();
        h.run_steps(2);
        assert!(!h.state().machine.open);
        assert!(h.state().sewing_view().is_none());
        // It needs a new thread: back it comes.
        until(
            &mut h,
            "thread change",
            status(Status::WaitingForColorChange),
        );
        h.run_steps(2);
        assert!(h.state().machine.open, "shown again for the thread change");
        assert!(h.query_by_label("Change the thread").is_some());
    }

    #[test]
    fn it_sews_where_the_design_sits_in_the_hoop_or_centered() {
        let (mut h, sim) = studio(SimConfig {
            needs_homing: false,
            ..fast()
        });
        let ctx = h.ctx.clone();
        h.state_mut().machine.open = true;
        h.state_mut().machine.trace_first = false;
        h.state_mut().machine.connect(true, &ctx);
        until(&mut h, "connected", |s| {
            s.link == Link::Connected && s.status.is_some()
        });
        // Off to the right of the hoop's middle.
        for b in &mut h.state_mut().design.blocks {
            for r in &mut b.runs {
                for p in &mut r.pts {
                    p.x += 200.0;
                }
            }
        }
        let pen = zigzag::formats::pen::encode(&h.state().design);
        let (l, t, r, b) = pen.bounds;
        let mid = (((l + r) / 2) as i16, ((t + b) / 2) as i16);
        // As placed: no move, its center where it is.
        h.state_mut().send_design(&ctx);
        until(&mut h, "sent", |s| s.uuid.is_some() && s.upload.is_none());
        let ((mx, my), c) = sim.layout_move().unwrap();
        assert_eq!((mx, my), (0, 0));
        assert!((c.0 - mid.0).abs() <= 1 && (c.1 - mid.1).abs() <= 1);
        // Centered: moved to the middle.
        h.state_mut().machine.centered = true;
        h.state_mut().send_design(&ctx);
        until(&mut h, "sent again", |s| {
            s.upload.is_none() && sim.layout_move().is_some_and(|m| m.1 == (0, 0))
        });
        let ((mx, my), _) = sim.layout_move().unwrap();
        assert!((mx + mid.0).abs() <= 1 && (my + mid.1).abs() <= 1);
        // No trial run asked for: still idle, Trial Run offered.
        assert_eq!(h.state().machine.snapshot().status, Some(Status::Idle));
    }

    #[test]
    fn settings_reads_mark_the_bytes_that_changed() {
        let (mut h, sim) = studio(SimConfig {
            needs_homing: false,
            ..fast()
        });
        let ctx = h.ctx.clone();
        h.state_mut().machine.open = true;
        h.state_mut().machine.connect(true, &ctx);
        until(&mut h, "connected", |s| {
            s.link == Link::Connected && s.status.is_some()
        });
        h.state_mut()
            .machine
            .ask(Cmd::ReadSettings("before".into()), &ctx);
        until(&mut h, "read", |s| s.settings.len() == 1);
        sim.set_setting(2, 0x06);
        h.state_mut()
            .machine
            .ask(Cmd::ReadSettings("tension +1".into()), &ctx);
        until(&mut h, "read again", |s| s.settings.len() == 2);
        let s = h.state().machine.snapshot();
        assert_eq!(
            zigzag::machine::capture::changed(&s.settings[0].1, &s.settings[1].1),
            vec![2]
        );
        // In the window, under the machine's details.
        assert!(h.query_by_label("Settings bytes").is_some());
    }
}
