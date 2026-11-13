//! Polygon booleans and offsets (via i_overlay), on rings in embroidery units.

use crate::geom::{FillRule, P, Polyline};
use i_overlay::core::fill_rule::FillRule as IFill;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::mesh::float::outline::offset::OutlineOffset;
use i_overlay::mesh::float::stroke::offset::StrokeOffset;
use i_overlay::mesh::float::style::{LineJoin, OutlineStyle, StrokeStyle};

type Shapes = Vec<Vec<Vec<[f64; 2]>>>;

fn to_contours(rings: &[Polyline]) -> Vec<Vec<[f64; 2]>> {
    rings
        .iter()
        .filter(|r| r.len() >= 3)
        .map(|r| r.iter().map(|p| [p.x, p.y]).collect())
        .collect()
}

fn from_shapes(shapes: Shapes) -> Vec<Polyline> {
    shapes
        .into_iter()
        .flatten()
        .map(|c| c.into_iter().map(|[x, y]| P::new(x, y)).collect())
        .collect()
}

fn rule(r: FillRule) -> IFill {
    match r {
        FillRule::NonZero => IFill::NonZero,
        FillRule::EvenOdd => IFill::EvenOdd,
    }
}

/// Normalize rings to clean shapes (outer CCW, holes CW in i_overlay's convention).
fn normalized(rings: &[Polyline], r: FillRule) -> Shapes {
    let subj = to_contours(rings);
    let empty: Vec<Vec<[f64; 2]>> = Vec::new();
    subj.overlay(&empty, OverlayRule::Subject, rule(r))
}

/// `subject` minus `clip`. Result rings use even-odd.
pub fn difference(
    subject: &[Polyline],
    subject_rule: FillRule,
    clip: &[Polyline],
) -> Vec<Polyline> {
    if clip.is_empty() {
        return from_shapes(normalized(subject, subject_rule));
    }
    let subj = from_shapes(normalized(subject, subject_rule));
    let a = to_contours(&subj);
    let b = to_contours(clip);
    from_shapes(a.overlay(&b, OverlayRule::Difference, IFill::NonZero))
}

/// `rings` as clean shapes, for several booleans with them: a ring set
/// normalized once rather than by every operation.
pub fn normalize(rings: &[Polyline], r: FillRule) -> Vec<Polyline> {
    from_shapes(normalized(rings, r))
}

/// The separate parts of a shape: each one's outline, then its holes.
pub fn parts(rings: &[Polyline], r: FillRule) -> Vec<Vec<Polyline>> {
    normalized(rings, r)
        .into_iter()
        .map(|shape| {
            shape
                .into_iter()
                .map(|c| c.into_iter().map(|[x, y]| P::new(x, y)).collect())
                .collect()
        })
        .collect()
}

/// The part of `rings` inside the rectangle `min`–`max`. Result uses even-odd.
pub fn clip_to_rect(rings: &[Polyline], r: FillRule, min: P, max: P) -> Vec<Polyline> {
    clip_normalized_to_rect(&normalize(rings, r), min, max)
}

/// [`clip_to_rect`] for rings from [`normalize`]: one overlay.
pub fn clip_normalized_to_rect(rings: &[Polyline], min: P, max: P) -> Vec<Polyline> {
    let rect = vec![vec![
        [min.x, min.y],
        [max.x, min.y],
        [max.x, max.y],
        [min.x, max.y],
    ]];
    from_shapes(to_contours(rings).overlay(&rect, OverlayRule::Intersect, IFill::NonZero))
}

