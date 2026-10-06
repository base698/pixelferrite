//! Preparing a layer (or the selected part of it) for an external image
//! model, and placing what comes back as a new layer. The network call itself
//! lives in the app; nothing here talks to anything.

use image::imageops::{self, FilterType};
use image::{GrayImage, RgbaImage};

use crate::buf::{Mask, Pixmap};
use crate::composite;
use crate::document::{DocState, Document, Layer, LayerId};
use crate::filter;
use crate::geom::IRect;
use crate::selection;

/// Which pixels a request is made from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The image as it looks: every visible layer, merged.
    Visible,
    /// The active layer on its own.
    Layer,
}

/// One edit request: what to send, and where the answer goes.
pub struct AiJob {
    pub source: Source,
    /// The active layer when the request was made. For [`Source::Layer`] the
    /// result is inserted above it; otherwise on top of everything.
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

pub(crate) fn resize(p: &Pixmap, w: u32, h: u32, f: FilterType) -> Pixmap {
    if (p.w, p.h) == (w, h) {
        return p.clone();
    }
    let img = RgbaImage::from_raw(p.w, p.h, p.data.clone()).expect("buffer size");
    Pixmap::from_raw(w, h, imageops::resize(&img, w, h, f).into_raw())
}

/// Build a request from the visible image or from the active layer. With a
/// selection, the request covers the selected area plus some surroundings
/// for context, and only the selected part will be replaced. `size_for` maps
/// the area's size to the image size the model accepts.
pub fn prepare(state: &DocState, source: Source, size_for: impl Fn(u32, u32) -> (u32, u32)) -> Option<AiJob> {
    let layer = state.active_layer()?;
    let visible = match source {
        Source::Visible => state.canvas(),
        Source::Layer => layer.rect().intersect(state.canvas()),
    };
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
    let crop = match source {
        Source::Visible => {
            let mut out = Pixmap::new(rect.width() as u32, rect.height() as u32);
            composite::composite_rect(state, rect, &mut out.data);
            out
        }
        Source::Layer => {
            let local = rect.translate(-layer.x, -layer.y);
            let mut crop = layer.pixels.reframed(local, [0; 4]);
            if let (Some(m), true) = (&layer.mask, layer.mask_enabled) {
                let m = m.reframed(local, [255]);
                for (p, m) in crop.data.chunks_exact_mut(4).zip(&m.data) {
                    p[3] = ((p[3] as u32 * *m as u32 + 127) / 255) as u8;
                }
            }
            crop
        }
    };
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
    Some(AiJob { source, layer: layer.id, rect, image, mask, clip })
}

impl Document {
    /// Place a model's answer to `job` as a new layer, scaled to the area that
    /// was sent: above the layer it came from, or on top of everything when
    /// the whole visible image was sent.
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
        match job.source {
            Source::Layer if self.state.layer(job.layer).is_some() => self.state.active = job.layer,
            Source::Layer => {}
            Source::Visible => self.state.active = self.state.layers.last().map_or(self.state.active, |l| l.id),
        }
        self.insert_layer("AI Edit", Layer::new(name, px, r.x0 + area.x0, r.y0 + area.y0))
    }
}
