//! Turns imported shapes into an ordered, machine-independent stitch plan:
//! color blocks made of continuous runs, with trims between runs.
//!
//! Steps: (1) optionally knock out fill hidden under objects sewn later,
//! (2) choose the sewing order (document order, or cap order: center-out and
//! bottom-up), (3) generate each object's stitches, (4) chain them together,
//! sewing short gaps straight across and trimming long ones.

use crate::boolean;
use crate::geom::{Bounds, FillRule, P, Polyline, dedupe, segment_inside};
use crate::params::{Fabric, Kind, defaults, underlay};
use crate::route::Zone;
use crate::stitch::fill::{FillParams, tatami};
use crate::stitch::satin::{self, Rails, SatinParams};
use crate::stitch::stroke::running;
use crate::svg::{Geometry, Shape};
use std::collections::HashMap;

/// A continuous sequence of stitches.
#[derive(Clone, Debug)]
pub struct Run {
    pub pts: Polyline,
    /// Cut the thread before moving to this run's first point.
    pub trim_before: bool,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub color: [u8; 3],
    pub runs: Vec<Run>,
}

#[derive(Clone, Debug, Default)]
pub struct Design {
    pub name: String,
    pub blocks: Vec<Block>,
    /// Stitches each input shape contributed (same order as the input).
    pub per_shape: Vec<usize>,
    /// Input shape indices in the order they're sewn.
    pub sew_order: Vec<usize>,
    /// Problems found while planning: (shape index, message).
    pub notes: Vec<(usize, String)>,
    /// Input shapes sewn in each color block (same order as `blocks`).
    pub block_shapes: Vec<Vec<usize>>,
    /// Input shapes left with nothing to fill: covered by what's sewn on
    /// top (so the checks needn't work the overlaps out again).
    pub covered: Vec<bool>,
    /// Gaps sewn straight across in the open because the document connects
    /// gaps that short (from, to): each would be a cut without it. (Travel
    /// that's out of sight, under a fill or what's sewn on top, isn't here.)
    pub bridges: Vec<(P, P)>,
}

#[derive(Clone, Debug)]
pub struct PlanOptions {
    /// Gaps up to this length (units) are sewn straight across instead of trimmed,
    /// when the straight line stays inside the shape being filled.
    pub travel_inside: f64,
    /// Gaps up to this length (units) between runs are always sewn across.
    pub max_jump_no_trim: f64,
    /// Shortest stitch kept, units. Must stay below fill row spacing, or row
    /// starts get dropped and the edges turn ragged.
    pub min_stitch: f64,
    pub fabric: Fabric,
    /// Don't fill areas that later objects will cover.
    pub remove_overlaps: bool,
    /// How far fills still extend under the covering object, units.
    pub overlap: f64,
    /// Cap order: center-out, bottom-up, fewest color changes.
    pub hat_order: bool,
    /// Gaps up to this long (units), between objects or the separate parts
    /// of one, are sewn across with running stitches rather than cut
    /// (Ink/Stitch's "jump to stroke"), where there's no way round out of
    /// sight. These show: [`Design::bridges`] lists them.
    pub connect: f64,
    /// Travel stitches that stay out of sight (round the inside of a fill's
    /// outline, through it, or under what's sewn later on top) go up to
    /// this far (units); further than that, the thread is cut.
    pub travel_route: f64,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions {
            travel_inside: 60.0,
            max_jump_no_trim: 10.0,
            min_stitch: 1.0,
            fabric: *Fabric::standard(),
            remove_overlaps: true,
            overlap: 5.0,
            hat_order: false,
            connect: 0.0,
            travel_route: 500.0,
        }
    }
}

/// A run plus an equivalent alternative that starts at the other end.
#[derive(Clone)]
struct Piece {
    pts: Polyline,
    alt: Option<Polyline>,
    /// Closed outline (single pass): may start at any vertex.
    ring: bool,
}

