//! File formats. The native format is OpenRaster (`.ora`), an open layered
//! format also read by Krita, GIMP and MyPaint; flat images go through `image`.

use std::io::{BufRead, Cursor, Read, Seek, Write};
use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageFormat, RgbaImage};

use crate::buf::Pixmap;
use crate::composite;
use crate::document::{DocState, Document};

pub mod atomic;
pub mod limits;
pub mod ora;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Image(#[from] image::ImageError),
    #[error("{0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("{0}")]
    Xml(#[from] roxmltree::Error),
    #[error("{0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Extensions [`open`] understands.
pub const OPEN_EXTENSIONS: &[&str] = &[
    "ora", "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff",
];
/// Extensions [`export`] can write.
pub const EXPORT_EXTENSIONS: &[&str] = &["png", "jpg", "gif", "ora"];

fn ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn is_ora(path: &Path) -> bool {
    ext(path) == "ora"
}

/// Decode a flat image file.
pub fn load_pixmap(path: &Path) -> Result<Pixmap> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > limits::MAX_ENCODED_IMAGE_BYTES {
        return Err(Error::Format(
            "encoded image exceeds the file-size limit".into(),
        ));
    }
    // Bound reads as well as metadata; the file could change after opening.
    let mut bytes = Vec::new();
    file.take(limits::MAX_ENCODED_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    decode_image(&bytes)
}

/// Decode an image held in memory (any format [`load_pixmap`] reads).
pub fn decode_image(bytes: &[u8]) -> Result<Pixmap> {
    decode_image_with_budget(bytes, limits::MAX_PIXELS)
}

pub(super) fn decode_image_with_budget(bytes: &[u8], pixel_budget: u64) -> Result<Pixmap> {
    let img = decode_dynamic(bytes, pixel_budget)?.into_rgba8();
    let (w, h) = img.dimensions();
    Ok(Pixmap::from_raw(w, h, img.into_raw()))
}

pub(super) fn decode_dynamic(bytes: &[u8], pixel_budget: u64) -> Result<DynamicImage> {
    if bytes.len() as u64 > limits::MAX_ENCODED_IMAGE_BYTES {
        return Err(Error::Format(
            "encoded image exceeds the file-size limit".into(),
        ));
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits::decoder_limits());
    decode_bounded(reader, pixel_budget)
}

pub(super) fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    if bytes.len() as u64 > limits::MAX_ENCODED_IMAGE_BYTES {
        return Err(Error::Format(
            "encoded image exceeds the file-size limit".into(),
        ));
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits::decoder_limits());
    let dimensions = reader.into_dimensions()?;
    limits::validate_dimensions(dimensions.0, dimensions.1)?;
    Ok(dimensions)
}

fn decode_bounded<R: BufRead + Seek>(
    reader: image::ImageReader<R>,
    pixel_budget: u64,
) -> Result<DynamicImage> {
    let mut decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    let pixels = limits::validate_dimensions(w, h)?;
    if pixels > pixel_budget {
        return Err(Error::Format(
            "document exceeds the total decoded layer and mask pixel budget".into(),
        ));
    }
    let mut limits = limits::decoder_limits();
    // ImageReader::decode performs this accounting too; retain it when checking
    // dimensions before the full decode rather than after allocating the output.
    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)?;
    Ok(DynamicImage::from_decoder(decoder)?)
}

/// Encode pixels as a PNG in memory.
pub fn encode_png(p: &Pixmap) -> Result<Vec<u8>> {
    limits::validate_dimensions(p.w, p.h)?;
    let img = RgbaImage::from_raw(p.w, p.h, p.data.clone())
        .ok_or_else(|| Error::Format("invalid pixel buffer".into()))?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Open a document. Flat images become a single-layer document with no
/// `path`, so saving asks where to put the `.ora`.
pub fn open(path: &Path) -> Result<Document> {
    if is_ora(path) {
        let mut doc = Document::from_state(ora::load(path)?);
        doc.path = Some(path.to_owned());
        Ok(doc)
    } else {
        // The layer takes the picture's name; the document stays untitled so
        // saving never overwrites the imported file.
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Layer");
        Ok(Document::from_pixmap(load_pixmap(path)?, name))
    }
}

pub fn save(doc: &mut Document, path: &Path) -> Result<()> {
    ora::save(&doc.state, path)?;
    doc.path = Some(path.to_owned());
    doc.modified = false;
    Ok(())
}

/// Export the flattened image; the format comes from the extension
/// (png, jpg/jpeg, gif). JPEG is flattened onto white.
pub fn export(state: &DocState, path: &Path) -> Result<()> {
    if is_ora(path) {
        return ora::export_compatible(state, path);
    }
    limits::validate_document(state)?;
    let format = match ext(path).as_str() {
        "png" => ImageFormat::Png,
        "gif" => ImageFormat::Gif,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        other => {
            return Err(Error::Format(format!(
                "can't export to .{other} (use png, jpg, gif or ora)"
            )));
        }
    };
    let flat = composite::flatten(state);
    let rgba = RgbaImage::from_raw(flat.w, flat.h, flat.data).expect("buffer size");
    let mut temporary = atomic::AtomicFile::new(path, false)?;
    {
        let mut writer = std::io::BufWriter::new(temporary.file());
        if format == ImageFormat::Jpeg {
            let mut rgb = image::RgbImage::new(flat.w, flat.h);
            for (o, p) in rgb.pixels_mut().zip(rgba.pixels()) {
                let a = p[3] as u32;
                o.0 = std::array::from_fn(|i| {
                    ((p[i] as u32 * a + 255 * (255 - a) + 127) / 255) as u8
                });
            }
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 92);
            DynamicImage::ImageRgb8(rgb).write_with_encoder(enc)?;
        } else {
            rgba.write_to(&mut writer, format)?;
        }
        writer.flush()?;
    }
    temporary.commit()?;
    Ok(())
}
