//! OpenRaster reader/writer.
//!
//! Layer masks aren't part of the OpenRaster spec, so they're stored as extra
//! PNGs referenced by `pixelferrite-mask` attributes that other apps ignore.

use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::sync::Arc;

use image::{GrayImage, ImageFormat, RgbaImage};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::{Error, Result};
use crate::blend::BlendMode;
use crate::buf::{Mask, Pixmap};
use crate::composite;
use crate::document::{DocState, Layer};
use crate::text::{Align, TextSpec};

fn png_rgba(p: &Pixmap) -> Result<Vec<u8>> {
    let img = RgbaImage::from_raw(p.w, p.h, p.data.clone()).expect("buffer size");
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

fn png_gray(m: &Mask) -> Result<Vec<u8>> {
    let img = GrayImage::from_raw(m.w, m.h, m.data.clone()).expect("buffer size");
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\n', "&#10;").replace('\t', "&#9;").replace('\r', "")
}

/// Attributes that keep a text layer editable. Other apps ignore them and
/// just show the rendered pixels.
fn text_attrs(t: &TextSpec) -> String {
    let path: Vec<String> = t.path.iter().map(|p| format!("{},{}", p.0, p.1)).collect();
    let c = t.color;
    format!(
        " pixelferrite-text=\"{}\" pixelferrite-text-font=\"{}\" pixelferrite-text-style=\"{}{}\" pixelferrite-text-size=\"{}\" pixelferrite-text-color=\"{:02x}{:02x}{:02x}{:02x}\" pixelferrite-text-align=\"{}\" pixelferrite-text-tracking=\"{}\" pixelferrite-text-leading=\"{}\" pixelferrite-text-path=\"{}\" pixelferrite-text-offset=\"{}\" pixelferrite-text-raster=\"{},{}\"",
        esc(&t.text),
        esc(&t.font),
        if t.bold { "bold " } else { "" },
        if t.italic { "italic" } else { "" },
        t.size,
        c[0],
        c[1],
        c[2],
        c[3],
        t.align.name(),
        t.tracking,
        t.leading,
        path.join(" "),
        t.path_offset,
        t.raster.0,
        t.raster.1,
    )
}

fn parse_text(node: &roxmltree::Node<'_, '_>) -> Option<TextSpec> {
    let attr = |k: &str| node.attribute(format!("pixelferrite-text{k}").as_str());
    let num = |k: &str| attr(k).and_then(|v| v.parse::<f32>().ok());
    let pair = |s: &str| {
        let (a, b) = s.split_once(',')?;
        Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))
    };
    let text = attr("")?.to_owned();
    let style = attr("-style").unwrap_or("");
    let hex = attr("-color").unwrap_or("000000ff");
    let color: [u8; 4] = std::array::from_fn(|i| hex.get(i * 2..i * 2 + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(255));
    let raster = attr("-raster").and_then(pair)?;
    Some(TextSpec {
        text,
        font: attr("-font").unwrap_or("").to_owned(),
        bold: style.contains("bold"),
        italic: style.contains("italic"),
        size: num("-size").unwrap_or(72.0),
        color,
        align: Align::from_name(attr("-align").unwrap_or("")),
        tracking: num("-tracking").unwrap_or(0.0),
        leading: num("-leading").unwrap_or(1.2),
        path: attr("-path").unwrap_or("").split_whitespace().filter_map(pair).collect(),
        path_offset: num("-offset").unwrap_or(0.0),
        raster: (raster.0 as i32, raster.1 as i32),
    })
}