impl Piece {
    fn reversible(pts: Polyline) -> Piece {
        let mut alt = pts.clone();
        alt.reverse();
        Piece {
            pts,
            alt: Some(alt),
            ring: false,
        }
    }
    fn fixed(pts: Polyline) -> Piece {
        Piece {
            pts,
            alt: None,
            ring: false,
        }
    }
    /// A closed loop (first point repeated at the end).
    fn ring(pts: Polyline) -> Piece {
        let closed = pts.len() > 3 && pts[0].dist(*pts.last().unwrap()) < 1e-6;
        if !closed {
            return Piece::reversible(pts);
        }
        Piece {
            pts,
            alt: None,
            ring: true,
        }
    }
    /// Distance from `here` to where this piece would start, and that start.
    fn nearest_start(&self, here: P) -> (f64, usize) {
        if self.ring {
            let (i, d) = self.pts[..self.pts.len() - 1]
                .iter()
                .enumerate()
                .map(|(i, p)| (i, here.dist(*p)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            return (d, i);
        }
        (here.dist(self.pts[0]), 0)
    }
}

/// Rotate a closed loop to start (and end) at vertex `i`.
fn rotate_ring(pts: &[P], i: usize) -> Polyline {
    let n = pts.len() - 1;
    let mut out: Polyline = (0..=n).map(|k| pts[(i + k) % n]).collect();
    out[n] = pts[i];
    out
}

/// Runs produced by one shape, in groups whose order must be preserved
/// (e.g. underlay before top stitching). Within a group, pieces may be reordered.
#[derive(Clone)]
struct ShapeRuns {
    groups: Vec<Vec<Piece>>,
    /// For fills: the outline, used to keep travel stitches inside.
    area: Option<(Vec<Polyline>, FillRule)>,
    /// Group indices that start after a machine stop (appliqué steps).
    stops: Vec<usize>,
    /// Groups sewn in a thread other than the object's (tartan stripes,
    /// multicolor satin): (group index, color). Each starts a color block.
    colors: Vec<(usize, [u8; 3])>,
    /// For a fill in several separate parts: which part each piece of each
    /// group is in, so a part's underlay and top are sewn together (rather
    /// than every part's underlay, then back round them all for the top).
    islands: Vec<Vec<usize>>,
}

impl ShapeRuns {
    fn new(groups: Vec<Vec<Piece>>, area: Option<(Vec<Polyline>, FillRule)>) -> Self {
        ShapeRuns {
            groups,
            area,
            stops: Vec::new(),
            colors: Vec::new(),
            islands: Vec::new(),
        }
    }

    /// Work out [`ShapeRuns::islands`] (left empty when the fill is all
    /// one part, or its groups aren't simply underlay and top).
    fn find_islands(&mut self) {
        self.islands.clear();
        let Some((rings, rule)) = &self.area else {
            return;
        };
        if rings.len() < 2
            || !self.stops.is_empty()
            || !self.colors.is_empty()
            || self.groups.iter().filter(|g| !g.is_empty()).count() < 2
        {
            return;
        }
        let parts = boolean::parts(rings, *rule);
        if parts.len() < 2 {
            return;
        }
        let part_of = |p: P| {
            parts
                .iter()
                .position(|part| crate::geom::point_inside(part, FillRule::EvenOdd, p))
                .unwrap_or_else(|| {
                    // On the outline, or just past it: the nearest part.
                    let near = |part: &Vec<Polyline>| {
                        part.iter()
                            .filter(|r| r.len() >= 2)
                            .map(|r| {
                                let mut closed = r.clone();
                                closed.push(r[0]);
                                nearest_along(&closed, p).0
                            })
                            .fold(f64::INFINITY, f64::min)
                    };
                    (0..parts.len())
                        .min_by(|&a, &b| near(&parts[a]).total_cmp(&near(&parts[b])))
                        .unwrap_or(0)
                })
        };
        self.islands = self
            .groups
            .iter()
            .map(|g| {
                g.iter()
                    .map(|piece| part_of(piece.pts[piece.pts.len() / 2]))
                    .collect()
            })
            .collect();
    }
}

/// The point of `ring` nearest `p`: (distance, distance along the ring).
fn nearest_along(ring: &[P], p: P) -> (f64, f64) {
    let (mut best_d, mut best_along, mut along) = (f64::INFINITY, 0.0, 0.0);
    for w in ring.windows(2) {
        let d = w[0].dist(w[1]);
        let t = if d > 0.0 {
            ((p - w[0]).dot(w[1] - w[0]) / (d * d)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let dist = w[0].lerp(w[1], t).dist(p);
        if dist < best_d {
            best_d = dist;
            best_along = along + t * d;
        }
        along += d;
    }
    (best_d, best_along)
}

/// The points of closed `ring` from `a` to `b` (distances along it),
/// forward (`a` to `b`, wrapping) or backward.
fn ring_walk(ring: &[P], a: f64, b: f64, forward: bool) -> Polyline {
    let total = crate::geom::polyline_length(ring);
    let at = |s: f64| {
        let mut want = s.rem_euclid(total);
        for w in ring.windows(2) {
            let d = w[0].dist(w[1]);
            if want <= d {
                return if d > 0.0 {
                    w[0].lerp(w[1], want / d)
                } else {
                    w[0]
                };
            }
            want -= d;
        }
        *ring.last().unwrap()
    };
    let mut out = vec![at(a)];
    // The vertices strictly between a and b, in walking order.
    let mut along = 0.0;
    let mut verts: Vec<(f64, P)> = Vec::new();
    for w in ring.windows(2) {
        along += w[0].dist(w[1]);
        verts.push((along, w[1]));
    }
    let between = |s: f64| {
        if forward {
            if a <= b {
                s > a && s < b
            } else {
                s > a || s < b
            }
        } else if a >= b {
            s < a && s > b
        } else {
            s < a || s > b
        }
    };
    let mut mid: Vec<(f64, P)> = verts.into_iter().filter(|(s, _)| between(*s)).collect();
    if forward {
        // Wrapping: the vertices past a, then those before b.
        mid.sort_by(|x, y| {
            let kx = if x.0 > a { x.0 - a } else { x.0 + total - a };
            let ky = if y.0 > a { y.0 - a } else { y.0 + total - a };
            kx.total_cmp(&ky)
        });
    } else {
        mid.sort_by(|x, y| {
            let kx = if x.0 < a { a - x.0 } else { a + total - x.0 };
            let ky = if y.0 < a { a - y.0 } else { a + total - y.0 };
            kx.total_cmp(&ky)
        });
    }
    out.extend(mid.into_iter().map(|(_, p)| p));
    out.push(at(b));
    out
}

/// Travel stitches from `from` to `to` that stay inside the fill: along
/// the inside of its outline (0.5 mm in), the shorter way round, when
/// that's no longer than `max_len`. The points returned exclude `from`.
pub fn route_inside(
    rings: &[Polyline],
    rule: FillRule,
    from: P,
    to: P,
    max_len: f64,
) -> Option<Polyline> {
    route_along(&travel_ring(rings, rule), from, to, max_len)
}

/// The rings travel stitches follow: the fill's outline, 0.5 mm in.
pub fn travel_ring(rings: &[Polyline], rule: FillRule) -> Vec<Polyline> {
    let mut inset = boolean::offset(rings, rule, -5.0);
    for r in &mut inset {
        if r.len() >= 3 && r.first() != r.last() {
            r.push(r[0]);
        }
    }
    inset
}

/// [`route_inside`] along rings already worked out by [`travel_ring`].
fn route_along(inset: &[Polyline], from: P, to: P, max_len: f64) -> Option<Polyline> {
    // Both ends must be nearest the same inset ring.
    let pick = |p: P| {
        inset
            .iter()
            .enumerate()
            .filter(|(_, r)| r.len() >= 3)
            .map(|(i, r)| {
                let (d, along) = nearest_along(r, p);
                (i, d, along)
            })
            .min_by(|x, y| x.1.total_cmp(&y.1))
    };
    let (ia, _, a) = pick(from)?;
    let (ib, _, b) = pick(to)?;
    if ia != ib {
        return None;
    }
    let ring = &inset[ia];
    let total = crate::geom::polyline_length(ring);
    let fwd = (b - a).rem_euclid(total);
    let forward = fwd <= total - fwd;
    let mut path = ring_walk(ring, a, b, forward);
    let len =
        from.dist(path[0]) + crate::geom::polyline_length(&path) + path.last().unwrap().dist(to);
    if len > max_len {
        return None;
    }
    path.push(to);
    // Stitches every 3 mm along it.
    let mut out = Vec::new();
    let mut prev = from;
    for q in path {
        let d = prev.dist(q);
        let n = (d / 30.0).ceil().max(1.0) as usize;
        for k in 1..=n {
            out.push(prev.lerp(q, k as f64 / n as f64));
        }
        prev = q;
    }
    Some(out)
}

/// What a shape's stitches cover (none, for a run), once it's been asked.
type Cover = Option<(Vec<Polyline>, FillRule)>;

/// A fill's rows no further apart than this (mm) hide travel under them.
const HIDING_SPACING: f64 = 1.0;

/// Where travel to and within the shape sewn at `order[pos]` is out of
/// sight: under its own fill (`own`, when its stitches hide what's under
/// them) and under whatever's sewn later on top. `from` is where the
/// needle's coming from: taken in when it's within `reach` (the last
/// object, close by).
#[allow(clippy::too_many_arguments)]
fn hidden_zone(
    shapes: &[Shape],
    order: &[usize],
    pos: usize,
    own: Option<&(Vec<Polyline>, FillRule)>,
    from: P,
    reach: f64,
    covers: &mut [Option<Cover>],
    shape_bounds: &mut Option<Vec<Bounds>>,
) -> Option<Zone> {
    let bounds = shape_bounds.get_or_insert_with(|| {
        shapes
            .iter()
            .map(|s| geometry_bounds(&s.geometry))
            .collect()
    });
    let mut b = match own {
        Some((rings, _)) => {
            let mut b = Bounds::empty();
            rings.iter().flatten().for_each(|&p| b.add(p));
            b
        }
        None => bounds[order[pos]],
    };
    if b.is_empty() {
        return None;
    }
    if from.x > b.min.x - reach
        && from.x < b.max.x + reach
        && from.y > b.min.y - reach
        && from.y < b.max.y + reach
    {
        b.add(from);
    }
    // Room to go round the outside of a part, under what covers it there.
    let grow = |b: &Bounds, by: f64| Bounds {
        min: P::new(b.min.x - by, b.min.y - by),
        max: P::new(b.max.x + by, b.max.y + by),
    };
    let b = grow(&b, 20.0);
    // (A satin's bounds are its centerline's: its width reaches further.)
    let near = grow(&b, 50.0);
    let mut later: Vec<usize> = Vec::new();
    for &j in &order[pos + 1..] {
        if bounds[j].is_empty() || !overlaps(&near, &bounds[j]) {
            continue;
        }
        if covers[j]
            .get_or_insert_with(|| coverage(&shapes[j]))
            .is_some()
        {
            later.push(j);
        }
    }
    let mut parts: Vec<(&[Polyline], FillRule)> = Vec::new();
    if let Some((rings, rule)) = own {
        parts.push((rings.as_slice(), *rule));
    }
    for j in later {
        if let Some(Some((rings, rule))) = &covers[j] {
            parts.push((rings.as_slice(), *rule));
        }
    }
    if parts.is_empty() {
        return None;
    }
    Zone::new(&parts, b)
}

/// The outline a shape's start and end points are measured along: a
/// fill's biggest ring (closed), or a stroke's first line.
fn outline_of(g: &Geometry) -> Option<std::borrow::Cow<'_, Polyline>> {
    use std::borrow::Cow;
    match g {
        Geometry::Fill { rings, .. } => {
            let r = rings
                .iter()
                .filter(|r| r.len() >= 2)
                .max_by(|a, b| boolean::ring_area(a).total_cmp(&boolean::ring_area(b)))?;
            if r.first() == r.last() {
                Some(Cow::Borrowed(r))
            } else {
                let mut c = r.clone();
                c.push(r[0]);
                Some(Cow::Owned(c))
            }
        }
        Geometry::Stroke { lines, .. } => {
            lines.iter().find(|(l, _)| l.len() >= 2).map(|(l, closed)| {
                if *closed && l.first() != l.last() {
                    let mut c = l.clone();
                    c.push(l[0]);
                    Cow::Owned(c)
                } else {
                    Cow::Borrowed(l)
                }
            })
        }
    }
}

/// The point `t` (0–1) of the way along a shape's outline (where its start
/// or end point is; see `Params::start`).
pub fn outline_at(g: &Geometry, t: f64) -> Option<P> {
    let pts = outline_of(g)?;
    let total = crate::geom::polyline_length(&pts);
    if total <= 0.0 {
        return pts.first().copied();
    }
    let mut want = t.rem_euclid(1.0) * total;
    for w in pts.windows(2) {
        let d = w[0].dist(w[1]);
        if want <= d {
            return Some(if d > 0.0 {
                w[0].lerp(w[1], want / d)
            } else {
                w[0]
            });
        }
        want -= d;
    }
    pts.last().copied()
}

/// Where along a shape's outline (0–1) the point nearest `p` is: the
/// inverse of [`outline_at`].
pub fn outline_param(g: &Geometry, p: P) -> Option<f64> {
    let pts = outline_of(g)?;
    let total = crate::geom::polyline_length(&pts);
    if total <= 0.0 {
        return Some(0.0);
    }
    let (mut best, mut best_d, mut along) = (0.0, f64::INFINITY, 0.0);
    for w in pts.windows(2) {
        let d = w[0].dist(w[1]);
        let t = if d > 0.0 {
            ((p - w[0]).dot(w[1] - w[0]) / (d * d)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let q = w[0].lerp(w[1], t);
        let dist = q.dist(p);
        if dist < best_d {
            best_d = dist;
            best = along + t * d;
        }
        along += d;
    }
    Some((best / total).clamp(0.0, 0.9999))
}

fn mm(v: f64) -> f64 {
    v * 10.0
}

/// The satin rails a shape will be sewn with, or why a part couldn't be.
/// Parts that fail fall back to fill (returned as `Err` with the component).
pub fn satin_rails(shape: &Shape) -> Vec<Result<Rails, (Vec<Polyline>, String)>> {
    let p = &shape.params;
    match &shape.geometry {
        Geometry::Stroke { lines, width } => {
            let w = p.width.map(mm).unwrap_or(*width);
            if let Some(r) = satin::rails_from_subpaths(lines) {
                return vec![Ok(r)];
            }
            // (A subpath without length, a dot drawn as `M p L p`, has no
            // column to sew.)
            lines
                .iter()
                .map(|(l, c)| satin::rails_from_centerline(l, *c, w))
                .filter(|r| r.a.len() > 1)
                .map(Ok)
                .collect()
        }
        Geometry::Fill { rings, rule } => {
            if p.is_border() {
                let w = p.width.map(mm).unwrap_or(mm(defaults::OUTLINE_SATIN_WIDTH));
                return rings
                    .iter()
                    .map(|r| satin::rails_from_centerline(r, true, w))
                    .filter(|r| r.a.len() > 1)
                    .map(Ok)
                    .collect();
            }
            satin::components(rings, *rule)
                .into_iter()
                .map(|comp| satin::auto_rails(&comp).map_err(|e| (comp, e.to_string())))
                .collect()
        }
    }
}

fn mean_width(r: &Rails) -> f64 {
    let ps = satin::pairs(r, 10.0);
    ps.iter().map(|(a, b)| a.dist(*b)).sum::<f64>() / ps.len().max(1) as f64
}

fn fill_runs(
    rings: &[Polyline],
    rule: FillRule,
    shape: &Shape,
    opt: &PlanOptions,
) -> Vec<Vec<Piece>> {
    let p = &shape.params;
    let f = &opt.fabric;
    let angle = p.fill_angle().to_radians();
    let mut groups = Vec::new();
    if p.underlay_on() {
        if f.fill_edge_walk {
            let inset = boolean::offset(rings, rule, -5.0);
            let walk: Vec<Piece> = inset
                .iter()
                .filter(|r| r.len() >= 3 && boolean::ring_area(r) > 100.0)
                .map(|r| Piece::reversible(running(r, true, mm(2.0), 1)))
                .collect();
            if !walk.is_empty() {
                groups.push(walk);
            }
        }
        let under = FillParams {
            angle: angle + std::f64::consts::FRAC_PI_2,
            spacing: mm(f.fill_underlay_spacing),
            len: mm(3.0),
            inset: mm(0.4),
            pull: 0.0,
            stagger: 1,
            bottom_up: opt.hat_order,
        };
        let u: Vec<Piece> = tatami(rings, rule, &under)
            .into_iter()
            .map(Piece::reversible)
            .collect();
        if !u.is_empty() {
            groups.push(u);
        }
    }
    let top = FillParams {
        angle,
        spacing: mm(p.fill_spacing(f)),
        len: mm(p.fill_len(f)),
        inset: 0.0,
        pull: mm(p.fill_pull(f)),
        stagger: 4,
        bottom_up: opt.hat_order,
    };
    groups.push(
        tatami(rings, rule, &top)
            .into_iter()
            .map(Piece::reversible)
            .collect(),
    );
    groups
}

fn satin_pieces(
    shape: &Shape,
    opt: &PlanOptions,
    notes: &mut Vec<String>,
) -> (Vec<Piece>, Vec<Vec<Piece>>) {
    satin_pieces_strip(shape, opt, notes, 0.0, 1.0)
}

/// [`satin_pieces`] for the strip of the column between fractions `t0`
/// and `t1` of its width (rail a at 0, rail b at 1): a multicolor satin
/// is several of these.
fn satin_pieces_strip(
    shape: &Shape,
    opt: &PlanOptions,
    notes: &mut Vec<String>,
    t0: f64,
    t1: f64,
) -> (Vec<Piece>, Vec<Vec<Piece>>) {
    let p = &shape.params;
    let f = &opt.fabric;
    let mut pieces = Vec::new();
    let mut fallback = Vec::new();
    let whole = t0 <= 0.0 && t1 >= 1.0;
    for r in satin_rails(shape) {
        match r {
            Ok(rails) => {
                let rails = if whole {
                    rails
                } else {
                    satin::strip(&rails, t0, t1)
                };
                let w = mean_width(&rails);
                let sp = SatinParams {
                    spacing: mm(p.satin_spacing_for(f, w / 10.0)),
                    pull: mm(p.satin_pull(f)),
                    underlay: p.satin_underlay_for(w / 10.0),
                    run_len: mm(2.5),
                    short_stitches: p.short_stitches_on(),
                    split: p.satin_split().map(mm),
                    style: p.satin_style(),
                };
                let fwd = satin::column(&rails, &sp);
                let alt = (!rails.closed).then(|| {
                    let rev = Rails {
                        a: rails.a.iter().rev().copied().collect(),
                        b: rails.b.iter().rev().copied().collect(),
                        closed: false,
                        sync: {
                            let (la, lb) = (
                                crate::geom::polyline_length(&rails.a),
                                crate::geom::polyline_length(&rails.b),
                            );
                            rails
                                .sync
                                .iter()
                                .rev()
                                .map(|&(a, b)| (la - a, lb - b))
                                .collect()
                        },
                    };
                    satin::column(&rev, &sp)
                });
                pieces.push(Piece {
                    pts: fwd,
                    alt,
                    ring: false,
                });
            }
            Err((comp, why)) => {
                notes.push(format!("Part of this shape {why}; it's filled instead."));
                fallback.extend(fill_runs(&comp, FillRule::EvenOdd, shape, opt));
            }
        }
    }
    (pieces, fallback)
}

/// Concentric rows following the outline, outside in.
fn contour_runs(
    rings: &[Polyline],
    rule: FillRule,
    shape: &Shape,
    opt: &PlanOptions,
) -> Vec<Vec<Piece>> {
    let p = &shape.params;
    let sp = mm(p.contour_spacing()).max(2.0);
    let len = mm(p.run_len(&opt.fabric));
    let mut groups = Vec::new();
    let mut d = sp * 0.5;
    for _ in 0..1000 {
        let level: Vec<Piece> = boolean::offset(rings, rule, -d)
            .iter()
            .filter(|r| r.len() >= 3 && boolean::ring_area(r) > sp * sp)
            .map(|r| Piece::ring(running(r, true, len, 1)))
            .collect();
        if level.is_empty() {
            break;
        }
        groups.push(level);
        d += sp;
    }
    groups
}

/// One spiral out from the shape's center (its start point, if set), the
/// turns `spacing` apart, clipped to the shape: Ink/Stitch's circular
/// fill. The pieces outside the shape are dropped; the planner joins what's
/// left inside it.
fn spiral_runs(
    rings: &[Polyline],
    rule: FillRule,
    shape: &Shape,
    opt: &PlanOptions,
) -> Vec<Vec<Piece>> {
    let p = &shape.params;
    let sp = mm(p.contour_spacing()).max(2.0);
    let len = mm(p.run_len(&opt.fabric)).max(5.0);
    let b = geometry_bounds(&shape.geometry);
    let center = p
        .start
        .and_then(|t| outline_at(&shape.geometry, t))
        .unwrap_or_else(|| {
            // The centroid of the biggest ring.
            rings
                .iter()
                .max_by(|a, c| boolean::ring_area(a).total_cmp(&boolean::ring_area(c)))
                .map(|r| {
                    let n = r.len().max(1) as f64;
                    r.iter().fold(P::default(), |acc, q| acc + *q) * (1.0 / n)
                })
                .unwrap_or(b.center())
        });
    let reach = [
        b.min,
        P::new(b.max.x, b.min.y),
        b.max,
        P::new(b.min.x, b.max.y),
    ]
    .iter()
    .map(|c| c.dist(center))
    .fold(0.0, f64::max)
        + sp;
    // Archimedean spiral, points about `len` apart along it.
    let mut pts: Polyline = vec![center];
    let mut theta: f64 = 0.0;
    while theta * sp / std::f64::consts::TAU <= reach {
        let r = theta * sp / std::f64::consts::TAU;
        let step = (len / r.max(len)).min(std::f64::consts::FRAC_PI_3);
        theta += step;
        let r = theta * sp / std::f64::consts::TAU;
        pts.push(center + P::new(r * theta.cos(), r * theta.sin()));
    }
    // Keep the parts inside the shape.
    let mut pieces = Vec::new();
    let mut cur: Polyline = Vec::new();
    for w in pts.windows(2) {
        if crate::geom::segment_inside(rings, rule, w[0], w[1], 2.0)
            && crate::geom::point_inside(rings, rule, w[0])
            && crate::geom::point_inside(rings, rule, w[1])
        {
            if cur.is_empty() {
                cur.push(w[0]);
            }
            cur.push(w[1]);
        } else if cur.len() >= 2 {
            pieces.push(Piece::reversible(std::mem::take(&mut cur)));
        } else {
            cur.clear();
        }
    }
    if cur.len() >= 2 {
        pieces.push(Piece::reversible(cur));
    }
    if pieces.is_empty() {
        return Vec::new();
    }
    vec![pieces]
}

/// Blanket (E-stitch): a running line with prongs of `width` every `spacing`,
/// on the `side` (+1 left of travel, -1 right).
pub fn blanket(line: &[P], closed: bool, spacing: f64, width: f64, side: f64) -> Polyline {
    let mut base: Polyline = line.to_vec();
    if closed && base.len() > 2 {
        base.push(base[0]);
    }
    let samples = crate::geom::sample_by_arclength(&base, spacing);
    let mut out = Vec::with_capacity(samples.len() * 3);
    for (k, (q, t)) in samples.iter().enumerate() {
        out.push(*q);
        if closed && k == samples.len() - 1 {
            break;
        }
        out.push(*q + t.perp() * (width * side));
        out.push(*q);
    }
    out
}

fn blanket_pieces(shape: &Shape, geometry: &Geometry) -> Vec<Piece> {
    let p = &shape.params;
    let sp = mm(p.spacing_set().unwrap_or(defaults::BLANKET_SPACING));
    let w = mm(p.width.unwrap_or(defaults::BLANKET_WIDTH));
    match geometry {
        Geometry::Fill { rings, rule } => rings
            .iter()
            .filter(|r| r.len() >= 3)
            .map(|r| {
                // Prongs point into the shape.
                let probe = crate::geom::sample_by_arclength(r, 20.0);
                let side = probe
                    .get(probe.len() / 2)
                    .map(|(q, t)| {
                        if crate::geom::point_inside(rings, *rule, *q + t.perp() * 3.0) {
                            1.0
                        } else {
                            -1.0
                        }
                    })
                    .unwrap_or(1.0);
                Piece::fixed(blanket(r, true, sp, w, side))
            })
            .collect(),
        Geometry::Stroke { lines, .. } => lines
            .iter()
            .map(|(l, c)| Piece::fixed(blanket(l, *c, sp, w, 1.0)))
            .collect(),
    }
}

/// Appliqué: placement line, stop, tack-down, stop, satin cover.
fn applique_runs(rings: &[Polyline], shape: &Shape, opt: &PlanOptions) -> ShapeRuns {
    let p = &shape.params;
    let f = &opt.fabric;
    let closed: Vec<&Polyline> = rings.iter().filter(|r| r.len() >= 3).collect();
    let placement: Vec<Piece> = closed
        .iter()
        .map(|r| Piece::ring(running(r, true, mm(2.5), 1)))
        .collect();
    let tack_params = SatinParams {
        spacing: 25.0,
        pull: 0.0,
        underlay: 0,
        run_len: 25.0,
        short_stitches: false,
        split: None,
        style: Default::default(),
    };
    let tack: Vec<Piece> = closed
        .iter()
        .map(|r| {
            Piece::fixed(satin::column(
                &satin::rails_from_centerline(r, true, 12.0),
                &tack_params,
            ))
        })
        .collect();
    let w = mm(p.width.unwrap_or(defaults::APPLIQUE_WIDTH));
    let cover_params = SatinParams {
        spacing: mm(p.satin_spacing_for(f, w / 10.0)),
        pull: mm(p.satin_pull(f)),
        underlay: if p.underlay_on() { underlay::CENTER } else { 0 },
        run_len: mm(2.5),
        short_stitches: p.short_stitches_on(),
        split: None,
        style: Default::default(),
    };
    let cover: Vec<Piece> = closed
        .iter()
        .map(|r| {
            Piece::fixed(satin::column(
                &satin::rails_from_centerline(r, true, w),
                &cover_params,
            ))
        })
        .collect();
    ShapeRuns {
        groups: vec![placement, tack, cover],
        area: None,
        stops: vec![1, 2],
        colors: Vec::new(),
        islands: Vec::new(),
    }
}

/// The decorative fills (see `stitch::patterns`): each a group of pieces
/// the planner joins, plus a tartan's per-colour groups.
fn pattern_runs(
    rings: &[Polyline],
    rule: FillRule,
    shape: &Shape,
    kind: Kind,
    opt: &PlanOptions,
) -> ShapeRuns {
    use crate::stitch::patterns;
    let p = &shape.params;
    let f = &opt.fabric;
    let len = mm(p.run_len(f));
    let area = Some((rings.to_vec(), rule));
    match kind {
        Kind::Meander => {
            let seed = shape.uid as u64 + 1;
            let lines =
                patterns::meander(rings, rule, mm(p.meander_spacing()).max(10.0), len, seed);
            ShapeRuns::new(
                vec![lines.into_iter().map(Piece::reversible).collect()],
                area,
            )
        }
        Kind::Ripple => {
            let rows = patterns::ripple(
                rings,
                rule,
                mm(p.ripple_spacing()).max(5.0),
                len,
                p.spiral == Some(true),
            );
            let pieces = rows
                .into_iter()
                .map(|r| {
                    if p.spiral == Some(true) {
                        Piece::reversible(r)
                    } else {
                        Piece::ring(r)
                    }
                })
                .collect();
            ShapeRuns::new(vec![pieces], area)
        }
        Kind::Cross => {
            let rows = patterns::cross_stitch(rings, rule, mm(p.cross_cell()).max(10.0));
            ShapeRuns::new(
                vec![rows.into_iter().map(Piece::reversible).collect()],
                area,
            )
        }
        Kind::Tartan => {
            let sett: Vec<patterns::Stripe> = p
                .tartan_sett(shape.color)
                .into_iter()
                .map(|(color, w)| patterns::Stripe {
                    color,
                    width: mm(w),
                })
                .collect();
            let per = patterns::tartan(
                rings,
                rule,
                &sett,
                mm(p.fill_spacing(f)),
                mm(p.fill_len(f)),
                p.fill_angle().to_radians(),
            );
            let mut sr = ShapeRuns::new(Vec::new(), area);
            for (gi, (color, rows)) in per.into_iter().enumerate() {
                sr.groups
                    .push(rows.into_iter().map(Piece::reversible).collect());
                sr.colors.push((gi, color));
            }
            sr
        }
        _ => ShapeRuns::new(Vec::new(), None),
    }
}

fn manual_pieces(geometry: &Geometry) -> Vec<Vec<Piece>> {
    let closed_loop = |l: &Polyline| {
        let mut v = l.clone();
        if v.len() > 2 {
            v.push(v[0]);
        }
        v
    };
    // One group per path: manual stitching keeps its order exactly.
    match geometry {
        Geometry::Fill { rings, .. } => rings
            .iter()
            .map(|r| vec![Piece::fixed(closed_loop(r))])
            .collect(),
        Geometry::Stroke { lines, .. } => lines
            .iter()
            .map(|(l, c)| vec![Piece::fixed(if *c { closed_loop(l) } else { l.clone() })])
            .collect(),
    }
}

/// [`shape_runs`], but an object whose stitching hits an internal error
/// (a panic in the fill, satin or boolean code on some odd outline) is
/// left out with a note, rather than taking the whole plan (and the app,
/// and the recovered document on the next launch) down with it.
fn shape_runs_guarded(
    shape: &Shape,
    geometry: &Geometry,
    opt: &PlanOptions,
    notes: &mut Vec<String>,
) -> ShapeRuns {
    let mut tried = Vec::new();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shape_runs(shape, geometry, opt, &mut tried)
    }));
    match r {
        Ok(mut sr) => {
            notes.append(&mut tried);
            // A piece without stitches (a subpath without length) has no
            // start or end to route to. The groups stay: stops count them.
            for g in &mut sr.groups {
                g.retain(|p| !p.pts.is_empty());
            }
            if shape.params.kind.unwrap_or(Kind::Fill) == Kind::Fill {
                sr.find_islands();
            }
            sr
        }
        Err(e) => {
            let what = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown error".into());
            notes.push(format!(
                "Couldn't work out this object's stitches ({what}); it's left out. \
                 Try another stitch type, Object › Path › Clean Up, or Simplify."
            ));
            ShapeRuns::new(Vec::new(), None)
        }
    }
}

fn shape_runs(
    shape: &Shape,
    geometry: &Geometry,
    opt: &PlanOptions,
    notes: &mut Vec<String>,
) -> ShapeRuns {
    let p = &shape.params;
    let f = &opt.fabric;
    let kind = p.kind.unwrap_or(Kind::Fill);
    match (geometry, kind) {
        (_, Kind::Skip) => ShapeRuns::new(Vec::new(), None),
        (_, Kind::Manual) => ShapeRuns::new(manual_pieces(geometry), None),
        (_, Kind::Blanket) => ShapeRuns::new(vec![blanket_pieces(shape, geometry)], None),
        (Geometry::Fill { rings, rule }, Kind::Fill) => ShapeRuns::new(
            fill_runs(rings, *rule, shape, opt),
            Some((rings.clone(), *rule)),
        ),
        (Geometry::Fill { rings, rule }, Kind::Contour) => ShapeRuns::new(
            if p.spiral == Some(true) {
                spiral_runs(rings, *rule, shape, opt)
            } else {
                contour_runs(rings, *rule, shape, opt)
            },
            Some((rings.clone(), *rule)),
        ),
        (Geometry::Fill { rings, .. }, Kind::Applique) => applique_runs(rings, shape, opt),
        (
            Geometry::Fill { rings, rule },
            Kind::Meander | Kind::Ripple | Kind::Cross | Kind::Tartan,
        ) => pattern_runs(rings, *rule, shape, kind, opt),
        (_, Kind::Satin) => {
            let area = match geometry {
                Geometry::Fill { rings, rule } => Some((rings.clone(), *rule)),
                _ => None,
            };
            if let Some(colors) = p.colors.as_ref().filter(|c| c.len() >= 2) {
                // Multicolor: the column as side-by-side strips, one per
                // thread, each its own color group.
                let mut sr = ShapeRuns::new(Vec::new(), area);
                let n = colors.len();
                for (k, &color) in colors.iter().enumerate() {
                    let (t0, t1) = (k as f64 / n as f64, (k + 1) as f64 / n as f64);
                    let (pieces, fallback) = satin_pieces_strip(shape, opt, notes, t0, t1);
                    let mut groups = fallback;
                    if !pieces.is_empty() {
                        groups.push(pieces);
                    }
                    for g in groups {
                        sr.colors.push((sr.groups.len(), color));
                        sr.groups.push(g);
                    }
                }
                return sr;
            }
            let (pieces, mut fallback) = satin_pieces(shape, opt, notes);
            if !pieces.is_empty() {
                fallback.push(pieces);
            }
            ShapeRuns::new(fallback, area)
        }
        (Geometry::Fill { rings, .. }, _) => {
            // Running stitch along each outline.
            let pieces = rings
                .iter()
                .map(|r| {
                    let run = running(r, true, mm(p.run_len(f)), p.run_repeats());
                    if p.run_repeats() == 1 {
                        Piece::ring(run)
                    } else {
                        Piece::reversible(run)
                    }
                })
                .collect();
            ShapeRuns::new(vec![pieces], None)
        }
        (Geometry::Stroke { lines, .. }, _) => {
            let pieces = lines
                .iter()
                .map(|(l, c)| {
                    let run = running(l, *c, mm(p.run_len(f)), p.run_repeats());
                    if *c && p.run_repeats() == 1 {
                        Piece::ring(run)
                    } else {
                        Piece::reversible(run)
                    }
                })
                .collect();
            ShapeRuns::new(vec![pieces], None)
        }
    }
}

/// Area a shape's stitches will cover, for overlap removal (None for runs).
pub fn coverage(shape: &Shape) -> Option<(Vec<Polyline>, FillRule)> {
    match (shape.params.kind.unwrap_or(Kind::Fill), &shape.geometry) {
        (Kind::Fill | Kind::Contour | Kind::Applique, Geometry::Fill { rings, rule }) => {
            Some((rings.clone(), *rule))
        }
        (Kind::Satin, _) => {
            let mut rings = Vec::new();
            for r in satin_rails(shape) {
                match r {
                    Ok(rails) if rails.closed => {
                        rings.push(rails.a.clone());
                        rings.push(rails.b.clone());
                    }
                    Ok(rails) => {
                        let mut ring = rails.a.clone();
                        ring.extend(rails.b.iter().rev());
                        rings.push(ring);
                    }
                    Err((comp, _)) => rings.extend(comp),
                }
            }
            Some((rings, FillRule::EvenOdd))
        }
        _ => None,
    }
}

fn geometry_bounds(g: &Geometry) -> Bounds {
    let mut b = Bounds::empty();
    match g {
        Geometry::Fill { rings, .. } => rings.iter().flatten().for_each(|&p| b.add(p)),
        Geometry::Stroke { lines, width } => {
            lines.iter().flat_map(|(l, _)| l).for_each(|&p| b.add(p));
            if !b.is_empty() {
                b.add(b.min - P::new(width / 2.0, width / 2.0));
                b.add(b.max + P::new(width / 2.0, width / 2.0));
            }
        }
    }
    b
}

fn overlaps(a: &Bounds, b: &Bounds) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && a.min.x <= b.max.x
        && b.min.x <= a.max.x
        && a.min.y <= b.max.y
        && b.min.y <= a.max.y
}

/// Sewing order: document order, or cap order (center-out, bottom-up, grouping
/// colors) that still sews anything underneath an object before the object.
fn sew_order(shapes: &[Shape], opt: &PlanOptions) -> Vec<usize> {
    let n = shapes.len();
    if !opt.hat_order {
        return (0..n).collect();
    }
    let bounds: Vec<Bounds> = shapes
        .iter()
        .map(|s| geometry_bounds(&s.geometry))
        .collect();
    let mut all = Bounds::empty();
    for b in &bounds {
        if !b.is_empty() {
            all.add(b.min);
            all.add(b.max);
        }
    }
    let bottom = all.max.y;
    // deps[j] = shapes that must come before j (earlier in the document and overlapping).
    let deps: Vec<Vec<usize>> = (0..n)
        .map(|j| {
            (0..j)
                .filter(|&i| overlaps(&bounds[i], &bounds[j]))
                .collect()
        })
        .collect();
    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut color: Option<[u8; 3]> = None;
    while order.len() < n {
        let pick = (0..n)
            .filter(|&j| !done[j] && deps[j].iter().all(|&i| done[i]))
            .min_by(|&a, &b| {
                let key = |j: usize| {
                    let c = bounds[j].center();
                    let change = (color.is_some() && Some(shapes[j].color) != color) as u8;
                    (change, c.x.abs() + 0.5 * (bottom - c.y))
                };
                let (ka, kb) = (key(a), key(b));
                ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
            })
            .expect("dependencies only point backwards, so something is always available");
        done[pick] = true;
        color = Some(shapes[pick].color);
        order.push(pick);
    }
    order
}

/// Geometry each shape is actually stitched with, after overlap removal.
pub fn effective_geometry(shapes: &[Shape], order: &[usize], opt: &PlanOptions) -> Vec<Geometry> {
    effective_geometry_cached(shapes, order, opt, &mut HashMap::new())
}

/// [`effective_geometry`], reusing `cache` (keyed by what each result
/// depends on) for every shape whose outline, and the outlines and stitch
/// types of what's sewn over it, haven't changed. Moving one object redoes
/// only the objects it overlaps, not the whole design.
fn effective_geometry_cached(
    shapes: &[Shape],
    order: &[usize],
    opt: &PlanOptions,
    cache: &mut HashMap<u64, Geometry>,
) -> Vec<Geometry> {
    use std::hash::{Hash, Hasher};
    let mut out: Vec<Geometry> = shapes.iter().map(|s| s.geometry.clone()).collect();
    if !opt.remove_overlaps {
        return out;
    }
    let bounds: Vec<Bounds> = shapes
        .iter()
        .map(|s| geometry_bounds(&s.geometry))
        .collect();
    // What each shape covers (a satin's coverage comes from its rails, so
    // it's worked out only for shapes that are actually on top of another),
    // normalized once for all the shapes under it.
    struct Cover {
        rings: Vec<Polyline>,
        bounds: Bounds,
        /// `rings` simplified, for where it's within reach whole.
        simple: Option<Vec<Polyline>>,
    }
    let mut cover: Vec<Option<Option<Cover>>> = (0..shapes.len()).map(|_| None).collect();
    let geo_keys: Vec<u64> = shapes.iter().map(|s| geometry_key(&s.geometry)).collect();
    let cover_keys: Vec<u64> = shapes
        .iter()
        .zip(&geo_keys)
        .map(|(s, k)| {
            let mut h = hasher();
            k.hash(&mut h);
            let kind = s.params.kind.unwrap_or(Kind::Fill);
            kind.hash(&mut h);
            if kind == Kind::Satin {
                format!("{:?}", s.params).hash(&mut h);
            }
            h.finish()
        })
        .collect();
    let mut used = std::collections::HashSet::new();
    for (pos, &i) in order.iter().enumerate() {
        let s = &shapes[i];
        let Geometry::Fill { rings, rule } = &s.geometry else {
            continue;
        };
        if !matches!(
            s.params.kind.unwrap_or(Kind::Fill),
            Kind::Fill | Kind::Contour
        ) {
            continue;
        }
        let on_top: Vec<usize> = order[pos + 1..]
            .iter()
            .copied()
            .filter(|&j| overlaps(&bounds[i], &bounds[j]))
            .collect();
        if on_top.is_empty() {
            continue;
        }
        let key = {
            let mut h = hasher();
            geo_keys[i].hash(&mut h);
            (*rule == FillRule::EvenOdd).hash(&mut h);
            opt.overlap.to_bits().hash(&mut h);
            for &j in &on_top {
                cover_keys[j].hash(&mut h);
            }
            h.finish()
        };
        used.insert(key);
        if let Some(g) = cache.get(&key) {
            out[i] = g.clone();
            continue;
        }
        // Only what's on top within reach of this shape matters (clipping
        // first gives the same result, faster), and shrinking an outline
        // traced to a few hundredths of a millimetre by half a millimetre
        // doesn't need every one of its points.
        let reach = opt.overlap.abs() + 2.0;
        let (lo, hi) = (
            P::new(bounds[i].min.x - reach, bounds[i].min.y - reach),
            P::new(bounds[i].max.x + reach, bounds[i].max.y + reach),
        );
        let mut above: Vec<Vec<Polyline>> = Vec::new();
        for &j in &on_top {
            let c = cover[j].get_or_insert_with(|| {
                coverage(&shapes[j]).map(|(r, f)| {
                    let rings = boolean::normalize(&r, f);
                    let mut bounds = Bounds::empty();
                    rings.iter().flatten().for_each(|&p| bounds.add(p));
                    Cover {
                        rings,
                        bounds,
                        simple: None,
                    }
                })
            });
            let Some(c) = c else { continue };
            let b = &c.bounds;
            if b.is_empty() || b.min.x > hi.x || b.max.x < lo.x || b.min.y > hi.y || b.max.y < lo.y
            {
                continue;
            }
            let within = b.min.x >= lo.x && b.max.x <= hi.x && b.min.y >= lo.y && b.max.y <= hi.y;
            let piece = if within {
                c.simple
                    .get_or_insert_with(|| boolean::simplify_rings(&c.rings, 0.5))
                    .clone()
            } else {
                boolean::simplify_rings(&boolean::clip_normalized_to_rect(&c.rings, lo, hi), 0.5)
            };
            if !piece.is_empty() {
                above.push(piece);
            }
        }
        if above.is_empty() {
            cache.insert(key, out[i].clone());
            continue;
        }
        let covered = boolean::simplify_rings(&boolean::union_normalized(&above), 0.5);
        let shrunk = boolean::offset(&covered, FillRule::EvenOdd, -opt.overlap);
        if shrunk.is_empty() {
            cache.insert(key, out[i].clone());
            continue;
        }
        let remaining: Vec<Polyline> = boolean::difference(rings, *rule, &shrunk)
            .into_iter()
            .filter(|r| r.len() >= 3 && boolean::ring_area(r) > 150.0)
            .collect();
        out[i] = Geometry::Fill {
            rings: remaining,
            rule: FillRule::EvenOdd,
        };
        cache.insert(key, out[i].clone());
    }
    // Forget shapes no longer there (or no longer under the same things).
    cache.retain(|k, _| used.contains(k));
    out
}

/// What planning keeps between runs, so a change re-plans only what it
/// touched: the overlap removal (each shape's redone only when its outline,
/// or the outlines and stitch types of what's sewn over it, change) and
/// each shape's stitches (redone only when that shape, its settings, or
/// what's sewn over it change).
#[derive(Default)]
pub struct PlanCache {
    overlaps: Option<(u64, Vec<Geometry>)>,
    /// Each shape's geometry after overlap removal, by what it depends on.
    overlap_shapes: HashMap<u64, Geometry>,
    runs: std::collections::HashMap<u64, (ShapeRuns, Vec<String>)>,
}

fn hasher() -> std::collections::hash_map::DefaultHasher {
    std::collections::hash_map::DefaultHasher::new()
}

/// A fingerprint of a geometry's every coordinate.
pub(crate) fn geometry_key(g: &Geometry) -> u64 {
    use std::hash::{Hash, Hasher};
    fn eat(h: &mut impl Hasher, pts: &[P]) {
        pts.len().hash(h);
        for p in pts {
            p.x.to_bits().hash(h);
            p.y.to_bits().hash(h);
        }
    }
    let mut h = hasher();
    match g {
        Geometry::Fill { rings, rule } => {
            0u8.hash(&mut h);
            (*rule == FillRule::EvenOdd).hash(&mut h);
            for r in rings {
                eat(&mut h, r);
            }
        }
        Geometry::Stroke { lines, width } => {
            1u8.hash(&mut h);
            width.to_bits().hash(&mut h);
            for (l, closed) in lines {
                closed.hash(&mut h);
                eat(&mut h, l);
            }
        }
    }
    h.finish()
}

/// Build the stitch plan. Consecutive shapes of the same color share a color block.
pub fn plan(name: &str, shapes: &[Shape], opt: &PlanOptions) -> Design {
    plan_cached(name, shapes, opt, &mut PlanCache::default())
}

/// [`plan`], reusing what `cache` kept from the last run where nothing it
/// depends on changed.
pub fn plan_cached(
    name: &str,
    shapes: &[Shape],
    opt: &PlanOptions,
    cache: &mut PlanCache,
) -> Design {
    use std::hash::{Hash, Hasher};
    let order = sew_order(shapes, opt);
    let geo_keys: Vec<u64> = shapes.iter().map(|s| geometry_key(&s.geometry)).collect();
    let opt_key = format!("{opt:?}");
    // Overlap removal depends on outlines, stitch types (a satin's coverage
    // on its settings too), the sewing order and the overlap options.
    let overlap_key = {
        let mut h = hasher();
        order.hash(&mut h);
        opt.remove_overlaps.hash(&mut h);
        opt.overlap.to_bits().hash(&mut h);
        for (s, k) in shapes.iter().zip(&geo_keys) {
            k.hash(&mut h);
            let kind = s.params.kind.unwrap_or(Kind::Fill);
            format!("{kind:?}").hash(&mut h);
            if kind == Kind::Satin {
                format!("{:?}", s.params).hash(&mut h);
            }
        }
        h.finish()
    };
    let geometry = match &cache.overlaps {
        Some((k, g)) if *k == overlap_key && g.len() == shapes.len() => g.clone(),
        _ => {
            let g = effective_geometry_cached(shapes, &order, opt, &mut cache.overlap_shapes);
            cache.overlaps = Some((overlap_key, g.clone()));
            g
        }
    };
    let mut used = std::collections::HashSet::new();
    let mut design = Design {
        name: name.to_string(),
        blocks: Vec::new(),
        per_shape: vec![0; shapes.len()],
        sew_order: order.clone(),
        notes: Vec::new(),
        block_shapes: Vec::new(),
        covered: shapes
            .iter()
            .zip(&geometry)
            .map(|(s, g)| {
                matches!(
                    (&s.geometry, g),
                    (Geometry::Fill { rings: a, .. }, Geometry::Fill { rings: b, .. })
                        if !a.is_empty() && b.is_empty()
                )
            })
            .collect(),
        bridges: Vec::new(),
    };
    // What each shape covers and where it is, worked out when travel first
    // looks for cover.
    let mut covers: Vec<Option<Cover>> = vec![None; shapes.len()];
    let mut shape_bounds: Option<Vec<Bounds>> = None;
    let mut cursor: Option<P> = None;
    // Set by the last object: cut before the next run, or pause before
    // the next object.
    let mut force_trim = false;
    let mut stop_next = false;
    // The last run sewn belongs to the object being sewn, so its outline
    // can be used to route travel within the same shape.
    let mut same_shape = false;
    for (pos, &si) in order.iter().enumerate() {
        let shape = &shapes[si];
        if shape.params.kind == Some(Kind::Skip) {
            continue;
        }
        // The fill's travel ring, worked out when a gap first needs it.
        let mut travel: Option<Vec<Polyline>> = None;
        // Likewise where travel is out of sight.
        let mut zone: Option<Option<Zone>> = None;
        // Under a fill's own stitches, if they're close enough to hide it.
        let own_hides = shape.params.kind.unwrap_or(Kind::Fill) == Kind::Fill
            && shape.params.fill_spacing(&opt.fabric) <= HIDING_SPACING;
        let start_at = shape
            .params
            .start
            .and_then(|t| outline_at(&shape.geometry, t));
        let end_at = shape
            .params
            .end
            .and_then(|t| outline_at(&shape.geometry, t));
        // This shape's stitches: kept from last time when its outline, what's
        // left of it after overlaps, its settings and the options are the same.
        let key = {
            let mut h = hasher();
            geo_keys[si].hash(&mut h);
            geometry_key(&geometry[si]).hash(&mut h);
            format!("{:?}{:?}", shape.params, shape.part).hash(&mut h);
            // A tartan's default sett comes from the thread color, and a
            // meander's seed from the uid.
            shape.color.hash(&mut h);
            shape.uid.hash(&mut h);
            opt_key.hash(&mut h);
            h.finish()
        };
        used.insert(key);
        let (sr, notes) = match cache.runs.get(&key) {
            Some(hit) => hit.clone(),
            None => {
                let mut notes = Vec::new();
                let sr = shape_runs_guarded(shape, &geometry[si], opt, &mut notes);
                cache.runs.insert(key, (sr.clone(), notes.clone()));
                (sr, notes)
            }
        };
        design.notes.extend(notes.into_iter().map(|n| (si, n)));
        if sr.groups.iter().all(|g| g.is_empty()) {
            continue;
        }
        if design.blocks.last().is_none_or(|b| b.color != shape.color)
            || (stop_next && design.blocks.last().is_some_and(|b| !b.runs.is_empty()))
        {
            design.blocks.push(Block {
                color: shape.color,
                runs: Vec::new(),
            });
            design.block_shapes.push(Vec::new());
        }
        let last_group = sr.groups.iter().rposition(|g| !g.is_empty()).unwrap_or(0);
        // The groups in sewing order: one after another, or (a fill in
        // separate parts, with no start or end point to honour) part by
        // part, each part's groups together, nearest part next.
        let mut queue: std::collections::VecDeque<(usize, Vec<Piece>)> =
            std::collections::VecDeque::new();
        let mut parts: Vec<Vec<(usize, Vec<Piece>)>> = Vec::new();
        if !sr.islands.is_empty() && start_at.is_none() && end_at.is_none() {
            for (gi, (group, of)) in sr.groups.into_iter().zip(&sr.islands).enumerate() {
                for (piece, &k) in group.into_iter().zip(of) {
                    if parts.len() <= k {
                        parts.resize_with(k + 1, Vec::new);
                    }
                    match parts[k].last_mut() {
                        Some((g, pieces)) if *g == gi => pieces.push(piece),
                        _ => parts[k].push((gi, vec![piece])),
                    }
                }
            }
            parts.retain(|p| !p.is_empty());
        } else {
            queue.extend(sr.groups.into_iter().enumerate());
        }
        loop {
            if queue.is_empty() && !parts.is_empty() {
                let next = cursor.map_or(0, |here| {
                    let reach = |part: &Vec<(usize, Vec<Piece>)>| {
                        part[0]
                            .1
                            .iter()
                            .map(|piece| {
                                let d = piece.nearest_start(here).0;
                                piece.alt.as_ref().map_or(d, |alt| d.min(here.dist(alt[0])))
                            })
                            .fold(f64::INFINITY, f64::min)
                    };
                    (0..parts.len())
                        .min_by(|&a, &b| reach(&parts[a]).total_cmp(&reach(&parts[b])))
                        .unwrap_or(0)
                });
                queue.extend(parts.swap_remove(next));
            }
            let Some((gi, mut group)) = queue.pop_front() else {
                break;
            };
            // The group's thread: the object's, unless the group names one.
            let group_color = sr
                .colors
                .iter()
                .find(|(g, _)| *g == gi)
                .map(|(_, c)| *c)
                .unwrap_or(shape.color);
            let stop_here = sr.stops.contains(&gi);
            let last_nonempty = design.blocks.last().is_some_and(|b| !b.runs.is_empty());
            if (stop_here && last_nonempty)
                || design.blocks.last().is_none_or(|b| b.color != group_color)
            {
                // A machine stop (same thread, new block) or a change of thread.
                design.blocks.push(Block {
                    color: group_color,
                    runs: Vec::new(),
                });
                design.block_shapes.push(Vec::new());
            }
            let bs = design.block_shapes.last_mut().unwrap();
            if !bs.contains(&si) {
                bs.push(si);
            }
            let block = design.blocks.last_mut().unwrap();
            // The object's end point: in its last group, the piece that can
            // end nearest it is kept for last, and sewn so it does.
            let mut ender = (gi == last_group).then_some(end_at).flatten().map(|e| {
                group
                    .iter()
                    .enumerate()
                    .map(|(i, piece)| {
                        let d = piece.pts.last().map_or(f64::INFINITY, |q| q.dist(e));
                        let d_alt = piece
                            .alt
                            .as_ref()
                            .and_then(|a| a.last())
                            .map_or(f64::INFINITY, |q| q.dist(e));
                        if piece.ring {
                            // A ring ends where it starts: nearest vertex to e.
                            (i, piece.nearest_start(e).0, false)
                        } else {
                            (i, d.min(d_alt), d_alt < d)
                        }
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _, alt)| (i, alt))
                    .unwrap_or((0, false))
            });
            let mut first = gi == 0;
            // Greedy nearest-neighbour ordering within the group.
            while !group.is_empty() {
                // The object's start point stands in for where the needle is
                // when choosing its first piece.
                let here_is_start = first && start_at.is_some();
                let here = match (first, start_at, cursor) {
                    (true, Some(s), _) => s,
                    (_, _, Some(c)) => c,
                    _ => group[0].pts[0],
                };
                first = false;
                let (mut best, mut best_d, mut best_alt, mut best_at) =
                    (0, f64::INFINITY, false, 0);
                let from_start = here_is_start;
                let finishing = group.len() == 1 && ender.is_some() && !from_start;
                let held = if group.len() > 1 {
                    ender.map(|(i, _)| i)
                } else {
                    None
                };
                for (i, piece) in group.iter().enumerate() {
                    if held == Some(i) {
                        continue;
                    }
                    if finishing {
                        // Whichever way ends nearest the end point.
                        let (_, alt) = ender.unwrap();
                        (best, best_alt, best_at) = (i, alt && piece.alt.is_some(), 0);
                        if piece.ring
                            && let Some(e) = end_at
                        {
                            best_at = piece.nearest_start(e).1;
                        }
                        break;
                    }
                    let (d0, at) = piece.nearest_start(here);
                    if d0 < best_d {
                        (best, best_d, best_alt, best_at) = (i, d0, false, at);
                    }
                    if let Some(alt) = &piece.alt {
                        let d1 = here.dist(alt[0]);
                        if d1 < best_d {
                            (best, best_d, best_alt, best_at) = (i, d1, true, 0);
                        }
                    }
                }
                let last_piece = group.len() == 1;
                let piece = group.swap_remove(best);
                // (That moved the last piece into the gap: the one kept
                // for the end, if it was last.)
                if let Some((held, _)) = &mut ender
                    && *held == group.len()
                {
                    *held = best;
                }
                let mut pts = if best_alt {
                    piece.alt.unwrap()
                } else if piece.ring && best_at > 0 {
                    rotate_ring(&piece.pts, best_at)
                } else {
                    piece.pts
                };
                dedupe(&mut pts, opt.min_stitch);
                if pts.is_empty() {
                    continue;
                }
                // Begin at the start point: a few stitches from it to the
                // first row, hidden under the fill (a satin or run just
                // starts at its nearest end).
                if from_start
                    && let Some(st) = start_at
                    && let Some((rings, rule)) = sr.area.as_ref()
                    && st.dist(pts[0]) > opt.min_stitch
                    && segment_inside(rings, *rule, st, pts[0], 2.0)
                {
                    let n = (st.dist(pts[0]) / 30.0).ceil() as usize;
                    let lead: Vec<P> = (0..n)
                        .map(|k| st.lerp(pts[0], k as f64 / n as f64))
                        .collect();
                    pts.splice(0..0, lead);
                }
                // Finish at the end point: a few stitches on to it.
                if last_piece
                    && gi == last_group
                    && let Some(e) = end_at
                    && let Some(&q) = pts.last()
                    && q.dist(e) > opt.min_stitch
                {
                    let n = (q.dist(e) / 30.0).ceil() as usize;
                    for k in 1..=n {
                        pts.push(q.lerp(e, k as f64 / n as f64));
                    }
                }
                design.per_shape[si] += pts.len();
                let start = pts[0];
                let mut trim_before = false;
                if let (Some(prev), Some(run)) = (cursor, block.runs.last_mut()) {
                    let gap = prev.dist(start);
                    let inside = sr.area.as_ref().is_some_and(|(rings, rule)| {
                        gap <= opt.travel_inside && segment_inside(rings, *rule, prev, start, 2.0)
                    });
                    let straight = || -> Vec<P> {
                        let n = (gap / 30.0).ceil() as usize;
                        (1..n)
                            .map(|i| prev.lerp(start, i as f64 / n as f64))
                            .collect()
                    };
                    // The stitches from the last run to this one, when the
                    // thread needn't be cut between them.
                    let mut across: Option<Vec<P>> = None;
                    if force_trim {
                        // Cut here, whatever's near.
                    } else if gap <= opt.max_jump_no_trim || inside {
                        // Too short to matter, or straight across the fill.
                        across = Some(straight());
                    } else {
                        // Within a fill: round the inside of its outline
                        // (Ink/Stitch's underpath).
                        if same_shape
                            && opt.travel_route > 0.0
                            && let Some((rings, rule)) = sr.area.as_ref()
                        {
                            across = route_along(
                                travel.get_or_insert_with(|| travel_ring(rings, *rule)),
                                prev,
                                start,
                                opt.travel_route,
                            );
                        }
                        // Or any way that's out of sight: through the fill
                        // (round its holes, through its necks) and under
                        // what's sewn later on top.
                        if across.is_none() && opt.travel_route > 0.0 {
                            across = zone
                                .get_or_insert_with(|| {
                                    hidden_zone(
                                        shapes,
                                        &order,
                                        pos,
                                        sr.area.as_ref().filter(|_| own_hides),
                                        prev,
                                        opt.travel_route,
                                        &mut covers,
                                        &mut shape_bounds,
                                    )
                                })
                                .as_ref()
                                .and_then(|z| z.route(prev, start, opt.travel_route));
                        }
                        // Or in the open, where the document says gaps this
                        // short are sewn across.
                        if across.is_none() && gap <= opt.connect {
                            design.bridges.push((prev, start));
                            across = Some(straight());
                        }
                    }
                    if let Some(across) = across {
                        run.pts
                            .extend(across.into_iter().filter(|q| q.dist(start) > 1e-6));
                        run.pts.extend(pts.iter().copied());
                        cursor = run.pts.last().copied();
                        force_trim = false;
                        same_shape = true;
                        continue;
                    }
                    trim_before = true;
                }
                force_trim = false;
                same_shape = true;
                cursor = pts.last().copied();
                block.runs.push(Run { pts, trim_before });
            }
        }
        force_trim = shape.params.trim == Some(true);
        stop_next = shape.params.stop == Some(true);
        same_shape = false;
    }
    // Forget shapes no longer there (or no longer like that).
    cache.runs.retain(|k, _| used.contains(k));
    let keep: Vec<bool> = design.blocks.iter().map(|b| !b.runs.is_empty()).collect();
    let mut k = keep.iter();
    design.block_shapes.retain(|_| *k.next().unwrap());
    design.blocks.retain(|b| !b.runs.is_empty());
    design
}

/// A sewing order with fewer color changes: objects of the current color go
/// first, but nothing moves above an object it overlaps that was meant to be
/// on top. Returns shape indices.
pub fn grouped_by_color(shapes: &[Shape]) -> Vec<usize> {
    let n = shapes.len();
    let bounds: Vec<Bounds> = shapes
        .iter()
        .map(|s| geometry_bounds(&s.geometry))
        .collect();
    let deps: Vec<Vec<usize>> = (0..n)
        .map(|j| {
            (0..j)
                .filter(|&i| overlaps(&bounds[i], &bounds[j]))
                .collect()
        })
        .collect();
    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let mut color: Option<[u8; 3]> = None;
    while order.len() < n {
        let ready = |j: usize| !done[j] && deps[j].iter().all(|&i| done[i]);
        let pick = (0..n)
            .find(|&j| ready(j) && Some(shapes[j].color) == color)
            .or_else(|| (0..n).find(|&j| ready(j)))
            .expect("dependencies only point backwards");
        done[pick] = true;
        color = Some(shapes[pick].color);
        order.push(pick);
    }
    order
}

/// Default satin width (mm) for a shape: its stroke width, or the outline default.
pub fn default_satin_width(shape: &Shape) -> f64 {
    match &shape.geometry {
        Geometry::Stroke { width, .. } => width / 10.0,
        Geometry::Fill { .. } => defaults::OUTLINE_SATIN_WIDTH,
    }
}

impl Design {
    pub fn bounds(&self) -> Bounds {
        let mut b = Bounds::empty();
        for blk in &self.blocks {
            for r in &blk.runs {
                for &p in &r.pts {
                    b.add(p);
                }
            }
        }
        b
    }

