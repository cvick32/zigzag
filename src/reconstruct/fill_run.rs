//! Recover consecutive tatami sweeps within a mixed machine stitch run.
//! Keep disconnected regions and intervening satin/underlay in sewing order.
use super::{
    median,
    shared::{Gaps, join_pieces},
};
use crate::geom::{Affine, P, Polyline, affine_apply};

#[derive(Clone, Debug)]
struct Row {
    start: usize,
    end: usize,
    a: P,
    b: P,
    lengths: Vec<f64>,
}
#[derive(Debug)]
struct FillSection {
    start: usize,
    end: usize,
    rows: Vec<(P, P)>,
    spacing: f64,
    length: f64,
}
pub struct FillRun {
    pub original: Polyline,
    sections: Vec<FillSection>,
    gaps: Gaps,
}

fn sweeps(points: &[P], minimum_width: f64, include_single: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut start = 0;
    while start + 1 < points.len() {
        let delta = points[start + 1] - points[start];
        if delta.len() < 8.0 {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end + 1 < points.len() {
            let step = points[end + 1] - points[end];
            let axis = (points[end + 1] - points[start]).norm();
            if step.len() < 1.0
                || step.len() > 70.0
                || step.norm().dot(axis) < 0.90
                || points[start..=end]
                    .iter()
                    .any(|p| (*p - points[start]).dot(axis.perp()).abs() > 3.0)
            {
                break;
            }
            end += 1;
        }
        if (include_single || end - start >= 2)
            && delta.len() <= 70.0
            && points[start].dist(points[end]) >= minimum_width
        {
            rows.push(Row {
                start,
                end,
                a: points[start],
                b: points[end],
                lengths: points[start..=end]
                    .windows(2)
                    .map(|w| w[0].dist(w[1]))
                    .collect(),
            });
        }
        start = end;
    }
    rows
}

fn group(rows: &[Row], minimum: usize) -> Option<FillSection> {
    if rows.len() < minimum
        || (minimum >= 6
            && rows
                .iter()
                .filter(|r| r.lengths.len() >= 2 && r.a.dist(r.b) >= 35.0)
                .count()
                < 4)
    {
        return None;
    }
    let pairs: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| if i % 2 == 0 { (r.a, r.b) } else { (r.b, r.a) })
        .collect();
    let progress: Vec<_> = pairs
        .windows(2)
        .map(|w| {
            let normal = ((w[0].1 - w[0].0).norm() + (w[1].1 - w[1].0).norm())
                .norm()
                .perp();
            ((w[1].0 + w[1].1) - (w[0].0 + w[0].1)).dot(normal) * 0.5
        })
        .collect();
    // Fan fills alternate small inner-edge advances with larger outer-edge
    // advances. Measure a complete out-and-back cycle, not one pivoting sweep.
    let sign = progress.iter().sum::<f64>().signum();
    let cycles: Vec<_> = if progress.len() > 1 {
        progress
            .windows(2)
            .map(|w| (w[0] + w[1]).abs() * 0.5)
            .collect()
    } else {
        vec![progress[0].abs()]
    };
    let spacing = median(&cycles);
    if !(1.5..=8.0).contains(&spacing)
        || progress
            .iter()
            .any(|d| d * sign < -0.2 || d.abs() > spacing * 4.0)
    {
        return None;
    }
    // Both boundaries must advance along the same sweep normal. Crossing or
    // backtracking rows are ambiguous layers, not a single fill section.
    if pairs.windows(2).any(|w| {
        let normal = ((w[0].1 - w[0].0).norm() + (w[1].1 - w[1].0).norm())
            .norm()
            .perp();
        (w[1].0 - w[0].0).dot(normal) * sign < -1.0 || (w[1].1 - w[0].1).dot(normal) * sign < -1.0
    }) {
        return None;
    }
    let lengths: Vec<_> = rows
        .iter()
        .flat_map(|r| r.lengths.iter().copied())
        .collect();
    let length = median(&lengths);
    if !(8.0..=60.0).contains(&length) || lengths.iter().any(|d| *d > 70.0) {
        return None;
    }
    Some(FillSection {
        start: rows[0].start,
        end: rows.last()?.end,
        rows: pairs,
        spacing,
        length,
    })
}

