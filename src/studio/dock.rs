//! Panels, Illustrator-style. Each panel lives in the left dock, the right
//! dock, or floats over the canvas; drag a panel by its header to move it
//! (between panels, to the other dock, or out onto the canvas). A dock can
//! collapse to a strip of icons (» at its top): click an icon and the panel
//! flies out beside the strip. Panels can also be collapsed, hidden, and
//! shown again from the Window menu. The layout is remembered.

use crate::app::{HOOPS, Studio, Tool};
use crate::icons::{self, Icon};
use crate::theme::{self, Form, callout, caption, form, icon_button, pal_ui, value_row};
use crate::widgets::*;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use zigzag::checks::Level;
use zigzag::design;
use zigzag::params::{FABRICS, Kind, Params, defaults, underlay};
use zigzag::svg::Geometry;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelId {
    Objects,
    Stitch,
    Transform,
    Lettering,
    Threads,
    Document,
    /// The in-app agent (Claude Code or Codex): its conversation and prompt.
    Agent,
}

/// How many panels there are.
pub const N: usize = 7;

enum ObjRow {
    Group {
        id: String,
        depth: usize,
        members: Vec<usize>,
    },
    Item {
        i: usize,
        depth: usize,
    },
}

pub const ALL: [PanelId; N] = [
    PanelId::Objects,
    PanelId::Stitch,
    PanelId::Transform,
    PanelId::Lettering,
    PanelId::Threads,
    PanelId::Document,
    PanelId::Agent,
];

/// Where a panel lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Left,
    Right,
    Float,
}

impl Place {
    fn key(self) -> &'static str {
        match self {
            Place::Left => "left",
            Place::Right => "right",
            Place::Float => "float",
        }
    }
    fn side(self) -> usize {
        match self {
            Place::Left => 0,
            _ => 1,
        }
    }
}

/// Width of a dock collapsed to icons, and of floating and flyout panels.
pub const ICON_STRIP_W: f32 = 40.0;
pub const FLOAT_W: f32 = 300.0;

/// A floating (or flyout) panel's width: the Agent's conversation is wider.
pub fn float_w(p: PanelId) -> f32 {
    if p == PanelId::Agent { 360.0 } else { FLOAT_W }
}

/// Panel interaction that isn't saved: the open flyout, a panel being
/// dragged, and where things were drawn this frame (for dropping).
#[derive(Default)]
pub struct DockUi {
    pub flyout: Option<PanelId>,
    drag: Option<PanelDrag>,
    /// Panels drawn this frame: (where, which, whole panel rect).
    drawn: Vec<(Place, PanelId, Rect)>,
    /// The dock columns this frame (left, right).
    columns: [Option<Rect>; 2],
    /// Where the panel being drawn lives.
    pub(crate) current: Option<Place>,
    /// Height of each dock column (for sizing the Objects list).
    pub(crate) column_h: [f32; 2],
    /// The panel being drawn sits in a rounded window (floating, flyout).
    pub(crate) in_window: bool,
    /// The tab group being drawn: its lead and the tabs showing.
    tabs: Option<(PanelId, Vec<PanelId>)>,
    /// The Agent panel's whole height when it's pinned to the bottom of
    /// the dock being drawn.
    pub(crate) pinned_h: Option<f32>,
    /// The next panel header drawn is the first in this dock's column: it
    /// carries the dock's « / » (collapse to icons), at its right end.
    collapse_btn: Option<Place>,
}

#[derive(Clone, Copy)]
struct PanelDrag {
    id: PanelId,
    /// Pointer offset from the panel header's top-left.
    grab: Vec2,
    /// The whole group moves (dragged by its header), not one tab.
    whole: bool,
}

/// Where a dragged panel would land.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DropAt {
    /// In a dock, before a panel (or at the end).
    Dock(Place, Option<PanelId>),
    /// Into a panel's tab group.
    Tab(PanelId),
    Float,
}

impl PanelId {
    pub fn icon(self) -> Icon {
        match self {
            PanelId::Objects => Icon::Layers,
            PanelId::Stitch => Icon::Stitch,
            PanelId::Transform => Icon::Transform,
            PanelId::Lettering => Icon::Text,
            PanelId::Threads => Icon::Swatches,
            PanelId::Document => Icon::Page,
            PanelId::Agent => Icon::Sparkle,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            PanelId::Objects => "Objects",
            PanelId::Stitch => "Stitch",
            PanelId::Transform => "Transform",
            PanelId::Lettering => "Lettering",
            PanelId::Threads => "Threads",
            PanelId::Document => "Document",
            PanelId::Agent => "Agent",
        }
    }
    pub(crate) fn index(self) -> usize {
        ALL.iter().position(|&p| p == self).unwrap()
    }
}

#[derive(Clone, PartialEq)]
pub struct DockState {
    pub left: bool,
    pub right: bool,
    hidden: [bool; N],
    collapsed: [bool; N],
    /// The Threads panel shows a palette to click (its menu's Show Palette).
    /// Off by default: the thread picker has the same palettes.
    pub show_palette: bool,
    /// Where each panel lives.
    pub place: [Place; N],
    /// Panel order within the docks.
    pub order: Vec<PanelId>,
    /// Top-left corners of floating panels.
    pub float_at: [Pos2; N],
    /// Each dock (left, right) collapsed to a strip of icons.
    pub iconic: [bool; 2],
    /// Tab groups: the panel each one is a tab of (the group's lead), and
    /// each lead's tab in front.
    pub tab_of: [Option<PanelId>; N],
    pub active: [Option<PanelId>; N],
    /// The contextual task bar under the selection.
    pub task_bar: bool,
    /// How much of its dock the Agent panel takes (it sits at the bottom).
    pub agent_share: f32,
    /// The unit lengths are shown in (View › Units).
    pub units: crate::units::Unit,
    /// Where the layout is remembered between launches (`None`: not saved).
    path: Option<std::path::PathBuf>,
}

fn default_place(p: PanelId) -> Place {
    match p {
        // The conversation gets a whole dock; everything else is on the right.
        PanelId::Agent => Place::Left,
        _ => Place::Right,
    }
}

impl Default for DockState {
    fn default() -> Self {
        // Tests start without the Agent panel, so the others have the
        // whole dock (its own tests open it).
        let mut hidden = [false; N];
        hidden[PanelId::Agent.index()] = cfg!(test);
        DockState {
            left: true,
            right: true,
            hidden,
            collapsed: [false; N],
            show_palette: false,
            units: Default::default(),
            place: ALL.map(default_place),
            order: ALL.to_vec(),
            float_at: std::array::from_fn(|k| {
                Pos2::new(360.0 + 24.0 * k as f32, 120.0 + 24.0 * k as f32)
            }),
            iconic: [false, false],
            tab_of: [None; N],
            active: [None; N],
            task_bar: true,
            agent_share: 0.55,
            path: None,
        }
    }
}

fn layout_file() -> Option<std::path::PathBuf> {
    config_file("panels.json")
}

/// A settings file in ZigZag's folder (Application Support on a Mac).
pub(crate) fn config_file(name: &str) -> Option<std::path::PathBuf> {
    Some(config_dir()?.join(name))
}

/// ZigZag's settings folder. The app used to be called Threadline: the first
/// time the new folder is missing and the old one is there, the old one is
/// moved over, so sessions, panel layouts and palettes come along.
fn config_dir() -> Option<std::path::PathBuf> {
    use std::sync::OnceLock;
    static DIR: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
        let (new, old) = if cfg!(target_os = "macos") {
            let base = home.join("Library/Application Support");
            (base.join("ZigZag"), base.join("threadline"))
        } else {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or(home.join(".config"));
            (base.join("zigzag"), base.join("threadline"))
        };
        if !new.exists() && old.is_dir() {
            // Best effort: if the move fails the app simply starts fresh.
            let _ = std::fs::rename(&old, &new);
        }
        Some(new)
    })
    .clone()
}

impl DockState {
    /// The panel layout from last time (or the default), saved from now on.
    pub fn load() -> DockState {
        Self::load_from(layout_file())
    }

    fn load_from(path: Option<std::path::PathBuf>) -> DockState {
        let mut d = DockState {
            path: path.clone(),
            ..DockState::default()
        };
        let Some(v) = path
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        else {
            return d;
        };
        d.left = v["left"].as_bool().unwrap_or(true);
        d.right = v["right"].as_bool().unwrap_or(true);
        d.show_palette = v["show_palette"].as_bool().unwrap_or(false);
        d.units = v["units"]
            .as_str()
            .and_then(crate::units::Unit::named)
            .unwrap_or_default();
        d.task_bar = v["task_bar"].as_bool().unwrap_or(true);
        d.agent_share = v["agent_share"].as_f64().unwrap_or(0.55).clamp(0.15, 0.95) as f32;
        for (k, id) in ALL.iter().enumerate() {
            let e = &v["panels"][id.title()];
            d.hidden[k] = e["hidden"].as_bool().unwrap_or(false);
            d.collapsed[k] = e["collapsed"].as_bool().unwrap_or(false);
            d.place[k] = match e["place"].as_str() {
                Some("left") => Place::Left,
                Some("right") => Place::Right,
                Some("float") => Place::Float,
                _ => default_place(*id),
            };
            if let (Some(x), Some(y)) = (e["x"].as_f64(), e["y"].as_f64()) {
                d.float_at[k] = Pos2::new(x as f32, y as f32);
            }
        }
        let mut order: Vec<PanelId> = v["order"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| ALL.iter().copied().find(|p| Some(p.title()) == t.as_str()))
            .collect();
        for p in ALL {
            if !order.contains(&p) {
                order.push(p);
            }
        }
        d.order = order;
        // A layout saved before the Agent panel: the Objects list moves to
        // the top of the right dock, and the Agent has the left to itself.
        if v["panels"].get("Agent").is_none() {
            d.place[PanelId::Objects.index()] = Place::Right;
            d.place[PanelId::Agent.index()] = Place::Left;
            d.order.retain(|&p| p != PanelId::Objects);
            d.order.insert(0, PanelId::Objects);
            d.left = true;
        }
        d.iconic = [
            v["iconic"]["left"].as_bool().unwrap_or(false),
            v["iconic"]["right"].as_bool().unwrap_or(false),
        ];
        let by_title = |t: Option<&str>| ALL.iter().copied().find(|p| Some(p.title()) == t);
        for (k, id) in ALL.iter().enumerate() {
            let e = &v["panels"][id.title()];
            d.tab_of[k] = by_title(e["tab_of"].as_str()).filter(|l| l != id);
            d.active[k] = by_title(e["active"].as_str());
        }
        // A lead can't itself be a tab of another: flatten.
        for k in 0..ALL.len() {
            if let Some(l) = d.tab_of[k]
                && d.tab_of[l.index()].is_some()
            {
                d.tab_of[k] = None;
            }
        }
        for k in 0..ALL.len() {
            if let Some(l) = d.tab_of[k] {
                d.place[k] = d.place[l.index()];
            }
        }
        d
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        let panels: serde_json::Map<String, serde_json::Value> = ALL
            .iter()
            .enumerate()
            .map(|(k, id)| {
                (
                    id.title().to_string(),
                    serde_json::json!({
                        "hidden": self.hidden[k],
                        "collapsed": self.collapsed[k],
                        "place": self.place[k].key(),
                        "x": self.float_at[k].x,
                        "y": self.float_at[k].y,
                        "tab_of": self.tab_of[k].map(|l| l.title()),
                        "active": self.active[k].map(|a| a.title()),
                    }),
                )
            })
            .collect();
        let v = serde_json::json!({
            "left": self.left,
            "right": self.right,
            "show_palette": self.show_palette,
            "units": self.units.suffix(),
            "task_bar": self.task_bar,
            "agent_share": self.agent_share,
            "panels": panels,
            "order": self.order.iter().map(|p| p.title()).collect::<Vec<_>>(),
            "iconic": { "left": self.iconic[0], "right": self.iconic[1] },
        });
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default());
    }
}

impl DockState {
    pub fn visible(&self, p: PanelId) -> bool {
        !self.hidden[p.index()]
    }
    pub fn toggle(&mut self, p: PanelId) {
        let i = p.index();
        self.hidden[i] = !self.hidden[i];
        if !self.hidden[i] {
            self.collapsed[i] = false;
            match self.place[i] {
                Place::Left => self.left = true,
                Place::Right => self.right = true,
                Place::Float => {}
            }
        }
    }

    pub fn place_of(&self, p: PanelId) -> Place {
        self.place[p.index()]
    }

