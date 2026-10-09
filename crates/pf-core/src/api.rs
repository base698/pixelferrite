//! The JSON command API. Everything an outside program can do to a document
//! is a command here, so the MCP server drives the same engine as the app,
//! whether it is talking to an open window or working on a file by itself.

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use image::imageops::FilterType;
use serde_json::{Value as J, json};

use crate::aiedit::resize;
use crate::blend::BlendMode;
use crate::buf::{Mask, Pixmap};
use crate::document::{Document, Layer, LayerId, Target};
use crate::fill::{self, FillMode, GradientOp, GradientParams, GradientShape};
use crate::filter::{self, EdgeParams, EdgeStyle, Filter};
use crate::geom::IRect;
use crate::ops::{Clip, Reorder};
use crate::paint::{BrushParams, PaintKind, Stroke};
use crate::selection::{self, Combine};
use crate::text::{Align, FontRef, TextSpec};
use crate::transform::{PerspectiveMode, PerspectiveOp};
use crate::{composite, fx, io, segment, text};

pub type R<T> = Result<T, String>;

/// Font file bytes and the face's index within the file.
pub type FontData = (Arc<Vec<u8>>, u32);

/// What the engine needs from whoever is hosting it.
pub trait Host {
    /// The face for a family and style; the built-in font when it is missing.
    fn font(&mut self, family: &str, bold: bool, italic: bool) -> FontData;
    fn font_families(&mut self) -> Vec<String>;
}

/// State that lasts between commands but is not part of the document.
#[derive(Default)]
pub struct Scratch {
    pub clip: Option<Clip>,
}

/// Given to AI clients so they know what they can send.
pub const REFERENCE: &str = r##"Pixelferrite is a layered raster image editor. Commands are JSON objects with an "op".

COORDINATES are document pixels: x to the right, y down, (0,0) the top-left of the canvas. RECT is [x0,y0,x1,y1]
(x1,y1 exclusive). POINT is [x,y]. COLOR is "#rrggbb", "#rrggbbaa", [r,g,b] or [r,g,b,a] with 0-255 channels.
LAYER is a layer id (a number from get_document_info) or a layer's exact name. Layers are listed bottom first.

THE ACTIVE LAYER. Painting, fills, filters and adjustments change the active layer, limited to the selection when
there is one. Any command may carry "layer": LAYER to make that layer active first, and "target": "pixels" | "mask"
to choose whether its pixels or its mask are edited. Locked or hidden layers refuse edits.
Every changing command is one undo step. Look at your work with the screenshot tool; zoom in with "region".

DOCUMENT
{"op":"new","width":1600,"height":1200,"background":"#ffffff"}     background omitted = transparent
{"op":"open","path":"/abs/photo.jpg"}                               .ora keeps layers; png, jpg, gif, webp, bmp, tiff open as one layer
{"op":"save","path":"/abs/work.ora"}                                layered OpenRaster; path may be omitted once the document has one
{"op":"export","path":"/abs/out.png"}                               flattened png, jpg or gif, by extension
{"op":"export_layer","path":"/abs/layer.png","layer":LAYER}         one layer's own pixels (mask applied), as png
   new and open refuse to discard unsaved work in the app: save first, or add "discard_unsaved":true.
{"op":"undo","steps":1} {"op":"redo","steps":1}
{"op":"crop","rect":RECT}                                           rect omitted = crop to the selection's bounds
{"op":"flatten"}
{"op":"content_aware_scale","width":1200,"height":800}              shrinks the active layer by removing its least interesting seams

LOOKING
{"op":"get_document_info"}                                          size, layers, selection, history
{"op":"get_layer_info","layer":LAYER}                               plus the bounds of what is actually painted and the text settings
{"op":"sample","x":10,"y":20,"radius":0}                            the colour of the image there; "layer":LAYER reads one layer; radius averages a square
{"op":"find_color","color":COLOR,"tolerance":40,"rect":RECT}        where that colour is: pixel count, bounds and centre. Good for locating things.
{"op":"list_fonts"}

LAYERS
{"op":"add_layer","name":"Sketch","fill":COLOR}                     fill optional
{"op":"add_image","path":"/abs/logo.png","x":40,"y":40}             as a new layer at its own size, centred when x,y are omitted
{"op":"add_image","path":"/abs/sky.jpg","rect":RECT}                stretched to that rectangle
{"op":"add_image","path":"/abs/sky.jpg","into_selection":true}      scaled to cover the selection and cut to its shape
{"op":"delete_layer","layer":LAYER} {"op":"duplicate_layer","layer":LAYER}
{"op":"set_active","layer":LAYER,"target":"pixels"}
{"op":"set_layer","layer":LAYER,"name":"Sky","visible":true,"locked":false,"opacity":0.8,"blend":"multiply","x":0,"y":0}
   any subset. opacity 0-1. blend: normal darken multiply color-burn lighten screen color-dodge add overlay soft-light
   hard-light difference exclusion hue saturation color luminosity. "dx","dy" move it relative to where it is.
{"op":"reorder_layer","layer":LAYER,"to":"front"}                   front | back | forward | backward | an index (0 = bottom)
{"op":"merge_down","layer":LAYER}
{"op":"flip_layer","layer":LAYER,"axis":"horizontal"}               horizontal | vertical
{"op":"resize_layer","layer":LAYER,"scale":0.5}                     or "width" and/or "height" in pixels; keeps the layer's centre
{"op":"perspective","layer":LAYER,"corners":[POINT,POINT,POINT,POINT],"mode":"distort"}
   corners are top-left, top-right, bottom-right, bottom-left. distort moves the layer's corners there (this also
   rotates, skews and scales). straighten treats the four points as a skewed rectangle and pulls it square.
{"op":"add_mask","layer":LAYER}                                     from the selection when there is one, else fully revealing
{"op":"delete_mask","layer":LAYER} {"op":"apply_mask","layer":LAYER} {"op":"invert_mask","layer":LAYER}

SELECTION. Each takes "mode": replace (default) | add | subtract | intersect, and "feather": pixels of soft edge.
{"op":"select_all"} {"op":"deselect"} {"op":"invert_selection"}
{"op":"select_rect","rect":RECT}
{"op":"select_ellipse","rect":RECT}                                 the ellipse inscribed in that rectangle
{"op":"select_polygon","points":[POINT,...]}
{"op":"select_wand","x":120,"y":80,"tolerance":32,"contiguous":true,"all_layers":true}   pixels like the one clicked
{"op":"select_color","color":COLOR,"tolerance":40,"rect":RECT,"all_layers":true}       every pixel near that colour, optionally only inside rect
{"op":"select_subject","rect":RECT}                                 separates the subject inside the box from its background
{"op":"select_regions","points":[POINT,...],"detail":0.5}           the edge-bounded region under each point
{"op":"feather_selection","radius":4}
{"op":"grow_selection","pixels":3}                                  negative shrinks; approximate

PAINT
{"op":"fill","color":COLOR,"opacity":1} {"op":"clear"}              the selection, or the whole layer
{"op":"bucket","x":10,"y":10,"color":COLOR,"tolerance":32,"contiguous":true,"all_layers":false,"opacity":1}
{"op":"stroke","tool":"brush","points":[POINT,...],"color":COLOR,"size":24,"hardness":0.8,"opacity":1,"flow":1}
   tool: brush | pencil | eraser | smudge ("strength":0.5) | clone ("source":POINT, where the first point copies from).
   One point is a single dab.
{"op":"gradient","from":POINT,"to":POINT,"colors":[COLOR,COLOR],"shape":"linear","opacity":1}   linear | radial
{"op":"copy"} {"op":"cut"} {"op":"paste"}                           paste makes a new layer

