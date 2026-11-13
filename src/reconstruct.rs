//! Conservative recovery of simple stitch runs. Coordinates are 0.1 mm.
//!
//! `recover` accepts complete regular runs; `SatinRun` separates curved satin
//! sections from connecting stitches and retains unrecognized spans. Neither
//! path infers a filled polygon across the open center of satin lettering.

use crate::bezier::{Anchor, BezPath, SubPath};
use crate::geom::{P, Polyline};
use crate::params::Kind;

pub mod fill_run;
pub mod motif_run;
pub mod repeated_run;
mod satin_run;
mod shared;
pub use satin_run::SatinRun;

/// Source model retained across resizes, rather than re-inferring generated stitches.
pub enum StitchRun {
    Satin(SatinRun),
    Fill(fill_run::FillRun),
    Motif(motif_run::MotifRun),
    Repeated(repeated_run::RepeatedRun),
}
impl StitchRun {
    pub fn recover(points: &[P]) -> Option<Self> {
        if let Some(model) = repeated_run::RepeatedRun::recover(points)
            && model.covered_fraction() >= 0.8
        {
            return Some(Self::Repeated(model));
        }
        let motif = motif_run::MotifRun::recover(points).map(Self::Motif);
        let fill = fill_run::FillRun::recover(points).map(Self::Fill);
        let satin = SatinRun::recover(points).map(Self::Satin);
        [motif, fill, satin].into_iter().flatten().min_by_key(|m| {
            m.uncertain_spans()
                .iter()
                .map(|(a, b)| b - a)
                .sum::<usize>()
        })
    }
    pub fn original(&self) -> &Polyline {
        match self {
            Self::Satin(m) => &m.original,
            Self::Fill(m) => &m.original,
            Self::Repeated(m) => &m.original,
            Self::Motif(m) => &m.original,
        }
    }
    pub fn generate(&self, matrix: crate::geom::Affine) -> Polyline {
        match self {
            Self::Satin(m) => m.generate(matrix),
            Self::Fill(m) => m.generate(matrix),
            Self::Repeated(m) => m.generate(matrix),
            Self::Motif(m) => m.generate(matrix),
        }
    }
    pub fn uncertain_spans(&self) -> Vec<(usize, usize)> {
        match self {
            Self::Satin(m) => m.uncertain_spans(),
            Self::Fill(m) => m.uncertain_spans(),
            Self::Repeated(m) => m.uncertain_spans(),
            Self::Motif(m) => m.uncertain_spans(),
        }
    }
    pub fn satin_count(&self) -> usize {
        match self {
            Self::Satin(m) => m.section_count(),
            Self::Fill(m) => m.satin_count(),
            Self::Motif(m) => m.satin_count(),
            _ => 0,
        }
    }
    pub fn fill_count(&self) -> usize {
        match self {
            Self::Fill(m) => m.fill_count(),
            _ => 0,
        }
    }
    pub fn repeated_count(&self) -> usize {
        usize::from(matches!(self, Self::Repeated(_) | Self::Motif(_)))
    }
}

pub struct Recovered {
    pub path: BezPath,
    pub kind: Kind,
    pub spacing_mm: f64,
    pub length_mm: f64,
    pub angle: f64,
}

fn subpath(points: &[P], closed: bool) -> SubPath {
    SubPath {
        anchors: points.iter().copied().map(Anchor::corner).collect(),
        closed,
    }
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn regular(values: &[f64], lo: f64, hi: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let m = median(values);
    (m >= lo && m <= hi && values.iter().all(|v| (*v - m).abs() <= (m * 0.3).max(0.8))).then_some(m)
}

pub fn recover(points: &[P]) -> Result<Recovered, &'static str> {
    if points.len() < 16 || points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
        return Err("too few usable stitches to identify a fill or satin");
    }
    // Recognize only the explicit short A-B-A-B-A locking pattern at a
    // run's ends. Export adds new ties; other small stitches are not discarded.
    let mut pts = points.to_vec();
    pts.dedup();
    let tie = |p: &[P]| {
        p.len() >= 5 && p[0] == p[2] && p[0] == p[4] && p[1] == p[3] && p[0].dist(p[1]) <= 6.0
    };
    if tie(&pts) {
        pts.drain(..4);
    }
    if pts.len() >= 5 && tie(&pts[pts.len() - 5..]) {
        pts.truncate(pts.len() - 4);
    }
    fill(&pts).or_else(|| satin(&pts)).ok_or(
        "mixed or irregular stitching; cannot confidently separate fill, satin, underlay or travel",
    )
}