pub fn save(state: &DocState, path: &Path) -> Result<()> {
    // Write to a sibling temp file first so a failed save can't destroy the old one.
    let tmp = path.with_extension("ora.tmp");
    let mut zip = ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(&tmp)?));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    // The spec requires `mimetype` to be the first entry, uncompressed.
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"image/openraster")?;

    let mut xml = format!(
        "<?xml version='1.0' encoding='UTF-8'?>\n<image version=\"0.0.3\" w=\"{}\" h=\"{}\" xres=\"72\" yres=\"72\">\n  <stack>\n",
        state.width, state.height
    );
    // stack.xml lists layers top to bottom.
    for (i, l) in state.layers.iter().enumerate().rev() {
        let src = format!("data/layer{i}.png");
        zip.start_file(&src, stored)?;
        zip.write_all(&png_rgba(&l.pixels)?)?;
        let mut extra = String::new();
        if let Some(m) = &l.mask {
            let msrc = format!("data/layer{i}_mask.png");
            zip.start_file(&msrc, stored)?;
            zip.write_all(&png_gray(m)?)?;
            extra = format!(" pixelferrite-mask=\"{msrc}\" pixelferrite-mask-enabled=\"{}\"", l.mask_enabled);
        }
        if let Some(t) = &l.text {
            extra += &text_attrs(t);
        }
        xml += &format!(
            "    <layer name=\"{}\" src=\"{src}\" x=\"{}\" y=\"{}\" opacity=\"{}\" visibility=\"{}\" composite-op=\"{}\" edit-locked=\"{}\"{}{extra}/>\n",
            esc(&l.name),
            l.x,
            l.y,
            l.opacity,
            if l.visible { "visible" } else { "hidden" },
            l.blend.ora_name(),
            l.locked,
            if l.id == state.active { " selected=\"true\"" } else { "" },
        );
    }
    xml += "  </stack>\n</image>\n";
    zip.start_file("stack.xml", SimpleFileOptions::default())?;
    zip.write_all(xml.as_bytes())?;

    let flat = composite::flatten(state);
    zip.start_file("mergedimage.png", stored)?;
    zip.write_all(&png_rgba(&flat)?)?;

    let img = RgbaImage::from_raw(flat.w, flat.h, flat.data).expect("buffer size");
    let thumb = image::imageops::thumbnail(&img, 256.min(flat.w.max(1)), 256.min(flat.h.max(1)));
    let mut buf = Cursor::new(Vec::new());
    thumb.write_to(&mut buf, ImageFormat::Png)?;
    zip.start_file("Thumbnails/thumbnail.png", stored)?;
    zip.write_all(&buf.into_inner())?;

    zip.finish()?.into_inner().map_err(|e| Error::Io(e.into_error()))?.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_entry<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    zip.by_name(name)?.read_to_end(&mut out)?;
    Ok(out)
}

pub fn load(path: &Path) -> Result<DocState> {
    let mut zip = ZipArchive::new(std::io::BufReader::new(std::fs::File::open(path)?))?;
    let xml = String::from_utf8(read_entry(&mut zip, "stack.xml")?)
        .map_err(|_| Error::Format("stack.xml is not UTF-8".into()))?;
    let doc = roxmltree::Document::parse(&xml)?;
    let root = doc.root_element();
    let dim = |k: &str| -> Result<u32> {
        root.attribute(k).and_then(|v| v.parse().ok()).ok_or_else(|| Error::Format(format!("missing image {k}")))
    };
    let (width, height) = (dim("w")?, dim("h")?);

    let mut layers = Vec::new();
    let mut active = 0;
    // Nested stacks (groups) are flattened into one list; document order is top to bottom.
    for node in root.descendants().filter(|n| n.has_tag_name("layer")) {
        let Some(src) = node.attribute("src") else { continue };
        let img = image::load_from_memory(&read_entry(&mut zip, src)?)?.into_rgba8();
        let (w, h) = img.dimensions();
        let num = |k: &str| node.attribute(k).and_then(|v| v.parse::<f32>().ok());
        let mut l = Layer::new(
            node.attribute("name").unwrap_or("Layer"),
            Pixmap::from_raw(w, h, img.into_raw()),
            num("x").unwrap_or(0.0) as i32,
            num("y").unwrap_or(0.0) as i32,
        );
        l.opacity = num("opacity").unwrap_or(1.0).clamp(0.0, 1.0);
        l.visible = node.attribute("visibility") != Some("hidden");
        l.locked = node.attribute("edit-locked") == Some("true");
        l.blend = BlendMode::from_ora_name(node.attribute("composite-op").unwrap_or(""));
        if let Some(msrc) = node.attribute("pixelferrite-mask") {
            let m = image::load_from_memory(&read_entry(&mut zip, msrc)?)?.into_luma8();
            if m.dimensions() == (w, h) {
                l.mask = Some(Arc::new(Mask::from_raw(w, h, m.into_raw())));
                l.mask_enabled = node.attribute("pixelferrite-mask-enabled") != Some("false");
            }
        }
        l.text = parse_text(&node).map(Arc::new);
        if node.attribute("selected") == Some("true") {
            active = l.id;
        }
        layers.push(l);
    }
    if layers.is_empty() {
        return Err(Error::Format("no layers in file".into()));
    }
    layers.reverse();
    if active == 0 {
        active = layers.last().unwrap().id;
    }
    Ok(DocState { width, height, layers, selection: None, active })
}
