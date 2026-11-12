//! Recover several curved satin sections from a continuous machine-file run.
//! Short points near one edge belong to that needle visit, not to a third rail.
//! Retain the original run and source indices; only replace supported spans.

use super::median;
use crate::geom::{Affine, P, Polyline, affine_apply};
use crate::stitch::satin::{self, Rails, SatinParams, SatinStyle};

#[derive(Clone, Debug)]
struct Section {
    start: usize,
    end: usize,
    pairs: Vec<(P, P)>,
    spacing: f64,
}

pub struct SatinRun {
    pub original: Polyline,
    sections: Vec<Section>,
}

/// A small edge cluster must be distinct from the opposite side. This is a
/// local width estimate: using a whole-run width swallows narrow satin tips.
fn edge_visits(points: &[P]) -> Vec<usize> {
    let lengths: Vec<_> = points.windows(2).map(|p| p[0].dist(p[1])).collect();
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, p) in points.iter().enumerate() {
        let mut nearby = lengths[i.saturating_sub(10)..(i + 10).min(lengths.len())].to_vec();
        nearby.sort_by(f64::total_cmp);
        let radius = (median(&nearby[nearby.len() / 2..]) * 0.45).min(12.0);
        if let Some(g) = groups
            .last_mut()
            .filter(|g| points[g.start].dist(*p) <= radius)
        {
            g.end = i + 1;
        } else {
            groups.push(i..i + 1);
        }
    }
    let centers: Vec<_> = groups
        .iter()
        .map(|g| {
            points[g.clone()]
                .iter()
                .copied()
                .fold(P::default(), |a, b| a + b)
                * (1.0 / g.len() as f64)
        })
        .collect();
    groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            // Choose the actual extreme needle point, never the cluster's average:
            // averaging would erode the letter's outline on every edge.
            let score = |p: P| {
                (if i > 0 { p.dist(centers[i - 1]) } else { 0.0 })
                    + (if i + 1 < centers.len() {
                        p.dist(centers[i + 1])
                    } else {
                        0.0
                    })
            };
            g.clone()
                .max_by(|&a, &b| score(points[a]).total_cmp(&score(points[b])))
                .unwrap()
        })
        .collect()
}

fn alternating(a: P, b: P, c: P) -> bool {
    let width = a.dist(b).min(b.dist(c));
    (9.0..=70.0).contains(&width)
        && a.dist(c) <= (width * 0.7).min(12.0)
        && (b - a).norm().dot((c - b).norm()) < -0.65
}

fn crossing(a: P, b: P, c: P, d: P) -> bool {
    let cross = |u: P, v: P| u.x * v.y - u.y * v.x;
    let s1 = cross(b - a, c - a);
    let s2 = cross(b - a, d - a);
    let t1 = cross(d - c, a - c);
    let t2 = cross(d - c, b - c);
    s1 * s2 < -1e-6 && t1 * t2 < -1e-6
}

fn section(points: &[P], visits: &[usize]) -> Option<Section> {
    let visits = &visits[..visits.len() / 2 * 2];
    if visits.len() < 24 {
        return None;
    }
    let pairs: Vec<_> = visits
        .chunks_exact(2)
        .map(|v| (points[v[0]], points[v[1]]))
        .collect();
    let steps: Vec<_> = pairs
        .windows(2)
        .map(|w| (w[0].0.dist(w[1].0) + w[0].1.dist(w[1].1)) * 0.5)
        .collect();
    let spacing = median(&steps);
    if !(1.5..=8.0).contains(&spacing)
        || steps.iter().any(|s| *s < 0.5 || *s > spacing * 2.5)
        || pairs
            .windows(2)
            .any(|w| (w[0].1 - w[0].0).norm().dot((w[1].1 - w[1].0).norm()) < 0.65)
    {
        return None;
    }
    // Cross-sections must not fold through one another. A spatial grid limits
    // candidate pairs on large designs while still checking a looping column.
    let mut cells: std::collections::HashMap<(i64, i64), Vec<usize>> = Default::default();
    for (i, &(a, b)) in pairs.iter().enumerate() {
        for x in (a.x.min(b.x) / 80.0).floor() as i64..=(a.x.max(b.x) / 80.0).floor() as i64 {
            for y in (a.y.min(b.y) / 80.0).floor() as i64..=(a.y.max(b.y) / 80.0).floor() as i64 {
                let bucket = cells.entry((x, y)).or_default();
                if bucket
                    .iter()
                    .any(|&j| crossing(a, b, pairs[j].0, pairs[j].1))
                {
                    return None;
                }
                bucket.push(i);
            }
        }
    }
    Some(Section {
        start: visits[0],
        end: *visits.last()?,
        pairs,
        spacing,
    })
}

