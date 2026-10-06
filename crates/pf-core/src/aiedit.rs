//! Preparing a layer (or the selected part of it) for an external image
//! model, and placing what comes back as a new layer. The network call itself
//! lives in the app; nothing here talks to anything.

use image::imageops::{self, FilterType};
use image::{GrayImage, RgbaImage};

use crate::buf::{Mask, Pixmap};
use crate::document::{DocState, Document, Layer, LayerId};
use crate::filter;
use crate::geom::IRect;
use crate::selection;

/// One edit request: what to send, and where the answer goes.
pub struct AiJob {
    /// The layer the pixels came from; the result is inserted above it.
    pub layer: LayerId,
    /// Document area the result is scaled onto.
    pub rect: IRect,
    /// The pixels of `rect`, resized to the size the model wants.
    pub image: Pixmap,
    /// Same size as `image`; transparent where the model should edit.
    /// `None` when the whole image is fair game.
    pub mask: Option<Pixmap>,
    /// Feathered selection over `rect`: the result is kept only inside it.
    clip: Option<Mask>,
}

fn resize(p: &Pixmap, w: u32, h: u32, f: FilterType) -> Pixmap {
    if (p.w, p.h) == (w, h) {
        return p.clone();
    }
    let img = RgbaImage::from_raw(p.w, p.h, p.data.clone()).expect("buffer size");
    Pixmap::from_raw(w, h, imageops::resize(&img, w, h, f).into_raw())
}

/// Build a request from the active layer. With a selection, the request
/// covers the selected area plus some surroundings for context, and only the
/// selected part will be replaced. `size_for` maps the area's size to the
/// image size the model accepts.
pub fn prepare(state: &DocState, size_for: impl Fn(u32, u32) -> (u32, u32)) -> Option<AiJob> {
    let layer = state.active_layer()?;
    let visible = layer.rect().intersect(state.canvas());
    if visible.is_empty() {
        return None;
    }
    let sel = state.selection.as_deref();
    let rect = match sel {
        Some(s) => {
            let b = selection::bounds(s)?.intersect(visible);
            if b.is_empty() {
                return None;
            }
            let context = (b.width().max(b.height()) as f32 * 0.4).max(48.0) as i32;
            b.expand(context).intersect(visible)
        }
        None => visible,
    };
    let local = rect.translate(-layer.x, -layer.y);
    let mut crop = layer.pixels.reframed(local, [0; 4]);
    if let (Some(m), true) = (&layer.mask, layer.mask_enabled) {
        let m = m.reframed(local, [255]);
        for (p, m) in crop.data.chunks_exact_mut(4).zip(&m.data) {
            p[3] = ((p[3] as u32 * *m as u32 + 127) / 255) as u8;
        }
    }
    let (w, h) = size_for(crop.w, crop.h);
    let image = resize(&crop, w, h, FilterType::CatmullRom);
    let (mask, clip) = match sel {
        Some(s) => {
            let part = s.reframed(rect, [0]);
            let gray = GrayImage::from_raw(part.w, part.h, part.data.clone()).expect("buffer size");
            let gray = imageops::resize(&gray, w, h, FilterType::Triangle);
            let mut mask = Pixmap::new(w, h);
            for (p, v) in mask.data.chunks_exact_mut(4).zip(gray.as_raw()) {
                p[3] = 255 - *v;
            }
            // Soften the edge so the new pixels blend into the old ones.
            let feather = (rect.width().max(rect.height()) as f32 * 0.008).clamp(1.5, 10.0);
            (Some(mask), Some(filter::blur_mask(&part, feather)))
        }
        None => (None, None),
    };
    Some(AiJob { layer: layer.id, rect, image, mask, clip })
}

impl Document {
    /// Place a model's answer to `job` as a new layer above the layer it came
    /// from, scaled to the area that was sent.
    pub fn insert_ai_result(&mut self, job: &AiJob, result: &Pixmap, name: &str) -> LayerId {
        let r = job.rect;
        let mut px = resize(result, r.width() as u32, r.height() as u32, FilterType::Lanczos3);
        let mut area = px.rect();
        if let Some(clip) = &job.clip {
            for (p, c) in px.data.chunks_exact_mut(4).zip(&clip.data) {
                p[3] = ((p[3] as u32 * *c as u32 + 127) / 255) as u8;
            }
            area = selection::bounds(clip).unwrap_or(IRect::xywh(0, 0, 1, 1));
            px = px.reframed(area, [0; 4]);
        }
        if self.state.layer(job.layer).is_some() {
            self.state.active = job.layer;
        }
        self.insert_layer("AI Edit", Layer::new(name, px, r.x0 + area.x0, r.y0 + area.y0))
    }
}