FILTERS AND ADJUSTMENTS
{"op":"filter","name":"gaussian_blur","radius":4}
   gaussian_blur {radius}            sharpen {radius 1.5, amount 1, threshold 0}
   surface_blur {radius 4, tolerance 25}   smooths but keeps edges     reduce_noise {strength 12}
   heal {radius 6}                   fills the selection from its surroundings; needs a selection
   auto_contrast {clip 0.5}          equalize {amount 1}               local_contrast {clip 2.5, tiles 8}
   threshold {level 128}             adaptive_threshold {radius 20, offset 6}
   match_colors {reference: LAYER or "/abs/image.jpg", amount 1}
   lens_distortion {amount 0.15}     edge_detect {threshold 40, smoothing 1.4, thickness 1, style lines|on_black|highlight, color}
{"op":"adjust","brightness":0,"contrast":0,"saturation":0,"hue":0,"temperature":0}
   brightness, contrast, saturation and temperature run -1 to 1 (saturation -1 is grey, temperature +1 warmer); hue is degrees.
{"op":"red_eye","rect":RECT,"darken":0.3}                           neutralises strongly red pixels in the rect, or in the selection when rect is omitted

TEXT
{"op":"add_text","text":"Hello","x":100,"y":200,"size":72,"color":COLOR,"font":"Helvetica","bold":false,"italic":false,
 "align":"left","tracking":0,"leading":1.2}                         x,y is the start of the first baseline
   "path":[POINT,...] makes the text follow that line; "path":"selection" runs it around the selection's outline.
{"op":"set_text","layer":LAYER,"text":"New words","size":96}        any of add_text's fields; x,y move it

