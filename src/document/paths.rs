//! Point-by-point path editing: read an object's outline as Bézier anchors
//! (design units) and write edited anchors back into its SVG element, keeping
//! its id, paint and transform. Circles, rects and other basic shapes become
//! `<path>` elements the first time they're edited, as in Illustrator.

use super::{Document, xml_escape};
use crate::bezier::{self, Anchor, BezPath, LiveCorners, SubPath};
use crate::boolean::{self, PathOp};
use crate::geom::{Affine, FillRule, IDENTITY, P, Polyline, affine_inv, affine_mul};
use crate::params::Kind;
use crate::svg::{self, PX_TO_UNITS};
use crate::svg::{Geometry, Part};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;

/// Geometry attributes of basic shapes, dropped when they become paths.
const SHAPE_GEOMETRY: [&str; 11] = [
    "x", "y", "width", "height", "rx", "ry", "cx", "cy", "r", "points", "d",
];
const LINE_GEOMETRY: [&str; 4] = ["x1", "y1", "x2", "y2"];
/// Where a path with rounded corners keeps them live (see
/// [`Document::live_corners`]): `radii;frame;outline`. The outline is the
/// path as it was before any corner was cut, as path data in the element's
/// own coordinates, and there's a radius for each of its anchors. The frame
/// is empty for an element drawn as it is, moved, turned or scaled evenly.
/// One that's stretched (its transform scales unevenly) has four numbers
/// there, `a b c d`: the matrix that takes its coordinates to a frame where
/// what's drawn round is round, in which its corners are cut and the radii
/// measured.
const CORNERS: &str = "data-tl-corners";

/// Whether `m` keeps circles round (moves, turns, flips and scales evenly):
/// only then is a corner cut round on one side of it round on the other.
/// With its scale.
fn even_scale(m: Affine) -> Option<f64> {
    let (x, y) = (m[0] * m[0] + m[1] * m[1], m[2] * m[2] + m[3] * m[3]);
    let dot = m[0] * m[2] + m[1] * m[3];
    let size = x.max(y);
    (size > 0.0 && (x - y).abs() <= 1e-5 * size && dot.abs() <= 1e-5 * size).then(|| x.sqrt())
}

/// Live corners as an element keeps them: the uncut outline in its own
/// coordinates, the radii, and the frame they're cut in.
struct Kept {
    live: LiveCorners,
    frame: Affine,
}

impl Kept {
    /// The path they draw, in the element's coordinates.
    fn path(&self) -> Option<BezPath> {
        let back = affine_inv(self.frame)?;
        let framed = LiveCorners {
            base: bezier::transform(&self.live.base, self.frame),
            radii: self.live.radii.clone(),
        };
        Some(bezier::transform(&framed.path(), back))
    }

    /// The attribute's text.
    fn text(&self) -> String {
        let mut text = String::new();
        for r in self.live.radii.iter().flatten() {
            if !text.is_empty() {
                text.push(' ');
            }
            bezier::write_num(&mut text, *r);
        }
        text.push(';');
        if self.frame != IDENTITY {
            let f = self.frame;
            text.push_str(&format!("{:.9} {:.9} {:.9} {:.9}", f[0], f[1], f[2], f[3]));
        }
        text.push(';');
        text.push_str(&bezier::to_svg_d(&self.live.base));
        text
    }

    /// What an attribute's text says.
    fn read(text: &str) -> Option<Kept> {
        let mut parts = text.splitn(3, ';');
        let (radii, frame, outline) = (parts.next()?, parts.next()?, parts.next()?);
        let base = bezier::from_svg_d(outline);
        let mut flat = radii
            .split_ascii_whitespace()
            .map(|r| r.parse::<f64>().ok());
        let mut radii = Vec::with_capacity(base.len());
        for sp in &base {
            let mut of = Vec::with_capacity(sp.anchors.len());
            for _ in &sp.anchors {
                of.push(flat.next()??.max(0.0));
            }
            radii.push(of);
        }
        if flat.next().is_some() || base.is_empty() {
            return None;
        }
        let f: Vec<f64> = frame
            .split_ascii_whitespace()
            .map(|v| v.parse::<f64>())
            .collect::<Result<_, _>>()
            .ok()?;
        let frame = match f[..] {
            [] => IDENTITY,
            [a, b, c, d] if f.iter().all(|v| v.is_finite()) => [a, b, c, d, 0.0, 0.0],
            _ => return None,
        };
        affine_inv(frame)?;
        Some(Kept {
            live: LiveCorners { base, radii },
            frame,
        })
    }

    /// Live corners `live` (design units) as the element whose coordinates
    /// `m` takes to the design keeps them: the attribute's text and the
    /// path data they draw. Both come from the text as it will be read
    /// back, to its last decimal, so that reading it gives exactly this
    /// path again: that's how a path edited behind its corners' back is
    /// told from one that still is what they say.
    fn write(live: &LiveCorners, m: Affine) -> Option<(String, String)> {
        let inv = affine_inv(m)?;
        // Round is round in the element's own coordinates, or in those
        // turned to the design's (as big, by area).
        let frame = if even_scale(m).is_some() {
            IDENTITY
        } else {
            let by = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
            [m[0] / by, m[1] / by, m[2] / by, m[3] / by, 0.0, 0.0]
        };
        let rough = Kept {
            live: LiveCorners {
                base: bezier::transform(&live.base, inv),
                radii: live.radii.clone(),
            },
            frame,
        };
        let mut kept = Kept::read(&rough.text())?;
        // (Written to three decimals, a point may land on its neighbour.)
        let same = kept.live.base.len() == live.base.len()
            && kept
                .live
                .base
                .iter()
                .zip(&live.base)
                .all(|(a, b)| a.anchors.len() == b.anchors.len() && a.closed == b.closed);
        if !same {
            return None;
        }
        let scale = even_scale(affine_mul(m, affine_inv(kept.frame)?))?;
        for r in kept.live.radii.iter_mut().flatten() {
            *r /= scale;
        }
        // (The radii as they'll be read.)
        let kept = Kept::read(&kept.text())?;
        Some((bezier::to_svg_d(&kept.path()?), kept.text()))
    }
}

impl Document {
    /// usvg's absolute transform (element px → canvas px), then to design units.
    fn to_design(&self, t: usvg::Transform) -> Affine {
        let k = PX_TO_UNITS;
        affine_mul(
            [k, 0.0, 0.0, k, -self.offset.x, -self.offset.y],
            [
                t.sx as f64,
                t.ky as f64,
                t.kx as f64,
                t.sy as f64,
                t.tx as f64,
                t.ty as f64,
            ],
        )
    }

    /// The element behind shape `index`, as usvg sees it.
    fn usvg_path(&self, tree: &usvg::Tree, index: usize) -> Result<(BezPath, Affine)> {
        let s = self
            .shapes
            .get(index)
            .ok_or_else(|| anyhow!("no object {index}"))?;
        let node = tree
            .node_by_id(&s.id)
            .ok_or_else(|| anyhow!("{} isn't drawn", s.id))?;
        let usvg::Node::Path(p) = node else {
            bail!("lettering is edited as text, not point by point");
        };
        let m = self.to_design(p.abs_transform());
        Ok((bezier::from_usvg(p.data(), m), m))
    }

    /// Shape `index`'s outline as editable anchors (design units). Text's
    /// is its glyph outlines; editing those (`set_paths`) turns the text
    /// into plain paths first, so text is a shape like any other.
    pub fn edit_path(&self, index: usize) -> Result<BezPath> {
        if self.lettering(index).is_some() {
            return Ok(self.glyph_path(index));
        }
        let tree = self.parsed()?;
        Ok(self.usvg_path(&tree, index)?.0)
    }

    /// Text's glyph outlines as a path (as drawn, corners kept).
    fn glyph_path(&self, index: usize) -> BezPath {
        match &self.shapes[index].geometry {
            Geometry::Fill { rings, .. } => bezier::from_rings(rings, 0.0),
            Geometry::Stroke { .. } => Vec::new(),
        }
    }

    /// Any text among `indices` becomes plain paths (so a point-level edit
    /// can go ahead). Returns the indices to use afterwards (text's index
    /// may change), in the same order.
    pub fn outlines_first(&mut self, indices: &[usize]) -> Result<Vec<usize>> {
        if !indices.iter().any(|&i| self.lettering(i).is_some()) {
            return Ok(indices.to_vec());
        }
        let uids: Vec<usize> = indices
            .iter()
            .filter_map(|&i| self.shapes.get(i).map(|s| s.uid))
            .collect();
        let mut out = Vec::with_capacity(uids.len());
        for uid in uids {
            let Some(i) = self.shapes.iter().position(|s| s.uid == uid) else {
                continue;
            };
            if self.lettering(i).is_some() {
                let j = self.expand_lettering(i)?;
                out.push(self.shapes[j].uid);
            } else {
                out.push(uid);
            }
        }
        Ok(out
            .into_iter()
            .filter_map(|uid| self.shapes.iter().position(|s| s.uid == uid))
            .collect())
    }

    /// The SVG as usvg reads it: kept from the last import when it's current.
    pub(crate) fn parsed(&self) -> Result<std::sync::Arc<usvg::Tree>> {
        match &self.tree {
            Some(t) => Ok(t.clone()),
            None => Ok(std::sync::Arc::new(svg::parse_tree(self.xml.as_bytes())?)),
        }
    }

    /// Every object's outline as editable anchors (design units), reading the
    /// SVG once: `None` for lettering and anything not drawn as a path.
    pub fn all_edit_paths(&self) -> Vec<Option<BezPath>> {
        let Ok(tree) = self.parsed() else {
            return vec![None; self.shapes.len()];
        };
        (0..self.shapes.len())
            .map(|i| {
                if self.lettering(i).is_some() {
                    return Some(self.glyph_path(i));
                }
                self.usvg_path(&tree, i).ok().map(|(p, _)| p)
            })
            .collect()
    }

    /// Replace shape `index`'s outline with `path` (design units). An empty
    /// path is refused; delete the object instead.
    pub fn set_path(&mut self, index: usize, path: &BezPath) -> Result<()> {
        self.set_paths(&[(index, path.clone())])
    }

    /// [`Document::set_path`] for several objects at once: the SVG is read
    /// once, every outline written, and the result read back in once.
    pub fn set_paths(&mut self, edits: &[(usize, BezPath)]) -> Result<()> {
        self.write_paths(edits, None)
    }

    /// Shape `index`'s corners as they can be rounded (design units): the
    /// outline with no corner cut, and each corner's radius. A path whose
    /// corners were rounded by their handles keeps both with its element,
    /// and gives them back for as long as its outline is still what they
    /// draw; any other path is its own outline, no corner rounded. Not for
    /// text.
    pub fn live_corners(&self, index: usize) -> Result<LiveCorners> {
        if self.lettering(index).is_some() {
            bail!("lettering is edited as text, not point by point");
        }
        let tree = self.parsed()?;
        let (path, m) = self.usvg_path(&tree, index)?;
        Ok(self
            .kept_corners(index, m)
            .unwrap_or_else(|| LiveCorners::plain(path)))
    }

    /// The live corners kept with shape `index`'s element, if it has any
    /// and they still draw its path.
    fn kept_corners(&self, index: usize, m: Affine) -> Option<LiveCorners> {
        // (Most documents have none: no need to read the XML to find out.)
        if !self.xml.contains(CORNERS) {
            return None;
        }
        let id = &self.shapes.get(index)?.id;
        let doc = roxmltree::Document::parse(&self.xml).ok()?;
        let n = *super::elements_by_id(&doc).get(id.as_str())?;
        let kept = Kept::read(n.attribute(CORNERS)?)?;
        if n.tag_name().name() != "path" || n.attribute("d")? != bezier::to_svg_d(&kept.path()?) {
            return None;
        }
        // Still round as drawn (the element hasn't been stretched since)?
        let scale = even_scale(affine_mul(m, affine_inv(kept.frame)?))?;
        Some(LiveCorners {
            base: bezier::transform(&kept.live.base, m),
            radii: kept
                .live
                .radii
                .iter()
                .map(|of| of.iter().map(|r| r * scale).collect())
                .collect(),
        })
    }

