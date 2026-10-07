//! OpenRaster reader/writer.
//!
//! Layer masks aren't part of the OpenRaster spec, so they're stored as extra
//! PNGs referenced by `pixelferrite-mask` attributes that other apps ignore.

use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use image::{GrayImage, ImageFormat, RgbaImage};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::{Error, Result, atomic, limits};
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
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\n', "&#10;")
        .replace('\t', "&#9;")
        .replace('\r', "")
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

fn parse_text(node: &roxmltree::Node<'_, '_>) -> Result<Option<TextSpec>> {
    let attr = |k: &str| node.attribute(format!("pixelferrite-text{k}").as_str());
    let Some(text) = attr("") else {
        return Ok(None);
    };
    let bad = || Error::Format("invalid OpenRaster editable text metadata".into());
    let num = |k: &str, default: f32| -> Result<f32> {
        attr(k).map_or(Ok(default), |v| v.parse::<f32>().map_err(|_| bad()))
    };
    let pair = |s: &str| -> Result<(f32, f32)> {
        let (a, b) = s.split_once(',').ok_or_else(bad)?;
        Ok((
            a.parse::<f32>().map_err(|_| bad())?,
            b.parse::<f32>().map_err(|_| bad())?,
        ))
    };
    let style = attr("-style").unwrap_or("");
    let hex = attr("-color").unwrap_or("000000ff");
    if hex.len() != 8 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let color: [u8; 4] = std::array::from_fn(|i| {
        hex.get(i * 2..i * 2 + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .unwrap_or(255)
    });
    let (rx, ry) = attr("-raster")
        .ok_or_else(bad)?
        .split_once(',')
        .ok_or_else(bad)?;
    let raster = (
        rx.parse::<i32>().map_err(|_| bad())?,
        ry.parse::<i32>().map_err(|_| bad())?,
    );
    let spec = TextSpec {
        text: text.to_owned(),
        font: attr("-font").unwrap_or("").to_owned(),
        bold: style.contains("bold"),
        italic: style.contains("italic"),
        size: num("-size", 72.0)?,
        color,
        align: Align::from_name(attr("-align").unwrap_or("")),
        tracking: num("-tracking", 0.0)?,
        leading: num("-leading", 1.2)?,
        path: attr("-path")
            .unwrap_or("")
            .split_whitespace()
            .map(pair)
            .collect::<Result<Vec<_>>>()?,
        path_offset: num("-offset", 0.0)?,
        raster,
    };
    limits::validate_text(&spec)?;
    Ok(Some(spec))
}

pub fn save(state: &DocState, path: &Path) -> Result<()> {
    save_with_privacy(state, path, false)
}

/// Save an application-owned recovery snapshot with private permissions from
/// the initial temporary-file creation, not only after encoding is complete.
pub fn save_private(state: &DocState, path: &Path) -> Result<()> {
    save_with_privacy(state, path, true)
}

fn save_with_privacy(state: &DocState, path: &Path, private: bool) -> Result<()> {
    limits::validate_document(state)?;
    // Exclusive creation prevents a preexisting sibling or symlink from being
    // overwritten, and Drop removes an incomplete temporary file on errors.
    let mut temporary = atomic::AtomicFile::new(path, private)?;
    let mut zip = ZipWriter::new(std::io::BufWriter::new(temporary.file()));
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
            extra = format!(
                " pixelferrite-mask=\"{msrc}\" pixelferrite-mask-enabled=\"{}\"",
                l.mask_enabled
            );
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
            if l.id == state.active {
                " selected=\"true\""
            } else {
                ""
            },
        );
        if xml.len() as u64 + 22 > limits::MAX_XML_BYTES {
            return Err(Error::Format(
                "document metadata exceeds the OpenRaster XML-size limit".into(),
            ));
        }
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

    zip.finish()?.flush()?;
    temporary.commit()?;
    Ok(())
}

/// Export standard ORA layers with enabled masks applied to their alpha. This
/// preserves their appearance in other editors, which ignore our mask metadata.
/// Native `save` retains editable masks and text; a compatible export rasterizes
/// text metadata and masks in the exported copy only.
pub fn export_compatible(state: &DocState, path: &Path) -> Result<()> {
    limits::validate_document(state)?;
    let mut exported = state.clone();
    for layer in &mut exported.layers {
        if let Some(mask) = &layer.mask {
            if layer.mask_enabled {
                for (pixel, coverage) in Arc::make_mut(&mut layer.pixels)
                    .data
                    .chunks_exact_mut(4)
                    .zip(&mask.data)
                {
                    pixel[3] = ((u32::from(pixel[3]) * u32::from(*coverage) + 127) / 255) as u8;
                }
            }
        }
        layer.mask = None;
        layer.text = None;
    }
    save(&exported, path)
}

