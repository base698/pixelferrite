//! Shared resource limits for file input and editor document creation.

use super::{Error, Result};
use crate::document::DocState;
use crate::text::TextSpec;

pub const MAX_DIMENSION: u32 = 16_384;
pub const MAX_PIXELS: u64 = 32 * 1024 * 1024;
pub const MAX_DOCUMENT_PIXELS: u64 = 64 * 1024 * 1024;
pub const MAX_LAYERS: usize = 256;
pub const MAX_LAYER_OFFSET: i32 = 65_536;
pub const MAX_XML_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_ENCODED_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_ARCHIVE_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_ARCHIVE_ENTRIES: usize = 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_DECODE_BYTES: u64 = MAX_PIXELS * 8;
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_TEXT_PATH_POINTS: usize = 16 * 1024;

fn valid_xml_text(text: &str) -> bool {
    text.chars().all(|c| matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'))
}

pub fn validate_text(spec: &TextSpec) -> Result<()> {
    let bounded = |v: f32, min: f32, max: f32| v.is_finite() && (min..=max).contains(&v);
    if spec.text.len() > MAX_TEXT_BYTES
        || spec.font.len() > 1024
        || !valid_xml_text(&spec.text)
        || !valid_xml_text(&spec.font)
        || !bounded(spec.size, 0.1, 4096.0)
        || !bounded(spec.tracking, -4096.0, 4096.0)
        || !bounded(spec.leading, 0.1, 10.0)
        || !bounded(
            spec.path_offset,
            -(MAX_LAYER_OFFSET as f32),
            MAX_LAYER_OFFSET as f32,
        )
        || spec.path.len() > MAX_TEXT_PATH_POINTS
        || spec.path.iter().any(|p| {
            !bounded(p.0, -(MAX_LAYER_OFFSET as f32), MAX_LAYER_OFFSET as f32)
                || !bounded(p.1, -(MAX_LAYER_OFFSET as f32), MAX_LAYER_OFFSET as f32)
        })
    {
        return Err(Error::Format(
            "text metadata exceeds supported size or coordinate limits".into(),
        ));
    }
    validate_offset(spec.raster.0, spec.raster.1)
}

pub fn validate_dimensions(width: u32, height: u32) -> Result<u64> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::Format("image dimensions overflow".into()))?;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || pixels > MAX_PIXELS
    {
        return Err(Error::Format(format!(
            "image dimensions {width}×{height} exceed supported limits (nonzero, at most {MAX_DIMENSION} per side and {} megapixels)",
            MAX_PIXELS / 1_000_000
        )));
    }
    Ok(pixels)
}

pub fn validate_offset(x: i32, y: i32) -> Result<()> {
    if !(-MAX_LAYER_OFFSET..=MAX_LAYER_OFFSET).contains(&x)
        || !(-MAX_LAYER_OFFSET..=MAX_LAYER_OFFSET).contains(&y)
    {
        return Err(Error::Format(
            "layer position exceeds the supported coordinate range".into(),
        ));
    }
    Ok(())
}

pub fn validate_document(state: &DocState) -> Result<()> {
    validate_dimensions(state.width, state.height)?;
    if state.layers.is_empty() || state.layers.len() > MAX_LAYERS {
        return Err(Error::Format(format!(
            "documents must have between 1 and {MAX_LAYERS} layers"
        )));
    }
    let mut total = 0u64;
    for layer in &state.layers {
        if !valid_xml_text(&layer.name) {
            return Err(Error::Format(
                "layer name contains characters that OpenRaster cannot store".into(),
            ));
        }
        let pixels = validate_dimensions(layer.pixels.w, layer.pixels.h)?;
        validate_offset(layer.x, layer.y)?;
        if layer.pixels.data.len() as u64 != pixels * 4
            || !layer.opacity.is_finite()
            || !(0.0..=1.0).contains(&layer.opacity)
        {
            return Err(Error::Format("invalid layer pixels or opacity".into()));
        }
        total += pixels;
        if let Some(mask) = &layer.mask {
            if (mask.w, mask.h) != (layer.pixels.w, layer.pixels.h)
                || mask.data.len() as u64 != pixels
            {
                return Err(Error::Format(
                    "layer mask dimensions do not match its pixels".into(),
                ));
            }
            total += pixels;
        }
        if let Some(text) = &layer.text {
            validate_text(text)?;
        }
        if total > MAX_DOCUMENT_PIXELS {
            return Err(Error::Format(
                "document exceeds the total decoded layer and mask pixel budget".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn decoder_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}