    /// The paths among `indices` with live corners, as an uneven scale
    /// `m` (design units) leaves them: (a shape's uid, the outline
    /// stretched by `m` with its corners' radii scaled by as much as `m`
    /// scales areas, to cut again). Empty when `m` scales evenly: then the
    /// corners go with the element's transform as they are.
    pub(super) fn stretched_corners(
        &self,
        indices: &[usize],
        m: Affine,
    ) -> Vec<(usize, LiveCorners)> {
        if even_scale(m).is_some() || !self.xml.contains(CORNERS) {
            return Vec::new();
        }
        let Ok(tree) = self.parsed() else {
            return Vec::new();
        };
        let by = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
        let mut out: Vec<(usize, LiveCorners)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for &i in indices {
            let Some(s) = self.shapes.get(i) else {
                continue;
            };
            if s.id.is_empty() || !seen.insert(&s.id) {
                continue;
            }
            let Ok((_, own)) = self.usvg_path(&tree, i) else {
                continue;
            };
            if let Some(live) = self.kept_corners(i, own) {
                let moved = LiveCorners {
                    base: bezier::transform(&live.base, m),
                    radii: live
                        .radii
                        .iter()
                        .map(|of| of.iter().map(|r| r * by).collect())
                        .collect(),
                };
                out.push((s.uid, moved));
            }
        }
        out
    }

    /// Round shape `index`'s corners as `live` says (design units), keeping
    /// them live: the element keeps the uncut outline and the radii beside
    /// the path they draw. With no corner rounded it's a plain path again.
    pub fn set_live_corners(&mut self, index: usize, live: &LiveCorners) -> Result<()> {
        if live.is_plain() {
            return self.set_path(index, &live.base);
        }
        self.write_paths(&[(index, live.path())], Some(live))
    }

    /// Write outlines into their elements. `live`: the corners to keep
    /// with the (one) element written.
    fn write_paths(
        &mut self,
        edits: &[(usize, BezPath)],
        live: Option<&LiveCorners>,
    ) -> Result<()> {
        if edits.is_empty() {
            return Ok(());
        }
        // Text edited point by point becomes plain paths first.
        let idx: Vec<usize> = edits.iter().map(|(i, _)| *i).collect();
        let idx = self.outlines_first(&idx)?;
        let edits: Vec<(usize, BezPath)> = idx
            .into_iter()
            .zip(edits.iter().map(|(_, p)| p.clone()))
            .collect();
        let edits = &edits[..];
        let mut wanted = Vec::new();
        for (index, path) in edits {
            if path.is_empty() {
                bail!("a path needs at least two points");
            }
            let uid = self
                .shapes
                .get(*index)
                .ok_or_else(|| anyhow!("no object {index}"))?
                .uid;
            wanted.push((uid, path));
        }
        self.sync()?;
        let tree = self.parsed()?;
        let doc = roxmltree::Document::parse(&self.xml).context("parsing document")?;
        let by_id = super::elements_by_id(&doc);
        let mut writes: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        let mut done = std::collections::HashSet::new();
        for (uid, path) in wanted {
            let index = self
                .shapes
                .iter()
                .position(|s| s.uid == uid)
                .ok_or_else(|| anyhow!("the object went away"))?;
            let id = self.shapes[index].id.clone();
            // An element's fill and outline are one path: written once.
            if !done.insert(id.clone()) {
                continue;
            }
            let (_, m) = self.usvg_path(&tree, index)?;
            let inv = affine_inv(m).ok_or_else(|| anyhow!("the object is squashed flat"))?;
            // Live corners: the outline and the radii in the element's own
            // coordinates, and the path they draw there.
            let kept = live.and_then(|l| Kept::write(l, m));
            let (d, kept) = match kept {
                Some((d, text)) => (d, Some(text)),
                None => (bezier::to_svg_d(&bezier::transform(path, inv)), None),
            };
            let n = *by_id
                .get(id.as_str())
                .ok_or_else(|| anyhow!("element {id} not found"))?;
            let keep = |quote: char| match &kept {
                Some(text) => format!("{quote} {CORNERS}={quote}{}", xml_escape(text)),
                None => String::new(),
            };
            if n.tag_name().name() == "path" {
                // Corners kept from before are for the path as it was.
                if let Some(a) = n.attributes().find(|a| a.name() == CORNERS) {
                    let r = a.range();
                    let space = self.xml[..r.start].ends_with(|c: char| c.is_ascii_whitespace());
                    writes.push((r.start - usize::from(space)..r.end, String::new()));
                }
            }
            writes.push(if n.tag_name().name() == "path" {
                match n.attributes().find(|a| a.name() == "d") {
                    Some(a) => {
                        // (The new attribute goes in after the path data,
                        // which keeps its closing quote.)
                        let r = a.range_value();
                        let quote = self.xml[r.end..].chars().next().unwrap_or('"');
                        (r, format!("{}{}", xml_escape(&d), keep(quote)))
                    }
                    None => bail!("{id} has no path data"),
                }
            } else {
                // A basic shape: the same element as a path.
                let drop: &[&str] = if n.tag_name().name() == "line" {
                    &LINE_GEOMETRY
                } else {
                    &SHAPE_GEOMETRY
                };
                let mut el = String::from("<path");
                for a in n.attributes() {
                    if a.namespace().is_none() && (drop.contains(&a.name()) || a.name() == CORNERS)
                    {
                        continue;
                    }
                    el.push(' ');
                    el.push_str(&self.xml[a.range()]);
                }
                el.push_str(&format!(" d=\"{}{}\"/>", xml_escape(&d), keep('"')));
                (n.range(), el)
            });
        }
        drop(doc);
        let uids: HashMap<_, _> = self
            .shapes
            .iter()
            .map(|s| ((s.id.clone(), s.part), s.uid))
            .collect();
        // From the end back, so the earlier ranges still point right.
        writes.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
        for (range, text) in writes {
            self.xml.replace_range(range, &text);
        }
        self.import(&uids)
    }

    /// Show shape `index`'s outline as `path` (design units) without
    /// writing it: a cheap live preview while a corner is dragged. The
    /// element's shapes (a fill and its outline are one path) take the
    /// geometry [`Document::set_path`] would give them; the SVG is left
    /// alone, so whoever previews puts the geometry back, or sets the path,
    /// when the gesture ends. Not for text (its outline is its letters').
    pub fn preview_path(&mut self, index: usize, path: &BezPath) {
        let Some(id) = self.shapes.get(index).map(|s| s.id.clone()) else {
            return;
        };
        if self.lettering(index).is_some() {
            return;
        }
        let subpaths = svg::flatten_bez(path);
        for (i, s) in self.shapes.iter_mut().enumerate() {
            if i != index && (id.is_empty() || s.id != id) {
                continue;
            }
            match &mut s.geometry {
                Geometry::Fill { rings, .. } => {
                    *rings = subpaths
                        .iter()
                        .map(|(pts, _)| pts.clone())
                        .filter(|r| r.len() >= 3)
                        .collect();
                }
                Geometry::Stroke { lines, .. } => *lines = subpaths.clone(),
            }
        }
    }

    /// Add a drawn path (design units): closed paths become filled shapes,
    /// open ones strokes. Returns the new shape's index.
    pub fn add_drawing_path(
        &mut self,
        path: &BezPath,
        color: [u8; 3],
        kind: Kind,
    ) -> Result<usize> {
        self.add_drawing_stroke(path, color, kind, 0.5)
    }

    /// As [`Document::add_drawing_path`], an open path stroked `width_mm`
    /// wide (a satin column's width, say).
    pub fn add_drawing_stroke(
        &mut self,
        path: &BezPath,
        color: [u8; 3],
        kind: Kind,
        width_mm: f64,
    ) -> Result<usize> {
        let width_mm = if width_mm.is_finite() {
            width_mm.clamp(0.1, 20.0)
        } else {
            0.5
        };
        let closed = path.first().is_some_and(|sp| sp.closed);
        if path.is_empty() || path.iter().all(|sp| sp.anchors.len() < 2) {
            bail!("draw at least {} points", if closed { 3 } else { 2 });
        }
        self.sync()?;
        let m = self.mm_to_user()?;
        // Design units → mm, inside a group that maps mm to the document.
        let d = bezier::to_svg_d(&bezier::transform(path, [0.1, 0.0, 0.0, 0.1, 0.0, 0.0]));
        let hex = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
        let paint = if closed {
            format!("fill=\"{hex}\"")
        } else {
            format!(
                "fill=\"none\" stroke=\"{hex}\" stroke-width=\"{width_mm:.2}\" stroke-linejoin=\"round\" stroke-linecap=\"round\""
            )
        };
        self.next_group += 1;
        let n = self.next_group;
        let id = format!("drawing_{n}_{}", kind.name());
        let group = format!(
            "<g id=\"tl-drawing-{n}\" transform=\"{}\"><path id=\"{id}\" d=\"{d}\" {paint}/></g>\n",
            super::edit::fmt_matrix(m),
        );
        let close = self
            .xml
            .rfind("</svg>")
            .ok_or_else(|| anyhow!("document has no closing </svg>"))?;
        self.xml.insert_str(close, &group);
        let map = self.uid_map(&HashMap::new());
        self.import(&map)?;
        self.shapes
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| anyhow!("the drawing has no area or length"))
    }

    /// Add a drawing through points (design units), joined by straight lines.
    pub fn add_drawing(
        &mut self,
        pts: &[P],
        closed: bool,
        color: [u8; 3],
        kind: Kind,
    ) -> Result<usize> {
        if pts.len() < if closed { 3 } else { 2 } {
            bail!("draw at least {} points", if closed { 3 } else { 2 });
        }
        let path = vec![SubPath {
            anchors: pts.iter().map(|&p| Anchor::corner(p)).collect(),
            closed,
        }];
        self.add_drawing_path(&path, color, kind)
    }
}

/// A color for an outline that shows against a shape sewn in `c`: a darker
/// shade of it, or for a shape that's nearly black already, a lighter one.
fn outline_shade(c: [u8; 3]) -> [u8; 3] {
    if c.iter().all(|&v| v < 64) {
        c.map(|v| v + (255 - v) / 2)
    } else {
        c.map(|v| (v as f64 * 0.6).round() as u8)
    }
}

