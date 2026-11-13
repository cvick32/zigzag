//! Travel stitches that stay out of sight.
//!
//! Between two parts of a design the thread is cut, unless the needle can
//! get from one to the other where nobody will see: under the fill being
//! sewn, or under something sewn later on top. A [`Zone`] is that hidden
//! ground, as a grid; [`Zone::route`] finds a way across it (round holes,
//! through narrow necks, under what covers), keeping off its edges where
//! there's room, so a stitch doesn't peek out from under the cover.

use crate::geom::{Bounds, FillRule, P, Polyline};
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Cells are at least this big, units (0.25 mm): fine enough for the necks
/// between a dither's blocks.
const CELL: f64 = 2.5;
/// And few enough to keep a hoop-sized zone quick.
const MAX_CELLS: f64 = 400_000.0;
/// Travel keeps this far from the zone's edge where it can: cells whose
/// centers are two cells and more (0.5 mm) in.
const CLEAR: u8 = 3;
/// What a step nearer the edge than that costs, times its length.
const NEAR_EDGE: f64 = 4.0;
/// How hard the search leans toward the far end. Over 1 it stops looking
/// for the very best way once it has a good one (within this factor of
/// it): most of a dither's cells are near an edge, and weighing every way
/// round them all costs more than a slightly longer route does.
const GREED: f64 = 2.0;
/// A route's stitches are no longer than this, units.
const STITCH: f64 = 30.0;

/// Ground that stitches will cover, as a grid of cells.
pub struct Zone {
    min: P,
    cell: f64,
    w: usize,
    h: usize,
    /// Cells from the edge (0: not in the zone), up to [`CLEAR`].
    clear: Vec<u8>,
    /// Which connected piece of the zone each cell is in (0: none), so two
    /// ends with no way between them are known without searching for one.
    piece: Vec<u32>,
    /// The search's working arrays, kept between routes.
    search: RefCell<Search>,
}

/// What a search knows of each cell it has reached: valid where `seen`
/// holds this search's number (so a new search needn't wipe them).
#[derive(Default)]
struct Search {
    number: u32,
    seen: Vec<u32>,
    cost: Vec<f64>,
    came: Vec<u32>,
}