    /// Move panel `p` to `to`, before panel `before` (or at the end). A
    /// group's lead takes its tabs along; a tab leaves its group.
    pub fn move_panel(&mut self, p: PanelId, to: Place, before: Option<PanelId>) {
        if self.tab_of[p.index()].is_some() {
            self.separate(p);
        }
        let group = self.members(p);
        let before = before.filter(|b| !group.contains(b));
        self.order.retain(|x| !group.contains(x));
        let at = before
            .and_then(|b| self.order.iter().position(|&x| x == b))
            .unwrap_or(self.order.len());
        for (k, &q) in group.iter().enumerate() {
            self.order.insert(at + k, q);
            self.place[q.index()] = to;
        }
        // Moving a hidden panel shows it; a group moved by a showing tab
        // keeps its hidden lead hidden.
        if group.iter().all(|q| self.hidden[q.index()]) {
            self.hidden[p.index()] = false;
        }
        match to {
            Place::Left => self.left = true,
            Place::Right => self.right = true,
            Place::Float => {}
        }
    }

    /// The group a panel leads or belongs to.
    pub fn lead_of(&self, p: PanelId) -> PanelId {
        self.tab_of[p.index()].unwrap_or(p)
    }

    /// A group's panels: its lead, then its tabs in panel order.
    pub fn members(&self, lead: PanelId) -> Vec<PanelId> {
        let mut v = vec![lead];
        v.extend(
            self.order
                .iter()
                .copied()
                .filter(|&q| self.tab_of[q.index()] == Some(lead)),
        );
        v
    }

    /// Put panel `p` (and its own tabs, if it leads a group) into the group
    /// of `onto`, in front.
    pub fn join_tab(&mut self, p: PanelId, onto: PanelId) {
        let lead = self.lead_of(onto);
        if self.lead_of(p) == lead {
            return;
        }
        let moving = if self.tab_of[p.index()].is_none() {
            self.members(p)
        } else {
            vec![p]
        };
        for q in moving {
            self.tab_of[q.index()] = Some(lead);
            self.place[q.index()] = self.place[lead.index()];
            self.hidden[q.index()] = false;
            // Tabs follow their lead in the order.
            self.order.retain(|&x| x != q);
            let after = self
                .order
                .iter()
                .rposition(|&x| self.lead_of(x) == lead)
                .map_or(self.order.len(), |i| i + 1);
            self.order.insert(after, q);
        }
        self.active[lead.index()] = Some(p);
    }

    /// Take panel `p` out of its group (a lead hands the group to its next tab).
    pub fn separate(&mut self, p: PanelId) {
        if self.tab_of[p.index()].take().is_some() {
            return;
        }
        let followers: Vec<PanelId> = self.members(p).into_iter().skip(1).collect();
        if let Some((&new, rest)) = followers.split_first() {
            self.tab_of[new.index()] = None;
            for &q in rest {
                self.tab_of[q.index()] = Some(new);
            }
            self.float_at[new.index()] = self.float_at[p.index()];
            // The one leaving steps aside rather than covering the group.
            self.float_at[p.index()] = self.float_at[p.index()] + Vec2::new(28.0, 28.0);
            self.collapsed[new.index()] = self.collapsed[p.index()];
            let front = self.active[p.index()].take().filter(|a| *a != p);
            self.active[new.index()] = front;
        }
    }

    /// Back to the standard layout (Window › Reset Panels).
    pub fn reset_layout(&mut self) {
        let path = self.path.take();
        let (show_palette, units) = (self.show_palette, self.units);
        *self = DockState {
            path,
            show_palette,
            units,
            ..DockState::default()
        };
    }
    pub fn collapsed(&self, p: PanelId) -> bool {
        self.collapsed[p.index()]
    }

    /// Show panel `p` open: unhidden, its group expanded with it in front,
    /// its dock on.
    pub fn reveal(&mut self, p: PanelId) {
        let lead = self.lead_of(p);
        self.hidden[p.index()] = false;
        self.collapsed[lead.index()] = false;
        if lead != p {
            self.active[lead.index()] = Some(p);
        }
        match self.place[lead.index()] {
            Place::Left => self.left = true,
            Place::Right => self.right = true,
            Place::Float => {}
        }
    }
}

impl Studio {
    /// The name a panel's header shows: the Agent's is the agent's own
    /// ("Claude Code", "Codex").
    pub fn panel_name(&self, p: PanelId) -> &'static str {
        if p == PanelId::Agent {
            self.agent.kind.name()
        } else {
            p.title()
        }
    }
}

/// Where a panel's open/closed animation lives (the same in any dock).
fn body_id(lead: PanelId) -> egui::Id {
    egui::Id::unique(("panel-body", lead.index()))
}

/// The height of a panel header.
pub const HEADER_H: f32 = 26.0;

impl Studio {
    /// A panel: a header bar that collapses it, then its body.
    fn panel(
        &mut self,
        ui: &mut egui::Ui,
        id: PanelId,
        body: impl FnOnce(&mut Studio, &mut egui::Ui),
    ) {
        self.panel_with(ui, id, None, body);
    }