AI (uses the user's OpenAI key; each call is a paid request and uploads the image)
{"op":"ai_edit","prompt":"remove the lamp post","source":"visible"} sends the selection's surroundings (or the whole image) and adds the
   answer as a new layer cut to the selection. source: visible (default) | layer. An empty prompt with a selection just extends the picture.

{"op":"batch","commands":[...]}                                     runs in order and stops at the first error
"##;

/// Check a complete request before any command is applied.
pub fn validate_request(c: &J) -> R<()> {
    let mut pending = vec![(c, 0)];
    let mut count = 0;
    while let Some((command, depth)) = pending.pop() {
        count += 1;
        if count > 1000 || depth > 16 {
            return Err("Automation is limited to 1000 commands and 16 nested batches per request.".into());
        }
        if command["op"] == "batch" {
            let commands = command["commands"].as_array().ok_or("batch needs \"commands\"")?;
            pending.extend(commands.iter().map(|c| (c, depth + 1)));
        }
    }
    Ok(())
}

/// The document a `new` or `open` command asks for, so a host with its own
/// window can swap documents its own way. `None` for every other command.
pub fn replacement(c: &J) -> R<Option<Document>> {
    match c["op"].as_str() {
        Some("new") => {
            let (w, h) = (uint(c, "width").unwrap_or(1600), uint(c, "height").unwrap_or(1200));
            io::limits::validate_dimensions(w, h).map_err(|e| e.to_string())?;
            let background = if c["background"].is_null() { None } else { Some(color(&c["background"])?) };
            Ok(Some(Document::new(w, h, background)))
        }
        Some("open") => {
            let path = path(c, "path")?;
            io::open(path).map(Some).map_err(|e| format!("could not open {}: {e}", path.display()))
        }
        _ => Ok(None),
    }
}

pub fn execute(doc: &mut Document, scratch: &mut Scratch, host: &mut dyn Host, c: &J) -> R<J> {
    validate_request(c)?;
    run(doc, scratch, host, c)
}

fn run(doc: &mut Document, scratch: &mut Scratch, host: &mut dyn Host, c: &J) -> R<J> {
    let op = c["op"].as_str().ok_or("the command needs an \"op\"")?;
    if op == "batch" {
        let commands = c["commands"].as_array().ok_or("batch needs \"commands\"")?;
        let mut results = Vec::new();
        for (i, command) in commands.iter().enumerate() {
            results.push(run(doc, scratch, host, command).map_err(|e| format!("command {i} ({}) failed: {e}. The {i} before it were applied.", command["op"].as_str().unwrap_or("?")))?);
        }
        return Ok(json!(results));
    }
    if let Some(new) = replacement(c)? {
        *doc = new;
        *scratch = Scratch::default();
        return Ok(document_info(doc));
    }
    // "layer" and "target" on any command choose what it works on.
    let reads_layer_itself = matches!(op, "get_layer_info" | "sample" | "export_layer" | "match_colors");
    if !c["layer"].is_null() && !reads_layer_itself {
        doc.state.active = layer_id(doc, &c["layer"])?;
        if c["target"].is_null() {
            doc.target = Target::Pixels;
        }
    }
    match c["target"].as_str() {
        None => {}
        Some("pixels") => doc.target = Target::Pixels,
        Some("mask") => {
            if doc.state.active_layer().is_none_or(|l| l.mask.is_none()) {
                return Err("that layer has no mask; add_mask first".into());
            }
            doc.target = Target::Mask;
        }
        Some(other) => return Err(format!("target is pixels or mask, not {other}")),
    }
    let active = doc.state.active;
    match op {
        // ---- looking ----
        "get_document_info" => Ok(document_info(doc)),
        "get_layer_info" => {
            let id = if c["layer"].is_null() { active } else { layer_id(doc, &c["layer"])? };
            layer_info(doc, id)
        }
        "get_canvas_screenshot" => screenshot(doc, c),
        "sample" => {
            let (x, y) = (int(c, "x").ok_or("sample needs x and y")?, int(c, "y").ok_or("sample needs x and y")?);
            let r = int(c, "radius").unwrap_or(0).clamp(0, 64);
            let src = if c["layer"].is_null() { composite::flatten(&doc.state) } else {
                let id = layer_id(doc, &c["layer"])?;
                composite::layer_on_canvas(&doc.state, doc.state.layer(id).unwrap())
            };
            let area = IRect::new(x - r, y - r, x + r + 1, y + r + 1).intersect(src.rect());
            if area.is_empty() {
                return Err("that point is outside the canvas".into());
            }
            let (mut sum, mut weight) = ([0f64; 4], 0f64);
            for yy in area.y0..area.y1 {
                for xx in area.x0..area.x1 {
                    let p = src.px(xx, yy);
                    let a = p[3] as f64 / 255.0;
                    for k in 0..3 {
                        sum[k] += p[k] as f64 * a;
                    }
                    sum[3] += p[3] as f64;
                    weight += a;
                }
            }
            let n = (area.width() * area.height()) as f64;
            let rgba = [0, 1, 2].map(|k| if weight > 0.0 { (sum[k] / weight).round() as u8 } else { 0 });
            let a = (sum[3] / n).round() as u8;
            Ok(json!({"color": [rgba[0], rgba[1], rgba[2], a], "hex": format!("#{:02x}{:02x}{:02x}", rgba[0], rgba[1], rgba[2])}))
        }
        "find_color" => {
            let m = color_mask(doc, c)?;
            let area = m.data.iter().filter(|v| **v > 0).count();
            let Some(b) = selection::bounds(&m) else { return Ok(json!({"pixels": 0})) };
            let (mut sx, mut sy) = (0f64, 0f64);
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    if m.px(x, y)[0] > 0 {
                        sx += x as f64;
                        sy += y as f64;
                    }
                }
            }
            Ok(json!({"pixels": area, "bounds": rect_json(b), "centre": [(sx / area as f64).round(), (sy / area as f64).round()]}))
        }
        "list_fonts" => Ok(json!({"built_in": "Ubuntu Light (used when \"font\" is empty or missing)", "families": host.font_families()})),

        // ---- document ----
        "save" => {
            let target = match c["path"].as_str() {
                Some(p) => std::path::PathBuf::from(p),
                None => doc.path.clone().ok_or("this document has no file yet; give \"path\" ending in .ora")?,
            };
            if !io::is_ora(&target) {
                return Err("save writes layered .ora files; use export for png, jpg or gif".into());
            }
            absolute(&target)?;
            io::save(doc, &target).map_err(|e| e.to_string())?;
            Ok(json!({"saved": target.display().to_string()}))
        }
        "export" => {
            let target = path(c, "path")?;
            io::export(&doc.state, target).map_err(|e| e.to_string())?;
            Ok(json!({"exported": target.display().to_string(), "width": doc.state.width, "height": doc.state.height}))
        }
        "export_layer" => {
            let target = path(c, "path")?;
            let id = if c["layer"].is_null() { active } else { layer_id(doc, &c["layer"])? };
            let l = doc.state.layer(id).ok_or("no such layer")?;
            let mut px = (*l.pixels).clone();
            if let (Some(m), true) = (&l.mask, l.mask_enabled) {
                for (p, m) in px.data.chunks_exact_mut(4).zip(&m.data) {
                    p[3] = ((p[3] as u32 * *m as u32 + 127) / 255) as u8;
                }
            }
            let bytes = io::encode_png(&px).map_err(|e| e.to_string())?;
            io::atomic::write(target, &bytes, false).map_err(|e| e.to_string())?;
            Ok(json!({"exported": target.display().to_string(), "x": l.x, "y": l.y, "width": px.w, "height": px.h}))
        }
        "undo" | "redo" => {
            let mut done = 0;
            for _ in 0..uint(c, "steps").unwrap_or(1).min(200) {
                if !(if op == "undo" { doc.undo() } else { doc.redo() }) {
                    break;
                }
                done += 1;
            }
            let (names, pos) = doc.history();
            Ok(json!({"steps": done, "history": names.collect::<Vec<_>>(), "applied": pos}))
        }
        "crop" => {
            let r = match c.get("rect") {
                Some(r) if !r.is_null() => rect(r)?,
                _ => doc.state.selection.as_deref().and_then(selection::bounds).ok_or("crop needs a rect or a selection")?,
            };
            if r.intersect(doc.canvas()).is_empty() {
                return Err("that rectangle is outside the canvas".into());
            }
            doc.crop(r);
            Ok(json!({"width": doc.state.width, "height": doc.state.height}))
        }
        "flatten" => {
            doc.flatten_image();
            Ok(document_info(doc))
        }
        "content_aware_scale" => {
            let l = doc.state.active_layer().ok_or("there is no active layer")?;
            let (w, h) = (uint(c, "width").unwrap_or(l.pixels.w), uint(c, "height").unwrap_or(l.pixels.h));
            if w == 0 || h == 0 {
                return Err("width and height must be at least 1".into());
            }
            if !doc.content_aware_scale(w, h) {
                return Err("content-aware scale only shrinks, and the layer must be unlocked".into());
            }
            Ok(changed(doc))
        }

        // ---- layers ----
        "add_layer" => {
            doc.check_layer_capacity(doc.state.width, doc.state.height).map_err(|e| e.to_string())?;
            let fill = if c["fill"].is_null() { [0; 4] } else { color(&c["fill"])? };
            let px = Pixmap::filled(doc.state.width, doc.state.height, fill);
            let name = c["name"].as_str().unwrap_or("Layer");
            let id = doc.try_insert_layer("New Layer", Layer::new(name, px, 0, 0)).map_err(|e| e.to_string())?;
            Ok(json!({"layer": id}))
        }
        "add_image" => {
            let file = path(c, "path")?;
            let px = io::load_pixmap(file).map_err(|e| format!("could not open {}: {e}", file.display()))?;
            let name = c["name"].as_str().map(str::to_owned).unwrap_or_else(|| file.file_stem().and_then(|s| s.to_str()).unwrap_or("Layer").to_owned());
            let id = if c["into_selection"].as_bool() == Some(true) {
                doc.add_image_in_selection(&name, &px).ok_or("select the area to fill first (or the image would exceed the size limits)")?
            } else if !c["rect"].is_null() {
                doc.add_image_scaled(&name, &px, rect(&c["rect"])?).map_err(|e| e.to_string())?
            } else {
                doc.check_layer_capacity(px.w, px.h).map_err(|e| e.to_string())?;
                let x = int(c, "x").unwrap_or((doc.state.width as i32 - px.w as i32) / 2);
                let y = int(c, "y").unwrap_or((doc.state.height as i32 - px.h as i32) / 2);
                io::limits::validate_offset(x, y).map_err(|e| e.to_string())?;
                doc.try_insert_layer("Add Image", Layer::new(name, px, x, y)).map_err(|e| e.to_string())?
            };
            layer_info(doc, id)
        }
        "delete_layer" => {
            if doc.state.layers.len() <= 1 {
                return Err("the last layer can't be deleted".into());
            }
            doc.delete_layer(active);
            Ok(document_info(doc))
        }
        "duplicate_layer" => {
            let l = doc.state.active_layer().ok_or("there is no active layer")?;
            doc.check_layer_capacity(l.pixels.w, l.pixels.h * if l.mask.is_some() { 2 } else { 1 }).map_err(|e| e.to_string())?;
            doc.duplicate_layer(active);
            Ok(json!({"layer": doc.state.active}))
        }
        "set_active" => Ok(json!({"active": doc.state.active, "target": target_name(doc)})),
        "set_layer" => {
            let before = doc.begin();
            let name = c["name"].as_str().map(|n| n.chars().take(200).collect::<String>());
            let blend = c["blend"].as_str().map(blend_mode).transpose()?;
            let l = doc.state.active_layer_mut().ok_or("there is no active layer")?;
            let old = l.rect();
            if let Some(n) = name {
                l.name = n;
            }
            if let Some(v) = c["visible"].as_bool() {
                l.visible = v;
            }
            if let Some(v) = c["locked"].as_bool() {
                l.locked = v;
            }
            if let Some(v) = c["opacity"].as_f64() {
                l.opacity = (v as f32).clamp(0.0, 1.0);
            }
            if let Some(b) = blend {
                l.blend = b;
            }
            let (x, y) = (int(c, "x").unwrap_or(l.x).saturating_add(int(c, "dx").unwrap_or(0)), int(c, "y").unwrap_or(l.y).saturating_add(int(c, "dy").unwrap_or(0)));
            if let Err(e) = io::limits::validate_offset(x, y) {
                doc.state = before;
                return Err(e.to_string());
            }
            (l.x, l.y) = (x, y);
            l.touch();
            let new = l.rect();
            doc.mark_dirty(old.union(new));
            doc.mark_all_dirty();
            doc.commit("Change Layer", before);
            layer_info(doc, active)
        }
        "reorder_layer" => {
            match &c["to"] {
                J::String(s) => doc.reorder_layer(active, match s.as_str() {
                    "front" => Reorder::Front,
                    "back" => Reorder::Back,
                    "forward" => Reorder::Forward,
                    "backward" => Reorder::Backward,
                    other => return Err(format!("to is front, back, forward, backward or an index, not {other}")),
                }),
                J::Number(n) => doc.move_layer_to(active, n.as_u64().unwrap_or(0).min(doc.state.layers.len() as u64 - 1) as usize),
                _ => return Err("reorder_layer needs \"to\"".into()),
            }
            Ok(document_info(doc))
        }
        "merge_down" => {
            doc.merge_down(active).map_err(|e| e.to_string())?;
            Ok(document_info(doc))
        }
        "flip_layer" => {
            editable(doc)?;
            doc.flip_layer(active, c["axis"].as_str() != Some("vertical"));
            Ok(changed(doc))
        }
        "resize_layer" => resize_layer(doc, c),
        "perspective" => {
            let pts = points(&c["corners"])?;
            let quad: [(f32, f32); 4] = pts.try_into().map_err(|_| "corners needs four points: top-left, top-right, bottom-right, bottom-left")?;
            let mode = match c["mode"].as_str().unwrap_or("distort") {
                "distort" => PerspectiveMode::Distort,
                "straighten" => PerspectiveMode::Straighten,
                other => return Err(format!("mode is distort or straighten, not {other}")),
            };
            let mut op = PerspectiveOp::begin(doc).ok_or("this layer is hidden, locked or too small")?;
            if !op.update(doc, quad, mode) {
                op.cancel(doc);
                return Err("those corners don't make a usable shape (or the result would be too large)".into());
            }
            op.finish(doc);
            Ok(changed(doc))
        }
        "add_mask" | "delete_mask" | "apply_mask" | "invert_mask" => {
            let l = editable(doc)?;
            match (op, l.mask.is_some()) {
                ("add_mask", true) => return Err("this layer already has a mask".into()),
                ("add_mask", false) => {
                    doc.check_layer_resize(active, l.pixels.w, l.pixels.h * 2).map_err(|e| e.to_string())?;
                    doc.add_mask(active)
                }
                (_, false) => return Err("this layer has no mask".into()),
                ("delete_mask", _) => doc.delete_mask(active),
                ("apply_mask", _) => doc.apply_mask(active),
                _ => doc.invert_mask(active),
            }
            layer_info(doc, active)
        }

        // ---- selection ----
        "select_all" => {
            doc.select_all();
            Ok(selection_info(doc))
        }
        "deselect" => {
            doc.deselect();
            Ok(selection_info(doc))
        }
        "invert_selection" => {
            doc.invert_selection();
            Ok(selection_info(doc))
        }
        "select_rect" | "select_ellipse" | "select_polygon" | "select_wand" | "select_color" | "select_subject" | "select_regions" => {
            let (w, h) = (doc.state.width, doc.state.height);
            let all_layers = c["all_layers"].as_bool().unwrap_or(true);
            let source = |doc: &Document| fill::sample_source(&doc.state, all_layers).ok_or("there is no layer to look at");
            let (name, mask) = match op {
                "select_rect" => ("Rectangular Selection", selection::rect_mask(w, h, rect(&c["rect"])?)),
                "select_ellipse" => {
                    let r = rect(&c["rect"])?;
                    ("Elliptical Selection", selection::ellipse_mask(w, h, r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32))
                }
                "select_polygon" => {
                    let pts = points(&c["points"])?;
                    if pts.len() < 3 {
                        return Err("a polygon needs at least three points".into());
                    }
                    ("Free Selection", selection::polygon_mask(w, h, &pts))
                }
                "select_wand" => {
                    let seed = (int(c, "x").ok_or("select_wand needs x and y")?, int(c, "y").ok_or("select_wand needs x and y")?);
                    if !doc.canvas().contains(seed.0, seed.1) {
                        return Err("that point is outside the canvas".into());
                    }
                    let tolerance = uint(c, "tolerance").unwrap_or(32).min(255) as u8;
                    ("Magic Wand", selection::flood_mask(&source(doc)?, seed, tolerance, c["contiguous"].as_bool().unwrap_or(true), None))
                }
                "select_color" => ("Select Color", color_mask(doc, c)?),
                "select_subject" => {
                    let r = rect(&c["rect"])?.intersect(doc.canvas());
                    if r.width() < 4 || r.height() < 4 {
                        return Err("the box around the subject is too small or outside the canvas".into());
                    }
                    ("Select Subject", segment::grabcut(&source(doc)?, r))
                }
                _ => {
                    let pts = points(&c["points"])?;
                    let regions = segment::watershed(&source(doc)?, c["detail"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0) as f32);
                    let labels: Vec<u32> = pts.iter().filter_map(|p| regions.label_at(p.0, p.1)).collect();
                    if labels.is_empty() {
                        return Err("none of those points are on the canvas".into());
                    }
                    ("Region Selection", regions.mask(&labels, w, h))
                }
            };
            let mask = match c["feather"].as_f64() {
                Some(f) if f > 0.0 => filter::blur_mask(&mask, (f as f32).min(500.0)),
                _ => mask,
            };
            let mode = match c["mode"].as_str().unwrap_or("replace") {
                "replace" => Combine::Replace,
                "add" => Combine::Add,
                "subtract" => Combine::Subtract,
                "intersect" => Combine::Intersect,
                other => return Err(format!("mode is replace, add, subtract or intersect, not {other}")),
            };
            doc.select(name, &mask, mode);
            Ok(selection_info(doc))
        }
        "feather_selection" | "grow_selection" => {
            let sel = doc.state.selection.clone().ok_or("nothing is selected")?;
            let out = if op == "feather_selection" {
                filter::blur_mask(&sel, (c["radius"].as_f64().unwrap_or(4.0) as f32).clamp(0.1, 500.0))
            } else {
                let n = c["pixels"].as_f64().unwrap_or(1.0) as f32;
                if n == 0.0 {
                    return Ok(selection_info(doc));
                }
                // Blur, then cut near one end of the ramp: low to grow, high to shrink.
                let mut m = filter::blur_mask(&sel, (n.abs() * 0.6).clamp(0.3, 300.0));
                let cut = if n > 0.0 { 12 } else { 243 };
                m.data.iter_mut().for_each(|v| *v = if *v > cut { 255 } else { 0 });
                m
            };
            let name = if op == "feather_selection" { "Feather Selection" } else { "Grow Selection" };
            doc.set_selection(name, out.data.iter().any(|v| *v > 0).then_some(out));
            Ok(selection_info(doc))
        }

        // ---- paint ----
        "fill" => {
            editable(doc)?;
            let cov = fill::selection_or_all(doc);
            let opacity = c["opacity"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
            if !fill::fill_mask(doc, "Fill", &cov, FillMode::Color(color(&c["color"])?), opacity) {
                return Err("nothing could be filled: the selection misses the canvas or the layer is too large to grow".into());
            }
            Ok(changed(doc))
        }
        "clear" => {
            editable(doc)?;
            if !doc.clear() {
                return Err("nothing to clear: the selection doesn't touch this layer".into());
            }
            Ok(changed(doc))
        }
        "bucket" => {
            editable(doc)?;
            let seed = (int(c, "x").ok_or("bucket needs x and y")?, int(c, "y").ok_or("bucket needs x and y")?);
            if !doc.canvas().contains(seed.0, seed.1) {
                return Err("that point is outside the canvas".into());
            }
            let ok = fill::bucket(doc, seed, color(&c["color"])?, uint(c, "tolerance").unwrap_or(32).min(255) as u8, c["contiguous"].as_bool().unwrap_or(true),
                c["all_layers"].as_bool().unwrap_or(false), c["opacity"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32);
            if !ok {
                return Err("nothing was filled".into());
            }
            Ok(changed(doc))
        }
        "stroke" => {
            editable(doc)?;
            let pts = points(&c["points"])?;
            let first = *pts.first().ok_or("stroke needs at least one point")?;
            let f = |k: &str, d: f64, lo: f64, hi: f64| c[k].as_f64().unwrap_or(d).clamp(lo, hi) as f32;
            let tool = c["tool"].as_str().unwrap_or("brush");
            let paint = || if c["color"].is_null() { Ok([0, 0, 0, 255]) } else { color(&c["color"]) };
            let kind = match tool {
                "brush" => PaintKind::Brush(paint()?),
                "pencil" => PaintKind::Pencil(paint()?),
                "eraser" => PaintKind::Eraser,
                "smudge" => PaintKind::Smudge { strength: f("strength", 0.5, 0.0, 1.0) },
                "clone" => {
                    let src = point(&c["source"]).map_err(|_| "clone needs \"source\": the point the first stroke point copies from")?;
                    PaintKind::Clone { dx: (src.0 - first.0).round() as i32, dy: (src.1 - first.1).round() as i32 }
                }
                other => return Err(format!("tool is brush, pencil, eraser, smudge or clone, not {other}")),
            };
            let p = BrushParams { size: f("size", 24.0, 1.0, 2000.0), hardness: f("hardness", 0.8, 0.0, 1.0), opacity: f("opacity", 1.0, 0.0, 1.0), flow: f("flow", 1.0, 0.01, 1.0), spacing: f("spacing", 0.15, 0.02, 2.0) };
            let mut stroke = Stroke::begin(doc, kind, p, first).ok_or("this layer can't be painted on (hidden, locked, or too large to grow to the canvas)")?;
            for q in &pts[1..] {
                stroke.line_to(doc, *q);
            }
            stroke.finish(doc);
            Ok(changed(doc))
        }
        "gradient" => {
            editable(doc)?;
            let (a, b) = (point(&c["from"])?, point(&c["to"])?);
            let colors = c["colors"].as_array().filter(|a| a.len() == 2).ok_or("gradient needs \"colors\": two of them")?;
            let p = GradientParams {
                shape: match c["shape"].as_str().unwrap_or("linear") {
                    "linear" => GradientShape::Linear,
                    "radial" => GradientShape::Radial,
                    other => return Err(format!("shape is linear or radial, not {other}")),
                },
                from: color(&colors[0])?,
                to: color(&colors[1])?,
                opacity: c["opacity"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32,
            };
            let mut op = GradientOp::begin(doc).ok_or("this layer can't be painted on")?;
            op.update(doc, a, b, &p);
            op.finish(doc);
            Ok(changed(doc))
        }
        "copy" | "cut" => {
            let clip = if op == "copy" { doc.copy() } else { doc.cut() }.ok_or("nothing to copy: the selection doesn't touch this layer")?;
            let out = json!({"copied": {"x": clip.x, "y": clip.y, "width": clip.pixels.w, "height": clip.pixels.h}});
            scratch.clip = Some(clip);
            Ok(out)
        }
        "paste" => {
            let clip = scratch.clip.as_ref().ok_or("nothing has been copied")?;
            doc.check_layer_capacity(clip.pixels.w, clip.pixels.h).map_err(|e| e.to_string())?;
            Ok(json!({"layer": doc.paste(clip)}))
        }

        // ---- filters ----
        "filter" => {
            editable(doc)?;
            let f = parse_filter(doc, c)?;
            if f.needs_selection() && doc.state.selection.is_none() {
                return Err("select what to fill in first".into());
            }
            if !filter::apply_filter(doc, &f) {
                return Err("this layer is hidden or locked".into());
            }
            Ok(changed(doc))
        }
        "adjust" => {
            let f = |k: &str| c[k].as_f64().unwrap_or(0.0) as f32;
            let (brightness, contrast, saturation, temperature) = (f("brightness").clamp(-1.0, 1.0), f("contrast").clamp(-1.0, 1.0), f("saturation").clamp(-1.0, 1.0), f("temperature").clamp(-1.0, 1.0));
            let (sin, cos) = f("hue").to_radians().sin_cos();
            // Contrast pivots on mid grey; +1 is a steep but finite slope.
            let slope = if contrast >= 0.0 { 1.0 + contrast * 3.0 } else { 1.0 + contrast };
            map_pixels(doc, "Adjust", None, |p| {
                let mut v = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
                if sin != 0.0 || cos != 1.0 {
                    // Rotate the two chroma axes of YIQ; luma is untouched.
                    let y = 0.299 * v[0] + 0.587 * v[1] + 0.114 * v[2];
                    let i = 0.596 * v[0] - 0.274 * v[1] - 0.322 * v[2];
                    let q = 0.211 * v[0] - 0.523 * v[1] + 0.312 * v[2];
                    let (i, q) = (i * cos - q * sin, i * sin + q * cos);
                    v = [y + 0.956 * i + 0.621 * q, y - 0.272 * i - 0.647 * q, y - 1.106 * i + 1.703 * q];
                }
                let y = 0.299 * v[0] + 0.587 * v[1] + 0.114 * v[2];
                for (k, ch) in v.iter_mut().enumerate() {
                    *ch = y + (*ch - y) * (1.0 + saturation);
                    *ch = (*ch - 0.5) * slope + 0.5 + brightness;
                    *ch += temperature * [0.12, 0.0, -0.12][k];
                }
                [(v[0] * 255.0).round().clamp(0.0, 255.0) as u8, (v[1] * 255.0).round().clamp(0.0, 255.0) as u8, (v[2] * 255.0).round().clamp(0.0, 255.0) as u8, p[3]]
            })
        }
        "red_eye" => {
            let area = if c["rect"].is_null() { None } else { Some(rect(&c["rect"])?) };
            if area.is_none() && doc.state.selection.is_none() {
                return Err("red_eye needs a rect around the eye, or a selection".into());
            }
            let darken = 1.0 - c["darken"].as_f64().unwrap_or(0.3).clamp(0.0, 1.0) as f32;
            let mut fixed = 0u32;
            let mut out = map_pixels(doc, "Red Eye", area, |p| {
                let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
                let rest = (g + b) / 2.0;
                // Flash red-eye: clearly red, and far redder than it is green or blue.
                if p[3] == 0 || r < 70.0 || r < rest * 1.6 {
                    return p;
                }
                fixed += 1;
                [(rest * darken) as u8, (g * darken) as u8, (b * darken) as u8, p[3]]
            })?;
            out["pixels_fixed"] = json!(fixed);
            Ok(out)
        }

        // ---- text ----
        "add_text" => {
            let mut spec = TextSpec { text: c["text"].as_str().ok_or("add_text needs \"text\"")?.to_owned(), ..TextSpec::default() };
            text_fields(&mut spec, c)?;
            let (mut origin, path) = match &c["path"] {
                J::Null => ((0, 0), None),
                J::String(s) if s == "selection" => {
                    let sel = doc.state.selection.clone().ok_or("select a shape for the text to follow first")?;
                    let outline = segment::contours(&sel).into_iter().next().ok_or("the selection has no outline")?;
                    // Begin at the leftmost point so the text starts up the left side and runs over the top.
                    let start = (0..outline.len()).min_by(|a, b| outline[*a].0.total_cmp(&outline[*b].0)).unwrap_or(0);
                    let pts: Vec<(f32, f32)> = outline[start..].iter().chain(&outline[..=start]).copied().collect();
                    let pts = text::smooth_path(&pts, 1.5);
                    ((pts[0].0.round() as i32, pts[0].1.round() as i32), Some(pts))
                }
                other => {
                    let pts = points(other)?;
                    if pts.len() < 2 {
                        return Err("a path needs at least two points".into());
                    }
                    ((pts[0].0.round() as i32, pts[0].1.round() as i32), Some(pts))
                }
            };
            match path {
                Some(pts) => spec.path = pts.iter().map(|p| (p.0 - origin.0 as f32, p.1 - origin.1 as f32)).collect(),
                None => origin = (int(c, "x").unwrap_or(doc.state.width as i32 / 10), int(c, "y").unwrap_or(doc.state.height as i32 / 2)),
            }
            io::limits::validate_text(&spec).map_err(|e| e.to_string())?;
            let (data, index) = host.font(&spec.font, spec.bold, spec.italic);
            let font = FontRef::try_from_slice_and_index(&data, index).map_err(|_| "that font could not be read")?;
            let id = doc.try_add_text_layer(spec, origin, &font).map_err(|e| e.to_string())?;
            layer_info(doc, id)
        }
        "set_text" => {
            let l = editable(doc)?;
            let (old, origin) = (l.text.clone().ok_or("that layer is not a text layer (painting on or filtering text turns it into pixels)")?, l.text_origin().unwrap());
            let mut spec = (*old).clone();
            if let Some(t) = c["text"].as_str() {
                spec.text = t.to_owned();
            }
            text_fields(&mut spec, c)?;
            match &c["path"] {
                J::Null => {}
                J::String(s) if s == "none" => spec.path.clear(),
                other => spec.path = points(other)?.iter().map(|p| (p.0 - origin.0 as f32, p.1 - origin.1 as f32)).collect(),
            }
            io::limits::validate_text(&spec).map_err(|e| e.to_string())?;
            let (data, index) = host.font(&spec.font, spec.bold, spec.italic);
            let font = FontRef::try_from_slice_and_index(&data, index).map_err(|_| "that font could not be read")?;
            let before = doc.begin();
            if !doc.update_text(active, spec, &font) {
                doc.state = before;
                return Err("the text could not be laid out (too large, or it would exceed the document's limits)".into());
            }
            if let (Some(x), Some(y)) = (int(c, "x"), int(c, "y")) {
                let l = doc.state.active_layer_mut().unwrap();
                let (dx, dy) = (x - origin.0, y - origin.1);
                if io::limits::validate_offset(l.x.saturating_add(dx), l.y.saturating_add(dy)).is_ok() {
                    l.x += dx;
                    l.y += dy;
                    l.touch();
                }
                doc.mark_all_dirty();
            }
            doc.commit("Edit Text", before);
            layer_info(doc, active)
        }
        "ai_edit" => Err("AI edits are made by the app or the MCP server, which hold the key; this host does not offer them".into()),
        other => Err(format!("unknown op '{other}'; get_reference lists them")),
    }
}

// ------------------------------------------------------------------ helpers

fn int(c: &J, k: &str) -> Option<i32> {
    c[k].as_f64().filter(|v| v.is_finite()).map(|v| v.round().clamp(-1e9, 1e9) as i32)
}

fn uint(c: &J, k: &str) -> Option<u32> {
    c[k].as_f64().filter(|v| v.is_finite() && *v >= 0.0).map(|v| v.round().min(u32::MAX as f64) as u32)
}

fn path<'a>(c: &'a J, k: &str) -> R<&'a Path> {
    let p = Path::new(c[k].as_str().ok_or_else(|| format!("the command needs \"{k}\""))?);
    absolute(p)?;
    Ok(p)
}

