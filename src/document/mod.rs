//! An editable embroidery document backed by its SVG text.
//!
//! The SVG stays the source of truth: every drawable element gets a stable id,
//! and stitch settings, thread colors and sewing order are saved back *into
//! those ids* (which Illustrator shows as layer/object names), so a file can
//! go Illustrator → ZigZag → Illustrator → ZigZag without losing
//! anything. Document-wide settings live in a `data-zigzag` attribute on
//! the root `<svg>`. Everything else in the file is preserved byte for byte.

use crate::formats::Lock;
use crate::geom::P;
use crate::params::{Fabric, Kind, Params};
use crate::svg::{self, Geometry, PX_TO_UNITS, Part, Shape};
use anyhow::{Context, Result, anyhow, bail};

mod build;
mod cleanup;
mod edit;
mod glyphs;
mod lettering;
mod paths;
mod simplify;
pub use build::Face;
mod reconstruct;
pub use cleanup::{CleanUp, CleanUpReport};
pub use edit::Fragment;
pub use glyphs::{Carets, GlyphBox};
pub use lettering::{Align, Case, TextSpec, default_font, font_families};
pub use reconstruct::{DensityReport, DensityWarning};
pub use simplify::{SimplifyDesign, SimplifyReport, Unite};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Root `<svg>` attribute that holds the document-wide settings.
pub const SETTINGS_ATTR: &str = "data-zigzag";
/// The same attribute as written before the app was renamed; still read, and
/// renamed to [`SETTINGS_ATTR`] on the next save.
pub const OLD_SETTINGS_ATTR: &str = "data-threadline";

/// Elements that draw something and therefore carry stitch settings.
const DRAWABLE: [&str; 8] = [
    "path", "rect", "circle", "ellipse", "line", "polyline", "polygon", "text",
];
/// Containers whose contents aren't drawn directly.
const NON_RENDERED: [&str; 7] = [
    "defs", "symbol", "clipPath", "mask", "pattern", "marker", "metadata",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub fabric: String,
    pub remove_overlaps: bool,
    pub hat_order: bool,
    /// The hoop's sewing field, width and height in mm: one of the editor's
    /// standard hoops or any other size.
    pub hoop: (f64, f64),
    /// Sew across gaps between objects up to this long (mm) with running
    /// stitches instead of cutting the thread (0: always cut).
    pub connect_mm: f64,
    /// Where the thread can't get from one part of an object to the next
    /// out of sight, sew it over the fill already there instead of
    /// cutting it (fewer cuts; the running stitch shows on top).
    pub over_sewn: bool,
    /// The fabric's color, for previewing the stitches on it.
    pub cloth: [u8; 3],
    /// The machine the design is set up for: the id of a profile
    /// ([`crate::machine::profile`]), kept as a label. What the machine
    /// decides (the hoop) is in the settings themselves, so a design reads
    /// the same where the profile isn't known.
    pub machine: Option<String>,
    /// No stitch shorter than this, mm: some machines drop the ones that
    /// are. Short stitches are merged into their neighbours, in the plan
    /// and in the files written.
    pub min_stitch_mm: f64,
    /// The lock stitches where sewing starts and before the thread is cut.
    pub lock: Lock,
    /// Satins narrower than this on average (mm) get a warning. How narrow
    /// a column still sews cleanly depends on the fabric and the thread.
    pub narrow_satin_mm: f64,
}

/// The narrowest satin that isn't warned about when a design doesn't say, mm.
pub const DEFAULT_NARROW_SATIN_MM: f64 = 1.0;

/// What the narrowest satin can be set to (mm; 0 never warns).
pub const NARROW_SATIN_MM: std::ops::RangeInclusive<f64> = 0.0..=3.0;

/// The shortest stitch when a design doesn't say, mm: what ZigZag has
/// always kept.
pub const DEFAULT_MIN_STITCH_MM: f64 = 0.1;

/// The shortest stitches a design can ask for (mm).
pub const MIN_STITCH_MM: std::ops::RangeInclusive<f64> = 0.1..=1.0;

/// The hoop when a design doesn't say: 4×4 in.
pub const DEFAULT_HOOP: (f64, f64) = (100.0, 100.0);

/// The sides a hoop can have (mm).
pub const HOOP_SIDE_MM: std::ops::RangeInclusive<f64> = 10.0..=1000.0;

/// Files up to 0.1.2 named the hoop by its place in the editor's list of
/// three.
const OLD_HOOPS: [(f64, f64); 3] = [DEFAULT_HOOP, (70.0, 70.0), (130.0, 180.0)];

/// A hoop size as written in files and by agents: `130x180`, width by
/// height in mm. `None` if it isn't one or a side is out of range.
pub fn parse_hoop(v: &str) -> Option<(f64, f64)> {
    let (w, h) = v.trim().split_once(['x', 'X', '×'])?;
    let (w, h): (f64, f64) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (HOOP_SIDE_MM.contains(&w) && HOOP_SIDE_MM.contains(&h)).then_some((w, h))
}

/// [`parse_hoop`]'s inverse.
pub fn format_hoop((w, h): (f64, f64)) -> String {
    format!(
        "{}x{}",
        crate::params::fmt_num(w),
        crate::params::fmt_num(h)
    )
}

/// The preview fabric color when a design doesn't say.
pub const DEFAULT_CLOTH: [u8; 3] = [236, 231, 220];

impl Default for Settings {
    fn default() -> Self {
        Settings {
            fabric: "standard".into(),
            remove_overlaps: true,
            hat_order: false,
            hoop: DEFAULT_HOOP,
            connect_mm: 0.0,
            over_sewn: false,
            cloth: DEFAULT_CLOTH,
            machine: None,
            min_stitch_mm: DEFAULT_MIN_STITCH_MM,
            lock: Lock::default(),
            narrow_satin_mm: DEFAULT_NARROW_SATIN_MM,
        }
    }
}

impl Settings {
    fn parse(v: &str) -> Settings {
        let mut s = Settings::default();
        for tok in v.split_whitespace() {
            let Some((k, v)) = tok.split_once('=') else {
                continue;
            };
            match k {
                "fabric" if Fabric::by_id(v).is_some() => s.fabric = v.into(),
                "overlaps" => s.remove_overlaps = v == "1",
                "hat" => s.hat_order = v == "1",
                "hoop" => {
                    let old = || OLD_HOOPS.get(v.parse::<usize>().ok()?).copied();
                    s.hoop = parse_hoop(v).or_else(old).unwrap_or(DEFAULT_HOOP);
                }
                "connect" => s.connect_mm = v.parse().unwrap_or(0.0),
                "oversewn" => s.over_sewn = v == "1",
                "cloth" if v.len() == 6 => {
                    if let Ok(n) = u32::from_str_radix(v, 16) {
                        s.cloth = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
                    }
                }
                "machine" if crate::machine::profile::is_id(v) => s.machine = Some(v.into()),
                "minst" => {
                    if let Some(m) = v.parse::<f64>().ok().filter(|m| m.is_finite()) {
                        s.min_stitch_mm = m.clamp(*MIN_STITCH_MM.start(), *MIN_STITCH_MM.end());
                    }
                }
                "lock" => s.lock = Lock::parse(v).unwrap_or_default(),
                "narrowsatin" => {
                    if let Some(m) = v.parse::<f64>().ok().filter(|m| m.is_finite()) {
                        s.narrow_satin_mm =
                            m.clamp(*NARROW_SATIN_MM.start(), *NARROW_SATIN_MM.end());
                    }
                }
                _ => {}
            }
        }
        s
    }
    fn format(&self) -> String {
        let mut s = format!(
            "fabric={} overlaps={} hat={} hoop={}",
            self.fabric,
            self.remove_overlaps as u8,
            self.hat_order as u8,
            format_hoop(self.hoop)
        );
        if self.connect_mm > 0.0 {
            s.push_str(&format!(
                " connect={}",
                crate::params::fmt_num(self.connect_mm)
            ));
        }
        if self.over_sewn {
            s.push_str(" oversewn=1");
        }
        if self.cloth != DEFAULT_CLOTH {
            let [r, g, b] = self.cloth;
            s.push_str(&format!(" cloth={r:02x}{g:02x}{b:02x}"));
        }
        if let Some(m) = self
            .machine
            .as_deref()
            .filter(|m| crate::machine::profile::is_id(m))
        {
            s.push_str(&format!(" machine={m}"));
        }
        if self.min_stitch_mm != DEFAULT_MIN_STITCH_MM {
            s.push_str(&format!(
                " minst={}",
                crate::params::fmt_num(self.min_stitch_mm)
            ));
        }
        if self.lock != Lock::default() {
            s.push_str(&format!(" lock={}", self.lock.format()));
        }
        if self.narrow_satin_mm != DEFAULT_NARROW_SATIN_MM {
            s.push_str(&format!(
                " narrowsatin={}",
                crate::params::fmt_num(self.narrow_satin_mm)
            ));
        }
        s
    }
    pub fn fabric(&self) -> Fabric {
        *Fabric::by_id(&self.fabric).unwrap_or(Fabric::standard())
    }
    /// What the machine file is written with.
    pub fn export_options(&self) -> crate::formats::ExportOptions {
        crate::formats::ExportOptions {
            lock: self.lock,
            // (The usual shortest stitch is what a file's whole units give
            // anyway: nothing more to merge.)
            min_stitch: if self.min_stitch_mm > DEFAULT_MIN_STITCH_MM {
                self.min_stitch_mm * 10.0
            } else {
                0.0
            },
        }
    }
}

