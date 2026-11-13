//! Studio app state and panels.

use crate::canvas::{self, DrawOptions, View, color32};
use crate::widgets::*;
use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, Stroke, Vec2};
use std::path::{Path, PathBuf};
use std::time::Instant;
use zigzag::checks::{self, Issue, Level};
use zigzag::design::{self, Design};
use zigzag::document::{Document, Settings};
use zigzag::geom::{Bounds, P};
use zigzag::params::{Kind, Params};
use zigzag::svg::{self, Geometry, Shape};

/// Hoops the machines we care about use. (id, label, width mm, height mm)
pub const HOOPS: [(&str, &str, f64, f64); 3] = [
    (
        "100x100",
        "4×4 in (100 × 100 mm) · Skitch / PE",
        100.0,
        100.0,
    ),
    ("70x70", "70 × 70 mm · Skitch small frame", 70.0, 70.0),
    ("130x180", "5×7 in (130 × 180 mm) · PE800", 130.0, 180.0),
];

/// Typical single-needle home machine speed, for time estimates.
pub const SPM: f64 = 400.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Tool {
    Select,
    /// Illustrator's Direct Selection: points, handles and segments.
    Direct,
    /// Drag to pan the view.
    Hand,
    Angle,
    /// Draw shapes, lines or manual stitches point by point.
    Pen,
    /// Illustrator's Anchor Point tool (Shift+C): convert points, pull out
    /// or take in handles, bend segments.
    Anchor,
    /// Add Anchor Point (+): click a path's outline to add a point there.
    AddAnchor,
    /// Delete Anchor Point (-): click a point to take it out.
    DeleteAnchor,
    /// Paint a smoothed line (satin or run).
    Brush,
    /// Smooth: drag along a selected path to take the wobble out of it.
    Smooth,
    /// Path Eraser: drag across a selected path to cut that part away.
    PathEraser,
    /// Join: scrub between two open ends to join them.
    Join,
    /// Rectangle, Ellipse, Polygon, Star and Line.
    Shape,
    Measure,
    /// Not a mode: opens the Add Text window.
    Text,
}

#[derive(Clone, Copy)]
pub enum History {
    Undo,
    Redo,
}

/// Cheap fingerprint of everything editable, compared every frame to detect edits.
pub(crate) type Snap = (Vec<(usize, Params, [u8; 3])>, Settings);

/// How many steps Undo can go back.
pub const UNDO_STEPS: usize = 200;

pub struct Studio {
    pub doc: Document,
    /// The document as of the last completed edit (what undo returns to).
    pub(crate) stable: Document,
    pub design: Design,
    pub issues: Vec<Issue>,
    pub names: Vec<String>,
    pub(crate) dirty: bool,
    pub(crate) plan_ms: f64,
    pub modified: bool,

    pub selected: Option<usize>,       // uid
    pub(crate) hovered: Option<usize>, // index
    pub view: View,
    pub canvas_rect: Rect,
    pub(crate) fit_pending: bool,
    pub tool: Tool,
    pub(crate) angle_drag: Option<Pos2>,
    /// Move / scale / rotate gesture in progress.
    pub(crate) drag: Option<crate::layout::Drag>,
    /// Pan instead of moving objects (drag started on empty canvas).
    pub(crate) panning: bool,
    /// Space held (Illustrator's temporary Hand): Some(true) once it panned.
    /// A tap without dragging plays the stitch-out instead.
    pub(crate) space_pan: Option<bool>,
    pub(crate) layout: crate::layout::LayoutState,
    pub(crate) path_edit: crate::path_edit::PathEdit,
    pub(crate) shapes: crate::shapes::ShapeState,
    pub(crate) brush: crate::brush::BrushState,
    pub(crate) trace: Option<crate::trace_ui::TraceUi>,
    pub(crate) snap: crate::snap::SnapState,
    pub(crate) cursor: crate::cursors::CursorState,
    /// Tool group flyouts and torn-off toolbars.
    pub(crate) flyouts: crate::flyout::Flyouts,
    /// A drag with Smooth, Path Eraser or Join.
    pub(crate) gesture: crate::path_tools::Gesture,
    pub(crate) palettes: crate::palettes::Palettes,
    /// The in-app agent (Claude Code or Codex) and its prompt bar.
    pub(crate) agent: crate::agent::AgentState,
    /// Room taken from the bottom of the canvas for its HUD (none now).
    pub(crate) hud_lift: f32,
    /// Open the issues list (Window › Issues) next frame.
    pub(crate) open_issues: bool,
    /// How many times the stitches were planned (tests check gestures
    /// don't re-plan every frame).
    pub(crate) replans: u64,
    /// What the last plan can lend the next (see `PlanCache`).
    pub(crate) plan_cache: zigzag::design::PlanCache,
    /// An MCP batch is running: edits re-plan once at its end (or before a
    /// step that reads the stitches), not after every step.
    pub(crate) batching: bool,
    /// Planning on a worker thread, for big designs.
    pub(crate) planner: crate::planner::Planner,
    /// When the menu bar's states were last brought up to date (egui time).
    pub(crate) menu_synced: f64,
    /// The stitches, tessellated (see `canvas::draw_design_cached`).
    pub(crate) stitch_mesh: std::cell::RefCell<Option<canvas::StitchMesh>>,
    /// The stitches drawn by the GPU (None: by the painter, as in tests).
    pub(crate) gpu: Option<std::cell::RefCell<crate::gpu::StitchGpu>>,
    /// Screenshots and recordings for the README (`ZIGZAG_CAPTURE`).
    pub(crate) capture: Option<crate::capture::Capture>,
    /// The design's bounds, by the plan they were measured on (the title
    /// bar and the Skitch window ask every frame).
    pub(crate) bounds_memo: std::cell::Cell<Option<(u64, Bounds)>>,
    /// The selected satin's rails, rungs and mean width, worked out once
    /// per version of it rather than every frame.
    pub(crate) satin_memo: std::cell::RefCell<Option<(u64, std::rc::Rc<SatinInfo>)>>,
    /// The thread color picker is open, anchored at this point.
    pub(crate) picker: Option<Pos2>,
    pub(crate) lettering: crate::lettering::LetteringState,
    pub(crate) view_tools: crate::view_tools::ViewTools,
    pub(crate) dock: crate::dock::DockState,
    /// The panel layout as last written to disk.
    pub(crate) dock_saved: crate::dock::DockState,
    pub(crate) dock_ui: crate::dock::DockUi,
    /// The macOS menu bar (None elsewhere, and in tests).
    pub(crate) native_menu: Option<crate::native_menu::NativeMenu>,
    pub(crate) show_jumps: bool,
    /// The gaps Connect sews across are lit up on the canvas until then
    /// (while its bar is being dragged, and a moment after).
    pub(crate) bridges_peek: Option<Instant>,

    pub(crate) player: Option<usize>,
    pub(crate) playing: bool,
    pub(crate) speed: f32,
    pub(crate) player_acc: f32,

    pub undo: Vec<Document>,
    pub redo: Vec<Document>,
    pub(crate) editing: bool,
    pub(crate) pending_history: Option<History>,

    pub status: String,
    pub(crate) title: String,
    pub mcp: Option<crate::mcp::Server>,
    /// The status bar's MCP chip: recent calls, and why the server is off.
    pub(crate) mcp_ui: crate::mcp_chip::McpUi,
    /// The UI context, for waking the window from background work.
    pub egui_ctx: Option<egui::Context>,
    /// The Skitch: the connection, and what was sent to it.
    pub machine: crate::machine::Machine,
    /// The stitches as the machine is sewing them, tessellated.
    pub(crate) sew_mesh: std::cell::RefCell<Option<canvas::StitchMesh>>,
    /// Stitches drawn up to a limit (the player, a sew in progress).
    pub(crate) limited_mesh: std::cell::RefCell<Option<canvas::StitchMesh>>,
    /// The Sew window's preview of what's sewn so far.
    pub(crate) preview_sewn_mesh: std::cell::RefCell<Option<canvas::StitchMesh>>,
    /// The Skitch window's picture of the design, tessellated.
    pub(crate) preview_mesh: std::cell::RefCell<Option<canvas::StitchMesh>>,
    /// What's open, kept for next time (see `session.rs`).
    pub(crate) session: crate::session::SessionState,
    /// The documents open in tabs (the one in front is `doc` and the rest
    /// of these fields; see `tabs.rs`).
    pub(crate) tabs: crate::tabs::Tabs,
    /// The density map of the current plan (by plan number).
    pub(crate) density: std::cell::RefCell<Option<(u64, std::rc::Rc<crate::density::DensityMap>)>>,
}