/// A length for an SVG attribute: up to four decimals, no trailing zeros.
fn fmt_len(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

/// How closely the curves fitted to path operations' results follow them
/// (design units: 0.06 mm, twice the 0.03 mm curves are flattened to).
pub(super) const RESULT_TOL: f64 = 0.6;

impl Document {
    /// The area an object covers: its fill, or its stroked line.
    pub(super) fn area(&self, i: usize) -> (Vec<Polyline>, FillRule) {
        match &self.shapes[i].geometry {
            Geometry::Fill { rings, rule } => (rings.clone(), *rule),
            Geometry::Stroke { lines, width } => (
                boolean::stroke_area(lines, (*width).max(1.0)),
                FillRule::NonZero,
            ),
        }
    }

    /// `indices`, and with each filled shape among them its outline (the
    /// stroke of the same element, sewn or not): what goes when the shape
    /// goes. An outline on its own stays one object.
    pub fn with_outlines(&self, indices: &[usize]) -> Vec<usize> {
        let mut out: Vec<usize> = indices.to_vec();
        for &i in indices {
            let Some(s) = self.shapes.get(i) else {
                continue;
            };
            if s.part != Part::Fill || s.id.is_empty() {
                continue;
            }
            if let Some(o) = self
                .shapes
                .iter()
                .position(|o| o.id == s.id && o.part == Part::Stroke)
                && !out.contains(&o)
            {
                out.push(o);
            }
        }
        out
    }

    pub(super) fn uid_indices(&self, uids: &[usize]) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| uids.contains(&s.uid))
            .map(|(i, _)| i)
            .collect();
        v.sort_unstable();
        v
    }

    /// Add a filled shape (design units) just behind the top-level element
    /// holding object `behind` (or at the end), with the given look. Returns
    /// its id; call `import` (or a caller that does) afterwards.
    pub(super) fn insert_shape(
        &mut self,
        path: &BezPath,
        behind: Option<usize>,
        name: &str,
    ) -> Result<String> {
        let m = self.mm_to_user()?;
        let d = bezier::to_svg_d(&bezier::transform(path, [0.1, 0.0, 0.0, 0.1, 0.0, 0.0]));
        let used: std::collections::HashSet<String> = {
            let doc = roxmltree::Document::parse(&self.xml).context("parsing document")?;
            doc.descendants()
                .filter_map(|n| n.attribute("id").map(str::to_string))
                .collect()
        };
        let mut k = 2;
        let mut id = format!("{name}_#{k}");
        while used.contains(&id) {
            k += 1;
            id = format!("{name}_#{k}");
        }
        self.next_group += 1;
        let el = format!(
            "<g id=\"tl-drawing-{}\" transform=\"{}\"><path id=\"{}\" d=\"{d}\" fill=\"#000\" fill-rule=\"evenodd\"/></g>\n",
            self.next_group,
            super::edit::fmt_matrix(m),
            xml_escape(&id)
        );
        let at = match behind.and_then(|i| self.shapes.get(i)) {
            Some(s) => {
                let doc = roxmltree::Document::parse(&self.xml).context("parsing document")?;
                let n = doc
                    .descendants()
                    .find(|n| n.is_element() && n.attribute("id") == Some(s.id.as_str()))
                    .ok_or_else(|| anyhow!("element {} not found", s.id))?;
                let top = n
                    .ancestors()
                    .filter(|a| a.is_element())
                    .find(|a| a.parent_element().is_some_and(|p| p == doc.root_element()))
                    .unwrap_or(n);
                top.range().start
            }
            None => self
                .xml
                .rfind("</svg>")
                .ok_or_else(|| anyhow!("document has no closing </svg>"))?,
        };
        self.xml.insert_str(at, &el);
        Ok(id)
    }

    /// After `insert_shape`: re-read, then give the new shape `from`'s look.
    fn finish_new(
        &mut self,
        id: &str,
        color: [u8; 3],
        params: crate::params::Params,
    ) -> Result<usize> {
        let map = self.uid_map(&HashMap::new());
        self.import(&map)?;
        let i = self
            .shapes
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| anyhow!("the result has no area"))?;
        self.shapes[i].color = color;
        self.shapes[i].params = params;
        Ok(i)
    }

    pub(super) fn name_of(&self, i: usize) -> String {
        let w = svg::name_words(&self.shapes[i].label);
        if w.is_empty() {
            "shape".into()
        } else {
            w.join("_")
        }
    }

    /// Fill settings for a shape made from object `i`.
    pub(super) fn fill_params(&self, i: usize) -> crate::params::Params {
        let mut p = self.shapes[i].params.clone();
        if !matches!(
            p.kind,
            Some(Kind::Fill | Kind::Contour | Kind::Applique | Kind::Satin)
        ) {
            p.kind = Some(Kind::Fill);
        }
        if self.shapes[i].part == Part::Stroke && p.kind == Some(Kind::Satin) {
            // A satin line outlined is a narrow shape: satin across it.
            p.width = None;
        }
        p
    }

    /// Illustrator's Pathfinder: combine objects (in sewing order, back to
    /// front) into one filled shape that replaces them. Returns its index.
    pub fn pathfinder(&mut self, indices: &[usize], op: PathOp) -> Result<usize> {
        let uids: Vec<usize> = indices
            .iter()
            .filter_map(|&i| self.shapes.get(i))
            .map(|s| s.uid)
            .collect();
        if uids.len() < 2 {
            bail!("select two or more overlapping objects");
        }
        let indices = self.outlines_first(indices)?;
        let uids: Vec<usize> = indices
            .iter()
            .filter_map(|&i| self.shapes.get(i))
            .map(|s| s.uid)
            .collect();
        self.sync()?;
        let idx = self.uid_indices(&uids);
        let parts: Vec<(Vec<Polyline>, FillRule)> = idx.iter().map(|&i| self.area(i)).collect();
        let rings = boolean::pathfinder(&parts, op);
        let path = bezier::from_rings(&rings, RESULT_TOL);
        if path.is_empty() {
            bail!(match op {
                PathOp::Intersect => "the objects don't all overlap",
                _ => "nothing would be left",
            });
        }
        let back = idx[0];
        let look = if op == PathOp::MinusFront {
            back
        } else {
            *idx.last().unwrap()
        };
        let (color, params, name) = (
            self.shapes[look].color,
            self.fill_params(look),
            self.name_of(look),
        );
        let id = self.insert_shape(&path, Some(back), &name)?;
        // (With their outlines: those were the old shapes' edges, not the
        // new one's.)
        let gone = self.with_outlines(&idx);
        self.delete_many(&gone)?;
        self.finish_new(&id, color, params)
    }

    /// Illustrator's Offset Path: a new filled shape `d` units outside each
    /// object (inside, if negative), behind it. Returns the new indices.
    pub fn offset_path(&mut self, indices: &[usize], d: f64) -> Result<Vec<usize>> {
        let uids: Vec<usize> = indices
            .iter()
            .filter_map(|&i| self.shapes.get(i))
            .map(|s| s.uid)
            .collect();
        self.sync()?;
        let mut made = Vec::new();
        for uid in uids {
            let Some(i) = self.shapes.iter().position(|s| s.uid == uid) else {
                continue;
            };
            let (rings, rule) = self.area(i);
            let path = bezier::from_rings(&boolean::offset(&rings, rule, d), RESULT_TOL);
            if path.is_empty() {
                continue;
            }
            let (color, params, name) =
                (self.shapes[i].color, self.fill_params(i), self.name_of(i));
            let id = self.insert_shape(&path, Some(i), &format!("{name}_offset"))?;
            let n = self.finish_new(&id, color, params)?;
            made.push(self.shapes[n].uid);
        }
        if made.is_empty() {
            bail!("nothing left after offsetting");
        }
        Ok(self.uid_indices(&made))
    }

    /// Illustrator's Outline Stroke: each stroked line becomes the filled
    /// shape it covers. Returns the new indices.
    pub fn outline_stroke(&mut self, indices: &[usize]) -> Result<Vec<usize>> {
        let uids: Vec<usize> = indices
            .iter()
            .filter_map(|&i| self.shapes.get(i))
            .filter(|s| matches!(s.geometry, Geometry::Stroke { .. }))
            .map(|s| s.uid)
            .collect();
        if uids.is_empty() {
            bail!("select a line (a stroke) to outline");
        }
        self.sync()?;
        let mut made = Vec::new();
        for uid in uids {
            let Some(i) = self.shapes.iter().position(|s| s.uid == uid) else {
                continue;
            };
            let (rings, _) = self.area(i);
            let path = bezier::from_rings(
                &boolean::pathfinder(&[(rings, FillRule::NonZero)], PathOp::Unite),
                RESULT_TOL,
            );
            if path.is_empty() {
                continue;
            }
            let (color, params, name) =
                (self.shapes[i].color, self.fill_params(i), self.name_of(i));
            let id = self.insert_shape(&path, Some(i), &name)?;
            let i = self.shapes.iter().position(|s| s.uid == uid).unwrap();
            self.delete(i)?;
            let n = self.finish_new(&id, color, params)?;
            made.push(self.shapes[n].uid);
        }
        Ok(self.uid_indices(&made))
    }

    /// Object › Path › Add Outline: give each filled shape a stroke along
    /// its edge, sewn as `kind` (a running stitch or satin), `width_mm`
    /// wide (as wide as the satin is; a running stitch is a line, and
    /// keeps the width for when it's made satin), in `color` (or a shade
    /// of the shape's own thread that shows against it). The stroke goes on the shape's own
    /// element, so the outline follows the shape when it's moved or
    /// reshaped, and is an object of its own to sew: its own thread, stitch
    /// type and place in the order (it starts right after its shape). Lines,
    /// text and shapes that have an outline already are left alone. Returns
    /// the outlines' indices.
    pub fn add_outline(
        &mut self,
        indices: &[usize],
        kind: Kind,
        width_mm: f64,
        color: Option<[u8; 3]>,
    ) -> Result<Vec<usize>> {
        if !matches!(kind, Kind::Run | Kind::Satin) {
            bail!("an outline is a running stitch or satin");
        }
        if !(width_mm.is_finite() && width_mm > 0.0) {
            bail!("an outline needs a width");
        }
        // All or nothing: a refusal part-way leaves the document as it was.
        let mut next = self.clone();
        let made = next.outline_shapes(indices, kind, width_mm.clamp(0.1, 20.0), color)?;
        *self = next;
        Ok(made)
    }

    /// [`Document::add_outline`], on a copy of the document to keep or drop.
    fn outline_shapes(
        &mut self,
        indices: &[usize],
        kind: Kind,
        width_mm: f64,
        color: Option<[u8; 3]>,
    ) -> Result<Vec<usize>> {
        let uids: Vec<usize> = indices
            .iter()
            .filter_map(|&i| self.shapes.get(i))
            .map(|s| s.uid)
            .collect();
        self.sync()?;
        let tree = self.parsed()?;
        // Why nothing was outlined, if nothing is.
        let mut why = "select a filled shape to outline";
        // (element id, the fill's uid, the outline's color)
        let mut wanted: Vec<(String, usize, [u8; 3])> = Vec::new();
        let mut writes: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        {
            let doc = roxmltree::Document::parse(&self.xml).context("parsing document")?;
            let by_id = super::elements_by_id(&doc);
            // What `<use>` copies are drawn from: a stroke on one of those
            // would be on every copy, which have no settings of their own.
            let copied: std::collections::HashSet<&str> = doc
                .descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "use")
                .flat_map(|n| n.attributes())
                .filter(|a| a.name() == "href")
                .filter_map(|a| a.value().strip_prefix('#'))
                .collect();
            for i in self.uid_indices(&uids) {
                let s = &self.shapes[i];
                if s.id.is_empty() || wanted.iter().any(|(id, ..)| *id == s.id) {
                    continue;
                }
                if !matches!(s.geometry, Geometry::Fill { .. }) || s.part != Part::Fill {
                    why = "a line is a stroke already; select a filled shape";
                    continue;
                }
                if self.lettering(i).is_some() {
                    why = "turn the text into shapes first (Type › Create Outlines)";
                    continue;
                }
                // (One that's hidden or switched off is made anew.)
                let outlined = self.shapes.iter().any(|o| {
                    o.id == s.id
                        && o.part == Part::Stroke
                        && o.params.kind != Some(Kind::Skip)
                        && !o.params.hidden()
                });
                if outlined {
                    why = "that shape has an outline already";
                    continue;
                }
                let Some(&n) = by_id.get(s.id.as_str()) else {
                    continue;
                };
                if n.ancestors().any(|a| {
                    a.is_element()
                        && (a.attribute("id").is_some_and(|id| copied.contains(id))
                            || super::NON_RENDERED.contains(&a.tag_name().name()))
                }) {
                    why = "that shape has copies drawn from it elsewhere in the artwork, which an outline would be on too";
                    continue;
                }
                // (Its own element, of a kind that can be stroked: not a
                // copy drawn by a `<use>`, whose look is the original's.)
                let Ok((_, m)) = self.usvg_path(&tree, i) else {
                    why = "that object can't take an outline of its own";
                    continue;
                };
                // The element's own units to design units (0.1 mm).
                let scale = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
                if !(scale.is_finite() && scale > 1e-9) || width_mm * 10.0 / scale < 1e-4 {
                    why = "that shape is squashed flat";
                    continue;
                }
                let color = color.unwrap_or_else(|| outline_shade(s.color));
                // (Opaque and unbroken, whatever the artwork said of a
                // stroke it didn't show.)
                let stroke = format!(
                    "stroke:#{:02x}{:02x}{:02x};stroke-width:{};stroke-linejoin:round;stroke-opacity:1;stroke-dasharray:none",
                    color[0],
                    color[1],
                    color[2],
                    fmt_len(width_mm * 10.0 / scale)
                );
                let Some(write) = super::stroke_style_write(n, &stroke) else {
                    why = "that object can't take an outline of its own";
                    continue;
                };
                writes.push(write);
                wanted.push((s.id.clone(), s.uid, color));
            }
        }
        if wanted.is_empty() {
            bail!(why);
        }
        // An outline deleted earlier comes back.
        self.removed_parts
            .retain(|(id, part)| !(*part == Part::Stroke && wanted.iter().any(|w| w.0 == *id)));
        let old = self.uid_map(&HashMap::new());
        writes.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
        for (range, text) in writes {
            self.xml.replace_range(range, &text);
        }
        self.import(&old)?;
        let mut made = Vec::new();
        for (id, fill_uid, color) in wanted {
            let Some(at) = self
                .shapes
                .iter()
                .position(|s| s.id == id && s.part == Part::Stroke)
            else {
                bail!("{id} can't take an outline");
            };
            // Its own settings, whatever the shape's are.
            let mut outline = self.shapes.remove(at);
            outline.params = crate::params::Params {
                kind: Some(kind),
                ..Default::default()
            };
            outline.color = color;
            if let Geometry::Stroke { width, .. } = &mut outline.geometry {
                *width = width_mm * 10.0;
            }
            // Sewn right after its shape (which may not be where the
            // document has it, if the order was changed).
            let after = self
                .shapes
                .iter()
                .position(|s| s.uid == fill_uid)
                .map_or(self.shapes.len(), |f| f + 1);
            made.push(outline.uid);
            self.shapes.insert(after, outline);
        }
        self.rev = super::next_rev();
        Ok(self.uid_indices(&made))
    }

    /// Illustrator's Release Compound Path: a `<path>` made of several
    /// separate shapes becomes a group of one `<path>` per shape, each with
    /// the original's paint, transform and stitch settings, so each can be
    /// set up on its own. Holes stay with the shape they cut into (a ring
    /// stays a ring). Returns the new objects' indices and the new groups'
    /// ids.
    pub fn release_compound(&mut self, indices: &[usize]) -> Result<(Vec<usize>, Vec<String>)> {
        self.sync()?;
        let mut ids: Vec<String> = Vec::new();
        for &i in indices {
            let Some(s) = self.shapes.get(i) else {
                continue;
            };
            if self.lettering(i).is_some() || ids.contains(&s.id) {
                continue;
            }
            ids.push(s.id.clone());
        }
        let tree = self.parsed()?;
        let doc = roxmltree::Document::parse(&self.xml).context("parsing document")?;
        let by_id = super::elements_by_id(&doc);
        let mut used: std::collections::HashSet<String> = doc
            .descendants()
            .filter_map(|n| n.attribute("id").map(str::to_string))
            .collect();
        let mut unique = |base: &str| {
            let mut k = 1;
            let mut id = format!("{base}_#{k}");
            while used.contains(&id) {
                k += 1;
                id = format!("{base}_#{k}");
            }
            used.insert(id.clone());
            id
        };
        let mut writes: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        // New element id -> the look it inherits, per part.
        let mut looks: Vec<(String, Part, [u8; 3], crate::params::Params)> = Vec::new();
        let mut groups = Vec::new();
        for id in &ids {
            let Some(i) = self.shapes.iter().position(|s| &s.id == id) else {
                continue;
            };
            let Ok((path, m)) = self.usvg_path(&tree, i) else {
                continue;
            };
            let pieces = split_pieces(&path);
            if pieces.len() < 2 {
                continue;
            }
            let Some(inv) = affine_inv(m) else { continue };
            let Some(&n) = by_id.get(id.as_str()) else {
                continue;
            };
            if n.tag_name().name() != "path" {
                continue;
            }
            // Everything but the id and the path data goes onto each piece.
            let mut attrs = String::new();
            for a in n.attributes() {
                if a.namespace().is_none() && (a.name() == "id" || a.name() == "d") {
                    continue;
                }
                attrs.push(' ');
                attrs.push_str(&self.xml[a.range()]);
            }
            let name = self.name_of(i);
            let gid = unique(&name);
            let mut el = format!("<g id=\"{}\">", xml_escape(&gid));
            // Numbered, so the pieces read apart in the Objects list.
            for (k, piece) in pieces.iter().enumerate() {
                let pid = unique(&format!("{name}_{}", k + 1));
                let d = bezier::to_svg_d(&bezier::transform(piece, inv));
                el.push_str(&format!(
                    "<path id=\"{}\"{attrs} d=\"{}\"/>",
                    xml_escape(&pid),
                    xml_escape(&d)
                ));
                for s in self.shapes.iter().filter(|s| &s.id == id) {
                    looks.push((pid.clone(), s.part, s.color, s.params.clone()));
                }
            }
            el.push_str("</g>");
            writes.push((n.range(), el));
            groups.push(gid);
        }
        drop(doc);
        if writes.is_empty() {
            bail!("select a path made of separate shapes (a compound path)");
        }
        let map = self.uid_map(&HashMap::new());
        writes.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
        for (range, text) in writes {
            self.xml.replace_range(range, &text);
        }
        self.import(&map)?;
        let mut made = Vec::new();
        for (pid, part, color, params) in looks {
            if let Some(s) = self
                .shapes
                .iter_mut()
                .find(|s| s.id == pid && s.part == part)
            {
                s.color = color;
                s.params = params;
                made.push(s.uid);
            }
        }
        Ok((self.uid_indices(&made), groups))
    }

    /// Illustrator's Simplify: fewer points, smooth curves, within `tol`
    /// units of the original outline.
    pub fn simplify_paths(&mut self, indices: &[usize], tol: f64) -> Result<usize> {
        let uids: Vec<usize> = indices
            .iter()
            .filter_map(|&i| self.shapes.get(i))
            .map(|s| s.uid)
            .collect();
        let mut removed = 0;
        let mut seen = std::collections::HashSet::new();
        let mut edits = Vec::new();
        for uid in uids {
            let Some(i) = self.shapes.iter().position(|s| s.uid == uid) else {
                continue;
            };
            if !seen.insert(self.shapes[i].id.clone()) {
                continue;
            }
            let path = self.edit_path(i)?;
            let before: usize = path.iter().map(|sp| sp.anchors.len()).sum();
            let simple = bezier::simplify(&path, tol);
            let after: usize = simple.iter().map(|sp| sp.anchors.len()).sum();
            if after < before && !simple.is_empty() {
                edits.push((i, simple));
                removed += before - after;
            }
        }
        self.set_paths(&edits)?;
        Ok(removed)
    }

    /// Round the corners of the objects at `indices` (Illustrator's Round
    /// Corners): `radius` in design units, each fitted so the rounds never
    /// pass each other. `only`, when not empty, limits it to the corners
    /// those points (of a single object, as its path is drawn) belong to.
    /// Returns how many corners it's for.
    ///
    /// The corners are the object's live ones ([`Document::live_corners`]):
    /// the ones still sharp and the ones rounded already, by their handles
    /// or by this, which all take `radius` (0 makes them sharp again) and
    /// stay live. An inverted round or a chamfer is cut into the path
    /// instead, in place of any round the corner had, and its points are
    /// ordinary points from then on.
    pub fn round_corners(
        &mut self,
        indices: &[usize],
        radius: f64,
        kind: bezier::CornerKind,
        only: &[bezier::AnchorRef],
    ) -> Result<usize> {
        let uids: Vec<usize> = indices
            .iter()
            .filter_map(|&i| self.shapes.get(i))
            .map(|s| s.uid)
            .collect();
        let radius = radius.max(0.0);
        let mut count = 0;
        let mut seen = std::collections::HashSet::new();
        let mut cut: Vec<(usize, BezPath)> = Vec::new();
        let mut live_ones: Vec<(usize, LiveCorners)> = Vec::new();
        for uid in uids {
            let Some(i) = self.shapes.iter().position(|s| s.uid == uid) else {
                continue;
            };
            if !seen.insert(self.shapes[i].id.clone()) {
                continue;
            }
            let drawn = self.edit_path(i)?;
            // (Text has no live corners until it's rounded: its outline.)
            let live = self
                .live_corners(i)
                .unwrap_or_else(|_| LiveCorners::plain(drawn.clone()));
            // The corners asked for: all of them, or those the points in
            // `only` are of (found by where they are: a path read back
            // from the file may have dropped a doubled point).
            let (as_drawn, from) = live.path_from();
            let asked: Option<Vec<bezier::AnchorRef>> = (!only.is_empty()).then(|| {
                only.iter()
                    .filter_map(|&(s, a)| {
                        let p = drawn.get(s)?.anchors.get(a)?.p;
                        let sp = as_drawn.get(s)?;
                        (0..sp.anchors.len())
                            .map(|k| (k, sp.anchors[k].p.dist(p)))
                            .min_by(|x, y| x.1.total_cmp(&y.1))
                            .filter(|(_, d)| *d < 0.05)
                            .map(|(k, _)| (s, from[s][k]))
                    })
                    .collect()
            });
            let corners: Vec<bezier::AnchorRef> = bezier::path_corners(&live.base)
                .iter()
                .map(|c| c.at_ref)
                .filter(|r| asked.as_ref().is_none_or(|a| a.contains(r)))
                .collect();
            if corners.is_empty() {
                continue;
            }
            count += corners.len();
            let round = kind == bezier::CornerKind::Round;
            let mut next = live.clone();
            for &c in &corners {
                next.set_radius(c, if round { radius } else { 0.0 });
            }
            if round || radius <= 0.0 {
                if next != live {
                    live_ones.push((uid, next));
                }
                continue;
            }
            // Cut another way: into the path as it is with those corners
            // sharp (the others keeping their rounds, as ordinary points).
            let (sharp, from) = next.path_from();
            let mut at: Vec<bezier::AnchorRef> = Vec::new();
            for (s, of) in from.iter().enumerate() {
                for (k, b) in of.iter().enumerate() {
                    if corners.contains(&(s, *b)) {
                        at.push((s, k));
                    }
                }
            }
            let out =
                bezier::cut_corners(&sharp, |r| if at.contains(&r) { radius } else { 0.0 }, kind);
            cut.push((uid, out));
        }
        let at = |d: &Document, uid: usize| d.shapes.iter().position(|s| s.uid == uid);
        let edits: Vec<(usize, BezPath)> = cut
            .into_iter()
            .filter_map(|(uid, p)| Some((at(self, uid)?, p)))
            .collect();
        self.set_paths(&edits)?;
        for (uid, live) in live_ones {
            if let Some(i) = at(self, uid) {
                self.set_live_corners(i, &live)?;
            }
        }
        Ok(count)
    }

    /// The radius the corners of the objects at `indices` are rounded with
    /// (design units): the largest any is drawn at, or `None` when they're
    /// all sharp. What Round Corners… starts from, so that it says what
    /// the corners' handles have set.
    pub fn corner_radius(&self, indices: &[usize]) -> Option<f64> {
        indices
            .iter()
            .filter_map(|&i| self.live_corners(i).ok())
            .flat_map(|live| {
                bezier::fitted_radii(&live.base, |r| live.radius(r))
                    .into_iter()
                    .map(|(_, r)| r)
            })
            .filter(|r| *r > 1e-6)
            .max_by(f64::total_cmp)
    }
}