/// Where to put an SVG added to the document.
#[derive(Clone, Copy, Debug, Default)]
pub struct Placement {
    /// Center of the added artwork, mm from the hoop center (+y down). Default: hoop center.
    pub center_mm: Option<(f64, f64)>,
    /// Scale the artwork to this width / height (mm). If both, it fits inside both.
    pub width_mm: Option<f64>,
    pub height_mm: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Document {
    /// SVG source with a stable id on every drawable element.
    pub xml: String,
    pub path: Option<PathBuf>,
    pub name: String,
    /// Subtracted from imported coordinates so the hoop center is (0, 0).
    pub offset: P,
    pub shapes: Vec<Shape>,
    pub settings: Settings,
    /// Parts removed from elements that still have another part.
    removed_parts: HashSet<(String, Part)>,
    next_uid: usize,
    next_group: usize,
    /// Ids of the lettering elements (`<text data-tl-lettering>`), found
    /// when the SVG is read, so asking whether an object is lettering
    /// doesn't parse the whole SVG.
    pub(crate) lettering_ids: HashSet<String>,
    /// The SVG as usvg read it at the last import (outlines and transforms
    /// are read from it rather than parsing the SVG again). Shared by the
    /// document's copies (undo steps).
    tree: Option<std::sync::Arc<usvg::Tree>>,
    /// The root's px → user transform, worked out once per revision.
    root_ts: RootTransform,
    /// Lettering settings read from the SVG (see [`Document::lettering`]),
    /// by element id, each for the revision (and the shape's color and
    /// stitch type) it was read at: the panel asks every frame.
    lettering_memo: lettering::SpecMemo,
    /// A name being given to an element (its id as it is now, and the
    /// words of the name), for the next [`Document::sync`] to write.
    renaming: Option<(String, Vec<String>)>,
    /// Changes whenever `xml` (and so the shapes read from it) does: a value
    /// never used before, so caches keyed by it can't mistake one version
    /// for another (an undone document keeps the revision it had).
    rev: u64,
}

/// A root transform measured at a revision, shared by a document's copies.
type RootTransform = std::sync::Arc<std::sync::Mutex<Option<(u64, [f64; 6])>>>;

/// Where document revisions come from (unique across documents).
static NEXT_REV: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_rev() -> u64 {
    NEXT_REV.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

const EMPTY_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="100mm" viewBox="-50 -50 100 100">
</svg>
"#;

impl Document {
    /// A blank 100 × 100 mm document whose user units are millimetres, origin at the hoop center.
    pub fn empty() -> Document {
        let mut d =
            Document::from_svg(EMPTY_SVG, "untitled", None).expect("built-in template parses");
        d.offset = P::default();
        d
    }

    /// Open an SVG, a machine file (PES/PEC/DST/EXP/JEF) as manual stitches,
    /// or a raster logo (PNG/JPEG/…) traced into shapes. Machine files and
    /// images get no save path, so they're never overwritten with SVG.
    pub fn open(path: &Path) -> Result<Document> {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if crate::formats::READABLE_FORMATS.contains(&ext.as_str()) {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let file = crate::formats::read::read_stitch_file(&bytes, &ext)?;
            return Document::from_svg(&crate::formats::read::to_svg(&file), &name, None);
        }
        if crate::trace::is_image(&ext) {
            // A raster logo is traced into shapes (default options); it gets
            // no save path, so the image is never overwritten with SVG.
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let t = crate::trace::trace_bytes(&bytes, &crate::trace::TraceOptions::default())
                .with_context(|| format!("tracing {}", path.display()))?;
            return Document::from_svg(&t.svg, &name, None);
        }
        // Anything else is taken for an SVG; what isn't even text is a
        // kind of file ZigZag doesn't open (a VP3, say: written, not read).
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let Ok(xml) = String::from_utf8(bytes) else {
            bail!(
                "{} isn't a file ZigZag can open: it opens SVG, images to trace, and {} machine files",
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                crate::formats::READABLE_FORMATS.join(", ")
            );
        };
        Document::from_svg(&xml, &name, Some(path.to_path_buf()))
    }

    /// Load SVG text, centering its artwork on the hoop.
    pub fn from_svg(xml: &str, name: &str, path: Option<PathBuf>) -> Result<Document> {
        let (xml, _) = prepare_ids(xml, &mut 0)?;
        let settings = root_settings(&xml);
        let mut d = Document {
            xml,
            path,
            name: name.to_string(),
            offset: P::default(),
            shapes: Vec::new(),
            settings,
            removed_parts: HashSet::new(),
            next_uid: 0,
            next_group: 0,
            lettering_ids: HashSet::new(),
            tree: None,
            root_ts: Default::default(),
            lettering_memo: Default::default(),
            renaming: None,
            rev: next_rev(),
        };
        d.import(&HashMap::new())?;
        let b = svg::shapes_bounds(&d.shapes);
        if !b.is_empty() {
            // On a whole 0.1 mm unit, so points on the machine's grid stay
            // on it (an opened stitch file exports unchanged).
            let c = b.center();
            d.offset = P::new(c.x.round(), c.y.round());
            translate(&mut d.shapes, -d.offset);
        }
        d.adopt_foreign_lettering()?;
        Ok(d)
    }

    /// The document's revision: it changes whenever its SVG does.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// (Re)build `shapes` from `xml`, keeping uids for elements we already had.
    fn import(&mut self, old_uids: &HashMap<(String, Part), usize>) -> Result<()> {
        self.rev = next_rev();
        // (The lettering ids come from the same parse as the shapes.)
        let lettering_ids = |doc: &roxmltree::Document| -> HashSet<String> {
            doc.descendants()
                .filter(|n| {
                    n.is_element()
                        && (n.attribute("data-tl-lettering").is_some()
                            || (n.tag_name().name() == "text"
                                && n.attribute("id").is_some_and(lettering::has_type_tokens)))
                })
                .filter_map(|n| n.attribute("id").map(str::to_string))
                .collect()
        };
        let (mut shapes, tree, ids) = svg::load_with_tree_and(self.xml.as_bytes(), lettering_ids)?;
        self.lettering_ids = ids.unwrap_or_default();
        self.tree = Some(std::sync::Arc::new(tree));
        shapes.retain(|s| !self.removed_parts.contains(&(s.id.clone(), s.part)));
        for s in &mut shapes {
            s.uid = match old_uids.get(&(s.id.clone(), s.part)) {
                Some(&u) => u,
                None => {
                    self.next_uid += 1;
                    self.next_uid - 1
                }
            };
        }
        translate(&mut shapes, -self.offset);
        self.shapes = shapes;
        Ok(())
    }

    fn uid_map(&self, renames: &HashMap<String, String>) -> HashMap<(String, Part), usize> {
        self.shapes
            .iter()
            .map(|s| {
                (
                    (
                        renames.get(&s.id).cloned().unwrap_or_else(|| s.id.clone()),
                        s.part,
                    ),
                    s.uid,
                )
            })
            .collect()
    }

    /// Write current settings into the XML and re-import from it.
    pub fn sync(&mut self) -> Result<()> {
        let (xml, renames) = self.render_xml()?;
        // Nothing to write (the settings are in the SVG already): no need to
        // read it all back in.
        if xml == self.xml && renames.is_empty() {
            return Ok(());
        }
        let uids = self.uid_map(&renames);
        self.xml = xml;
        self.removed_parts = self
            .removed_parts
            .iter()
            .map(|(id, p)| (renames.get(id).cloned().unwrap_or(id.clone()), *p))
            .collect();
        self.import(&uids)
    }

    /// The words of a name someone typed for an object, as they go into
    /// its id: letters, digits and the like, with the characters ids keep
    /// for other things (`_ , ;` between words, `=` in a setting, `#`
    /// before a number) read as spaces. An error if nothing's left, or if
    /// a word is a stitch setting (`satin`, `trim`, `a=45`): in an id that
    /// would change how the object sews, not what it's called.
    pub fn name_words_typed(name: &str) -> Result<Vec<String>> {
        let words: Vec<String> = name
            .split(|c: char| c.is_whitespace() || matches!(c, '_' | ',' | ';' | '=' | '#' | '"'))
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect();
        if words.is_empty() {
            bail!("a name needs at least one letter or number");
        }
        if let Some(w) = words.iter().find(|w| {
            crate::params::is_param_token(w)
                || Params::parse(w) != Params::default()
                || w.to_ascii_lowercase().starts_with("tl-")
        }) {
            bail!("\"{w}\" is a stitch setting, so it can't be part of a name");
        }
        Ok(words)
    }

    /// Give the object at `index` a name (see [`Document::name_words_typed`]
    /// for what a name can be). The name is the element's: a fill and its
    /// outline share it, and its settings stay as they are.
    pub fn rename(&mut self, index: usize, name: &str) -> Result<()> {
        let words = Self::name_words_typed(name)?;
        let Some(shape) = self.shapes.get(index) else {
            bail!("no such object");
        };
        if shape.id.is_empty() {
            bail!("this object can't be named");
        }
        if svg::name_words(&shape.id) == words {
            return Ok(());
        }
        self.renaming = Some((shape.id.clone(), words));
        let done = self.sync();
        self.renaming = None;
        done
    }

    /// The SVG with every object's settings written into its id.
    pub fn to_svg(&self) -> Result<String> {
        Ok(self.render_xml()?.0)
    }

    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf> {
        let path = match path.or(self.path.as_deref()) {
            Some(p) => p.to_path_buf(),
            None => bail!("no file name yet; use Save As"),
        };
        self.sync()?;
        std::fs::write(&path, &self.xml).with_context(|| format!("writing {}", path.display()))?;
        if self.path.as_deref() != Some(path.as_path()) {
            self.name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.path = Some(path.clone());
        }
        Ok(path)
    }

    /// Remove a shape. The SVG element goes too once none of its parts remain.
    pub fn delete(&mut self, index: usize) -> Result<()> {
        self.delete_many(&[index])
    }

    /// Remove shapes (indices into `shapes` as they are now), reading the
    /// SVG once for all of them. Elements go once none of their parts remain.
    pub fn delete_many(&mut self, indices: &[usize]) -> Result<()> {
        let mut idx: Vec<usize> = indices.to_vec();
        idx.sort_unstable();
        idx.dedup();
        if let Some(&i) = idx.last()
            && i >= self.shapes.len()
        {
            bail!("no object {i}");
        }
        let doomed: HashSet<usize> = idx.iter().copied().collect();
        let mut gone = Vec::new();
        let mut kept = Vec::with_capacity(self.shapes.len() - doomed.len());
        for (i, s) in std::mem::take(&mut self.shapes).into_iter().enumerate() {
            if doomed.contains(&i) {
                gone.push(s);
            } else {
                kept.push(s);
            }
        }
        self.shapes = kept;
        let left: HashSet<&str> = self.shapes.iter().map(|s| s.id.as_str()).collect();
        let mut whole: HashSet<String> = HashSet::new();
        // An outline deleted from a shape that stays: the stroke comes off
        // the element (so it isn't there, switched off, the next time the
        // file is opened).
        let mut unstroke: Vec<String> = Vec::new();
        for s in gone {
            if left.contains(s.id.as_str()) {
                // The element's other part stays.
                if s.part == Part::Stroke {
                    unstroke.push(s.id.clone());
                }
                self.removed_parts.insert((s.id, s.part));
            } else {
                whole.insert(s.id);
            }
        }
        if !unstroke.is_empty() {
            let mut writes = Vec::new();
            {
                let doc = roxmltree::Document::parse(&self.xml).context("parsing document")?;
                let by_id = elements_by_id(&doc);
                for id in &unstroke {
                    if let Some(w) = by_id
                        .get(id.as_str())
                        .and_then(|n| stroke_style_write(*n, "stroke:none"))
                    {
                        writes.push((id.clone(), w));
                    }
                }
            }
            writes.sort_by_key(|(_, (r, _))| std::cmp::Reverse(r.start));
            for (id, (range, text)) in writes {
                self.xml.replace_range(range, &text);
                // (Nothing left to remember as removed.)
                self.removed_parts.remove(&(id, Part::Stroke));
            }
            self.rev = next_rev();
        }
        if whole.is_empty() {
            return Ok(());
        }
        let doc = roxmltree::Document::parse(&self.xml).context("parsing document")?;
        let mut ranges = Vec::with_capacity(whole.len());
        let mut found: HashSet<&str> = HashSet::new();
        for n in doc.descendants().filter(|n| n.is_element()) {
            if let Some(id) = n.attribute("id")
                && whole.contains(id)
                && found.insert(id)
            {
                ranges.push(n.range());
            }
        }
        if let Some(id) = whole.iter().find(|id| !found.contains(id.as_str())) {
            bail!("element {id} not found");
        }
        drop(doc);
        // Back to front; one inside another goes with it.
        ranges.sort_by_key(|r| r.start);
        let mut outer: Vec<std::ops::Range<usize>> = Vec::new();
        for r in ranges {
            if outer.last().is_none_or(|o| r.start >= o.end) {
                outer.push(r);
            }
        }
        for r in outer.into_iter().rev() {
            self.xml.replace_range(r, "");
        }
        self.rev = next_rev();
        Ok(())
    }

    /// Add SVG artwork (a complete `<svg>` document) to the canvas. Returns
    /// the indices of the new shapes. Stitch settings in the artwork's ids apply.
    pub fn add_svg(&mut self, fragment: &str, place: Placement) -> Result<Vec<usize>> {
        self.sync()?;
        let frag = svg::parse_tree(fragment.as_bytes()).context("parsing the added SVG")?;
        let bbox = frag.root().abs_stroke_bounding_box();
        if bbox.width() <= 0.0 && bbox.height() <= 0.0 {
            bail!("the added SVG has nothing visible in it");
        }
        // Normalize: usvg resolves CSS, <use>, units and transforms into plain elements.
        let normalized = frag.to_string(&usvg::WriteOptions {
            preserve_text: true,
            ..Default::default()
        });
        let ndoc = roxmltree::Document::parse(&normalized).context("normalizing the added SVG")?;
        let root = ndoc.root_element();
        let inner_start = root
            .first_child()
            .map(|c| c.range().start)
            .unwrap_or(root.range().end);
        let inner_end = root
            .last_child()
            .map(|c| c.range().end)
            .unwrap_or(inner_start);
        let inner = &normalized[inner_start..inner_end];

        // Fragment px → main px.
        let (fw, fh) = (bbox.width() as f64, bbox.height() as f64);
        let fc = (bbox.x() as f64 + fw / 2.0, bbox.y() as f64 + fh / 2.0);
        let mut s = 1.0f64;
        let to_px = |mm: f64| mm * 10.0 / PX_TO_UNITS;
        match (place.width_mm, place.height_mm) {
            (Some(w), Some(h)) => s = (to_px(w) / fw.max(1e-9)).min(to_px(h) / fh.max(1e-9)),
            (Some(w), None) => s = to_px(w) / fw.max(1e-9),
            (None, Some(h)) => s = to_px(h) / fh.max(1e-9),
            _ => {}
        }
        let (cx, cy) = place.center_mm.unwrap_or((0.0, 0.0));
        let target = (
            (cx * 10.0 + self.offset.x) / PX_TO_UNITS,
            (cy * 10.0 + self.offset.y) / PX_TO_UNITS,
        );
        let m_px = [s, 0.0, 0.0, s, target.0 - s * fc.0, target.1 - s * fc.1];
        // Main px → main user units: invert the root transform.
        let root_ts = self.root_transform()?;
        let w = mat_mul(
            mat_inv(root_ts).ok_or_else(|| anyhow!("document root transform isn't invertible"))?,
            m_px,
        );

        self.next_group += 1;
        let gid = format!("tl-added-{}", self.next_group);
        let group = format!(
            "<g id=\"{gid}\" transform=\"matrix({} {} {} {} {} {})\">{inner}</g>\n",
            w[0], w[1], w[2], w[3], w[4], w[5]
        );
        let close = self
            .xml
            .rfind("</svg>")
            .ok_or_else(|| anyhow!("document has no closing </svg>"))?;
        self.xml.insert_str(close, &group);
        let before: HashSet<(String, Part)> =
            self.shapes.iter().map(|s| (s.id.clone(), s.part)).collect();
        let (xml, _) = prepare_ids(&self.xml, &mut self.next_group)?;
        self.xml = xml;
        let uids = self.uid_map(&HashMap::new());
        self.import(&uids)?;
        Ok(self
            .shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| !before.contains(&(s.id.clone(), s.part)))
            .map(|(i, _)| i)
            .collect())
    }

    /// The transform from root user units to px (viewBox scaling).
    fn root_transform(&self) -> Result<[f64; 6]> {
        // It depends only on the root element's attributes (viewBox, size):
        // edits inside the document leave it as it was.
        let key = match root_tag(&self.xml) {
            Some(tag) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                tag.hash(&mut h);
                h.finish()
            }
            None => self.rev,
        };
        if let Ok(c) = self.root_ts.lock()
            && let Some((k, m)) = *c
            && k == key
        {
            return Ok(m);
        }
        let m = self.measure_root_transform()?;
        if let Ok(mut c) = self.root_ts.lock() {
            *c = Some((key, m));
        }
        Ok(m)
    }

