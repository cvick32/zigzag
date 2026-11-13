//! Recover repeated decorative motifs along a running-stitch centerline.
//! Require geometrically matching copies, not merely periodic stitch lengths.
use super::shared::{Gaps, join_pieces};
use crate::geom::{Affine, P, Polyline, affine_apply, sample_by_arclength};

struct MotifSection {
    start: usize,
    period: usize,
    copies: usize,
}
pub struct MotifRun {
    pub original: Polyline,
    sections: Vec<MotifSection>,
    gaps: Gaps,
}

fn coordinates(points: &[P]) -> Option<Polyline> {
    let a = points[0];
    let delta = *points.last()? - a;
    let length = delta.len();
    if !(12.0..=65.0).contains(&length) {
        return None;
    }
    let tangent = delta.norm();
    Some(
        points
            .iter()
            .map(|p| {
                let v = (*p - a) * (1.0 / length);
                P::new(v.dot(tangent), v.dot(tangent.perp()))
            })
            .collect(),
    )
}

fn decorative(template: &[P], length: f64) -> bool {
    let low = template.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let high = template
        .iter()
        .map(|p| p.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let height = low.abs().max(high.abs());
    (0.18..=0.8).contains(&height) && low <= -0.12 && high >= 0.12
        // A phase starting in the middle of a motif can project far beyond
        // its endpoint anchors after resampling. Anchor complete motifs at
        // their longitudinal extremes, allowing only small digitizer jitter.
        && template.iter().all(|p|(-4.0 / length..=1.0 + 4.0 / length).contains(&p.x))
}
fn matches(a: &[P], b: &[P]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let errors: Vec<_> = a.iter().zip(b).map(|(a, b)| a.dist(*b)).collect();
    errors.iter().all(|e| *e <= 0.18)
        && errors.iter().map(|e| e * e).sum::<f64>() / errors.len() as f64 <= 0.08 * 0.08
}
impl MotifSection {
    fn end(&self) -> usize {
        self.start + self.period * self.copies
    }
    fn seed(points: &[P], start: usize) -> Option<Self> {
        // Decorative stars repeatedly reverse their sewing direction. This
        // prefilter avoids searching every phase of ordinary long fill rows.
        if start + 6 >= points.len() {
            return None;
        }
        let steps: Vec<_> = points[start..=start + 6]
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect();
        if steps
            .windows(2)
            .filter(|w| {
                w[0].len() >= 8.0 && w[1].len() >= 8.0 && w[0].norm().dot(w[1].norm()) < -0.3
            })
            .count()
            < 4
        {
            return None;
        }
        let mut best: Option<Self> = None;
        for period in 6..=24 {
            if start + 4 * period >= points.len() {
                continue;
            }
            let Some(template) = coordinates(&points[start..=start + period]) else {
                continue;
            };
            if !decorative(&template, points[start].dist(points[start + period])) {
                continue;
            }
            let copies = Self::copies(points, start, period, &template, points.len() - 1);
            if copies >= 4
                && best
                    .as_ref()
                    .is_none_or(|s| copies * period > s.copies * s.period)
            {
                best = Some(Self {
                    start,
                    period,
                    copies,
                });
            }
        }
        best
    }
    fn copies(points: &[P], start: usize, period: usize, template: &[P], end: usize) -> usize {
        let mut copies = 0;
        while start + (copies + 1) * period <= end {
            let at = start + copies * period;
            let Some(next) = coordinates(&points[at..=at + period]) else {
                break;
            };
            if !decorative(&next, points[at].dist(points[at + period])) || !matches(template, &next)
            {
                break;
            }
            copies += 1;
        }
        copies
    }
    fn generate(&self, original: &[P], m: Affine) -> Polyline {
        let centers: Vec<_> = (0..=self.copies)
            .map(|i| affine_apply(m, original[self.start + i * self.period]))
            .collect();
        let lengths: Vec<_> = (0..self.copies)
            .map(|i| {
                original[self.start + i * self.period]
                    .dist(original[self.start + (i + 1) * self.period])
            })
            .collect();
        let spacing = super::median(&lengths);
        let mut path = Vec::new();
        let mut at = 0;
        for i in 1..centers.len() {
            if i + 1 == centers.len()
                || (centers[i] - centers[i - 1])
                    .norm()
                    .dot((centers[i + 1] - centers[i]).norm())
                    < 0.866
            {
                path.extend(
                    sample_by_arclength(&centers[at..=i], spacing)
                        .into_iter()
                        .map(|p| p.0),
                );
                at = i;
            }
        }
        path.dedup_by(|a, b| a.dist(*b) < 1e-8);
        let mut out = vec![affine_apply(m, original[self.start])];
        for (i, pair) in path.windows(2).enumerate() {
            let source = self.start
                + (i * self.copies / (path.len() - 1)).min(self.copies - 1) * self.period;
            let template = coordinates(&original[source..=source + self.period]).unwrap();
            let length = original[source].dist(original[source + self.period]);
            let axis = (pair[1] - pair[0]).norm();
            let source_axis = (original[source + self.period] - original[source]).norm();
            let vector = |v: P| P::new(m[0] * v.x + m[2] * v.y, m[1] * v.x + m[3] * v.y);
            let mapped_axis = vector(source_axis).norm();
            let mapped_normal = vector(source_axis.perp());
            let normal = axis * mapped_normal.dot(mapped_axis)
                + axis.perp() * mapped_normal.dot(mapped_axis.perp());
            for p in template.iter().skip(1) {
                out.push(pair[0].lerp(pair[1], p.x) + normal * (p.y * length));
            }
        }
        out.dedup_by(|a, b| a.dist(*b) < 1e-8);
        out
    }
}
impl MotifRun {
    pub fn recover(points: &[P]) -> Option<Self> {
        if points.len() < 49 || points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
            return None;
        }
        let mut sections = Vec::new();
        let mut cursor = 0;
        while cursor + 24 < points.len() {
            if let Some(section) = MotifSection::seed(points, cursor) {
                cursor = section.end();
                sections.push(section);
            } else {
                cursor += 1;
            }
        }
        if sections.is_empty() {
            return None;
        }
        // Once four copies establish a motif, short occurrences of that exact
        // geometry can be recognized elsewhere in the same source run.
        let templates: Vec<_> = sections
            .iter()
            .map(|s| {
                (
                    s.period,
                    coordinates(&points[s.start..=s.start + s.period]).unwrap(),
                )
            })
            .collect();
        let mut bounds = Vec::new();
        cursor = 0;
        for s in &sections {
            if cursor < s.start {
                bounds.push((cursor, s.start));
            }
            cursor = s.end();
        }
        if cursor + 1 < points.len() {
            bounds.push((cursor, points.len() - 1));
        }
        for (start, end) in bounds {
            cursor = start;
            while cursor + 6 <= end {
                let mut best: Option<MotifSection> = None;
                for (period, template) in &templates {
                    if cursor + period > end {
                        continue;
                    }
                    let copies = MotifSection::copies(points, cursor, *period, template, end);
                    if copies > 0
                        && best
                            .as_ref()
                            .is_none_or(|s| copies * period > s.copies * s.period)
                    {
                        best = Some(MotifSection {
                            start: cursor,
                            period: *period,
                            copies,
                        });
                    }
                }
                if let Some(section) = best {
                    cursor = section.end();
                    sections.push(section);
                } else {
                    cursor += 1;
                }
            }
        }
        sections.sort_by_key(|s| s.start);
        let gaps = Gaps::recover(points, sections.iter().map(|s| (s.start, s.end())));
        Some(Self {
            original: points.to_vec(),
            sections,
            gaps,
        })
    }
    pub fn satin_count(&self) -> usize {
        self.gaps.satin_count()
    }
    pub fn uncertain_spans(&self) -> Vec<(usize, usize)> {
        self.gaps.uncertain_spans()
    }
    pub fn generate(&self, m: Affine) -> Polyline {
        let mut pieces: Vec<_> = self
            .sections
            .iter()
            .map(|s| (s.start, s.generate(&self.original, m)))
            .collect();
        pieces.extend(self.gaps.generate(&self.original, m));
        join_pieces(pieces)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downloaded_handstitch_is_a_repeated_motif() {
        let f = crate::formats::read::read_stitch_file(
            include_bytes!("../../examples/pes/Handstitch/1.5in/0.pes"),
            "pes",
        )
        .unwrap();
        let m = MotifRun::recover(&f.blocks[0].runs[0]).unwrap();
        assert_eq!(m.sections[0].period, 12);
        assert!(m.uncertain_spans().is_empty());
        for scale in [0.5, 2.0] {
            let p = m.generate(crate::geom::scale_about(P::default(), scale, scale));
            let ratio = p.len() as f64 / m.original.len() as f64;
            assert!((ratio - scale).abs() < 0.12, "{ratio}");
            assert!(p.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
        }
    }
    #[test]
    fn narrow_letter_scales_width_without_scaling_motif_spacing() {
        let f = crate::formats::read::read_stitch_file(
            include_bytes!("../../examples/pes/Handstitch/1.5in/IUpper.pes"),
            "pes",
        )
        .unwrap();
        let m = MotifRun::recover(&f.blocks[0].runs[0]).unwrap();
        let bounds = |p: &[P]| {
            let lo = p.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
            let hi = p.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
            hi - lo
        };
        for scale in [0.5, 2.0] {
            let p = m.generate(crate::geom::scale_about(P::default(), scale, scale));
            assert!((bounds(&p) - scale * bounds(&m.original)).abs() < 0.1);
            let ratio = p.len() as f64 / m.original.len() as f64;
            assert!((ratio - scale).abs() < 0.12);
        }
    }
    #[test]
    fn downloaded_spacing_changes_and_connectors_are_reconstructed() {
        for bytes in [
            include_bytes!("../../examples/pes/Handstitch/1.5in/5.pes").as_slice(),
            include_bytes!("../../examples/pes/Handstitch/1.5in/8.pes").as_slice(),
            include_bytes!("../../examples/pes/Handstitch/1.5in/DUpper.pes").as_slice(),
            include_bytes!("../../examples/pes/Handstitch/1.5in/1.pes").as_slice(),
        ] {
            let file = crate::formats::read::read_stitch_file(bytes, "pes").unwrap();
            let source = &file.blocks[0].runs[0];
            let model = MotifRun::recover(source).unwrap();
            assert!(model.uncertain_spans().is_empty());
            for scale in [0.5, 2.0] {
                let output = model.generate(crate::geom::scale_about(P::default(), scale, scale));
                if scale < 1.0 {
                    assert!(output.len() < source.len());
                } else {
                    assert!(output.len() > source.len());
                }
                assert!(output.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
            }
        }
    }
    #[test]
    fn interrupted_motif_is_preserved_and_flagged() {
        let file = crate::formats::read::read_stitch_file(
            include_bytes!("../../examples/pes/Handstitch/1.5in/AUpper.pes"),
            "pes",
        )
        .unwrap();
        let source = &file.blocks[0].runs[0];
        let model = MotifRun::recover(source).unwrap();
        assert!(model.sections.iter().any(|s| s.start == 0 && s.copies == 3));
        assert!(!model.uncertain_spans().is_empty());
        let matrix = crate::geom::scale_about(P::default(), 2.0, 2.0);
        let generated = model.generate(matrix);
        for (a, b) in model.uncertain_spans() {
            for p in &source[a..=b] {
                assert!(
                    generated
                        .iter()
                        .any(|q| q.dist(affine_apply(matrix, *p)) < 1e-8)
                );
            }
        }
    }
    #[test]
    fn shrinking_motif_phases_stays_inside_the_requested_outline() {
        for bytes in [
            include_bytes!("../../examples/pes/Handstitch/1.5in/AUpper.pes").as_slice(),
            include_bytes!("../../examples/pes/Handstitch/1in/AUpper.pes").as_slice(),
            include_bytes!("../../examples/pes/Handstitch/2in/FUpper.pes").as_slice(),
            include_bytes!("../../examples/pes/Handstitch/2in/aLower.pes").as_slice(),
            include_bytes!("../../examples/pes/Handstitch/3in/tLower.pes").as_slice(),
        ] {
            let file = crate::formats::read::read_stitch_file(bytes, "pes").unwrap();
            for source in &file.blocks[0].runs {
                let Some(model) = MotifRun::recover(source) else {
                    continue;
                };
                let output = model.generate(crate::geom::scale_about(P::default(), 0.5, 0.5));
                for axis in [|p: P| p.x, |p: P| p.y] {
                    let low = source
                        .iter()
                        .map(|p| axis(*p))
                        .fold(f64::INFINITY, f64::min)
                        * 0.5;
                    let high = source
                        .iter()
                        .map(|p| axis(*p))
                        .fold(f64::NEG_INFINITY, f64::max)
                        * 0.5;
                    assert!(
                        output
                            .iter()
                            .all(|p| axis(*p) >= low - 5.0 && axis(*p) <= high + 5.0)
                    );
                }
            }
        }
    }

    #[test]
    fn ordinary_fills_and_satins_are_not_motifs() {
        let narrow: Vec<_> = (0..200)
            .map(|i| P::new(i as f64 * 2.0, if i % 2 == 0 { 0.0 } else { 10.0 }))
            .collect();
        assert!(MotifRun::recover(&narrow).is_none());
        assert!(MotifRun::recover(&crate::reconstruct::tests::satin_sample()).is_none());
        assert!(MotifRun::recover(&crate::reconstruct::tests::fill_sample()).is_none());
    }
}
