//! Shared handling of source spans between reconstructed sections.
//! Span endpoints are inclusive; adjacent sections share their boundary stitch.
use super::SatinRun;
use crate::geom::{Affine, P, Polyline, affine_apply};

struct Gap {
    start: usize,
    end: usize,
    model: Option<SatinRun>,
}

pub(super) struct Gaps(Vec<Gap>);

/// Sections must be supplied in source order without overlapping interiors.
pub(super) fn gaps_between(
    point_count: usize,
    sections: impl IntoIterator<Item = (usize, usize)>,
) -> Vec<(usize, usize)> {
    let mut gaps = Vec::new();
    let mut cursor = 0;
    for (start, end) in sections {
        if cursor < start {
            gaps.push((cursor, start));
        }
        cursor = end;
    }
    if cursor + 1 < point_count {
        gaps.push((cursor, point_count - 1));
    }
    gaps
}

impl Gaps {
    pub(super) fn recover(
        points: &[P],
        sections: impl IntoIterator<Item = (usize, usize)>,
    ) -> Self {
        Self(
            gaps_between(points.len(), sections)
                .into_iter()
                .map(|(start, end)| {
                    let slice = &points[start..=end];
                    Gap {
                        start,
                        end,
                        model: SatinRun::recover(slice)
                            .or_else(|| SatinRun::connector_model(slice)),
                    }
                })
                .collect(),
        )
    }

    pub(super) fn satin_count(&self) -> usize {
        self.0
            .iter()
            .filter_map(|g| g.model.as_ref())
            .map(|m| m.section_count())
            .sum()
    }

    pub(super) fn uncertain_spans(&self) -> Vec<(usize, usize)> {
        self.0
            .iter()
            .flat_map(|g| match &g.model {
                Some(m) => m
                    .uncertain_spans()
                    .into_iter()
                    .map(|(a, b)| (a + g.start, b + g.start))
                    .collect::<Vec<_>>(),
                None => vec![(g.start, g.end)],
            })
            .collect()
    }

    pub(super) fn generate(&self, original: &[P], m: Affine) -> Vec<(usize, Polyline)> {
        self.0
            .iter()
            .map(|g| {
                (
                    g.start,
                    match &g.model {
                        Some(model) => model.generate(m),
                        None => original[g.start..=g.end]
                            .iter()
                            .map(|p| affine_apply(m, *p))
                            .collect(),
                    },
                )
            })
            .collect()
    }
}

/// Restore sewing order and remove duplicate boundary stitches only.
pub(super) fn join_pieces(mut pieces: Vec<(usize, Polyline)>) -> Polyline {
    pieces.sort_by_key(|p| p.0);
    let mut out: Polyline = pieces.into_iter().flat_map(|p| p.1).collect();
    out.dedup_by(|a, b| a.dist(*b) < 1e-8);
    out
}
