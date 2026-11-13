//! Detect repeated running stitches with small digitizer jitter.
//! Unlike satin, these revisit the same two endpoints before progressing.
use crate::geom::{Affine, P, Polyline, affine_apply, sample_by_arclength};
use std::collections::HashMap;

#[derive(Debug)]
pub struct RepeatedBlock {
    pub start: usize,
    pub end: usize,
    pub a: P,
    pub b: P,
    pub passes: usize,
    pub needles: Polyline,
}
#[derive(Debug)]
pub struct RepeatedRun {
    pub original: Polyline,
    pub blocks: Vec<RepeatedBlock>,
}
impl RepeatedRun {
    fn gap_supported(&self, a: usize, b: usize) -> bool {
        let p = &self.original[a..=b];
        b - a <= 12
            && p[0].dist(*p.last().unwrap()) <= 3.0
            && p.iter().all(|q| q.dist(p[0]) <= 15.0)
    }
    pub fn uncertain_spans(&self) -> Vec<(usize, usize)> {
        super::shared::gaps_between(
            self.original.len(),
            self.blocks.iter().map(|b| (b.start, b.end)),
        )
        .into_iter()
        .filter(|&(a, b)| !self.gap_supported(a, b))
        .collect()
    }
    pub fn covered_fraction(&self) -> f64 {
        self.blocks.iter().map(|b| b.end - b.start).sum::<usize>() as f64
            / (self.original.len() - 1) as f64
    }
    pub fn generate(&self, m: Affine) -> Polyline {
        let mut out = Vec::new();
        let mut cursor = 0;
        let mut first = 0;
        while first < self.blocks.len() {
            let mut end = first + 1;
            while end < self.blocks.len()
                && self.blocks[end - 1].end == self.blocks[end].start
                && self.blocks[end - 1].passes == self.blocks[end].passes
            {
                end += 1;
            }
            let chain = &self.blocks[first..end];
            let start = chain[0].start;
            out.extend(
                self.original[if first == 0 { 0 } else { cursor + 1 }..=start]
                    .iter()
                    .map(|p| affine_apply(m, *p)),
            );
            let lengths: Vec<_> = chain.iter().map(|b| b.a.dist(b.b)).collect();
            let length = super::median(&lengths);
            let mut centers = vec![affine_apply(m, chain[0].a)];
            centers.extend(chain.iter().map(|b| affine_apply(m, b.b)));
            // Keep sharp turns; resample smooth stretches across source block
            // boundaries so shrinking removes stitches as enlargement adds them.
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
                        sample_by_arclength(&centers[at..=i], length)
                            .into_iter()
                            .map(|p| p.0),
                    );
                    at = i;
                }
            }
            path.dedup_by(|a, b| a.dist(*b) < 1e-8);
            for (k, pair) in path.windows(2).enumerate() {
                let template = &chain[(k * chain.len() / (path.len() - 1)).min(chain.len() - 1)];
                let source = (template.b - template.a).norm();
                let target = (pair[1] - pair[0]).norm();
                for j in 1..=template.passes {
                    let (base, anchor) = if j % 2 == 1 {
                        (pair[1], template.b)
                    } else {
                        (pair[0], template.a)
                    };
                    let delta = template.needles[j] - anchor;
                    out.push(
                        base + target * delta.dot(source)
                            + target.perp() * delta.dot(source.perp()),
                    );
                }
            }
            cursor = chain.last().unwrap().end;
            first = end;
        }
        out.extend(
            self.original[cursor + 1..]
                .iter()
                .map(|p| affine_apply(m, *p)),
        );
        out.dedup_by(|a, b| a.dist(*b) < 1e-8);
        out
    }
    pub fn recover(points: &[P]) -> Option<Self> {
        if points.len() < 30 || points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
            return None;
        }
        // PES positions are quantized to 0.1 mm. The Floss corpus revisits
        // endpoints with up to 0.45 mm jitter; require at least 1.2 mm between
        // endpoints so those clusters remain distinct.
        let mut nodes: Vec<P> = Vec::new();
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        let mut visits: Vec<(usize, usize)> = Vec::new();
        for (index, &p) in points.iter().enumerate() {
            let cell = ((p.x / 5.0).floor() as i64, (p.y / 5.0).floor() as i64);
            let mut best = None;
            for x in cell.0 - 1..=cell.0 + 1 {
                for y in cell.1 - 1..=cell.1 + 1 {
                    if let Some(ids) = cells.get(&(x, y)) {
                        for &id in ids {
                            let distance = p.dist(nodes[id]);
                            if distance <= 4.5 && best.is_none_or(|(_, d)| distance < d) {
                                best = Some((id, distance));
                            }
                        }
                    }
                }
            }
            let id = best.map(|v| v.0).unwrap_or_else(|| {
                let id = nodes.len();
                nodes.push(p);
                cells.entry(cell).or_default().push(id);
                id
            });
            if visits.last().is_none_or(|v| v.0 != id) {
                visits.push((id, index));
            }
        }
        let mut blocks = Vec::new();
        let mut i = 0;
        while i + 1 < visits.len() {
            let a = visits[i].0;
            let b = visits[i + 1].0;
            let mut end = i + 1;
            while end + 1 < visits.len()
                && visits[end + 1].0
                    == if (end + 1 - i).is_multiple_of(2) {
                        a
                    } else {
                        b
                    }
            {
                end += 1;
            }
            let passes = end - i;
            if passes >= 5
                && !passes.is_multiple_of(2)
                && (12.0..=60.0).contains(&nodes[a].dist(nodes[b]))
            {
                blocks.push(RepeatedBlock {
                    start: visits[i].1,
                    end: visits[end].1,
                    a: nodes[a],
                    b: nodes[b],
                    passes,
                    needles: visits[i..=end].iter().map(|v| points[v.1]).collect(),
                });
                i = end;
            } else {
                i += 1;
            }
        }
        if blocks.len() < 4 {
            return None;
        }
        Some(Self {
            original: points.to_vec(),
            blocks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_passes_are_distinct_from_satin() {
        let mut points = vec![P::new(0.0, 0.0)];
        for i in 0..12 {
            let a = P::new(i as f64 * 25.0, 0.0);
            let b = P::new((i + 1) as f64 * 25.0, 0.0);
            for j in 0..9 {
                let p = if j % 2 == 0 { b } else { a };
                points.push(p + P::new(0.0, if j % 3 == 0 { 1.0 } else { 0.0 }));
            }
        }
        let model = RepeatedRun::recover(&points).unwrap();
        assert_eq!(model.blocks.len(), 12);
        assert!(model.blocks.iter().all(|b| b.passes == 9));
        assert!(RepeatedRun::recover(&crate::reconstruct::tests::satin_sample()).is_none());
        assert!(RepeatedRun::recover(&crate::reconstruct::tests::fill_sample()).is_none());
    }
    #[test]
    fn repeated_run_resize_preserves_pass_count_length_and_jitter() {
        let mut points = vec![P::new(0.0, 0.0)];
        for i in 0..12 {
            for j in 1..=9 {
                let x = (i + j % 2) as f64 * 25.0;
                points.push(P::new(x, if j % 3 == 0 { 1.0 } else { 0.0 }));
            }
        }
        let model = RepeatedRun::recover(&points).unwrap();
        for scale in [0.5, 2.0] {
            let output = model.generate(crate::geom::scale_about(P::default(), scale, scale));
            let recovered = RepeatedRun::recover(&output).unwrap();
            assert_eq!(recovered.blocks.len(), (12.0 * scale) as usize);
            assert!(recovered.blocks.iter().all(|b| b.passes == 9));
            assert!(
                recovered
                    .blocks
                    .iter()
                    .all(|b| (b.a.dist(b.b) - 25.0).abs() < 0.1)
            );
            let height = output.iter().map(|p| p.y).fold(0.0, f64::max);
            assert!((height - 1.0).abs() < 0.1, "jitter must not grow: {height}");
        }
    }
    #[test]
    fn floss_fixture_has_repeated_running_blocks() {
        for bytes in [
            include_bytes!("../../examples/pes/Floss/.5in/0.pes").as_slice(),
            include_bytes!("../../examples/pes/Floss/1in/0.pes").as_slice(),
            include_bytes!("../../examples/pes/Floss/4in/0.pes").as_slice(),
        ] {
            let file = crate::formats::read::read_stitch_file(bytes, "pes").unwrap();
            let m = RepeatedRun::recover(&file.blocks[0].runs[0]).unwrap();
            assert!(m.blocks.len() >= 12, "{}", m.blocks.len());
            assert!(m.blocks.iter().all(|b| b.passes >= 5));
            assert!(m.covered_fraction() >= 0.8);
            assert!(m.uncertain_spans().is_empty());
        }
    }
}