impl Studio {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        path: Option<PathBuf>,
        select: Option<usize>,
        mcp_port: Option<u16>,
    ) -> Self {
        crate::theme::install(&cc.egui_ctx);
        let reopen = path.is_none();
        let mut s = Self::with_file(path, select);
        // Kept from now on; reopened unless a file was asked for.
        s.session = crate::session::SessionState::in_config();
        if reopen {
            s.restore_session();
        }
        // A job on the Skitch outlives the app: pick it up too.
        let resume = s.restore_sent() && s.session.resume_skitch;
        s.dock = crate::dock::DockState::load();
        s.palettes = crate::palettes::Palettes::load();
        s.agent = crate::agent::AgentState::load();
        s.dock_saved = s.dock.clone();
        s.egui_ctx = Some(cc.egui_ctx.clone());
        s.capture = crate::capture::Capture::from_env();
        if std::env::var_os("TL_NO_GPU").is_none()
            && let Some(gl) = cc.gl.clone()
        {
            match crate::gpu::StitchGpu::new(gl) {
                Ok(g) => s.gpu = Some(std::cell::RefCell::new(g)),
                Err(e) => s.status = format!("Stitches drawn by the CPU ({e}).",),
            }
        }
        if resume && !s.machine.open {
            // Reopens the Skitch window, which reconnects; once the machine
            // reports the design, its progress shows on it again.
            s.toggle_machine_window();
            let name = s
                .machine
                .sent
                .as_ref()
                .map(|x| x.design.name.clone())
                .unwrap_or_default();
            s.status = format!("{} · Picking up the job on the Skitch: {name}.", s.status);
        }
        #[cfg(target_os = "macos")]
        match crate::native_menu::NativeMenu::new(cc.egui_ctx.clone(), &s) {
            Ok(m) => s.native_menu = Some(m),
            Err(e) => s.status = format!("Menu bar unavailable: {e}"),
        }
        s.mcp_ui.port = mcp_port;
        if let Some(port) = mcp_port {
            let ctx = cc.egui_ctx.clone();
            // (Where to connect, and who has, is under the MCP chip in the
            // status bar.)
            match crate::mcp::start(
                port,
                crate::tools::tool_specs(),
                crate::tools::INSTRUCTIONS.into(),
                move || ctx.request_repaint(),
            ) {
                Ok(server) => s.mcp = Some(server),
                Err(e) => {
                    s.status = format!(
                        "{} · MCP server couldn't start on port {port}: {e}",
                        s.status
                    );
                    s.mcp_ui.start_error = Some(e.to_string());
                }
            }
        }
        s
    }

    /// Construct without an eframe context (used by tests).
    pub fn with_file(path: Option<PathBuf>, select: Option<usize>) -> Self {
        let doc = Document::empty();
        let mut s = Studio {
            stable: doc.clone(),
            doc,
            design: Design::default(),
            issues: Vec::new(),
            names: Vec::new(),
            dirty: true,
            plan_ms: 0.0,
            modified: false,
            selected: None,
            hovered: None,
            view: View::default(),
            canvas_rect: Rect::NOTHING,
            fit_pending: true,
            tool: Tool::Select,
            angle_drag: None,
            drag: None,
            panning: false,
            space_pan: None,
            layout: Default::default(),
            path_edit: Default::default(),
            shapes: Default::default(),
            brush: Default::default(),
            trace: None,
            snap: Default::default(),
            cursor: Default::default(),
            flyouts: Default::default(),
            gesture: Default::default(),
            palettes: Default::default(),
            picker: None,
            agent: Default::default(),
            hud_lift: 0.0,
            open_issues: false,
            replans: 0,
            plan_cache: Default::default(),
            batching: false,
            planner: Default::default(),
            menu_synced: f64::NEG_INFINITY,
            stitch_mesh: Default::default(),
            gpu: None,
            capture: None,
            bounds_memo: Default::default(),
            satin_memo: Default::default(),
            lettering: Default::default(),
            view_tools: Default::default(),
            dock: Default::default(),
            dock_saved: Default::default(),
            dock_ui: Default::default(),
            native_menu: None,
            show_jumps: true,
            bridges_peek: None,
            player: None,
            playing: false,
            speed: 400.0,
            player_acc: 0.0,
            undo: Vec::new(),
            redo: Vec::new(),
            editing: false,
            pending_history: None,
            status: "Open an SVG (or drop one on the window).".into(),
            title: String::new(),
            mcp: None,
            mcp_ui: Default::default(),
            egui_ctx: None,
            machine: Default::default(),
            sew_mesh: Default::default(),
            limited_mesh: Default::default(),
            preview_sewn_mesh: Default::default(),
            preview_mesh: Default::default(),
            session: Default::default(),
            tabs: Default::default(),
            density: Default::default(),
        };
        if let Some(p) = path {
            s.load(&p);
        }
        if let Some(i) = select.filter(|&i| i < s.doc.shapes.len()) {
            s.select_index(Some(i));
        }
        s.replan();
        s
    }

    pub fn load(&mut self, path: &Path) {
        if path
            .extension()
            .is_some_and(|e| zigzag::trace::is_image(&e.to_string_lossy()))
        {
            self.open_image(path, crate::trace_ui::Target::Open);
            return;
        }
        // Open in another tab already: that one comes forward.
        if let Some(i) = self.tab_with_path(path)
            && i != self.active_tab()
        {
            self.switch_tab(i);
            self.status = format!("{} is open in this tab.", path.display());
            return;
        }
        match Document::open(path) {
            Ok(doc) => {
                let n = doc.shapes.len();
                if !self.open_document(doc) {
                    self.status = format!(
                        "{} is already open, with unsaved changes (kept).",
                        path.display()
                    );
                    return;
                }
                self.status = format!("Opened {} ({n} objects).", path.display());
                // Artwork too big for the hoop is scaled down to fit (one
                // undo step). Stitch files keep their size.
                let svg = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
                if svg && let Some(s) = self.fit_to_hoop(true) {
                    self.status = format!(
                        "Opened {}. It was bigger than the hoop, so it's scaled to {:.0}%. Undo puts it back to full size.",
                        path.display(),
                        s * 100.0
                    );
                }
            }
            Err(e) => self.status = format!("Couldn't open {}: {e:#}", path.display()),
        }
    }

    /// Replace the whole document (open, new). Clears history.
    pub fn set_document(&mut self, doc: Document) {
        self.doc = doc;
        self.stable = self.doc.clone();
        self.selected = None;
        self.layout.multi.clear();
        self.drag = None;
        self.undo.clear();
        self.redo.clear();
        self.player = None;
        self.playing = false;
        self.dirty = true;
        self.fit_pending = true;
        self.modified = false;
    }

    pub fn replan(&mut self) {
        let t = Instant::now();
        // An internal error planning (a panic on some odd geometry) keeps
        // the last plan on screen and says so, rather than ending the app
        // (and then again on the next launch, when the document comes back).
        let planned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let design = self.doc.plan_cached(&mut self.plan_cache);
            let issues = checks::check(
                &self.doc.shapes,
                &design,
                &self.doc.plan_options(),
                Some(self.hoop_dims()),
            );
            (design, issues)
        }));
        match planned {
            Ok((design, issues)) => {
                self.design = design;
                self.issues = issues;
            }
            Err(e) => {
                self.status = format!(
                    "Couldn't update the stitches ({}); showing the last plan.",
                    crate::planner::panic_text(&e)
                );
                self.dirty = false;
                self.planner.superseded();
                return;
            }
        }
        self.plan_ms = t.elapsed().as_secs_f64() * 1000.0;
        self.replans += 1;
        // A slow plan sends the next ones to the worker; one made here
        // supersedes any still out.
        self.planner.took(self.plan_ms);
        self.planner.superseded();
        self.names = svg::display_names(&self.doc.shapes);
        if let Some(p) = self.player {
            self.player = Some(p.min(self.design.stitch_count()));
        }
        self.dirty = false;
    }

    fn snapshot(&self) -> Snap {
        snapshot_of(&self.doc)
    }

    /// Draw a design's stitches: on the GPU, or by the painter (tests, or
    /// a GL context that can't). `slot` and `generation`: see
    /// [`crate::gpu::StitchGpu::draw`].
    fn paint_stitches(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        d: &Design,
        opt: &DrawOptions,
        slot: usize,
        generation: u64,
    ) -> Option<Pos2> {
        if let Some(gpu) = &self.gpu
            && self.view_tools.gpu_stitches
        {
            return gpu
                .borrow_mut()
                .draw(painter, rect, &self.view, d, opt, slot, generation);
        }
        let cache = if opt.limit != usize::MAX {
            // The player, or what's sewn so far: kept apart from the whole
            // design's meshes, which are drawn in the same frame.
            &self.limited_mesh
        } else if slot == 0 {
            &self.stitch_mesh
        } else {
            &self.sew_mesh
        };
        canvas::draw_design_cached(
            painter,
            rect,
            &self.view,
            d,
            opt,
            generation,
            &mut cache.borrow_mut(),
        )
    }

    /// Object `i` is the text being typed on the canvas.
    pub(crate) fn typing_into(&self, i: usize) -> bool {
        match (&self.lettering.typing, self.doc.shapes.get(i)) {
            (Some(t), Some(s)) => t.uid == s.uid,
            _ => false,
        }
    }

    /// Move the planned stitches of the blocks made only of `indices` by
    /// `m`, as a stand-in until they're planned again (a drop while the
    /// worker is still planning: the stitches stay with their objects).
    pub(crate) fn shift_planned(&mut self, indices: &[usize], m: zigzag::geom::Affine) {
        let mut any = false;
        for (k, blk) in self.design.blocks.iter_mut().enumerate() {
            let shapes = self
                .design
                .block_shapes
                .get(k)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if shapes.is_empty() || !shapes.iter().all(|i| indices.contains(i)) {
                continue;
            }
            for r in &mut blk.runs {
                for p in &mut r.pts {
                    *p = zigzag::geom::affine_apply(m, *p);
                }
            }
            any = true;
        }
        if any {
            self.replans += 1;
        }
    }

    /// The current plan's bounds (units): measured once per plan, not every
    /// frame (a scan of every stitch).
    pub(crate) fn design_bounds(&self) -> Bounds {
        if let Some((plan, b)) = self.bounds_memo.get()
            && plan == self.replans
        {
            return b;
        }
        let b = self.design.bounds();
        self.bounds_memo.set(Some((self.replans, b)));
        b
    }

    /// Keep `doc` as an undo step. The history keeps the last [`UNDO_STEPS`]
    /// (each is a whole document: half a megabyte for a traced logo).
    pub(crate) fn push_undo(&mut self, doc: Document) {
        self.undo.push(doc);
        // (A batch counts on its steps' places in the history: trimmed after.)
        if !self.batching && self.undo.len() > UNDO_STEPS {
            let extra = self.undo.len() - UNDO_STEPS;
            self.undo.drain(..extra);
        }
    }

    /// Apply a structural change (add, delete, replace…) as one undo step.
    pub fn structural(
        &mut self,
        f: impl FnOnce(&mut Document) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let before = self.doc.clone();
        match f(&mut self.doc) {
            Ok(()) => {
                self.push_undo(before);
                self.redo.clear();
                self.stable = self.doc.clone();
                self.dirty = true;
                self.modified = true;
                Ok(())
            }
            Err(e) => {
                self.doc = before;
                Err(e)
            }
        }
    }

    pub fn apply_history(&mut self, h: History) -> bool {
        let (from, to) = match h {
            History::Undo => (&mut self.undo, &mut self.redo),
            History::Redo => (&mut self.redo, &mut self.undo),
        };
        let Some(doc) = from.pop() else { return false };
        to.push(std::mem::replace(&mut self.doc, doc));
        self.stable = self.doc.clone();
        if self.sel_index().is_none() {
            self.selected = None;
        }
        self.dirty = true;
        self.modified = true;
        self.status = match h {
            History::Undo => "Undone.".into(),
            History::Redo => "Redone.".into(),
        };
        true
    }

    pub fn sel_index(&self) -> Option<usize> {
        self.selected
            .and_then(|uid| self.doc.shapes.iter().position(|s| s.uid == uid))
    }

    pub fn select_index(&mut self, i: Option<usize>) {
        self.selected = i.and_then(|i| self.doc.shapes.get(i)).map(|s| s.uid);
    }

    /// Show lengths in `u` (View › Units).
    pub fn set_units(&mut self, u: crate::units::Unit) {
        self.dock.units = u;
        crate::units::set_current(u);
        self.status = format!("Showing lengths in {}.", u.label().to_lowercase());
    }

    /// The density map of the current plan, worked out once per plan.
    pub(crate) fn density_map(&self) -> std::rc::Rc<crate::density::DensityMap> {
        let mut slot = self.density.borrow_mut();
        if let Some((k, m)) = slot.as_ref()
            && *k == self.replans
        {
            return m.clone();
        }
        let m = std::rc::Rc::new(crate::density::DensityMap::of(&self.design));
        *slot = Some((self.replans, m.clone()));
        m
    }

    /// Number each object at its center, in sewing order (View › Sewing
    /// Order), like Ink/Stitch's stacking-order labels.
    fn draw_order_labels(&self, painter: &egui::Painter, rect: Rect) {
        let p = crate::theme::pal(painter.ctx());
        let font = egui::FontId::proportional(11.0);
        // Objects with the same center (a ring round a disc) would hide
        // each other's numbers: later ones step down.
        let mut placed: Vec<Rect> = Vec::new();
        for (k, &i) in self.design.sew_order.iter().enumerate() {
            let Some(s) = self.doc.shapes.get(i) else {
                continue;
            };
            if s.params.kind == Some(zigzag::params::Kind::Skip)
                || self.design.per_shape.get(i).copied().unwrap_or(0) == 0
            {
                continue;
            }
            let at = self.view.to_screen(rect, shape_bounds(s).center());
            if !rect.contains(at) {
                continue;
            }
            let g = painter.layout_no_wrap((k + 1).to_string(), font.clone(), Color32::WHITE);
            let size = Vec2::new(g.size().x.max(g.size().y) + 8.0, g.size().y + 4.0);
            let mut r = Rect::from_center_size(at, size);
            while placed.iter().any(|q| q.intersects(r)) {
                r = r.translate(Vec2::new(0.0, size.y + 2.0));
            }
            placed.push(r);
            painter.rect_filled(r, size.y / 2.0, p.accent);
            painter.galley(r.min + (size - g.size()) / 2.0, g, Color32::WHITE);
        }
    }

    pub fn request_fit(&mut self) {
        self.fit_pending = true;
    }

    pub fn hoop_dims(&self) -> (f64, f64) {
        let h = HOOPS[self.doc.settings.hoop.min(HOOPS.len() - 1)];
        (h.2, h.3)
    }

    pub(crate) fn design_over_hoop(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.shape.is_none() && i.level == Level::Error)
    }

    pub fn save(&mut self, path: Option<&Path>) {
        match self.doc.save(path) {
            Ok(p) => {
                self.stable = self.doc.clone();
                self.modified = false;
                self.status = format!(
                    "Saved {} (settings are stored in the object names)",
                    p.display()
                );
                self.dirty = true;
            }
            Err(e) => self.status = format!("Couldn't save: {e:#}"),
        }
    }

    pub(crate) fn save_as_dialog(&mut self) {
        let name = format!(
            "{}.svg",
            if self.doc.name.is_empty() {
                "design"
            } else {
                &self.doc.name
            }
        );
        let mut dialog = rfd::FileDialog::new()
            .add_filter("SVG", &["svg"])
            .set_file_name(&name);
        if let Some(dir) = self.doc.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(p) = dialog.save_file() {
            self.save(Some(&p));
        }
    }

    pub fn export_to(&mut self, ext: &str, path: &Path) -> anyhow::Result<()> {
        self.plan_now();
        let bytes = if ext == "html" {
            self.worksheet_html()?.into_bytes()
        } else {
            zigzag::formats::write_format(&self.design, ext)?
        };
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub(crate) fn export_dialog(&mut self, ext: &str) {
        let default = format!(
            "{}.{ext}",
            if self.doc.name.is_empty() {
                "design"
            } else {
                &self.doc.name
            }
        );
        let mut dialog = rfd::FileDialog::new()
            .add_filter(ext.to_uppercase(), &[ext])
            .set_file_name(&default);
        if let Some(dir) = self.doc.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        self.status = match self.export_to(ext, &path) {
            Ok(()) => format!("Exported {}", path.display()),
            Err(e) => format!("Couldn't export {}: {e}", path.display()),
        };
    }

    /// Set the stitch type of every selected object that can take it.
    pub fn set_kind_selected(&mut self, kind: Kind) {
        let idx: Vec<usize> = self
            .sel_indices()
            .into_iter()
            .filter(|&i| {
                let s = &self.doc.shapes[i];
                s.params.kind != Some(kind) && allowed_kinds(s).contains(&kind)
            })
            .collect();
        if idx.is_empty() {
            return;
        }
        // An edit of its own (undo step, unsaved, re-planned) wherever it's
        // called from: a key or the menu bar runs outside the frame's own
        // tracking of edits made in the panels.
        let _ = self.structural(|d| {
            for i in idx {
                d.shapes[i].params.kind = Some(kind);
            }
            Ok(())
        });
    }

    /// Edit › Paste Stitch Settings: the copied object's stitch type and
    /// settings onto the selection. Each object keeps its own thread, order
    /// and start/end points.
    pub fn paste_settings(&mut self) {
        let Some(src) = self.layout.settings_clip.clone() else {
            return;
        };
        let idx = self.sel_indices();
        if idx.is_empty() {
            return;
        }
        let mut n = 0;
        let _ = self.structural(|d| {
            for &i in &idx {
                let own = d.shapes[i].params.clone();
                let kind = src
                    .kind
                    .filter(|k| allowed_kinds(&d.shapes[i]).contains(k))
                    .or(own.kind);
                d.shapes[i].params = Params {
                    kind,
                    thread: own.thread,
                    order: own.order,
                    start: own.start,
                    end: own.end,
                    trim: own.trim,
                    stop: own.stop,
                    ..src.clone()
                };
                n += 1;
            }
            Ok(())
        });
        self.status = format!("Pasted stitch settings onto {n} object(s).");
    }

    /// Edit › Select Same Stitch Type.
    pub fn select_same_kind(&mut self) {
        let Some(i) = self.sel_index() else { return };
        let kind = self.doc.shapes[i].params.kind.unwrap_or(Kind::Fill);
        let same: Vec<usize> = (0..self.doc.shapes.len())
            .filter(|&j| self.doc.shapes[j].params.kind.unwrap_or(Kind::Fill) == kind)
            .collect();
        let n = same.len();
        self.set_selection(&same);
        self.status = format!(
            "{n} {} object(s) selected.",
            crate::widgets::kind_name(kind).to_lowercase()
        );
    }

    /// Object › Path › Reverse Direction: each selected path the other way
    /// round, so a satin or run sews from its other end.
    pub fn reverse_paths(&mut self) {
        let idx = self.sel_indices();
        if idx.is_empty() {
            return;
        }
        let r = self.structural(|d| {
            d.edit_paths_with(&idx, |p| {
                Some(p.iter().map(zigzag::bezier::reversed).collect())
            })?;
            Ok(())
        });
        self.status = match r {
            Ok(()) => "Reversed: it sews from the other end now.".into(),
            Err(e) => format!("Couldn't reverse: {e:#}"),
        };
    }

    pub(crate) fn move_selected(&mut self, delta: isize) {
        if let Some(i) = self.sel_index() {
            let j = i as isize + delta;
            if j >= 0 && (j as usize) < self.doc.shapes.len() {
                let _ = self.structural(|d| {
                    d.shapes.swap(i, j as usize);
                    Ok(())
                });
            }
        }
    }

    /// Add an SVG file's artwork to the document (Illustrator's Place).
    pub fn place_svg(&mut self, path: &Path) {
        if path
            .extension()
            .is_some_and(|e| zigzag::trace::is_image(&e.to_string_lossy()))
        {
            self.open_image(path, crate::trace_ui::Target::Place);
            return;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Couldn't read {}: {e}", path.display());
                return;
            }
        };
        let mut added = Vec::new();
        match self.structural(|d| {
            added = d.add_svg(&text, zigzag::document::Placement::default())?;
            Ok(())
        }) {
            Ok(()) => {
                self.set_selection(&added);
                self.status = format!("Placed {} ({} objects).", path.display(), added.len());
                if let Some(s) = self.fit_to_hoop(true) {
                    self.status = format!(
                        "Placed {}. It was bigger than the hoop, so it's scaled to {:.0}%. Undo puts it back to full size.",
                        path.display(),
                        s * 100.0
                    );
                }
            }
            Err(e) => self.status = format!("Couldn't place {}: {e:#}", path.display()),
        }
    }
}