fn absolute(p: &Path) -> R<()> {
    if p.is_absolute() { Ok(()) } else { Err(format!("give an absolute path, not {}", p.display())) }
}

fn point(v: &J) -> R<(f32, f32)> {
    match v.as_array().map(|a| a.iter().map(J::as_f64).collect::<Option<Vec<f64>>>()) {
        Some(Some(a)) if a.len() == 2 && a.iter().all(|v| v.is_finite()) => Ok((a[0] as f32, a[1] as f32)),
        _ => Err(format!("expected a point [x,y], got {v}")),
    }
}

fn points(v: &J) -> R<Vec<(f32, f32)>> {
    let a = v.as_array().ok_or("expected a list of points [[x,y],...]")?;
    if a.len() > 20_000 {
        return Err("too many points (the limit is 20000)".into());
    }
    a.iter().map(point).collect()
}

fn rect(v: &J) -> R<IRect> {
    match v.as_array().map(|a| a.iter().map(J::as_f64).collect::<Option<Vec<f64>>>()) {
        Some(Some(a)) if a.len() == 4 && a.iter().all(|v| v.is_finite()) => {
            let r = a.iter().map(|v| v.round().clamp(-1e6, 1e6) as i32).collect::<Vec<_>>();
            if r[2] <= r[0] || r[3] <= r[1] {
                return Err(format!("the rectangle {v} is empty; it is [x0,y0,x1,y1] with x1 > x0 and y1 > y0"));
            }
            Ok(IRect::new(r[0], r[1], r[2], r[3]))
        }
        _ => Err(format!("expected a rectangle [x0,y0,x1,y1], got {v}")),
    }
}

