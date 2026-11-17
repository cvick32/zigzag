//! Convert confidently recognized manual runs into editable stitch objects.

use super::{Document, elements_by_id, xml_escape};
use crate::geom::{Affine, P, affine_inv, affine_mul};
use crate::params::Kind;
use crate::reconstruct::StitchRun;
use crate::svg::{Geometry, Part};
use anyhow::{Result, anyhow};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct DensityWarning {
    pub uid: usize,
    pub label: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct DensityReport {
    pub fills: usize,
    pub satins: usize,
    pub repeated_runs: usize,
    pub warnings: Vec<DensityWarning>,
}

impl Document {
    fn stitch_source_xml(&self, index: usize) -> Result<String> {
        let xml = roxmltree::Document::parse(&self.xml)?;
        let nodes = elements_by_id(&xml);
        Ok(self.xml[nodes[self.shapes[index].id.as_str()].range()].to_owned())
    }
    // Store source coordinates in the element's local coordinate system via
    // `to_local`. Its ordinary SVG transform then handles moves, rotations,
    // groups and subsequent resizes without compounding reconstruction error.
    fn stitch_source(&self, index: usize) -> Result<Option<(StitchRun, Affine)>> {
        let xml = roxmltree::Document::parse(&self.xml)?;
        let nodes = elements_by_id(&xml);
        let node = nodes[self.shapes[index].id.as_str()];
        let Some(raw) = node.attribute("data-zigzag-density-source") else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_str(raw)?;
        if value["version"].as_u64() != Some(1) {
            return Err(anyhow!("unsupported saved density reconstruction version"));
        }
        if value["generated"].as_str() != node.attribute("d") {
            // Point editing changes the path. Never overwrite the user's edits
            // with a stale reconstruction: infer from their new stitches instead.
            return Ok(None);
        }
        let read = || -> Option<(Vec<P>, Affine)> {
            let points = value["points"]
                .as_array()?
                .iter()
                .map(|p| Some(P::new(p.get(0)?.as_f64()?, p.get(1)?.as_f64()?)))
                .collect::<Option<Vec<_>>>()?;
            let values = value["to_local"].as_array()?;
            if values.len() != 6 {
                return None;
            }
            let mut m = [0.0; 6];
            for (a, v) in m.iter_mut().zip(values) {
                *a = v.as_f64()?;
            }
            affine_inv(m)?;
            Some((points, m))
        };
        let (points, m) = read().ok_or_else(|| anyhow!("invalid saved density source"))?;
        let model = StitchRun::recover(&points)
            .ok_or_else(|| anyhow!("saved stitch source could not be reconstructed"))?;
        Ok(Some((model, m)))
    }

    fn save_stitch_source(
        &mut self,
        index: usize,
        model: &StitchRun,
        to_local: Affine,
        original: &str,
    ) -> Result<()> {
        let xml = roxmltree::Document::parse(&self.xml)?;
        let nodes = elements_by_id(&xml);
        let node = nodes[self.shapes[index].id.as_str()];
        let source = serde_json::json!({
            "version": 1, "points": model.original().iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
            "to_local": to_local, "generated": node.attribute("d").unwrap_or_default(),
        })
        .to_string();
        let mut edits = Vec::new();
        let mut insert = String::new();
        for (key, value) in [
            ("data-zigzag-density-source", source.as_str()),
            ("data-zigzag-original-stitches", original),
        ] {
            if let Some(a) = node.attributes().find(|a| a.name() == key) {
                if key == "data-zigzag-density-source" {
                    edits.push((a.range_value(), xml_escape(value)));
                }
            } else {
                insert.push_str(&format!(" {key}=\"{}\"", xml_escape(value)));
            }
        }
        if !insert.is_empty() {
            let at = node.range().start
                + 1
                + node.tag_name().name().len()
                + super::root_prefix_len(&self.xml, node.range().start);
            edits.push((at..at, insert));
        }
        drop(nodes);
        drop(xml);
        edits.sort_by_key(|e| std::cmp::Reverse(e.0.start));
        for (range, value) in edits {
            self.xml.replace_range(range, &value);
        }
        // Keep the parsed SVG cache and revision in step with the metadata too.
        self.import(&self.uid_map(&HashMap::new()))
    }

    /// Reconstruct before scaling, so spacing is measured in the original size.
    /// Unsupported runs are scaled normally and reported individually. Successful
    /// runs become native objects or retain a source model for compound satin,
    /// so later resizes never infer from previously regenerated stitches.
    /// Work on a copy: any XML or transform failure leaves the document intact.
    pub fn transform_keep_density(
        &mut self,
        indices: &[usize],
        m: Affine,
    ) -> Result<DensityReport> {
        let mut next = self.clone();
        next.sync()?;
        let uids: Vec<_> = indices
            .iter()
            .filter_map(|&i| next.shapes.get(i).map(|s| s.uid))
            .collect();
        let mut report = DensityReport::default();
        let mut stitch_runs = Vec::new();
        for &uid in &uids {
            let i = next
                .shapes
                .iter()
                .position(|s| s.uid == uid)
                .ok_or_else(|| anyhow!("object disappeared"))?;
            let s = &next.shapes[i];
            if s.params.kind != Some(Kind::Manual) {
                continue;
            }
            if let Some((model, to_local)) = next.stitch_source(i)? {
                stitch_runs.push((uid, model, to_local, next.stitch_source_xml(i)?));
                continue;
            }
            let recovered = match &s.geometry {
                Geometry::Stroke { lines, .. } if lines.len() == 1 && !lines[0].1 => {
                    crate::reconstruct::recover(&lines[0].0)
                }
                _ => Err("closed or multiple stitch paths need manual review"),
            };
            let recovered = match recovered {
                Ok(r) if r.kind == Kind::Fill => r,
                other => {
                    if let Geometry::Stroke { lines, .. } = &s.geometry
                        && lines.len() == 1
                        && !lines[0].1
                        && let Some(model) = StitchRun::recover(&lines[0].0)
                    {
                        let to_local = affine_inv(next.path_to_design(i)?)
                            .ok_or_else(|| anyhow!("degenerate stitch transform"))?;
                        stitch_runs.push((uid, model, to_local, next.stitch_source_xml(i)?));
                        continue;
                    }
                    report.warnings.push(DensityWarning {
                        uid,
                        label: s.label.clone(),
                        reason: match other {
                            Err(reason) => reason.into(),
                            _ => "Satin rails could not be reconstructed confidently.".into(),
                        },
                    });
                    continue;
                }
            };
            let color = s.color;
            let mut params = s.params.clone();
            params.kind = Some(recovered.kind);
            params.spacing = Some(recovered.spacing_mm);
            params.len = Some(recovered.length_mm);
            params.angle = Some(recovered.angle);
            // Don't invent additional layers or compensate an already compensated
            // outline. A whole run with underlay is rejected by the recognizer.
            params.underlay = Some(false);
            params.pull = Some(0.0);
            params.short_stitches = Some(false);
            params.width = None;
            next.sync()?;
            let i = next.shapes.iter().position(|s| s.uid == uid).unwrap();
            let original = next.stitch_source_xml(i)?;
            next.set_path(i, &recovered.path)?;
            let i = next.shapes.iter().position(|s| s.uid == uid).unwrap();
            let id = next.shapes[i].id.clone();
            let xml = roxmltree::Document::parse(&next.xml)?;
            let nodes = elements_by_id(&xml);
            let node = nodes[id.as_str()];
            let mut replacement = String::from("<path");
            for a in node.attributes() {
                if [
                    "fill",
                    "stroke",
                    "style",
                    "data-zigzag-original-stitches",
                    "data-zigzag-density-source",
                ]
                .contains(&a.name())
                {
                    continue;
                }
                replacement.push(' ');
                replacement.push_str(&next.xml[a.range()]);
            }
            let rgb = format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
            replacement.push_str(&format!(
                " fill=\"{rgb}\" stroke=\"none\" data-zigzag-original-stitches=\"{}\"/>",
                xml_escape(&original)
            ));
            let range = node.range();
            drop(nodes);
            drop(xml);
            let mut map = next.uid_map(&HashMap::new());
            map.insert((id, Part::Fill), uid);
            next.xml.replace_range(range, &replacement);
            next.import(&map)?;
            let shape = next
                .shapes
                .iter_mut()
                .find(|s| s.uid == uid)
                .ok_or_else(|| anyhow!("recovered shape has no geometry"))?;
            shape.params = params;
            report.fills += 1;
        }
        let indices: Vec<_> = uids
            .iter()
            .filter_map(|u| next.shapes.iter().position(|s| s.uid == *u))
            .collect();
        next.transform(&indices, m)?;
        for (uid, model, to_local, original) in stitch_runs {
            let i = next.shapes.iter().position(|s| s.uid == uid).unwrap();
            let label = next.shapes[i].label.clone();
            let transformed = affine_mul(next.path_to_design(i)?, to_local);
            let points = model.generate(transformed);
            let path = vec![crate::bezier::SubPath {
                anchors: points
                    .into_iter()
                    .map(crate::bezier::Anchor::corner)
                    .collect(),
                closed: false,
            }];
            next.set_path(i, &path)?;
            let i = next.shapes.iter().position(|s| s.uid == uid).unwrap();
            next.save_stitch_source(i, &model, to_local, &original)?;
            report.satins += model.satin_count();
            report.fills += model.fill_count();
            report.repeated_runs += model.repeated_count();
            for (start, end) in model.uncertain_spans() {
                report.warnings.push(DensityWarning {
                    uid, label: format!("{label}: stitches {}–{}", start + 1, end + 1),
                    reason: "This span could not be confidently reconstructed; its stitches were scaled without preserving density.".into(),
                });
            }
        }
        *self = next;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::{Block, Design, Run};
    use crate::formats::{
        pes,
        read::{read_stitch_file, to_svg},
    };
    use crate::geom::{P, Polyline, scale_about};
    use crate::reconstruct::tests::{fill_sample, satin_sample};

    fn imported(runs: Vec<Polyline>) -> Document {
        let d = Design {
            blocks: vec![Block {
                color: [200, 20, 30],
                runs: runs
                    .into_iter()
                    .map(|pts| Run {
                        pts,
                        trim_before: true,
                    })
                    .collect(),
            }],
            ..Default::default()
        };
        let pes = pes::write_pes(&d);
        let f = read_stitch_file(&pes, "pes").unwrap();
        Document::from_svg(&to_svg(&f), "density test", None).unwrap()
    }

    #[test]
    fn pes_fill_rebuilds_density_and_survives_save_and_repeated_resize() {
        let mut d = imported(vec![fill_sample()]);
        let before = d.plan().stitch_count();
        let uid = d.shapes[0].uid;
        let report = d
            .transform_keep_density(&[0], scale_about(P::default(), 2.0, 2.0))
            .unwrap();
        assert_eq!(report.fills, 1);
        assert!(report.warnings.is_empty());
        assert_eq!(d.shapes[0].uid, uid);
        assert_eq!(d.shapes[0].params.kind, Some(Kind::Fill));
        assert!((d.shapes[0].params.spacing.unwrap() - 0.4).abs() < 1e-6);
        let count = d.plan().stitch_count();
        assert!(
            count > before * 3 && count < before * 5,
            "{before} -> {count}"
        );
        let saved = d.to_svg().unwrap();
        assert!(saved.contains("data-zigzag-original-stitches="));
        let mut back = Document::from_svg(&saved, "back", None).unwrap();
        assert_eq!(back.plan().stitch_count(), count);
        let r = back
            .transform_keep_density(&[0], scale_about(P::default(), 0.5, 0.5))
            .unwrap();
        assert_eq!(r.fills, 0); // Native geometry is reused, not inferred again.
        assert_eq!(back.shapes[0].params.spacing, d.shapes[0].params.spacing);
        back.transform_keep_density(&[0], scale_about(P::default(), 2.0, 2.0))
            .unwrap();
        assert_eq!(back.plan().stitch_count(), count);
    }

    #[test]
    fn pes_satin_preserves_peak_spacing_and_reports_unsupported_runs() {
        let unknown = (0..20)
            .map(|i| P::new(100.0 + i as f64 * 10.0, 300.0))
            .collect();
        let mut d = imported(vec![satin_sample(), unknown]);
        let original = d.clone();
        let indices: Vec<_> = (0..d.shapes.len()).collect();
        let report = d
            .transform_keep_density(&indices, scale_about(P::default(), 2.0, 2.0))
            .unwrap();
        assert_eq!(report.satins, 1);
        assert_eq!(report.warnings.len(), 1);
        let unsupported = d
            .shapes
            .iter()
            .position(|s| s.uid == report.warnings[0].uid)
            .unwrap();
        assert_eq!(d.shapes[unsupported].params.kind, Some(Kind::Manual));
        assert_eq!(
            d.plan().per_shape[unsupported],
            original.plan().per_shape[unsupported]
        );
        let (model, _) = d.stitch_source(0).unwrap().unwrap();
        let source = crate::reconstruct::recover(model.original()).unwrap();
        assert!((source.spacing_mm - 0.4).abs() < 1e-6);
        assert!(d.plan().stitch_count() > original.plan().stitch_count());
        let reloaded = Document::from_svg(&d.to_svg().unwrap(), "back", None).unwrap();
        assert_eq!(reloaded.plan().stitch_count(), d.plan().stitch_count());
        // Wide columns stay one reconstructed column and split long stitches,
        // rather than falling back to two unrelated stroked lines.
        let r = d
            .transform_keep_density(&[0], scale_about(P::default(), 3.0, 3.0))
            .unwrap();
        assert_eq!(r.satins, 1);
        assert!(r.warnings.is_empty());
        if let Geometry::Stroke { lines, .. } = &d.shapes[0].geometry {
            assert!(lines[0].0.windows(2).all(|w| w[0].dist(w[1]) < 70.1));
        } else {
            panic!("rebuilt satin should remain a stitch path");
        }
    }

    #[test]
    fn failed_transform_is_atomic() {
        let mut d = imported(vec![fill_sample()]);
        let before = d.to_svg().unwrap();
        assert!(
            d.transform_keep_density(&[0], scale_about(P::default(), 0.0, 1.0))
                .is_err()
        );
        assert_eq!(d.to_svg().unwrap(), before);
    }

    #[test]
    fn downloaded_alower_rebuilds_both_satin_sections_and_reuses_saved_source() {
        let file = read_stitch_file(include_bytes!("../../examples/alower.pes"), "pes").unwrap();
        let mut d = Document::from_svg(&to_svg(&file), "alower", None).unwrap();
        let before = d.plan();
        let indices: Vec<_> = (0..d.shapes.len()).collect();
        let m = scale_about(P::default(), 2.0, 2.0);
        let report = d.transform_keep_density(&indices, m).unwrap();
        assert_eq!(report.satins, 2);
        assert!(report.warnings.is_empty(), "{report:?}");
        let plan = d.plan();
        assert!(plan.stitch_count() > before.stitch_count() * 3 / 2);
        assert!(plan.stitch_count() < before.stitch_count() * 3);
        assert!((plan.bounds().width() - before.bounds().width() * 2.0).abs() < 1.0);
        assert!((plan.bounds().height() - before.bounds().height() * 2.0).abs() < 1.0);
        assert_eq!(plan.blocks.len(), before.blocks.len());
        assert_eq!(plan.blocks[0].color, before.blocks[0].color);
        assert_eq!(
            plan.blocks[0].runs.len(),
            1,
            "no extra jumps or trims between sections"
        );
        let saved = d.to_svg().unwrap();
        assert!(saved.contains("data-zigzag-density-source="));
        let mut reopened = Document::from_svg(&saved, "reopened", None).unwrap();
        let source = reopened
            .stitch_source(0)
            .unwrap()
            .unwrap()
            .0
            .original()
            .clone();
        let resized = reopened
            .transform_keep_density(&[0], scale_about(P::default(), 0.5, 0.5))
            .unwrap();
        assert_eq!(resized.satins, 2);
        reopened.transform_keep_density(&[0], m).unwrap();
        assert_eq!(
            reopened.stitch_source(0).unwrap().unwrap().0.original(),
            &source
        );
        assert!(reopened.plan().stitch_count().abs_diff(plan.stitch_count()) <= 2);
        assert!((reopened.plan().bounds().width() - plan.bounds().width()).abs() < 0.1);
    }

    #[test]
    fn editing_rebuilt_satin_invalidates_the_saved_source() {
        let file = read_stitch_file(include_bytes!("../../examples/alower.pes"), "pes").unwrap();
        let mut d = Document::from_svg(&to_svg(&file), "alower", None).unwrap();
        d.transform_keep_density(&[0], scale_about(P::default(), 2.0, 2.0))
            .unwrap();
        assert!(d.stitch_source(0).unwrap().is_some());
        d.transform(&[0], crate::geom::translation(P::new(20.0, 30.0)))
            .unwrap();
        assert!(d.stitch_source(0).unwrap().is_some());
        let mut p = d.edit_path(0).unwrap();
        p[0].anchors[10] = crate::bezier::Anchor::corner(P::new(400.0, 400.0));
        d.set_path(0, &p).unwrap();
        assert!(d.stitch_source(0).unwrap().is_none());
    }
}