impl Zone {
    /// The union of `parts` (each with its fill rule) within `bounds`.
    pub fn new(parts: &[(&[Polyline], FillRule)], bounds: Bounds) -> Option<Zone> {
        if bounds.is_empty() {
            return None;
        }
        let (bw, bh) = (bounds.width().max(1.0), bounds.height().max(1.0));
        let cell = CELL.max((bw * bh / MAX_CELLS).sqrt());
        // A ring of empty cells all round: the edge is always an edge.
        let min = P::new(bounds.min.x - cell, bounds.min.y - cell);
        let w = (bw / cell).ceil() as usize + 2;
        let h = (bh / cell).ceil() as usize + 2;
        let mut inside = vec![false; w * h];
        // Each row's crossings with the outline, an edge at a time (an edge
        // only meets the rows it spans).
        let mut rows: Vec<Vec<(f64, i32)>> = vec![Vec::new(); h];
        for (rings, rule) in parts {
            rows.iter_mut().for_each(Vec::clear);
            for ring in rings.iter() {
                let n = ring.len();
                if n < 3 {
                    continue;
                }
                let mut a = ring[n - 1];
                for &b in ring {
                    let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
                    // Rows whose center line y has lo <= y < hi.
                    let r0 = ((lo - min.y) / cell - 0.5).ceil().max(0.0) as usize;
                    let r1 = ((hi - min.y) / cell - 0.5).ceil().min(h as f64);
                    if r1 > 0.0 && hi > lo {
                        for (row, xs) in rows.iter_mut().enumerate().take(r1 as usize).skip(r0) {
                            let y = min.y + (row as f64 + 0.5) * cell;
                            if (a.y <= y) != (b.y <= y) {
                                let x = a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x);
                                xs.push((x, if b.y > a.y { 1 } else { -1 }));
                            }
                        }
                    }
                    a = b;
                }
            }
            for (row, xs) in rows.iter_mut().enumerate() {
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut wind = 0;
                for pair in xs.windows(2) {
                    wind += pair[0].1;
                    if !rule.inside(wind) {
                        continue;
                    }
                    // Cells whose centers lie between the two crossings.
                    let c0 = ((pair[0].0 - min.x) / cell - 0.5).ceil().max(0.0) as usize;
                    let c1 = ((pair[1].0 - min.x) / cell - 0.5).floor();
                    if c1 < 0.0 {
                        continue;
                    }
                    for c in c0..=(c1 as usize).min(w - 1) {
                        inside[row * w + c] = true;
                    }
                }
            }
        }
        if !inside.iter().any(|&v| v) {
            return None;
        }
        // How far each cell is from the edge, as far as it matters: one more
        // than its nearest neighbour to it, there and back over the grid.
        let mut clear: Vec<u8> = inside.iter().map(|&v| if v { CLEAR } else { 0 }).collect();
        let at = |clear: &[u8], r: i32, c: i32| {
            if r < 0 || c < 0 || r >= h as i32 || c >= w as i32 {
                0
            } else {
                clear[r as usize * w + c as usize]
            }
        };
        for back in [false, true] {
            for i in 0..w * h {
                let k = if back { w * h - 1 - i } else { i };
                if clear[k] == 0 {
                    continue;
                }
                let (r, c) = ((k / w) as i32, (k % w) as i32);
                let d = if back { 1 } else { -1 };
                let near = at(&clear, r + d, c - 1)
                    .min(at(&clear, r + d, c))
                    .min(at(&clear, r + d, c + 1))
                    .min(at(&clear, r, c + d));
                clear[k] = clear[k].min(near + 1);
            }
        }
        let mut zone = Zone {
            min,
            cell,
            w,
            h,
            piece: Vec::new(),
            clear,
            search: RefCell::default(),
        };
        let mut piece = vec![0; zone.clear.len()];
        let mut pieces = 0;
        let mut stack = Vec::new();
        for k in 0..zone.clear.len() {
            if zone.clear[k] == 0 || piece[k] != 0 {
                continue;
            }
            pieces += 1;
            piece[k] = pieces;
            stack.push(k);
            while let Some(k) = stack.pop() {
                for n in zone.steps(k) {
                    if piece[n] == 0 {
                        piece[n] = pieces;
                        stack.push(n);
                    }
                }
            }
        }
        zone.piece = piece;
        Some(zone)
    }

    /// The cells a step from cell `k`: beside it, or diagonally (without
    /// cutting a corner of what's outside).
    fn steps(&self, k: usize) -> impl Iterator<Item = usize> + '_ {
        let (r, c) = ((k / self.w) as i32, (k % self.w) as i32);
        [
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (-1, 1),
            (1, -1),
            (1, 1),
        ]
        .into_iter()
        .filter_map(move |(dr, dc)| {
            let (rr, cc) = (r + dr, c + dc);
            if rr < 0 || cc < 0 || rr >= self.h as i32 || cc >= self.w as i32 {
                return None;
            }
            let n = rr as usize * self.w + cc as usize;
            let open = |r: i32, c: i32| self.clear[r as usize * self.w + c as usize] > 0;
            (self.clear[n] > 0 && (dr == 0 || dc == 0 || (open(r, cc) && open(rr, c)))).then_some(n)
        })
    }

    fn center(&self, k: usize) -> P {
        P::new(
            self.min.x + ((k % self.w) as f64 + 0.5) * self.cell,
            self.min.y + ((k / self.w) as f64 + 0.5) * self.cell,
        )
    }

    fn cell_of(&self, p: P) -> Option<usize> {
        let c = ((p.x - self.min.x) / self.cell).floor();
        let r = ((p.y - self.min.y) / self.cell).floor();
        (c >= 0.0 && r >= 0.0 && (c as usize) < self.w && (r as usize) < self.h)
            .then(|| r as usize * self.w + c as usize)
    }

    /// Whether `p` is in the zone.
    pub fn contains(&self, p: P) -> bool {
        self.cell_of(p).is_some_and(|k| self.clear[k] > 0)
    }

    /// The zone's cell at `p`, or the nearest within a couple of cells (a
    /// row's end sits on the outline, or just past it with pull added).
    fn enter(&self, p: P) -> Option<usize> {
        let reach = 3i32;
        let c = ((p.x - self.min.x) / self.cell).floor() as i32;
        let r = ((p.y - self.min.y) / self.cell).floor() as i32;
        let mut best: Option<(f64, usize)> = None;
        for rr in (r - reach).max(0)..=(r + reach).min(self.h as i32 - 1) {
            for cc in (c - reach).max(0)..=(c + reach).min(self.w as i32 - 1) {
                let k = rr as usize * self.w + cc as usize;
                if self.clear[k] == 0 {
                    continue;
                }
                let d = self.center(k).dist(p);
                if d <= reach as f64 * self.cell && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, k));
                }
            }
        }
        best.map(|(_, k)| k)
    }

    /// What a step onto cell `k` costs, times its length: more along the
    /// zone's edge.
    fn weight(&self, k: usize) -> f64 {
        if self.clear[k] < CLEAR {
            NEAR_EDGE
        } else {
            1.0
        }
    }

    /// What the straight line between two cells costs, counted as the
    /// search counts its steps (`None` if it leaves the zone).
    fn line_cost(&self, a: usize, b: usize) -> Option<f64> {
        let (pa, pb) = (self.center(a), self.center(b));
        let d = pa.dist(pb);
        let n = (d / (self.cell * 0.5)).ceil().max(1.0) as usize;
        let mut cost = 0.0;
        for i in 1..=n {
            let k = self.cell_of(pa.lerp(pb, (i as f64 - 0.5) / n as f64))?;
            if self.clear[k] == 0 {
                return None;
            }
            cost += d / n as f64 * self.weight(k);
        }
        Some(cost)
    }

    /// Stitches from `from` to `to` through the zone, when there's a way no
    /// longer than `max_len`: the points after `from`, ending on `to`.
    pub fn route(&self, from: P, to: P, max_len: f64) -> Option<Polyline> {
        let (start, goal) = (self.enter(from)?, self.enter(to)?);
        if self.piece[start] != self.piece[goal] {
            return None;
        }
        let goal_at = self.center(goal);
        // A*: steps cost their length, more along the zone's edge.
        let budget = max_len * NEAR_EDGE;
        let mut search = self.search.borrow_mut();
        let Search {
            number,
            seen,
            cost,
            came,
        } = &mut *search;
        if seen.len() != self.clear.len() || *number == u32::MAX {
            *seen = vec![0; self.clear.len()];
            *cost = vec![0.0; self.clear.len()];
            *came = vec![0; self.clear.len()];
            *number = 0;
        }
        *number += 1;
        let number = *number;
        let mut open: BinaryHeap<(Reverse<u64>, u32)> = BinaryHeap::new();
        let key = |f: f64| Reverse((f * 16.0) as u64);
        seen[start] = number;
        cost[start] = 0.0;
        open.push((key(GREED * self.center(start).dist(goal_at)), start as u32));
        let mut found = false;
        while let Some((_, k)) = open.pop() {
            let k = k as usize;
            if k == goal {
                found = true;
                break;
            }
            for n in self.steps(k) {
                let step = if n / self.w != k / self.w && n % self.w != k % self.w {
                    std::f64::consts::SQRT_2
                } else {
                    1.0
                } * self.cell;
                let g = cost[k] + step * self.weight(n);
                if (seen[n] != number || g < cost[n]) && g + self.center(n).dist(goal_at) <= budget
                {
                    seen[n] = number;
                    cost[n] = g;
                    came[n] = k as u32;
                    open.push((key(g + GREED * self.center(n).dist(goal_at)), n as u32));
                }
            }
        }
        if !found {
            return None;
        }
        let mut cells = vec![goal];
        while *cells.last().unwrap() != start {
            cells.push(came[*cells.last().unwrap()] as usize);
        }
        cells.reverse();
        // Straighten: each leg as long as it can be without costing more
        // than the way found (so it cuts no corner along an edge that the
        // search went round).
        let mut upto = vec![0.0; cells.len()];
        for i in 1..cells.len() {
            let diagonal = cells[i] / self.w != cells[i - 1] / self.w
                && cells[i] % self.w != cells[i - 1] % self.w;
            upto[i] = upto[i - 1]
                + if diagonal {
                    std::f64::consts::SQRT_2
                } else {
                    1.0
                } * self.cell
                    * self.weight(cells[i]);
        }
        let mut corners = vec![start];
        let mut i = 0;
        while i + 1 < cells.len() {
            let mut j = i + 1;
            while j + 1 < cells.len()
                && self
                    .line_cost(cells[i], cells[j + 1])
                    .is_some_and(|c| c <= upto[j + 1] - upto[i] + 1e-6)
            {
                j += 1;
            }
            corners.push(cells[j]);
            i = j;
        }
        // The corners between the two ends (not one right beside an end:
        // that would be a stitch too short to sew).
        let mut path: Polyline = vec![from];
        for &k in &corners {
            let p = self.center(k);
            if p.dist(from) > 2.0 * self.cell && p.dist(to) > 2.0 * self.cell {
                path.push(p);
            }
        }
        path.push(to);
        if crate::geom::polyline_length(&path) > max_len {
            return None;
        }
        let mut out = Vec::new();
        for leg in path.windows(2) {
            let n = (leg[0].dist(leg[1]) / STITCH).ceil().max(1.0) as usize;
            for k in 1..=n {
                out.push(leg[0].lerp(leg[1], k as f64 / n as f64));
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::polyline_length;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polyline {
        vec![
            P::new(x0, y0),
            P::new(x1, y0),
            P::new(x1, y1),
            P::new(x0, y1),
        ]
    }

    fn zone(parts: &[(&[Polyline], FillRule)]) -> Zone {
        let mut b = Bounds::empty();
        for (rings, _) in parts {
            rings.iter().flatten().for_each(|&p| b.add(p));
        }
        Zone::new(parts, b).unwrap()
    }

    /// Every stitch of the route, and the thread between them, in the zone.
    fn stays_in(z: &Zone, from: P, route: &[P]) -> bool {
        let mut a = from;
        route.iter().all(|&b| {
            let n = (a.dist(b) / 1.0).ceil().max(1.0) as usize;
            let ok = (1..n).all(|i| z.contains(a.lerp(b, i as f64 / n as f64)));
            a = b;
            ok
        })
    }

    #[test]
    fn straight_across_open_ground() {
        let sq = vec![rect(0.0, 0.0, 200.0, 200.0)];
        let z = zone(&[(&sq, FillRule::EvenOdd)]);
        let (a, b) = (P::new(20.0, 100.0), P::new(180.0, 100.0));
        let r = z.route(a, b, 500.0).unwrap();
        assert_eq!(*r.last().unwrap(), b);
        let len = a.dist(r[0]) + polyline_length(&r);
        assert!(len < 165.0, "{len}");
        assert!(r.windows(2).all(|w| w[0].dist(w[1]) <= STITCH + 1e-6));
    }

    #[test]
    fn round_a_hole_not_over_it() {
        // A 20 mm square with a 10 mm hole in the middle.
        let rings = vec![rect(0.0, 0.0, 200.0, 200.0), rect(50.0, 50.0, 150.0, 150.0)];
        let z = zone(&[(&rings, FillRule::EvenOdd)]);
        let (a, b) = (P::new(25.0, 100.0), P::new(175.0, 100.0));
        assert!(!z.contains(P::new(100.0, 100.0)));
        let r = z.route(a, b, 500.0).unwrap();
        assert!(stays_in(&z, a, &r), "{r:?}");
        // Round it: further than straight across, and clear of the hole's
        // edge and the outline (0.5 mm) where there's 2.5 mm of room.
        let len = a.dist(r[0]) + polyline_length(&r);
        assert!(len > 200.0 && len < 300.0, "{len}");
        for p in &r[..r.len() - 1] {
            let by_the_hole = p.x > 45.0 && p.x < 155.0 && p.y > 45.0 && p.y < 155.0;
            assert!(!by_the_hole, "{p:?} hugs the hole");
            let by_the_outline = p.x < 5.0 || p.x > 195.0 || p.y < 5.0 || p.y > 195.0;
            assert!(!by_the_outline, "{p:?} hugs the outline");
        }
        // Too far to be worth it: no route.
        assert!(z.route(a, b, 180.0).is_none());
    }

    #[test]
    fn through_a_neck_narrower_than_the_margin() {
        // Two blocks joined by a neck 0.6 mm wide.
        let rings = vec![vec![
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
        ]];
        let z = zone(&[(&rings, FillRule::NonZero)]);
        let (a, b) = (P::new(50.0, 20.0), P::new(170.0, 80.0));
        let r = z.route(a, b, 500.0).unwrap();
        assert!(stays_in(&z, a, &r), "{r:?}");
        assert!(a.dist(r[0]) + polyline_length(&r) < 200.0);
    }

    #[test]
    fn no_way_between_islands_unless_something_covers_the_gap() {
        let both = vec![rect(0.0, 0.0, 100.0, 100.0), rect(140.0, 0.0, 240.0, 100.0)];
        let (a, b) = (P::new(50.0, 50.0), P::new(190.0, 50.0));
        let apart = zone(&[(&both, FillRule::NonZero)]);
        assert!(apart.route(a, b, 500.0).is_none());
        // Something sewn later over the gap: under it.
        let cover = vec![rect(90.0, 20.0, 150.0, 80.0)];
        let covered = zone(&[(&both, FillRule::NonZero), (&cover, FillRule::NonZero)]);
        let r = covered.route(a, b, 500.0).unwrap();
        assert!(stays_in(&covered, a, &r));
        assert!(a.dist(r[0]) + polyline_length(&r) < 150.0);
    }

    #[test]
    fn an_end_just_past_the_outline_still_gets_in() {
        let sq = vec![rect(0.0, 0.0, 200.0, 200.0)];
        let z = zone(&[(&sq, FillRule::EvenOdd)]);
        // Pull compensation: a row's end 0.3 mm outside.
        let r = z.route(P::new(-3.0, 100.0), P::new(203.0, 60.0), 500.0);
        assert!(r.is_some());
        // Well outside: not this zone's to route.
        assert!(
            z.route(P::new(-30.0, 100.0), P::new(100.0, 60.0), 500.0)
                .is_none()
        );
    }
}