    /// Move the design so its bounding box is centered on the origin.
    pub fn center(&mut self) {
        let c = self.bounds().center();
        for blk in &mut self.blocks {
            for r in &mut blk.runs {
                for p in &mut r.pts {
                    *p = *p - c;
                }
            }
        }
        for (a, b) in &mut self.bridges {
            (*a, *b) = (*a - c, *b - c);
        }
    }

    pub fn stitch_count(&self) -> usize {
        self.blocks
            .iter()
            .flat_map(|b| &b.runs)
            .map(|r| r.pts.len())
            .sum()
    }

    pub fn trim_count(&self) -> usize {
        self.blocks
            .iter()
            .flat_map(|b| &b.runs)
            .filter(|r| r.trim_before)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Params;
    use crate::svg::Part;

    fn shape(geometry: Geometry, kind: Kind, color: [u8; 3]) -> Shape {
        Shape {
            id: String::new(),
            part: Part::Fill,
            label: String::new(),
            color,
            paint: color,
            geometry,
            params: Params {
                kind: Some(kind),
                underlay: Some(false),
                ..Params::default()
            },
            uid: 0,
            groups: Vec::new(),
        }
    }

    fn sq(x: f64, y: f64, w: f64) -> Polyline {
        vec![
            P::new(x, y),
            P::new(x + w, y),
            P::new(x + w, y + w),
            P::new(x, y + w),
        ]
    }

    fn fill(r: Polyline) -> Geometry {
        Geometry::Fill {
            rings: vec![r],
            rule: FillRule::NonZero,
        }
    }

    fn same_stitches(a: &Design, b: &Design) -> bool {
        a.blocks.len() == b.blocks.len()
            && a.blocks.iter().zip(&b.blocks).all(|(x, y)| {
                x.color == y.color
                    && x.runs.len() == y.runs.len()
                    && x.runs.iter().zip(&y.runs).all(|(r, s)| {
                        r.trim_before == s.trim_before
                            && r.pts.len() == s.pts.len()
                            && r.pts.iter().zip(&s.pts).all(|(p, q)| p.dist(*q) < 1e-9)
                    })
            })
    }

    /// Overlap removal is cached per shape: after moving one object, the
    /// cached plan must still equal a plan made from scratch (what moved
    /// and what it overlapped are redone; the rest is reused).
    #[test]
    fn cached_overlap_removal_matches_a_fresh_plan_after_a_move() {
        let grid = |dx: f64| -> Vec<Shape> {
            let mut shapes = Vec::new();
            for k in 0..40 {
                let (x, y) = ((k % 8) as f64 * 90.0, (k / 8) as f64 * 90.0);
                let c = [(k * 37 % 256) as u8, 60, 120];
                shapes.push(shape(fill(sq(x, y, 100.0)), Kind::Fill, c));
            }
            // One on top of the first row, to be moved along it.
            shapes.push(shape(
                fill(sq(40.0 + dx, 40.0, 120.0)),
                Kind::Fill,
                [200, 10, 10],
            ));
            shapes
        };
        let opt = PlanOptions {
            remove_overlaps: true,
            ..PlanOptions::default()
        };
        let mut cache = PlanCache::default();
        let first = plan_cached("t", &grid(0.0), &opt, &mut cache);
        assert!(same_stitches(&first, &plan("t", &grid(0.0), &opt)));
        let moved = grid(150.0);
        let t = std::time::Instant::now();
        let cached = plan_cached("t", &moved, &opt, &mut cache);
        let warm = t.elapsed();
        let t = std::time::Instant::now();
        let fresh = plan("t", &moved, &opt);
        let cold = t.elapsed();
        assert!(same_stitches(&cached, &fresh), "the cached plan differs");
        eprintln!("warm replan {warm:?}, cold {cold:?}");
        // And once nothing has changed, nothing is redone.
        let again = plan_cached("t", &moved, &opt, &mut cache);
        assert!(same_stitches(&again, &fresh));
    }

    #[test]
    fn grouping_colors_keeps_layers() {
        let red = [200, 0, 0];
        let blue = [0, 0, 200];
        // red A, blue B (on top of A), red C (apart), blue D (apart).
        let a = shape(fill(sq(0.0, 0.0, 100.0)), Kind::Fill, red);
        let b = shape(fill(sq(50.0, 50.0, 100.0)), Kind::Fill, blue);
        let c = shape(fill(sq(500.0, 0.0, 50.0)), Kind::Fill, red);
        let d = shape(fill(sq(800.0, 0.0, 50.0)), Kind::Fill, blue);
        let order = grouped_by_color(&[a.clone(), b.clone(), c.clone(), d.clone()]);
        assert_eq!(order, vec![0, 2, 1, 3]);
        // C on top of B must stay after it.
        let c2 = shape(fill(sq(120.0, 120.0, 50.0)), Kind::Fill, red);
        assert_eq!(grouped_by_color(&[a, b, c2, d]), vec![0, 1, 3, 2]);
    }

    #[test]
    fn contour_fill_rows_follow_the_outline_inside() {
        let s = shape(fill(sq(0.0, 0.0, 200.0)), Kind::Contour, [0, 0, 0]);
        let d = plan("t", &[s], &PlanOptions::default());
        let pts: Vec<P> = d
            .blocks
            .iter()
            .flat_map(|b| &b.runs)
            .flat_map(|r| r.pts.iter().copied())
            .collect();
        assert!(
            pts.iter()
                .all(|p| p.x >= -0.5 && p.x <= 200.5 && p.y >= -0.5 && p.y <= 200.5)
        );
        // ~0.45 mm rows across a 20 mm square: about 22 concentric rings, sewn in one piece.
        let near_center = pts
            .iter()
            .filter(|p| p.dist(P::new(100.0, 100.0)) < 15.0)
            .count();
        assert!(near_center > 0, "rows reach the middle");
        assert_eq!(d.trim_count(), 0, "rings connect without trims");
        assert!(d.stitch_count() > 300, "{}", d.stitch_count());
    }

    #[test]
    fn ring_walks_follow_the_ring_both_ways_round() {
        let n = 36;
        let ring: Polyline = (0..=n)
            .map(|k| {
                let a = k as f64 / n as f64 * std::f64::consts::TAU;
                P::new(100.0 + 100.0 * a.cos(), 100.0 + 100.0 * a.sin())
            })
            .collect();
        let total = crate::geom::polyline_length(&ring);
        let edge = ring[0].dist(ring[1]);
        for (a, b) in [
            (0.1 * total, 0.4 * total),
            (0.4 * total, 0.1 * total),
            (0.95 * total, 0.05 * total),
            (0.05 * total, 0.95 * total),
            (0.5 * total, 0.5 * total + 1.0),
        ] {
            for forward in [true, false] {
                let w = ring_walk(&ring, a, b, forward);
                for q in w.windows(2) {
                    assert!(
                        q[0].dist(q[1]) <= edge + 1e-6,
                        "{a} -> {b} forward={forward}: jump {:?} -> {:?}",
                        q[0],
                        q[1]
                    );
                }
                let want = if forward {
                    (b - a).rem_euclid(total)
                } else {
                    (a - b).rem_euclid(total)
                };
                let len = crate::geom::polyline_length(&w);
                assert!(
                    (len - want).abs() < 1.0,
                    "{a} -> {b} forward={forward}: {len} vs {want}"
                );
            }
        }
    }

    #[test]
    fn spiral_fill_winds_out_from_the_center_inside_the_shape() {
        let mut s = shape(fill(sq(0.0, 0.0, 200.0)), Kind::Contour, [0, 0, 0]);
        s.params.spiral = Some(true);
        let d = plan("t", &[s], &PlanOptions::default());
        let pts: Vec<P> = d
            .blocks
            .iter()
            .flat_map(|b| &b.runs)
            .flat_map(|r| r.pts.iter().copied())
            .collect();
        assert!(pts.len() > 300, "{}", pts.len());
        assert!(
            pts.iter()
                .all(|p| p.x >= -0.5 && p.x <= 200.5 && p.y >= -0.5 && p.y <= 200.5)
        );
        assert!(
            pts[0].dist(P::new(100.0, 100.0)) < 5.0,
            "starts at the center: {:?}",
            pts[0]
        );
        // Winds out: distance from the center grows along the stitches.
        let d0 = pts[20].dist(P::new(100.0, 100.0));
        let d1 = pts[pts.len() / 2].dist(P::new(100.0, 100.0));
        assert!(d1 > d0 + 20.0, "{d0} {d1}");
        // Inside a square, the spiral is clipped at the corners into a few
        // pieces joined by travel, not cuts.
        assert_eq!(d.trim_count(), 0);
    }

    #[test]
    fn decorative_fills_stay_inside_and_tartan_splits_into_threads() {
        for kind in [Kind::Meander, Kind::Ripple, Kind::Cross] {
            let s = shape(fill(sq(0.0, 0.0, 200.0)), kind, [0, 0, 0]);
            let d = plan("t", &[s], &PlanOptions::default());
            let pts: Vec<P> = d
                .blocks
                .iter()
                .flat_map(|b| &b.runs)
                .flat_map(|r| r.pts.iter().copied())
                .collect();
            assert!(pts.len() > 50, "{kind:?}: {}", pts.len());
            assert!(
                pts.iter()
                    .all(|p| p.x >= -0.5 && p.x <= 200.5 && p.y >= -0.5 && p.y <= 200.5),
                "{kind:?} leaves the shape"
            );
            assert_eq!(d.blocks.len(), 1);
            // A tatami fill of the same square is denser than any of these.
            let t = plan(
                "t",
                &[shape(fill(sq(0.0, 0.0, 200.0)), Kind::Fill, [0, 0, 0])],
                &PlanOptions::default(),
            );
            assert!(
                d.stitch_count() < t.stitch_count(),
                "{kind:?} is lighter than tatami"
            );
        }
        let mut s = shape(fill(sq(0.0, 0.0, 300.0)), Kind::Tartan, [200, 0, 0]);
        s.params.sett = Some(vec![([200, 0, 0], 4.0), ([0, 0, 200], 2.0)]);
        let d = plan("t", &[s], &PlanOptions::default());
        assert_eq!(d.blocks.len(), 2, "one block per stripe colour");
        assert_eq!(d.blocks[0].color, [200, 0, 0]);
        assert_eq!(d.blocks[1].color, [0, 0, 200]);
        assert_eq!(d.block_shapes, vec![vec![0], vec![0]]);
    }

    #[test]
    fn multicolor_satin_and_styles() {
        // A 4 mm wide bar as a satin column.
        let bar = fill(vec![
            P::new(0.0, 0.0),
            P::new(300.0, 0.0),
            P::new(300.0, 40.0),
            P::new(0.0, 40.0),
        ]);
        let mut s = shape(bar.clone(), Kind::Satin, [0, 0, 0]);
        let plain = plan("t", &[s.clone()], &PlanOptions::default());
        assert_eq!(plain.blocks.len(), 1);
        s.params.colors = Some(vec![[255, 0, 0], [0, 0, 255]]);
        let d = plan("t", &[s.clone()], &PlanOptions::default());
        assert_eq!(d.blocks.len(), 2, "two threads across the column");
        assert_eq!(d.blocks[0].color, [255, 0, 0]);
        // The red strip is the top half (y in 0..20), the blue the bottom.
        let red_y: Vec<f64> = d.blocks[0]
            .runs
            .iter()
            .flat_map(|r| r.pts.iter().map(|p| p.y))
            .collect();
        let blue_y: Vec<f64> = d.blocks[1]
            .runs
            .iter()
            .flat_map(|r| r.pts.iter().map(|p| p.y))
            .collect();
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
        assert!(
            (mean(&red_y) - mean(&blue_y)).abs() > 12.0,
            "{} {}",
            mean(&red_y),
            mean(&blue_y)
        );
        // Zigzag is lighter than plain; S-stitch has more points.
        let mut z = shape(bar.clone(), Kind::Satin, [0, 0, 0]);
        z.params.style = Some(crate::stitch::satin::SatinStyle::Zigzag);
        let mut ss = shape(bar, Kind::Satin, [0, 0, 0]);
        ss.params.style = Some(crate::stitch::satin::SatinStyle::SStitch);
        let (zp, sp) = (
            plan("t", &[z], &PlanOptions::default()),
            plan("t", &[ss], &PlanOptions::default()),
        );
        assert!(
            zp.stitch_count() < plain.stitch_count() / 2,
            "{} vs {}",
            zp.stitch_count(),
            plain.stitch_count()
        );
        assert!(
            sp.stitch_count() > plain.stitch_count(),
            "{} vs {}",
            sp.stitch_count(),
            plain.stitch_count()
        );
    }

    #[test]
    fn blanket_prongs_point_into_the_shape() {
        let s = shape(fill(sq(0.0, 0.0, 200.0)), Kind::Blanket, [0, 0, 0]);
        let d = plan("t", &[s], &PlanOptions::default());
        let pts: Vec<P> = d.blocks[0]
            .runs
            .iter()
            .flat_map(|r| r.pts.iter().copied())
            .collect();
        let inner = pts
            .iter()
            .filter(|p| p.x > 10.0 && p.x < 190.0 && p.y > 10.0 && p.y < 190.0)
            .count();
        assert!(inner > 20, "prong tips land 2.5 mm inside: {inner}");
        assert!(
            pts.iter()
                .all(|p| p.x > -1.0 && p.y > -1.0 && p.x < 201.0 && p.y < 201.0),
            "none point outward"
        );
        // Reversed ring direction gives the same inward prongs.
        let mut ring = sq(0.0, 0.0, 200.0);
        ring.reverse();
        let d2 = plan(
            "t",
            &[shape(fill(ring), Kind::Blanket, [0, 0, 0])],
            &PlanOptions::default(),
        );
        assert!(
            d2.blocks[0]
                .runs
                .iter()
                .flat_map(|r| &r.pts)
                .all(|p| p.x > -1.0 && p.y > -1.0 && p.x < 201.0 && p.y < 201.0)
        );
    }

    #[test]
    fn applique_stops_twice_in_the_same_thread() {
        let s = shape(fill(sq(0.0, 0.0, 300.0)), Kind::Applique, [10, 20, 30]);
        let d = plan("t", &[s], &PlanOptions::default());
        assert_eq!(d.blocks.len(), 3, "placement | tack-down | cover");
        assert!(d.blocks.iter().all(|b| b.color == [10, 20, 30]));
        let n: Vec<usize> = d
            .blocks
            .iter()
            .map(|b| b.runs.iter().map(|r| r.pts.len()).sum())
            .collect();
        assert!(
            n[0] < n[1] && n[1] < n[2],
            "placement is lightest, cover heaviest: {n:?}"
        );
        let pes = crate::formats::pes::write_pes(&d);
        let back = crate::formats::read::read_stitch_file(&pes, "pes").unwrap();
        assert_eq!(back.blocks.len(), 3, "machine stops survive export");
    }

    #[test]
    fn start_and_end_points_place_the_first_and_last_stitch() {
        // A 20 mm square: start at the top-left corner (t = 0), end at the
        // bottom-right (t = 0.5).
        let mut sh = shape(fill(sq(0.0, 0.0, 200.0)), Kind::Fill, [0; 3]);
        let plain = plan("p", &[sh.clone()], &PlanOptions::default());
        sh.params.start = Some(0.0);
        sh.params.end = Some(0.5);
        let d = plan("p", &[sh.clone()], &PlanOptions::default());
        let first = d.blocks[0].runs[0].pts[0];
        let last = *d.blocks[0].runs.last().unwrap().pts.last().unwrap();
        assert!(
            first.dist(P::new(0.0, 0.0)) < 15.0,
            "starts near the corner: {first:?}"
        );
        assert!(
            last.dist(P::new(200.0, 200.0)) < 2.0,
            "ends at the corner: {last:?}"
        );
        assert_eq!(outline_at(&sh.geometry, 0.5), Some(P::new(200.0, 200.0)));
        assert!((outline_param(&sh.geometry, P::new(205.0, 205.0)).unwrap() - 0.5).abs() < 1e-6);
        // The other way round: the same rows, sewn from the other corner.
        sh.params.start = Some(0.5);
        sh.params.end = Some(0.0);
        let d = plan("p", &[sh.clone()], &PlanOptions::default());
        let first = d.blocks[0].runs[0].pts[0];
        assert!(first.dist(P::new(200.0, 200.0)) < 15.0, "{first:?}");
        assert!(d.stitch_count() >= plain.stitch_count());
    }

    /// The piece kept back to end on stays kept back while the others are
    /// sewn (taking one out of the list moves the last one into its place).
    #[test]
    fn the_end_point_piece_is_sewn_last_among_several() {
        // A 30 mm square with a hole: rows above, either side of and below
        // the hole. The rows below finish at the bottom-left corner, so
        // ending there, they go last.
        let mut sh = shape(
            Geometry::Fill {
                rings: vec![sq(0.0, 0.0, 300.0), sq(100.0, 100.0, 100.0)],
                rule: FillRule::EvenOdd,
            },
            Kind::Fill,
            [0; 3],
        );
        let corner = P::new(0.0, 300.0);
        sh.params.end = outline_param(&sh.geometry, corner);
        let d = plan("p", &[sh], &PlanOptions::default());
        let pts: Vec<P> = d.blocks[0]
            .runs
            .iter()
            .flat_map(|r| r.pts.clone())
            .collect();
        assert!(pts.last().unwrap().dist(corner) < 2.0, "ends at the corner");
        let below = pts.iter().position(|q| q.y > 230.0).expect("rows below");
        assert!(
            pts[below..].iter().all(|q| q.y > 195.0),
            "once below the hole, it stays there to the end"
        );
    }

    #[test]
    fn a_subpath_without_length_is_left_out_not_fatal() {
        let dot = (vec![P::new(50.0, 50.0), P::new(50.0, 50.0)], false);
        let line = (vec![P::new(0.0, 0.0), P::new(200.0, 0.0)], false);
        for kind in [Kind::Satin, Kind::Blanket, Kind::Run] {
            let both = shape(
                Geometry::Stroke {
                    lines: vec![dot.clone(), line.clone()],
                    width: 30.0,
                },
                kind,
                [0; 3],
            );
            let only_dot = shape(
                Geometry::Stroke {
                    lines: vec![dot.clone()],
                    width: 30.0,
                },
                kind,
                [0; 3],
            );
            let shapes = [both, only_dot];
            let opt = PlanOptions::default();
            let d = plan("p", &shapes, &opt);
            assert!(d.notes.is_empty(), "{kind:?}: {:?}", d.notes);
            assert!(d.per_shape[0] > 0, "{kind:?}: the line is sewn");
            // The checks read the same rails.
            crate::checks::check(&shapes, &d, &opt, None);
        }
    }

    #[test]
    fn a_zero_spacing_or_length_is_floored_not_endless() {
        for kind in [Kind::Fill, Kind::Satin, Kind::Run, Kind::Contour] {
            let mut sh = shape(fill(sq(-20.0, -20.0, 40.0)), kind, [0; 3]);
            sh.params.spacing = Some(0.0);
            sh.params.len = Some(0.0);
            let d = plan("p", &[sh], &PlanOptions::default());
            assert!(d.stitch_count() < 200_000, "{kind:?}");
        }
    }

    #[test]
    fn a_new_thread_color_replans_a_tartan_from_the_cache() {
        let mut sh = shape(fill(sq(0.0, 0.0, 200.0)), Kind::Tartan, [200, 30, 30]);
        let opt = PlanOptions::default();
        let mut cache = PlanCache::default();
        plan_cached("p", &[sh.clone()], &opt, &mut cache);
        sh.color = [30, 30, 200];
        let cached = plan_cached("p", &[sh.clone()], &opt, &mut cache);
        assert!(same_stitches(&cached, &plan("p", &[sh], &opt)));
    }

    #[test]
    fn trim_stop_and_connect_between_objects() {
        let a = shape(fill(sq(0.0, 0.0, 100.0)), Kind::Fill, [0; 3]);
        let b = shape(fill(sq(130.0, 0.0, 100.0)), Kind::Fill, [0; 3]);
        // 3 mm apart: cut by default.
        let d = plan("p", &[a.clone(), b.clone()], &PlanOptions::default());
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.trim_count(), 1);
        // Connect gaps up to 15 mm: sewn across, no cut.
        let opt = PlanOptions {
            connect: 150.0,
            ..PlanOptions::default()
        };
        let d = plan("p", &[a.clone(), b.clone()], &opt);
        assert_eq!(d.trim_count(), 0, "sewn across the gap");
        assert_eq!(d.blocks[0].runs.len(), 1);
        // Trim after A cuts even though they'd connect.
        let mut at = a.clone();
        at.params.trim = Some(true);
        let d = plan("p", &[at, b.clone()], &opt);
        assert_eq!(d.trim_count(), 1);
        // Stop after A: a second block in the same thread.
        let mut ast = a.clone();
        ast.params.stop = Some(true);
        let d = plan("p", &[ast, b.clone()], &PlanOptions::default());
        assert_eq!(d.blocks.len(), 2);
        assert_eq!(d.blocks[0].color, d.blocks[1].color);
        assert_eq!(d.block_shapes, vec![vec![0], vec![1]]);
    }