    /// A panel whose header also shows `detail`: (what it's showing, a
    /// figure on the right), like the Stitch panel's object and stitch count.
    fn panel_with(
        &mut self,
        ui: &mut egui::Ui,
        id: PanelId,
        detail: Option<(String, String)>,
        body: impl FnOnce(&mut Studio, &mut egui::Ui),
    ) {
        if !self.dock.visible(id) {
            return;
        }
        let tabs = self.dock_ui.tabs.clone();
        // A group collapses and moves as one, by its lead.
        let key = tabs.as_ref().map_or(id, |t| t.0);
        let p = pal_ui(ui);
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), 26.0),
            Sense::click_and_drag(),
        );
        // Drag the header to move the panel (or the whole group).
        if resp.drag_started_by(egui::PointerButton::Primary)
            && let Some(at) = ui.input(|i| i.pointer.press_origin())
        {
            self.begin_panel_drag(key, rect, at, true);
        }
        if resp.hovered() && !resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{} panel", id.title()))
        });
        if let Some((what, figure)) = &detail {
            resp.clone().on_hover_text(format!("{what} · {figure}"));
        }
        // The first panel in a dock holds the dock's « / » at its right end,
        // so the dock needs no bar of its own.
        let collapse_side = if self.dock_ui.in_window {
            None
        } else {
            self.dock_ui.collapse_btn.take()
        };
        let inset = if collapse_side.is_some() { 24.0 } else { 0.0 };
        let painter = ui.painter();
        // In a floating window the header follows its rounded top corners.
        let corners = if self.dock_ui.in_window {
            egui::CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            }
        } else {
            egui::CornerRadius::ZERO
        };
        painter.rect_filled(rect, corners, p.header);
        painter.hline(
            rect.x_range(),
            rect.top() + 0.5,
            Stroke::new(1.0, p.separator),
        );
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, p.separator),
        );
        // Alone in a dock there's nothing to make room for: it can't be
        // collapsed (no chevron, no Collapse in its menu).
        let alone = tabs.is_none()
            && !self.dock_ui.in_window
            && match self.dock_ui.current {
                Some(side @ (Place::Left | Place::Right)) => self.panels_in(side).len() == 1,
                _ => false,
            };
        if alone && self.dock.collapsed(key) {
            self.dock.collapsed[key.index()] = false;
        }
        let collapsed = self.dock.collapsed(key);
        if !alone {
            let chev = Rect::from_center_size(
                Pos2::new(rect.left() + 14.0, rect.center().y),
                Vec2::splat(9.0),
            );
            icons::paint(
                painter,
                chev,
                if collapsed {
                    Icon::ChevronRight
                } else {
                    Icon::ChevronDown
                },
                p.secondary,
            );
        }
        let title_x = rect.left() + if alone { 12.0 } else { 26.0 };
        let title = match &tabs {
            // "Agent: Claude Code ⌄": the name picks the agent.
            None if id == PanelId::Agent => {
                let font = egui::FontId::proportional(12.0);
                let a = painter.text(
                    Pos2::new(title_x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Agent:",
                    font.clone(),
                    p.secondary,
                );
                let n = painter.text(
                    Pos2::new(a.right() + 4.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    self.agent.kind.name(),
                    font,
                    p.text,
                );
                let chev = Rect::from_center_size(
                    Pos2::new(n.right() + 9.0, rect.center().y + 1.0),
                    Vec2::splat(8.0),
                );
                icons::paint(painter, chev, Icon::ChevronDown, p.secondary);
                a.union(chev)
            }
            None => painter.text(
                Pos2::new(title_x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                self.panel_name(id),
                egui::FontId::proportional(12.0),
                p.text,
            ),
            Some((lead, members)) => {
                // One tab per panel: click to bring it to the front, drag
                // it out of the group.
                let mut x = rect.left() + 24.0;
                let mut last = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::ZERO);
                for &t in members {
                    let galley = ui.painter().layout_no_wrap(
                        self.panel_name(t).to_string(),
                        egui::FontId::proportional(12.0),
                        p.text,
                    );
                    let tab = Rect::from_min_max(
                        Pos2::new(x, rect.top() + 3.0),
                        Pos2::new(x + galley.size().x + 16.0, rect.bottom()),
                    );
                    let front = t == id;
                    let tr = ui.interact(
                        tab,
                        ui.make_persistent_id(("panel-tab", lead.index(), t.index())),
                        Sense::click_and_drag(),
                    );
                    tr.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::Role::Tab,
                            true,
                            front,
                            format!("{} tab", t.title()),
                        )
                    });
                    if front {
                        let r = egui::CornerRadius {
                            nw: 5,
                            ne: 5,
                            sw: 0,
                            se: 0,
                        };
                        ui.painter().rect_filled(tab, r, p.dock);
                    } else if tr.hovered() {
                        let r = egui::CornerRadius {
                            nw: 5,
                            ne: 5,
                            sw: 0,
                            se: 0,
                        };
                        ui.painter().rect_filled(tab, r, p.hover);
                    }
                    ui.painter().galley(
                        Pos2::new(tab.left() + 8.0, tab.center().y - galley.size().y / 2.0),
                        galley,
                        if front { p.text } else { p.secondary },
                    );
                    if tr.clicked() {
                        self.dock.active[lead.index()] = Some(t);
                        self.dock.collapsed[lead.index()] = false;
                    }
                    if tr.drag_started_by(egui::PointerButton::Primary)
                        && let Some(at) = ui.input(|i| i.pointer.press_origin())
                    {
                        self.begin_panel_drag(t, tab, at, false);
                    }
                    last = tab;
                    x = tab.right() + 2.0;
                }
                last
            }
        };
        let painter = ui.painter();
        if let Some((what, figure)) = &detail {
            let right = painter.text(
                Pos2::new(rect.right() - 34.0 - inset, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                figure,
                egui::FontId::proportional(11.0),
                p.secondary,
            );
            // The name, cut to fit between the title and the figure.
            let room = (right.left() - title.right() - 22.0).max(0.0);
            let galley = painter.layout(
                what.clone(),
                egui::FontId::proportional(12.0),
                p.secondary,
                f32::INFINITY,
            );
            let mut job = egui::text::LayoutJob::simple_singleline(
                what.clone(),
                egui::FontId::proportional(12.0),
                p.secondary,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            let g = if galley.size().x > room {
                painter.layout_job(job)
            } else {
                galley
            };
            painter.galley(
                Pos2::new(title.right() + 8.0, rect.center().y - g.size().y / 2.0),
                g,
                p.secondary,
            );
        }
        // The agent's name opens a small menu of agents.
        let mut picked_agent = false;
        if id == PanelId::Agent && tabs.is_none() {
            let pick = Rect::from_min_max(
                Pos2::new(title.left() - 5.0, rect.top() + 3.0),
                Pos2::new(title.right() + 5.0, rect.bottom() - 3.0),
            );
            let presp = ui.interact(pick, ui.make_persistent_id("agent-pick"), Sense::click());
            presp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "Choose Agent")
            });
            let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&presp));
            if presp.hovered() || open {
                ui.painter().rect_filled(pick, 4.0, p.hover);
            }
            picked_agent = presp.clicked();
            egui::Popup::menu(&presp)
                .align(egui::RectAlign::BOTTOM_START)
                .show(|ui| {
                    ui.set_min_width(150.0);
                    use crate::agent::AgentKind;
                    for k in AgentKind::ALL {
                        if ui
                            .add(egui::Button::new(k.name()).selected(self.agent.kind == k))
                            .clicked()
                        {
                            self.agent.set_kind(k);
                            ui.close();
                        }
                    }
                });
        }
        // The panel menu, at the right of the header (Illustrator's flyout).
        let mrect = Rect::from_center_size(
            Pos2::new(rect.right() - 16.0 - inset, rect.center().y),
            Vec2::new(22.0, 20.0),
        );
        let mut collapse_clicked = false;
        if let Some(side) = collapse_side {
            let crect = Rect::from_center_size(
                Pos2::new(rect.right() - 16.0, rect.center().y),
                Vec2::new(22.0, 20.0),
            );
            let cresp = ui.interact(
                crect,
                ui.make_persistent_id(("dock-collapse", side.side())),
                Sense::click(),
            );
            cresp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "Collapse to Icons")
            });
            if cresp.hovered() {
                ui.painter().rect_filled(crect, 4.0, p.hover);
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(crect.center(), Vec2::splat(10.0)),
                if side == Place::Left {
                    Icon::ChevronsLeft
                } else {
                    Icon::ChevronsRight
                },
                if cresp.hovered() { p.text } else { p.secondary },
            );
            if cresp.on_hover_text("Collapse to icons").clicked() {
                self.dock.iconic[side.side()] = true;
                collapse_clicked = true;
            }
        }
        let mresp = ui.interact(
            mrect,
            ui.make_persistent_id(("panel-menu", id.index())),
            Sense::click(),
        );
        mresp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{} menu", id.title()))
        });
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&mresp));
        if mresp.hovered() || open {
            ui.painter().rect_filled(mrect, 4.0, p.hover);
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(mrect.center(), Vec2::splat(11.0)),
            Icon::Menu,
            if mresp.hovered() || open {
                p.text
            } else {
                p.secondary
            },
        );
        // The Agent's header: + starts a new chat.
        if id == PanelId::Agent {
            let nrect = Rect::from_center_size(
                Pos2::new(rect.right() - 40.0 - inset, rect.center().y),
                Vec2::new(22.0, 20.0),
            );
            let some = !self.agent.log.is_empty() || self.agent.running();
            let nresp = ui.interact(
                nrect,
                ui.make_persistent_id("agent-new-chat"),
                if some { Sense::click() } else { Sense::hover() },
            );
            nresp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, some, "New Chat"));
            if some && nresp.hovered() {
                ui.painter().rect_filled(nrect, 4.0, p.hover);
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(nrect.center(), Vec2::splat(11.0)),
                Icon::Plus,
                if !some {
                    p.tertiary
                } else if nresp.hovered() {
                    p.text
                } else {
                    p.secondary
                },
            );
            if nresp
                .on_hover_text("New chat: the agent forgets this conversation")
                .clicked()
            {
                self.agent.new_chat();
            }
        }
        egui::Popup::menu(&mresp)
            .align(egui::RectAlign::BOTTOM_END)
            .show(|ui| {
                ui.set_min_width(170.0);
                if id == PanelId::Agent {
                    // (Which agent: the name in the header.)
                    if ui.button("New Chat").clicked() {
                        self.agent.new_chat();
                        ui.close();
                    }
                    ui.separator();
                }
                if id == PanelId::Threads {
                    if ui
                        .add(egui::Button::new("Show Palette").selected(self.dock.show_palette))
                        .clicked()
                    {
                        self.dock.show_palette = !self.dock.show_palette;
                        ui.close();
                    }
                    let name = self.palettes.name(self.palettes.source);
                    if ui
                        .button(format!("Match All to {name}"))
                        .on_hover_text(
                            "Change every color in the design to the nearest one in this palette",
                        )
                        .clicked()
                    {
                        self.match_all_to_palette();
                        ui.close();
                    }
                    ui.separator();
                }
                if !alone {
                    if ui
                        .button(if collapsed { "Expand" } else { "Collapse" })
                        .clicked()
                    {
                        self.dock.collapsed[key.index()] = !collapsed;
                        ui.close();
                    }
                    ui.separator();
                }
                let here = self.dock.place_of(key);
                for (to, label) in [
                    (Place::Left, "Dock Left"),
                    (Place::Right, "Dock Right"),
                    (Place::Float, "Float"),
                ] {
                    if to != here && ui.button(label).clicked() {
                        self.dock.move_panel(key, to, None);
                        self.dock_ui.flyout = None;
                        ui.close();
                    }
                }
                if tabs.is_some() && ui.button(format!("Separate {}", id.title())).clicked() {
                    // Out of the group, just below it.
                    let place = self.dock.place_of(key);
                    let group = self.dock.members(key);
                    let last = self
                        .dock
                        .order
                        .iter()
                        .rposition(|x| group.contains(x))
                        .unwrap_or(0);
                    let next = self.dock.order[last + 1..].iter().copied().find(|&x| {
                        self.dock.tab_of[x.index()].is_none() && self.dock.place_of(x) == place
                    });
                    self.dock.separate(id);
                    self.dock.move_panel(id, place, next);
                    ui.close();
                }
                ui.separator();
                if ui.button("Hide Panel").clicked() {
                    self.dock.toggle(id);
                    self.dock_ui.flyout = None;
                    ui.close();
                }
            });
        if resp.clicked() && !mresp.clicked() && !collapse_clicked && !picked_agent && !alone {
            self.dock.collapsed[key.index()] = !collapsed;
        }
        if id == PanelId::Agent && collapsed {
            // Working with the transcript put away: the step and the time
            // after the title (the transcript's own spinner shows otherwise).
            self.paint_agent_header(ui, title, rect);
        }
        // The body opens and closes smoothly.
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            body_id(key),
            !collapsed,
        );
        state.set_open(!collapsed);
        state.show_body_unindented(ui, |ui| {
            egui::Frame::new()
                .inner_margin(theme::pad())
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    body(self, ui);
                });
        });
    }

    // ------------------------------------------------------------ left dock

    /// The panels (group leads) in a place, in order, that have something
    /// to show.
    pub fn panels_in(&self, place: Place) -> Vec<PanelId> {
        let mut v: Vec<PanelId> = self
            .dock
            .order
            .iter()
            .copied()
            .filter(|&p| {
                self.dock.tab_of[p.index()].is_none()
                    && self.dock.place_of(p) == place
                    && !self.group_shown(p).is_empty()
            })
            .collect();
        // Lettering comes to the top of its dock while text is selected,
        // as Illustrator brings the Character panel forward: its settings
        // aren't buried under the others.
        if let Some(k) = v.iter().position(|&p| p == PanelId::Lettering)
            && k > 0
        {
            let l = v.remove(k);
            v.insert(0, l);
        }
        v
    }

    /// The panels of a group that are showing (visible, with something to show).
    pub fn group_shown(&self, lead: PanelId) -> Vec<PanelId> {
        self.dock
            .members(lead)
            .into_iter()
            .filter(|&q| self.dock.visible(q) && self.applies(q))
            .collect()
    }

    /// Every panel showing in a place, tabs included (for icon strips).
    pub fn panels_flat_in(&self, place: Place) -> Vec<PanelId> {
        self.panels_in(place)
            .into_iter()
            .flat_map(|l| self.group_shown(l))
            .collect()
    }

    /// Lettering only shows with lettering selected.
    fn applies(&self, p: PanelId) -> bool {
        match p {
            PanelId::Lettering => {
                self.sel_indices().len() == 1
                    && self.doc.xml.contains("data-tl-lettering")
                    && self
                        .sel_index()
                        .is_some_and(|i| self.doc.lettering(i).is_some())
            }
            _ => true,
        }
    }

    /// Whether a dock has anything in it (or is a drop target mid-drag).
    pub fn dock_shown(&self, side: Place) -> bool {
        let on = match side {
            Place::Left => self.dock.left,
            _ => self.dock.right,
        };
        on && (!self.panels_in(side).is_empty() || self.dock_ui.drag.is_some())
    }

    pub fn dock_iconic(&self, side: Place) -> bool {
        self.dock.iconic[side.side()]
    }

    /// One panel, header and body.
    fn show_panel(&mut self, ui: &mut egui::Ui, id: PanelId, place: Place) {
        self.dock_ui.current = Some(place);
        let top = ui.cursor().top();
        // A tab group shows one header with a tab each, and its front tab.
        let lead = self.dock.lead_of(id);
        let shown = self.group_shown(lead);
        let id = if shown.len() > 1 {
            let front = self.dock.active[lead.index()]
                .filter(|a| shown.contains(a))
                .unwrap_or(shown[0]);
            self.dock_ui.tabs = Some((lead, shown));
            front
        } else {
            shown.first().copied().unwrap_or(id)
        };
        match id {
            PanelId::Stitch => {
                let d = self.stitch_header();
                self.panel_with(ui, id, d, |s, ui| s.stitch_body(ui));
                crate::profile::mark("d:stitch");
            }
            PanelId::Transform => {
                self.panel(ui, id, |s, ui| s.transform_body(ui));
                crate::profile::mark("d:transform");
            }
            PanelId::Lettering => {
                self.panel(ui, id, |s, ui| {
                    if let Some(i) = s.sel_index() {
                        s.lettering_panel(ui, i);
                    }
                });
                crate::profile::mark("d:lettering");
            }
            PanelId::Threads => {
                self.panel(ui, id, |s, ui| s.threads_body(ui));
                crate::profile::mark("d:threads");
            }
            PanelId::Document => {
                self.panel(ui, id, |s, ui| s.document_body(ui));
                crate::profile::mark("d:document");
            }
            PanelId::Objects => {
                self.panel(ui, id, |s, ui| s.objects_body(ui));
                crate::profile::mark("d:objects");
            }
            PanelId::Agent => {
                self.panel(ui, id, |s, ui| s.agent_body(ui));
                crate::profile::mark("d:agent");
            }
        }
        self.dock_ui.tabs = None;
        let r = Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
        self.dock_ui.drawn.push((place, lead, r));
    }

    /// A dock: its panels in a column, or (collapsed) a strip of icons.
    pub fn dock_column(&mut self, ui: &mut egui::Ui, side: Place) {
        let p = pal_ui(ui);
        self.dock_ui.columns[side.side()] = Some(ui.max_rect());
        self.dock_ui.column_h[side.side()] = ui.max_rect().height();
        let ids = self.panels_in(side);
        if self.dock_iconic(side) {
            // A header-high bar over the icons holding » / « (expand), level
            // with the headers beside it.
            let (bar, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_H), Sense::hover());
            ui.painter().rect_filled(bar, 0.0, p.header);
            ui.painter().hline(
                bar.x_range(),
                bar.bottom() - 0.5,
                Stroke::new(1.0, p.separator),
            );
            let b = Rect::from_center_size(bar.center(), Vec2::new(22.0, 20.0));
            let resp = ui.interact(
                b,
                ui.make_persistent_id(("dock-expand", side.side())),
                Sense::click(),
            );
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "Expand Panels")
            });
            if resp.hovered() {
                ui.painter().rect_filled(b, 4.0, p.hover);
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(b.center(), Vec2::splat(10.0)),
                if side == Place::Left {
                    Icon::ChevronsRight
                } else {
                    Icon::ChevronsLeft
                },
                if resp.hovered() { p.text } else { p.secondary },
            );
            if resp.on_hover_text("Expand the panels").clicked() {
                self.dock.iconic[side.side()] = false;
                self.dock_ui.flyout = None;
            }
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = theme::space::XS;
                ui.add_space(theme::space::S);
                for id in self.panels_flat_in(side) {
                    let on = self.dock_ui.flyout == Some(id);
                    // Click for the flyout; drag to move the panel elsewhere.
                    let (rect, r) =
                        ui.allocate_exact_size(Vec2::splat(30.0), Sense::click_and_drag());
                    r.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Button, true, on, id.title())
                    });
                    let bg = if on {
                        Some(if p.dark {
                            Color32::from_rgb(70, 70, 74)
                        } else {
                            Color32::from_rgb(222, 222, 224)
                        })
                    } else if r.hovered() {
                        Some(p.hover)
                    } else {
                        None
                    };
                    if let Some(bg) = bg {
                        ui.painter().rect_filled(rect, 6.0, bg);
                    }
                    icons::paint(
                        ui.painter(),
                        rect.shrink(8.0),
                        id.icon(),
                        if on { p.accent } else { p.text },
                    );
                    self.dock_ui.drawn.push((side, id, rect));
                    if r.drag_started_by(egui::PointerButton::Primary)
                        && let Some(at) = ui.input(|i| i.pointer.press_origin())
                    {
                        self.begin_panel_drag(id, rect, at, false);
                    }
                    if r.on_hover_text(id.title()).clicked() {
                        self.dock_ui.flyout = if on { None } else { Some(id) };
                        // A tab opens with its group, itself in front.
                        let lead = self.dock.lead_of(id);
                        self.dock.active[lead.index()] = Some(id);
                    }
                }
            });
            return;
        }
        // The Agent's group sits at the bottom of the dock, the rest above
        // it (scrolling on their own), a divider between to share the height.
        let pinned = ids
            .iter()
            .copied()
            .find(|&l| self.group_shown(l).contains(&PanelId::Agent));
        let rest: Vec<PanelId> = ids.iter().copied().filter(|&l| Some(l) != pinned).collect();
        ui.spacing_mut().item_spacing.y = 0.0;
        let total = ui.available_height();
        const DIVIDER: f32 = 7.0;
        let mut top_h = total;
        let mut block = None;
        if let Some(lead) = pinned {
            let full = if rest.is_empty() {
                total
            } else {
                (total * self.dock.agent_share)
                    .clamp(200.0_f32.min(total), (total - 120.0).max(200.0))
            };
            // Collapsing it gives the others the room back, smoothly.
            let open = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                body_id(lead),
                !self.dock.collapsed(lead),
            )
            .openness(ui.ctx());
            let h = HEADER_H + (full - HEADER_H) * open;
            if !rest.is_empty() {
                top_h = (total - h - DIVIDER).max(0.0);
            }
            block = Some((lead, full));
        }
        // The first panel's header carries the dock's « / ».
        self.dock_ui.collapse_btn = Some(side);
        if !rest.is_empty() || pinned.is_none() {
            egui::ScrollArea::vertical()
                .id_salt(("dock", side.side()))
                .auto_shrink([false, false])
                .max_height(top_h)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for id in rest {
                        self.show_panel(ui, id, side);
                    }
                    if self.dock_ui.drag.is_some() {
                        // Room to drop at the end of the column.
                        ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 60.0),
                            Sense::hover(),
                        );
                    }
                });
            if let Some((lead, _)) = block {
                // The divider: drag it to give the Agent more or less room.
                let (r, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), DIVIDER), Sense::drag());
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Splitter, true, "Agent panel divider")
                });
                let on = resp.hovered() || resp.dragged();
                if on && !self.dock.collapsed(lead) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                ui.painter().hline(
                    r.x_range(),
                    r.center().y,
                    Stroke::new(
                        if on { 2.0 } else { 1.0 },
                        if on { p.accent } else { p.separator },
                    ),
                );
                if resp.dragged() && !self.dock.collapsed(lead) && total > 0.0 {
                    let share = self.dock.agent_share - resp.drag_delta().y / total;
                    self.dock.agent_share = share.clamp(0.15, 0.95);
                }
            }
        }
        if let Some((lead, full)) = block {
            ui.spacing_mut().item_spacing.y = 0.0;
            self.dock_ui.pinned_h = Some(full);
            self.show_panel(ui, lead, side);
            self.dock_ui.pinned_h = None;
        }
        // (Nothing in the dock to carry it: an empty dock mid-drag.)
        self.dock_ui.collapse_btn = None;
    }

    /// Panels outside the docks: floating ones, and the flyout from an icon strip.
    pub fn floating_panels(&mut self, ctx: &egui::Context) {
        for id in self.panels_in(Place::Float) {
            let at = self.dock.float_at[id.index()];
            egui::Area::new(egui::Id::unique(("float-panel", id.index())))
                .order(egui::Order::Middle)
                .fixed_pos(at)
                .constrain(true)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .inner_margin(egui::Margin::ZERO)
                        .corner_radius(8)
                        .show(ui, |ui| {
                            ui.set_width(float_w(id));
                            ui.spacing_mut().item_spacing.y = 0.0;
                            self.dock_ui.in_window = true;
                            self.show_panel(ui, id, Place::Float);
                            self.dock_ui.in_window = false;
                        });
                });
        }
        // The flyout beside an icon strip.
        let Some(id) = self.dock_ui.flyout else {
            return;
        };
        let place = self.dock.place_of(id);
        let side_iconic = place != Place::Float && self.dock_iconic(place);
        let anchor = self
            .dock_ui
            .drawn
            .iter()
            .find(|(pl, p, _)| *p == id && *pl == place)
            .map(|x| x.2);
        let (Some(anchor), true) = (anchor, side_iconic) else {
            self.dock_ui.flyout = None;
            return;
        };
        let (pivot, pos) = match place {
            Place::Left => (
                egui::Align2::LEFT_TOP,
                Pos2::new(anchor.right() + theme::space::S, anchor.top()),
            ),
            _ => (
                egui::Align2::RIGHT_TOP,
                Pos2::new(anchor.left() - theme::space::S, anchor.top()),
            ),
        };
        let max_h = (ctx.content_rect().bottom() - pos.y - 40.0).max(200.0);
        let area = egui::Area::new(egui::Id::unique("panel-flyout"))
            .order(egui::Order::Foreground)
            .pivot(pivot)
            .fixed_pos(pos)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::ZERO)
                    .corner_radius(8)
                    .show(ui, |ui| {
                        ui.set_width(float_w(id));
                        egui::ScrollArea::vertical()
                            .id_salt("flyout-scroll")
                            .max_height(max_h)
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                self.dock_ui.in_window = true;
                                self.show_panel(ui, id, place);
                                self.dock_ui.in_window = false;
                            });
                    });
            });
        // A click elsewhere closes it (not one in its own menus).
        let outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p) && !anchor.contains(p))
        });
        if outside && !egui::Popup::is_any_open(ctx) {
            self.dock_ui.flyout = None;
        }
    }

    /// Where a dragged panel would land if dropped at `pos`: on another
    /// panel's header (into its tab group), in a dock before a panel, or
    /// floating.
    fn drop_target(&self, pos: Pos2, d: PanelDrag) -> DropAt {
        let own = if d.whole {
            self.dock.lead_of(d.id)
        } else {
            d.id
        };
        // Another panel's header (an icon in a strip counts as one): into
        // its group. Its own group's header leaves things as they are.
        for (_, lead, r) in &self.dock_ui.drawn {
            let header = Rect::from_min_size(r.min, Vec2::new(r.width(), r.height().min(26.0)));
            // Its top edge inserts above it instead (an icon has no edge).
            let edge = if r.height() > 30.0 { 7.0 } else { 0.0 };
            if header.contains(pos) && pos.y > header.top() + edge {
                return DropAt::Tab(self.dock.lead_of(*lead));
            }
        }
        for side in [Place::Left, Place::Right] {
            if self.dock_ui.columns[side.side()].is_some_and(|r| r.contains(pos)) {
                let before = self
                    .dock_ui
                    .drawn
                    .iter()
                    .filter(|(pl, p, _)| *pl == side && *p != own)
                    .find(|(_, _, r)| pos.y < r.center().y)
                    .map(|x| x.1);
                return DropAt::Dock(side, before);
            }
        }
        DropAt::Float
    }

    /// Start dragging a panel by its header.
    fn begin_panel_drag(&mut self, id: PanelId, header: Rect, pointer: Pos2, whole: bool) {
        self.dock_ui.drag = Some(PanelDrag {
            id,
            grab: pointer - header.min,
            whole,
        });
        self.dock_ui.flyout = None;
    }

    /// Feedback while a panel is dragged, and the drop. Call after the
    /// panels are drawn.
    pub fn panel_drag(&mut self, ctx: &egui::Context) {
        // Each frame starts with a fresh record of where panels were drawn.
        let Some(d) = self.dock_ui.drag else {
            self.dock_ui.drawn.clear();
            self.dock_ui.columns = [None, None];
            return;
        };
        let (pos, down) = ctx.input(|i| (i.pointer.interact_pos(), i.pointer.primary_down()));
        let Some(pos) = pos else {
            self.dock_ui.drag = None;
            return;
        };
        let target = self.drop_target(pos, d);
        let pal = crate::theme::pal(ctx);
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::unique("panel-drag"),
        ));
        // Whether the dragged panel floats on its own already (it then just
        // follows the pointer).
        let floats_alone = self.dock.place_of(d.id) == Place::Float
            && (d.whole || self.dock.members(self.dock.lead_of(d.id)).len() == 1);
        match target {
            DropAt::Float => {
                if floats_alone {
                    let lead = self.dock.lead_of(d.id);
                    self.dock.float_at[lead.index()] = pos - d.grab;
                } else {
                    let r = Rect::from_min_size(pos - d.grab, Vec2::new(FLOAT_W, 26.0));
                    painter.rect_filled(r, 6.0, pal.accent.gamma_multiply(0.25));
                    painter.rect_stroke(
                        r,
                        6.0,
                        Stroke::new(1.0, pal.accent),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        r.left_center() + Vec2::new(10.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        d.id.title(),
                        egui::FontId::proportional(12.0),
                        pal.text,
                    );
                }
            }
            DropAt::Tab(lead) => {
                // The header it would join, outlined.
                if lead != self.dock.lead_of(d.id)
                    && let Some((_, _, r)) = self.dock_ui.drawn.iter().find(|x| x.1 == lead)
                {
                    let header =
                        Rect::from_min_size(r.min, Vec2::new(r.width(), r.height().min(26.0)));
                    painter.rect_filled(header, 4.0, pal.accent.gamma_multiply(0.2));
                    painter.rect_stroke(
                        header,
                        4.0,
                        Stroke::new(2.0, pal.accent),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            DropAt::Dock(side, before) => {
                let own = self.dock.lead_of(d.id);
                let col = self.dock_ui.columns[side.side()].unwrap_or(Rect::NOTHING);
                let y = match before {
                    Some(b) => self
                        .dock_ui
                        .drawn
                        .iter()
                        .find(|x| x.0 == side && x.1 == b)
                        .map(|x| x.2.top())
                        .unwrap_or(col.top()),
                    None => self
                        .dock_ui
                        .drawn
                        .iter()
                        .filter(|x| x.0 == side && x.1 != own)
                        .map(|x| x.2.bottom())
                        .fold(col.top() + 20.0, f32::max),
                };
                painter.line_segment(
                    [
                        Pos2::new(col.left() + 4.0, y),
                        Pos2::new(col.right() - 4.0, y),
                    ],
                    Stroke::new(3.0, pal.accent),
                );
            }
        }
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        if !down {
            self.dock_ui.drag = None;
            match target {
                DropAt::Tab(lead) => {
                    if lead != self.dock.lead_of(d.id) {
                        if !d.whole {
                            self.dock.separate(d.id);
                        }
                        self.dock.join_tab(d.id, lead);
                    }
                }
                DropAt::Float if floats_alone => {}
                DropAt::Float => {
                    if !d.whole {
                        self.dock.separate(d.id);
                    }
                    self.dock.float_at[d.id.index()] = pos - d.grab;
                    self.dock.move_panel(d.id, Place::Float, None);
                }
                DropAt::Dock(side, before) => {
                    if !d.whole {
                        self.dock.separate(d.id);
                    }
                    self.dock.move_panel(d.id, side, before);
                }
            }
        }
        ctx.request_repaint();
        self.dock_ui.drawn.clear();
        self.dock_ui.columns = [None, None];
    }

    /// The Objects list as a tree: groups (open or closed) and their objects,
    /// in sewing order. Layers aren't groups (see `svg::layer_ids`), and a
    /// group of one object is listed as the object.
    fn object_rows(&self) -> Vec<ObjRow> {
        let n = self.doc.shapes.len();
        let mut members: std::collections::HashMap<&str, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, s) in self.doc.shapes.iter().enumerate() {
            for g in &s.groups {
                members.entry(g.as_str()).or_default().push(i);
            }
        }
        let mut rows = Vec::new();
        let mut stack: Vec<String> = Vec::new();
        for i in 0..n {
            // A group of one is shown as just its object.
            let gs: Vec<&String> = self.doc.shapes[i]
                .groups
                .iter()
                .filter(|g| members[g.as_str()].len() > 1)
                .collect();
            let mut k = 0;
            while k < stack.len() && k < gs.len() && stack[k] == *gs[k] {
                k += 1;
            }
            stack.truncate(k);
            for g in &gs[k..] {
                if stack.iter().all(|s| self.layout.expanded.contains(s)) {
                    rows.push(ObjRow::Group {
                        id: (*g).clone(),
                        depth: stack.len(),
                        members: members[g.as_str()].clone(),
                    });
                }
                stack.push((*g).clone());
            }
            if stack.iter().all(|s| self.layout.expanded.contains(s)) {
                rows.push(ObjRow::Item {
                    i,
                    depth: stack.len(),
                });
            }
        }
        rows
    }

    fn objects_body(&mut self, ui: &mut egui::Ui) {
        let p = pal_ui(ui);
        let rows = self.object_rows();
        // The list fills its dock when it's alone there; sharing, it's as
        // tall as its rows, up to a third of the dock (then it scrolls).
        let list_h = match self.dock_ui.current {
            Some(Place::Float) | None => 320.0,
            Some(side) if self.dock_iconic(side) => 320.0,
            Some(side) => {
                let h = self.dock_ui.column_h[side.side()];
                if self.panels_in(side).len() > 1 {
                    (rows.len().max(3) as f32 * 26.0 + 8.0).clamp(110.0, (h * 0.36).max(110.0))
                } else {
                    (h - 20.0 - 26.0 - 64.0).max(120.0)
                }
            }
        };
        let sel = self.sel_index();
        let all_sel: std::collections::HashSet<usize> = self.sel_indices().into_iter().collect();
        // Objects with a warning, found once rather than per row.
        let warned: std::collections::HashSet<usize> = self
            .issues
            .iter()
            .filter(|x| x.level >= Level::Warning)
            .filter_map(|x| x.shape)
            .collect();
        let toggle = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        let mut clicked = None;
        let mut group_click: Option<Vec<usize>> = None;
        // Only the rows in view are laid out and painted (a traced design
        // has hundreds; each row is text, an icon and a hover check).
        const ROW: f32 = 26.0;
        egui::ScrollArea::vertical()
            .id_salt("objects")
            .max_height(list_h)
            .auto_shrink([false, false])
            .show_rows(ui, ROW, rows.len(), |ui, range| {
                if self.doc.shapes.is_empty() {
                    ui.add_space(8.0);
                    caption(
                        ui,
                        "No objects yet. Open or drop an SVG, add text, or draw with the pen.",
                    );
                }
                for row in &rows[range] {
                    let (i, depth) = match row {
                        ObjRow::Group { id, depth, members } => {
                            let indent = *depth as f32 * 14.0;
                            let (rect, resp) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), ROW),
                                Sense::click(),
                            );
                            let open = self.layout.expanded.contains(id);
                            let label = Studio::group_label(id);
                            let all = members.iter().all(|m| all_sel.contains(m));
                            let some = members.iter().any(|m| all_sel.contains(m));
                            resp.widget_info(|| {
                                egui::WidgetInfo::selected(egui::Role::ListItem, true, all, &label)
                            });
                            let painter = ui.painter();
                            if all {
                                painter.rect_filled(rect, 5.0, p.accent);
                            } else if resp.hovered() {
                                painter.rect_filled(rect, 5.0, p.hover);
                            } else if some {
                                painter.rect_filled(rect, 5.0, p.accent.gamma_multiply(0.18));
                            }
                            let fg = if all { Color32::WHITE } else { p.text };
                            let fg2 = if all {
                                Color32::from_white_alpha(200)
                            } else {
                                p.secondary
                            };
                            let chev = Rect::from_center_size(
                                Pos2::new(rect.left() + indent + 10.0, rect.center().y),
                                Vec2::splat(9.0),
                            );
                            icons::paint(
                                painter,
                                chev,
                                if open {
                                    Icon::ChevronDown
                                } else {
                                    Icon::ChevronRight
                                },
                                fg2,
                            );
                            // A folder-ish stack of two squares.
                            let f = Rect::from_center_size(
                                Pos2::new(rect.left() + indent + 27.0, rect.center().y + 1.0),
                                Vec2::new(13.0, 10.0),
                            );
                            painter.rect_stroke(
                                f,
                                2.0,
                                Stroke::new(1.2, fg2),
                                egui::StrokeKind::Middle,
                            );
                            painter.line_segment(
                                [
                                    f.left_top() + Vec2::new(2.0, -3.0),
                                    f.right_top() + Vec2::new(-2.0, -3.0),
                                ],
                                Stroke::new(1.2, fg2),
                            );
                            let stitches: usize = members
                                .iter()
                                .map(|&m| self.design.per_shape.get(m).copied().unwrap_or(0))
                                .sum();
                            let right = rect.right() - 8.0;
                            let galley = painter.layout_no_wrap(
                                fmt_int(stitches),
                                egui::FontId::proportional(11.0),
                                fg2,
                            );
                            let cw = galley.size().x;
                            painter.galley(
                                Pos2::new(right - cw, rect.center().y - galley.size().y / 2.0),
                                galley,
                                fg2,
                            );
                            let name_rect = Rect::from_min_max(
                                Pos2::new(rect.left() + indent + 40.0, rect.top()),
                                Pos2::new(right - cw - 6.0, rect.bottom()),
                            );
                            painter.with_clip_rect(name_rect).text(
                                Pos2::new(name_rect.left(), rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                format!("{label}  ·  {}", members.len()),
                                egui::FontId::proportional(13.0),
                                fg,
                            );
                            let on_chevron = resp
                                .interact_pointer_pos()
                                .is_some_and(|q| q.x < rect.left() + indent + 18.0);
                            if (resp.clicked() && on_chevron) || resp.double_clicked() {
                                if open {
                                    self.layout.expanded.remove(id);
                                } else {
                                    self.layout.expanded.insert(id.clone());
                                }
                            } else if resp.clicked() || (resp.secondary_clicked() && !all) {
                                group_click = Some(members.clone());
                            }
                            resp.on_hover_text(format!(
                                "{label}: {} objects, {} stitches. Click to select the group.",
                                members.len(),
                                fmt_int(stitches)
                            ))
                            .context_menu(|ui| self.object_menu_items(ui));
                            continue;
                        }
                        ObjRow::Item { i, depth } => (*i, *depth),
                    };
                    let indent = depth as f32 * 14.0;
                    let (kind, color) = {
                        let s = &self.doc.shapes[i];
                        (s.params.kind.unwrap_or(Kind::Fill), s.color)
                    };
                    let count = self.design.per_shape.get(i).copied().unwrap_or(0);
                    let (rect, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::click());
                    let label = self.names.get(i).cloned().unwrap_or_default();
                    let selected = all_sel.contains(&i);
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::ListItem, true, selected, &label)
                    });
                    let painter = ui.painter();
                    if selected {
                        let c = if Some(i) == sel {
                            p.accent
                        } else {
                            p.accent.gamma_multiply(0.55)
                        };
                        painter.rect_filled(rect, 5.0, c);
                    } else if resp.hovered() || self.hovered == Some(i) {
                        painter.rect_filled(rect, 5.0, p.hover);
                    }
                    let fg = if selected { Color32::WHITE } else { p.text };
                    let fg2 = if selected {
                        Color32::from_white_alpha(200)
                    } else {
                        p.secondary
                    };
                    let dot = Pos2::new(rect.left() + indent + 12.0, rect.center().y);
                    painter.circle_filled(dot, 5.5, crate::canvas::color32(color));
                    painter.circle_stroke(
                        dot,
                        5.5,
                        Stroke::new(1.0, Color32::from_black_alpha(50)),
                    );
                    let icon = Rect::from_center_size(
                        Pos2::new(rect.left() + indent + 30.0, rect.center().y),
                        Vec2::splat(13.0),
                    );
                    draw_kind_icon(painter, icon, kind, fg2);
                    let name_color = if kind == Kind::Skip { fg2 } else { fg };
                    let right = rect.right() - 8.0;
                    let count_text = if kind == Kind::Skip {
                        "off".to_string()
                    } else {
                        fmt_int(count)
                    };
                    let galley =
                        painter.layout_no_wrap(count_text, egui::FontId::proportional(11.0), fg2);
                    let cw = galley.size().x;
                    painter.galley(
                        Pos2::new(right - cw, rect.center().y - galley.size().y / 2.0),
                        galley,
                        fg2,
                    );
                    let warn = warned.contains(&i);
                    let mut name_right = right - cw - 6.0;
                    if warn {
                        let wr = Rect::from_center_size(
                            Pos2::new(name_right - 6.0, rect.center().y),
                            Vec2::splat(11.0),
                        );
                        icons::paint(
                            painter,
                            wr,
                            Icon::Warning,
                            if selected { Color32::WHITE } else { p.warning },
                        );
                        name_right -= 16.0;
                    }
                    let name_rect = Rect::from_min_max(
                        Pos2::new(rect.left() + indent + 42.0, rect.top()),
                        Pos2::new(name_right, rect.bottom()),
                    );
                    painter.with_clip_rect(name_rect).text(
                        Pos2::new(name_rect.left(), rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        &label,
                        egui::FontId::proportional(13.0),
                        name_color,
                    );
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    if resp.secondary_clicked() && !selected {
                        clicked = Some(i);
                    }
                    resp.context_menu(|ui| self.object_menu_items(ui));
                }
            });
        if let Some(i) = clicked {
            if toggle {
                self.toggle_selected(i);
            } else if !(ui.input(|i| i.pointer.secondary_released()) && all_sel.contains(&i)) {
                self.set_selection(&[i]);
            }
        }
        if let Some(members) = group_click {
            if toggle {
                let mut cur = self.sel_indices();
                if members.iter().all(|m| cur.contains(m)) {
                    cur.retain(|m| !members.contains(m));
                } else {
                    cur.extend(members.iter().filter(|m| !all_sel.contains(m)));
                }
                self.set_selection(&cur);
            } else {
                self.set_selection(&members);
            }
        }
        // Bottom bar, like a Mac source list.
        theme::hline(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let add = icon_button(ui, Icon::Plus, "Add", false, 24.0);
            egui::Popup::menu(&add).show(|ui| {
                if ui.button("Add Text…").clicked() {
                    self.open_text_window();
                }
                if ui.button("Place SVG…").clicked() {
                    self.run_action(crate::commands::Action::Place);
                }
                if ui.button("Draw with Pen").clicked() {
                    self.set_tool(Tool::Pen);
                }
            });
            let has = !self.sel_indices().is_empty();
            ui.add_enabled_ui(has, |ui| {
                if icon_button(ui, Icon::Minus, "Delete", false, 24.0).clicked() {
                    self.delete_selection();
                }
                ui.add_space(theme::space::S);
                if icon_button(ui, Icon::ArrowUp, "Sew earlier", false, 24.0).clicked() {
                    self.move_selected(-1);
                }
                if icon_button(ui, Icon::ArrowDown, "Sew later", false, 24.0).clicked() {
                    self.move_selected(1);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                caption(ui, format!("{} objects", self.doc.shapes.len()));
            });
        });
    }

    /// Stitch type and editing commands for the selection (context menus).
    pub fn object_menu_items(&mut self, ui: &mut egui::Ui) {
        use crate::commands::Action as A;
        use crate::flyout::{RowIcon, menu_heading, menu_row, menu_separator};
        let Some(i) = self.sel_index() else {
            ui.label("Nothing selected");
            return;
        };
        ui.set_min_width(240.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        // Stitch types, each with its icon; a square beside the current one.
        menu_heading(ui, "Stitch Type");
        let current = self.doc.shapes[i].params.kind.unwrap_or(Kind::Fill);
        for k in allowed_kinds(&self.doc.shapes[i]) {
            if menu_row(ui, RowIcon::Kind(k), kind_name(k), "", current == k, true).clicked() {
                self.set_kind_selected(k);
                ui.close();
            }
        }
        menu_separator(ui);
        self.action_row(
            ui,
            A::Duplicate,
            "Duplicate",
            RowIcon::Icon(Icon::Duplicate),
        );
        self.action_row(ui, A::Copy, "Copy", RowIcon::None);
        self.action_row(ui, A::Paste, "Paste", RowIcon::None);
        menu_separator(ui);
        self.action_row(ui, A::FlipH, "Flip Horizontal", RowIcon::Icon(Icon::FlipH));
        self.action_row(ui, A::FlipV, "Flip Vertical", RowIcon::Icon(Icon::FlipV));
        self.action_row(
            ui,
            A::BringToFront,
            "Sew Last",
            RowIcon::Icon(Icon::ArrowDown),
        );
        self.action_row(ui, A::SendToBack, "Sew First", RowIcon::Icon(Icon::ArrowUp));
        // One path drawing several shapes: offer to split it up.
        let compound = match &self.doc.shapes[i].geometry {
            zigzag::svg::Geometry::Fill { rings, .. } => rings.len() > 1,
            zigzag::svg::Geometry::Stroke { lines, .. } => lines.len() > 1,
        };
        if compound && self.doc.lettering(i).is_none() {
            menu_separator(ui);
            self.action_row(
                ui,
                A::ReleaseCompound,
                "Release Compound Path",
                RowIcon::None,
            );
        }
        menu_separator(ui);
        if menu_row(ui, RowIcon::Icon(Icon::Close), "Delete", "", false, true).clicked() {
            self.delete_selection();
            ui.close();
        }
    }

    /// The design's issues, each naming its object (click to select it).
    pub(crate) fn issues_body(&mut self, ui: &mut egui::Ui) {
        let p = pal_ui(ui);
        if self.issues.is_empty() {
            caption(ui, "No problems found.");
            return;
        }
        let mut pick = None;
        egui::ScrollArea::vertical()
            .id_salt("issues")
            .max_height(380.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for issue in &self.issues {
                    let (c, icon) = match issue.level {
                        Level::Error => (p.error, Icon::Warning),
                        Level::Warning => (p.warning, Icon::Warning),
                        Level::Info => (p.secondary, Icon::Info),
                    };
                    ui.horizontal_top(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
                        icons::paint(ui.painter(), r, icon, c);
                        ui.vertical(|ui| {
                            if let Some(i) = issue.shape
                                && ui
                                    .link(
                                        egui::RichText::new(
                                            self.names.get(i).cloned().unwrap_or_default(),
                                        )
                                        .size(12.0),
                                    )
                                    .clicked()
                            {
                                pick = Some(i);
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&issue.message)
                                        .size(12.0)
                                        .color(p.secondary),
                                )
                                .wrap(),
                            );
                        });
                    });
                    ui.add_space(4.0);
                }
            });
        if let Some(i) = pick {
            self.set_selection(&[i]);
        }
    }

    // ------------------------------------------------------------ right dock

    /// What the Stitch panel's header shows: the selection and its stitches.
    fn stitch_header(&self) -> Option<(String, String)> {
        let i = self.sel_index()?;
        let mut sel = self.sel_indices();
        let count = sel.len();
        let what = if count > 1 {
            sel.sort_unstable();
            match self.doc.shapes[i]
                .groups
                .iter()
                .find(|g| self.group_members(g) == sel)
            {
                Some(g) => format!("{} · {count} objects", Studio::group_label(g)),
                None => format!("{count} objects"),
            }
        } else {
            self.names.get(i).cloned().unwrap_or_default()
        };
        let stitches: usize = sel
            .iter()
            .map(|&j| self.design.per_shape.get(j).copied().unwrap_or(0))
            .sum();
        Some((what, format!("{} stitches", fmt_int(stitches))))
    }

    fn stitch_body(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = theme::space::S;
        let p = pal_ui(ui);
        let Some(i) = self.sel_index() else {
            stitch_type_guide(ui);
            return;
        };
        let count = self.sel_indices().len();
        let name = self.names.get(i).cloned().unwrap_or_default();
        if count > 1 {
            caption(
                ui,
                format!("Showing {name}'s settings. Stitch types apply to all."),
            );
        }
        // The stitch type: the main decision for every object.
        let allowed = allowed_kinds(&self.doc.shapes[i]);
        let current = self.doc.shapes[i].params.kind.unwrap_or(Kind::Fill);
        let filled = matches!(self.doc.shapes[i].geometry, Geometry::Fill { .. });
        let mut kind_clicked = None;
        // Tiles in four even columns that fill the width.
        let gap = theme::space::XS;
        let tile_w = ((ui.available_width() - 3.0 * gap) / 4.0).floor();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(gap);
            for row in allowed.chunks(4) {
                ui.horizontal(|ui| {
                    for &k in row {
                        if kind_tile(ui, k, k == current, tile_w, 52.0)
                            .on_hover_text(kind_help(k, filled))
                            .clicked()
                        {
                            kind_clicked = Some(k);
                        }
                    }
                });
            }
        });
        if let Some(k) = kind_clicked {
            self.set_kind_selected(k);
        }
        // The thread color is picked from the swatch in the tool strip.
        let f = self.doc.settings.fabric();
        theme::section_break(ui);

        let default_width = design::default_satin_width(&self.doc.shapes[i]);
        // Only a satin needs its width (worked out once, not every frame;
        // not at all while its text is being typed, when every keystroke
        // would mean finding the rails of every letter again).
        let satin_width = if current == Kind::Satin && !self.typing_into(i) {
            self.satin_info(i).mean_width
        } else if current == Kind::Satin {
            default_width
        } else {
            0.0
        };
        let mut to_angle_tool = false;
        let color_now = self.doc.shapes[i].color;
        let pr = &mut self.doc.shapes[i].params;
        form(ui, "stitch-form", |fm| match current {
            Kind::Fill => {
                fm.row("Angle", |ui, lid| {
                    let shown = pr.angle.unwrap_or(defaults::FILL_ANGLE);
                    let mut v = shown;
                    ui.spacing_mut().slider_width = 100.0;
                    let r = ui
                        .add(
                            egui::Slider::new(&mut v, -90.0..=90.0)
                                .show_value(false)
                                .step_by(1.0),
                        )
                        .labelled_by(lid);
                    let d = ui.add(egui::DragValue::new(&mut v).range(-90.0..=90.0).suffix("°"));
                    // Only a user's change counts: the slider snaps to whole
                    // degrees, and a non-uniform scale leaves fractions.
                    let touched = r.dragged()
                        || r.clicked()
                        || r.has_focus()
                        || d.dragged()
                        || d.has_focus()
                        || d.lost_focus();
                    if (r.changed() || d.changed()) && touched && v != shown {
                        pr.angle = Some(v);
                    }
                    if ui
                        .add(egui::Button::new("Draw").small())
                        .on_hover_text("Drag across the shape to set the angle (G)")
                        .clicked()
                    {
                        to_angle_tool = true;
                    }
                });
                value_row(
                    fm,
                    "Spacing",
                    &mut pr.spacing,
                    f.fill_spacing,
                    0.15..=0.8,
                    " mm",
                    0.01,
                    "Row spacing: smaller is denser",
                );
                value_row(
                    fm,
                    "Length",
                    &mut pr.len,
                    f.fill_len,
                    1.5..=7.0,
                    " mm",
                    0.1,
                    "Longest stitch in a row",
                );
                value_row(
                    fm,
                    "Pull comp.",
                    &mut pr.pull,
                    f.fill_pull,
                    0.0..=0.6,
                    " mm",
                    0.05,
                    "Extends rows at the edges to counter fabric pulling in",
                );
                fm.row("Underlay", |ui, _| {
                    underlay_box(
                        ui,
                        pr,
                        if f.fill_edge_walk {
                            "Edge walk + rows"
                        } else {
                            "Stabilizing rows"
                        },
                    )
                });
            }
            Kind::Satin => {
                if filled {
                    fm.row("Shape", |ui, _| {
                        let mut border = pr.is_border();
                        if theme::segmented(
                            ui,
                            "satin-shape",
                            &mut border,
                            &[(false, "Column"), (true, "Border")],
                        ) {
                            pr.border = border.then_some(true);
                        }
                    });
                }
                fm.row("Stitch", |ui, _| {
                    use zigzag::stitch::satin::SatinStyle;
                    let mut st = pr.satin_style();
                    if theme::segmented(
                        ui,
                        "satin-stitch",
                        &mut st,
                        &[
                            (SatinStyle::Plain, "Satin"),
                            (SatinStyle::SStitch, "S-stitch"),
                            (SatinStyle::Zigzag, "Zig-zag"),
                        ],
                    ) {
                        pr.style = (st != SatinStyle::Plain).then_some(st);
                    }
                });
                fm.row("Threads", |ui, _| {
                    // Multicolor satin: the threads across the column.
                    let mut colors = pr.colors.clone().unwrap_or_default();
                    let mut changed = false;
                    for c in colors.iter_mut() {
                        changed |= ui.color_edit_button_srgb(c).changed();
                    }
                    if colors.len() < 4
                        && ui
                            .add(egui::Button::new("+").small())
                            .on_hover_text("Add a thread: the column is split across its width, one strip per thread")
                            .clicked()
                    {
                        let last = colors.last().copied().unwrap_or(color_now);
                        if colors.is_empty() {
                            colors.push(color_now);
                        }
                        colors.push([255 - last[0], 255 - last[1], 255 - last[2]]);
                        changed = true;
                    }
                    if !colors.is_empty()
                        && ui
                            .add(egui::Button::new("−").small())
                            .on_hover_text("Remove the last thread")
                            .clicked()
                    {
                        colors.pop();
                        if colors.len() < 2 {
                            colors.clear();
                        }
                        changed = true;
                    }
                    if colors.is_empty() {
                        caption(ui, "one");
                    }
                    if changed {
                        pr.colors = (colors.len() >= 2).then_some(colors);
                    }
                });
                if !filled || pr.is_border() {
                    value_row(
                        fm,
                        "Width",
                        &mut pr.width,
                        default_width,
                        0.6..=10.0,
                        " mm",
                        0.1,
                        "",
                    );
                }
                value_row(
                    fm,
                    "Density",
                    &mut pr.spacing,
                    f.satin_spacing,
                    0.2..=1.0,
                    " mm",
                    0.01,
                    "Distance between zigzags (auto-adjusted for width unless set)",
                );
                value_row(
                    fm,
                    "Pull comp.",
                    &mut pr.pull,
                    f.satin_pull,
                    0.0..=0.6,
                    " mm",
                    0.05,
                    "Extra width per side; satins pull in narrower than drawn",
                );
                fm.row("Underlay", |ui, _| {
                    let mut auto = pr.satin_underlay.is_none() && pr.underlay_on();
                    if ui
                        .checkbox(&mut auto, "Automatic")
                        .on_hover_text(
                            "None under 1.5 mm, center walk to 4 mm, contour + zigzag above",
                        )
                        .changed()
                    {
                        pr.satin_underlay = if auto {
                            None
                        } else {
                            Some(pr.satin_underlay_for(satin_width))
                        };
                        pr.underlay = None;
                    }
                });
                let current_ul = pr.satin_underlay_for(satin_width);
                fm.wide(|ui| {
                    for (bit, name, tip) in [
                        (
                            underlay::CENTER,
                            "Center",
                            "A run down the middle: anchors narrow columns",
                        ),
                        (
                            underlay::CONTOUR,
                            "Contour",
                            "Runs just inside both edges: crisp edges on wide columns",
                        ),
                        (
                            underlay::ZIGZAG,
                            "Zigzag",
                            "Sparse zigzag: lifts and supports wide columns",
                        ),
                    ] {
                        let mut on = current_ul & bit != 0;
                        if ui.checkbox(&mut on, name).on_hover_text(tip).changed() {
                            pr.satin_underlay = Some(if on {
                                current_ul | bit
                            } else {
                                current_ul & !bit
                            });
                            pr.underlay = None;
                        }
                    }
                });
                fm.row("Curves", |ui, _| {
                    let mut short = pr.short_stitches_on();
                    if ui
                        .checkbox(&mut short, "Short stitches")
                        .on_hover_text("Shortens every other stitch on the inside of tight curves")
                        .changed()
                    {
                        pr.short_stitches = Some(short);
                    }
                });
                fm.row("Auto split", |ui, lid| {
                    let mut on = pr.satin_split().is_some();
                    if ui
                        .checkbox(&mut on, "")
                        .labelled_by(lid)
                        .on_hover_text("Split stitches longer than this so wide columns don't snag")
                        .changed()
                    {
                        pr.split = Some(if on { defaults::SATIN_SPLIT } else { 0.0 });
                    }
                    if let Some(mut v) = pr.satin_split() {
                        let r = ui.add(
                            egui::DragValue::new(&mut v)
                                .range(3.0..=12.0)
                                .speed(0.1)
                                .fixed_decimals(1)
                                .suffix(" mm"),
                        );
                        r.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Slider, true, "Split length")
                        });
                        if r.changed() {
                            pr.split = Some(v);
                        }
                    }
                });
            }
            Kind::Contour => {
                fm.row("Style", |ui, _| {
                    let mut spiral = pr.spiral == Some(true);
                    if theme::segmented(
                        ui,
                        "contour-style",
                        &mut spiral,
                        &[(false, "Rings"), (true, "Spiral")],
                    ) {
                        pr.spiral = spiral.then_some(true);
                    }
                });
                value_row(
                    fm,
                    "Spacing",
                    &mut pr.spacing,
                    defaults::CONTOUR_SPACING,
                    0.25..=1.5,
                    " mm",
                    0.05,
                    "Distance between the outline-following rows",
                );
                value_row(
                    fm,
                    "Length",
                    &mut pr.len,
                    f.run_len,
                    1.0..=5.0,
                    " mm",
                    0.1,
                    "",
                );
            }
            Kind::Blanket => {
                value_row(
                    fm,
                    "Spacing",
                    &mut pr.spacing,
                    defaults::BLANKET_SPACING,
                    1.0..=6.0,
                    " mm",
                    0.1,
                    "Distance between prongs",
                );
                value_row(
                    fm,
                    "Prong",
                    &mut pr.width,
                    defaults::BLANKET_WIDTH,
                    0.8..=6.0,
                    " mm",
                    0.1,
                    "Prong length",
                );
            }
            Kind::Applique => {
                value_row(
                    fm,
                    "Cover",
                    &mut pr.width,
                    defaults::APPLIQUE_WIDTH,
                    1.5..=6.0,
                    " mm",
                    0.1,
                    "Satin cover width",
                );
                value_row(
                    fm,
                    "Density",
                    &mut pr.spacing,
                    f.satin_spacing,
                    0.2..=1.0,
                    " mm",
                    0.01,
                    "",
                );
                fm.row("Underlay", |ui, _| underlay_box(ui, pr, "Under the cover"));
            }
            Kind::Manual => {}
            Kind::Meander => {
                value_row(
                    fm,
                    "Spacing",
                    &mut pr.spacing,
                    defaults::MEANDER_SPACING,
                    1.0..=8.0,
                    " mm",
                    0.1,
                    "How close the wandering line comes to itself",
                );
                value_row(
                    fm,
                    "Length",
                    &mut pr.len,
                    f.run_len,
                    1.0..=5.0,
                    " mm",
                    0.1,
                    "",
                );
            }
            Kind::Ripple => {
                fm.row("Style", |ui, _| {
                    let mut spiral = pr.spiral == Some(true);
                    if theme::segmented(
                        ui,
                        "ripple-style",
                        &mut spiral,
                        &[(false, "Rings"), (true, "Spiral")],
                    ) {
                        pr.spiral = spiral.then_some(true);
                    }
                });
                value_row(
                    fm,
                    "Spacing",
                    &mut pr.spacing,
                    defaults::RIPPLE_SPACING,
                    0.5..=6.0,
                    " mm",
                    0.1,
                    "Distance between the rows",
                );
                value_row(
                    fm,
                    "Length",
                    &mut pr.len,
                    f.run_len,
                    1.0..=5.0,
                    " mm",
                    0.1,
                    "",
                );
            }
            Kind::Cross => {
                value_row(
                    fm,
                    "Cell",
                    &mut pr.cell,
                    defaults::CROSS_CELL,
                    1.0..=8.0,
                    " mm",
                    0.1,
                    "Size of each X",
                );
            }
            Kind::Tartan => {
                // The sett: a colour and width per stripe (mirrored into a
                // symmetric pattern), one row each.
                let mut sett = pr.tartan_sett(color_now);
                let mut changed = false;
                let mut remove = None;
                let n = sett.len();
                for (k, (c, w)) in sett.iter_mut().enumerate() {
                    fm.row(if k == 0 { "Stripes" } else { "" }, |ui, _| {
                        changed |= ui.color_edit_button_srgb(c).changed();
                        changed |= ui.add(crate::units::drag_mm(w, 0.5..=30.0, 0.1)).changed();
                        if n > 1
                            && ui
                                .add(egui::Button::new("−").small())
                                .on_hover_text("Remove this stripe")
                                .clicked()
                        {
                            remove = Some(k);
                        }
                    });
                }
                fm.row("", |ui, _| {
                    if sett.len() < 8
                        && ui
                            .add(egui::Button::new("+ Stripe").small())
                            .on_hover_text("The stripes repeat mirrored: a b c b a b c…")
                            .clicked()
                    {
                        let last = sett.last().map(|s| s.0).unwrap_or(color_now);
                        sett.push(([255 - last[0], 255 - last[1], 255 - last[2]], 2.0));
                        changed = true;
                    }
                });
                if let Some(k) = remove {
                    sett.remove(k);
                    changed = true;
                }
                if changed {
                    pr.sett = Some(sett);
                }
                fm.row("Angle", |ui, lid| {
                    let mut v = pr.angle.unwrap_or(0.0);
                    let r = ui
                        .add(egui::DragValue::new(&mut v).range(-90.0..=90.0).suffix("°"))
                        .labelled_by(lid);
                    if r.changed() {
                        pr.angle = Some(v);
                    }
                });
                value_row(
                    fm,
                    "Spacing",
                    &mut pr.spacing,
                    f.fill_spacing,
                    0.2..=0.8,
                    " mm",
                    0.01,
                    "Row spacing within each stripe",
                );
            }
            Kind::Run => {
                value_row(
                    fm,
                    "Length",
                    &mut pr.len,
                    f.run_len,
                    1.0..=5.0,
                    " mm",
                    0.1,
                    "",
                );
                fm.row("Passes", |ui, _| {
                    let mut r = pr.run_repeats();
                    if theme::segmented(ui, "passes", &mut r, &[(1, "Single"), (3, "Triple")]) {
                        pr.repeats = Some(r);
                    }
                });
            }
            Kind::Skip => fm.wide(|ui| {
                caption(ui, "This object won't be stitched.");
            }),
        });
        if to_angle_tool {
            self.tool = Tool::Angle;
        }
        if current != Kind::Skip {
            self.sewing_rows(ui, i);
        }
        for issue in self.issues.iter().filter(|x| x.shape == Some(i)) {
            let (c, icon) = if issue.level >= Level::Warning {
                (p.warning, Icon::Warning)
            } else {
                (p.secondary, Icon::Info)
            };
            callout(ui, c, icon, &issue.message);
        }
        ui.add_space(theme::space::M - ui.spacing().item_spacing.y);
        if ui
            .add(egui::Button::new("Reset to Defaults").small())
            .on_hover_text("Back to the fabric's defaults for this stitch type")
            .clicked()
        {
            let kind = self.doc.shapes[i].params.kind;
            self.doc.shapes[i].params = Params {
                kind,
                ..Params::default()
            };
        }
    }

    /// Where object `i` starts and ends sewing, and what happens after it
    /// (Ink/Stitch's start, end, trim and stop commands).
    fn sewing_rows(&mut self, ui: &mut egui::Ui, i: usize) {
        use crate::view_tools::Marker;
        theme::section_break(ui);
        let p = pal_ui(ui);
        let picking = self.view_tools.marker_pick;
        let mut pick: Option<Option<Marker>> = None;
        let mut clear: Option<Marker> = None;
        let pr = &mut self.doc.shapes[i].params;
        form(ui, "sewing-form", |fm| {
            fm.row("Sewing", |ui, _| {
                for (m, label, set) in [
                    (Marker::Start, "Start", pr.start.is_some()),
                    (Marker::End, "End", pr.end.is_some()),
                ] {
                    let on = picking == Some(m);
                    let text = egui::RichText::new(label).color(if set && !on {
                        p.accent
                    } else {
                        p.text
                    });
                    let r = ui
                        .add(egui::Button::new(text).selected(on).small())
                        .on_hover_text(match m {
                            Marker::Start => {
                                "Click on the outline where this object should start sewing (drag the marker to move it)"
                            }
                            Marker::End => {
                                "Click on the outline where this object should finish (the thread goes on from there)"
                            }
                        });
                    if r.clicked() {
                        pick = Some(if on { None } else { Some(m) });
                    }
                    if set && r.secondary_clicked() {
                        clear = Some(m);
                    }
                }
                if (pr.start.is_some() || pr.end.is_some())
                    && ui
                        .add(egui::Button::new("Clear").small())
                        .on_hover_text("Let the planner choose where to start and end")
                        .clicked()
                {
                    pr.start = None;
                    pr.end = None;
                    pick = Some(None);
                }
                ui.add_space(theme::space::S);
                let mut trim = pr.trim == Some(true);
                if ui
                    .checkbox(&mut trim, "Trim")
                    .on_hover_text("Cut the thread after this object, even if the next one is close")
                    .changed()
                {
                    pr.trim = trim.then_some(true);
                }
                let mut stop = pr.stop == Some(true);
                if ui
                    .checkbox(&mut stop, "Stop")
                    .on_hover_text("Pause the machine after this object (to lay foam, or trim appliqué fabric)")
                    .changed()
                {
                    pr.stop = stop.then_some(true);
                }
            });
        });
        if let Some(m) = clear {
            match m {
                Marker::Start => pr.start = None,
                Marker::End => pr.end = None,
            }
        }
        if let Some(m) = pick {
            self.view_tools.marker_pick = m;
            if let Some(m) = m {
                self.status = format!(
                    "Click on the object's outline where it should {}. Esc cancels.",
                    match m {
                        Marker::Start => "start sewing",
                        Marker::End => "finish",
                    }
                );
            }
        }
    }

    /// The threads in sewing order: one row per color block, a stop where
    /// the same thread comes twice running. Click a row for its objects;
    /// click its swatch to change that block's thread.
    pub(crate) fn sequence_list(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let p = pal_ui(ui);
        if self.design.blocks.is_empty() {
            caption(ui, "Nothing to sew yet.");
            return;
        }
        let selected = self.sel_indices();
        let mut pick = None;
        let mut picker = None;
        for (k, blk) in self.design.blocks.iter().enumerate() {
            let n: usize = blk.runs.iter().map(|r| r.pts.len()).sum();
            let stop = k > 0 && self.design.blocks[k - 1].color == blk.color;
            let (rect, resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), theme::ROW_H),
                Sense::click(),
            );
            let objects = self.design.block_shapes.get(k).cloned().unwrap_or_default();
            let on = !selected.is_empty()
                && objects.len() == selected.len()
                && objects.iter().all(|i| selected.contains(i));
            if on {
                ui.painter()
                    .rect_filled(rect, 5.0, p.accent.gamma_multiply(0.18));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 5.0, p.hover);
            }
            let sw = Rect::from_center_size(
                Pos2::new(
                    rect.left() + 20.0 + theme::LIST_SWATCH / 2.0,
                    rect.center().y,
                ),
                Vec2::splat(theme::LIST_SWATCH),
            );
            let swr = ui.interact(
                sw,
                ui.make_persistent_id(("block-swatch", k)),
                Sense::click(),
            );
            let painter = ui.painter();
            painter.text(
                Pos2::new(rect.left() + 4.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!("{}", k + 1),
                egui::FontId::proportional(11.0),
                p.secondary,
            );
            painter.rect_filled(sw, 3.0, crate::canvas::color32(blk.color));
            painter.rect_stroke(
                sw,
                3.0,
                Stroke::new(
                    if swr.hovered() { 1.5 } else { 1.0 },
                    if swr.hovered() {
                        p.text
                    } else {
                        Color32::from_black_alpha(40)
                    },
                ),
                egui::StrokeKind::Inside,
            );
            let (_, name, number) = crate::threads::nearest(self.view_tools.brand, blk.color);
            let name = crate::threads::label(name, number);
            let label = if stop {
                format!("{name} · stop")
            } else {
                name
            };
            painter.text(
                Pos2::new(sw.right() + theme::space::S, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &label,
                egui::FontId::proportional(12.0),
                if on { p.accent } else { p.text },
            );
            painter.text(
                Pos2::new(rect.right() - 4.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                fmt_int(n),
                egui::FontId::proportional(11.0),
                p.secondary,
            );
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{} {label}", k + 1))
            });
            let names: Vec<String> = objects
                .iter()
                .filter_map(|&i| self.names.get(i).cloned())
                .collect();
            if swr.on_hover_text("Change this block's thread").clicked() {
                pick = Some(objects.clone());
                let w = crate::threads::PICKER_W + 2.0 * theme::space::M;
                picker = Some(Pos2::new(
                    rect.left() - theme::space::M - theme::space::S - w,
                    rect.top() - theme::space::M,
                ));
            } else if resp
                .on_hover_text(format!("Sews: {}", names.join(", ")))
                .clicked()
            {
                pick = Some(objects);
            }
        }
        if let Some(v) = pick {
            self.set_selection(&v);
        }
        if let Some(at) = picker {
            self.picker = Some(at);
        }
    }

    fn document_body(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = theme::space::S;
        let fabric = self.doc.settings.fabric();
        form(ui, "doc-form", |fm: &mut Form| {
            fm.row("Fabric", |ui, lid| {
                egui::ComboBox::from_id_salt("fabric")
                    .selected_text(fabric.name)
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        for f in FABRICS {
                            ui.selectable_value(
                                &mut self.doc.settings.fabric,
                                f.id.to_string(),
                                f.name,
                            )
                            .on_hover_text(f.about);
                        }
                    })
                    .response
                    .labelled_by(lid)
                    .on_hover_text(fabric.about);
            });
            fm.row("Hoop", |ui, lid| {
                egui::ComboBox::from_id_salt("hoop")
                    .selected_text(HOOPS[self.doc.settings.hoop.min(HOOPS.len() - 1)].1)
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        for (k, h) in HOOPS.iter().enumerate() {
                            ui.selectable_value(&mut self.doc.settings.hoop, k, h.1);
                        }
                    })
                    .response
                    .labelled_by(lid);
            });
            fm.row("Layers", |ui, _| {
                ui.checkbox(&mut self.doc.settings.remove_overlaps, "Remove overlaps")
                    .on_hover_text(
                        "Don't fill areas that objects on top will cover (keeps 0.5 mm of overlap)",
                    );
            });
            fm.row("Order", |ui, _| {
                ui.checkbox(&mut self.doc.settings.hat_order, "Cap order")
                    .on_hover_text("Sew from the center out and the bottom up, grouping colors, so puckering is pushed away");
            });
            // Connect: gaps this short are sewn across in the open rather
            // than cut. A bar to drag, the cuts it saves beside it, and what
            // it sews lit up on the canvas meanwhile.
            let mut peek = false;
            fm.row("Connect", |ui, lid| {
                let mm = &mut self.doc.settings.connect_mm;
                ui.spacing_mut().slider_width = 100.0;
                let bar = ui
                    .add(
                        egui::Slider::new(mm, 0.0..=CONNECT_BAR_MM)
                            .show_value(false)
                            .step_by(0.1)
                            .clamping(egui::SliderClamping::Never),
                    )
                    .labelled_by(lid)
                    .on_hover_text(
                        "Sew across gaps up to this long with running stitches instead of cutting the thread, between objects or the parts of one. These stitches show: they light up on the canvas as you drag. (Travel that's out of sight, under a fill or what's sewn on top, is made anyway.)",
                    );
                bar.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Slider, true, "Connect gaps")
                });
                let field = ui.add(crate::units::drag_mm(mm, 0.0..=50.0, 0.1));
                peek = [&bar, &field]
                    .iter()
                    .any(|r| r.hovered() || r.dragged() || r.has_focus() || r.changed());
            });
            let (cuts, bridged) = (self.design.trim_count(), self.design.bridges.len());
            fm.wide(|ui| {
                caption(
                    ui,
                    connect_note(self.doc.settings.connect_mm, cuts, bridged),
                );
            });
            if peek {
                self.bridges_peek =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(6));
                fm.ui.ctx().request_repaint();
            }
            fm.row("Preview on", |ui, lid| {
                ui.color_edit_button_srgb(&mut self.doc.settings.cloth)
                    .labelled_by(lid)
                    .on_hover_text("Your hat's color, for the preview (saved with the design; doesn't change the stitches)");
            });
        });
    }

    // ------------------------------------------------------------ transform

    fn transform_body(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = theme::space::S;
        if self.sel_indices().is_empty() {
            caption(ui, "Nothing selected.");
            return;
        }
        self.layout_panel(ui);
    }
}