fn fill(pts: &[P]) -> Option<Recovered> {
    let seed = pts
        .windows(2)
        .map(|w| w[1] - w[0])
        .max_by(|a, b| a.len().total_cmp(&b.len()))?
        .norm();
    // Average whole-row travel rather than trusting one quantized PES stitch.
    let axis = pts
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|d| d.len() >= 10.0 && d.norm().dot(seed).abs() > 0.98)
        .fold(P::default(), |sum, d| sum + d * d.dot(seed).signum())
        .norm();
    let normal = axis.perp();
    // Each row must contain several collinear stitches. The short connector
    // between rows is excluded from the measured stitch length.
    let mut rows: Vec<&[P]> = Vec::new();
    let mut start = 0;
    let mut row_y = pts[0].dot(normal);
    for i in 1..pts.len() {
        let y = pts[i].dot(normal);
        if (y - row_y).abs() > 0.9 {
            rows.push(&pts[start..i]);
            start = i;
            row_y = y;
        } else {
            row_y += (y - row_y) / (i - start + 1) as f64;
        }
    }
    rows.push(&pts[start..]);
    if rows.len() < 6 || rows.iter().any(|r| r.len() < 4) {
        return None;
    }
    let ys: Vec<f64> = rows
        .iter()
        .map(|r| r.iter().map(|p| p.dot(normal)).sum::<f64>() / r.len() as f64)
        .collect();
    let steps: Vec<f64> = ys.windows(2).map(|w| w[1] - w[0]).collect();
    let sign = steps[0].signum();
    if steps.iter().any(|s| s.signum() != sign) {
        return None;
    }
    let spacing = regular(&steps.iter().map(|s| s.abs()).collect::<Vec<_>>(), 2.0, 8.0)?;
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut lengths = Vec::new();
    let mut previous_dir = 0.0;
    for row in &rows {
        let dir = (row.last()?.dot(axis) - row[0].dot(axis)).signum();
        if dir == 0.0 || dir == previous_dir {
            return None;
        }
        previous_dir = dir;
        for pair in row.windows(2) {
            let d = pair[1] - pair[0];
            if d.dot(axis) * dir < 1.0 || d.dot(normal).abs() > 1.5 {
                return None;
            }
            lengths.push(d.len());
        }
        let (a, b) = if dir > 0.0 {
            (row[0], *row.last()?)
        } else {
            (*row.last()?, row[0])
        };
        if a.dist(b) < 40.0 {
            return None;
        }
        left.push(a);
        right.push(b);
    }
    let length = median(&lengths);
    if !(10.0..=60.0).contains(&length) || lengths.iter().any(|l| *l > length * 1.8) {
        return None;
    }
    // Restrict this first version to gently varying, single-span boundaries.
    // Large end-point jumps can indicate holes or adjacent stitched regions.
    if [&left, &right].iter().any(|rail| {
        rail.windows(2)
            .any(|w| (w[1] - w[0]).dot(axis).abs() > spacing * 2.0)
    }) {
        return None;
    }
    let ring: Polyline = left.into_iter().chain(right.into_iter().rev()).collect();
    Some(Recovered {
        path: vec![subpath(&ring, true)],
        kind: Kind::Fill,
        spacing_mm: spacing / 10.0,
        length_mm: length / 10.0,
        angle: axis.y.atan2(axis.x).to_degrees(),
    })
}