    fn measure_root_transform(&self) -> Result<[f64; 6]> {
        let close = self
            .xml
            .rfind("</svg>")
            .ok_or_else(|| anyhow!("document has no closing </svg>"))?;
        let mut probe = self.xml.clone();
        probe.insert_str(
            close,
            r#"<path id="tl-probe" d="M0 0 L1 0 L0 1 Z" fill="black"/>"#,
        );
        let tree = svg::parse_tree(probe.as_bytes())?;
        let node = tree
            .node_by_id("tl-probe")
            .ok_or_else(|| anyhow!("probe vanished"))?;
        let t = node.abs_transform();
        Ok([
            t.sx as f64,
            t.ky as f64,
            t.kx as f64,
            t.sy as f64,
            t.tx as f64,
            t.ty as f64,
        ])
    }

    /// Serialize: ids rewritten from current settings. Returns old id → new id.
    fn render_xml(&self) -> Result<(String, HashMap<String, String>)> {
        let doc = roxmltree::Document::parse(&self.xml).context("parsing document")?;
        // Sewing order is only written when it differs from document order.
        let doc_order: Vec<(String, Part)> = {
            let pos: HashMap<&str, usize> = doc
                .descendants()
                .filter(|n| n.is_element())
                .filter_map(|n| n.attribute("id"))
                .enumerate()
                .map(|(i, id)| (id, i))
                .collect();
            let mut v: Vec<(String, Part)> =
                self.shapes.iter().map(|s| (s.id.clone(), s.part)).collect();
            v.sort_by_key(|(id, part)| {
                (
                    pos.get(id.as_str()).copied().unwrap_or(usize::MAX),
                    *part as u8,
                )
            });
            v
        };
        let current: Vec<(String, Part)> =
            self.shapes.iter().map(|s| (s.id.clone(), s.part)).collect();
        let write_order = doc_order != current;

        let mut used: HashSet<String> = doc
            .descendants()
            .filter(|n| n.is_element() && !DRAWABLE.contains(&n.tag_name().name()))
            .filter_map(|n| n.attribute("id").map(str::to_string))
            .collect();
        let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        let mut renames = HashMap::new();
        // Each element's shapes, looked up rather than searched for (a
        // stitch file opened as SVG has thousands).
        let mut index: HashMap<(&str, Part), usize> = HashMap::with_capacity(self.shapes.len());
        for (i, s) in self.shapes.iter().enumerate() {
            index.entry((s.id.as_str(), s.part)).or_insert(i);
        }
        let removed: HashSet<(&str, Part)> = self
            .removed_parts
            .iter()
            .map(|(id, p)| (id.as_str(), *p))
            .collect();
        for n in doc
            .descendants()
            .filter(|n| n.is_element() && DRAWABLE.contains(&n.tag_name().name()))
        {
            let Some(id) = n.attribute("id") else {
                continue;
            };
            let fill = index.get(&(id, Part::Fill)).copied();
            let stroke = index.get(&(id, Part::Stroke)).copied();
            let fill_removed = removed.contains(&(id, Part::Fill));
            let stroke_removed = removed.contains(&(id, Part::Stroke));
            if fill.is_none() && stroke.is_none() && !fill_removed && !stroke_removed {
                used.insert(id.to_string());
                continue;
            }
            let own = |i: usize| -> Params {
                let s = &self.shapes[i];
                Params {
                    thread: (s.color != s.paint).then_some(s.color),
                    order: write_order.then_some(i as u32),
                    ..s.params.clone()
                }
            };
            let skip = Params {
                kind: Some(Kind::Skip),
                ..Params::default()
            };
            let has_fill = fill.is_some() || fill_removed;
            let mut tokens = Vec::new();
            if let Some(i) = fill {
                tokens.extend(own(i).tokens());
            } else if fill_removed {
                tokens.extend(skip.tokens());
            }
            let stroke_params = stroke.map(own).or(stroke_removed.then(|| skip.clone()));
            if let Some(p) = stroke_params {
                let toks = p.tokens();
                if has_fill {
                    tokens.extend(toks.into_iter().map(|t| format!("stroke.{t}")));
                } else {
                    tokens.extend(toks);
                }
            }
            let words = match &self.renaming {
                Some((of, words)) if of == id => words.clone(),
                _ => svg::name_words(id),
            };
            let mut parts: Vec<String> = words;
            parts.extend(tokens);
            // Text records itself in its id too (it outlives Illustrator
            // there); outlines made from text keep the record, so they can
            // be turned back into text.
            match fill.or(stroke) {
                Some(i) if self.lettering_ids.contains(id) => {
                    if let Some(spec) = self.lettering_at(i, Some(n)) {
                        parts.extend(spec.id_tokens());
                    }
                }
                _ => parts.extend(lettering::type_tokens(id)),
            }
            let base = if parts.is_empty() {
                id.to_string()
            } else {
                parts.join("_")
            };
            let mut new_id = base.clone();
            let mut k = 2;
            while used.contains(&new_id) {
                new_id = format!("{base}_#{k}");
                k += 1;
            }
            used.insert(new_id.clone());
            if new_id != id {
                let attr = n.attributes().find(|a| a.name() == "id").expect("has id");
                edits.push((attr.range_value(), xml_escape(&new_id)));
                renames.insert(id.to_string(), new_id);
            }
        }
        // Whatever points at a renamed id by name follows it: `<use>`
        // copies and `<textPath>` (usvg drops one whose target is gone).
        if !renames.is_empty() {
            for n in doc.descendants().filter(|n| n.is_element()) {
                for a in n.attributes().filter(|a| a.name() == "href") {
                    if let Some(new_id) = a.value().strip_prefix('#').and_then(|o| renames.get(o)) {
                        edits.push((a.range_value(), format!("#{}", xml_escape(new_id))));
                    }
                }
            }
        }
        // Document settings on the root element.
        let root = doc.root_element();
        let settings = self.settings.format();
        match root.attributes().find(|a| a.name() == SETTINGS_ATTR) {
            Some(a) => edits.push((a.range_value(), settings)),
            None => match root.attributes().find(|a| a.name() == OLD_SETTINGS_ATTR) {
                // A file from when the app was called Threadline: rename the attribute.
                Some(a) => {
                    edits.push((a.range_qname(), SETTINGS_ATTR.into()));
                    edits.push((a.range_value(), settings));
                }
                None => {
                    let at = root.range().start
                        + 1
                        + root.tag_name().name().len()
                        + root_prefix_len(&self.xml, root.range().start);
                    edits.push((at..at, format!(" {SETTINGS_ATTR}=\"{settings}\"")));
                }
            },
        }
        let mut xml = self.xml.clone();
        edits.sort_by_key(|e| std::cmp::Reverse(e.0.start));
        for (range, text) in edits {
            xml.replace_range(range, &text);
        }
        Ok((xml, renames))
    }