    /// One object in two separate 10 mm squares, `gap` apart.
    fn two_squares(gap: f64) -> Shape {
        shape(
            Geometry::Fill {
                rings: vec![sq(0.0, 0.0, 100.0), sq(100.0 + gap, 0.0, 100.0)],
                rule: FillRule::NonZero,
            },
            Kind::Fill,
            [0; 3],
        )
    }

    /// Every point along the thread of `run` (each stitch, and between).
    fn along(run: &Run) -> Vec<P> {
        run.pts
            .windows(2)
            .flat_map(|w| {
                let n = w[0].dist(w[1]).ceil().max(1.0) as usize;
                (0..=n).map(move |i| w[0].lerp(w[1], i as f64 / n as f64))
            })
            .collect()
    }

    #[test]
    fn each_part_of_a_fill_is_sewn_whole_before_the_next() {
        let mut s = two_squares(200.0);
        s.params.underlay = Some(true);
        let d = plan("p", &[s.clone()], &PlanOptions::default());
        // One cut, between the parts: not one after the first's underlay
        // and another back for its top.
        assert_eq!(d.trim_count(), 1);
        let runs = &d.blocks[0].runs;
        assert_eq!(runs.len(), 2);
        let left = |r: &Run| r.pts.iter().all(|p| p.x < 150.0);
        let right = |r: &Run| r.pts.iter().all(|p| p.x > 150.0);
        assert!(
            (left(&runs[0]) && right(&runs[1])) || (right(&runs[0]) && left(&runs[1])),
            "a run to each square"
        );
        // A start point to honour: sewn group by group, as it was.
        s.params.start = Some(0.0);
        assert!(plan("p", &[s], &PlanOptions::default()).trim_count() > 1);
    }

