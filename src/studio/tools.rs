//! MCP tools: what agents can do with the live studio document.

use crate::app::{HOOPS, SPM, Studio, shape_bounds};
use crate::mcp::{Content, ToolResult};
use crate::widgets::{allowed_kinds, kind_name};
use serde_json::{Value, json};
use std::path::PathBuf;
use zigzag::checks::{Issue, Level};
use zigzag::document::{Document, Placement};
use zigzag::formats::preview::{PreviewOptions, render_png};
use zigzag::geom::P;
use zigzag::params::{FABRICS, Kind, Params, parse_hex, underlay};
use zigzag::svg::{Geometry, Part};

pub const INSTRUCTIONS: &str = "ZigZag is a machine-embroidery editor showing live documents on a hoop, one per tab; the tools work on the one in front (list_documents, switch_document, new_document). \
Coordinates are millimetres from the hoop center (+x right, +y down). The person watching sees every change, and each tool call is one undo step. \
Typical flow: get_design → add_svg (any SVG artwork) and add_text (lettering) → transform_objects to size and place → set_object to choose stitch types → render_preview to look at the stitches → check_design → save, or export (machine file or html worksheet). \
Stitch types: fill (tatami rows; areas wider than ~7 mm), satin (glossy zigzag columns 1–7 mm wide; works on strokes, and on narrow filled shapes like letters, bars and rings), run (thin running stitch for details), \
contour (rows following the outline inward), blanket (E-stitch edge), applique (placement line, stop, tack-down, stop, satin cover) and manual (every path point is a needle point). \
Settings can also be preset in SVG element ids, e.g. id=\"logo_fill_a=45\" or id=\"border_satin_w=2\". \
Hat designs are small (about 50 × 50 mm on the front of a cap): keep satin columns under 7 mm wide, letters at least 5 mm tall, and details at least 1.5 mm. \
open_file also opens machine files (pes, dst, exp, jef) as manual stitches. \
trace_image turns a PNG/JPEG logo into shapes (snap colors to Brother or Janome threads with `threads`). round_corners softens sharp corners, which stitch more cleanly; simplify takes extra points out of outlines (traced ones especially) and clean_up removes stray points, objects too small to sew and extra points across the design. \
Use batch to make several changes in one call: it is all or nothing and one undo step, and later steps can use earlier results (\"$0.added.0.name\").";

fn obj_schema() -> Value {
    json!({ "description": "Object: its index in sewing order (0-based) or its name from get_design.", "oneOf": [{ "type": "integer" }, { "type": "string" }] })
}

fn text_props(new: bool) -> Value {
    let mut p = json!({
        "text": { "type": "string" },
        "font": { "type": "string", "description": "Font family (see list_fonts). Default: a clean sans." },
        "size_mm": { "type": "number", "description": "Capital letter height. Default 8." },
        "spacing_mm": { "type": "number", "description": "Extra letter spacing. Default 0.4." },
        "arc_mm": { "type": "number", "description": "Arc radius: 0 straight, >0 over the top, <0 along the bottom." },
        "bold": { "type": "boolean" },
        "italic": { "type": "boolean" },
        "color": { "type": "string", "description": "#rrggbb" },
        "kind": { "type": "string", "enum": ["satin", "fill", "run"], "description": "Default satin." },
        "leading": { "type": "number", "description": "Line spacing for multi-line text (newlines in `text`), as a multiple of the letter height. Default 1.5." },
        "align": { "type": "string", "enum": ["left", "center", "right"], "description": "Line alignment. Default center." },
        "h_scale": { "type": "number", "description": "Horizontal scale, percent. Default 100." },
        "v_scale": { "type": "number", "description": "Vertical scale, percent. Default 100." },
        "case": { "type": "string", "enum": ["as", "upper", "lower", "title"], "description": "Letter case applied to the text. Default as typed." },
        "on_path": { "description": "Put the text on an existing path object's outline (the object is then not sewn); null takes it off.", "oneOf": [{ "type": "integer" }, { "type": "string" }, { "type": "null" }] },
        "path_offset": { "type": "number", "description": "On a path: where the text starts, 0–1 along it." },
        "path_flip": { "type": "boolean", "description": "On a path: letters on the other side." },
        "kern": { "type": "array", "description": "Pair kerning: for each, the 0-based character index and how far (mm) to move it and the rest of the line; replaces any kerning set before.", "items": { "type": "object", "properties": { "at": { "type": "integer" }, "mm": { "type": "number" } }, "required": ["at", "mm"] } }
    });
    if new {
        p["x_mm"] = json!({ "type": "number", "description": "Center x (mm from hoop center). Default 0." });
        p["y_mm"] = json!({ "type": "number", "description": "Center y. Default 0." });
    } else {
        p["object"] = obj_schema();
    }
    p
}

/// Apply text tool arguments onto a spec.
fn apply_text_args(spec: &mut zigzag::document::TextSpec, args: &Value) -> Result<(), String> {
    if let Some(v) = args.get("text") {
        spec.text = v.as_str().ok_or("text must be a string")?.to_string();
    }
    if let Some(v) = args.get("font") {
        let want = v.as_str().ok_or("font must be a string")?;
        let fonts = zigzag::document::font_families();
        spec.font = fonts
            .iter()
            .find(|f| f.eq_ignore_ascii_case(want))
            .cloned()
            .ok_or_else(|| format!("font '{want}' isn't installed; call list_fonts"))?;
    }
    let num = |k: &str| {
        args.get(k)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
    };
    if let Some(v) = num("size_mm") {
        spec.size_mm = v.clamp(2.0, 80.0);
    }
    if let Some(v) = num("spacing_mm") {
        spec.spacing_mm = v.clamp(-2.0, 10.0);
    }
    if let Some(v) = num("arc_mm") {
        spec.arc_mm = if v.abs() < 0.5 {
            0.0
        } else {
            v.signum() * v.abs().clamp(5.0, 200.0)
        };
    }
    if let Some(v) = args.get("bold").and_then(Value::as_bool) {
        spec.bold = v;
    }
    if let Some(v) = args.get("italic").and_then(Value::as_bool) {
        spec.italic = v;
    }
    if let Some(v) = args.get("color") {
        spec.color = v
            .as_str()
            .and_then(parse_hex)
            .ok_or("color must look like #rrggbb")?;
    }
    if let Some(v) = args.get("kind") {
        spec.kind = match v.as_str() {
            Some("satin") => Kind::Satin,
            Some("fill") => Kind::Fill,
            Some("run") => Kind::Run,
            _ => return Err("kind must be satin, fill or run".into()),
        };
    }
    if let Some(v) = num("leading") {
        spec.leading = v.clamp(0.5, 5.0);
    }
    if let Some(v) = args.get("align") {
        spec.align = v
            .as_str()
            .and_then(zigzag::document::Align::parse)
            .ok_or("align must be left, center or right")?;
    }
    if let Some(v) = num("h_scale") {
        spec.h_scale = v.clamp(20.0, 400.0);
    }
    if let Some(v) = num("v_scale") {
        spec.v_scale = v.clamp(20.0, 400.0);
    }
    if let Some(v) = args.get("case") {
        spec.case = v
            .as_str()
            .and_then(zigzag::document::Case::parse)
            .ok_or("case must be as, upper, lower or title")?;
    }
    if let Some(v) = num("path_offset") {
        spec.path_offset = v.clamp(0.0, 1.0);
    }
    if let Some(v) = args.get("path_flip").and_then(Value::as_bool) {
        spec.path_flip = v;
    }
    if let Some(list) = args.get("kern") {
        let list = list.as_array().ok_or("kern must be a list of {at, mm}")?;
        spec.kern.clear();
        for item in list {
            let at = item
                .get("at")
                .and_then(Value::as_u64)
                .ok_or("kern.at must be an index")? as usize;
            let mm = item
                .get("mm")
                .and_then(Value::as_f64)
                .ok_or("kern.mm must be a number")?;
            spec.set_kern(at, mm.clamp(-5.0, 5.0));
        }
    }
    Ok(())
}