    /// Plan options for this document's settings.
    pub fn plan_options(&self) -> crate::design::PlanOptions {
        crate::design::PlanOptions {
            fabric: self.settings.fabric(),
            remove_overlaps: self.settings.remove_overlaps,
            hat_order: self.settings.hat_order,
            connect: self.settings.connect_mm * 10.0,
            over_sewn: self.settings.over_sewn,
            // (A hair under, so a stitch that's exactly the shortest
            // allowed isn't dropped for a rounding error in its length.)
            min_stitch: self.settings.min_stitch_mm * 10.0 - 1e-6,
            narrow_satin: self.settings.narrow_satin_mm,
            ..Default::default()
        }
    }

    /// A map from the commands of the machine file `file` (written from
    /// `d`, this document's plan, in format `ext`) to the objects they're
    /// for, so a checker can name the shape behind a stitch: the objects,
    /// and the runs of commands for each. `None` for a format that has no
    /// such map (the Skitch's PEN).
    pub fn object_map(
        &self,
        d: &crate::design::Design,
        ext: &str,
        file: &str,
    ) -> Option<serde_json::Value> {
        use serde_json::json;
        let ranges = crate::formats::object_ranges(d, ext, &self.settings.export_options())?;
        let names = svg::display_names(&self.shapes);
        let mut commands = vec![0usize; self.shapes.len()];
        let mut stitches = vec![0usize; self.shapes.len()];
        for r in &ranges {
            if let Some(i) = r.shape.filter(|&i| i < self.shapes.len()) {
                commands[i] += r.last - r.first + 1;
                stitches[i] += r.stitches.map_or(0, |(a, b)| b - a + 1);
            }
        }
        let mm = |v: f64| (v * 10.0).round() / 100.0;
        Some(json!({
            "file": file,
            "format": ext.to_ascii_lowercase(),
            "numbering": "Commands are numbered from 0 in the order the file has them: every stitch, jump, trim, change of thread and the end. first_stitch and last_stitch number the stitches (needle points) alone. The locks and the jump that begin an object's stitches, and the lock and trim that end them, count as that object's; travel to an object counts as its.",
            "coordinates": "at_mm is where a range begins: millimetres, +x right, +y down, as the design is planned",
            "objects": self.shapes.iter().enumerate().filter(|(i, _)| commands[*i] > 0).map(|(i, s)| json!({
                "index": i,
                "name": names.get(i),
                "id": s.id,
                "kind": s.params.kind.unwrap_or(Kind::Fill).name(),
                "commands": commands[i],
                "stitches": stitches[i],
            })).collect::<Vec<_>>(),
            "ranges": ranges.iter().map(|r| json!({
                "first": r.first,
                "last": r.last,
                "first_stitch": r.stitches.map(|s| s.0),
                "last_stitch": r.stitches.map(|s| s.1),
                "object": r.shape,
                "at_mm": [mm(r.at.x), mm(r.at.y)],
            })).collect::<Vec<_>>(),
        }))
    }

    /// Where the object map for a machine file goes: beside it, as
    /// `<file>.objects.json`.
    pub fn object_map_path(file: &Path) -> PathBuf {
        let mut name = file.file_name().unwrap_or_default().to_os_string();
        name.push(".objects.json");
        file.with_file_name(name)
    }

    pub fn plan(&self) -> crate::design::Design {
        crate::design::plan(&self.name, &self.shapes, &self.plan_options())
    }