impl Document {
    /// Whether Simplify Design may unite object `i` with its neighbors: a
    /// filled shape of its own (not lettering, not half of an element with
    /// an outline too), sewn, not locked, and sewn as stitches that one
    /// bigger shape could be (not an appliqué or a pattern of threads).
    fn unitable(&self, i: usize) -> bool {
        let s = &self.shapes[i];
        matches!(s.geometry, Geometry::Fill { .. })
            && !matches!(
                s.params.kind,
                Some(Kind::Skip | Kind::Applique | Kind::Cross | Kind::Tartan | Kind::Manual)
            )
            && !s.params.hidden()
            && !s.params.locked()
            && self.lettering(i).is_none()
            && self.shapes.iter().filter(|o| o.id == s.id).count() == 1
    }

    /// The next layers to unite: filled shapes in one thread that sew one
    /// after another (nothing in another thread between them) and touch,
    /// as indices in sewing order; with `alike`, only ones sewn as the
    /// same stitch type. `None` when there are none left.
    fn touching_run(&self, alike: bool) -> Option<Vec<usize>> {
        let sewn = |s: &svg::Shape| s.params.kind != Some(Kind::Skip) && !s.params.hidden();
        let n = self.shapes.len();
        let mut start = 0;
        while start < n {
            // A run: up to the next sewn layer in another thread.
            let color = self.shapes[start].color;
            let mut end = start;
            while end < n && (!sewn(&self.shapes[end]) || self.shapes[end].color == color) {
                end += 1;
            }
            let run: Vec<usize> = (start..end)
                .filter(|&i| sewn(&self.shapes[i]) && self.unitable(i))
                .collect();
            if run.len() >= 2 {
                let areas: Vec<(Vec<Polyline>, FillRule)> =
                    run.iter().map(|&i| self.area(i)).collect();
                let boxes: Vec<crate::geom::Bounds> = areas
                    .iter()
                    .map(|(rings, _)| {
                        let mut b = crate::geom::Bounds::empty();
                        rings.iter().flatten().for_each(|&p| b.add(p));
                        b
                    })
                    .collect();
                // Touching: within a tenth of a millimetre of overlapping.
                let kind = |k: usize| self.shapes[run[k]].params.kind.unwrap_or(Kind::Fill);
                let near = |a: usize, b: usize| {
                    let (x, y) = (&boxes[a], &boxes[b]);
                    if (alike && kind(a) != kind(b))
                        || x.is_empty()
                        || y.is_empty()
                        || x.min.x > y.max.x + 1.0
                        || y.min.x > x.max.x + 1.0
                        || x.min.y > y.max.y + 1.0
                        || y.min.y > x.max.y + 1.0
                    {
                        return false;
                    }
                    let grown = boolean::offset(&areas[a].0, areas[a].1, 1.0);
                    !boolean::pathfinder(
                        &[(grown, FillRule::NonZero), areas[b].clone()],
                        PathOp::Intersect,
                    )
                    .is_empty()
                };
                // The first layer, and everything it reaches through
                // layers that touch.
                for first in 0..run.len() {
                    let mut group = vec![first];
                    let mut k = 0;
                    while k < group.len() {
                        for other in 0..run.len() {
                            if !group.contains(&other) && near(group[k], other) {
                                group.push(other);
                            }
                        }
                        k += 1;
                    }
                    if group.len() >= 2 {
                        group.sort_unstable();
                        return Some(group.into_iter().map(|k| run[k]).collect());
                    }
                }
            }
            start = end.max(start + 1);
        }
        None
    }