    #[test]
    fn travel_between_parts_goes_under_what_is_sewn_on_top() {
        // Two squares 4 mm apart, and a later object across the gap.
        let under = two_squares(40.0);
        let over = shape(
            fill(vec![
                P::new(80.0, 20.0),
                P::new(160.0, 20.0),
                P::new(160.0, 80.0),
                P::new(80.0, 80.0),
            ]),
            Kind::Fill,
            [255, 0, 0],
        );
        let d = plan("p", &[under.clone(), over.clone()], &PlanOptions::default());
        assert_eq!(d.blocks[0].runs.len(), 1, "no cut between the squares");
        assert!(d.bridges.is_empty(), "and nothing in the open");
        // What crosses the gap is under the object on top, off its edges.
        let crossing: Vec<P> = along(&d.blocks[0].runs[0])
            .into_iter()
            .filter(|p| p.x > 105.0 && p.x < 135.0)
            .collect();
        assert!(!crossing.is_empty());
        assert!(
            crossing.iter().all(|p| p.y > 24.0 && p.y < 76.0),
            "{crossing:?}"
        );
        // Nothing on top: the gap is bare cloth, and the thread is cut.
        let d = plan("p", std::slice::from_ref(&under), &PlanOptions::default());
        assert_eq!(d.trim_count(), 1);
        // Sewn the other way up, the cover is underneath: no hiding there.
        let d = plan("p", &[over.clone(), under.clone()], &PlanOptions::default());
        assert_eq!(d.blocks[1].runs.len(), 2);
        // And no routing asked for: cut.
        let off = PlanOptions {
            travel_route: 0.0,
            ..PlanOptions::default()
        };
        let d = plan("p", &[under, over], &off);
        assert!(d.blocks[0].runs.len() >= 2);
        assert!(
            d.blocks[0]
                .runs
                .iter()
                .all(|r| along(r).iter().all(|p| p.x < 105.0 || p.x > 135.0)),
            "nothing sewn across the gap"
        );
    }