/// The `tools/list` result.
pub fn tool_specs() -> Value {
    let fabrics: Vec<&str> = FABRICS.iter().map(|f| f.id).collect();
    let hoops: Vec<&str> = HOOPS.iter().map(|h| h.0).collect();
    let mut tools = vec![
        json!({
            "name": "get_design",
            "description": "Describe the current document: every object (index, name, stitch type, color, position and size in mm, effective settings, stitch count, problems), design stats, fabric, hoop and options.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "add_svg",
            "description": "Add SVG artwork to the canvas (or replace the document). Every filled shape and stroke becomes an object; fills default to fill stitch, strokes to satin (≥1 mm) or run. Settings in element ids apply (e.g. id=\"leaf_satin\"). By default the artwork keeps its own size (viewBox units scaled by width/height attributes, else 1 unit = 1 px) and is centered on the hoop.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "svg": { "type": "string", "description": "A complete <svg> document." },
                    "x_mm": { "type": "number", "description": "Center of the artwork, mm right of the hoop center. Default 0." },
                    "y_mm": { "type": "number", "description": "Center of the artwork, mm below the hoop center. Default 0." },
                    "width_mm": { "type": "number", "description": "Scale the artwork to this width." },
                    "height_mm": { "type": "number", "description": "Scale the artwork to this height (with width_mm: fit inside both)." },
                    "replace": { "type": "boolean", "description": "Start a new document with only this artwork. Default false." }
                },
                "required": ["svg"]
            }
        }),
        json!({
            "name": "set_object",
            "description": "Change how an object is stitched. Omitted fields stay as they are; pass null to reset a field to the fabric's default.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "object": obj_schema(),
                    "kind": { "type": "string", "enum": ["fill", "satin", "run", "contour", "blanket", "applique", "meander", "ripple", "cross", "tartan", "manual", "skip"] },
                    "angle": { "type": ["number", "null"], "description": "Fill row direction in degrees (0 = horizontal, 90 = vertical)." },
                    "spacing_mm": { "type": ["number", "null"], "description": "Fill/contour row spacing, satin zigzag spacing, or blanket prong spacing." },
                    "length_mm": { "type": ["number", "null"], "description": "Max stitch length for fill and run." },
                    "pull_mm": { "type": ["number", "null"], "description": "Pull compensation per side." },
                    "width_mm": { "type": ["number", "null"], "description": "Satin width (strokes and borders), appliqué cover width, or blanket prong length." },
                    "split_mm": { "type": ["number", "null"], "description": "Satin auto-split: split stitches longer than this (0 = off; default 7)." },
                    "border": { "type": ["boolean", "null"], "description": "Satin on a filled shape: true = along its outline, false = across it as a column." },
                    "spiral": { "type": ["boolean", "null"], "description": "Contour fill as one spiral out from the center (or the start point) instead of rings." },
                    "underlay": { "type": ["boolean", "null"] },
                    "satin_underlay": { "description": "Satin underlay passes, or \"auto\".", "oneOf": [{ "type": "array", "items": { "type": "string", "enum": ["center", "contour", "zigzag"] } }, { "type": "string", "enum": ["auto"] }] },
                    "short_stitches": { "type": ["boolean", "null"] },
                    "repeats": { "type": ["integer", "null"], "description": "Running stitch passes: 1 or 3 (bean)." },
                    "color": { "type": ["string", "null"], "description": "Thread color #rrggbb, or null for the SVG's color." },
                    "start": { "type": ["number", "null"], "description": "Where sewing starts: 0–1 along the object's outline (fills: the outer ring from its first point; lines: from their start). Null: the planner chooses." },
                    "end": { "type": ["number", "null"], "description": "Where sewing ends, 0–1 along the outline (the thread goes on from there to the next object)." },
                    "trim": { "type": ["boolean", "null"], "description": "Cut the thread after this object even if the next one is close." },
                    "stop": { "type": ["boolean", "null"], "description": "Pause the machine after this object (to lay foam, or trim appliqué fabric)." }
                },
                "required": ["object"]
            }
        }),
        json!({
            "name": "delete_object",
            "description": "Delete an object.",
            "inputSchema": { "type": "object", "properties": { "object": obj_schema() }, "required": ["object"] },
            "annotations": { "destructiveHint": true }
        }),
        json!({
            "name": "move_object",
            "description": "Move an object in the sewing order. Later objects sew on top of earlier ones.",
            "inputSchema": { "type": "object", "properties": { "object": obj_schema(), "to": { "type": "integer", "description": "New 0-based position." } }, "required": ["object", "to"] }
        }),
        json!({
            "name": "transform_objects",
            "description": "Move, resize, rotate or mirror objects (together, around their combined center). Applied in order: size/scale, mirror, rotate, then move. Fill angles turn with the objects. Changes are written into the SVG as transforms.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "objects": { "description": "One object (index or name) or a list of them.", "oneOf": [{ "type": "integer" }, { "type": "string" }, { "type": "array", "items": { "oneOf": [{ "type": "integer" }, { "type": "string" }] } }] },
                    "width_mm": { "type": "number", "description": "Resize to this width (keeps proportions unless height_mm is also given)." },
                    "height_mm": { "type": "number", "description": "Resize to this height." },
                    "scale": { "type": "number", "description": "Scale factor, e.g. 1.5." },
                    "mirror": { "type": "string", "enum": ["horizontal", "vertical"] },
                    "rotate_deg": { "type": "number", "description": "Clockwise degrees." },
                    "move_to_mm": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Put the objects' center here [x, y] (mm from hoop center)." },
                    "move_by_mm": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Shift by [dx, dy] mm." }
                },
                "required": ["objects"]
            }
        }),
        json!({
            "name": "duplicate_objects",
            "description": "Copy objects with all their settings. The copies sew last and are selected.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "objects": { "description": "One object (index or name) or a list of them.", "oneOf": [{ "type": "integer" }, { "type": "string" }, { "type": "array", "items": { "oneOf": [{ "type": "integer" }, { "type": "string" }] } }] },
                    "offset_mm": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "Where the copies go relative to the originals. Default [3, 3]." }
                },
                "required": ["objects"]
            }
        }),
        json!({
            "name": "add_text",
            "description": "Add lettering in a system font: straight, or on an arc (positive arc_mm curves over the top like a badge's top text, negative along the bottom). The text stays editable with edit_text. Satin letters should be at least 5 mm tall.",
            "inputSchema": {
                "type": "object",
                "properties": text_props(true),
                "required": ["text"]
            }
        }),
        json!({
            "name": "edit_text",
            "description": "Change a lettering object's text or settings. It keeps its position, rotation and stitch settings.",
            "inputSchema": {
                "type": "object",
                "properties": text_props(false),
                "required": ["object"]
            }
        }),
        json!({
            "name": "list_fonts",
            "description": "Font families installed on this computer, for add_text.",
            "inputSchema": { "type": "object", "properties": { "search": { "type": "string" } } },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "open_file",
            "description": "Open a file in a new tab (or the empty tab in front), which becomes the document the other tools work on: an SVG, or a machine file (pes, pec, dst, exp, jef) which opens as manual stitches you can move, scale and recolor. A file that's already open comes to the front instead, keeping its unsaved changes.",
            "inputSchema": { "type": "object", "properties": { "path": { "type": "string", "description": "Absolute path." } }, "required": ["path"] }
        }),
        json!({
            "name": "list_documents",
            "description": "The documents open in the studio's tabs, in order: index, name, file, unsaved changes, and which one is in front (the one every other tool works on).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "switch_document",
            "description": "Bring another open document to the front, so the other tools work on it. By its index from list_documents, or its name.",
            "inputSchema": { "type": "object", "properties": {
                "index": { "type": "integer" },
                "name": { "type": "string" }
            } }
        }),
        json!({
            "name": "new_document",
            "description": "Open a new, empty document in a new tab and bring it to the front (the document you were on stays open in its tab).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "group_colors",
            "description": "Reorder objects to cut thread changes, without moving anything above an object it should sit under.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "set_document",
            "description": "Document-wide settings.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "fabric": { "type": "string", "enum": fabrics, "description": "Fabric preset; sets default densities and compensation." },
                    "hoop": { "type": "string", "enum": hoops },
                    "remove_overlaps": { "type": "boolean", "description": "Skip fill hidden under later objects." },
                    "hat_order": { "type": "boolean", "description": "Sew center-out and bottom-up (for caps)." },
                    "connect_mm": { "type": "number", "description": "Sew across gaps up to this long (mm), between objects or the separate parts of one, with running stitches instead of cutting the thread, where there is no way round out of sight (under a fill or what is sewn on top: that travel is made anyway); 0 cuts those." },
                    "preview_color": { "type": "string", "description": "Fabric color for previews, #rrggbb (e.g. the hat's color)." }
                }
            }
        }),
        json!({
            "name": "render_preview",
            "description": "Render the stitches as they'll be sewn (PNG), on the preview fabric color with the hoop outline.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "width_px": { "type": "integer", "description": "Image width, 200–2000. Default 900." },
                    "show_jumps": { "type": "boolean", "description": "Draw jump stitches. Default true." },
                    "fit": { "type": "string", "enum": ["design", "hoop"], "description": "Frame the design closely or show the whole hoop. Default design." }
                }
            },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "check_design",
            "description": "List problems an experienced digitizer would flag (sizes, satin widths, hoop fit, lettering size).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "get_svg",
            "description": "The document as SVG, with every object's stitch settings written into its id (what Save writes).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "select",
            "description": "Select an object in the editor so the person sees it highlighted (null to clear).",
            "inputSchema": { "type": "object", "properties": { "object": { "oneOf": [{ "type": "integer" }, { "type": "string" }, { "type": "null" }] } } }
        }),
        json!({
            "name": "undo",
            "description": "Undo the last change (yours or the person's).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "save",
            "description": "Save the document as SVG (settings stored in object ids). Without a path, saves over the open file.",
            "inputSchema": { "type": "object", "properties": { "path": { "type": "string", "description": "Absolute path ending in .svg." } } }
        }),
        json!({
            "name": "export",
            "description": "Write a machine file: pes (Brother, Baby Lock), dst (Tajima, most commercial machines), exp (Melco, Bernina), jef (Janome), vp3 (Husqvarna Viking, Pfaff), pec, or pen (raw Skitch stream). Or html: a printable production worksheet (picture, size, time, thread list with Brother/Janome names).",
            "inputSchema": { "type": "object", "properties": { "format": { "type": "string", "enum": ["pes", "dst", "exp", "jef", "vp3", "pec", "pen", "html"] }, "path": { "type": "string", "description": "Absolute output path." } }, "required": ["format", "path"] }
        }),
        json!({
            "name": "trace_image",
            "description": "Trace a raster logo (png, jpg, webp, gif, bmp, tiff) into stitchable shapes and add them (or start a new document with them). Flat logos trace exactly (edges, corners, circles); photos are posterized. Use `threads` to snap every color to a thread chart.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path of the image." },
                    "colors": { "type": "integer", "description": "Thread colors, 2–10. Default: found automatically." },
                    "width_mm": { "type": "number", "description": "Width of the traced artwork. Default: 80% of the hoop." },
                    "x_mm": { "type": "number", "description": "Center x (mm from hoop center). Default 0." },
                    "y_mm": { "type": "number", "description": "Center y. Default 0." },
                    "remove_background": { "type": "boolean", "description": "Leave out white areas. Default true." },
                    "threads": { "type": "string", "enum": ["none", "brother", "janome"], "description": "Snap colors to this thread chart. Default none." },
                    "lines": { "type": "boolean", "description": "Trace thin lines as single lines (run/satin) instead of thin fills. Default false." },
                    "line_width_mm": { "type": "number", "description": "With lines: the widest line traced as a line. Default 1.5." },
                    "detail": { "type": "number", "description": "0–100, how closely outlines follow the pixels. Default 60." },
                    "speckle_mm": { "type": "number", "description": "Drop areas smaller than this across. Default 1." },
                    "replace": { "type": "boolean", "description": "Start a new document with only the trace. Default false." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "round_corners",
            "description": "Round (or chamfer) the sharp corners of objects' outlines. Each corner rounds as far as its sides allow, so neighbouring cuts never overlap. Satin and fill stitch more cleanly round sharp corners.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "objects": { "type": "array", "items": obj_schema(), "description": "Objects to round." },
                    "radius_mm": { "type": "number", "description": "Corner radius. Default 1.5." },
                    "kind": { "type": "string", "enum": ["round", "inverted", "chamfer"], "description": "Default round." }
                },
                "required": ["objects"]
            }
        }),
        json!({
            "name": "simplify",
            "description": "Simplify objects' outlines: fewer points, smooth curves (Illustrator's and VectorCraft's Simplify). Fewer points stitch more smoothly, especially traced artwork. Corners stay corners.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "objects": { "type": "array", "items": obj_schema(), "description": "Objects to simplify." },
                    "level": { "type": "number", "description": "0–100: 0 for the fewest points, 100 to stay closest to the original (each 10 halves how far the outline may move; 50 is 1 pt). Default: Auto, the level for the objects' size." }
                },
                "required": ["objects"]
            }
        }),
        json!({
            "name": "clean_up",
            "description": "Clean up the whole document (Object > Path > Clean Up): remove stray points (objects with one point or no size), objects too small to sew, and extra points (points on top of each other, points in the middle of straight lines). Lettering is left alone. Says what it removed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "stray_points": { "type": "boolean", "description": "Default true." },
                    "tiny_objects": { "type": "boolean", "description": "Default true." },
                    "tiny_mm": { "type": "number", "description": "With tiny_objects: remove objects smaller than this across. Default 1." },
                    "extra_points": { "type": "boolean", "description": "Default true." },
                    "dry_run": { "type": "boolean", "description": "Only count what it would remove. Default false." }
                }
            }
        }),
        json!({
            "name": "batch",
            "description": "Run several tool calls in one go, in order. Atomic by default: if any step fails nothing changes, and the whole batch is one undo step. A string argument \"$N.field\" (e.g. \"$0.added.0.name\") is replaced by that field of step N's JSON result; write \"$$\" for a literal \"$\". open_file and undo can't run in an atomic batch. The stitches are planned once, at the end (and before any step that reads them: get_design, render_preview, check_design, export), so stitch counts in other steps' results are from before the batch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "steps": {
                        "type": "array",
                        "maxItems": 64,
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": { "type": "string" },
                                "arguments": { "type": "object" }
                            },
                            "required": ["tool"]
                        }
                    },
                    "atomic": { "type": "boolean", "description": "Roll everything back if a step fails, and undo as one step. Default true." },
                    "label": { "type": "string", "description": "What the batch did, shown to the person." }
                },
                "required": ["steps"]
            }
        }),
    ];
    // Hints for clients: which tools only read, and which can lose work.
    for t in &mut tools {
        let name = t["name"].as_str().unwrap_or("").to_string();
        let read_only = matches!(
            name.as_str(),
            "get_design"
                | "list_fonts"
                | "render_preview"
                | "check_design"
                | "get_svg"
                | "list_documents"
        );
        let destructive = matches!(
            name.as_str(),
            "delete_object" | "open_file" | "export" | "save"
        );
        if t.get("annotations").is_none() {
            t["annotations"] = if read_only {
                json!({ "readOnlyHint": true })
            } else {
                json!({ "readOnlyHint": false, "destructiveHint": destructive })
            };
        }
    }
    tools.push(json!({
        "name": "send_to_skitch",
        "description": "Send the design to the connected Brother Skitch (the studio's Skitch window connects; ask the person to connect there first if machine_status says it isn't). The window then walks the person through tracing the outline and sewing. Check the design and get the person's go-ahead first.",
        "inputSchema": { "type": "object", "properties": {} },
        "annotations": { "destructiveHint": false, "openWorldHint": true }
    }));
    tools.push(json!({
        "name": "machine_status",
        "description": "How the Skitch is doing: connected or not, what it's doing, any error and what to do about it, which thread is on and which comes next, and how far through the design it is.",
        "inputSchema": { "type": "object", "properties": {} },
        "annotations": { "readOnlyHint": true }
    }));
    Value::Array(tools)
}