    /// Simplify Design's unite: layers that touch, sew one after another
    /// and share a thread become one shape. It's the biggest of them, grown
    /// to cover the rest (so it keeps its name, its group and its stitch
    /// settings), where the first of them was in the sewing order. They're
    /// one color already, so nothing looks different from a distance; what
    /// goes is the stitching under and between them. Returns how many
    /// layers went. With `alike`, only layers sewn as the same stitch type
    /// unite (a satin on a fill stays a satin on a fill).
    pub(super) fn unite_touching(&mut self, alike: bool) -> Result<usize> {
        self.sync()?;
        let mut gone = 0;
        // (Each pass unites one group, so this ends: layers only go.)
        while let Some(idx) = self.touching_run(alike) {
            let parts: Vec<(Vec<Polyline>, FillRule)> = idx.iter().map(|&i| self.area(i)).collect();
            let size = |(rings, _): &(Vec<Polyline>, FillRule)| {
                let mut b = crate::geom::Bounds::empty();
                rings.iter().flatten().for_each(|&p| b.add(p));
                if b.is_empty() {
                    0.0
                } else {
                    b.width() * b.height()
                }
            };
            let biggest = (0..idx.len())
                .max_by(|&a, &b| size(&parts[a]).total_cmp(&size(&parts[b])))
                .map_or(idx[0], |k| idx[k]);
            let path = bezier::from_rings(&boolean::pathfinder(&parts, PathOp::Unite), RESULT_TOL);
            if path.is_empty() {
                break;
            }
            let keep = self.shapes[biggest].uid;
            let rest: Vec<usize> = idx
                .iter()
                .filter(|&&i| i != biggest)
                .map(|&i| self.shapes[i].uid)
                .collect();
            let slot = idx[0];
            self.set_paths(&[(biggest, path)])?;
            let doomed = self.uid_indices(&rest);
            self.delete_many(&doomed)?;
            // Where the first of them was.
            if let Some(at) = self.shapes.iter().position(|s| s.uid == keep)
                && at != slot.min(self.shapes.len() - 1)
            {
                let made = self.shapes.remove(at);
                self.shapes.insert(slot.min(self.shapes.len()), made);
            }
            self.sync()?;
            gone += rest.len();
        }
        Ok(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svg::Geometry;

    const ART: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60mm" height="40mm" viewBox="0 0 60 40">
      <g transform="translate(5 5)">
        <circle id="dot" cx="10" cy="10" r="6" fill="#c00" stroke="#000" stroke-width="0.5" transform="rotate(10 10 10)"/>
        <path id="wave" d="M20 20 L40 20" stroke="#00c" fill="none" stroke-width="0.4"/>
      </g>
    </svg>"##;

    fn bounds(d: &Document, i: usize) -> crate::geom::Bounds {
        let mut b = crate::geom::Bounds::empty();
        match &d.shapes[i].geometry {
            Geometry::Fill { rings, .. } => rings.iter().flatten().for_each(|&p| b.add(p)),
            Geometry::Stroke { lines, .. } => {
                lines.iter().flat_map(|(l, _)| l).for_each(|&p| b.add(p))
            }
        }
        b
    }

    #[test]
    fn a_previewed_path_is_the_geometry_setting_it_gives() {
        // A fill with an outline, moved and turned: one path, two shapes.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60mm" height="40mm" viewBox="0 0 60 40">
          <rect id="box" x="10" y="8" width="30" height="20" fill="#c00" stroke="#000" stroke-width="1" transform="rotate(8 20 20)"/>
        </svg>"##;
        let mut shown = Document::from_svg(svg, "t", None).unwrap();
        assert_eq!(shown.shapes.len(), 2);
        let xml = shown.xml.clone();
        let round = bezier::cut_corners(
            &shown.edit_path(0).unwrap(),
            |_| 30.0,
            bezier::CornerKind::Round,
        );
        let mut set = shown.clone();
        shown.preview_path(0, &round);
        assert_eq!(shown.xml, xml, "the SVG isn't touched");
        set.set_path(0, &round).unwrap();
        let pts = |d: &Document, i: usize| -> Vec<P> {
            match &d.shapes[i].geometry {
                Geometry::Fill { rings, .. } => rings.concat(),
                Geometry::Stroke { lines, .. } => {
                    assert!(lines.iter().all(|(_, closed)| *closed));
                    lines.iter().flat_map(|(l, _)| l.clone()).collect()
                }
            }
        };
        for i in 0..2 {
            let (a, b) = (pts(&shown, i), pts(&set, i));
            assert!(a.len() > 8, "curves, flattened");
            assert_eq!(a.len(), b.len(), "shape {i}");
            for (p, q) in a.iter().zip(&b) {
                assert!(p.dist(*q) < 0.05, "{p:?} vs {q:?}");
            }
        }
    }

    #[test]
    fn release_compound_path_splits_shapes_and_keeps_holes() {
        // Three separate squares in one path; the first has a square hole.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60mm" height="20mm" viewBox="0 0 60 20">
          <path id="pills" fill="#420" fill-rule="evenodd" transform="translate(1 1)"
            d="M0 0H16V16H0Z M4 4H12V12H4Z M20 0H36V16H20Z M40 0H56V16H40Z"/>
        </svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        assert_eq!(d.shapes.len(), 1);
        d.shapes[0].params.angle = Some(45.0);
        let before = bounds(&d, 0);
        let (made, groups) = d.release_compound(&[0]).unwrap();
        assert_eq!(made.len(), 3, "one object per separate shape");
        assert_eq!(groups.len(), 1);
        let mut rings: Vec<usize> = made
            .iter()
            .map(|&i| match &d.shapes[i].geometry {
                Geometry::Fill { rings, .. } => rings.len(),
                _ => 0,
            })
            .collect();
        rings.sort();
        assert_eq!(rings, vec![1, 1, 2], "the hole stays with its square");
        let mut after = crate::geom::Bounds::empty();
        for &i in &made {
            assert_eq!(
                d.shapes[i].params.angle,
                Some(45.0),
                "settings carried over"
            );
            assert_eq!(d.shapes[i].color, [0x44, 0x22, 0x00]);
            assert_eq!(d.shapes[i].groups.last(), Some(&groups[0]));
            let b = bounds(&d, i);
            after.add(b.min);
            after.add(b.max);
        }
        assert!((after.min - before.min).len() < 0.5 && (after.max - before.max).len() < 0.5);
        let out = d.to_svg().unwrap();
        assert!(out.contains(&format!("<g id=\"{}\"", groups[0])), "{out}");
        assert!(
            d.release_compound(&made[..1]).is_err(),
            "a single shape has nothing to release"
        );
    }

    #[test]
    fn editing_points_round_trips_through_the_svg() {
        let mut d = Document::from_svg(ART, "t", None).unwrap();
        let i = d.shapes.iter().position(|s| s.id == "wave").unwrap();
        let uid = d.shapes[i].uid;
        let mut p = d.edit_path(i).unwrap();
        assert_eq!(p[0].anchors.len(), 2);
        let b0 = bounds(&d, i);
        // Bend it: pull the first point's handle down 50 units (5 mm).
        let a = p[0].anchors[0].p;
        p[0].anchors[0].h_out = a + P::new(30.0, 50.0);
        d.set_path(i, &p).unwrap();
        let i = d.shapes.iter().position(|s| s.uid == uid).unwrap();
        let b1 = bounds(&d, i);
        assert!(b1.max.y > b0.max.y + 10.0, "it curves down now");
        let again = d.edit_path(i).unwrap();
        assert!((again[0].anchors[0].h_out - p[0].anchors[0].h_out).len() < 0.05);
        assert!(d.to_svg().unwrap().contains(" C"));
    }

    #[test]
    fn a_circle_becomes_a_path_keeping_paint_and_transform() {
        let mut d = Document::from_svg(ART, "t", None).unwrap();
        let n = d.shapes.len();
        let i = d.shapes.iter().position(|s| s.id == "dot").unwrap();
        let uid = d.shapes[i].uid;
        let b0 = bounds(&d, i);
        let mut p = d.edit_path(i).unwrap();
        assert_eq!(p[0].anchors.len(), 4);
        // Unchanged: the same shape, now as a path.
        d.set_path(i, &p).unwrap();
        let i = d.shapes.iter().position(|s| s.uid == uid).unwrap();
        let b1 = bounds(&d, i);
        assert!((b1.min - b0.min).len() < 0.5 && (b1.max - b0.max).len() < 0.5);
        let xml = d.to_svg().unwrap();
        assert!(!xml.contains("<circle"), "{xml}");
        assert!(xml.contains("rotate(10 10 10)") && xml.contains("fill=\"#c00\""));
        assert_eq!(d.shapes.len(), n, "fill and outline both still there");
        // Drag a point out.
        p[0].anchors[0].p = p[0].anchors[0].p + P::new(40.0, 0.0);
        d.set_path(i, &p).unwrap();
        let i = d.shapes.iter().position(|s| s.uid == uid).unwrap();
        assert!(bounds(&d, i).width() > b0.width() + 20.0);
    }

    #[test]
    fn lettering_reads_as_its_glyph_outlines() {
        let mut d = Document::from_svg(ART, "t", None).unwrap();
        let spec = super::super::TextSpec {
            text: "A".into(),
            ..Default::default()
        };
        let i = d.add_lettering(&spec, (0.0, 0.0)).unwrap();
        let p = d.edit_path(i).unwrap();
        assert_eq!(p.len(), 2, "an A: outer ring and its hole");
        assert!(d.lettering(i).is_some(), "reading doesn't change it");
    }

    const SQUARES: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="40mm" viewBox="0 0 40 40">
      <rect id="back" x="0" y="0" width="20" height="20" fill="#c00"/>
      <rect id="front" x="10" y="10" width="20" height="20" fill="#00c"/>
      <path id="line" d="M0 35 L30 35" stroke="#0a0" stroke-width="3" fill="none"/>
    </svg>"##;

    fn area_mm2(d: &Document, i: usize) -> f64 {
        match &d.shapes[i].geometry {
            Geometry::Fill { rings, .. } => {
                let mut v: Vec<f64> = rings.iter().map(|r| boolean::ring_area(r)).collect();
                v.sort_by(|a, b| b.total_cmp(a));
                let (outer, holes) = v.split_first().unwrap();
                (outer - holes.iter().sum::<f64>()) / 100.0
            }
            _ => 0.0,
        }
    }

    #[test]
    fn pathfinder_replaces_the_objects_with_one_shape() {
        for (op, mm2, color) in [
            (PathOp::Unite, 700.0, [0, 0, 0xcc]),
            (PathOp::MinusFront, 300.0, [0xcc, 0, 0]),
            (PathOp::Intersect, 100.0, [0, 0, 0xcc]),
            (PathOp::Exclude, 600.0, [0, 0, 0xcc]),
        ] {
            let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
            let i = d.pathfinder(&[0, 1], op).unwrap();
            assert_eq!(d.shapes.len(), 2, "{op:?}: two squares became one");
            // Exclude leaves two L shapes (no holes): add them up.
            let a = if op == PathOp::Exclude {
                match &d.shapes[i].geometry {
                    Geometry::Fill { rings, .. } => {
                        rings.iter().map(|r| boolean::ring_area(r)).sum::<f64>() / 100.0
                    }
                    _ => 0.0,
                }
            } else {
                area_mm2(&d, i)
            };
            assert!((a - mm2).abs() < 1.0, "{op:?}: {a}");
            assert_eq!(d.shapes[i].color, color, "{op:?}");
            assert_eq!(i, 0, "{op:?}: sews where the back object did");
            // It survives a save and reload.
            let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
            assert_eq!(again.shapes.len(), 2);
        }
    }

    #[test]
    fn offset_outline_and_simplify() {
        let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
        let new = d.offset_path(&[0], 20.0).unwrap();
        assert_eq!(d.shapes.len(), 4);
        let a = area_mm2(&d, new[0]);
        assert!(
            a > 20.0 * 20.0 + 4.0 * 20.0 * 2.0 - 1.0,
            "grew by 2 mm: {a}"
        );
        assert!(
            new[0]
                < d.shapes
                    .iter()
                    .position(|s| s.label.contains("back"))
                    .unwrap()
                    + 1
        );

        let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
        let line = d.shapes.iter().position(|s| s.id == "line").unwrap();
        let new = d.outline_stroke(&[line]).unwrap();
        assert_eq!(d.shapes.len(), 3);
        assert!(matches!(d.shapes[new[0]].geometry, Geometry::Fill { .. }));
        assert!(
            (area_mm2(&d, new[0]) - 30.0 * 3.0).abs() < 2.0,
            "{}",
            area_mm2(&d, new[0])
        );

        let mut d = Document::from_svg(ART, "t", None).unwrap();
        let dot = d.shapes.iter().position(|s| s.id == "dot").unwrap();
        // Make it a 40-point polygon first, then simplify it back down.
        let p = d.edit_path(dot).unwrap();
        let ring: Vec<P> = (0..40)
            .map(|k| {
                crate::bezier::cubic_at(
                    &p[0].cubic(k / 10, (k / 10 + 1) % 4),
                    (k % 10) as f64 / 10.0,
                )
            })
            .collect();
        d.set_path(dot, &crate::bezier::from_rings(&[ring], 0.0))
            .unwrap();
        let dot = d
            .shapes
            .iter()
            .position(|s| s.label.contains("dot"))
            .unwrap();
        let removed = d.simplify_paths(&[dot], 1.5).unwrap();
        assert!(removed > 20, "{removed}");
    }

    /// Shape `id`'s part, wherever it is in the list.
    fn part_of(d: &Document, id: &str, part: Part) -> Option<usize> {
        d.shapes
            .iter()
            .position(|s| svg::name_words(&s.id) == [id] && s.part == part)
    }

    fn stroke_width(d: &Document, i: usize) -> f64 {
        match &d.shapes[i].geometry {
            Geometry::Stroke { width, .. } => *width,
            g => panic!("a stroke: {g:?}"),
        }
    }

    #[test]
    fn a_shape_is_given_an_outline_of_its_own() {
        let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
        let back = part_of(&d, "back", Part::Fill).unwrap();
        let new = d.add_outline(&[back], Kind::Satin, 2.0, None).unwrap();
        assert_eq!(new, [back + 1], "sewn right after its shape");
        assert_eq!(d.shapes.len(), 4);
        let o = &d.shapes[new[0]];
        assert_eq!(
            (o.id.as_str(), o.part),
            (d.shapes[back].id.as_str(), Part::Stroke)
        );
        assert_eq!(o.params.kind, Some(Kind::Satin));
        assert!((stroke_width(&d, new[0]) - 20.0).abs() < 1e-6);
        // A darker shade of the shape's red.
        assert_eq!(o.color, [0x7a, 0, 0]);
        assert_eq!(d.shapes[back].color, [0xcc, 0, 0]);
        assert_eq!(svg::display_names(&d.shapes)[new[0]], "back (outline)");

        // The shape and its outline are colored and set up apart, and stay
        // that way through a save.
        d.shapes[back].color = [0, 0, 0xff];
        d.shapes[back].params.spacing = Some(0.9);
        d.shapes[new[0]].color = [0xff, 0xff, 0];
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        let (f, o) = (
            part_of(&again, "back", Part::Fill).unwrap(),
            part_of(&again, "back", Part::Stroke).unwrap(),
        );
        assert_eq!(again.shapes[f].color, [0, 0, 0xff]);
        assert_eq!(again.shapes[f].params.spacing, Some(0.9));
        assert_eq!(again.shapes[o].color, [0xff, 0xff, 0]);
        assert_eq!(again.shapes[o].params.spacing, None);
        assert_eq!(again.shapes[o].params.kind, Some(Kind::Satin));
        assert!((stroke_width(&again, o) - 20.0).abs() < 0.01);

        // Switched off (hidden), an outline stays off through a save, and
        // its shape stays on.
        let mut hidden = again.clone();
        hidden.shapes[o].params.hide = Some(true);
        let hidden = Document::from_svg(&hidden.to_svg().unwrap(), "t", None).unwrap();
        assert!(hidden.shapes[o].params.hidden());
        assert!(!hidden.shapes[f].params.hidden());
        assert_eq!(hidden.shapes[o].params.kind, Some(Kind::Satin));
        assert_eq!(hidden.shapes[o].color, [0xff, 0xff, 0]);

        // Left as it was: the shape's own color under a new thread, and an
        // outline nobody recolored keeps the shade it was given.
        let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
        let new = d.add_outline(&[0], Kind::Run, 2.0, None).unwrap();
        d.shapes[0].color = [0, 0xff, 0];
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(again.shapes[new[0]].color, [0x7a, 0, 0]);
    }

    #[test]
    fn an_outline_is_as_wide_as_asked_whatever_the_shapes_scale_and_style() {
        // Scaled 3× by its group, with a stylesheet and a `style` that both
        // say there's no stroke.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60mm" height="60mm" viewBox="0 0 60 60">
          <style>.st0{fill:#E8A33C;stroke:none;}</style>
          <g transform="scale(3)">
            <circle id="sun" class="st0" cx="10" cy="10" r="5"/>
            <rect id="box" x="1" y="1" width="4" height="4" style="fill:#2F4858; stroke:none; stroke-width:9" stroke="none"/>
          </g>
        </svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        assert_eq!(d.shapes.len(), 2);
        let new = d
            .add_outline(&[0, 1], Kind::Satin, 1.5, Some([1, 2, 3]))
            .unwrap();
        assert_eq!(new, [1, 3]);
        for &i in &new {
            assert!((stroke_width(&d, i) - 15.0).abs() < 1e-6);
            assert_eq!(d.shapes[i].color, [1, 2, 3]);
        }
        let saved = d.to_svg().unwrap();
        assert!(
            saved.contains("style=\"fill:#2F4858;stroke:#010203;"),
            "{saved}"
        );
        let again = Document::from_svg(&saved, "t", None).unwrap();
        assert_eq!(again.shapes.len(), 4);
        for &i in &new {
            assert!(
                (stroke_width(&again, i) - 15.0).abs() < 0.01,
                "{}",
                stroke_width(&again, i)
            );
            assert_eq!(again.shapes[i].color, [1, 2, 3]);
        }
        // The outline is the shape's own edge: reshape one, both follow.
        let wide = bounds(&d, 1).max.x;
        let mut p = d.edit_path(0).unwrap();
        for a in &mut p[0].anchors {
            a.p.x += 50.0;
            a.h_in.x += 50.0;
            a.h_out.x += 50.0;
        }
        d.set_path(0, &p).unwrap();
        assert!((bounds(&d, 1).max.x - wide - 50.0).abs() < 0.5);

        // A running stitch keeps its width, for when it's made satin, and
        // stays a running stitch through a save.
        let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
        let new = d.add_outline(&[0], Kind::Run, 2.0, None).unwrap();
        assert_eq!(d.shapes[new[0]].params.kind, Some(Kind::Run));
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(again.shapes[new[0]].params.kind, Some(Kind::Run));
        assert!((stroke_width(&again, new[0]) - 20.0).abs() < 0.01);
        assert!(d.add_outline(&[2], Kind::Fill, 2.0, None).is_err());
    }

    #[test]
    fn a_satin_outline_follows_each_edge_of_a_shape_in_parts() {
        // Two squares side by side and a ring, each one path: read as a
        // stroked path's subpaths, each pair would be the two rails of one
        // column, sewn across the whole shape.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60mm" height="40mm" viewBox="0 0 60 40">
          <path id="pair" d="M2 2h10v10h-10z M14 2h10v10h-10z" fill="#c00"/>
          <path id="ring" d="M30 2h20v20h-20z M35 7h10v10h-10z" fill="#00c" fill-rule="evenodd"/>
        </svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        let new = d.add_outline(&[0, 1], Kind::Satin, 2.0, None).unwrap();
        for &i in &new {
            assert!(d.shapes[i].outline);
            let rails = crate::design::satin_rails(&d.shapes[i]);
            assert_eq!(rails.len(), 2, "a column along each edge");
            for r in rails {
                let r = r.ok().expect("a column");
                let widest = crate::stitch::satin::pairs(&r, 10.0)
                    .into_iter()
                    .map(|(a, b)| a.dist(b))
                    .fold(0.0, f64::max);
                assert!(widest < 30.0, "2 mm wide, not across the shape: {widest}");
            }
        }
        assert!(!d.shapes[0].outline);
        // The same of a saved file's outline.
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(crate::design::satin_rails(&again.shapes[new[0]]).len(), 2);
    }

    #[test]
    fn outlines_of_every_style_and_width_are_sewn_on_the_edge() {
        let star: String = (0..10)
            .map(|k| {
                let a = std::f64::consts::PI * (k as f64 / 5.0 - 0.5);
                let r = if k % 2 == 0 { 12.0 } else { 5.0 };
                let (x, y) = (20.0 + r * a.cos(), 20.0 + r * a.sin());
                format!("{}{x:.3} {y:.3}", if k == 0 { "M" } else { "L" })
            })
            .collect::<String>()
            + "Z";
        let shapes = [
            ("square", "M5 5h20v20h-20z"),
            ("circle", "M10 20a10 10 0 1 0 20 0a10 10 0 1 0 -20 0z"),
            ("star", star.as_str()),
            ("ell", "M5 5h8v14h12v8h-20z"),
            ("bar", "M5 5h30v3h-30z"),
            ("dot", "M18 20a2 2 0 1 0 4 0a2 2 0 1 0 -4 0z"),
            ("ring", "M5 5h24v24h-24z M12 12h10v10h-10z"),
            ("pair", "M2 2h10v10h-10z M14 2h10v10h-10z"),
        ];
        // (how it's sewn, width mm, running passes)
        let styles = [
            (Kind::Run, 2.0, None),
            (Kind::Run, 2.0, Some(3)),
            (Kind::Manual, 2.0, None),
            (Kind::Satin, 1.0, None),
            (Kind::Satin, 1.5, None),
            (Kind::Satin, 2.0, None),
            (Kind::Satin, 3.0, None),
            (Kind::Satin, 4.0, None),
            (Kind::Satin, 6.0, None),
            (Kind::Blanket, 2.0, None),
        ];
        let off = |l: &[P], p: P| {
            (0..l.len())
                .map(|i| {
                    let (a, b) = (l[i], l[(i + 1) % l.len()]);
                    let ab = b - a;
                    let t = ((p - a).dot(ab) / ab.dot(ab).max(1e-12)).clamp(0.0, 1.0);
                    p.dist(a + ab * t)
                })
                .fold(f64::INFINITY, f64::min)
        };
        for (name, path) in shapes {
            for (kind, w, repeats) in styles {
                let what = format!("{name}, {kind:?} {w} mm");
                let svg = format!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" width="40mm" height="40mm" viewBox="0 0 40 40"><path id="s" d="{path}" fill="#c00" fill-rule="evenodd"/></svg>"##
                );
                let mut d = Document::from_svg(&svg, "t", None).unwrap();
                let made = if kind == Kind::Satin { kind } else { Kind::Run };
                let o = d.add_outline(&[0], made, w, None).unwrap()[0];
                d.shapes[o].params.kind = Some(kind);
                d.shapes[o].params.repeats = repeats;
                // As it is in a saved file.
                let d = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
                let s = d.shapes[o].clone();
                assert_eq!(s.params.kind, Some(kind), "{what}");
                let Geometry::Stroke { lines, width } = &s.geometry else {
                    panic!("{what}: a stroke");
                };
                assert!((width - w * 10.0).abs() < 0.01, "{what}: {width}");
                let plan = crate::design::plan("t", std::slice::from_ref(&s), &d.plan_options());
                assert!(plan.notes.is_empty(), "{what}: {:?}", plan.notes);
                let pts: Vec<P> = plan
                    .blocks
                    .iter()
                    .flat_map(|b| &b.runs)
                    .flat_map(|r| r.pts.iter().copied())
                    .collect();
                // Every edge is sewn: each of the path's points has a
                // stitch near it.
                let reach = match kind {
                    Kind::Satin => w * 10.0 + 3.0,
                    _ => 0.5,
                };
                for (l, _) in lines {
                    for &c in l {
                        assert!(
                            pts.iter().any(|p| p.dist(c) <= reach.max(15.0)),
                            "{what}: nothing sewn at {c:?}"
                        );
                    }
                }
                // And nothing is sewn away from the edge: a running stitch
                // on it, a blanket's prongs 2.5 mm in, satin half its
                // width either side (its whole width at a sharp point).
                let far = pts
                    .iter()
                    .map(|&p| {
                        lines
                            .iter()
                            .map(|(l, _)| off(l, p))
                            .fold(f64::INFINITY, f64::min)
                    })
                    .fold(0.0, f64::max);
                let most = match kind {
                    Kind::Satin => w * 10.0 + 3.0,
                    Kind::Blanket => 25.5,
                    _ => 0.5,
                };
                assert!(far <= most, "{what}: {far} units from the edge");
                if kind == Kind::Satin {
                    assert!(far >= w * 5.0, "{what}: as wide as asked, {far}");
                }
            }
        }
    }

    #[test]
    fn a_shapes_outline_goes_where_the_shape_goes() {
        let outlined = || {
            let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
            d.add_outline(&[0, 1], Kind::Satin, 2.0, Some([9, 9, 9]))
                .unwrap();
            assert_eq!(d.shapes.len(), 5);
            d
        };
        // Deleted with its shape (and not left behind in the file); an
        // outline deleted on its own leaves the shape.
        let mut d = outlined();
        assert_eq!(d.with_outlines(&[0]), [0, 1]);
        assert_eq!(d.with_outlines(&[1]), [1]);
        assert_eq!(d.with_outlines(&[0, 1, 4]), [0, 1, 4]);
        let gone = d.with_outlines(&[0]);
        d.delete_many(&gone).unwrap();
        d.sync().unwrap();
        assert_eq!(d.shapes.len(), 3);
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(again.shapes.len(), 3);
        assert!(again.shapes.iter().all(|s| !s.label.contains("back")));
        // An outline deleted on its own is gone for good: not back,
        // switched off, the next time the file is opened. The shape stays,
        // and takes a new one when asked.
        let mut d = outlined();
        d.delete(1).unwrap();
        d.sync().unwrap();
        assert_eq!(d.shapes.len(), 4);
        assert_eq!(d.shapes[0].params.kind, Some(Kind::Fill));
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(again.shapes.len(), 4, "{}", again.xml);
        assert!(part_of(&again, "back", Part::Stroke).is_none());
        assert!(!again.xml.contains("stroke.skip"), "{}", again.xml);
        let mut again = again;
        let o = again.add_outline(&[0], Kind::Run, 2.0, None).unwrap();
        assert_eq!(o, [1]);
        // United into another shape, the old shapes' outlines go too: they
        // were those shapes' edges, not the new one's.
        let mut d = outlined();
        d.pathfinder(&[0, 2], PathOp::Unite).unwrap();
        assert_eq!(d.shapes.len(), 2, "the new shape and the line");
        assert!(d.shapes.iter().all(|s| !s.outline));
        // Copied with it, thread and all; moved with it; and each piece of
        // a shape released into pieces keeps one.
        let mut d = outlined();
        let copy = d.duplicate(&[0], P::new(300.0, 0.0)).unwrap();
        assert_eq!(copy.len(), 2);
        let c = &d.shapes[copy[1]];
        assert!(c.outline && c.color == [9, 9, 9] && c.params.kind == Some(Kind::Satin));
        let before = bounds(&d, 1).min.x;
        d.transform(&[0], [1.0, 0.0, 0.0, 1.0, 70.0, 0.0]).unwrap();
        assert!((bounds(&d, 1).min.x - before - 70.0).abs() < 0.01);
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60mm" height="40mm" viewBox="0 0 60 40">
          <path id="c" d="M30 2h8v8h-8z M40 2h8v8h-8z" fill="#0a0"/></svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        d.add_outline(&[0], Kind::Run, 2.0, Some([9, 9, 9]))
            .unwrap();
        let (pieces, _) = d.release_compound(&[0]).unwrap();
        assert_eq!(pieces.len(), 4);
        assert_eq!(d.shapes.iter().filter(|s| s.outline).count(), 2);
        assert!(
            d.shapes
                .iter()
                .filter(|s| s.outline)
                .all(|s| s.color == [9, 9, 9] && s.params.kind == Some(Kind::Run))
        );
    }

    #[test]
    fn outlines_on_awkward_artwork_are_added_or_refused_cleanly() {
        // Switched off (hidden), it's made anew when asked for again.
        let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
        let o = d.add_outline(&[0], Kind::Satin, 2.0, None).unwrap()[0];
        d.shapes[o].params.hide = Some(true);
        let o = d
            .add_outline(&[0], Kind::Run, 2.0, Some([5, 5, 5]))
            .unwrap()[0];
        assert!(!d.shapes[o].params.hidden());
        assert_eq!(d.shapes[o].params.kind, Some(Kind::Run));
        assert_eq!(d.shapes[o].color, [5, 5, 5]);
        assert_eq!(d.shapes.len(), 4);

        // A shape that `<use>` copies are drawn from is refused, and so are
        // the copies (which have no look of their own): an outline on the
        // original would be on every copy, with no settings to call its
        // own. Refused, the document is exactly as it was.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 40">
          <path id="leaf" d="M0 0h20v20h-20z" fill="#0a0"/>
          <use id="twin" href="#leaf" x="40"/>
          <defs><rect id="stamp" width="5" height="5" fill="#00a"/></defs>
          <use href="#stamp" x="70"/>
          <rect id="plain" x="80" y="20" width="10" height="10" fill="#a00"/>
        </svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        d.sync().unwrap();
        let before = (d.to_svg().unwrap(), d.shapes.len());
        let plain = part_of(&d, "plain", Part::Fill).unwrap();
        for i in (0..d.shapes.len()).filter(|&i| i != plain) {
            let err = d.add_outline(&[i], Kind::Run, 2.0, None).unwrap_err();
            assert!(
                err.to_string().contains("copies")
                    || err.to_string().contains("select a filled shape"),
                "{}: {err}",
                d.shapes[i].id
            );
            assert_eq!(
                (d.to_svg().unwrap(), d.shapes.len()),
                before,
                "refused, nothing changed"
            );
        }
        // (One that isn't copied, in the same selection, still takes one.)
        let all: Vec<usize> = (0..d.shapes.len()).collect();
        let new = d.add_outline(&all, Kind::Run, 2.0, None).unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(d.shapes.len(), before.1 + 1);
        assert!(
            d.shapes
                .iter()
                .filter(|s| s.outline)
                .all(|s| !s.id.is_empty())
        );

        // A `style` in single quotes, with quoted words in it, is still
        // XML afterwards; and a shape with nothing to its name but a
        // stylesheet's class, hidden by `display`, or inside a transformed,
        // nested group, takes one too.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 40">
          <style>.k{fill:#123456;stroke:none;stroke-width:0}</style>
          <rect id="a" x="2" y="2" width="10" height="10" style='fill:#c00;font-family:&apos;A B&apos;, "C"'/>
          <g transform="translate(30 0) rotate(20) scale(2 2)"><g transform="skewX(10)">
            <circle id="b" class="k" cx="10" cy="10" r="4"/>
          </g></g>
          <polygon id="c" points="60,2 70,2 65,12" fill="#00c" stroke="#fff" stroke-opacity="0" stroke-dasharray="1 2"/>
        </svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        let fills: Vec<usize> = (0..d.shapes.len())
            .filter(|&i| d.shapes[i].part == Part::Fill)
            .collect();
        assert_eq!(fills.len(), 3);
        let new = d.add_outline(&fills, Kind::Satin, 1.5, None).unwrap();
        assert_eq!(new.len(), 3, "{}", d.xml);
        for &i in &new {
            assert!(
                (stroke_width(&d, i) - 15.0).abs() < 1e-6,
                "{}: {}",
                d.shapes[i].id,
                stroke_width(&d, i)
            );
        }
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(again.shapes.len(), 6);
        for &i in &new {
            assert!(again.shapes[i].outline);
            assert!(
                (stroke_width(&again, i) - 15.0).abs() < 0.05,
                "{}: {}",
                again.shapes[i].id,
                stroke_width(&again, i)
            );
        }
    }

    /// A design with an outline put on every shape that takes one (in
    /// every style, some recolored, some switched off) sews, and reads
    /// back from a save exactly as it was, twice over; with the outlines
    /// taken off again its shapes are as they started.
    fn outlines_hold_on(name: &str, xml: &str) {
        // What matters of each object, for telling two readings apart.
        fn look(d: &Document) -> Vec<String> {
            d.shapes
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let b = bounds(d, i);
                    let w = match &s.geometry {
                        Geometry::Stroke { width, .. } => *width,
                        Geometry::Fill { .. } => 0.0,
                    };
                    format!(
                        "{:?} {:?} {:?} {:?} hidden={} outline={} w={:.1} [{:.0} {:.0} {:.0} {:.0}] {}",
                        svg::name_words(&s.id),
                        s.part,
                        s.params.kind,
                        s.color,
                        s.params.hidden(),
                        s.outline,
                        w,
                        b.min.x,
                        b.min.y,
                        b.max.x,
                        b.max.y,
                        s.params.tokens().join(" "),
                    )
                })
                .collect()
        }
        let reopened = |d: &Document| Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        {
            let mut d = Document::from_svg(xml, name, None).unwrap();
            // As it is: saved and read again, nothing changes (and again).
            d.sync().unwrap();
            let once = reopened(&d);
            assert_eq!(look(&once), look(&d), "{name}, reopened");
            assert_eq!(
                look(&reopened(&once)),
                look(&once),
                "{name}, reopened twice"
            );
            let objects = d.shapes.len();
            // An outline on every shape that takes one, in turn a running
            // stitch, satin and blanket, some recolored, some switched off.
            let fills: Vec<usize> = (0..objects)
                .filter(|&i| {
                    d.shapes[i].part == Part::Fill
                        && matches!(d.shapes[i].geometry, Geometry::Fill { .. })
                        && d.lettering(i).is_none()
                        && !d
                            .shapes
                            .iter()
                            .any(|o| o.id == d.shapes[i].id && o.part == Part::Stroke)
                })
                .collect();
            let uids: Vec<usize> = fills.iter().map(|&i| d.shapes[i].uid).collect();
            let mut made = 0;
            for (k, uid) in uids.iter().enumerate() {
                let i = d.shapes.iter().position(|s| s.uid == *uid).unwrap();
                let kind = if k % 3 == 1 { Kind::Satin } else { Kind::Run };
                let was = d.clone();
                match d.add_outline(&[i], kind, 1.0 + (k % 4) as f64, None) {
                    Ok(new) => {
                        made += 1;
                        let o = new[0];
                        assert_eq!(o, d.shapes.iter().position(|s| s.uid == *uid).unwrap() + 1);
                        if k % 3 == 2 {
                            d.shapes[o].params.kind = Some(Kind::Blanket);
                        }
                        if k % 2 == 0 {
                            d.shapes[o].color = [7, (k % 250) as u8, 200];
                        }
                        if k % 5 == 4 {
                            d.shapes[o].params.hide = Some(true);
                        }
                    }
                    // (Refused: nothing may have changed.)
                    Err(_) => d = was,
                }
            }
            assert!(
                made > 0 || fills.is_empty(),
                "{name}: no shape took an outline"
            );
            assert_eq!(d.shapes.len(), objects + made, "{name}");
            // It all sews…
            let plan = d.plan();
            assert_eq!(plan.per_shape.len(), d.shapes.len());
            for (i, s) in d.shapes.iter().enumerate() {
                if s.outline && s.params.hidden() {
                    assert_eq!(plan.per_shape[i], 0, "{name}: a hidden outline is sewn");
                } else if s.outline {
                    assert!(plan.per_shape[i] > 0, "{name}: {} isn't sewn", s.id);
                }
            }
            // …and it comes back the same from a save, twice over.
            d.sync().unwrap();
            let once = reopened(&d);
            assert_eq!(look(&once), look(&d), "{name}, outlined and reopened");
            let twice = reopened(&once);
            assert_eq!(
                look(&twice),
                look(&once),
                "{name}, outlined and reopened twice"
            );
            assert_eq!(
                twice.to_svg().unwrap(),
                once.to_svg().unwrap(),
                "{name}: the file settles"
            );
            // Every one taken off again leaves the design it started as.
            let outlines: Vec<usize> = (0..twice.shapes.len())
                .filter(|&i| twice.shapes[i].outline && uids.len() > 0)
                .filter(|&i| {
                    // (Only the ones made here: artwork's own stay.)
                    !look(&Document::from_svg(xml, name, None).unwrap())
                        .iter()
                        .any(|l| {
                            l.contains("outline=true")
                                && l.starts_with(&format!(
                                    "{:?}",
                                    svg::name_words(&twice.shapes[i].id)
                                ))
                        })
                })
                .collect();
            let mut bare = twice.clone();
            bare.delete_many(&outlines).unwrap();
            bare.sync().unwrap();
            let bare = reopened(&bare);
            let start = Document::from_svg(xml, name, None).unwrap();
            let fills_now: Vec<&crate::svg::Shape> = bare
                .shapes
                .iter()
                .filter(|s| s.part == Part::Fill)
                .collect();
            let fills_then: Vec<&crate::svg::Shape> = start
                .shapes
                .iter()
                .filter(|s| s.part == Part::Fill)
                .collect();
            assert_eq!(
                fills_now.len(),
                fills_then.len(),
                "{name}: every shape is still there"
            );
            for (a, b) in fills_now.iter().zip(&fills_then) {
                assert_eq!(a.params.kind, b.params.kind, "{name}: {}", a.id);
                assert_eq!(a.color, b.color, "{name}: {}", a.id);
            }
        }
    }

    #[test]
    fn the_example_designs_come_back_the_same_with_outlines_on_everything() {
        for name in ["badge", "groups", "lettering", "bench-grid"] {
            let path = format!("{}/examples/{name}.svg", env!("CARGO_MANIFEST_DIR"));
            outlines_hold_on(name, &std::fs::read_to_string(&path).unwrap());
        }
    }

    /// The same of your own designs: every `.svg` in the folder
    /// `ZIGZAG_DESIGNS` names (`cargo test -- --ignored designs_of_your_own`).
    #[test]
    #[ignore]
    fn designs_of_your_own_come_back_the_same_with_outlines_on_everything() {
        let dir = std::env::var("ZIGZAG_DESIGNS").expect("ZIGZAG_DESIGNS: a folder of .svg files");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "svg") {
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                outlines_hold_on(&name, &std::fs::read_to_string(&path).unwrap());
                seen += 1;
            }
        }
        assert!(seen > 0, "no .svg files in {dir}");
        eprintln!("{seen} designs checked");
    }

    #[test]
    fn a_lines_settings_are_its_own_whatever_svg_says_of_its_fill() {
        // No `fill="none"` on any of these: SVG's default fill is black,
        // and a line has nothing to fill. They're lines, not shapes with
        // outlines, and what they're set to holds through a save.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="80mm" height="100mm" viewBox="0 0 80 100">
          <line id="rule" x1="10" y1="5" x2="60" y2="5" stroke="#000" stroke-width="1"/>
          <path id="diag" d="M10 10 L60 60" stroke="#000" stroke-width="0.5"/>
          <polyline id="bend" points="10,70 40,70" stroke="#000" stroke-width="2"/>
          <path id="rails_satin" d="M10 90 L60 90 M10 94 L60 94" stroke="#000" stroke-width="1"/>
        </svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        assert_eq!(d.shapes.len(), 4);
        assert!(
            d.shapes
                .iter()
                .all(|s| s.part == Part::Stroke && !s.outline)
        );
        // (A name's stitch type is the line's.)
        let rails = d
            .shapes
            .iter()
            .position(|s| s.label.contains("rails"))
            .unwrap();
        assert_eq!(d.shapes[rails].params.kind, Some(Kind::Satin));
        // Two lines side by side in one path are still the two edges of
        // one satin column.
        assert_eq!(crate::design::satin_rails(&d.shapes[rails]).len(), 1);
        for (k, s) in d.shapes.iter_mut().enumerate() {
            s.params.kind = Some(if k % 2 == 0 { Kind::Satin } else { Kind::Run });
            s.params.width = Some(3.0);
            s.params.repeats = Some(3);
            s.color = [1, 2, 3 + k as u8];
            s.params.hide = (k == 1).then_some(true);
        }
        let look = |d: &Document| -> Vec<String> {
            d.shapes
                .iter()
                .map(|s| {
                    format!(
                        "{:?} {:?} {:?} {:?} {:?} {} {}",
                        svg::name_words(&s.id),
                        s.params.kind,
                        s.params.width,
                        s.params.repeats,
                        s.color,
                        s.params.hidden(),
                        s.outline
                    )
                })
                .collect()
        };
        let set = look(&d);
        d.sync().unwrap();
        assert_eq!(look(&d), set, "after a sync");
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(look(&again), set, "reopened");
        let twice = Document::from_svg(&again.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(look(&twice), set, "reopened twice");
    }

    #[test]
    fn only_shapes_without_an_outline_get_one() {
        let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
        let line = part_of(&d, "line", Part::Stroke).unwrap();
        let err = d.add_outline(&[line], Kind::Run, 2.0, None).unwrap_err();
        assert!(err.to_string().contains("stroke already"), "{err}");
        assert_eq!(d.shapes.len(), 3);

        // With a line in the selection, the shapes are outlined all the same.
        let new = d.add_outline(&[0, 1, line], Kind::Run, 2.0, None).unwrap();
        assert_eq!(new, [1, 3]);
        let err = d.add_outline(&[0], Kind::Run, 2.0, None).unwrap_err();
        assert!(err.to_string().contains("outline already"), "{err}");
        assert_eq!(d.shapes.len(), 5);

        // A deleted outline comes back, as asked for this time.
        d.delete(1).unwrap();
        d.sync().unwrap();
        assert_eq!(d.shapes.len(), 4);
        let new = d
            .add_outline(&[0], Kind::Satin, 3.0, Some([9, 9, 9]))
            .unwrap();
        assert_eq!(new, [1]);
        assert_eq!(d.shapes[1].params.kind, Some(Kind::Satin));
        assert_eq!(d.shapes[1].color, [9, 9, 9]);
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(again.shapes.len(), 5);
        assert!((stroke_width(&again, 1) - 30.0).abs() < 0.01);
        assert_eq!(again.shapes[1].color, [9, 9, 9]);

        // Text is outlined as shapes, not as text.
        let mut d = Document::empty();
        let spec = crate::document::TextSpec {
            text: "Hi".into(),
            ..Default::default()
        };
        let t = d.add_lettering(&spec, (0.0, 0.0)).unwrap();
        let err = d.add_outline(&[t], Kind::Run, 2.0, None).unwrap_err();
        assert!(err.to_string().contains("Create Outlines"), "{err}");
    }

    #[test]
    fn an_outline_is_sewn_after_its_shape_when_the_order_was_changed() {
        let mut d = Document::from_svg(SQUARES, "t", None).unwrap();
        // The line first, then front, then back.
        d.shapes.reverse();
        d.sync().unwrap();
        let front = part_of(&d, "front", Part::Fill).unwrap();
        assert_eq!(front, 1);
        let new = d.add_outline(&[front], Kind::Run, 2.0, None).unwrap();
        assert_eq!(new, [2]);
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        let order: Vec<(Vec<String>, Part)> = again
            .shapes
            .iter()
            .map(|s| (svg::name_words(&s.id), s.part))
            .collect();
        assert_eq!(
            order,
            [
                (vec!["line".to_string()], Part::Stroke),
                (vec!["front".to_string()], Part::Fill),
                (vec!["front".to_string()], Part::Stroke),
                (vec!["back".to_string()], Part::Fill),
            ]
        );
    }

    #[test]
    fn drawn_curves_are_written_as_curves() {
        let mut d = Document::from_svg(ART, "t", None).unwrap();
        let path = vec![SubPath {
            anchors: vec![
                Anchor {
                    p: P::new(0.0, 0.0),
                    h_in: P::new(0.0, 0.0),
                    h_out: P::new(0.0, 80.0),
                },
                Anchor {
                    p: P::new(100.0, 0.0),
                    h_in: P::new(100.0, 80.0),
                    h_out: P::new(100.0, 0.0),
                },
            ],
            closed: false,
        }];
        let i = d.add_drawing_path(&path, [0, 0, 0], Kind::Run).unwrap();
        let b = bounds(&d, i);
        assert!(b.max.y > 50.0, "bulges down: {:?}", b.max);
        let back = d.edit_path(i).unwrap();
        assert!((back[0].anchors[0].h_out - P::new(0.0, 80.0)).len() < 0.05);
    }
}