    #[test]
    fn travel_goes_through_a_neck_of_a_close_fill_but_not_an_open_one() {
        // Two blocks joined by a neck 0.6 mm wide: one outline, but the
        // walk round its inside (0.5 mm in) can't get through.
        let necked: Polyline = vec![
            P::new(0.0, 0.0),
            P::new(100.0, 0.0),
            P::new(100.0, 47.0),
            P::new(120.0, 47.0),
            P::new(120.0, 0.0),
            P::new(220.0, 0.0),
            P::new(220.0, 100.0),
            P::new(120.0, 100.0),
            P::new(120.0, 53.0),
            P::new(100.0, 53.0),
            P::new(100.0, 100.0),
            P::new(0.0, 100.0),
        ];
        let mut sh = shape(fill(necked.clone()), Kind::Fill, [0; 3]);
        sh.params.angle = Some(90.0);
        let d = plan("p", &[sh.clone()], &PlanOptions::default());
        assert_eq!(d.trim_count(), 0, "through the neck");
        // All of it inside the outline (a row's end may pull a little past).
        let grown = boolean::offset(&[necked], FillRule::NonZero, 6.0);
        for run in &d.blocks[0].runs {
            assert!(along(run).iter().all(|&p| crate::geom::point_inside(
                &grown,
                FillRule::NonZero,
                p
            )));
        }
        // Rows 2 mm apart hide nothing under them: cut instead.
        sh.params.spacing = Some(2.0);
        let d = plan("p", &[sh], &PlanOptions::default());
        assert!(d.trim_count() > 0);
    }