fn compatible(a: &Row, b: &Row, axis: P) -> bool {
    if b.start < a.end || b.start - a.end > 3 {
        return false;
    }
    let da = (a.b - a.a).norm();
    let db = (b.b - b.a).norm();
    if da.dot(db) > -0.97 {
        return false;
    }
    // The sewing transition must stay near the edge; do not bridge holes or
    // separate patches just because their row angles happen to match.
    if a.b.dist(b.a) > 20.0 {
        return false;
    }
    let shift = ((b.a + b.b) - (a.a + a.b)) * 0.5;
    let progress = shift.dot(axis.perp());
    let step = progress.abs();
    (0.0..=12.0).contains(&step)
        && (b.b - a.a).dot(axis.perp()) * progress.signum() >= -1.0
        && (b.a - a.b).dot(axis.perp()) * progress.signum() >= -1.0
        && (a.a - b.b).dot(axis).abs() <= 30.0
        && (a.b - b.a).dot(axis).abs() <= 30.0
}

// Short row groups are accepted only with independently recognized fill on
// both sides, matching spacing, traversal order, and continuous boundaries.
fn merge_neighbors(points: &[P], previous: &mut FillSection, next: &FillSection) -> bool {
    if next.start < previous.end || next.start - previous.end > 40 {
        return false;
    }
    let (mut a, mut b) = *previous.rows.last().unwrap();
    if previous.rows.len().is_multiple_of(2) {
        std::mem::swap(&mut a, &mut b);
    }
    let mut context = vec![Row {
        start: previous.end,
        end: previous.end,
        a,
        b,
        lengths: vec![previous.length],
    }];
    let middle = sweeps(&points[previous.end..=next.start], 20.0, true);
    for mut row in middle {
        row.start += previous.end;
        row.end += previous.end;
        context.push(row);
    }
    context.push(Row {
        start: next.start,
        end: next.start,
        a: next.rows[0].0,
        b: next.rows[0].1,
        lengths: vec![next.length],
    });
    if context
        .windows(2)
        .any(|w| !compatible(&w[0], &w[1], (w[0].b - w[0].a).norm()))
    {
        return false;
    }
    let Some(join) = group(&context, 2) else {
        return false;
    };
    if [previous.spacing, next.spacing]
        .iter()
        .any(|s| (join.spacing - s).abs() > 1.0_f64.max(s * 0.3))
    {
        return false;
    }
    let expected = (previous.spacing + next.spacing) * 0.5;
    if (previous.spacing - next.spacing).abs() > 1.0_f64.max(expected * 0.3) {
        return false;
    }
    let previous_count = previous.rows.len();
    for r in context.iter().skip(1).take(context.len() - 2) {
        previous
            .rows
            .push(if previous.rows.len().is_multiple_of(2) {
                (r.a, r.b)
            } else {
                (r.b, r.a)
            });
    }
    let flip = previous.rows.len() % 2 == 1;
    previous.rows.extend(
        next.rows
            .iter()
            .map(|&(a, b)| if flip { (b, a) } else { (a, b) }),
    );
    let count = previous_count + next.rows.len();
    previous.spacing = (previous.spacing * previous_count as f64
        + next.spacing * next.rows.len() as f64)
        / count as f64;
    previous.length = (previous.length * previous_count as f64
        + next.length * next.rows.len() as f64)
        / count as f64;
    previous.end = next.end;
    true
}