/// Closed rings with points closer than `tol` to the line through their
/// neighbours dropped (Douglas–Peucker); rings left with under 3 points go.
pub fn simplify_rings(rings: &[Polyline], tol: f64) -> Vec<Polyline> {
    fn dp(pts: &[P], tol: f64, keep: &mut [bool]) {
        let (mut stack, n) = (vec![(0usize, pts.len() - 1)], pts.len());
        let _ = n;
        while let Some((a, b)) = stack.pop() {
            if b <= a + 1 {
                continue;
            }
            let (pa, pb) = (pts[a], pts[b]);
            let d = pb - pa;
            let len = d.len();
            let (mut far, mut at) = (0.0, a);
            for (k, &p) in pts.iter().enumerate().take(b).skip(a + 1) {
                let dist = if len < 1e-12 {
                    (p - pa).len()
                } else {
                    ((p - pa).x * d.y - (p - pa).y * d.x).abs() / len
                };
                if dist > far {
                    (far, at) = (dist, k);
                }
            }
            if far > tol {
                keep[at] = true;
                stack.push((a, at));
                stack.push((at, b));
            }
        }
    }
    rings
        .iter()
        .filter_map(|r| {
            if r.len() < 8 {
                return Some(r.clone());
            }
            // Split the ring at its farthest point from the first, so both
            // halves are open runs.
            let far = (1..r.len())
                .max_by(|&i, &j| (r[i] - r[0]).len().total_cmp(&(r[j] - r[0]).len()))
                .unwrap_or(r.len() / 2);
            let mut pts: Vec<P> = r.clone();
            pts.push(r[0]);
            let mut keep = vec![false; pts.len()];
            keep[0] = true;
            keep[far] = true;
            *keep.last_mut().unwrap() = true;
            dp(&pts[..=far], tol, &mut keep[..=far]);
            dp(&pts[far..], tol, &mut keep[far..]);
            let out: Polyline = pts[..pts.len() - 1]
                .iter()
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|(p, _)| *p)
                .collect();
            (out.len() >= 3).then_some(out)
        })
        .collect()
}

/// Union of several ring sets (each with its own fill rule). Result uses even-odd.
pub fn union(parts: &[(Vec<Polyline>, FillRule)]) -> Vec<Polyline> {
    let norm: Vec<Vec<Polyline>> = parts.iter().map(|(r, f)| normalize(r, *f)).collect();
    union_normalized(&norm)
}

/// Union of ring sets from [`normalize`], in one overlay: clean shapes all
/// wind the same way, so together, inside any of them is a nonzero winding.
pub fn union_normalized(parts: &[Vec<Polyline>]) -> Vec<Polyline> {
    let all: Vec<Vec<[f64; 2]>> = parts.iter().flat_map(|p| to_contours(p)).collect();
    if all.is_empty() {
        return Vec::new();
    }
    let empty: Vec<Vec<[f64; 2]>> = Vec::new();
    from_shapes(all.overlay(&empty, OverlayRule::Subject, IFill::NonZero))
}

/// Illustrator's Pathfinder operations on shape areas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathOp {
    /// Everything, merged.
    Unite,
    /// The backmost shape, minus everything in front of it.
    MinusFront,
    /// Only where every shape overlaps.
    Intersect,
    /// Where an odd number of shapes overlap.
    Exclude,
}

/// Combine shape areas (back to front). Result rings use even-odd.
pub fn pathfinder(parts: &[(Vec<Polyline>, FillRule)], op: PathOp) -> Vec<Polyline> {
    let norm: Vec<Vec<Polyline>> = parts
        .iter()
        .map(|(r, f)| from_shapes(normalized(r, *f)))
        .collect();
    let Some((first, rest)) = norm.split_first() else {
        return Vec::new();
    };
    let combine = |a: &Vec<Polyline>, b: &Vec<Polyline>, r: OverlayRule| {
        from_shapes(to_contours(a).overlay(&to_contours(b), r, IFill::NonZero))
    };
    match op {
        PathOp::Unite => rest
            .iter()
            .fold(first.clone(), |acc, b| combine(&acc, b, OverlayRule::Union)),
        PathOp::MinusFront => {
            let front = rest
                .iter()
                .skip(1)
                .fold(rest.first().cloned().unwrap_or_default(), |acc, b| {
                    combine(&acc, b, OverlayRule::Union)
                });
            combine(first, &front, OverlayRule::Difference)
        }
        PathOp::Intersect => rest.iter().fold(first.clone(), |acc, b| {
            combine(&acc, b, OverlayRule::Intersect)
        }),
        PathOp::Exclude => rest
            .iter()
            .fold(first.clone(), |acc, b| combine(&acc, b, OverlayRule::Xor)),
    }
}

