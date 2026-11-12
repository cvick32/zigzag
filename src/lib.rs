//! zigzag: SVG → embroidery (the engine under ZigZag).
//!
//! Pipeline: [`svg`] import → [`design::plan`] (stitch generation + ordering)
//! → [`formats`] writers (PES, DST, PEN, preview) → optional upload to a
//! Brother Skitch over Bluetooth ([`machine`], `ble` feature).

pub mod bezier;
pub mod boolean;
pub mod checks;
pub mod design;
pub mod document;
pub mod formats;
pub mod geom;
pub mod machine;
pub mod params;
pub mod pathtools;
pub mod reconstruct;
pub mod stitch;
pub mod svg;
pub mod trace;

use anyhow::Result;
use std::path::Path;

/// Load an SVG and produce a stitch plan centered on the hoop, using the
/// settings saved in the file.
pub fn design_from_svg(path: &Path) -> Result<design::Design> {
    Ok(document::Document::open(path)?.plan())
}