impl eframe::App for Studio {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(c) = &mut self.capture {
            c.frame(ctx);
        }
        if !self.handle_mcp() {
            // Calls may be waiting on a gesture or a dialog: look again
            // soon (their arrival was the only wake-up).
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(mut m) = self.native_menu.take() {
            for a in m.poll() {
                if !self.previewing() {
                    self.run_action(a);
                }
            }
            // The menu bar's enabled / checked states, ten times a second at
            // most (not every frame: each item asks about the selection).
            let now = ctx.input(|i| i.time);
            if now - self.menu_synced >= 0.1 || now < self.menu_synced {
                // While typing (a field, or text on the canvas), the Edit
                // menu's keys belong to the text.
                let typing = ctx.egui_wants_keyboard_input() || self.lettering.typing.is_some();
                m.sync(self, typing);
                self.menu_synced = now;
            }
            self.native_menu = Some(m);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Quitting from the menu bar ends the app without a closing frame.
        self.flush_session();
        if let Some(g) = &self.gpu {
            g.borrow_mut().destroy();
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        crate::app_icon::sync(ui.ctx());
        crate::profile::whole_frame(frame.info().cpu_usage);
        self.frame(ui);
        // A copy from a panel button, this frame.
        self.flush_system_clipboard(ui.ctx());
        if crate::profile::enabled() {
            // Profiling: frame after frame, for steady readings.
            ui.ctx().request_repaint();
        }
    }
}

impl Studio {
    /// Answer queued MCP tool calls (on the UI thread, between frames).
    /// False when they have to wait.
    pub fn handle_mcp(&mut self) -> bool {
        if self.mcp.is_none() {
            return true;
        }
        // Calls wait while a dialog previews into the document, and while
        // something is dragged: the drag works from the document as it was
        // when it began, and would drop an edit made under it.
        if self.previewing() || self.drag.is_some() {
            return false;
        }
        let calls: Vec<crate::mcp::Call> = match &self.mcp {
            Some(server) => server.rx.try_iter().collect(),
            None => return true,
        };
        for call in calls {
            let at = Instant::now();
            // Its client gave up waiting (a dialog was open): it was told
            // the call failed, so running it now would surprise it.
            if at > call.deadline {
                continue;
            }
            let result = self.run_mcp_call(&call.name, &call.args);
            self.mcp_ui.note(&call.name, at, &result);
            let _ = call.reply.send(result);
        }
        true
    }

    /// One UI frame. Separate from `eframe::App::ui` so tests can drive it with plain egui.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        crate::units::set_current(self.dock.units);
        crate::profile::begin();

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        let openable = |e: &std::ffi::OsStr| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            e == "svg"
                || zigzag::formats::READABLE_FORMATS.contains(&e.as_str())
                || zigzag::trace::is_image(&e)
        };
        if let Some(p) = dropped
            .into_iter()
            .find(|p| p.extension().is_some_and(openable))
        {
            self.load(&p);
        }

        self.flush_system_clipboard(&ctx);
        self.shortcuts(&ctx);
        self.apply_tab_op(&ctx);
        self.tidy_isolation();
        if let Some(h) = self.pending_history.take() {
            self.apply_history(h);
        }
        self.take_plan();
        crate::profile::mark("input");
        if self.dirty {
            self.plan_changes(&ctx);
        }
        crate::profile::mark("plan");
        self.tick_player(&ctx);
        self.agent_tick(&ctx);
        self.update_title(&ctx);

        let before = self.snapshot();
        crate::profile::mark("snapshot");

        let pal = crate::theme::pal(&ctx);
        egui::Panel::top("titlebar")
            .exact_size(48.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.toolbar)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show(ui, |ui| self.titlebar(ui));
        egui::Panel::bottom("statusbar")
            .exact_size(30.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.toolbar)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("tools")
            .exact_size(44.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(pal.dock))
            .show(ui, |ui| self.tool_strip(ui));
        // The docks: full panels, or collapsed to a strip of icons.
        self.tidy_agent_dock();
        use crate::dock::Place;
        for side in [Place::Left, Place::Right] {
            if !self.dock_shown(side) {
                continue;
            }
            // Separate ids so each mode keeps its own width.
            let id = match (side, self.dock_iconic(side)) {
                (Place::Left, false) => "left-dock",
                (Place::Left, true) => "left-dock-icons",
                (_, false) => "right-dock",
                (_, true) => "right-dock-icons",
            };
            let mut panel = if side == Place::Left {
                egui::Panel::left(id)
            } else {
                egui::Panel::right(id)
            };
            panel = if self.dock_iconic(side) {
                panel.exact_size(crate::dock::ICON_STRIP_W).resizable(false)
            } else if side == Place::Left {
                panel.default_size(340.0).min_size(240.0).max_size(560.0)
            } else {
                panel.default_size(300.0).min_size(270.0).max_size(460.0)
            };
            panel
                .frame(egui::Frame::new().fill(pal.dock))
                .show(ui, |ui| self.dock_column(ui, side));
        }
        crate::profile::mark("chrome+docks");
        // The open documents' tabs, over the canvas between the docks.
        egui::Panel::top("doc-tabs")
            .exact_size(crate::tabs::STRIP_H)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(pal.dock))
            .show(ui, |ui| self.doc_tabs(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| self.canvas(ui));
        crate::profile::mark("canvas");
        self.floating_panels(&ctx);
        self.torn_toolbars(&ctx);
        self.panel_drag(&ctx);
        self.text_window(&ctx);
        self.typing_box(&ctx, self.canvas_rect);
        crate::profile::mark("typing_box");
        self.offset_window(&ctx);
        self.corner_window(&ctx);
        self.simplify_window(&ctx);
        self.machine_window(&ctx);
        self.cleanup_window(&ctx);
        self.swatch_window(&ctx);
        self.shape_window(&ctx);
        self.trace_window(&ctx);
        self.close_prompt(&ctx);

        // (Not on every frame of dragging a panel or a divider: when it's let go.)
        if self.dock != self.dock_saved && !ctx.input(|i| i.pointer.any_down()) {
            self.dock.save();
            self.dock_saved = self.dock.clone();
        }

        // Coalesce a slider drag (or any burst of edits) into one undo step.
        let after = self.snapshot();
        // Structural edits (delete, move, paste…) push their own undo step and
        // update `stable`; only in-place edits are coalesced here.
        if after != before && after != snapshot_of(&self.stable) && self.drag.is_none() {
            if !self.editing {
                self.push_undo(self.stable.clone());
                self.redo.clear();
                self.editing = true;
            }
            self.dirty = true;
            self.modified = true;
            ctx.request_repaint();
        } else if self.editing && !ctx.input(|i| i.pointer.any_down()) {
            self.editing = false;
            self.stable = self.doc.clone();
        }
        self.keep_session(&ctx);
        crate::profile::mark("undo+session");
        crate::profile::end();
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = if self.doc.name.is_empty() {
            "untitled"
        } else {
            &self.doc.name
        };
        let title = format!("{}{} — ZigZag", if self.modified { "• " } else { "" }, name);
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if (self.picker.is_some()
            || self.layout.offset_dialog.is_some()
            || self.previewing()
            || self.layout.cleanup_dialog.is_some()
            || self.shapes.dialog.is_some()
            || self.trace.is_some()
            || self.dock_ui.flyout.is_some())
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            self.picker = None;
            self.layout.offset_dialog = None;
            // Cancel any preview.
            self.cancel_previews();
            self.layout.cleanup_dialog = None;
            self.shapes.dialog = None;
            if self.trace.take().is_some() {
                self.status = "Trace canceled.".into();
            }
            self.dock_ui.flyout = None;
            return;
        }
        // A modal dialog with a live preview takes no other commands.
        if self.previewing() {
            return;
        }
        // Typing with the Type tool: every key is text (Escape ends it in
        // the typing box).
        if self.lettering.typing.is_some() {
            return;
        }
        if self.pen_keys(ctx) || self.direct_keys(ctx) || self.shape_keys(ctx) {
            return;
        }
        self.action_shortcuts(ctx);
        self.layout_shortcuts(ctx);
        if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.tool == Tool::Select && self.exit_group() {
                return;
            }
            self.set_selection(&[]);
            self.tool = Tool::Select;
            self.view_tools.measure = None;
        }
    }

    pub(crate) fn toggle_play(&mut self) {
        if self.playing {
            self.playing = false;
        } else {
            let total = self.design.stitch_count();
            if self.player.is_none_or(|p| p >= total) {
                self.player = Some(0);
            }
            self.playing = true;
        }
    }

    fn tick_player(&mut self, ctx: &egui::Context) {
        if !self.playing {
            return;
        }
        let total = self.design.stitch_count();
        self.player_acc += self.speed * ctx.input(|i| i.stable_dt).min(0.1);
        let step = self.player_acc.floor() as usize;
        self.player_acc -= step as f32;
        let p = (self.player.unwrap_or(0) + step).min(total);
        self.player = Some(p);
        if p >= total {
            self.playing = false;
        }
        ctx.request_repaint();
    }
}

