//! File formats. The native format is OpenRaster (`.ora`), an open layered
//! format also read by Krita, GIMP and MyPaint; flat images go through `image`.

use std::path::Path;

use image::{DynamicImage, ImageFormat, RgbaImage};

use crate::buf::Pixmap;
use crate::composite;
use crate::document::{DocState, Document};

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
pub const OPEN_EXTENSIONS: &[&str] = &["ora", "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"];
/// Extensions [`export`] can write.
pub const EXPORT_EXTENSIONS: &[&str] = &["png", "jpg", "gif"];

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_ora(path: &Path) -> bool {
    ext(path) == "ora"
}

/// Decode a flat image file.
pub fn load_pixmap(path: &Path) -> Result<Pixmap> {
    let img = image::ImageReader::open(path)?.with_guessed_format()?.decode()?.into_rgba8();
    let (w, h) = img.dimensions();
    Ok(Pixmap::from_raw(w, h, img.into_raw()))
}

/// Open a document. Flat images become a single-layer document with no
/// `path`, so saving asks where to put the `.ora`.
pub fn open(path: &Path) -> Result<Document> {
    if is_ora(path) {
        let mut doc = Document::from_state(ora::load(path)?);
        doc.path = Some(path.to_owned());
        Ok(doc)
    } else {
        Ok(Document::from_pixmap(load_pixmap(path)?, "Layer"))
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
    let flat = composite::flatten(state);
    let rgba = RgbaImage::from_raw(flat.w, flat.h, flat.data).expect("buffer size");
    match ext(path).as_str() {
        "png" => rgba.save_with_format(path, ImageFormat::Png)?,
        "gif" => rgba.save_with_format(path, ImageFormat::Gif)?,
        "jpg" | "jpeg" => {
            let mut rgb = image::RgbImage::new(flat.w, flat.h);
            for (o, p) in rgb.pixels_mut().zip(rgba.pixels()) {
                let a = p[3] as u32;
                o.0 = std::array::from_fn(|i| ((p[i] as u32 * a + 255 * (255 - a) + 127) / 255) as u8);
            }
            let file = std::io::BufWriter::new(std::fs::File::create(path)?);
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 92);
            DynamicImage::ImageRgb8(rgb).write_with_encoder(enc)?;
        }
        other => return Err(Error::Format(format!("can't export to .{other} (use png, jpg or gif)"))),
    }
    Ok(())
}