    /// [`Document::plan`], re-planning only what changed since `cache` was filled.
    pub fn plan_cached(&self, cache: &mut crate::design::PlanCache) -> crate::design::Design {
        crate::design::plan_cached(&self.name, &self.shapes, &self.plan_options(), cache)
    }
}

/// Elements by id (the first with each), to look up rather than search for.
/// The elements whose own stroke can be set: plain shapes (not text, and
/// not a `<use>`, whose look is its original's).
const STROKED: [&str; 6] = ["path", "rect", "circle", "ellipse", "polygon", "polyline"];

/// The edit that gives element `n` the stroke `decl` says (`stroke:…`
/// declarations, `;` between them), in place of any it has: where in the
/// SVG to write, and what. In `style`, which wins over a stylesheet's rule
/// and over stroke attributes on the element. None for an element whose
/// stroke isn't its own to set.
pub(super) fn stroke_style_write(
    n: roxmltree::Node,
    decl: &str,
) -> Option<(std::ops::Range<usize>, String)> {
    if !STROKED.contains(&n.tag_name().name()) {
        return None;
    }
    match n
        .attributes()
        .find(|a| a.namespace().is_none() && a.name() == "style")
    {
        Some(a) => {
            let mut decls: Vec<&str> = a
                .value()
                .split(';')
                .map(str::trim)
                .filter(|d| {
                    let name = d.split(':').next().unwrap_or("").trim();
                    !d.is_empty() && !name.to_ascii_lowercase().starts_with("stroke")
                })
                .collect();
            decls.push(decl);
            // (Escaped for whichever quotes the attribute is in.)
            let value = xml_escape(&decls.join(";")).replace('\'', "&apos;");
            Some((a.range_value(), value))
        }
        None => {
            let at = n.attributes().find(|a| a.name() == "id")?.range().start;
            Some((at..at, format!("style=\"{}\" ", xml_escape(decl))))
        }
    }
}

pub(super) fn elements_by_id<'a, 'input>(
    doc: &'a roxmltree::Document<'input>,
) -> HashMap<&'a str, roxmltree::Node<'a, 'input>> {
    let mut out = HashMap::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        if let Some(id) = n.attribute("id") {
            out.entry(id).or_insert(n);
        }
    }
    out
}

/// The root element's start tag (`<svg ...>`), past any XML declaration,
/// comments and doctype; none if it can't be found that simply.
fn root_tag(xml: &str) -> Option<&str> {
    let mut at = 0;
    loop {
        let start = at + xml[at..].find('<')?;
        let rest = &xml[start..];
        if rest.starts_with("<!--") {
            at = start + rest.find("-->")? + 3;
        } else if rest.starts_with("<?") {
            at = start + rest.find("?>")? + 2;
        } else if rest.starts_with("<!") {
            // A doctype; with an internal subset, it ends at `]>`.
            let end = match (rest.find('['), rest.find('>')) {
                (Some(b), Some(g)) if b < g => rest.find("]>")? + 2,
                (_, Some(g)) => g + 1,
                _ => return None,
            };
            at = start + end;
        } else {
            let mut quote = None;
            for (i, c) in rest.char_indices() {
                match (quote, c) {
                    (None, '"' | '\'') => quote = Some(c),
                    (Some(q), c) if c == q => quote = None,
                    (None, '>') => return Some(&rest[..=i]),
                    _ => {}
                }
            }
            return None;
        }
    }
}

/// Length of a namespace prefix on the root tag (`<svg:svg` → 4).
fn root_prefix_len(xml: &str, start: usize) -> usize {
    let tag = &xml[start + 1..];
    let end = tag
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(0);
    tag[..end].find(':').map(|i| i + 1).unwrap_or(0)
}

fn translate(shapes: &mut [Shape], d: P) {
    for s in shapes {
        s.origin = s.origin + d;
        match &mut s.geometry {
            Geometry::Fill { rings, .. } => rings.iter_mut().flatten().for_each(|p| *p = *p + d),
            Geometry::Stroke { lines, .. } => lines
                .iter_mut()
                .flat_map(|(l, _)| l)
                .for_each(|p| *p = *p + d),
        }
    }
}

fn root_settings(xml: &str) -> Settings {
    roxmltree::Document::parse(xml)
        .ok()
        .and_then(|d| {
            let root = d.root_element();
            root.attribute(SETTINGS_ATTR)
                .or_else(|| root.attribute(OLD_SETTINGS_ATTR))
                .map(Settings::parse)
        })
        .unwrap_or_default()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Give every drawable element a unique id: `tl-N` for a missing one, a
/// `_#N` suffix on a repeat.
fn prepare_ids(xml: &str, counter: &mut usize) -> Result<(String, usize)> {
    let doc = roxmltree::Document::parse(xml).context("parsing SVG")?;
    let mut seen: HashSet<String> = HashSet::new();
    let all_ids: HashSet<&str> = doc
        .descendants()
        .filter_map(|n| n.attribute("id"))
        .collect();
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut fresh = |seen: &HashSet<String>| loop {
        *counter += 1;
        let id = format!("tl-{}", *counter);
        if !all_ids.contains(id.as_str()) && !seen.contains(&id) {
            return id;
        }
    };
    // A repeated id: keep the name and settings, add a `_#N` suffix.
    let repeat = |value: &str, seen: &mut HashSet<String>| {
        let base = value.rsplit_once("_#").map_or(value, |(b, _)| b);
        let mut k = 2;
        let mut id = format!("{base}_#{k}");
        while all_ids.contains(id.as_str()) || seen.contains(&id) {
            k += 1;
            id = format!("{base}_#{k}");
        }
        seen.insert(id.clone());
        id
    };
    // Groups need a unique id so they can be told apart (and selected as a
    // group): the same artwork placed twice makes two groups, not one.
    for n in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "g")
    {
        if n.ancestors()
            .any(|a| a.is_element() && NON_RENDERED.contains(&a.tag_name().name()))
        {
            continue;
        }
        if let Some(a) = n.attributes().find(|a| a.name() == "id") {
            if seen.insert(a.value().to_string()) {
                continue;
            }
            let id = repeat(a.value(), &mut seen);
            edits.push((a.range_value(), xml_escape(&id)));
            continue;
        }
        let id = fresh(&seen);
        seen.insert(id.clone());
        let at =
            n.range().start + 1 + n.tag_name().name().len() + root_prefix_len(xml, n.range().start);
        edits.push((at..at, format!(" id=\"{id}\"")));
    }
    for n in doc
        .descendants()
        .filter(|n| n.is_element() && DRAWABLE.contains(&n.tag_name().name()))
    {
        if n.ancestors()
            .any(|a| a.is_element() && NON_RENDERED.contains(&a.tag_name().name()))
        {
            continue;
        }
        // Text content elements (tspan) are part of their <text>.
        match n.attributes().find(|a| a.name() == "id") {
            Some(a) if !seen.contains(a.value()) => {
                seen.insert(a.value().to_string());
            }
            Some(a) => {
                // The same artwork added twice: the copy keeps its name and
                // stitch settings (they're in the id).
                let id = repeat(a.value(), &mut seen);
                edits.push((a.range_value(), xml_escape(&id)));
            }
            None => {
                let id = fresh(&seen);
                seen.insert(id.clone());
                let at = n.range().start
                    + 1
                    + n.tag_name().name().len()
                    + root_prefix_len(xml, n.range().start);
                edits.push((at..at, format!(" id=\"{id}\"")));
            }
        }
    }
    let mut out = xml.to_string();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0.start));
    let n = edits.len();
    for (r, t) in edits {
        out.replace_range(r, &t);
    }
    Ok((out, n))
}

use crate::geom::{affine_inv as mat_inv, affine_mul as mat_mul};

#[cfg(test)]
mod tests {
    use super::*;