// ---------------------------------------------------------------- canvas

impl Studio {
    /// Open or close the Skitch window (File › Send to Skitch, and the
    /// toolbar's Send button), connecting when it opens.
    pub fn send_action(&mut self) {
        self.toggle_machine_window();
    }

    /// The object under `p`: inside the entered group first, then anywhere.
    pub(crate) fn pick(&self, p: P, tol: f64) -> Option<usize> {
        if self.scope().is_some()
            && let Some(i) = canvas::hit_test_where(&self.doc.shapes, p, tol, |i| self.in_scope(i))
        {
            return Some(i);
        }
        canvas::hit_test(&self.doc.shapes, p, tol)
    }

    /// The gaps Connect sews across in the open, lit up while its bar is
    /// in hand: these are the stitches that will show.
    fn draw_bridges(&mut self, painter: &egui::Painter, rect: Rect) {
        let Some(until) = self.bridges_peek else {
            return;
        };
        let now = Instant::now();
        if now >= until {
            self.bridges_peek = None;
            return;
        }
        painter.ctx().request_repaint_after(until - now);
        let p = crate::theme::pal(painter.ctx());
        // Over any thread color: a dark edge, then the highlight.
        let edge = Color32::from_rgba_unmultiplied(0, 0, 0, 170);
        for &(a, b) in &self.design.bridges {
            let (a, b) = (self.view.to_screen(rect, a), self.view.to_screen(rect, b));
            painter.line_segment([a, b], egui::Stroke::new(5.0, edge));
            painter.line_segment([a, b], egui::Stroke::new(2.5, p.warning));
            for q in [a, b] {
                painter.circle_filled(q, 4.5, edge);
                painter.circle_filled(q, 3.0, p.warning);
            }
        }
    }