fn rect_json(r: IRect) -> J {
    json!([r.x0, r.y0, r.x1, r.y1])
}

pub fn color(v: &J) -> R<[u8; 4]> {
    match v {
        J::String(s) => {
            let hex = s.trim_start_matches('#');
            let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("x"), 16).map_err(|_| format!("{s} is not a colour like #rrggbb"));
            match hex.len() {
                6 => Ok([byte(0)?, byte(2)?, byte(4)?, 255]),
                8 => Ok([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
                _ => Err(format!("{s} is not a colour like #rrggbb")),
            }
        }
        J::Array(a) if a.len() == 3 || a.len() == 4 => {
            let ch = |i: usize| a.get(i).map_or(Some(255.0), J::as_f64).filter(|v| (0.0..=255.0).contains(v)).map(|v| v.round() as u8).ok_or_else(|| format!("colour channels are 0-255 in {v}"));
            Ok([ch(0)?, ch(1)?, ch(2)?, ch(3)?])
        }
        J::Null => Err("the command needs a colour".into()),
        _ => Err(format!("{v} is not a colour; use \"#rrggbb\" or [r,g,b]")),
    }
}

fn layer_id(doc: &Document, v: &J) -> R<LayerId> {
    let found = match v {
        J::Number(n) => n.as_u64().filter(|id| doc.state.layer(*id).is_some()),
        J::String(s) => doc.state.layers.iter().rev().find(|l| l.name == *s).map(|l| l.id),
        _ => None,
    };
    found.ok_or_else(|| format!("there is no layer {v}; get_document_info lists them"))
}

/// The active layer, if tools may change it.
fn editable(doc: &Document) -> R<&Layer> {
    let l = doc.state.active_layer().ok_or("there is no active layer")?;
    if l.locked {
        return Err(format!("layer \"{}\" is locked; set_layer with \"locked\":false first", l.name));
    }
    if !l.visible {
        return Err(format!("layer \"{}\" is hidden; set_layer with \"visible\":true first", l.name));
    }
    Ok(l)
}

fn target_name(doc: &Document) -> &'static str {
    if doc.effective_target() == Target::Mask { "mask" } else { "pixels" }
}