    #[test]
    fn connect_bridges_the_parts_of_one_object_and_says_where() {
        let s = two_squares(30.0);
        let d = plan("p", std::slice::from_ref(&s), &PlanOptions::default());
        assert_eq!(d.trim_count(), 1);
        assert!(d.bridges.is_empty());
        let opt = PlanOptions {
            connect: 300.0,
            ..PlanOptions::default()
        };
        let d = plan("p", &[s], &opt);
        assert_eq!(d.trim_count(), 0, "sewn across the gap");
        assert_eq!(d.bridges.len(), 1);
        let (a, b) = d.bridges[0];
        assert!(a.dist(b) <= 300.0);
        assert!(a.x.min(b.x) < 105.0 && a.x.max(b.x) > 125.0, "{a:?} {b:?}");
        // Further apart than that: still cut.
        let d = plan("p", &[two_squares(500.0)], &opt);
        assert_eq!(d.trim_count(), 1);
        assert!(d.bridges.is_empty());
        // Centered, the bridges move with the stitches.
        let mut d = plan("p", &[two_squares(30.0)], &opt);
        let before = (d.bridges[0].0, d.blocks[0].runs[0].pts[0]);
        d.center();
        let after = (d.bridges[0].0, d.blocks[0].runs[0].pts[0]);
        assert!((before.0 - after.0).dist(before.1 - after.1) < 1e-9);
    }