fn read_entry<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
    max_bytes: u64,
    remaining: &mut u64,
) -> Result<Vec<u8>> {
    let mut entry = zip.by_name(name)?;
    let limit = max_bytes.min(*remaining);
    if entry.size() > limit {
        return Err(Error::Format(format!(
            "OpenRaster entry {name:?} exceeds the expanded-size budget"
        )));
    }
    let mut out = Vec::new();
    // Enforce the actual stream size as well as the untrusted ZIP header.
    entry.by_ref().take(limit + 1).read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        return Err(Error::Format(format!(
            "OpenRaster entry {name:?} exceeds the expanded-size budget"
        )));
    }
    *remaining -= out.len() as u64;
    Ok(out)
}

// Bound the central-directory allocation before the ZIP library creates its
// index. ZIP64 is unnecessary under our document/file limits and is rejected
// explicitly rather than allowing its much larger advertised entry counts.
fn preflight_archive(file: &mut std::fs::File) -> Result<()> {
    let len = file.metadata()?.len();
    if len > limits::MAX_ARCHIVE_BYTES {
        return Err(Error::Format(
            "OpenRaster archive exceeds the file-size limit".into(),
        ));
    }
    let tail_len = len.min(65_557) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            tail.get(i..i + 4) == Some(b"PK\x05\x06")
                && i + 22 + usize::from(u16::from_le_bytes([tail[i + 20], tail[i + 21]]))
                    == tail.len()
        })
        .ok_or_else(|| Error::Format("invalid OpenRaster ZIP directory".into()))?;
    let u16_at = |i: usize| u16::from_le_bytes([tail[end + i], tail[end + i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(tail[end + i..end + i + 4].try_into().unwrap());
    if u16_at(4) != 0 || u16_at(6) != 0 || u16_at(8) != u16_at(10) {
        return Err(Error::Format(
            "multi-volume OpenRaster archives are unsupported".into(),
        ));
    }
    if usize::from(u16_at(10)) > limits::MAX_ARCHIVE_ENTRIES
        || u64::from(u32_at(12)) > limits::MAX_CENTRAL_DIRECTORY_BYTES
        || u32_at(16) == u32::MAX
        || (end >= 20 && tail.get(end - 20..end - 16) == Some(b"PK\x06\x07"))
    {
        return Err(Error::Format(
            "OpenRaster ZIP directory exceeds supported limits (including ZIP64)".into(),
        ));
    }
    let directory_end = u64::from(u32_at(16)) + u64::from(u32_at(12));
    if directory_end != len - tail_len as u64 + end as u64 {
        return Err(Error::Format(
            "invalid OpenRaster ZIP directory bounds".into(),
        ));
    }
    file.rewind()?;
    Ok(())
}

pub fn load(path: &Path) -> Result<DocState> {
    let mut file = std::fs::File::open(path)?;
    preflight_archive(&mut file)?;
    let mut zip = ZipArchive::new(std::io::BufReader::new(file))?;
    if zip.len() > limits::MAX_ARCHIVE_ENTRIES {
        return Err(Error::Format(
            "OpenRaster archive contains too many entries".into(),
        ));
    }
    let mut expanded_remaining = limits::MAX_ARCHIVE_EXPANDED_BYTES;
    let xml = String::from_utf8(read_entry(
        &mut zip,
        "stack.xml",
        limits::MAX_XML_BYTES,
        &mut expanded_remaining,
    )?)
    .map_err(|_| Error::Format("stack.xml is not UTF-8".into()))?;
    let doc = roxmltree::Document::parse(&xml)?;
    let root = doc.root_element();
    if !root.has_tag_name("image") {
        return Err(Error::Format(
            "OpenRaster stack.xml must have an image root".into(),
        ));
    }
    let dim = |k: &str| -> Result<u32> {
        root.attribute(k)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| Error::Format(format!("missing image {k}")))
    };
    let (width, height) = (dim("w")?, dim("h")?);
    limits::validate_dimensions(width, height)?;

    // Flattening group descendants is not equivalent to rendering an isolated,
    // hidden, or partially opaque group. Reject until groups can be represented
    // faithfully rather than silently changing the picture and future saves.
    let stacks: Vec<_> = root
        .descendants()
        .filter(|n| n.has_tag_name("stack"))
        .collect();
    if stacks.len() != 1 || stacks[0].parent() != Some(root) {
        return Err(Error::Format("Grouped OpenRaster documents are not supported yet. Flatten groups in the source editor before opening; this file was not changed.".into()));
    }
    let stack = stacks[0];
    let nontrivial =
        |name: &str, default: &str| stack.attribute(name).is_some_and(|v| v != default);
    if nontrivial("visibility", "visible")
        || nontrivial("composite-op", "svg:src-over")
        || stack
            .attribute("opacity")
            .is_some_and(|v| v.parse::<f32>().ok() != Some(1.0))
        || ["x", "y"].iter().any(|k| {
            stack
                .attribute(*k)
                .is_some_and(|v| v.parse::<i32>().ok() != Some(0))
        })
    {
        return Err(Error::Format("OpenRaster stack effects are not supported yet. Flatten the stack in the source editor before opening; this file was not changed.".into()));
    }
    let nodes: Vec<_> = stack.children().filter(|n| n.is_element()).collect();
    if nodes.len() > limits::MAX_LAYERS {
        return Err(Error::Format(format!(
            "OpenRaster document exceeds the {} layer limit",
            limits::MAX_LAYERS
        )));
    }
    if nodes.iter().any(|n| !n.has_tag_name("layer")) {
        return Err(Error::Format(
            "OpenRaster stack contains unsupported elements".into(),
        ));
    }

    // Check the aggregate budget from image headers before decoding any layer.
    // In particular, many references to one small compressed image must not
    // evade the budget or allocate the entire allowance before being rejected.
    let entries_budget = expanded_remaining;
    let mut total_pixels = 0u64;
    for node in &nodes {
        let src = node
            .attribute("src")
            .ok_or_else(|| Error::Format("OpenRaster layer is missing its image source".into()))?;
        for name in std::iter::once(src).chain(node.attribute("pixelferrite-mask")) {
            let bytes = read_entry(
                &mut zip,
                name,
                limits::MAX_ENCODED_IMAGE_BYTES,
                &mut expanded_remaining,
            )?;
            let (w, h) = super::image_dimensions(&bytes)?;
            total_pixels += u64::from(w) * u64::from(h);
            if total_pixels > limits::MAX_DOCUMENT_PIXELS {
                return Err(Error::Format(
                    "document exceeds the total decoded layer and mask pixel budget".into(),
                ));
            }
        }
    }
    expanded_remaining = entries_budget;

    let mut layers = Vec::new();
    let mut active = 0;
    let mut pixels_remaining = limits::MAX_DOCUMENT_PIXELS;
    // The file lists layers top to bottom.
    for node in nodes {
        let src = node
            .attribute("src")
            .ok_or_else(|| Error::Format("OpenRaster layer is missing its image source".into()))?;
        let bytes = read_entry(
            &mut zip,
            src,
            limits::MAX_ENCODED_IMAGE_BYTES,
            &mut expanded_remaining,
        )?;
        let pixels = super::decode_image_with_budget(&bytes, pixels_remaining)?;
        let (w, h) = (pixels.w, pixels.h);
        pixels_remaining -= u64::from(w) * u64::from(h);
        let num = |k: &str| node.attribute(k).and_then(|v| v.parse::<f32>().ok());
        let offset = |k: &str| -> Result<i32> {
            node.attribute(k).map_or(Ok(0), |v| {
                v.parse::<i32>()
                    .map_err(|_| Error::Format(format!("invalid layer {k} coordinate")))
            })
        };
        let (x, y) = (offset("x")?, offset("y")?);
        limits::validate_offset(x, y)?;
        let mut l = Layer::new(node.attribute("name").unwrap_or("Layer"), pixels, x, y);
        l.opacity = match node.attribute("opacity") {
            Some(_) => num("opacity")
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| Error::Format("invalid layer opacity".into()))?,
            None => 1.0,
        };
        l.visible = node.attribute("visibility") != Some("hidden");
        l.locked = node.attribute("edit-locked") == Some("true");
        let blend = node.attribute("composite-op").unwrap_or("svg:src-over");
        l.blend = BlendMode::ALL
            .into_iter()
            .find(|m| m.ora_name() == blend)
            .ok_or_else(|| Error::Format(format!("unsupported OpenRaster blend mode {blend:?}")))?;
        if let Some(msrc) = node.attribute("pixelferrite-mask") {
            let bytes = read_entry(
                &mut zip,
                msrc,
                limits::MAX_ENCODED_IMAGE_BYTES,
                &mut expanded_remaining,
            )?;
            let m = super::decode_dynamic(&bytes, pixels_remaining)?.into_luma8();
            if m.dimensions() != (w, h) {
                return Err(Error::Format(
                    "OpenRaster mask dimensions do not match the layer".into(),
                ));
            }
            pixels_remaining -= u64::from(w) * u64::from(h);
            l.mask = Some(Arc::new(Mask::from_raw(w, h, m.into_raw())));
            l.mask_enabled = node.attribute("pixelferrite-mask-enabled") != Some("false");
        }
        l.text = parse_text(&node)?.map(Arc::new);
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
    let state = DocState {
        width,
        height,
        layers,
        selection: None,
        active,
    };
    limits::validate_document(&state)?;
    Ok(state)
}