impl FillRun {
    pub fn recover(points: &[P]) -> Option<Self> {
        let wide = Self::recover_rows(points, false);
        let narrow = Self::recover_rows(points, true);
        [wide, narrow].into_iter().flatten().min_by_key(|m| {
            m.uncertain_spans()
                .iter()
                .map(|(a, b)| b - a)
                .sum::<usize>()
        })
    }
    fn recover_rows(points: &[P], include_single: bool) -> Option<Self> {
        if points.len() < 24 || points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
            return None;
        }
        let rows = sweeps(
            points,
            if include_single { 20.0 } else { 35.0 },
            include_single,
        );
        let mut sections = Vec::new();
        let mut start = 0;
        while start < rows.len() {
            let mut end = start + 1;
            while end < rows.len()
                && compatible(
                    &rows[end - 1],
                    &rows[end],
                    (rows[end - 1].b - rows[end - 1].a).norm(),
                )
            {
                if end > start + 1 {
                    let step = |a: &Row, b: &Row| {
                        ((b.a + b.b) - (a.a + a.b)).dot((a.b - a.a).norm().perp()) * 0.5
                    };
                    let previous = step(&rows[start], &rows[start + 1]);
                    let current = step(&rows[end - 1], &rows[end])
                        * if (end - start - 1) % 2 == 0 {
                            1.0
                        } else {
                            -1.0
                        };
                    if previous * current < -0.2 {
                        break;
                    }
                }
                end += 1;
            }
            if let Some(s) = group(&rows[start..end], 6) {
                sections.push(s);
            }
            start = end;
        }
        if sections.is_empty() {
            return None;
        }
        let mut merged: Vec<FillSection> = Vec::new();
        for next in sections {
            if let Some(previous) = merged.last_mut()
                && merge_neighbors(points, previous, &next)
            {
                continue;
            }
            merged.push(next);
        }
        let sections = merged;
        let gaps = Gaps::recover(points, sections.iter().map(|s| (s.start, s.end)));
        Some(Self {
            original: points.to_vec(),
            sections,
            gaps,
        })
    }
    pub fn fill_count(&self) -> usize {
        self.sections.len()
    }
    pub fn satin_count(&self) -> usize {
        self.gaps.satin_count()
    }
    pub fn uncertain_spans(&self) -> Vec<(usize, usize)> {
        self.gaps.uncertain_spans()
    }
    pub fn generate(&self, m: Affine) -> Polyline {
        let mut pieces = self.gaps.generate(&self.original, m);
        pieces.extend(self.sections.iter().map(|s| (s.start, s.generate(m))));
        join_pieces(pieces)
    }
}
impl FillSection {
    fn generate(&self, m: Affine) -> Polyline {
        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|(a, b)| (affine_apply(m, *a), affine_apply(m, *b)))
            .collect();
        let mut distances = vec![0.0];
        for w in rows.windows(2) {
            let normal = ((w[0].1 - w[0].0).norm() + (w[1].1 - w[1].0).norm())
                .norm()
                .perp();
            let step = ((w[1].0 + w[1].1) - (w[0].0 + w[0].1)).dot(normal).abs() * 0.5;
            distances.push(distances.last().unwrap() + step);
        }
        let total = *distances.last().unwrap();
        let mut intervals = (total / self.spacing).round().max(1.0) as usize;
        // Finish on the original side so the next section is not reached by
        // a newly invented diagonal across the fill.
        if intervals % 2 != (rows.len() - 1) % 2 {
            let less = intervals.saturating_sub(1).max(1);
            let more = intervals + 1;
            intervals = if less % 2 == (rows.len() - 1) % 2
                && (total / less as f64 - self.spacing).abs()
                    < (total / more as f64 - self.spacing).abs()
            {
                less
            } else {
                more
            };
        }
        let mut out = Vec::new();
        let mut segment = 0;
        for i in 0..=intervals {
            let distance = total * i as f64 / intervals as f64;
            while segment + 2 < rows.len() && distance > distances[segment + 1] {
                segment += 1;
            }
            let span = distances[segment + 1] - distances[segment];
            let t = if span > 1e-9 {
                (distance - distances[segment]) / span
            } else {
                0.0
            };
            let mut a = rows[segment].0.lerp(rows[segment + 1].0, t);
            let mut b = rows[segment].1.lerp(rows[segment + 1].1, t);
            if i % 2 == 1 {
                std::mem::swap(&mut a, &mut b);
            }
            let width = a.dist(b);
            out.push(a);
            // Alternate the penetration phase between rows while retaining the
            // measured interior stitch length and each boundary needle point.
            let mut along = self.length * if i % 2 == 0 { 0.25 } else { 0.75 };
            while along < width - 0.5 {
                out.push(a.lerp(b, along / width));
                along += self.length;
            }
            out.push(b);
        }
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_rows_inside_underlay_are_recovered_and_resized_in_both_axes() {
        let mut p = vec![P::new(-60.0, 0.0), P::new(-30.0, 0.0)];
        for row in 0..20 {
            for j in 0..=2 {
                p.push(P::new(
                    if row % 2 == 0 {
                        j as f64 * 25.0
                    } else {
                        50.0 - j as f64 * 25.0
                    },
                    row as f64 * 4.0,
                ));
            }
        }
        let m = FillRun::recover(&p).unwrap();
        assert_eq!(m.fill_count(), 1);
        assert!(m.uncertain_spans().is_empty());
        for scale in [0.5, 2.0] {
            let out = m.generate(crate::geom::scale_about(P::default(), scale, scale));
            if scale < 1.0 {
                assert!(out.len() < p.len());
            } else {
                assert!(out.len() > p.len());
            }
            let section = &m.sections[0];
            let fill = section.generate(crate::geom::scale_about(P::default(), scale, scale));
            let mut levels: Vec<_> = fill.iter().map(|p| p.y).collect();
            levels.dedup_by(|a, b| (*a - *b).abs() < 1e-8);
            assert!(
                levels
                    .windows(2)
                    .all(|w| ((w[1] - w[0]).abs() - section.spacing).abs() < 1.0)
            );
            assert!(
                fill.windows(2)
                    .all(|w| w[0].dist(w[1]) <= section.length + 0.1)
            );
        }
    }
    #[test]
    fn broad_fill_scales_stitch_count_with_area() {
        let mut p = Vec::new();
        for row in 0..100 {
            for j in 0..=20 {
                p.push(P::new(
                    if row % 2 == 0 {
                        j as f64 * 25.0
                    } else {
                        500.0 - j as f64 * 25.0
                    },
                    row as f64 * 4.0,
                ));
            }
        }
        let model = FillRun::recover(&p).unwrap();
        assert_eq!(model.fill_count(), 1);
        for scale in [0.5, 2.0] {
            let out = model.generate(crate::geom::scale_about(P::default(), scale, scale));
            let ratio = out.len() as f64 / p.len() as f64;
            assert!(
                (ratio / (scale * scale) - 1.0).abs() < 0.15,
                "{scale}: {ratio}"
            );
        }
    }
    #[test]
    fn separate_patches_do_not_become_a_fill_across_the_gap() {
        let patch = |offset: f64| {
            let mut p = Vec::new();
            for row in 0..10 {
                for j in 0..=3 {
                    p.push(P::new(
                        offset
                            + if row % 2 == 0 {
                                j as f64 * 25.0
                            } else {
                                75.0 - j as f64 * 25.0
                            },
                        row as f64 * 4.0,
                    ));
                }
            }
            p
        };
        let mut points = patch(0.0);
        let end = points.len() - 1;
        points.extend(patch(300.0));
        let model = FillRun::recover(&points).unwrap();
        assert_eq!(model.fill_count(), 2);
        assert!(
            model
                .uncertain_spans()
                .iter()
                .any(|&(a, b)| a <= end && b > end)
        );
        let m = crate::geom::scale_about(P::default(), 2.0, 2.0);
        let output = model.generate(m);
        assert!(
            output
                .windows(2)
                .any(|w| w[0] == affine_apply(m, points[end])
                    && w[1] == affine_apply(m, points[end + 1]))
        );
    }