fn blend_mode(s: &str) -> R<BlendMode> {
    let key = |n: &str| n.to_lowercase().replace([' ', '_'], "-");
    BlendMode::ALL.into_iter().find(|b| key(b.name()) == key(s)).ok_or_else(|| format!("unknown blend mode {s}"))
}

fn selection_info(doc: &Document) -> J {
    match doc.state.selection.as_deref() {
        Some(m) => match selection::bounds(m) {
            Some(b) => json!({"selection": {"bounds": rect_json(b), "pixels": m.data.iter().filter(|v| **v > 127).count()}}),
            None => json!({"selection": null}),
        },
        None => json!({"selection": null}),
    }
}

fn layer_summary(doc: &Document, l: &Layer, index: usize) -> J {
    let r = l.rect();
    let mut v = json!({
        "id": l.id, "index": index, "name": l.name, "rect": rect_json(r), "visible": l.visible, "locked": l.locked,
        "opacity": l.opacity, "blend": l.blend.name().to_lowercase().replace(' ', "-"), "mask": l.mask.is_some(), "active": l.id == doc.state.active,
    });
    if let Some(t) = &l.text {
        v["text"] = json!(t.text);
    }
    v
}

pub fn document_info(doc: &Document) -> J {
    let (names, pos) = doc.history();
    let names: Vec<&str> = names.collect();
    let shown = names.len().saturating_sub(30);
    let mut v = json!({
        "width": doc.state.width, "height": doc.state.height,
        "file": doc.path.as_ref().map(|p| p.display().to_string()),
        "unsaved_changes": doc.modified,
        "active_layer": doc.state.active, "target": target_name(doc),
        "layers_bottom_first": doc.state.layers.iter().enumerate().map(|(i, l)| layer_summary(doc, l, i)).collect::<Vec<_>>(),
        "history": {"steps": &names[shown..], "applied": pos, "total": names.len()},
    });
    v["selection"] = selection_info(doc)["selection"].take();
    v
}