/// How far the Connect bar goes (mm; the field beside it goes further).
const CONNECT_BAR_MM: f64 = 20.0;

/// Under the Connect bar: the cuts there are, and how many it saved.
pub(crate) fn connect_note(connect_mm: f64, cuts: usize, bridged: usize) -> String {
    let n = |k: usize| format!("{k} cut{}", if k == 1 { "" } else { "s" });
    if bridged > 0 {
        // (In words: the app's font has no arrow.)
        format!("{} down to {cuts}", n(cuts + bridged))
    } else if connect_mm > 0.0 && cuts > 0 {
        format!("{}: none this short", n(cuts))
    } else if cuts > 0 {
        format!("{}: drag to sew across the short ones", n(cuts))
    } else {
        "No cuts".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn the_unit_is_remembered() {
        let f = std::env::temp_dir().join(format!("tl-units-{}.json", std::process::id()));
        let mut d = DockState::load_from(Some(f.clone()));
        assert_eq!(d.units, crate::units::Unit::Mm, "millimetres to start");
        d.units = crate::units::Unit::In;
        d.save();
        assert_eq!(
            DockState::load_from(Some(f.clone())).units,
            crate::units::Unit::In
        );
        d.reset_layout();
        assert_eq!(d.units, crate::units::Unit::In, "Reset Panels keeps it");
        let _ = std::fs::remove_file(f);
    }

    #[test]
    fn stitch_types_show_as_icons() {
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 1500.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(None, None),
            );
        h.run_steps(2);
        let blurb = kind_blurb(Kind::Satin);
        assert!(h.query_by_label(blurb).is_none(), "icons only");
        h.get_by_label("Satin"); // the icon, named for screen readers
        assert!(h.query_by_label_contains("Tip: name layers").is_none());
        assert!(h.query_by_label_contains("Select an object").is_none());
        // Panel menus no longer offer descriptions.
        h.get_by_label("Transform menu").click();
        h.run_steps(2);
        assert!(h.query_by_label("Show Descriptions").is_none());
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        // The menu also hides the panel.
        h.get_by_label("Threads menu").click();
        h.run_steps(2);
        h.get_by_label("Hide Panel").click();
        h.run_steps(2);
        assert!(!h.state().dock.visible(PanelId::Threads));
    }

    fn harness() -> Harness<'static, Studio> {
        let path =
            std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/badge.svg"));
        let mut h = Harness::builder()
            .with_size(Vec2::new(1400.0, 1000.0))
            .build_ui_state(
                |ui, s: &mut Studio| s.frame(ui),
                Studio::with_file(Some(path), None),
            );
        // As it starts outside tests: the Agent in the left dock.
        h.state_mut().dock.reveal(PanelId::Agent);
        h.run_steps(3);
        h
    }

    fn drag(h: &mut Harness<'_, Studio>, from: Pos2, to: Pos2) {
        h.hover_at(from);
        h.run_steps(1);
        h.drag_at(from);
        h.run_steps(1);
        for k in 1..=5 {
            h.hover_at(from + (to - from) * (k as f32 / 5.0));
            h.run_steps(1);
        }
        h.drop_at(to);
        h.run_steps(3);
    }

    #[test]
    fn docks_collapse_to_icons_with_flyouts() {
        let mut h = harness();
        // Right dock's » collapses it; there are two (one per dock).
        let n = h.query_all_by_label("Collapse to Icons").count();
        assert_eq!(n, 2);
        h.state_mut()
            .run_action(crate::commands::Action::IconicPanels);
        h.run_steps(2);
        assert!(h.state().dock.iconic[1]);
        assert!(h.query_by_label("Stitch panel").is_none(), "only icons now");
        h.get_by_label("Stitch").click();
        h.run_steps(2);
        assert_eq!(h.state().dock_ui.flyout, Some(PanelId::Stitch));
        h.get_by_label("Stitch panel"); // flown out beside the strip
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert_eq!(h.state().dock_ui.flyout, None);
        h.get_by_label("Expand Panels").click();
        h.run_steps(2);
        assert!(!h.state().dock.iconic[1]);
    }

    #[test]
    fn panels_drag_between_docks_and_out_to_float() {
        let mut h = harness();
        // Threads, dragged by its header onto the canvas, floats.
        let header = h.get_by_label("Threads panel").rect();
        let canvas = h.state().canvas_rect.center();
        drag(&mut h, header.center(), canvas);
        assert_eq!(
            h.state().dock.place_of(PanelId::Threads),
            Place::Float,
            "floated"
        );
        h.get_by_label("Threads panel");
        // Document, dragged into the left dock above the Agent.
        h.state_mut().dock.reveal(PanelId::Agent);
        h.run_steps(3);
        let header = h.get_by_label("Document panel").rect();
        let agent = h.get_by_label("Agent panel").rect();
        // (Its top edge: on the header itself it would join the Agent as a tab.)
        drag(
            &mut h,
            header.center(),
            agent.left_top() + Vec2::new(60.0, 3.0),
        );
        assert_eq!(h.state().dock.place_of(PanelId::Document), Place::Left);
        let left = h.state().panels_in(Place::Left);
        let d = left.iter().position(|&p| p == PanelId::Document).unwrap();
        let i = left.iter().position(|&p| p == PanelId::Agent).unwrap();
        assert!(d < i, "{left:?}");
        // The Agent stays at the bottom, the Document above it.
        h.run_steps(30);
        let doc = h.get_by_label("Document panel").rect();
        let agent = h.get_by_label("Agent panel").rect();
        assert!(doc.top() < agent.top(), "{doc:?} {agent:?}");
        // The floating one docks again from its menu.
        h.get_by_label("Threads menu").click();
        h.run_steps(2);
        h.get_by_label("Dock Right").click();
        h.run_steps(2);
        assert_eq!(h.state().dock.place_of(PanelId::Threads), Place::Right);
        // Reset puts everything back.
        h.state_mut()
            .run_action(crate::commands::Action::ResetPanels);
        h.run_steps(2);
        assert_eq!(h.state().dock.place_of(PanelId::Document), Place::Right);
    }

    #[test]
    fn panels_combine_into_tabs_and_come_apart() {
        let mut h = harness();
        // Threads dropped on the Stitch panel's header joins it as a tab.
        let threads = h.get_by_label("Threads panel").rect();
        let stitch = h.get_by_label("Stitch panel").rect();
        drag(
            &mut h,
            threads.center(),
            stitch.center() + Vec2::new(0.0, 4.0),
        );
        assert_eq!(h.state().dock.lead_of(PanelId::Threads), PanelId::Stitch);
        assert_eq!(
            h.state().dock.active[PanelId::Stitch.index()],
            Some(PanelId::Threads)
        );
        assert!(
            !h.state()
                .panels_in(Place::Right)
                .contains(&PanelId::Threads)
        );
        // Two tabs, Threads in front; click Stitch's tab to switch.
        assert!(h.query_by_label("Threads tab").is_some());
        h.get_by_label("Stitch tab").click();
        h.run_steps(2);
        assert_eq!(
            h.state().dock.active[PanelId::Stitch.index()],
            Some(PanelId::Stitch)
        );
        // Dragging the group's header moves both; to the left dock.
        let header = h.get_by_label("Stitch panel").rect();
        let agent = h.get_by_label("Agent panel").rect();
        drag(
            &mut h,
            header.right_center() - Vec2::new(60.0, 0.0),
            agent.left_top() + Vec2::new(60.0, 3.0),
        );
        assert_eq!(h.state().dock.place_of(PanelId::Stitch), Place::Left);
        assert_eq!(h.state().dock.place_of(PanelId::Threads), Place::Left);
        // Dragging a tab out takes just that panel, here onto the canvas.
        let tab = h.get_by_label("Threads tab").rect();
        let canvas = h.state().canvas_rect.center();
        drag(&mut h, tab.center(), canvas);
        assert_eq!(h.state().dock.lead_of(PanelId::Threads), PanelId::Threads);
        assert_eq!(h.state().dock.place_of(PanelId::Threads), Place::Float);
        assert_eq!(h.state().dock.place_of(PanelId::Stitch), Place::Left);
        assert!(
            h.query_by_label("Stitch tab").is_none(),
            "a group of one is a plain panel"
        );
    }

    #[test]
    fn issues_open_from_the_status_bar() {
        let mut h = harness();
        let id = egui::Id::unique("issues-popover");
        assert!(!egui::Popup::is_id_open(&h.ctx, id));
        h.get_by_label("Issues").click();
        h.run_steps(2);
        assert!(egui::Popup::is_id_open(&h.ctx, id));
        assert!(h.query_by_label("No problems found.").is_some());
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        // A problem shows in the list, and Window > Issues opens it.
        h.state_mut().run_tool(
            "set_object",
            &serde_json::json!({ "object": "sun", "kind": "satin" }),
        );
        h.run_steps(3);
        assert!(!h.state().issues.is_empty());
        egui::Popup::close_id(&h.ctx, id);
        h.state_mut()
            .run_action(crate::commands::Action::ShowIssues);
        h.run_steps(3);
        assert!(egui::Popup::is_id_open(&h.ctx, id));
        assert!(h.query_by_label("No problems found.").is_none());
    }

    #[test]
    fn a_layout_from_before_the_agent_panel_moves_objects_right() {
        let file =
            std::env::temp_dir().join(format!("zigzag-old-panels-{}.json", std::process::id()));
        std::fs::write(
            &file,
            r#"{"left":true,"right":true,"panels":{"Objects":{"place":"left"},"Issues":{"place":"left"},"Stitch":{"place":"right"}},"order":["Objects","Issues","Stitch","Transform"]}"#,
        )
        .unwrap();
        let d = DockState::load_from(Some(file.clone()));
        assert_eq!(d.place_of(PanelId::Objects), Place::Right);
        assert_eq!(d.order[0], PanelId::Objects);
        assert_eq!(d.place_of(PanelId::Agent), Place::Left);
        assert!(d.visible(PanelId::Agent) || cfg!(test));
        // Saved again, it's a layout with the Agent: left as it is.
        let mut d = d;
        d.move_panel(PanelId::Objects, Place::Left, None);
        d.save();
        let e = DockState::load_from(Some(file.clone()));
        assert_eq!(e.place_of(PanelId::Objects), Place::Left);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn panel_layout_is_remembered() {
        let file = std::env::temp_dir().join(format!("zigzag-panels-{}.json", std::process::id()));
        let mut d = DockState::load_from(Some(file.clone()));
        assert!(d.visible(PanelId::Lettering) && d.left);
        d.toggle(PanelId::Lettering);
        d.collapsed[PanelId::Document.index()] = true;
        d.left = false;
        d.move_panel(PanelId::Threads, Place::Float, None);
        d.float_at[PanelId::Threads.index()] = Pos2::new(500.0, 200.0);
        d.move_panel(PanelId::Document, Place::Left, Some(PanelId::Objects));
        d.iconic = [false, true];
        d.left = false;
        d.join_tab(PanelId::Transform, PanelId::Stitch);
        d.save();
        let e = DockState::load_from(Some(file.clone()));
        assert_eq!(e.place_of(PanelId::Threads), Place::Float);
        assert_eq!(
            e.float_at[PanelId::Threads.index()],
            Pos2::new(500.0, 200.0)
        );
        assert_eq!(e.place_of(PanelId::Document), Place::Left);
        let at = |p: PanelId| e.order.iter().position(|&q| q == p).unwrap();
        assert!(
            at(PanelId::Document) < at(PanelId::Objects),
            "{:?}",
            e.order
        );
        assert_eq!(e.iconic, [false, true]);
        assert!(!e.visible(PanelId::Lettering));
        assert!(e.collapsed(PanelId::Document));
        assert!(!e.left && e.right);
        assert_eq!(
            e.lead_of(PanelId::Transform),
            PanelId::Stitch,
            "tab groups too"
        );
        assert_eq!(e.active[PanelId::Stitch.index()], Some(PanelId::Transform));
        let _ = std::fs::remove_file(file);
    }
}