    #[test]
    fn downloaded_fan_fill_reconstructs_completely_without_crossing_the_hole() {
        let file = crate::formats::read::read_stitch_file(
            include_bytes!("../../examples/pes/DailyFilled/1in/0.pes"),
            "pes",
        )
        .unwrap();
        let source = &file.blocks[0].runs[0];
        let model = FillRun::recover(source).unwrap();
        assert!(model.uncertain_spans().is_empty());
        for scale in [0.5, 2.0] {
            let output = model.generate(crate::geom::scale_about(P::default(), scale, scale));
            if scale < 1.0 {
                assert!(output.len() < source.len() * 3 / 4);
            } else {
                assert!(output.len() > source.len() * 5 / 2);
            }
            let center = P::new(103.0, 190.0);
            for w in output.windows(2) {
                let a = w[0] * (1.0 / scale);
                let b = w[1] * (1.0 / scale);
                let delta = b - a;
                let t = if delta.len() > 1e-8 {
                    ((center - a).dot(delta) / delta.dot(delta)).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                assert!(
                    a.lerp(b, t).dist(center) > 20.0,
                    "resizing sewed across the open counter"
                );
            }
        }
    }
    #[test]
    fn satin_and_decorative_motifs_are_not_fills() {
        assert!(FillRun::recover(&crate::reconstruct::tests::satin_sample()).is_none());
        let f = crate::formats::read::read_stitch_file(
            include_bytes!("../../examples/pes/Handstitch/1.5in/0.pes"),
            "pes",
        )
        .unwrap();
        assert!(FillRun::recover(&f.blocks[0].runs[0]).is_none());
    }
}