fn layer_info(doc: &Document, id: LayerId) -> R<J> {
    let i = doc.state.index_of(id).ok_or("no such layer")?;
    let l = &doc.state.layers[i];
    let mut v = layer_summary(doc, l, i);
    // Where there is actually something to see, which is often much less than the layer's frame.
    let (w, mut b) = (l.pixels.w as usize, None::<IRect>);
    for (y, row) in l.pixels.data.chunks_exact((w * 4).max(4)).enumerate() {
        let Some(x0) = row.chunks_exact(4).position(|p| p[3] != 0) else { continue };
        let x1 = row.chunks_exact(4).rposition(|p| p[3] != 0).unwrap();
        let line = IRect::new(x0 as i32, y as i32, x1 as i32 + 1, y as i32 + 1);
        b = Some(b.map_or(line, |b| b.union(line)));
    }
    v["painted_bounds"] = b.map_or(J::Null, |b| rect_json(b.translate(l.x, l.y)));
    if let (Some(t), Some(o)) = (&l.text, l.text_origin()) {
        v["text"] = json!({
            "text": t.text, "x": o.0, "y": o.1, "font": t.font, "bold": t.bold, "italic": t.italic, "size": t.size,
            "color": t.color, "align": t.align.name(), "tracking": t.tracking, "leading": t.leading, "follows_path": !t.path.is_empty(),
        });
    }
    Ok(v)
}

/// A short confirmation that names what is now active.
fn changed(doc: &Document) -> J {
    let l = doc.state.active_layer();
    json!({"ok": true, "layer": doc.state.active, "rect": l.map(|l| rect_json(l.rect()))})
}

fn text_fields(spec: &mut TextSpec, c: &J) -> R<()> {
    if let Some(f) = c["font"].as_str() {
        spec.font = f.to_owned();
    }
    if let Some(v) = c["bold"].as_bool() {
        spec.bold = v;
    }
    if let Some(v) = c["italic"].as_bool() {
        spec.italic = v;
    }
    if let Some(v) = c["size"].as_f64() {
        spec.size = (v as f32).clamp(2.0, 4000.0);
    }
    if !c["color"].is_null() {
        spec.color = color(&c["color"])?;
    }
    if let Some(a) = c["align"].as_str() {
        if !matches!(a, "left" | "center" | "right") {
            return Err(format!("align is left, center or right, not {a}"));
        }
        spec.align = Align::from_name(a);
    }
    if let Some(v) = c["tracking"].as_f64() {
        spec.tracking = (v as f32).clamp(-500.0, 2000.0);
    }
    if let Some(v) = c["leading"].as_f64() {
        spec.leading = (v as f32).clamp(0.2, 10.0);
    }
    if let Some(v) = c["path_offset"].as_f64() {
        spec.path_offset = v as f32;
    }
    Ok(())
}

fn parse_filter(doc: &Document, c: &J) -> R<Filter> {
    let f = |k: &str, d: f32, lo: f32, hi: f32| c[k].as_f64().map_or(d, |v| v as f32).clamp(lo, hi);
    Ok(match c["name"].as_str().ok_or("filter needs \"name\"")? {
        "gaussian_blur" => Filter::GaussianBlur { radius: f("radius", 4.0, 0.1, 500.0) },
        "sharpen" => Filter::UnsharpMask { radius: f("radius", 1.5, 0.2, 50.0), amount: f("amount", 1.0, 0.0, 5.0), threshold: f("threshold", 0.0, 0.0, 50.0) },
        "surface_blur" => Filter::SurfaceBlur { radius: f("radius", 4.0, 0.5, 20.0), tolerance: f("tolerance", 25.0, 2.0, 120.0) },
        "reduce_noise" => Filter::Denoise { strength: f("strength", 12.0, 1.0, 80.0) },
        "heal" => Filter::Inpaint { radius: f("radius", 6.0, 2.0, 24.0) },
        "auto_contrast" => Filter::AutoContrast { clip: f("clip", 0.5, 0.0, 5.0) },
        "equalize" => Filter::Equalize { amount: f("amount", 1.0, 0.0, 1.0) },
        "local_contrast" => Filter::LocalContrast { clip: f("clip", 2.5, 1.0, 8.0), tiles: f("tiles", 8.0, 2.0, 16.0) as u32 },
        "threshold" => Filter::Threshold { level: f("level", 128.0, 1.0, 254.0) },
        "adaptive_threshold" => Filter::AdaptiveThreshold { radius: f("radius", 20.0, 2.0, 100.0), offset: f("offset", 6.0, -40.0, 40.0) },
        "lens_distortion" => Filter::LensDistortion { amount: f("amount", 0.15, -0.6, 0.6) },
        "match_colors" => {
            let stats = match &c["reference"] {
                J::String(s) if Path::new(s).is_absolute() => fx::color_stats(&io::load_pixmap(Path::new(s)).map_err(|e| format!("could not open {s}: {e}"))?),
                J::Null => return Err("match_colors needs \"reference\": a layer or an absolute image path".into()),
                other => fx::color_stats(&doc.state.layer(layer_id(doc, other)?).unwrap().pixels),
            };
            let (mean, dev) = stats.ok_or("the reference has nothing visible to take colours from")?;
            Filter::MatchColors { mean, dev, amount: f("amount", 1.0, 0.0, 1.0) }
        }
        "edge_detect" => Filter::EdgeDetect(EdgeParams {
            threshold: f("threshold", 40.0, 1.0, 255.0),
            smoothing: f("smoothing", 1.4, 0.0, 20.0),
            thickness: f("thickness", 1.0, 1.0, 20.0),
            style: match c["style"].as_str().unwrap_or("lines") {
                "lines" => EdgeStyle::Lines,
                "on_black" => EdgeStyle::LinesOnBlack,
                "highlight" => EdgeStyle::Highlight,
                other => return Err(format!("style is lines, on_black or highlight, not {other}")),
            },
            color: if c["color"].is_null() { EdgeParams::default().color } else { color(&c["color"])? },
        }),
        other => return Err(format!("unknown filter '{other}'; get_reference lists them")),
    })
}