fn satin(pts: &[P]) -> Option<Recovered> {
    if pts.len() < 24 || !pts.len().is_multiple_of(2) {
        return None;
    }
    let a: Polyline = pts.iter().step_by(2).copied().collect();
    let b: Polyline = pts.iter().skip(1).step_by(2).copied().collect();
    let steps: Vec<f64> = a
        .windows(2)
        .zip(b.windows(2))
        .map(|(a, b)| (a[0].dist(a[1]) + b[0].dist(b[1])) * 0.5)
        .collect();
    let spacing = regular(&steps, 2.0, 8.0)?;
    let forward = ((*a.last()? + *b.last()?) - (a[0] + b[0])).norm();
    for i in 0..a.len() {
        let cross = b[i] - a[i];
        if cross.len() < (spacing * 3.0).max(10.0) || cross.len() > 70.0 {
            return None;
        }
        if i > 0 {
            let da = a[i] - a[i - 1];
            let db = b[i] - b[i - 1];
            if da.len() < 0.8
                || db.len() < 0.8
                || da.norm().dot(db.norm()) < 0.8
                || cross.norm().dot((b[i - 1] - a[i - 1]).norm()) < 0.95
                || da.norm().dot(cross.norm()).abs() > 0.5
                || db.norm().dot(cross.norm()).abs() > 0.5
                || (da + db).norm().dot(forward) < 0.25
            {
                return None;
            }
        }
        if i > 1
            && ((a[i] - a[i - 1]).norm().dot((a[i - 1] - a[i - 2]).norm()) < 0.8
                || (b[i] - b[i - 1]).norm().dot((b[i - 1] - b[i - 2]).norm()) < 0.8)
        {
            return None;
        }
    }
    Some(Recovered {
        path: vec![subpath(&a, false), subpath(&b, false)],
        kind: Kind::Satin,
        spacing_mm: spacing / 10.0,
        length_mm: 2.5,
        angle: 0.0,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::geom::{affine_apply, rotate_about};

    pub fn fill_sample() -> Polyline {
        (0..30)
            .flat_map(|row| {
                let mut xs = vec![0.0];
                let offset = 5.0 + (row % 4) as f64 * 5.0;
                xs.extend(
                    (0..10)
                        .map(|i| i as f64 * 25.0 + offset)
                        .filter(|x| *x < 250.0),
                );
                xs.push(250.0);
                if row % 2 == 1 {
                    xs.reverse();
                }
                xs.into_iter().map(move |x| P::new(x, row as f64 * 4.0))
            })
            .collect()
    }

    pub fn satin_sample() -> Polyline {
        (0..40)
            .flat_map(|i| [P::new(0.0, i as f64 * 4.0), P::new(30.0, i as f64 * 4.0)])
            .collect()
    }

    #[test]
    fn recognizes_rotated_staggered_fill_and_satin() {
        for (pts, kind) in [(fill_sample(), Kind::Fill), (satin_sample(), Kind::Satin)] {
            for deg in [0.0, 37.0, 90.0, -120.0] {
                let m = rotate_about(P::default(), deg);
                let rotated: Vec<_> = pts.iter().map(|p| affine_apply(m, *p)).collect();
                let r = recover(&rotated).unwrap();
                assert_eq!(r.kind, kind);
                assert!((r.spacing_mm - 0.4).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn recognizes_quantized_rotated_fill() {
        let m = rotate_about(P::default(), 37.0);
        let pts: Vec<_> = fill_sample()
            .into_iter()
            .map(|p| {
                let p = affine_apply(m, p);
                P::new(p.x.round(), p.y.round())
            })
            .collect();
        let r = recover(&pts).unwrap();
        assert_eq!(r.kind, Kind::Fill);
        assert!((r.spacing_mm - 0.4).abs() < 0.02);
    }

    #[test]
    fn rejects_running_stitches_underlay_and_disconnected_regions() {
        let line: Vec<_> = (0..60).map(|i| P::new(i as f64 * 20.0, 0.0)).collect();
        assert!(recover(&line).is_err());
        for mut pts in [fill_sample(), satin_sample()] {
            pts.splice(
                0..0,
                [P::new(15.0, 0.0), P::new(15.0, 40.0), P::new(15.0, 80.0)],
            );
            assert!(recover(&pts).is_err());
        }
        let mut pts = fill_sample();
        let mid = pts.len() / 2;
        for p in &mut pts[mid..] {
            p.x += 200.0;
        }
        assert!(recover(&pts).is_err());
        let mut pts = satin_sample();
        pts[20].x = 60.0;
        assert!(recover(&pts).is_err());
    }
}