/// Grow (positive `d`) or shrink (negative) a shape by `d` units.
pub fn offset(rings: &[Polyline], r: FillRule, d: f64) -> Vec<Polyline> {
    if d.abs() < 1e-9 {
        return from_shapes(normalized(rings, r));
    }
    let shapes = normalized(rings, r);
    let style = OutlineStyle::new(d).line_join(LineJoin::Miter(0.5));
    from_shapes(shapes.outline(&style))
}

/// Area covered by stroking `lines` with `width` (round-ish joins, butt caps).
pub fn stroke_area(lines: &[(Polyline, bool)], width: f64) -> Vec<Polyline> {
    let mut out = Vec::new();
    for (line, closed) in lines {
        if line.len() < 2 {
            continue;
        }
        let path: Vec<[f64; 2]> = line.iter().map(|p| [p.x, p.y]).collect();
        let style: StrokeStyle<[f64; 2]> = StrokeStyle::new(width).line_join(LineJoin::Miter(0.5));
        out.extend(from_shapes(path.stroke(style, *closed)));
    }
    out
}

/// Absolute area of a ring.
pub fn ring_area(r: &[P]) -> f64 {
    let n = r.len();
    ((0..n)
        .map(|i| crate::geom::cross(r[i], r[(i + 1) % n]))
        .sum::<f64>()
        * 0.5)
        .abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::point_inside;

    fn sq(x: f64, y: f64, w: f64) -> Polyline {
        vec![
            P::new(x, y),
            P::new(x + w, y),
            P::new(x + w, y + w),
            P::new(x, y + w),
        ]
    }

    #[test]
    fn difference_cuts_a_hole() {
        let r = difference(
            &[sq(0.0, 0.0, 100.0)],
            FillRule::NonZero,
            &[sq(30.0, 30.0, 40.0)],
        );
        assert!(point_inside(&r, FillRule::EvenOdd, P::new(10.0, 10.0)));
        assert!(!point_inside(&r, FillRule::EvenOdd, P::new(50.0, 50.0)));
        let area: f64 = r.iter().map(|x| ring_area(x)).sum::<f64>();
        assert!(
            (area - (10000.0 + 1600.0)).abs() < 1.0,
            "outer + hole ring areas: {area}"
        );
    }

    #[test]
    fn pathfinder_operations() {
        let a = (vec![sq(0.0, 0.0, 100.0)], FillRule::NonZero);
        let b = (vec![sq(50.0, 50.0, 100.0)], FillRule::NonZero);
        let area = |r: &[Polyline]| -> f64 {
            // Even-odd: holes subtract.
            let mut v: Vec<f64> = r.iter().map(|x| ring_area(x)).collect();
            v.sort_by(|x, y| y.total_cmp(x));
            v.iter().sum()
        };
        let parts = [a, b];
        assert!((area(&pathfinder(&parts, PathOp::Unite)) - 17500.0).abs() < 1.0);
        assert!((area(&pathfinder(&parts, PathOp::MinusFront)) - 7500.0).abs() < 1.0);
        assert!((area(&pathfinder(&parts, PathOp::Intersect)) - 2500.0).abs() < 1.0);
        let x = pathfinder(&parts, PathOp::Exclude);
        assert!(!point_inside(&x, FillRule::EvenOdd, P::new(75.0, 75.0)));
        assert!(point_inside(&x, FillRule::EvenOdd, P::new(25.0, 25.0)));
        assert!(point_inside(&x, FillRule::EvenOdd, P::new(125.0, 125.0)));
    }

    #[test]
    fn offset_shrinks_and_grows() {
        let inner = offset(&[sq(0.0, 0.0, 100.0)], FillRule::NonZero, -10.0);
        assert!((ring_area(&inner[0]) - 6400.0).abs() < 1.0);
        let outer = offset(&[sq(0.0, 0.0, 100.0)], FillRule::NonZero, 10.0);
        assert!(ring_area(&outer[0]) > 14000.0);
    }

    #[test]
    fn stroke_covers_line() {
        let a = stroke_area(&[(vec![P::new(0.0, 0.0), P::new(100.0, 0.0)], false)], 20.0);
        assert!(point_inside(&a, FillRule::NonZero, P::new(50.0, 9.0)));
        assert!(!point_inside(&a, FillRule::NonZero, P::new(50.0, 11.0)));
    }
}