/// Canvas-sized mask of pixels within `tolerance` of a colour.
fn color_mask(doc: &Document, c: &J) -> R<Mask> {
    let want = color(&c["color"])?;
    let tolerance = uint(c, "tolerance").unwrap_or(40).min(255) as i32;
    let src = fill::sample_source(&doc.state, c["all_layers"].as_bool().unwrap_or(true)).ok_or("there is no layer to look at")?;
    let area = if c["rect"].is_null() { src.rect() } else { rect(&c["rect"])?.intersect(src.rect()) };
    let mut out = Mask::new(src.w, src.h);
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let p = src.px(x, y);
            if p[3] > 0 && (0..3).all(|k| (p[k] as i32 - want[k] as i32).abs() <= tolerance) {
                out.set(x, y, [255]);
            }
        }
    }
    Ok(out)
}

/// Run `f` over the active layer's pixels inside the selection (and `area`,
/// if given), blending by the selection's coverage. One undo step.
fn map_pixels(doc: &mut Document, name: &str, area: Option<IRect>, mut f: impl FnMut([u8; 4]) -> [u8; 4]) -> R<J> {
    editable(doc)?;
    if doc.effective_target() == Target::Mask {
        return Err("this works on a layer's pixels; use \"target\":\"pixels\"".into());
    }
    let before = doc.begin();
    let sel = doc.state.selection.clone();
    let canvas = doc.canvas();
    let layer = doc.state.active_layer_mut().unwrap();
    let (ox, oy, id) = (layer.x, layer.y, layer.id);
    let mut region = layer.rect().intersect(canvas);
    if let Some(a) = area {
        region = region.intersect(a);
    }
    if let Some(b) = sel.as_deref().and_then(selection::bounds) {
        region = region.intersect(b);
    }
    if region.is_empty() {
        return Err("that area doesn't touch the layer".into());
    }
    layer.text = None;
    let px = Arc::make_mut(&mut layer.pixels);
    for y in region.y0..region.y1 {
        for x in region.x0..region.x1 {
            let cover = sel.as_ref().map_or(255, |m| m.px(x, y)[0]) as u32;
            if cover == 0 {
                continue;
            }
            let old = px.px(x - ox, y - oy);
            let new = f(old);
            let mix = |a: u8, b: u8| ((a as u32 * (255 - cover) + b as u32 * cover + 127) / 255) as u8;
            px.set(x - ox, y - oy, [mix(old[0], new[0]), mix(old[1], new[1]), mix(old[2], new[2]), mix(old[3], new[3])]);
        }
    }
    layer.touch();
    doc.mark_dirty(region);
    doc.commit_patch(name, before, id, Target::Pixels, region.translate(-ox, -oy));
    Ok(json!({"ok": true, "layer": id, "changed_rect": rect_json(region)}))
}

fn resize_layer(doc: &mut Document, c: &J) -> R<J> {
    let l = editable(doc)?;
    let (ow, oh) = (l.pixels.w, l.pixels.h);
    let (w, h) = match (c["scale"].as_f64(), uint(c, "width"), uint(c, "height")) {
        (Some(s), _, _) if s > 0.0 => (((ow as f64 * s).round() as u32).max(1), ((oh as f64 * s).round() as u32).max(1)),
        (None, Some(w), Some(h)) => (w, h),
        (None, Some(w), None) => (w, ((oh as f64 * w as f64 / ow as f64).round() as u32).max(1)),
        (None, None, Some(h)) => (((ow as f64 * h as f64 / oh as f64).round() as u32).max(1), h),
        _ => return Err("resize_layer needs a positive \"scale\", or \"width\" and/or \"height\"".into()),
    };
    if w == 0 || h == 0 {
        return Err("width and height must be at least 1".into());
    }
    let id = l.id;
    doc.check_layer_resize(id, w, h).map_err(|e| e.to_string())?;
    let (x, y) = (l.x + (ow as i32 - w as i32) / 2, l.y + (oh as i32 - h as i32) / 2);
    io::limits::validate_offset(x, y).map_err(|e| e.to_string())?;
    let px = resize(&l.pixels, w, h, FilterType::Lanczos3);
    let mask = l.mask.as_ref().map(|m| {
        let gray = image::GrayImage::from_raw(m.w, m.h, m.data.clone()).expect("buffer size");
        Mask::from_raw(w, h, image::imageops::resize(&gray, w, h, FilterType::Triangle).into_raw())
    });
    let before = doc.begin();
    let l = doc.state.active_layer_mut().unwrap();
    l.pixels = Arc::new(px);
    l.mask = mask.map(Arc::new);
    l.text = None;
    (l.x, l.y) = (x, y);
    l.touch();
    doc.mark_all_dirty();
    doc.commit("Resize Layer", before);
    Ok(changed(doc))
}

/// A picture of the canvas (or part of it) for a client to look at.
fn screenshot(doc: &Document, c: &J) -> R<J> {
    let region = if c["region"].is_null() { doc.canvas() } else { rect(&c["region"])?.intersect(doc.canvas()) };
    if region.is_empty() {
        return Err("that region is outside the canvas".into());
    }
    let max = uint(c, "max_size").unwrap_or(1024).clamp(64, 2048) as f32;
    let mut px = Pixmap::new(region.width() as u32, region.height() as u32);
    if c["layer"].is_null() {
        composite::composite_rect(&doc.state, region, &mut px.data);
    } else {
        let l = doc.state.layer(layer_id(doc, &c["layer"])?).unwrap();
        let mut only = l.clone();
        (only.visible, only.opacity, only.blend) = (true, 1.0, BlendMode::Normal);
        let alone = crate::document::DocState { layers: vec![only], selection: None, ..doc.state };
        composite::composite_rect(&alone, region, &mut px.data);
    }
    // Small regions are enlarged with hard pixels so details can be made out.
    let longest = region.width().max(region.height()) as f32;
    let scale = (max / longest).min(8.0);
    let (w, h) = (((px.w as f32 * scale).round() as u32).max(1), ((px.h as f32 * scale).round() as u32).max(1));
    let mut out = resize(&px, w, h, if scale > 1.0 { FilterType::Nearest } else { FilterType::CatmullRom });
    let overlay = c["show_selection"].as_bool().unwrap_or(true);
    let sel = doc.state.selection.as_deref().filter(|_| overlay);
    for y in 0..out.h {
        for x in 0..out.w {
            let i = (y * out.w + x) as usize * 4;
            let p = &mut out.data[i..i + 4];
            // Checkerboard under transparency.
            let bg = if (x / 12 + y / 12) % 2 == 0 { 236.0 } else { 204.0 };
            let a = p[3] as f32 / 255.0;
            let mut rgb = [0, 1, 2].map(|k| p[k] as f32 * a + bg * (1.0 - a));
            if let Some(m) = sel {
                // Dim and cool what is not selected.
                let (dx, dy) = (region.x0 + (x as f32 / scale) as i32, region.y0 + (y as f32 / scale) as i32);
                let cover = m.get(dx, dy).map_or(0.0, |v| v[0] as f32 / 255.0);
                let dim = 0.42 + 0.58 * cover;
                rgb = [rgb[0] * dim, rgb[1] * dim, rgb[2] * dim + (1.0 - cover) * 28.0];
            }
            p.copy_from_slice(&[rgb[0].min(255.0) as u8, rgb[1].min(255.0) as u8, rgb[2].min(255.0) as u8, 255]);
        }
    }
    let png = io::encode_png(&out).map_err(|e| e.to_string())?;
    Ok(json!({
        "png_base64": base64::engine::general_purpose::STANDARD.encode(png),
        "width": out.w, "height": out.h, "region": rect_json(region), "scale": scale,
        "note": if sel.is_some() { "The selection is shown at full brightness; everything else is dimmed and tinted blue." } else { "" },
    }))
}