    /// Dim everything outside the entered group.
    fn draw_isolation(&self, painter: &egui::Painter, rect: Rect) {
        let Some(g) = self.scope() else {
            return;
        };
        let members = self.group_members(g);
        let mut b = Bounds::empty();
        for &i in &members {
            let sb = shape_bounds(&self.doc.shapes[i]);
            if !sb.is_empty() {
                b.add(sb.min);
                b.add(sb.max);
            }
        }
        if b.is_empty() {
            return;
        }
        let a = self.view.to_screen(rect, b.min);
        let z = self.view.to_screen(rect, b.max);
        let spot = Rect::from_two_pos(a, z).expand(10.0);
        let desk = crate::theme::pal(painter.ctx()).desk;
        let wash = Color32::from_rgba_unmultiplied(desk.r(), desk.g(), desk.b(), 185);
        for r in [
            Rect::from_min_max(rect.min, egui::pos2(rect.max.x, spot.min.y)),
            Rect::from_min_max(egui::pos2(rect.min.x, spot.max.y), rect.max),
            Rect::from_min_max(
                egui::pos2(rect.min.x, spot.min.y),
                egui::pos2(spot.min.x, spot.max.y),
            ),
            Rect::from_min_max(
                egui::pos2(spot.max.x, spot.min.y),
                egui::pos2(rect.max.x, spot.max.y),
            ),
        ] {
            let r = r.intersect(rect);
            if r.is_positive() {
                painter.rect_filled(r, 0.0, wash);
            }
        }
        painter.rect_stroke(
            spot,
            4.0,
            Stroke::new(1.0, Color32::from_gray(128).gamma_multiply(0.6)),
            egui::StrokeKind::Outside,
        );
    }