impl SatinRun {
    pub fn recover(points: &[P]) -> Option<Self> {
        if points.len() < 24 || points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
            return None;
        }
        let visits = edge_visits(points);
        let mut sections = Vec::new();
        let mut start = 0;
        for j in 0..visits.len().saturating_sub(2) {
            if !alternating(
                points[visits[j]],
                points[visits[j + 1]],
                points[visits[j + 2]],
            ) {
                if let Some(s) = section(points, &visits[start..j + 2]) {
                    sections.push(s);
                }
                start = j + 1;
            }
        }
        if let Some(s) = section(points, &visits[start..]) {
            sections.push(s);
        }
        if sections.is_empty() {
            return None;
        }
        Some(Self {
            original: points.to_vec(),
            sections,
        })
    }

    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// Source spans that are neither recovered satin nor a short connector.
    pub fn uncertain_spans(&self) -> Vec<(usize, usize)> {
        self.gaps()
            .into_iter()
            .filter(|&(a, b)| !self.connector(a, b))
            .collect()
    }

    fn gaps(&self) -> Vec<(usize, usize)> {
        let mut gaps = Vec::new();
        let mut cursor = 0;
        for s in &self.sections {
            if s.start > cursor {
                gaps.push((cursor, s.start));
            }
            cursor = s.end;
        }
        if cursor + 1 < self.original.len() {
            gaps.push((cursor, self.original.len() - 1));
        }
        gaps
    }

    fn connector(&self, a: usize, b: usize) -> bool {
        // At most twelve source stitches between supported regions (including
        // locking stitches at the ends). Long/mixed regions remain explicitly
        // uncertain, even if they look like a running line.
        if b - a > 12 {
            return false;
        }
        let points = &self.original[a..=b];
        let directions: Vec<_> = points
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|d| d.len() > 0.1)
            .collect();
        if directions.iter().all(|d| d.len() <= 40.0)
            && directions
                .windows(2)
                .all(|w| w[0].norm().dot(w[1].norm()) > -0.3)
        {
            return true;
        }
        // A short collinear back-and-forth at a run boundary is a lock, not
        // another satin region. Don't apply this exception inside the run.
        if a != 0 && b + 1 != self.original.len() {
            return false;
        }
        let far = points
            .iter()
            .copied()
            .max_by(|x, y| x.dist(points[0]).total_cmp(&y.dist(points[0])))
            .unwrap();
        let dir = (far - points[0]).norm();
        points
            .iter()
            .all(|p| p.dist(points[0]) <= 15.0 && (*p - points[0]).dot(dir.perp()).abs() <= 1.5)
    }

    pub fn generate(&self, m: Affine) -> Polyline {
        let mut out = Vec::new();
        let mut cursor = 0;
        let append_gap = |out: &mut Polyline, a: usize, b: usize| {
            let pts: Vec<_> = self.original[a..=b]
                .iter()
                .map(|p| affine_apply(m, *p))
                .collect();
            if self.connector(a, b) {
                // Keep every corner and the sewing order; add points to long
                // travel stitches when enlarging rather than stretching them.
                out.extend(crate::geom::resample_keep_corners(&pts, 25.0));
            } else {
                out.extend(pts);
            }
        };
        for s in &self.sections {
            if cursor < s.start {
                append_gap(&mut out, cursor, s.start);
            }
            let a: Vec<_> = s.pairs.iter().map(|p| affine_apply(m, p.0)).collect();
            let b: Vec<_> = s.pairs.iter().map(|p| affine_apply(m, p.1)).collect();
            let mut sync = Vec::new();
            let (mut la, mut lb) = (0.0, 0.0);
            for i in 1..a.len() {
                la += a[i - 1].dist(a[i]);
                lb += b[i - 1].dist(b[i]);
                if i + 1 < a.len() {
                    sync.push((la, lb));
                }
            }
            let column = satin::column(
                &Rails {
                    a,
                    b,
                    sync,
                    closed: false,
                },
                &SatinParams {
                    spacing: s.spacing,
                    pull: 0.0,
                    underlay: 0,
                    run_len: 25.0,
                    short_stitches: false,
                    split: Some(70.0),
                    style: SatinStyle::Plain,
                },
            );
            // Staggering can make a split slightly longer than its nominal
            // length. Enforce the 7 mm ceiling on the reconstructed result.
            out.extend(crate::geom::resample_keep_corners(&column, 70.0));
            cursor = s.end;
        }
        if cursor + 1 < self.original.len() {
            append_gap(&mut out, cursor, self.original.len() - 1);
        }
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{IDENTITY, rotate_about, scale_about};

    fn sample() -> Polyline {
        crate::formats::read::read_stitch_file(include_bytes!("../../examples/alower.pes"), "pes")
            .unwrap()
            .blocks[0]
            .runs[0]
            .clone()
    }

    #[test]
    fn alower_has_two_columns_with_stable_spacing_and_an_open_counter() {
        let model = SatinRun::recover(&sample()).unwrap();
        assert_eq!(model.section_count(), 2);
        assert!(model.uncertain_spans().is_empty());
        let enlarged = model.generate(scale_about(P::default(), 2.0, 2.0));
        assert!(enlarged.len() > model.generate(IDENTITY).len() * 19 / 10);
        // The letter's counter must remain empty: no reconstructed stitches
        // bridge the hole. Test full segments, not just needle positions.
        let hole = P::new(110.0, 120.0);
        for w in enlarged.windows(2) {
            let d = w[1] - w[0];
            let t = ((hole - w[0]).dot(d) / d.dot(d).max(1e-9)).clamp(0.0, 1.0);
            assert!(hole.dist(w[0] + d * t) > 18.0);
        }
        let first = &model.sections[0];
        let average_length: f64 = first
            .pairs
            .windows(2)
            .map(|w| w[0].0.dist(w[1].0) + w[0].1.dist(w[1].1))
            .sum();
        let n = (average_length / first.spacing).ceil() as usize;
        let steps: Vec<_> = enlarged[..(n + 1) * 2]
            .chunks_exact(2)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[0][0].dist(w[1][0]) + w[0][1].dist(w[1][1])) * 0.5)
            .collect();
        assert!((median(&steps) - first.spacing).abs() < 0.15);
        let large = model.generate(scale_about(P::default(), 5.0, 5.0));
        assert!(large.windows(2).all(|w| w[0].dist(w[1]) <= 70.01));
    }

    #[test]
    fn curved_columns_with_extra_edge_points_are_not_specific_to_the_fixture() {
        let mut points = Vec::new();
        for i in 0..100 {
            let t = i as f64 * 0.015;
            let a = P::new(t.cos() * 90.0, t.sin() * 90.0);
            let b = P::new(t.cos() * 120.0, t.sin() * 120.0);
            points.push(a);
            if i % 3 == 0 {
                points.push(a.lerp(b, 0.2));
            }
            points.push(b);
        }
        for angle in [0.0, 47.0, -90.0] {
            let m = rotate_about(P::default(), angle);
            let p: Vec<_> = points.iter().map(|p| affine_apply(m, *p)).collect();
            let model = SatinRun::recover(&p).unwrap();
            assert_eq!(model.section_count(), 1);
            assert!(model.uncertain_spans().is_empty());
        }
    }

    #[test]
    fn preserves_unrecognized_spans_and_rejects_non_satin() {
        let line: Vec<_> = (0..50).map(|i| P::new(i as f64 * 20.0, 0.0)).collect();
        assert!(SatinRun::recover(&line).is_none());
        assert!(SatinRun::recover(&crate::reconstruct::tests::fill_sample()).is_none());
        let mut p = sample();
        p.extend(line);
        let model = SatinRun::recover(&p).unwrap();
        assert!(!model.uncertain_spans().is_empty());
        let generated = model.generate(scale_about(P::default(), 2.0, 2.0));
        let tail: Vec<_> = p[p.len() - 50..].iter().map(|p| *p * 2.0).collect();
        assert_eq!(&generated[generated.len() - 50..], &tail);
    }
}