    #[test]
    fn travel_follows_the_inside_of_a_fill_instead_of_cutting() {
        // A C shape: the two arms' rows can't be joined by a straight line
        // inside it, but a walk round the inside of the outline joins them.
        let c: Polyline = vec![
            P::new(0.0, 0.0),
            P::new(300.0, 0.0),
            P::new(300.0, 60.0),
            P::new(60.0, 60.0),
            P::new(60.0, 240.0),
            P::new(300.0, 240.0),
            P::new(300.0, 300.0),
            P::new(0.0, 300.0),
        ];
        let mut sh = shape(fill(c.clone()), Kind::Fill, [0; 3]);
        sh.params.angle = Some(90.0);
        let routed = plan(
            "c",
            &[sh.clone()],
            &PlanOptions {
                travel_route: 1000.0,
                ..PlanOptions::default()
            },
        );
        let cut = plan(
            "c",
            &[sh],
            &PlanOptions {
                travel_route: 0.0,
                ..PlanOptions::default()
            },
        );
        assert!(cut.trim_count() > 0, "the arms need a cut without routing");
        assert_eq!(routed.trim_count(), 0, "routed round the inside instead");
        // Every travel stitch stays inside the shape.
        for r in routed.blocks.iter().flat_map(|b| &b.runs) {
            for w in r.pts.windows(2) {
                assert!(
                    crate::geom::segment_inside(
                        std::slice::from_ref(&c),
                        FillRule::NonZero,
                        w[0],
                        w[1],
                        5.0
                    ) || w[0].dist(w[1]) < 1.0,
                    "{:?} -> {:?} leaves the shape",
                    w[0],
                    w[1]
                );
            }
        }
        let route = route_inside(
            std::slice::from_ref(&c),
            FillRule::NonZero,
            P::new(290.0, 30.0),
            P::new(290.0, 270.0),
            1000.0,
        )
        .unwrap();
        assert!(route.len() > 10, "stitched every 3 mm: {}", route.len());
        assert_eq!(*route.last().unwrap(), P::new(290.0, 270.0));
    }

    #[test]
    fn manual_stitches_are_kept_exactly() {
        let pts = vec![
            P::new(0.0, 0.0),
            P::new(17.0, 3.0),
            P::new(5.0, 40.0),
            P::new(60.0, 41.0),
        ];
        let mut s = shape(
            Geometry::Stroke {
                lines: vec![(pts.clone(), false)],
                width: 4.0,
            },
            Kind::Manual,
            [0, 0, 0],
        );
        s.part = Part::Stroke;
        let d = plan("t", &[s], &PlanOptions::default());
        assert_eq!(d.blocks[0].runs[0].pts, pts);
    }

    #[test]
    fn wide_satin_is_auto_split() {
        let line = vec![P::new(0.0, 0.0), P::new(300.0, 0.0)];
        let mut s = shape(
            Geometry::Stroke {
                lines: vec![(line, false)],
                width: 90.0,
            },
            Kind::Satin,
            [0, 0, 0],
        );
        s.part = Part::Stroke;
        s.params.underlay = Some(false);
        let longest = |d: &Design| {
            d.blocks[0]
                .runs
                .iter()
                .flat_map(|r| r.pts.windows(2).map(|w| w[0].dist(w[1])))
                .fold(0.0, f64::max)
        };
        let split = plan("t", std::slice::from_ref(&s), &PlanOptions::default());
        assert!(longest(&split) <= 75.0, "{}", longest(&split));
        s.params.split = Some(0.0);
        let whole = plan("t", &[s], &PlanOptions::default());
        assert!(longest(&whole) > 85.0);
    }

    #[test]
    fn dense_fill_keeps_every_row_start() {
        // 10 mm square, 0.25 mm rows: every row's first and last stitch must survive planning.
        let s = shape(fill(sq(0.0, 0.0, 100.0)), Kind::Fill, [0, 0, 0]);
        let d = plan("t", &[s], &PlanOptions::default());
        let pts: Vec<P> = d.blocks[0]
            .runs
            .iter()
            .flat_map(|r| r.pts.iter().copied())
            .collect();
        let edge_hits = pts
            .iter()
            .filter(|p| p.x.abs() < 1e-6 || (p.x - 100.0).abs() < 1e-6)
            .count();
        assert_eq!(edge_hits, 80, "40 rows × 2 ends");
        assert_eq!(d.per_shape, vec![pts.len()]);
    }

    #[test]
    fn overlap_removal_skips_fill_under_later_objects() {
        let base = shape(fill(sq(0.0, 0.0, 200.0)), Kind::Fill, [0, 0, 80]);
        let top = shape(fill(sq(50.0, 50.0, 100.0)), Kind::Fill, [255, 200, 0]);
        let mut opt = PlanOptions::default();
        let with = plan("t", &[base.clone(), top.clone()], &opt);
        opt.remove_overlaps = false;
        let without = plan("t", &[base, top], &opt);
        let hidden = |d: &Design| {
            d.blocks[0]
                .runs
                .iter()
                .flat_map(|r| &r.pts)
                .filter(|p| p.x > 60.0 && p.x < 140.0 && p.y > 60.0 && p.y < 140.0)
                .count()
        };
        assert!(hidden(&without) > 30, "{}", hidden(&without));
        assert_eq!(
            hidden(&with),
            0,
            "no base stitches deep under the top object"
        );
        // ...but the base still reaches 0.5 mm under its edge, so no gap shows.
        let near_edge = with.blocks[0]
            .runs
            .iter()
            .flat_map(|r| &r.pts)
            .any(|p| p.x > 52.0 && p.x < 56.0 && p.y > 80.0 && p.y < 120.0);
        assert!(near_edge);
    }

    #[test]
    fn hat_order_sews_center_then_out_and_keeps_layering() {
        let left = shape(fill(sq(-300.0, 0.0, 50.0)), Kind::Fill, [1, 1, 1]);
        let center = shape(fill(sq(-25.0, 0.0, 50.0)), Kind::Fill, [1, 1, 1]);
        let under = shape(fill(sq(200.0, 0.0, 80.0)), Kind::Fill, [2, 2, 2]);
        let over = shape(fill(sq(220.0, 20.0, 40.0)), Kind::Fill, [1, 1, 1]);
        let opt = PlanOptions {
            hat_order: true,
            ..PlanOptions::default()
        };
        let d = plan("t", &[left, center, under, over], &opt);
        let pos = |i: usize| d.sew_order.iter().position(|&x| x == i).unwrap();
        assert_eq!(d.sew_order[0], 1, "center first");
        assert!(pos(2) < pos(3), "the object underneath still sews first");
    }

    #[test]
    fn satin_on_filled_bar_is_a_column() {
        let bar = vec![
            P::new(0.0, 0.0),
            P::new(200.0, 0.0),
            P::new(200.0, 30.0),
            P::new(0.0, 30.0),
        ];
        let s = shape(fill(bar), Kind::Satin, [0, 0, 0]);
        let d = plan("t", &[s], &PlanOptions::default());
        assert!(d.notes.is_empty(), "{:?}", d.notes);
        let pts: Vec<P> = d.blocks[0]
            .runs
            .iter()
            .flat_map(|r| r.pts.iter().copied())
            .collect();
        // Satin stitches cross the bar: consecutive points alternate between the long edges.
        let crossings = pts
            .windows(2)
            .filter(|w| (w[0].y - w[1].y).abs() > 25.0)
            .count();
        assert!(crossings > pts.len() / 2, "{crossings} of {}", pts.len());
    }

    #[test]
    fn satin_on_a_blob_falls_back_to_fill_with_a_note() {
        let s = shape(fill(sq(0.0, 0.0, 300.0)), Kind::Satin, [0, 0, 0]);
        let d = plan("t", &[s], &PlanOptions::default());
        assert_eq!(d.notes.len(), 1);
        assert!(d.stitch_count() > 100);
    }
}

#[cfg(test)]
mod concave_fill_tests {
    use super::*;
    use crate::params::Params;
    use crate::svg::Part;

    /// An "H": stems meet a crossbar, so fill rows change length abruptly.
    /// No stitch may cut across the notches above or below the crossbar.
    #[test]
    fn fill_never_cuts_across_notches() {
        let pts = [
            (27.5, 3.0),
            (30.1, 3.0),
            (30.1, 10.6),
            (33.5, 10.6),
            (33.5, 3.0),
            (36.1, 3.0),
            (36.1, 21.0),
            (33.5, 21.0),
            (33.5, 13.2),
            (30.1, 13.2),
            (30.1, 21.0),
            (27.5, 21.0),
        ];
        let ring: Polyline = pts
            .iter()
            .map(|&(x, y)| P::new(x * 10.0, y * 10.0))
            .collect();
        for angle in [0.0, 30.0, 90.0] {
            let s = Shape {
                id: String::new(),
                part: Part::Fill,
                label: String::new(),
                color: [0, 0, 0],
                paint: [0, 0, 0],
                geometry: Geometry::Fill {
                    rings: vec![ring.clone()],
                    rule: FillRule::NonZero,
                },
                params: Params {
                    kind: Some(Kind::Fill),
                    angle: Some(angle),
                    ..Params::default()
                },
                uid: 0,
                groups: Vec::new(),
            };
            let d = plan("t", &[s], &PlanOptions::default());
            for r in d.blocks.iter().flat_map(|b| &b.runs) {
                for w in r.pts.windows(2) {
                    let m = w[0].lerp(w[1], 0.5);
                    let in_notch = m.x > 302.0 && m.x < 334.0 && (m.y < 104.0 || m.y > 134.0);
                    assert!(
                        !in_notch,
                        "angle {angle}: stitch {:?} -> {:?} crosses a notch",
                        w[0], w[1]
                    );
                }
            }
        }
    }
}