/// A tool result's text.
fn text_of(r: &ToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.as_str()),
            Content::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Refuse arguments a tool doesn't take (a misspelt `widht_mm` would
/// otherwise be ignored without a word).
fn check_args(name: &str, args: &Value) -> Result<(), String> {
    static SPECS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let specs = SPECS.get_or_init(tool_specs);
    let Some(spec) = specs
        .as_array()
        .and_then(|a| a.iter().find(|t| t["name"] == name))
    else {
        return Ok(()); // unknown tools are reported by run_tool
    };
    let Some(obj) = args.as_object() else {
        return if args.is_null() {
            Ok(())
        } else {
            Err(format!("{name}: arguments must be an object"))
        };
    };
    let props = spec["inputSchema"]["properties"].as_object();
    let known = |k: &str| props.is_some_and(|p| p.contains_key(k));
    let unknown: Vec<&String> = obj.keys().filter(|k| !known(k)).collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let mut expected: Vec<&String> = props.map(|p| p.keys().collect()).unwrap_or_default();
    expected.sort();
    Err(format!(
        "{name} doesn't take {}. It takes: {}.",
        unknown
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", "),
        if expected.is_empty() {
            "no arguments".to_string()
        } else {
            expected
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ))
}

/// Replace "$N.path" strings in `v` by that field of step N's result.
fn resolve_refs(v: &Value, results: &[Value]) -> Result<Value, String> {
    Ok(match v {
        // "$$…" is a literal "$…".
        Value::String(s) if s.starts_with("$$") => Value::String(s[1..].to_string()),
        Value::String(s) if s.starts_with('$') => {
            let body = &s[1..];
            let (step, path) = body.split_once('.').unwrap_or((body, ""));
            let Ok(n) = step.parse::<usize>() else {
                return Ok(v.clone());
            };
            let mut cur = results
                .get(n)
                .ok_or(format!("{s}: there is no earlier step {n}"))?;
            for key in path.split('.').filter(|k| !k.is_empty()) {
                cur = match key.parse::<usize>() {
                    Ok(i) if cur.is_array() => cur.get(i),
                    _ => cur.get(key),
                }
                .ok_or(format!("{s}: step {n}'s result has no `{key}`"))?;
            }
            cur.clone()
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| resolve_refs(x, results))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| Ok((k.clone(), resolve_refs(x, results)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn mm(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn issue_json(i: &Issue, names: &[String]) -> Value {
    json!({
        "level": match i.level { Level::Error => "error", Level::Warning => "warning", Level::Info => "note" },
        "object": i.shape.and_then(|s| names.get(s).cloned()),
        "index": i.shape,
        "message": i.message,
    })
}

impl Studio {
    /// Resolve an object reference: index, exact name, or a unique partial name.
    pub(crate) fn resolve(&self, v: Option<&Value>) -> Result<usize, String> {
        let n = self.doc.shapes.len();
        match v {
            Some(Value::Number(x)) => {
                let i = x
                    .as_u64()
                    .ok_or("object index must be a non-negative integer")?
                    as usize;
                if i < n {
                    Ok(i)
                } else {
                    Err(format!(
                        "no object {i}; there are {n} (0..{})",
                        n.saturating_sub(1)
                    ))
                }
            }
            Some(Value::String(s)) => {
                let lower = s.to_lowercase();
                if let Some(i) = self.names.iter().position(|x| x.to_lowercase() == lower) {
                    return Ok(i);
                }
                if let Some(i) = self.doc.shapes.iter().position(|x| x.id == *s) {
                    return Ok(i);
                }
                let hits: Vec<usize> = self
                    .names
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| x.to_lowercase().contains(&lower))
                    .map(|(i, _)| i)
                    .collect();
                match hits.as_slice() {
                    [i] => Ok(*i),
                    [] => Err(format!(
                        "no object named '{s}'. Objects: {}",
                        self.names.join(", ")
                    )),
                    _ => Err(format!(
                        "'{s}' matches several objects: {}",
                        hits.iter()
                            .map(|&i| self.names[i].clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                }
            }
            _ => Err("`object` must be an index or a name".into()),
        }
    }

    fn object_json(&self, i: usize) -> Value {
        let s = &self.doc.shapes[i];
        let f = self.doc.settings.fabric();
        let p = &s.params;
        let kind = p.kind.unwrap_or(Kind::Fill);
        let b = shape_bounds(s);
        let mut settings = serde_json::Map::new();
        match kind {
            Kind::Fill => {
                settings.insert("angle".into(), json!(p.fill_angle()));
                settings.insert("spacing_mm".into(), json!(p.fill_spacing(&f)));
                settings.insert("length_mm".into(), json!(p.fill_len(&f)));
                settings.insert("pull_mm".into(), json!(p.fill_pull(&f)));
                settings.insert("underlay".into(), json!(p.underlay_on()));
            }
            Kind::Satin => {
                let w = crate::app::mean_satin_width(s);
                let ul = p.satin_underlay_for(w);
                let names: Vec<&str> = [
                    (underlay::CENTER, "center"),
                    (underlay::CONTOUR, "contour"),
                    (underlay::ZIGZAG, "zigzag"),
                ]
                .iter()
                .filter(|(b, _)| ul & b != 0)
                .map(|(_, n)| *n)
                .collect();
                settings.insert("mean_width_mm".into(), json!(mm(w)));
                if matches!(s.geometry, Geometry::Fill { .. }) {
                    settings.insert("border".into(), json!(p.is_border()));
                }
                settings.insert("spacing_mm".into(), json!(p.satin_spacing(&f)));
                settings.insert("pull_mm".into(), json!(p.satin_pull(&f)));
                settings.insert("satin_underlay".into(), json!(names));
                settings.insert(
                    "satin_underlay_auto".into(),
                    json!(p.satin_underlay.is_none()),
                );
                settings.insert("short_stitches".into(), json!(p.short_stitches_on()));
                settings.insert("split_mm".into(), json!(p.satin_split().unwrap_or(0.0)));
            }
            Kind::Run => {
                settings.insert("length_mm".into(), json!(p.run_len(&f)));
                settings.insert("repeats".into(), json!(p.run_repeats()));
            }
            Kind::Contour => {
                settings.insert("spacing_mm".into(), json!(p.contour_spacing()));
                settings.insert("length_mm".into(), json!(p.run_len(&f)));
                settings.insert("spiral".into(), json!(p.spiral == Some(true)));
            }
            Kind::Blanket => {
                settings.insert(
                    "spacing_mm".into(),
                    json!(
                        p.spacing
                            .unwrap_or(zigzag::params::defaults::BLANKET_SPACING)
                    ),
                );
                settings.insert(
                    "width_mm".into(),
                    json!(p.width.unwrap_or(zigzag::params::defaults::BLANKET_WIDTH)),
                );
            }
            Kind::Applique => {
                settings.insert(
                    "width_mm".into(),
                    json!(p.width.unwrap_or(zigzag::params::defaults::APPLIQUE_WIDTH)),
                );
                settings.insert("spacing_mm".into(), json!(p.satin_spacing(&f)));
                settings.insert(
                    "steps".into(),
                    json!([
                        "placement line",
                        "stop: lay fabric",
                        "tack-down",
                        "stop: trim fabric",
                        "satin cover"
                    ]),
                );
            }
            Kind::Meander => {
                settings.insert("spacing_mm".into(), json!(p.meander_spacing()));
            }
            Kind::Ripple => {
                settings.insert("spacing_mm".into(), json!(p.ripple_spacing()));
                settings.insert("spiral".into(), json!(p.spiral == Some(true)));
            }
            Kind::Cross => {
                settings.insert("cell_mm".into(), json!(p.cross_cell()));
            }
            Kind::Tartan => {
                settings.insert(
                    "sett".into(),
                    json!(
                        p.tartan_sett(s.color)
                            .iter()
                            .map(|(c, w)| json!({ "color": hex(*c), "width_mm": w }))
                            .collect::<Vec<_>>()
                    ),
                );
            }
            Kind::Skip | Kind::Manual => {}
        }
        if kind != Kind::Skip {
            for (k, v) in [("start", p.start), ("end", p.end)] {
                if let Some(t) = v {
                    settings.insert(k.into(), json!(t));
                }
            }
            for (k, v) in [("trim", p.trim), ("stop", p.stop)] {
                if v == Some(true) {
                    settings.insert(k.into(), json!(true));
                }
            }
        }
        json!({
            "index": i,
            "name": self.names.get(i),
            "kind": kind.name(),
            "allowed_kinds": allowed_kinds(s).iter().map(|k| k.name()).collect::<Vec<_>>(),
            "lettering": self.doc.lettering(i).map(|t| json!({ "text": t.text, "font": t.font, "size_mm": t.size_mm, "spacing_mm": t.spacing_mm, "arc_mm": t.arc_mm, "bold": t.bold, "italic": t.italic, "leading": t.leading, "align": t.align.name(), "h_scale": t.h_scale, "v_scale": t.v_scale, "case": t.case.name(), "on_path": t.path.is_some(), "path_offset": t.path_offset, "path_flip": t.path_flip, "kern": t.kern.iter().map(|(k, v)| json!({ "at": k, "mm": v })).collect::<Vec<_>>() })),
            "source": match (s.part, &s.geometry) { (Part::Stroke, _) => "stroke", (_, Geometry::Fill { .. }) => "filled shape", _ => "stroke" },
            "color": hex(s.color),
            "center_mm": [mm(b.center().x / 10.0), mm(b.center().y / 10.0)],
            "size_mm": [mm(b.width() / 10.0), mm(b.height() / 10.0)],
            "stitches": self.design.per_shape.get(i).copied().unwrap_or(0),
            "settings": settings,
            "problems": self.issues.iter().filter(|x| x.shape == Some(i)).map(|x| x.message.clone()).collect::<Vec<_>>(),
        })
    }

    /// The open documents, for list_documents.
    fn documents_json(&self) -> Value {
        let front = self.active_tab();
        json!({
            "documents": self.tab_infos().iter().enumerate().map(|(i, t)| json!({
                "index": i,
                "name": t.name,
                "file": t.path.as_ref().map(|p| p.display().to_string()),
                "unsaved_changes": t.modified,
                "in_front": i == front,
            })).collect::<Vec<_>>(),
        })
    }

    fn design_json(&self) -> Value {
        let b = self.design.bounds();
        let total = self.design.stitch_count();
        let (hw, hh) = self.hoop_dims();
        let fabric = self.doc.settings.fabric();
        json!({
            "coordinates": "millimetres; origin = hoop center; +x right, +y down",
            "document": {
                "name": self.doc.name,
                "file": self.doc.path.as_ref().map(|p| p.display().to_string()),
                "unsaved_changes": self.modified,
                "fabric": fabric.id,
                "fabric_name": fabric.name,
                "hoop": { "id": HOOPS[self.doc.settings.hoop.min(2)].0, "width_mm": hw, "height_mm": hh },
                "remove_overlaps": self.doc.settings.remove_overlaps,
                "hat_order": self.doc.settings.hat_order,
                "preview_color": hex(self.doc.settings.cloth),
            },
            "stats": {
                "width_mm": mm(b.width() / 10.0),
                "height_mm": mm(b.height() / 10.0),
                "stitches": total,
                "colors": self.design.blocks.len(),
                "trims": self.design.trim_count(),
                "minutes": (total as f64 / SPM).ceil(),
            },
            "objects": (0..self.doc.shapes.len()).map(|i| self.object_json(i)).collect::<Vec<_>>(),
            "design_problems": self.issues.iter().filter(|x| x.shape.is_none()).map(|x| issue_json(x, &self.names)).collect::<Vec<_>>(),
        })
    }

    /// Run one MCP tool against the live document.
    pub fn run_tool(&mut self, name: &str, args: &Value) -> ToolResult {
        if let Err(e) = check_args(name, args) {
            return ToolResult::error(e);
        }
        // Inside a batch, only a step that reads the stitches waits for them.
        let reads = matches!(
            name,
            "get_design" | "render_preview" | "check_design" | "export" | "send_to_skitch"
        );
        if reads {
            self.plan_now();
        }
        if (reads || !self.batching) && self.dirty_needs_plan() {
            self.replan();
        }
        let result = match name {
            "batch" => return self.tool_batch(args),
            "trace_image" => self.tool_trace_image(args),
            "round_corners" => self.tool_round_corners(args),
            "simplify" => self.tool_simplify(args),
            "clean_up" => self.tool_clean_up(args),
            "get_design" => Ok(ToolResult::json(&self.design_json())),
            "add_svg" => self.tool_add_svg(args),
            "set_object" => self.tool_set_object(args),
            "delete_object" => self.resolve(args.get("object")).and_then(|i| {
                let name = self.names[i].clone();
                self.structural(|d| d.delete(i))
                    .map_err(|e| e.to_string())?;
                self.selected = self
                    .selected
                    .filter(|&u| self.doc.shapes.iter().any(|s| s.uid == u));
                Ok(ToolResult::text(format!("Deleted {name}.")))
            }),
            "move_object" => self.resolve(args.get("object")).and_then(|i| {
                let to =
                    args.get("to")
                        .and_then(Value::as_u64)
                        .ok_or("`to` must be a non-negative integer")? as usize;
                let to = to.min(self.doc.shapes.len() - 1);
                self.structural(|d| {
                    let s = d.shapes.remove(i);
                    d.shapes.insert(to, s);
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
                self.replan_soon();
                Ok(ToolResult::text(format!(
                    "Sewing order: {}",
                    self.names.join(" → ")
                )))
            }),
            "set_document" => self.tool_set_document(args),
            "transform_objects" => self.tool_transform(args),
            "add_text" => {
                let mut spec = zigzag::document::TextSpec::default();
                apply_text_args(&mut spec, args).and_then(|_| {
                    let mut x = args.get("x_mm").and_then(Value::as_f64).unwrap_or(0.0);
                    let mut y = args.get("y_mm").and_then(Value::as_f64).unwrap_or(0.0);
                    let on = self.path_for_text(args.get("on_path"))?;
                    if let Some((src, rel, c)) = on {
                        spec.path = Some(rel);
                        (x, y) = (c.x / 10.0, c.y / 10.0);
                        self.doc.shapes[src].params.kind = Some(Kind::Skip);
                    }
                    let i = self
                        .add_lettering(&spec, (x, y))
                        .map_err(|e| format!("{e:#}"))?;
                    self.replan_soon();
                    self.fit_view();
                    Ok(ToolResult::json(&self.object_json(i)))
                })
            }
            "edit_text" => self.resolve(args.get("object")).and_then(|i| {
                let mut spec = self.doc.lettering(i).ok_or(format!(
                    "{} isn't lettering (only text added with add_text can be edited)",
                    self.names[i]
                ))?;
                apply_text_args(&mut spec, args)?;
                if args.get("on_path").is_some_and(Value::is_null) {
                    spec.path = None;
                } else if let Some((src, rel, _)) = self.path_for_text(args.get("on_path"))? {
                    spec.path = Some(rel);
                    self.doc.shapes[src].params.kind = Some(Kind::Skip);
                }
                let uid = self.doc.shapes[i].uid;
                self.update_lettering(i, &spec)
                    .map_err(|e| format!("{e:#}"))?;
                self.replan_soon();
                let i = self
                    .doc
                    .shapes
                    .iter()
                    .position(|s| s.uid == uid)
                    .unwrap_or(i);
                Ok(ToolResult::json(&self.object_json(i)))
            }),
            "open_file" => match args.get("path").and_then(Value::as_str).map(PathBuf::from) {
                Some(p) if p.is_absolute() => {
                    let doc = Document::open(&p).map_err(|e| format!("{e:#}"));
                    doc.map(|d| {
                        self.open_document(d);
                        self.replan_soon();
                        ToolResult::json(&self.design_json())
                    })
                }
                _ => Err("path must be absolute".into()),
            },
            "list_documents" => Ok(ToolResult::json(&self.documents_json())),
            "switch_document" => {
                let infos = self.tab_infos();
                let i = match (
                    args.get("index").and_then(Value::as_u64),
                    args.get("name").and_then(Value::as_str),
                ) {
                    (Some(i), _) => Some(i as usize).filter(|&i| i < infos.len()),
                    (None, Some(n)) => infos.iter().position(|t| t.name == n),
                    _ => return ToolResult::error("give index or name"),
                };
                match i {
                    Some(i) => {
                        self.switch_tab(i);
                        self.replan_soon();
                        Ok(ToolResult::json(&self.documents_json()))
                    }
                    None => Err(format!(
                        "no such document; open: {}",
                        infos
                            .iter()
                            .map(|t| t.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                }
            }
            "new_document" => {
                self.new_tab();
                self.replan_soon();
                Ok(ToolResult::json(&self.documents_json()))
            }
            "group_colors" => {
                self.group_colors();
                Ok(ToolResult::text(format!(
                    "{} Sewing order: {}",
                    self.status,
                    self.names.join(" → ")
                )))
            }
            "list_fonts" => {
                let q = args
                    .get("search")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_lowercase();
                let fonts: Vec<String> = zigzag::document::font_families()
                    .into_iter()
                    .filter(|f| f.to_lowercase().contains(&q))
                    .collect();
                Ok(ToolResult::json(
                    &json!({ "default": zigzag::document::default_font(), "fonts": fonts }),
                ))
            }
            "duplicate_objects" => self.tool_duplicate(args),
            "render_preview" => self.tool_render(args),
            "check_design" => {
                let v: Vec<Value> = self
                    .issues
                    .iter()
                    .map(|x| issue_json(x, &self.names))
                    .collect();
                Ok(if v.is_empty() {
                    ToolResult::text("No problems found.")
                } else {
                    ToolResult::json(&json!(v))
                })
            }
            "get_svg" => self
                .doc
                .to_svg()
                .map(ToolResult::text)
                .map_err(|e| format!("{e:#}")),
            "select" => match args.get("object") {
                None | Some(Value::Null) => {
                    self.selected = None;
                    Ok(ToolResult::text("Selection cleared."))
                }
                v => self.resolve(v).map(|i| {
                    self.select_index(Some(i));
                    ToolResult::text(format!("Selected {}.", self.names[i]))
                }),
            },
            "undo" => {
                if self.apply_history(crate::app::History::Undo) {
                    Ok(ToolResult::text("Undone."))
                } else {
                    Ok(ToolResult::text("Nothing to undo."))
                }
            }
            "save" => {
                let path = args.get("path").and_then(Value::as_str).map(PathBuf::from);
                if let Some(p) = &path
                    && !p.is_absolute()
                {
                    return ToolResult::error("path must be absolute");
                }
                if path.is_none() && self.doc.path.is_none() {
                    return ToolResult::error(
                        "this document has never been saved; pass an absolute path ending in .svg",
                    );
                }
                self.save(path.as_deref());
                if self.modified {
                    Err(self.status.clone())
                } else {
                    Ok(ToolResult::text(self.status.clone()))
                }
            }
            "export" => {
                let fmt = args.get("format").and_then(Value::as_str).unwrap_or("");
                match args.get("path").and_then(Value::as_str).map(PathBuf::from) {
                    Some(p) if p.is_absolute() => self
                        .export_to(fmt, &p)
                        .map(|_| {
                            ToolResult::text(format!(
                                "Wrote {} ({} stitches).",
                                p.display(),
                                self.design.stitch_count()
                            ))
                        })
                        .map_err(|e| format!("{e:#}")),
                    _ => Err("path must be absolute".into()),
                }
            }
            "send_to_skitch" => self.tool_send_to_skitch(),
            "machine_status" => Ok(ToolResult::json(&self.machine_json())),
            other => Err(format!("unknown tool '{other}'")),
        };
        if !self.batching && self.dirty_needs_plan() {
            self.replan();
        } else if self.batching {
            // Later steps name objects as they are now (names are cheap).
            self.names = zigzag::svg::display_names(&self.doc.shapes);
        }
        result.unwrap_or_else(ToolResult::error)
    }

    /// Several tool calls in order; atomic: all or nothing, one undo step.
    fn tool_batch(&mut self, args: &Value) -> ToolResult {
        let was = std::mem::replace(&mut self.batching, true);
        let r = self.tool_batch_steps(args);
        self.batching = was;
        if !was && self.undo.len() > crate::app::UNDO_STEPS {
            let extra = self.undo.len() - crate::app::UNDO_STEPS;
            self.undo.drain(..extra);
        }
        if !was && (self.dirty || self.dirty_needs_plan()) {
            self.replan();
        }
        r
    }

    fn tool_batch_steps(&mut self, args: &Value) -> ToolResult {
        let Some(steps) = args.get("steps").and_then(Value::as_array) else {
            return ToolResult::error("`steps` must be a list of {tool, arguments}");
        };
        if steps.len() > 64 {
            return ToolResult::error("at most 64 steps in a batch");
        }
        let atomic = args.get("atomic").and_then(Value::as_bool).unwrap_or(true);
        let (undo0, doc0, redo0, sel0, modified0) = (
            self.undo.len(),
            self.doc.clone(),
            self.redo.clone(),
            self.selected,
            self.modified,
        );
        let mut results: Vec<Value> = Vec::new();
        let mut report = Vec::new();
        let mut images = Vec::new();
        for (k, step) in steps.iter().enumerate() {
            let tool = step.get("tool").and_then(Value::as_str).unwrap_or("");
            let failed = |msg: String| {
                if atomic {
                    format!(
                        "Step {k} ({tool}) failed: {msg} Nothing was changed: the batch is all or nothing."
                    )
                } else {
                    format!("Step {k} ({tool}) failed: {msg} Steps before it were kept.")
                }
            };
            // Opening a file clears the history, so it couldn't be rolled
            // back; nor could a step in another document (the batch's undo
            // step and its rollback belong to the one it began in).
            let outcome = if tool == "batch"
                || (atomic
                    && matches!(
                        tool,
                        "undo" | "open_file" | "switch_document" | "new_document"
                    )) {
                Err(format!("`{tool}` can't run inside an atomic batch."))
            } else {
                resolve_refs(step.get("arguments").unwrap_or(&json!({})), &results)
                    .map(|a| self.run_tool(tool, &a))
            };
            let r = outcome.unwrap_or_else(ToolResult::error);
            if r.is_error {
                let msg = failed(text_of(&r));
                if atomic {
                    self.doc = doc0;
                    self.undo.truncate(undo0);
                    self.redo = redo0;
                    self.stable = self.doc.clone();
                    self.selected = sel0;
                    self.modified = modified0;
                    self.dirty = true;
                    self.replan();
                }
                let mut r = ToolResult::json(&json!({ "error": msg, "results": report }));
                r.is_error = true;
                return r;
            }
            let text = text_of(&r);
            let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            images.extend(
                r.content
                    .into_iter()
                    .filter(|c| matches!(c, Content::Image(_))),
            );
            report.push(json!({ "step": k, "tool": tool, "result": value.clone() }));
            results.push(value);
        }
        // One undo step for the lot.
        if atomic && self.undo.len() > undo0 + 1 {
            self.undo.truncate(undo0 + 1);
        }
        if let Some(label) = args.get("label").and_then(Value::as_str) {
            self.status = label.to_string();
        }
        let mut out = ToolResult::json(&json!({ "results": report }));
        out.content.extend(images);
        out
    }

    fn tool_trace_image(&mut self, args: &Value) -> Result<ToolResult, String> {
        let path = match args.get("path").and_then(Value::as_str).map(PathBuf::from) {
            Some(p) if p.is_absolute() => p,
            _ => return Err("path must be absolute".into()),
        };
        let bytes =
            std::fs::read(&path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
        let num = |k: &str| {
            args.get(k)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
        };
        let (hw, hh) = self.hoop_dims();
        let image = zigzag::trace::Image::decode(&bytes).map_err(|e| format!("{e:#}"))?;
        let (w, h) = image.size();
        let aspect = h as f64 / w.max(1) as f64;
        let mut o = zigzag::trace::TraceOptions {
            width_mm: num("width_mm").unwrap_or_else(|| (hw * 0.8).min(hh * 0.8 / aspect)),
            ..Default::default()
        };
        if let Some(n) = args.get("colors").and_then(Value::as_u64) {
            if !(2..=10).contains(&n) {
                return Err("colors must be 2–10 (or leave it out to find them)".into());
            }
            o.colors = n as u32;
        }
        if let Some(b) = args.get("remove_background").and_then(Value::as_bool) {
            o.remove_background = b;
        }
        if let Some(b) = args.get("lines").and_then(Value::as_bool) {
            o.lines = b;
        }
        if let Some(v) = num("line_width_mm") {
            o.line_width_mm = v.clamp(0.3, 5.0);
        }
        if let Some(v) = num("detail") {
            o.detail = v.clamp(0.0, 100.0);
        }
        if let Some(v) = num("speckle_mm") {
            o.speckle_mm = v.clamp(0.0, 10.0);
        }
        o.threads = match args.get("threads").and_then(Value::as_str) {
            None | Some("none") => Vec::new(),
            Some("brother") => crate::threads::chart(crate::threads::Brand::Brother)
                .into_iter()
                .map(|t| t.0)
                .collect(),
            Some("janome") => crate::threads::chart(crate::threads::Brand::Janome)
                .into_iter()
                .map(|t| t.0)
                .collect(),
            Some(other) => {
                return Err(format!(
                    "threads must be none, brother or janome, not {other}"
                ));
            }
        };
        let t = zigzag::trace::trace(&image, &o).map_err(|e| format!("{e:#}"))?;
        let replace = args
            .get("replace")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let place = Placement {
            center_mm: Some((num("x_mm").unwrap_or(0.0), num("y_mm").unwrap_or(0.0))),
            width_mm: None,
            height_mm: None,
        };
        let mut added = Vec::new();
        self.structural(|d| {
            if replace {
                *d = Document::empty();
            }
            added = d.add_svg(&t.svg, place)?;
            Ok(())
        })
        .map_err(|e| format!("{e:#}"))?;
        if replace {
            self.selected = None;
        }
        self.replan_soon();
        self.fit_view();
        let objs: Vec<Value> = added.iter().map(|&i| self.object_json(i)).collect();
        Ok(ToolResult::json(&json!({
            "traced_as": t.method,
            "colors": t.colors.iter().map(|&c| hex(c)).collect::<Vec<_>>(),
            "added": objs,
            "total_objects": self.doc.shapes.len(),
        })))
    }

    fn tool_simplify(&mut self, args: &Value) -> Result<ToolResult, String> {
        use zigzag::pathtools;
        let list = args
            .get("objects")
            .and_then(Value::as_array)
            .ok_or("`objects` must be a list")?;
        let idx: Vec<usize> = list
            .iter()
            .map(|v| self.resolve(Some(v)))
            .collect::<Result<_, _>>()?;
        let level = match args.get("level") {
            None | Some(Value::Null) => {
                let mut b = zigzag::geom::Bounds::empty();
                for &i in &idx {
                    if let Ok(p) = self.doc.edit_path(i) {
                        let pb = zigzag::bezier::bounds(&p);
                        if !pb.is_empty() {
                            b.add(pb.min);
                            b.add(pb.max);
                        }
                    }
                }
                let size = if b.is_empty() {
                    0.0
                } else {
                    b.width().max(b.height())
                };
                pathtools::auto_simplify_level(size / pathtools::PT)
            }
            Some(v) => v
                .as_f64()
                .filter(|l| (0.0..=100.0).contains(l))
                .ok_or("level must be a number from 0 to 100")?,
        };
        let tol = pathtools::simplify_level_tolerance(level) * pathtools::PT;
        let mut removed = 0;
        let redo = self.redo.clone();
        self.structural(|d| {
            removed = d.simplify_paths(&idx, tol)?;
            Ok(())
        })
        .map_err(|e| format!("{e:#}"))?;
        if removed == 0 {
            self.undo.pop();
            self.redo = redo;
            return Ok(ToolResult::text(format!(
                "Already as simple as level {level:.0} gets."
            )));
        }
        self.replan_soon();
        Ok(ToolResult::text(format!(
            "Simplified at level {level:.0}: {removed} fewer points."
        )))
    }

    fn tool_clean_up(&mut self, args: &Value) -> Result<ToolResult, String> {
        let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(true);
        let tiny = args.get("tiny_mm").and_then(Value::as_f64).unwrap_or(1.0);
        if !(tiny.is_finite() && tiny > 0.0) {
            return Err("tiny_mm must be a positive number".into());
        }
        let opt = zigzag::document::CleanUp {
            stray: flag("stray_points"),
            specks: flag("tiny_objects"),
            speck: tiny.min(100.0) * 10.0,
            points: flag("extra_points"),
        };
        let dry = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if dry {
            let mut doc = self.doc.clone();
            let r = doc.clean_up(&opt, true).map_err(|e| format!("{e:#}"))?;
            return Ok(ToolResult::text(
                crate::path_tools::cleanup_summary(&r).replacen("Removed", "Would remove", 1),
            ));
        }
        let mut r = zigzag::document::CleanUpReport::default();
        let redo = self.redo.clone();
        self.structural(|d| {
            r = d.clean_up(&opt, false)?;
            Ok(())
        })
        .map_err(|e| format!("{e:#}"))?;
        if r.total() == 0 {
            self.undo.pop();
            self.redo = redo;
        } else {
            self.replan_soon();
        }
        Ok(ToolResult::text(crate::path_tools::cleanup_summary(&r)))
    }

    fn tool_round_corners(&mut self, args: &Value) -> Result<ToolResult, String> {
        let list = args
            .get("objects")
            .and_then(Value::as_array)
            .ok_or("`objects` must be a list")?;
        let idx: Vec<usize> = list
            .iter()
            .map(|v| self.resolve(Some(v)))
            .collect::<Result<_, _>>()?;
        let r = args.get("radius_mm").and_then(Value::as_f64).unwrap_or(1.5);
        if !(r.is_finite() && r > 0.0) {
            return Err("radius_mm must be a positive number".into());
        }
        let kind = match args.get("kind").and_then(Value::as_str) {
            None | Some("round") => zigzag::bezier::CornerKind::Round,
            Some("inverted") => zigzag::bezier::CornerKind::Inverted,
            Some("chamfer") => zigzag::bezier::CornerKind::Chamfer,
            Some(k) => return Err(format!("kind must be round, inverted or chamfer, not {k}")),
        };
        let mut n = 0;
        let redo = self.redo.clone();
        self.structural(|d| {
            n = d.round_corners(&idx, r.min(50.0) * 10.0, kind, &[])?;
            Ok(())
        })
        .map_err(|e| format!("{e:#}"))?;
        if n == 0 {
            // Nothing changed: no undo step, and redo as it was.
            self.undo.pop();
            self.redo = redo;
            return Ok(ToolResult::text(
                "No sharp corners to round on those objects.",
            ));
        }
        self.replan_soon();
        Ok(ToolResult::text(format!("Rounded {n} corners.")))
    }

    /// Re-plan after an edit now, or (in a batch) once it's done.
    pub(crate) fn replan_soon(&mut self) {
        if self.batching {
            self.dirty = true;
            // Names are cheap, and later steps refer to objects by them.
            self.names = zigzag::svg::display_names(&self.doc.shapes);
        } else {
            self.replan();
        }
    }

    pub(crate) fn dirty_needs_plan(&self) -> bool {
        self.design.per_shape.len() != self.doc.shapes.len()
            || self.names.len() != self.doc.shapes.len()
    }

    fn tool_add_svg(&mut self, args: &Value) -> Result<ToolResult, String> {
        let svg = args
            .get("svg")
            .and_then(Value::as_str)
            .ok_or("`svg` is required")?;
        let num = |k: &str| args.get(k).and_then(Value::as_f64);
        let place = Placement {
            center_mm: Some((num("x_mm").unwrap_or(0.0), num("y_mm").unwrap_or(0.0))),
            width_mm: num("width_mm").filter(|v| *v > 0.0),
            height_mm: num("height_mm").filter(|v| *v > 0.0),
        };
        let replace = args
            .get("replace")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut added = Vec::new();
        self.structural(|d| {
            if replace {
                *d = Document::empty();
            }
            added = d.add_svg(svg, place)?;
            Ok(())
        })
        .map_err(|e| format!("{e:#}"))?;
        if replace {
            self.selected = None;
        }
        self.replan_soon();
        self.fit_view();
        let objs: Vec<Value> = added.iter().map(|&i| self.object_json(i)).collect();
        Ok(ToolResult::json(&json!({
            "added": objs,
            "total_objects": self.doc.shapes.len(),
            "design_problems": self.issues.iter().filter(|x| x.shape.is_none()).map(|x| issue_json(x, &self.names)).collect::<Vec<_>>(),
        })))
    }

    /// `on_path` for add_text / edit_text: the object, its outline relative
    /// to its center (the text's baseline), and that center (units).
    fn path_for_text(
        &mut self,
        v: Option<&Value>,
    ) -> Result<Option<(usize, zigzag::bezier::BezPath, P)>, String> {
        let Some(v) = v.filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let i = self.resolve(Some(v))?;
        let path = self
            .doc
            .edit_path(i)
            .map_err(|e| format!("{} can't be a baseline: {e:#}", self.names[i]))?;
        let b = zigzag::bezier::bounds(&path);
        let c = b.center();
        let rel = zigzag::bezier::transform(&path, zigzag::geom::translation(P::default() - c));
        Ok(Some((i, rel, c)))
    }

    fn tool_set_object(&mut self, args: &Value) -> Result<ToolResult, String> {
        let i = self.resolve(args.get("object"))?;
        let shape = &self.doc.shapes[i];
        let mut p: Params = shape.params.clone();
        let mut color = shape.color;
        let paint = shape.paint;
        let allowed = allowed_kinds(shape);
        let num = |k: &str| -> Result<Option<Option<f64>>, String> {
            match args.get(k) {
                None => Ok(None),
                Some(Value::Null) => Ok(Some(None)),
                Some(v) => v
                    .as_f64()
                    .filter(|x| x.is_finite())
                    .map(|x| Some(Some(x)))
                    .ok_or(format!("`{k}` must be a number")),
            }
        };
        let flag = |k: &str| -> Result<Option<Option<bool>>, String> {
            match args.get(k) {
                None => Ok(None),
                Some(Value::Null) => Ok(Some(None)),
                Some(v) => v
                    .as_bool()
                    .map(|x| Some(Some(x)))
                    .ok_or(format!("`{k}` must be true, false or null")),
            }
        };
        if let Some(k) = args.get("kind") {
            let k = k.as_str().and_then(Kind::parse).ok_or(
                "`kind` must be fill, satin, run, contour, blanket, applique, meander, ripple, cross, tartan, manual or skip",
            )?;
            if !allowed.contains(&k) {
                return Err(format!(
                    "{} can't be {} (it's a stroke; use {})",
                    self.names[i],
                    k.name(),
                    allowed
                        .iter()
                        .map(|k| k.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            p.kind = Some(k);
        }
        if let Some(v) = num("angle")? {
            p.angle = v.map(crate::widgets::normalize_angle);
        }
        if let Some(v) = num("spacing_mm")? {
            p.spacing = v.map(|x| x.clamp(0.1, 2.0));
        }
        if let Some(v) = num("length_mm")? {
            p.len = v.map(|x| x.clamp(0.5, 10.0));
        }
        if let Some(v) = num("pull_mm")? {
            p.pull = v.map(|x| x.clamp(0.0, 1.0));
        }
        if let Some(v) = num("width_mm")? {
            p.width = v.map(|x| x.clamp(0.3, 15.0));
        }
        if let Some(v) = flag("border")? {
            p.border = v;
        }
        if let Some(v) = flag("underlay")? {
            p.underlay = v;
        }
        if let Some(v) = flag("short_stitches")? {
            p.short_stitches = v;
        }
        if let Some(v) = num("split_mm")? {
            p.split = v.map(|x| x.max(0.0));
        }
        if let Some(v) = num("repeats")? {
            p.repeats = v.map(|x| x.clamp(1.0, 9.0) as u32);
        }
        if let Some(v) = num("start")? {
            p.start = v.map(|x| x.rem_euclid(1.0));
        }
        if let Some(v) = num("end")? {
            p.end = v.map(|x| x.rem_euclid(1.0));
        }
        if let Some(v) = flag("trim")? {
            p.trim = v.filter(|&x| x);
        }
        if let Some(v) = flag("spiral")? {
            p.spiral = v.filter(|&x| x);
        }
        if let Some(v) = flag("stop")? {
            p.stop = v.filter(|&x| x);
        }
        match args.get("satin_underlay") {
            None => {}
            Some(Value::String(s)) if s == "auto" => p.satin_underlay = None,
            Some(Value::Null) => p.satin_underlay = None,
            Some(Value::Array(a)) => {
                let mut m = 0;
                for x in a {
                    m |= x
                        .as_str()
                        .and_then(underlay::parse)
                        .ok_or("satin_underlay items must be center, contour or zigzag")?;
                }
                p.satin_underlay = Some(m);
                p.underlay = None;
            }
            Some(_) => return Err("satin_underlay must be a list or \"auto\"".into()),
        }
        match args.get("color") {
            None => {}
            Some(Value::Null) => color = paint,
            Some(Value::String(s)) => color = parse_hex(s).ok_or("color must look like #rrggbb")?,
            Some(_) => return Err("color must look like #rrggbb".into()),
        }
        self.structural(|d| {
            d.shapes[i].params = p;
            d.shapes[i].color = color;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
        self.replan_soon();
        Ok(ToolResult::json(&self.object_json(i)))
    }

    pub(crate) fn resolve_many(&self, v: Option<&Value>) -> Result<Vec<usize>, String> {
        match v {
            Some(Value::Array(a)) if !a.is_empty() => {
                let mut out = Vec::new();
                for x in a {
                    let i = self.resolve(Some(x))?;
                    if !out.contains(&i) {
                        out.push(i);
                    }
                }
                Ok(out)
            }
            Some(Value::Array(_)) => Err("`objects` is empty".into()),
            v => self.resolve(v).map(|i| vec![i]),
        }
    }

    fn pair(args: &Value, k: &str) -> Result<Option<(f64, f64)>, String> {
        match args.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(a)) if a.len() == 2 => match (a[0].as_f64(), a[1].as_f64()) {
                (Some(x), Some(y)) => Ok(Some((x, y))),
                _ => Err(format!("`{k}` must be [x, y] numbers")),
            },
            _ => Err(format!("`{k}` must be [x, y]")),
        }
    }

    fn tool_transform(&mut self, args: &Value) -> Result<ToolResult, String> {
        use zigzag::geom::{affine_mul, rotate_about, scale_about, translation};
        let idx = self.resolve_many(args.get("objects"))?;
        self.set_selection(&idx);
        let b = self.selection_bounds();
        if b.is_empty() {
            return Err("those objects have no size".into());
        }
        let c = b.center();
        let num = |k: &str| {
            args.get(k)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
        };
        let (mut sx, mut sy) = (1.0, 1.0);
        if let Some(k) = num("scale") {
            if k <= 0.0 {
                return Err("scale must be positive".into());
            }
            (sx, sy) = (k, k);
        }
        match (
            num("width_mm").filter(|v| *v > 0.0),
            num("height_mm").filter(|v| *v > 0.0),
        ) {
            (Some(w), Some(h)) => {
                (sx, sy) = (
                    w * 10.0 / b.width().max(1e-6),
                    h * 10.0 / b.height().max(1e-6),
                )
            }
            (Some(w), None) => {
                (sx, sy) = (
                    w * 10.0 / b.width().max(1e-6),
                    w * 10.0 / b.width().max(1e-6),
                )
            }
            (None, Some(h)) => {
                (sx, sy) = (
                    h * 10.0 / b.height().max(1e-6),
                    h * 10.0 / b.height().max(1e-6),
                )
            }
            _ => {}
        }
        let mut m = scale_about(c, sx, sy);
        match args.get("mirror").and_then(Value::as_str) {
            Some("horizontal") => m = affine_mul(scale_about(c, -1.0, 1.0), m),
            Some("vertical") => m = affine_mul(scale_about(c, 1.0, -1.0), m),
            Some(_) => return Err("mirror must be horizontal or vertical".into()),
            None => {}
        }
        if let Some(deg) = num("rotate_deg") {
            m = affine_mul(rotate_about(c, deg), m);
        }
        let shift = match (
            Self::pair(args, "move_to_mm")?,
            Self::pair(args, "move_by_mm")?,
        ) {
            (Some((x, y)), _) => P::new(x * 10.0, y * 10.0) - c,
            (None, Some((dx, dy))) => P::new(dx * 10.0, dy * 10.0),
            _ => P::default(),
        };
        m = affine_mul(translation(shift), m);
        if !self.transform_selected(m, "Transformed") && m != zigzag::geom::IDENTITY {
            return Err(self.status.clone());
        }
        self.replan_soon();
        let objs: Vec<Value> = self
            .sel_indices()
            .iter()
            .map(|&i| self.object_json(i))
            .collect();
        Ok(ToolResult::json(
            &json!({ "objects": objs, "design_problems": self.issues.iter().filter(|x| x.shape.is_none()).map(|x| issue_json(x, &self.names)).collect::<Vec<_>>() }),
        ))
    }

    fn tool_duplicate(&mut self, args: &Value) -> Result<ToolResult, String> {
        let idx = self.resolve_many(args.get("objects"))?;
        let (dx, dy) = Self::pair(args, "offset_mm")?.unwrap_or((3.0, 3.0));
        let mut new = Vec::new();
        self.structural(|d| {
            new = d.duplicate(&idx, P::new(dx * 10.0, dy * 10.0))?;
            Ok(())
        })
        .map_err(|e| format!("{e:#}"))?;
        self.replan_soon();
        self.set_selection(&new);
        Ok(ToolResult::json(
            &json!({ "added": new.iter().map(|&i| self.object_json(i)).collect::<Vec<_>>() }),
        ))
    }

    fn tool_set_document(&mut self, args: &Value) -> Result<ToolResult, String> {
        let mut s = self.doc.settings.clone();
        if let Some(f) = args.get("fabric") {
            let id = f.as_str().ok_or("fabric must be a string")?;
            zigzag::params::Fabric::by_id(id).ok_or(format!(
                "unknown fabric; use one of {}",
                FABRICS.iter().map(|f| f.id).collect::<Vec<_>>().join(", ")
            ))?;
            s.fabric = id.to_string();
        }
        if let Some(h) = args.get("hoop") {
            let id = h.as_str().ok_or("hoop must be a string")?;
            s.hoop = HOOPS.iter().position(|x| x.0 == id).ok_or(format!(
                "unknown hoop; use one of {}",
                HOOPS.iter().map(|h| h.0).collect::<Vec<_>>().join(", ")
            ))?;
        }
        if let Some(v) = args.get("remove_overlaps") {
            s.remove_overlaps = v.as_bool().ok_or("remove_overlaps must be true or false")?;
        }
        if let Some(v) = args.get("hat_order") {
            s.hat_order = v.as_bool().ok_or("hat_order must be true or false")?;
        }
        if let Some(v) = args.get("connect_mm") {
            s.connect_mm = v
                .as_f64()
                .filter(|x| x.is_finite() && *x >= 0.0)
                .ok_or("connect_mm must be a number of millimeters")?
                .min(50.0);
        }
        if let Some(v) = args.get("preview_color") {
            s.cloth = v
                .as_str()
                .and_then(parse_hex)
                .ok_or("preview_color must look like #rrggbb")?;
        }
        self.structural(|d| {
            d.settings = s;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
        self.replan_soon();
        Ok(ToolResult::json(&self.design_json()["document"]))
    }

    fn tool_render(&mut self, args: &Value) -> Result<ToolResult, String> {
        let width = args
            .get("width_px")
            .and_then(Value::as_u64)
            .unwrap_or(900)
            .clamp(200, 2000) as u32;
        let show_jumps = args
            .get("show_jumps")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let fit_hoop = args.get("fit").and_then(Value::as_str) == Some("hoop");
        let opt = PreviewOptions {
            hoop_mm: fit_hoop.then(|| self.hoop_dims()),
            background: self.doc.settings.cloth,
            show_jumps,
            thread_mm: 0.38,
        };
        let png = render_png(&self.design, &opt, width).map_err(|e| format!("{e:#}"))?;
        let b = self.design.bounds();
        let legend: Vec<String> = self
            .design
            .sew_order
            .iter()
            .filter(|&&i| self.design.per_shape.get(i).copied().unwrap_or(0) > 0)
            .map(|&i| {
                format!(
                    "{} ({}, {})",
                    self.names[i],
                    kind_name(self.doc.shapes[i].params.kind.unwrap_or(Kind::Fill)),
                    hex(self.doc.shapes[i].color)
                )
            })
            .collect();
        let text = format!(
            "{:.1} × {:.1} mm, {} stitches, {} colors. Sewing order: {}.",
            b.width() / 10.0,
            b.height() / 10.0,
            self.design.stitch_count(),
            self.design.blocks.len(),
            legend.join(" → ")
        );
        Ok(ToolResult {
            content: vec![Content::Image(png), Content::Text(text)],
            is_error: false,
        })
    }

    pub fn fit_view(&mut self) {
        self.request_fit();
    }
}

impl Studio {
    fn tool_send_to_skitch(&mut self) -> Result<ToolResult, String> {
        let s = self.machine.snapshot();
        self.machine.open = true;
        if s.link != crate::machine::Link::Connected {
            return Err("The Skitch isn't connected. Ask the person to click Connect in the Skitch window (it's open now), then try again.".into());
        }
        if self.design.blocks.is_empty() {
            return Err("There's nothing to sew yet.".into());
        }
        if self.design_over_hoop() {
            return Err("The design is bigger than the hoop.".into());
        }
        let ctx = self.egui_ctx.clone().unwrap_or_default();
        self.send_design(&ctx);
        Ok(ToolResult::text(
            "Sending. The Skitch window now walks the person through tracing the outline and sewing; call machine_status to follow along.",
        ))
    }

    /// The machine's state for agents.
    pub(crate) fn machine_json(&self) -> Value {
        use zigzag::machine::protocol;
        let s = self.machine.snapshot();
        let ours = self.machine.ours(&s);
        let step = crate::sew::step(&s, ours.is_some());
        let link = match &s.link {
            crate::machine::Link::Off => "not connected".to_string(),
            crate::machine::Link::Searching => "connecting".into(),
            crate::machine::Link::NotFound => "no machine found".into(),
            crate::machine::Link::NotPaired => "not paired with this computer".into(),
            crate::machine::Link::Failed(e) => format!("failed: {e}"),
            crate::machine::Link::Connected => "connected".into(),
            crate::machine::Link::Lost(e) => format!("connection lost: {e}"),
        };
        let error = protocol::error_help(s.error)
            .map(|h| json!({ "code": s.error, "title": h.title, "what_to_do": h.steps }));
        let mut v = json!({
            "connection": link,
            "simulated": s.sim,
            "step": crate::sew::step_name(step),
            "status": s.status.map(|st| st.label()),
            "error": error,
            "model": s.info.as_ref().map(|i| i.model.trim().to_string()),
            "has_our_design": ours.is_some(),
        });
        if let Some(sent) = ours {
            let total = sent.pen.stitches;
            let at = s.progress.map(|p| p.current_stitch as usize).unwrap_or(0);
            let threads: Vec<Value> = sent
                .pen
                .blocks
                .iter()
                .map(|b| {
                    let c = sent
                        .design
                        .blocks
                        .get(b.block)
                        .map(|k| k.color)
                        .unwrap_or([0; 3]);
                    json!({
                        "color": format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]),
                        "stitches": b.end - b.start,
                        "done": at >= b.end,
                    })
                })
                .collect();
            v["progress"] = json!({
                "stitch": at,
                "of": total,
                "thread": sent
                    .pen
                    .thread_at(at, s.status == Some(zigzag::machine::protocol::Status::WaitingForColorChange))
                    .map(|k| k + 1),
                "threads": threads,
            });
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Studio;
    use std::io::{Read, Write};

    const BADGE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
        <circle id="patch" cx="50" cy="50" r="48" fill="#1d3557"/>
        <path id="ring" d="M50,8 a42,42 0 1,0 0.01,0 Z M50,14 a36,36 0 1,0 0.01,0 Z" fill="#f1faee" fill-rule="evenodd"/>
        <rect id="bar" x="20" y="46" width="60" height="8" fill="#e63946"/>
        <path id="stitchline" d="M25,70 L75,70" stroke="#f1faee" stroke-width="1" fill="none"/>
    </svg>"##;

    fn text(r: &ToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| match c {
                Content::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ok(r: ToolResult) -> Value {
        assert!(!r.is_error, "tool failed: {}", text(&r));
        serde_json::from_str(&text(&r)).unwrap_or(Value::Null)
    }

    #[test]
    fn tool_list_is_well_formed() {
        let tools = tool_specs();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for want in [
            "get_design",
            "add_svg",
            "set_object",
            "render_preview",
            "check_design",
            "save",
            "export",
            "undo",
        ] {
            assert!(names.contains(&want), "{want} missing");
        }
        for t in tools.as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["description"].as_str().unwrap().len() > 10);
        }
    }

    #[test]
    fn agent_workflow_end_to_end() {
        let mut s = Studio::with_file(None, None);
        let r = ok(s.run_tool(
            "add_svg",
            &json!({ "svg": BADGE, "width_mm": 50.0, "replace": true }),
        ));
        assert_eq!(r["added"].as_array().unwrap().len(), 4);
        let design = ok(s.run_tool("get_design", &json!({})));
        let w = design["stats"]["width_mm"].as_f64().unwrap();
        assert!((w - 50.0).abs() < 1.5, "scaled to 50 mm: {w}");
        let names: Vec<String> = design["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["patch", "ring", "bar", "stitchline"]);

        // Ring and bar as satin columns; thread color; fabric for a thin cap.
        let ring = ok(s.run_tool("set_object", &json!({ "object": "ring", "kind": "satin" })));
        assert_eq!(ring["kind"], "satin");
        assert!(ring["problems"].as_array().unwrap().is_empty(), "{ring}");
        ok(s.run_tool("set_object", &json!({ "object": 2, "kind": "satin", "color": "#ffb703", "satin_underlay": ["center", "contour"] })));
        let bad = s.run_tool(
            "set_object",
            &json!({ "object": "stitchline", "kind": "fill" }),
        );
        assert!(bad.is_error && text(&bad).contains("stroke"));
        ok(s.run_tool(
            "set_document",
            &json!({ "fabric": "thin-cap", "hat_order": true, "preview_color": "#c1121f" }),
        ));

        // Preview is a real PNG with a summary.
        let img = s.run_tool("render_preview", &json!({ "width_px": 400 }));
        assert!(!img.is_error);
        let png = img
            .content
            .iter()
            .find_map(|c| match c {
                Content::Image(b) => Some(b.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(text(&img).contains("Sewing order"));

        // Undo the last change (document settings).
        ok(s.run_tool("undo", &json!({})));
        assert_eq!(s.doc.settings.fabric, "standard");
        ok(s.run_tool("set_document", &json!({ "fabric": "thin-cap" })));

        // Save, reopen, everything is still there.
        let dir = std::env::temp_dir().join(format!("zigzag-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("badge.svg");
        let r = s.run_tool("save", &json!({ "path": path.to_str().unwrap() }));
        assert!(!r.is_error, "{}", text(&r));
        let reopened = Document::open(&path).unwrap();
        assert_eq!(reopened.settings.fabric, "thin-cap");
        let bar = reopened
            .shapes
            .iter()
            .find(|x| zigzag::svg::name_words(&x.id) == vec!["bar"])
            .unwrap();
        assert_eq!(bar.params.kind, Some(Kind::Satin));
        assert_eq!(bar.color, [0xff, 0xb7, 0x03]);
        assert_eq!(
            bar.params.satin_underlay,
            Some(underlay::CENTER | underlay::CONTOUR)
        );

        let pes = dir.join("badge.pes");
        let r = s.run_tool(
            "export",
            &json!({ "format": "pes", "path": pes.to_str().unwrap() }),
        );
        assert!(!r.is_error, "{}", text(&r));
        assert!(std::fs::read(&pes).unwrap().starts_with(b"#PES0001"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agents_can_lay_out_objects() {
        let mut s = Studio::with_file(None, None);
        ok(s.run_tool(
            "add_svg",
            &json!({ "svg": BADGE, "width_mm": 50.0, "replace": true }),
        ));
        let r = ok(s.run_tool(
            "transform_objects",
            &json!({ "objects": "bar", "width_mm": 20.0, "move_to_mm": [0, 30], "rotate_deg": 90 }),
        ));
        let bar = &r["objects"][0];
        assert!(
            (bar["size_mm"][1].as_f64().unwrap() - 20.0).abs() < 0.2,
            "{bar}"
        );
        assert!(
            (bar["center_mm"][1].as_f64().unwrap() - 30.0).abs() < 0.2,
            "{bar}"
        );
        let r = ok(s.run_tool(
            "duplicate_objects",
            &json!({ "objects": ["bar", "stitchline"], "offset_mm": [5, 0] }),
        ));
        assert_eq!(r["added"].as_array().unwrap().len(), 2);
        assert_eq!(s.doc.shapes.len(), 6);
        let svg = text(&s.run_tool("get_svg", &json!({})));
        assert!(svg.contains("transform=\"matrix("));

        let t = ok(s.run_tool(
            "add_text",
            &json!({ "text": "ALPINE", "size_mm": 6, "arc_mm": 20, "y_mm": -15 }),
        ));
        assert_eq!(t["lettering"]["text"], "ALPINE");
        assert_eq!(t["kind"], "satin");
        let t = ok(s.run_tool(
            "edit_text",
            &json!({ "object": t["index"], "text": "ALPINE CLUB", "kind": "fill" }),
        ));
        assert_eq!(t["lettering"]["text"], "ALPINE CLUB");
        assert_eq!(t["kind"], "fill");
        let dir = std::env::temp_dir().join(format!("zigzag-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for fmt in ["jef", "vp3", "exp", "html"] {
            let p = dir.join(format!("d.{fmt}"));
            let r = s.run_tool(
                "export",
                &json!({ "format": fmt, "path": p.to_str().unwrap() }),
            );
            assert!(!r.is_error, "{fmt}: {}", text(&r));
        }
        let r = ok(s.run_tool(
            "open_file",
            &json!({ "path": dir.join("d.jef").to_str().unwrap() }),
        ));
        assert!(
            r["objects"]
                .as_array()
                .unwrap()
                .iter()
                .all(|o| o["kind"] == "manual")
        );
        let _ = std::fs::remove_dir_all(&dir);
        let bad = s.run_tool("add_text", &json!({ "text": "x", "font": "No Such Font" }));
        assert!(bad.is_error && text(&bad).contains("list_fonts"));
    }

    #[test]
    fn errors_are_helpful() {
        let mut s = Studio::with_file(None, None);
        let r = s.run_tool("set_object", &json!({ "object": 3 }));
        assert!(r.is_error && text(&r).contains("no object 3"));
        let r = s.run_tool("save", &json!({}));
        assert!(r.is_error && text(&r).contains("absolute path"));
        let r = s.run_tool(
            "add_svg",
            &json!({ "svg": "<svg xmlns='http://www.w3.org/2000/svg'/>" }),
        );
        assert!(r.is_error && text(&r).contains("nothing visible"));
        let r = s.run_tool("nope", &json!({}));
        assert!(r.is_error);
    }

    fn http(port: u16, body: &str, origin: Option<&str>) -> (u16, String) {
        http_as(port, body, origin, "application/json")
    }

    fn http_as(port: u16, body: &str, origin: Option<&str>, kind: &str) -> (u16, String) {
        let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let origin = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        write!(sock, "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Type: {kind}\r\n{origin}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut out = String::new();
        sock.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        (
            head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body.to_string(),
        )
    }

    /// A real JSON-RPC session over HTTP, with tool calls answered by the
    /// "UI thread" (this test's main thread) through `handle_mcp`.
    #[test]
    fn json_rpc_over_http() {
        let server = crate::mcp::start(0, tool_specs(), INSTRUCTIONS.into(), || {}).unwrap();
        let port = server.addr.port();
        let mut studio = Studio::with_file(None, None);
        studio.mcp = Some(server);

        let client = std::thread::spawn(move || {
            let (code, body) = http(
                port,
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
                None,
            );
            assert_eq!(code, 200);
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["result"]["serverInfo"]["name"], "zigzag");
            assert!(
                v["result"]["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("millimetres")
            );

            let (code, body) = http(
                port,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                None,
            );
            assert_eq!((code, body.as_str()), (202, ""));

            let (_, body) = http(
                port,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                None,
            );
            let v: Value = serde_json::from_str(&body).unwrap();
            assert!(v["result"]["tools"].as_array().unwrap().len() >= 12);

            let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"add_svg","arguments":{"svg": BADGE, "width_mm": 40}}});
            let (_, body) = http(port, &call.to_string(), Some("http://localhost:5173"));
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["result"]["isError"], false, "{body}");

            let call = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"render_preview","arguments":{"width_px": 300}}});
            let (_, body) = http(port, &call.to_string(), None);
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["result"]["content"][0]["type"], "image");
            assert_eq!(v["result"]["content"][0]["mimeType"], "image/png");

            // Other websites can't drive the studio.
            let (code, _) = http(
                port,
                r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#,
                Some("https://evil.example"),
            );
            assert_eq!(code, 403);
            // Nor from a sandboxed iframe (Origin: null), which any site can make.
            let export = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"export","arguments":{"format":"dst","path":"/tmp/zigzag-pwned.dst"}}}"#;
            let (code, _) = http(port, export, Some("null"));
            assert_eq!(code, 403);
            // Nor as a "simple" text post, which browsers send without a
            // CORS preflight (and charset parameters still count as JSON).
            let (code, _) = http_as(port, export, None, "text/plain");
            assert_eq!(code, 415);
            let (code, _) = http_as(
                port,
                r#"{"jsonrpc":"2.0","id":8,"method":"tools/list"}"#,
                None,
                "application/json; charset=utf-8",
            );
            assert_eq!(code, 200);
            assert!(
                !std::path::Path::new("/tmp/zigzag-pwned.dst").exists(),
                "nothing was written"
            );

            let (_, body) = http(port, r#"{"jsonrpc":"2.0","id":6,"method":"bogus"}"#, None);
            assert!(body.contains("-32601"));
        });
        while !client.is_finished() {
            studio.handle_mcp();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        client.join().unwrap();
        assert_eq!(
            studio.doc.shapes.len(),
            4,
            "the agent's artwork is on the canvas"
        );
    }

    #[test]
    fn misspelt_arguments_are_refused_with_what_the_tool_takes() {
        let mut s = Studio::with_file(None, None);
        ok(s.run_tool("add_svg", &json!({ "svg": BADGE })));
        let r = s.run_tool("set_object", &json!({ "object": 0, "widht_mm": 2.0 }));
        assert!(r.is_error);
        let t = text(&r);
        assert!(t.contains("`widht_mm`") && t.contains("`width_mm`"), "{t}");
    }

    #[test]
    fn an_atomic_batch_is_one_undo_step_and_passes_results_on() {
        let mut s = Studio::with_file(None, None);
        let undo0 = s.undo.len();
        let r = ok(s.run_tool(
            "batch",
            &json!({
                "label": "Badge in satin",
                "steps": [
                    { "tool": "add_svg", "arguments": { "svg": BADGE } },
                    { "tool": "set_object", "arguments": { "object": "$0.added.2.name", "kind": "satin" } },
                    { "tool": "get_design" }
                ]
            }),
        ));
        assert_eq!(r["results"].as_array().unwrap().len(), 3);
        assert_eq!(s.undo.len(), undo0 + 1, "one undo step");
        let bar = r["results"][0]["result"]["added"][2]["name"]
            .as_str()
            .unwrap()
            .to_string();
        let i = s.names.iter().position(|n| *n == bar).unwrap();
        assert_eq!(s.doc.shapes[i].params.kind, Some(Kind::Satin));
        assert_eq!(s.status, "Badge in satin");
    }

    /// Later steps of a batch name objects as they are after the earlier ones.
    #[test]
    fn a_batch_step_after_a_delete_finds_objects_by_name() {
        let mut s = Studio::with_file(None, None);
        ok(s.run_tool("add_svg", &json!({ "svg": BADGE })));
        let names = s.names.clone();
        assert!(names.len() >= 3);
        let (first, last) = (names[0].clone(), names.last().unwrap().clone());
        let uid = s.doc.shapes.last().unwrap().uid;
        ok(s.run_tool(
            "batch",
            &json!({ "steps": [
                { "tool": "delete_object", "arguments": { "object": first } },
                { "tool": "set_object", "arguments": { "object": last, "spacing_mm": 0.7 } }
            ] }),
        ));
        assert_eq!(s.doc.shapes.len(), names.len() - 1);
        let edited = s.doc.shapes.iter().find(|x| x.uid == uid).unwrap();
        assert_eq!(edited.params.spacing, Some(0.7));
        assert_eq!(s.names.len(), s.doc.shapes.len());
    }

    #[test]
    fn a_failing_atomic_batch_changes_nothing() {
        let mut s = Studio::with_file(None, None);
        ok(s.run_tool("add_svg", &json!({ "svg": BADGE })));
        let (n, undo) = (s.doc.shapes.len(), s.undo.len());
        s.modified = true;
        let failed = |r: ToolResult| -> Value {
            assert!(r.is_error, "a failed batch is an error for MCP clients");
            serde_json::from_str(&text(&r)).unwrap()
        };
        let r = failed(s.run_tool(
            "batch",
            &json!({ "steps": [
                { "tool": "add_svg", "arguments": { "svg": BADGE } },
                { "tool": "set_object", "arguments": { "object": "nope", "kind": "satin" } }
            ] }),
        ));
        assert!(
            r["error"].as_str().unwrap().contains("Step 1 (set_object)"),
            "{r}"
        );
        assert_eq!(s.doc.shapes.len(), n, "the first step was rolled back");
        assert_eq!(s.undo.len(), undo);
        assert!(s.modified, "still unsaved");
        // Opening a file would clear the history: refused, nothing lost.
        let r = failed(s.run_tool(
            "batch",
            &json!({ "steps": [ { "tool": "open_file", "arguments": { "path": "/tmp/x.svg" } } ] }),
        ));
        assert!(r["error"].as_str().unwrap().contains("open_file"), "{r}");
        assert_eq!(s.undo.len(), undo);
        // "$$" is a literal dollar sign, not a step reference.
        let r = ok(s.run_tool(
            "batch",
            &json!({ "steps": [ { "tool": "add_text", "arguments": { "text": "$$5" } } ] }),
        ));
        let name = r["results"][0]["result"]["name"]
            .as_str()
            .unwrap()
            .to_string();
        let i = s.names.iter().position(|x| *x == name).unwrap();
        assert_eq!(s.doc.lettering(i).unwrap().text, "$5");
        s.run_tool("undo", &json!({}));
        // Not atomic: the first step stays.
        failed(s.run_tool(
            "batch",
            &json!({ "atomic": false, "steps": [
                { "tool": "add_svg", "arguments": { "svg": BADGE } },
                { "tool": "set_object", "arguments": { "object": "nope" } }
            ] }),
        ));
        assert_eq!(s.doc.shapes.len(), n * 2);
    }

    #[test]
    fn trace_image_and_round_corners() {
        let dir = std::env::temp_dir().join(format!("tl-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("mark.png");
        let img = vectorcraft_trace::Raster::from_fn(120, 60, |x, y| {
            if (10..50).contains(&x) && (10..50).contains(&y) {
                [200, 20, 30, 255]
            } else if (70..110).contains(&x) && (10..50).contains(&y) {
                [20, 50, 190, 255]
            } else {
                [255, 255, 255, 255]
            }
        });
        std::fs::write(&png, img.encode_png()).unwrap();
        let mut s = Studio::with_file(None, None);
        let r = ok(s.run_tool(
            "trace_image",
            &json!({ "path": png.to_str().unwrap(), "width_mm": 60.0, "threads": "brother" }),
        ));
        assert_eq!(r["added"].as_array().unwrap().len(), 2, "{r}");
        assert_eq!(r["traced_as"], "flat logo");
        let names: Vec<String> = r["added"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap().into())
            .collect();
        let r = s.run_tool(
            "round_corners",
            &json!({ "objects": names, "radius_mm": 2.0 }),
        );
        assert!(!r.is_error, "{}", text(&r));
        assert!(text(&r).contains("Rounded 8 corners"), "{}", text(&r));
        let p = s.doc.edit_path(0).unwrap();
        assert!(zigzag::bezier::path_corners(&p).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tools_say_which_only_read() {
        let tools = tool_specs();
        let get = |n: &str| {
            tools
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == n)
                .unwrap()
                .clone()
        };
        assert_eq!(get("get_design")["annotations"]["readOnlyHint"], true);
        assert_eq!(get("render_preview")["annotations"]["readOnlyHint"], true);
        assert_eq!(get("delete_object")["annotations"]["destructiveHint"], true);
        assert_eq!(get("set_object")["annotations"]["readOnlyHint"], false);
    }

    #[test]
    fn rpc_runs_tool_calls_through_any_runner() {
        // The headless (stdio) server and the HTTP one share this.
        let mut s = Studio::with_file(None, None);
        let mut call = |name: String, args: Value| s.run_tool(&name, &args);
        let tools = tool_specs();
        let r = crate::mcp::handle_rpc(
            &json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "add_svg", "arguments": { "svg": BADGE } } }),
            &tools,
            INSTRUCTIONS,
            &mut call,
        )
        .unwrap();
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(s.doc.shapes.len(), 4);
    }
}