/// A compound path's separate shapes: each outer subpath with the holes
/// directly inside it (a subpath inside an odd number of others is a hole).
/// Open subpaths are each their own piece.
fn split_pieces(path: &BezPath) -> Vec<BezPath> {
    let rings: Vec<Polyline> = path.iter().map(flatten_subpath).collect();
    let inside = |j: usize, k: usize| {
        path[k].closed
            && rings[j]
                .first()
                .is_some_and(|&q| crate::geom::point_inside(&rings[k..=k], FillRule::NonZero, q))
    };
    let n = path.len();
    let depth: Vec<usize> = (0..n)
        .map(|j| (0..n).filter(|&k| k != j && inside(j, k)).count())
        .collect();
    let mut pieces: Vec<(usize, BezPath)> = Vec::new();
    for j in 0..n {
        if !path[j].closed || depth[j].is_multiple_of(2) {
            pieces.push((j, vec![path[j].clone()]));
        }
    }
    for j in 0..n {
        if path[j].closed && depth[j] % 2 == 1 {
            // The hole goes with the outer shape one level up that holds it.
            let owner = pieces
                .iter_mut()
                .find(|(o, _)| path[*o].closed && depth[*o] + 1 == depth[j] && inside(j, *o));
            match owner {
                Some((_, piece)) => piece.push(path[j].clone()),
                None => pieces.push((j, vec![path[j].clone()])),
            }
        }
    }
    pieces.into_iter().map(|(_, p)| p).collect()
}

fn flatten_subpath(sp: &SubPath) -> Polyline {
    let mut out: Polyline = Vec::new();
    if let Some(a) = sp.anchors.first() {
        out.push(a.p);
    }
    for (a, b) in sp.segments() {
        let c = sp.cubic(a, b);
        for k in 1..=16 {
            out.push(bezier::cubic_at(&c, k as f64 / 16.0));
        }
    }
    out
}