    const ILLUSTRATOR: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- Generator: Adobe Illustrator 28.0 -->
<svg version="1.1" id="Layer_1" xmlns="http://www.w3.org/2000/svg" width="50.8mm" height="50.8mm" viewBox="0 0 144 144">
  <style type="text/css">.st0{fill:#E8A33C;} .st1{fill:none;stroke:#2F4858;stroke-width:4;}</style>
  <g id="fill_a_x3D_45">
    <rect x="8" y="8" width="128" height="128" fill="#2F4858"/>
  </g>
  <circle id="sun" class="st0" cx="94" cy="52" r="16"/>
  <path class="st1" d="M20,110 L124,110"/>
  <circle cx="40" cy="40" r="10" fill="#C9452E" stroke="#000" stroke-width="1"/>
</svg>"##;

    fn by_name<'a>(d: &'a Document, words: &str) -> &'a Shape {
        d.shapes
            .iter()
            .find(|s| svg::name_words(&s.id).join(" ") == words)
            .unwrap()
    }

    #[test]
    fn groups_are_tracked_even_without_ids() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
          <g id="Layer_1">
            <g><rect x="0" y="0" width="10" height="10" fill="#f00"/><rect x="20" y="0" width="10" height="10" fill="#0f0"/></g>
            <g id="leaf fill a=30"><circle cx="50" cy="50" r="10" fill="#00f"/><circle cx="80" cy="50" r="5" fill="#00f"/></g>
            <rect x="0" y="80" width="10" height="10" fill="#000"/>
          </g>
        </svg>"##;
        let d = Document::from_svg(svg, "t", None).unwrap();
        assert_eq!(d.shapes.len(), 5);
        let g0 = &d.shapes[0].groups;
        assert_eq!(
            g0.len(),
            1,
            "the unnamed group (Layer_1 is a layer): {g0:?}"
        );
        assert!(g0[0].starts_with("tl-"));
        assert_eq!(d.shapes[1].groups, *g0, "same unnamed group");
        assert_eq!(d.shapes[2].groups[0], "leaf fill a=30");
        assert_eq!(
            d.shapes[2].params.angle,
            Some(30.0),
            "group settings still apply"
        );
        assert!(d.shapes[4].groups.is_empty(), "loose in the layer");
        // The assigned group id survives saving.
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(again.shapes[0].groups, *g0);
    }

    #[test]
    fn layers_are_not_groups_but_a_group_of_everything_is() {
        // Illustrator with one layer: the layer is the <svg>, and artwork
        // grouped in it is a real group even though it holds everything.
        let one = r##"<svg id="Layer_1" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
          <g><rect width="10" height="10"/><rect x="20" width="10" height="10"/></g>
        </svg>"##;
        let d = Document::from_svg(one, "t", None).unwrap();
        assert!(d.shapes.iter().all(|s| s.groups.len() == 1), "a group");

        // Inkscape layers and Figma's clipped frame are layers.
        let ink = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" viewBox="0 0 100 100">
          <g id="layer1" inkscape:groupmode="layer"><rect width="10" height="10"/><g id="pair"><rect x="20" width="10" height="10"/><rect x="40" width="10" height="10"/></g></g>
        </svg>"##;
        let d = Document::from_svg(ink, "t", None).unwrap();
        assert!(d.shapes[0].groups.is_empty());
        assert_eq!(d.shapes[1].groups, vec!["pair".to_string()]);
        let figma = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
          <g clip-path="url(#c)"><rect width="10" height="10"/><rect x="20" width="10" height="10"/></g>
          <defs><clipPath id="c"><rect width="100" height="100"/></clipPath></defs>
        </svg>"##;
        let d = Document::from_svg(figma, "t", None).unwrap();
        assert!(d.shapes.iter().all(|s| s.groups.is_empty()));
    }

    /// Settings are written into ids, so whatever points at an id by name
    /// (a `<use>` copy) has to follow the rename.
    #[test]
    fn renaming_an_object_keeps_the_copies_that_point_at_it() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 100 100">
          <path id="leaf" d="M0 0h20v20h-20z" fill="#0a0"/>
          <use xlink:href="#leaf" x="40"/>
          <use href="#leaf" y="40"/>
        </svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        assert_eq!(d.shapes.len(), 3);
        d.sync().unwrap();
        assert_eq!(d.shapes.len(), 3, "after a sync");
        let saved = d.to_svg().unwrap();
        let again = Document::from_svg(&saved, "t", None).unwrap();
        assert_eq!(again.shapes.len(), 3, "after a save: {saved}");
    }

    #[test]
    fn an_object_can_be_renamed_and_keeps_everything_else() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
          <path id="leaf_satin_w_x3D_2" d="M0 0h20v20h-20z" fill="#0a0" stroke="#050" stroke-width="2"/>
          <use href="#leaf_satin_w_x3D_2" x="40"/>
          <rect x="60" y="60" width="20" height="20" fill="#00a"/>
        </svg>"##;
        let mut d = Document::from_svg(svg, "t", None).unwrap();
        let names = |d: &Document| svg::display_names(&d.shapes);
        let before = d.shapes.len();
        let leaf = d
            .shapes
            .iter()
            .position(|s| s.label.contains("leaf"))
            .unwrap();
        let (kind, width, uid) = (
            d.shapes[leaf].params.kind,
            d.shapes[leaf].params.width,
            d.shapes[leaf].uid,
        );
        d.rename(leaf, "Oak Leaf").unwrap();
        // The name changed (for the fill and its outline alike); the
        // settings, the copy that points at it and its place didn't.
        assert_eq!(d.shapes.len(), before);
        assert_eq!(d.shapes[leaf].uid, uid);
        assert!(names(&d)[leaf].starts_with("Oak Leaf"), "{:?}", names(&d));
        assert_eq!(
            (d.shapes[leaf].params.kind, d.shapes[leaf].params.width),
            (kind, width)
        );
        assert_eq!(
            names(&d).iter().filter(|n| n.contains("Oak Leaf")).count(),
            names(&d).iter().filter(|n| !n.contains("Shape")).count()
        );
        assert!(!names(&d).iter().any(|n| n.contains("leaf")));
        // It's in the file, and reads back.
        let out = d.to_svg().unwrap();
        assert!(out.contains("id=\"Oak_Leaf_"), "{out}");
        let again = Document::from_svg(&out, "t", None).unwrap();
        assert_eq!(again.shapes.len(), before);
        assert!(svg::display_names(&again.shapes)[leaf].starts_with("Oak Leaf"));
        // An object with no name of its own gets one. (By its uid: with
        // copies about, a sync can move objects in the list.)
        let rect_uid = d
            .shapes
            .iter()
            .find(|s| s.color == [0, 0, 0xaa])
            .unwrap()
            .uid;
        let at = |d: &Document| d.shapes.iter().position(|s| s.uid == rect_uid).unwrap();
        let rect = at(&d);
        d.rename(rect, "  back, panel ").unwrap();
        assert_eq!(names(&d)[at(&d)], "back panel");
        // The same name again changes nothing.
        let xml = d.xml.clone();
        let rect = at(&d);
        d.rename(rect, "back panel").unwrap();
        assert_eq!(d.xml, xml);
        // A name can't be nothing, or a stitch setting (which in an id
        // would change how the object sews).
        for bad in ["", "   ", "_;,", "satin", "Front Fill", "trim"] {
            let rect = at(&d);
            assert!(d.rename(rect, bad).is_err(), "{bad:?}");
        }
        assert_eq!(names(&d)[at(&d)], "back panel");
        // (An `=` can't make a setting of it: it's read as a space.)
        assert_eq!(Document::name_words_typed("a=45").unwrap(), ["a", "45"]);
        // Two objects can have the same name: the ids are told apart.
        let rect = at(&d);
        d.rename(rect, "Oak Leaf").unwrap();
        assert_eq!(d.shapes.len(), before);
        assert_eq!(
            names(&d)
                .iter()
                .filter(|n| n.starts_with("Oak Leaf"))
                .count(),
            3
        );
        let again = Document::from_svg(&d.to_svg().unwrap(), "t", None).unwrap();
        assert_eq!(again.shapes.len(), before);
    }

    #[test]
    fn placing_artwork_twice_makes_two_groups() {
        let art = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 20">
          <g id="pair"><rect width="10" height="10"/><rect x="20" width="10" height="10"/></g>
        </svg>"##;
        let mut d = Document::from_svg(art, "t", None).unwrap();
        d.add_svg(art, Placement::default()).unwrap();
        assert_eq!(d.shapes.len(), 4);
        let first = &d.shapes[0].groups;
        let second = &d.shapes[3].groups;
        assert_eq!(first, &vec!["pair".to_string()]);
        assert_eq!(second.len(), 1);
        assert_ne!(first, second, "each copy is its own group: {second:?}");
        assert_eq!(svg::name_words(&second[0]), vec!["pair"]);
    }

    #[test]
    fn adding_the_same_artwork_twice_keeps_each_objects_name_and_settings() {
        let star = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
            <polygon id="star satin" points="5,0 6,4 10,4 7,6 8,10 5,7 2,10 3,6 0,4 4,4" fill="#ff0"/></svg>"##;
        let mut d = Document::from_svg(ILLUSTRATOR, "t", None).unwrap();
        d.add_svg(star, Placement::default()).unwrap();
        let new = d.add_svg(star, Placement::default()).unwrap();
        assert_eq!(new.len(), 1);
        let s = &d.shapes[new[0]];
        assert_eq!(svg::name_words(&s.id), vec!["star"], "{}", s.id);
        assert_eq!(s.params.kind, Some(Kind::Satin));
    }

    #[test]
    fn duplicating_a_group_keeps_it_a_group() {
        let art = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 60 20">
          <g id="pair"><rect width="10" height="10"/><rect x="20" width="10" height="10"/></g>
          <rect x="45" width="10" height="10"/>
        </svg>"##;
        let mut d = Document::from_svg(art, "t", None).unwrap();
        // The group plus the loose rect: the group is rebuilt, the rect stays loose.
        let new = d.duplicate(&[0, 1, 2], P::new(30.0, 30.0)).unwrap();
        assert_eq!(new.len(), 3);
        let gs: Vec<&Vec<String>> = new.iter().map(|&i| &d.shapes[i].groups).collect();
        assert_eq!(gs[0].len(), 1);
        assert_eq!(gs[0], gs[1], "the copies share a new group");
        assert_ne!(gs[0][0], "pair");
        assert!(gs[2].is_empty());
        // Copying one piece of a group doesn't make a new group.
        let one = d.duplicate(&[0], P::new(30.0, 30.0)).unwrap();
        assert!(d.shapes[one[0]].groups.is_empty());
    }

    #[test]
    fn assigns_ids_and_centers() {
        let d = Document::from_svg(ILLUSTRATOR, "t", None).unwrap();
        assert_eq!(
            d.shapes.len(),
            5,
            "rect, sun, line, circle fill + circle stroke"
        );
        assert!(d.shapes.iter().all(|s| !s.id.is_empty()));
        let b = svg::shapes_bounds(&d.shapes);
        assert!(
            b.center().len() < 1.0,
            "centered on the hoop: {:?}",
            b.center()
        );
        assert_eq!(d.shapes[0].params.angle, Some(45.0), "group settings apply");
    }

    #[test]
    fn settings_round_trip_through_ids() {
        let mut d = Document::from_svg(ILLUSTRATOR, "t", None).unwrap();
        let sun = d.shapes.iter().position(|s| s.id == "sun").unwrap();
        d.shapes[sun].params.kind = Some(Kind::Satin);
        d.shapes[sun].params.spacing = Some(0.35);
        d.shapes[sun].color = [10, 20, 30];
        let circle_stroke = d
            .shapes
            .iter()
            .position(|s| {
                s.part == Part::Stroke
                    && s.id.starts_with("tl-")
                    && matches!(s.geometry, Geometry::Stroke { width, .. } if width < 5.0)
            })
            .unwrap();
        d.shapes[circle_stroke].params.kind = Some(Kind::Skip);
        d.settings.fabric = "thin-cap".into();
        d.settings.hat_order = true;
        // Sew the sun first.
        let s = d.shapes.remove(sun);
        d.shapes.insert(0, s);

        let saved = d.to_svg().unwrap();
        assert!(
            saved.contains("<!-- Generator: Adobe Illustrator 28.0 -->"),
            "rest of file preserved"
        );
        assert!(saved.contains(".st0{fill:#E8A33C;}"));
        let e = Document::from_svg(&saved, "t", None).unwrap();
        assert_eq!(e.settings.fabric, "thin-cap");
        assert!(e.settings.hat_order);
        let sun2 = by_name(&e, "sun");
        assert_eq!(sun2.params.kind, Some(Kind::Satin));
        assert_eq!(sun2.params.spacing, Some(0.35));
        assert_eq!(sun2.color, [10, 20, 30]);
        assert_eq!(sun2.paint, [0xe8, 0xa3, 0x3c], "SVG paint untouched");
        assert_eq!(svg::name_words(&e.shapes[0].id), vec!["sun"], "order kept");
        // The circle's fill keeps its kind while its stroke stays skipped.
        let circle: Vec<&Shape> = e
            .shapes
            .iter()
            .filter(|s| {
                s.paint == [0xc9, 0x45, 0x2e] || (s.part == Part::Stroke && s.paint == [0, 0, 0])
            })
            .collect();
        assert_eq!(
            circle
                .iter()
                .find(|s| s.part == Part::Fill)
                .unwrap()
                .params
                .kind,
            Some(Kind::Fill)
        );
        assert_eq!(
            circle
                .iter()
                .find(|s| s.part == Part::Stroke)
                .unwrap()
                .params
                .kind,
            Some(Kind::Skip)
        );
        // Saving twice is stable.
        assert_eq!(e.to_svg().unwrap(), saved);
    }

    #[test]
    fn illustrator_style_escaped_ids_parse_after_save() {
        let mut d = Document::from_svg(ILLUSTRATOR, "t", None).unwrap();
        d.shapes[0].params.spacing = Some(0.4);
        let saved = d.to_svg().unwrap();
        // Simulate Illustrator re-exporting: it escapes '=' and '.' in ids.
        let reexported = saved.replace('=', "_x3D_").replace("0.4", "0_x2E_4");
        let reexported = reexported
            .replace("xmlns_x3D_", "xmlns=")
            .replace("version_x3D_", "version=");
        let fixed: String = fix_attr_eq(&reexported);
        let e = Document::from_svg(&fixed, "t", None).unwrap();
        assert_eq!(e.shapes[0].params.spacing, Some(0.4));
        assert_eq!(e.shapes[0].params.angle, Some(45.0));
    }

    /// Undo the over-eager '=' replacement outside of id values in the test above.
    fn fix_attr_eq(s: &str) -> String {
        let re = [
            "width",
            "height",
            "viewBox",
            "x",
            "y",
            "cx",
            "cy",
            "r",
            "d",
            "class",
            "fill",
            "stroke",
            "stroke-width",
            "id",
            "type",
            "encoding",
            SETTINGS_ATTR,
            OLD_SETTINGS_ATTR,
        ];
        let mut out = s.to_string();
        for a in re {
            out = out.replace(&format!(" {a}_x3D_\""), &format!(" {a}=\""));
        }
        out.replace("<?xml version_x3D_", "<?xml version=")
            .replace("encoding_x3D_", "encoding=")
    }

    #[test]
    fn delete_removes_element_or_skips_part() {
        let mut d = Document::from_svg(ILLUSTRATOR, "t", None).unwrap();
        let sun = d.shapes.iter().position(|s| s.id == "sun").unwrap();
        d.delete(sun).unwrap();
        assert!(!d.xml.contains("id=\"sun\""));
        let stroke = d
            .shapes
            .iter()
            .position(|s| s.part == Part::Stroke && s.paint == [0, 0, 0])
            .unwrap();
        d.delete(stroke).unwrap();
        d.sync().unwrap();
        assert_eq!(d.shapes.len(), 3);
        assert!(d.shapes.iter().all(|s| s.paint != [0, 0, 0]));
    }

    #[test]
    fn add_svg_places_and_scales_in_mm() {
        let mut d = Document::from_svg(ILLUSTRATOR, "t", None).unwrap();
        let star = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
            <polygon id="star satin" points="5,0 6,4 10,4 7,6 8,10 5,7 2,10 3,6 0,4 4,4" fill="#ff0"/></svg>"##;
        let place = Placement {
            center_mm: Some((10.0, -5.0)),
            width_mm: Some(12.0),
            height_mm: None,
        };
        let new = d.add_svg(star, place).unwrap();
        assert_eq!(new.len(), 1);
        let s = &d.shapes[new[0]];
        assert_eq!(
            s.params.kind,
            Some(Kind::Satin),
            "settings in the added SVG's ids apply"
        );
        let b = svg::shapes_bounds(std::slice::from_ref(s));
        assert!((b.width() - 120.0).abs() < 0.5, "12 mm wide: {}", b.width());
        assert!(
            (b.center().x - 100.0).abs() < 0.5 && (b.center().y + 50.0).abs() < 0.5,
            "at (10, -5) mm: {:?}",
            b.center()
        );
        // Survives a save/load cycle in place.
        let saved = d.to_svg().unwrap();
        let mut e = Document::from_svg(&saved, "t", None).unwrap();
        e.offset = d.offset;
        e.import(&HashMap::new()).unwrap();
        let s2 = e
            .shapes
            .iter()
            .find(|x| svg::name_words(&x.id) == vec!["star"])
            .unwrap();
        let b2 = svg::shapes_bounds(std::slice::from_ref(s2));
        assert!((b2.center().x - b.center().x).abs() < 0.5);
    }

    #[test]
    fn empty_document_uses_mm_around_hoop_center() {
        let mut d = Document::empty();
        assert!(d.shapes.is_empty());
        let dot = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10mm" height="10mm" viewBox="0 0 10 10"><circle cx="5" cy="5" r="5" fill="red"/></svg>"#;
        let new = d
            .add_svg(
                dot,
                Placement {
                    center_mm: Some((-20.0, 0.0)),
                    ..Default::default()
                },
            )
            .unwrap();
        let b = svg::shapes_bounds(&d.shapes[new[0]..=new[0]]);
        assert!(
            (b.width() - 100.0).abs() < 1.0,
            "keeps its 10 mm size: {}",
            b.width()
        );
        assert!((b.center().x + 200.0).abs() < 1.0);
        assert!(d.to_svg().unwrap().contains("data-zigzag="));
    }

    #[test]
    fn preview_color_is_saved_with_the_design() {
        let mut d = Document::empty();
        assert!(
            !d.to_svg().unwrap().contains("cloth="),
            "the default isn't written"
        );
        d.settings.cloth = [0x1a, 0x2b, 0x3c];
        let out = d.to_svg().unwrap();
        assert!(out.contains("cloth=1a2b3c"), "{out}");
        let back = Document::from_svg(&out, "t", None).unwrap();
        assert_eq!(back.settings.cloth, [0x1a, 0x2b, 0x3c]);
    }

    #[test]
    fn the_hoop_is_saved_as_a_size_and_old_files_still_read() {
        let mut d = Document::empty();
        assert_eq!(d.settings.hoop, DEFAULT_HOOP);
        d.settings.hoop = (152.5, 220.0);
        let out = d.to_svg().unwrap();
        assert!(out.contains("hoop=152.5x220"), "{out}");
        let back = Document::from_svg(&out, "t", None).unwrap();
        assert_eq!(back.settings.hoop, (152.5, 220.0));
        // Up to 0.1.2 the hoop was its place in a list of three.
        for (old, size) in [
            ("0", (100.0, 100.0)),
            ("1", (70.0, 70.0)),
            ("2", (130.0, 180.0)),
            ("9", DEFAULT_HOOP),
            ("5x20000", DEFAULT_HOOP),
            ("junk", DEFAULT_HOOP),
        ] {
            let s = Settings::parse(&format!("fabric=standard hoop={old}"));
            assert_eq!(s.hoop, size, "hoop={old}");
        }
        assert_eq!(parse_hoop("130 × 180"), Some((130.0, 180.0)));
        assert_eq!(parse_hoop("130"), None);
    }

    #[test]
    fn the_machine_is_saved_as_a_label_only_when_one_is_chosen() {
        let mut d = Document::empty();
        assert_eq!(d.settings.machine, None);
        // No machine, no token: files written before stay as they were.
        assert!(!d.to_svg().unwrap().contains("machine="));
        let melco = crate::machine::profile::builtin_named("melco-summit").unwrap();
        melco.apply(&mut d.settings, false);
        let out = d.to_svg().unwrap();
        assert!(out.contains("hoop=410x393 machine=melco-summit"), "{out}");
        let back = Document::from_svg(&out, "t", None).unwrap();
        assert_eq!(back.settings, d.settings);
        // A machine this computer has no profile for is still the label,
        // and the hoop it chose is still the hoop.
        let s = Settings::parse("fabric=standard hoop=300x210 machine=shop-barudan");
        assert_eq!(s.machine.as_deref(), Some("shop-barudan"));
        assert_eq!(s.hoop, (300.0, 210.0));
        // Not a name a file can hold: no machine.
        assert_eq!(Settings::parse("machine=Bad_Name").machine, None);
    }

    #[test]
    fn the_shortest_stitch_and_the_locks_are_saved_when_they_are_not_the_usual() {
        use crate::formats::LockStyle;
        let mut d = Document::empty();
        let out = d.to_svg().unwrap();
        assert!(!out.contains("minst=") && !out.contains("lock="), "{out}");
        assert_eq!(d.settings.export_options(), Default::default());
        d.settings.min_stitch_mm = 0.4;
        d.settings.lock = Lock::new(LockStyle::Triangle, 0.5, 3);
        let out = d.to_svg().unwrap();
        assert!(out.contains("minst=0.4 lock=triangle:0.5:3"), "{out}");
        let back = Document::from_svg(&out, "t", None).unwrap();
        assert_eq!(back.settings, d.settings);
        assert_eq!(back.settings.export_options().min_stitch, 4.0);
        assert!(back.plan_options().min_stitch < 4.0 && back.plan_options().min_stitch > 3.99);
        // How narrow a satin may be before it's warned about, likewise.
        assert!(!out.contains("narrowsatin="), "{out}");
        d.settings.narrow_satin_mm = 0.8;
        let out = d.to_svg().unwrap();
        assert!(out.contains(" narrowsatin=0.8"), "{out}");
        let back = Document::from_svg(&out, "t", None).unwrap();
        assert_eq!(back.plan_options().narrow_satin, 0.8);
        assert_eq!(Settings::parse("narrowsatin=9").narrow_satin_mm, 3.0);
        // Out of range is brought in; nonsense is the usual.
        let s = Settings::parse("minst=30 lock=knot");
        assert_eq!((s.min_stitch_mm, s.lock), (1.0, Lock::default()));
        assert_eq!(Settings::parse("lock=none").lock.style, LockStyle::None);
    }

    #[test]
    fn a_shortest_stitch_holds_through_the_plan_and_the_file() {
        // Fills (0.25 mm rows), a satin that narrows to nothing, objects a
        // short hop apart: all sources of stitches under 0.4 mm.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60mm" height="60mm" viewBox="0 0 60 60">
            <circle cx="15" cy="15" r="10" fill="#203040"/>
            <rect id="fill_a_x3D_45" x="30" y="5" width="12" height="12" fill="#203040"/>
            <rect x="42.5" y="5" width="12" height="12" fill="#203040"/>
            <path id="satin" d="M10 40 Q30 30 50 40 Q30 50 10 40 Z" fill="#203040"/>
            <path d="M5 55 L55 55" fill="none" stroke="#203040" stroke-width="0.3"/>
        </svg>"##;
        let mut doc = Document::from_svg(svg, "t", None).unwrap();
        let shortest = |doc: &Document| {
            let d = doc.plan();
            let plan = d
                .blocks
                .iter()
                .flat_map(|b| &b.runs)
                .flat_map(|r| r.pts.windows(2).map(|w| w[0].dist(w[1])))
                .fold(f64::INFINITY, f64::min);
            let cmds = crate::formats::dst::stream_with(&d, &doc.settings.export_options());
            let file = cmds
                .windows(2)
                .filter(|w| {
                    w[0].op == crate::formats::Op::Stitch && w[1].op == crate::formats::Op::Stitch
                })
                .map(|w| P::new(w[0].x, w[0].y).dist(P::new(w[1].x, w[1].y)))
                .fold(f64::INFINITY, f64::min);
            (plan, file, d.stitch_count())
        };
        let (plan, file, usual) = shortest(&doc);
        assert!(
            plan < 3.0 && file < 3.0,
            "the usual plan has short stitches"
        );
        for mm in [0.4, 0.6] {
            doc.settings.min_stitch_mm = mm;
            let (plan, file, n) = shortest(&doc);
            assert!(plan >= mm * 10.0 - 1e-6, "{mm}: {plan} in the plan");
            assert!(file >= mm * 10.0, "{mm}: {file} in the file");
            // Merged or sewn in another order, not thinned out.
            assert!(n as f64 > usual as f64 * 0.97, "{mm}: {n} of {usual}");
        }
    }

    #[test]
    fn the_object_map_says_which_object_every_command_is_for() {
        // Two squares in one thread (joined: the second begins where the
        // first ends, near enough to sew across) and a line in another.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60mm" height="30mm" viewBox="0 0 60 30">
            <rect id="left" x="5" y="5" width="12" height="12" fill="#203040"/>
            <rect id="right" x="17.5" y="5" width="12" height="12" fill="#203040"/>
            <path id="line" d="M35 5 L55 25" fill="none" stroke="#c00000" stroke-width="0.3"/>
        </svg>"##;
        let doc = Document::from_svg(svg, "t", None).unwrap();
        let d = doc.plan();
        for ext in ["dst", "exp", "pes", "jef", "vp3"] {
            let map = doc.object_map(&d, ext, &format!("t.{ext}")).unwrap();
            let ranges = map["ranges"].as_array().unwrap();
            // Every command once, in order, none left out.
            let mut next = 0;
            for r in ranges {
                assert_eq!(r["first"], next, "{ext}");
                next = r["last"].as_u64().unwrap() + 1;
            }
            // (The same stream the writers write.)
            if ext == "dst" {
                let cmds = crate::formats::dst::stream_with(&d, &doc.settings.export_options());
                assert_eq!(next as usize, cmds.len());
                let n = cmds
                    .iter()
                    .filter(|c| c.op == crate::formats::Op::Stitch)
                    .count() as u64;
                let last = ranges
                    .iter()
                    .filter_map(|r| r["last_stitch"].as_u64())
                    .max()
                    .unwrap();
                assert_eq!(last + 1, n);
            }
            // Left, right, a change of thread, the line, the end.
            let who: Vec<Option<u64>> = ranges.iter().map(|r| r["object"].as_u64()).collect();
            assert_eq!(who, [Some(0), Some(1), None, Some(2), None], "{ext}");
            // Each object by name, with its share of the stitches.
            let objects = map["objects"].as_array().unwrap();
            let names: Vec<&str> = objects
                .iter()
                .map(|o| o["name"].as_str().unwrap())
                .collect();
            assert_eq!(names, ["left", "right", "line"]);
            assert_eq!(objects[2]["kind"], "run");
            for (o, r) in objects.iter().zip([&ranges[0], &ranges[1], &ranges[3]]) {
                assert_eq!(
                    o["commands"],
                    r["last"].as_u64().unwrap() - r["first"].as_u64().unwrap() + 1
                );
                assert!(
                    o["stitches"].as_u64().unwrap()
                        >= d.per_shape[o["index"].as_u64().unwrap() as usize] as u64 - 2
                );
            }
            // The line's stitches begin (with the jump to them) after the
            // change of thread, where the squares ended; all on the cloth.
            for r in ranges {
                let (x, y) = (
                    r["at_mm"][0].as_f64().unwrap(),
                    r["at_mm"][1].as_f64().unwrap(),
                );
                assert!(x.abs() <= 30.0 && y.abs() <= 15.0, "{ext}: {r}");
            }
        }
        assert!(doc.object_map(&d, "pen", "t.pen").is_none());
        assert_eq!(
            Document::object_map_path(Path::new("/tmp/a b/logo.exp")),
            Path::new("/tmp/a b/logo.exp.objects.json")
        );
    }

    #[test]
    fn old_threadline_settings_attribute_still_reads_and_is_renamed() {
        let xml = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10mm" height="10mm" viewBox="0 0 10 10" data-threadline="fabric=standard overlaps=1 hat=0 hoop=0"><circle id="dot" cx="5" cy="5" r="5" fill="red"/></svg>"#;
        let d = Document::from_svg(xml, "old", None).unwrap();
        assert!(
            d.settings.remove_overlaps,
            "settings read from data-threadline"
        );
        let out = d.to_svg().unwrap();
        assert!(
            out.contains("data-zigzag=\"fabric=standard overlaps=1"),
            "{out}"
        );
        assert!(!out.contains("data-threadline"), "{out}");
    }
}