    /// The breadcrumb shown while inside a group: Document › flower › petals.
    fn isolation_bar(&mut self, ctx: &egui::Context, id: egui::Id, rect: Rect) {
        if self.layout.isolated.is_empty() {
            return;
        }
        let mut go_to: Option<usize> = None;
        egui::Area::new(id)
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_top() + Vec2::new(12.0, 10.0))
            .show(ctx, |ui| {
                let p = crate::theme::pal_ui(ui);
                egui::Frame::new()
                    .fill(p.toolbar)
                    .stroke(Stroke::new(1.0, p.separator))
                    .corner_radius(7.0)
                    .inner_margin(egui::Margin::symmetric(
                        crate::theme::space::S as i8,
                        crate::theme::space::XS as i8,
                    ))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = crate::theme::space::XS;
                            if ui
                                .add(egui::Button::new("Exit Group").small())
                                .on_hover_text("Leave this group (Esc)")
                                .clicked()
                            {
                                go_to = Some(self.layout.isolated.len() - 1);
                            }
                            ui.add_space(crate::theme::space::XS);
                            let n = self.layout.isolated.len();
                            let crumbs: Vec<String> = std::iter::once("Document".to_string())
                                .chain(self.layout.isolated.iter().map(|g| Self::group_label(g)))
                                .collect();
                            for (k, name) in crumbs.iter().enumerate() {
                                if k > 0 {
                                    let (r, _) =
                                        ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                                    crate::icons::paint(
                                        ui.painter(),
                                        r,
                                        crate::icons::Icon::ChevronRight,
                                        p.tertiary,
                                    );
                                }
                                let last = k == n;
                                let text = egui::RichText::new(name).size(12.0).color(if last {
                                    p.text
                                } else {
                                    p.secondary
                                });
                                if last {
                                    ui.label(text.strong());
                                } else if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                    go_to = Some(k);
                                }
                            }
                        });
                    });
            });
        if let Some(k) = go_to {
            while self.layout.isolated.len() > k {
                self.exit_group();
            }
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        self.canvas_rect = rect;
        let ctx = ui.ctx().clone();
        let pal = crate::theme::pal(&ctx);
        painter.rect_filled(rect, 0.0, pal.desk);

        let (hw, hh) = self.hoop_dims();
        if self.fit_pending && rect.width() > 10.0 {
            let b = self.design_bounds();
            if b.is_empty() {
                self.view.fit(rect, P::default(), hw, hh);
            } else {
                self.view.fit(
                    rect,
                    b.center(),
                    (b.width() / 10.0).max(20.0),
                    (b.height() / 10.0).max(20.0),
                );
            }
            self.fit_pending = false;
        }

        // Zoom (wheel / pinch) and pan (drag; middle/right drag always pans).
        if let Some(hover) = resp.hover_pos() {
            let (scroll, zoom) = ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            let factor = zoom * (scroll.y * 0.0015).exp();
            if (factor - 1.0).abs() > 1e-4 {
                self.view.zoom_at(rect, hover, factor);
            }
        }
        let tol = 6.0 * 10.0 / self.view.scale as f64;
        self.hovered = resp.hover_pos().and_then(|h| {
            let p = self.view.to_design(rect, h);
            self.pick(p, tol).or_else(|| {
                // Selecting: text is picked anywhere in its box, gaps
                // between the letters included.
                (self.tool == Tool::Select)
                    .then(|| self.text_at(p, tol))
                    .flatten()
            })
        });
        crate::profile::mark("c:pick");
        let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));

        // Space: hold and drag to pan (any tool); tap to play the stitch-out.
        let space_down = !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_down(Key::Space));
        // (Not while the pen is placing a point: there Space moves the point.)
        if space_down
            && self.space_pan.is_none()
            && self.drag.is_none()
            && self.view_tools.pen_press.is_none()
        {
            self.space_pan = Some(false);
        }
        if !space_down {
            if self.space_pan == Some(false)
                && self.action_enabled(crate::commands::Action::PlayPause)
            {
                self.toggle_play();
            }
            self.space_pan = None;
        }
        let space = self.space_pan.is_some();
        if space {
            ctx.set_cursor_icon(if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
            if resp.dragged_by(egui::PointerButton::Primary) {
                self.space_pan = Some(true);
            }
        }

        if self.tool == Tool::Direct && !space {
            let mods = crate::layout::DragMods {
                shift,
                alt,
                snap: self.view_tools.smart_guides,
                tol,
            };
            self.direct_canvas(&ctx, &resp, rect, tol, mods);
        }
        if self.tool == Tool::Anchor && !space {
            let mods = crate::layout::DragMods {
                shift,
                alt,
                snap: self.view_tools.smart_guides,
                tol,
            };
            self.anchor_canvas(&ctx, &resp, rect, tol, mods);
        }
        if matches!(self.tool, Tool::AddAnchor | Tool::DeleteAnchor) && !space {
            self.point_tool_canvas(&ctx, &resp, rect, tol, alt);
        }
        let marker_busy = !space && self.markers_canvas(&ctx, &resp, rect);
        if self.tool == Tool::Select && !space && !marker_busy {
            if self.drag.is_none()
                && let Some(hp) = resp.hover_pos()
                && let Some(h) = self.handle_at(rect, hp)
            {
                match h {
                    // A curved arrow facing out from the corner.
                    crate::layout::Handle::Rotate => {
                        let c = self.view.to_screen(rect, self.selection_bounds().center());
                        let d = hp - c;
                        self.want_cursor(crate::cursors::Glyph::Rotate(d.y.atan2(d.x)));
                    }
                    _ => ctx.set_cursor_icon(Self::handle_cursor(h)),
                }
            }
            if let Some(d) = &self.drag
                && let crate::layout::DragMode::Rotate { center } = d.mode
                && let Some(hp) = resp.interact_pointer_pos()
            {
                let v = hp - self.view.to_screen(rect, center);
                self.want_cursor(crate::cursors::Glyph::Rotate(v.y.atan2(v.x)));
            }
            if resp.drag_started_by(egui::PointerButton::Primary) {
                let start = ctx
                    .input(|i| i.pointer.press_origin())
                    .or(resp.interact_pointer_pos())
                    .unwrap_or(rect.center());
                let p = self.view.to_design(rect, start);
                let handle = self.handle_at(rect, start);
                if handle.is_some() {
                    self.begin_drag(handle, p);
                } else if let Some(i) = self.pick(p, tol).or_else(|| self.text_at(p, tol)) {
                    if !self.sel_indices().contains(&i) {
                        let direct = ctx.input(|inp| inp.modifiers.command);
                        self.click_select(i, shift, direct);
                    }
                    self.begin_drag(None, p);
                } else {
                    self.panning = true;
                }
            }
            if self.drag.is_some()
                && resp.dragged_by(egui::PointerButton::Primary)
                && let Some(pos) = resp.interact_pointer_pos()
            {
                let mods = crate::layout::DragMods {
                    shift,
                    alt,
                    snap: self.view_tools.smart_guides,
                    tol,
                };
                self.update_drag(self.view.to_design(rect, pos), mods);
                if self.drag.as_ref().is_some_and(|d| d.copy) {
                    ctx.set_cursor_icon(egui::CursorIcon::Copy);
                }
                ctx.request_repaint();
            }
            if resp.drag_stopped() {
                self.end_drag();
                self.panning = false;
            }
            let text_under = self.hovered.filter(|&i| self.doc.lettering(i).is_some());
            if let Some(i) = text_under
                && (resp.double_clicked() || resp.triple_clicked())
            {
                // Double-click on text edits the text (the Type tool, caret
                // at the click), as in Illustrator.
                let p = resp
                    .interact_pointer_pos()
                    .map(|hp| self.view.to_design(rect, hp))
                    .unwrap_or_default();
                self.edit_text_at(i, p);
            } else if resp.double_clicked() {
                // Double-click goes into the group (Illustrator's isolation
                // mode); on empty canvas it comes back out.
                match self.hovered {
                    Some(i) => self.enter_group(i),
                    None => self.exit_isolation(),
                }
            } else if resp.clicked() {
                let direct = ctx.input(|inp| inp.modifiers.command);
                match self.hovered {
                    Some(i) => self.click_select(i, shift, direct),
                    None => self.set_selection(&[]),
                }
            }
        }
        if resp.secondary_clicked()
            && let Some(i) = self.hovered
            && !self.sel_indices().contains(&i)
        {
            self.click_select(i, false, false);
        }
        let pan = resp.dragged_by(egui::PointerButton::Middle)
            || resp.dragged_by(egui::PointerButton::Secondary)
            || (resp.dragged_by(egui::PointerButton::Primary)
                && (self.panning || self.tool == Tool::Hand || space));
        if self.tool == Tool::Hand {
            ctx.set_cursor_icon(if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }
        if pan {
            let d = resp.drag_delta();
            self.view.center =
                self.view.center - P::new(d.x as f64, d.y as f64) * (10.0 / self.view.scale as f64);
        }

        if self.tool == Tool::Angle {
            self.want_cursor(crate::cursors::Glyph::Cross(Some(
                crate::icons::Icon::Angle,
            )));
            if resp.drag_started_by(egui::PointerButton::Primary) {
                if self.selected_fill().is_none() {
                    self.select_index(self.hovered);
                }
                self.angle_drag = resp.interact_pointer_pos();
            }
            if let (Some(a), Some(b), Some(i)) = (
                self.angle_drag,
                resp.interact_pointer_pos(),
                self.selected_fill(),
            ) {
                let d = b - a;
                if d.length() > 8.0 {
                    let deg = normalize_angle(d.y.atan2(d.x).to_degrees() as f64);
                    let snap = ctx.input(|i| i.modifiers.shift);
                    let deg = if snap {
                        (deg / 15.0).round() * 15.0
                    } else {
                        deg.round()
                    };
                    self.doc.shapes[i].params.angle = Some(deg);
                    let accent = Color32::from_rgb(255, 200, 60);
                    painter.arrow(a, d, Stroke::new(2.0, accent));
                    painter.text(
                        b + Vec2::new(10.0, -10.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("{deg:.0}°"),
                        egui::FontId::proportional(14.0),
                        accent,
                    );
                }
            }
            if resp.drag_stopped() {
                self.angle_drag = None;
            }
        }

        resp.context_menu(|ui| {
            ui.set_min_width(240.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            if self.sel_index().is_some() {
                self.object_menu_items(ui);
            } else {
                use crate::commands::Action as A;
                use crate::flyout::RowIcon;
                use crate::icons::Icon;
                for (a, label, icon) in [
                    (A::Paste, "Paste", RowIcon::None),
                    (A::SelectAll, "Select All", RowIcon::Icon(Icon::Select)),
                    (A::AddText, "Add Text…", RowIcon::Icon(Icon::Text)),
                    (A::Place, "Place SVG…", RowIcon::Icon(Icon::Plus)),
                    (A::ZoomFit, "Zoom to Fit", RowIcon::Icon(Icon::ZoomFit)),
                ] {
                    self.action_row(ui, a, label, icon);
                }
            }
        });

        crate::profile::mark("c:tools");
        // Draw.
        let over = self.design_over_hoop();
        canvas::draw_hoop(
            &painter,
            rect,
            &self.view,
            hw,
            hh,
            HOOPS[self.doc.settings.hoop.min(2)].1,
            over,
            color32(self.doc.settings.cloth),
        );
        if self.view_tools.grid {
            crate::view_tools::draw_grid(&painter, rect, &self.view);
        }
        let opts = DrawOptions {
            show_jumps: self.show_jumps,
            limit: self.player.unwrap_or(usize::MAX),
            thread_mm: 0.4,
            view: self.view_tools.stitch_view,
            background: self.doc.settings.cloth,
            hidden: self.hidden_text(),
            // (Text being typed needs nothing special: its block's mesh is
            // keyed by its stitches, so a keystroke re-tessellates just
            // that block, once, and the frames between keystrokes reuse it.)
            moving: self.drag.as_ref().map(|d| {
                let on = self
                    .design
                    .block_shapes
                    .iter()
                    .map(|v| !v.is_empty() && v.iter().all(|i| d.indices.contains(i)))
                    .collect();
                (on, d.m)
            }),
        };
        // While the Skitch sews: the design as sent, faded, and what's
        // sewn so far over it.
        let sewing = self.sewing_view();
        let needle = if let Some((sent, sewn)) = &sewing {
            let d = &sent.design;
            let generation = self
                .machine
                .sent
                .as_ref()
                .map(|s| u64::from_le_bytes(s.uuid[..8].try_into().unwrap_or([0; 8])))
                .unwrap_or(0);
            let full = DrawOptions {
                limit: usize::MAX,
                moving: None,
                hidden: None,
                ..opts.clone()
            };
            self.paint_stitches(&painter, rect, d, &full, 1, generation);
            let b = sent.bounds;
            let r = Rect::from_two_pos(
                self.view.to_screen(rect, b.min),
                self.view.to_screen(rect, b.max),
            )
            .expand(6.0);
            crate::sew::fade_rect(&painter, r, self.doc.settings.cloth);
            let sewn_opts = DrawOptions {
                limit: (*sewn).max(1),
                moving: None,
                hidden: None,
                ..opts.clone()
            };
            self.paint_stitches(&painter, rect, d, &sewn_opts, 1, generation)
        } else {
            self.paint_stitches(&painter, rect, &self.design, &opts, 0, self.replans)
        };
        crate::profile::mark("c:stitches");
        if sewing.is_none() {
            self.draw_bridges(&painter, rect);
        }
        self.draw_isolation(&painter, rect);
        if self.view_tools.density {
            self.density_map().draw(&painter, rect, &self.view);
        }
        if self.view_tools.order_labels {
            self.draw_order_labels(&painter, rect);
        }
        if self.view_tools.points {
            crate::view_tools::draw_points(&painter, rect, &self.view, &self.design, opts.limit);
        }
        if let Some(i) = self.hovered.filter(|&h| Some(h) != self.sel_index()) {
            canvas::draw_outline(
                &painter,
                rect,
                &self.view,
                &self.doc.shapes[i],
                Stroke::new(1.0, Color32::from_gray(60)),
            );
        }
        for j in self.sel_indices().into_iter().skip(1) {
            canvas::draw_outline(
                &painter,
                rect,
                &self.view,
                &self.doc.shapes[j],
                Stroke::new(1.2, pal.accent),
            );
        }
        if self.tool == Tool::Select && !self.playing {
            self.draw_selection_box(&painter, rect);
            self.draw_guides(&painter, rect);
        }
        if let (Some(label), Some(pos)) = (self.drag_label(), resp.interact_pointer_pos()) {
            let galley =
                painter.layout_no_wrap(label, egui::FontId::proportional(12.0), Color32::WHITE);
            let bg = Rect::from_min_size(
                pos + Vec2::new(14.0, 14.0),
                galley.size() + Vec2::new(10.0, 6.0),
            );
            painter.rect_filled(bg, 4.0, Color32::from_black_alpha(190));
            painter.galley(bg.min + Vec2::new(5.0, 3.0), galley, Color32::WHITE);
        }
        // The Selection tool shows the selected paths' points too (filled,
        // like Illustrator's and VectorCraft's), not while they're moving.
        crate::profile::mark("c:overlays");
        if self.tool == Tool::Select && self.drag.is_none() {
            self.draw_selected_points(&painter, rect, pal.accent);
        }
        crate::profile::mark("c:sel_points");
        if let Some(i) = self.sel_index()
            && self.hidden_text() != Some(i)
        {
            let s = &self.doc.shapes[i];
            canvas::draw_outline(&painter, rect, &self.view, s, Stroke::new(1.5, pal.accent));
            match s.params.kind.unwrap_or(Kind::Fill) {
                Kind::Fill if matches!(s.geometry, Geometry::Fill { .. }) => {
                    self.draw_angle_indicator(&painter, rect, i)
                }
                // (Not while typing into it: the rails of every letter
                // would be found again at each keystroke.)
                Kind::Satin if !self.typing_into(i) => {
                    let info = self.satin_info(i);
                    canvas::draw_rails(&painter, rect, &self.view, &info);
                }
                _ => {}
            }
        }
        crate::profile::mark("c:selected");
        self.draw_agent_work(&painter, rect);
        if matches!(
            self.tool,
            Tool::Direct | Tool::Anchor | Tool::AddAnchor | Tool::DeleteAnchor
        ) || (self.tool == Tool::Pen && self.view_tools.pen.is_empty())
        {
            self.draw_path_edit(&painter, rect);
        }
        if matches!(self.tool, Tool::AddAnchor | Tool::DeleteAnchor) {
            let add = (self.tool == Tool::AddAnchor) != ctx.input(|i| i.modifiers.alt);
            self.hud(
                &painter,
                rect,
                match (self.sel_index().is_some(), add) {
                    (_, true) => {
                        "Click a path's outline to add a point · Option deletes points instead"
                    }
                    (_, false) => "Click a point to delete it · Option adds points instead",
                },
            );
        }
        if self.tool == Tool::Anchor {
            self.hud(
                &painter,
                rect,
                if self.sel_index().is_none() {
                    "Click a shape to edit its points"
                } else {
                    "Click a smooth point for a corner · drag from a point for smooth handles · drag a handle to move it alone, click its end to take it in · drag a curve to bend it"
                },
            );
        }
        if self.tool == Tool::Direct {
            self.draw_guides(&painter, rect);
            // Lettering can't be edited point by point: say so (no hint otherwise).
            if self.sel_index().is_some() && self.editable_path().is_none() {
                self.hud(
                    &painter,
                    rect,
                    "Lettering is edited with its text settings, not point by point.",
                );
            }
        }
        if self.view_tools.grid {
            crate::view_tools::draw_grid_labels(&painter, rect, &self.view);
        }
        self.view_tools_canvas(ui, &resp, &painter, rect);
        self.draw_markers(&painter, rect);
        self.draw_snap(&painter, rect);
        self.isolation_bar(&ctx, ui.make_persistent_id("isolation-bar"), rect);
        self.task_bar(&ctx, ui.make_persistent_id("task-bar"), rect);
        if !space {
            self.paint_cursor(&ctx, &resp);
        }
        if (self.player.is_some() || sewing.is_some())
            && let Some(n) = needle
        {
            painter.circle_stroke(n, 6.0, Stroke::new(2.0, Color32::from_rgb(255, 60, 60)));
            painter.circle_filled(n, 2.0, Color32::from_rgb(255, 60, 60));
        }
        if self.tool == Tool::Angle {
            let msg = if self.selected_fill().is_some() {
                "Drag across the fill to set the stitch direction · Shift snaps to 15° · Esc to finish"
            } else {
                "Select a Fill object, then drag to set its stitch direction"
            };
            self.hud(&painter, rect, msg);
        }
        if let Some(i) = self.hovered
            && self.angle_drag.is_none()
            && self.drag.is_none()
        {
            resp.on_hover_text_at_pointer(format!(
                "{} · {}",
                self.names.get(i).cloned().unwrap_or_default(),
                kind_name(self.doc.shapes[i].params.kind.unwrap_or(Kind::Fill))
            ));
        }
    }

    fn selected_fill(&self) -> Option<usize> {
        self.sel_index().filter(|&i| {
            self.doc.shapes[i].params.kind.unwrap_or(Kind::Fill) == Kind::Fill
                && matches!(self.doc.shapes[i].geometry, Geometry::Fill { .. })
        })
    }

    /// Double-headed line through the shape showing its fill direction.
    fn draw_angle_indicator(&self, painter: &egui::Painter, rect: Rect, i: usize) {
        let b = shape_bounds(&self.doc.shapes[i]);
        let c = self.view.to_screen(rect, b.center());
        let len = ((b.width().hypot(b.height()) / 10.0) as f32 * self.view.scale * 0.3)
            .clamp(24.0, 160.0);
        let a = self.doc.shapes[i].params.fill_angle().to_radians() as f32;
        let d = Vec2::new(a.cos(), a.sin()) * len;
        let col = Color32::from_rgb(255, 200, 60);
        let s = Stroke::new(2.0, col);
        painter.line_segment([c - d, c + d], s);
        painter.arrow(c, d, s);
        painter.arrow(c, -d, s);
        painter.circle_filled(c, 3.0, col);
    }
}

pub(crate) fn snapshot_of(d: &Document) -> Snap {
    (
        d.shapes
            .iter()
            .map(|s| (s.uid, s.params.clone(), s.color))
            .collect(),
        d.settings.clone(),
    )
}

pub fn shape_bounds(s: &Shape) -> Bounds {
    let mut b = Bounds::empty();
    match &s.geometry {
        Geometry::Fill { rings, .. } => rings.iter().flatten().for_each(|&p| b.add(p)),
        Geometry::Stroke { lines, .. } => lines.iter().flat_map(|(l, _)| l).for_each(|&p| b.add(p)),
    }
    b
}

/// A satin's rails (as found for planning), the rungs drawn between them,
/// and its mean column width (mm).
pub struct SatinInfo {
    pub rails: Vec<(Vec<P>, Vec<P>)>,
    pub rungs: Vec<(P, P)>,
    pub mean_width: f64,
}

impl Studio {
    /// Object `i`'s [`SatinInfo`], from the memo when it hasn't changed.
    pub fn satin_info(&self, i: usize) -> std::rc::Rc<SatinInfo> {
        use std::hash::{Hash, Hasher};
        let s = &self.doc.shapes[i];
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (s.uid, self.doc.rev()).hash(&mut h);
            format!("{:?}", s.params).hash(&mut h);
            // Moved without the SVG changing (a drag): the first point says so.
            let first = match &s.geometry {
                Geometry::Fill { rings, .. } => rings.first().and_then(|r| r.first()).copied(),
                Geometry::Stroke { lines, .. } => lines.first().and_then(|l| l.0.first()).copied(),
            };
            first.map(|p| (p.x.to_bits(), p.y.to_bits())).hash(&mut h);
            h.finish()
        };
        if let Some((k, v)) = &*self.satin_memo.borrow()
            && *k == key
        {
            return v.clone();
        }
        let mut rails = Vec::new();
        let mut rungs = Vec::new();
        let mut widths = Vec::new();
        for r in design::satin_rails(s)
            .iter()
            .filter_map(|r| r.as_ref().ok())
        {
            rails.push((r.a.clone(), r.b.clone()));
            rungs.extend(zigzag::stitch::satin::pairs(r, 25.0));
            widths.extend(
                zigzag::stitch::satin::pairs(r, 10.0)
                    .into_iter()
                    .map(|(a, b)| a.dist(b)),
            );
        }
        let mean_width = if widths.is_empty() {
            0.0
        } else {
            widths.iter().sum::<f64>() / widths.len() as f64 / 10.0
        };
        let info = std::rc::Rc::new(SatinInfo {
            rails,
            rungs,
            mean_width,
        });
        *self.satin_memo.borrow_mut() = Some((key, info.clone()));
        info
    }
}

/// Average satin column width (mm) for a shape, for the automatic underlay choice.
pub fn mean_satin_width(s: &Shape) -> f64 {
    let ws: Vec<f64> = design::satin_rails(s)
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .flat_map(|r| {
            zigzag::stitch::satin::pairs(r, 10.0)
                .into_iter()
                .map(|(a, b)| a.dist(b))
        })
        .collect();
    if ws.is_empty() {
        0.0
    } else {
        ws.iter().sum::<f64>() / ws.len() as f64 / 10.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn studio(select: Option<usize>) -> Harness<'static, Studio> {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/badge.svg"));
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 1500.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(Some(path), select),
            );
        h.run_steps(4);
        h
    }

    fn index_of(h: &Harness<'_, Studio>, name: &str) -> usize {
        h.state()
            .names
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("{name} not in {:?}", h.state().names))
    }

    fn kind(h: &Harness<'_, Studio>, i: usize) -> Option<Kind> {
        h.state().doc.shapes[i].params.kind
    }

    #[test]
    fn selecting_does_not_create_undo_steps() {
        for sel in 0..4 {
            let h = studio(Some(sel));
            assert!(
                h.state().undo.is_empty(),
                "selecting shape {sel} registered an edit"
            );
            assert!(!h.state().modified);
        }
    }

    #[test]
    fn type_buttons_change_stitch_type_and_undo_restores() {
        let mut h = studio(None);
        let sun = index_of(&h, "sun");
        h.state_mut().select_index(Some(sun));
        h.run_steps(2);
        assert_eq!(kind(&h, sun), Some(Kind::Fill));
        let before = h.state().design.stitch_count();

        h.get_by_label("Run").click();
        h.run_steps(3);
        assert_eq!(kind(&h, sun), Some(Kind::Run));
        assert_ne!(
            h.state().design.stitch_count(),
            before,
            "design was re-planned"
        );
        assert!(h.state().modified);

        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Z);
        h.run_steps(3);
        assert_eq!(kind(&h, sun), Some(Kind::Fill));
        assert_eq!(h.state().design.stitch_count(), before);

        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, Key::Z);
        h.run_steps(3);
        assert_eq!(kind(&h, sun), Some(Kind::Run));

        // An 11 mm disc is too wide for a satin column: it's filled, and the
        // checks say why.
        h.get_by_label("Satin").click();
        h.run_steps(3);
        assert_eq!(kind(&h, sun), Some(Kind::Satin));
        assert!(
            h.state()
                .issues
                .iter()
                .any(|x| x.shape == Some(sun) && x.message.contains("filled instead"))
        );
    }

    #[test]
    fn keyboard_shortcuts_set_type() {
        let mut h = studio(None);
        let swoosh = index_of(&h, "swoosh");
        h.state_mut().select_index(Some(swoosh));
        h.run_steps(1);
        let (undo, stitches) = (h.state().undo.len(), h.state().design.stitch_count());
        h.key_press(Key::R);
        h.run_steps(2);
        assert_eq!(kind(&h, swoosh), Some(Kind::Run));
        // An edit like any other: undoable, unsaved, and re-planned.
        assert_eq!(h.state().undo.len(), undo + 1);
        assert!(h.state().modified);
        assert_ne!(h.state().design.stitch_count(), stitches);
        // Fill isn't offered for open strokes, so F does nothing.
        h.key_press(Key::F);
        h.run_steps(2);
        assert_eq!(kind(&h, swoosh), Some(Kind::Run));
    }

    #[test]
    fn list_and_canvas_clicks_select_objects() {
        let mut h = studio(None);
        h.get_by_label("swoosh").click();
        h.run_steps(2);
        assert_eq!(h.state().sel_index(), Some(index_of(&h, "swoosh")));

        let sun = index_of(&h, "sun");
        let c = shape_bounds(&h.state().doc.shapes[sun]).center();
        let at = h.state().view.to_screen(h.state().canvas_rect, c);
        h.hover_at(at);
        h.run_steps(1);
        h.drag_at(at);
        h.run_steps(1);
        h.drop_at(at);
        h.run_steps(2);
        assert_eq!(h.state().sel_index(), Some(sun));
    }

    #[test]
    fn angle_tool_drag_sets_fill_angle() {
        let mut h = studio(None);
        let sun = index_of(&h, "sun");
        h.state_mut().select_index(Some(sun));
        h.state_mut().tool = Tool::Angle;
        h.run_steps(2);
        let c = shape_bounds(&h.state().doc.shapes[sun]).center();
        let a = h.state().view.to_screen(h.state().canvas_rect, c);
        let steps = [a, a + Vec2::new(20.0, 20.0), a + Vec2::new(60.0, 60.0)];
        h.hover_at(steps[0]);
        h.run_steps(1);
        h.drag_at(steps[0]);
        h.run_steps(1);
        for &p in &steps[1..] {
            h.hover_at(p);
            h.run_steps(1);
        }
        h.drop_at(steps[2]);
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes[sun].params.angle, Some(45.0));
        assert_eq!(h.state().undo.len(), 1, "one drag = one undo step");
    }

    #[test]
    fn clicks_select_whole_groups_and_command_click_one_object() {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/groups.svg"));
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 1500.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(Some(path), None),
            );
        h.run_steps(4);
        assert_eq!(h.state().doc.shapes.len(), 7);
        let petal = 0;
        let flower = h.state().group_members("flower");
        assert_eq!(flower.len(), 5);
        let at = h.state().view.to_screen(
            h.state().canvas_rect,
            shape_bounds(&h.state().doc.shapes[petal]).center(),
        );
        h.hover_at(at);
        h.run_steps(1);
        h.drag_at(at);
        h.run_steps(1);
        h.drop_at(at);
        h.run_steps(2);
        let mut sel = h.state().sel_indices();
        sel.sort_unstable();
        assert_eq!(
            sel, flower,
            "a click selects the outermost group, not the whole layer"
        );

        h.key_down(Key::Escape);
        h.run_steps(1);
        h.state_mut().click_select(petal, false, true);
        h.run_steps(1);
        assert_eq!(
            h.state().sel_indices(),
            vec![petal],
            "⌘-click picks one object"
        );

        // The Objects panel lists the groups.
        h.get_by_label("flower");
        h.get_by_label("Group");
    }

    fn groups_studio() -> Harness<'static, Studio> {
        let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/groups.svg"));
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 1500.0))
            // Real frame times, so two clicks make a double-click.
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(Some(path), None),
            );
        h.run_steps(4);
        h
    }

    fn screen_center(h: &Harness<'_, Studio>, i: usize) -> Pos2 {
        h.state().view.to_screen(
            h.state().canvas_rect,
            shape_bounds(&h.state().doc.shapes[i]).center(),
        )
    }

    fn drag_by(h: &mut Harness<'_, Studio>, from: Pos2, by: Vec2) {
        h.hover_at(from);
        h.run_steps(1);
        h.drag_at(from);
        h.run_steps(1);
        for k in 1..=4 {
            h.hover_at(from + by * (k as f32 / 4.0));
            h.run_steps(1);
        }
        h.drop_at(from + by);
        h.run_steps(3);
    }

    fn double_click(h: &mut Harness<'_, Studio>, at: Pos2) {
        h.hover_at(at);
        // Long enough after any earlier click not to make a triple-click.
        h.run_steps(30);
        for _ in 0..2 {
            h.drag_at(at);
            h.run_steps(1);
            h.drop_at(at);
            h.run_steps(1);
        }
        h.run_steps(2);
    }

    fn sorted_sel(h: &Harness<'_, Studio>) -> Vec<usize> {
        let mut v = h.state().sel_indices();
        v.sort_unstable();
        v
    }

    #[test]
    fn dragging_moves_the_whole_group_until_you_go_into_it() {
        let mut h = groups_studio();
        // Clicks here land on art the task bar would cover.
        h.state_mut().dock.task_bar = false;
        let petal = 0;
        let flower = h.state().group_members("flower");
        let petals = h.state().group_members("petals");
        let center_of =
            |h: &Harness<'_, Studio>, i: usize| shape_bounds(&h.state().doc.shapes[i]).center();
        let before: Vec<P> = (0..7).map(|i| center_of(&h, i)).collect();

        // A plain drag on a petal moves the whole flower.
        let at = screen_center(&h, petal);
        drag_by(&mut h, at, Vec2::new(60.0, 0.0));
        assert_eq!(sorted_sel(&h), flower);
        for (i, b) in before.iter().enumerate() {
            let moved = (center_of(&h, i) - *b).len() > 1.0;
            assert_eq!(moved, flower.contains(&i), "object {i}");
        }
        assert_eq!(h.state().undo.len(), 1);

        // Double-click goes into the flower: a click now picks the petals group.
        let at = screen_center(&h, petal);
        double_click(&mut h, at);
        assert_eq!(h.state().layout.isolated, vec!["flower".to_string()]);
        assert_eq!(sorted_sel(&h), petals);
        h.get_by_label("Exit Group");

        // Again: into the petals, and a drag moves one petal.
        let at = screen_center(&h, petal);
        double_click(&mut h, at);
        assert_eq!(
            h.state().layout.isolated,
            vec!["flower".to_string(), "petals".to_string()]
        );
        assert_eq!(sorted_sel(&h), vec![petal]);
        let other = center_of(&h, 1);
        let mine = center_of(&h, petal);
        let at = screen_center(&h, petal);
        drag_by(&mut h, at, Vec2::new(0.0, 50.0));
        assert!((center_of(&h, petal) - mine).len() > 1.0, "the petal moved");
        assert!(
            (center_of(&h, 1) - other).len() < 1e-6,
            "its neighbour didn't"
        );

        // Esc backs out one level at a time, selecting the group it left.
        h.key_press(Key::Escape);
        h.run_steps(2);
        assert_eq!(h.state().layout.isolated, vec!["flower".to_string()]);
        assert_eq!(sorted_sel(&h), petals);
        h.key_press(Key::Escape);
        h.run_steps(2);
        assert!(h.state().layout.isolated.is_empty());
        assert_eq!(sorted_sel(&h), flower);

        // Select All inside a group means the group.
        let at = screen_center(&h, petal);
        double_click(&mut h, at);
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
        h.run_steps(2);
        assert_eq!(sorted_sel(&h), flower);

        // Clicking art outside the group leaves it.
        let line = 5;
        assert!(!flower.contains(&line));
        let at = screen_center(&h, line);
        drag_by(&mut h, at, Vec2::ZERO);
        assert!(h.state().layout.isolated.is_empty());
        let lines = h
            .state()
            .group_members(&h.state().doc.shapes[line].groups[0]);
        assert_eq!(sorted_sel(&h), lines);
    }

    #[test]
    fn oversized_artwork_is_scaled_to_fit_the_hoop() {
        let dir = std::env::temp_dir().join(format!("tl-fit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.svg");
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="300mm" height="150mm" viewBox="0 0 300 150">
              <rect width="300" height="150" fill="#2f4858"/>
              <circle cx="150" cy="75" r="40" fill="#e76f51"/>
            </svg>"##,
        )
        .unwrap();
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 1500.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(Some(path.clone()), None),
            );
        h.run_steps(4);
        let all: Vec<usize> = (0..h.state().doc.shapes.len()).collect();
        let b = h.state().art_bounds(&all);
        assert!((b.width() - 960.0).abs() < 2.0, "96 mm wide: {}", b.width());
        assert!(b.center().len() < 1.0, "centered");
        assert!(
            h.state().status.contains("scaled to 32%"),
            "{}",
            h.state().status
        );
        assert!(h.state().sel_indices().is_empty(), "nothing left selected");
        assert!(!h.state().design_over_hoop());
        assert_eq!(h.state().undo.len(), 1);

        // ⌘Z: full size again.
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Z);
        h.run_steps(3);
        assert!((h.state().art_bounds(&all).width() - 3000.0).abs() < 2.0);

        // Fit to Hoop works on demand, and grows small art too.
        h.state_mut().set_selection(&[1]);
        assert!(h.state_mut().fit_to_hoop(false).is_some());
        h.run_steps(2);
        let c = h.state().art_bounds(&[1]);
        assert!((c.width() - 960.0).abs() < 2.0, "{}", c.width());

        // Artwork that already fits opens as it is.
        let mut h = groups_studio();
        assert!(h.state().undo.is_empty());
        h.run_steps(1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_and_undo() {
        let mut h = studio(None);
        let n = h.state().doc.shapes.len();
        let sun = index_of(&h, "sun");
        h.state_mut().select_index(Some(sun));
        h.run_steps(1);
        h.key_press(Key::Delete);
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n - 1);
        assert!(!h.state().names.contains(&"sun".to_string()));
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Z);
        h.run_steps(3);
        assert_eq!(h.state().doc.shapes.len(), n);
    }

    #[test]
    fn document_toggles_replan_and_are_undoable() {
        let mut h = studio(None);
        let before = h.state().design.stitch_count();
        h.get_by_label("Remove overlaps").click();
        h.run_steps(3);
        assert!(!h.state().doc.settings.remove_overlaps);
        assert_ne!(
            h.state().design.stitch_count(),
            before,
            "re-planned with the hidden stitches back"
        );
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Z);
        h.run_steps(3);
        assert!(h.state().doc.settings.remove_overlaps);
    }

    #[test]
    fn angles_normalize_to_half_turn() {
        assert_eq!(normalize_angle(135.0), -45.0);
        assert_eq!(normalize_angle(-135.0), 45.0);
        assert_eq!(normalize_angle(90.0), 90.0);
        assert_eq!(normalize_angle(-90.0), 90.0);
    }
}
