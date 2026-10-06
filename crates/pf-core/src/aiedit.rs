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
    let mut image = resize(&crop, w, h, FilterType::CatmullRom);
    fill_empty(&mut image);
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

/// Paint over transparency with a soft continuation of the nearby colours.
/// The model treats see-through pixels as black and tends to leave them that
/// way, so empty canvas has to arrive looking like a rough guess instead.
fn fill_empty(px: &mut Pixmap) {
    if px.data.chunks_exact(4).all(|p| p[3] == 255) {
        return;
    }
    // Pull: average what is there into ever smaller copies.
    let first: Vec<[f32; 4]> = px
        .data
        .chunks_exact(4)
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
        })
        .collect();
    let mut levels = vec![(px.w as usize, px.h as usize, first)];
    while let Some((w, h, cur)) = levels.last().filter(|l| l.0 > 1 || l.1 > 1) {
        let (w, h) = (*w, *h);
        let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
        let mut next = vec![[0f32; 4]; nw * nh];
        for y in 0..h {
            for x in 0..w {
                let (src, dst) = (cur[y * w + x], &mut next[(y / 2) * nw + x / 2]);
                for c in 0..4 {
                    dst[c] += src[c];
                }
            }
        }
        for v in &mut next {
            if v[3] > 1.0 {
                *v = v.map(|c| c / v[3]);
            }
        }
        levels.push((nw, nh, next));
    }
    let top = levels.last_mut().expect("at least one level");
    for v in &mut top.2 {
        // Nothing opaque anywhere: plain white is as good a start as any.
        *v = if v[3] > 0.0 { [v[0] / v[3], v[1] / v[3], v[2] / v[3], 1.0] } else { [255.0, 255.0, 255.0, 1.0] };
    }
    // Push: fill each level's gaps from the smoother level above it.
    for i in (0..levels.len() - 1).rev() {
        let (fine, coarse) = levels.split_at_mut(i + 1);
        let ((w, h, cur), (cw, ch, up)) = (&mut fine[i], &coarse[0]);
        for y in 0..*h {
            for x in 0..*w {
                let v = &mut cur[y * *w + x];
                if v[3] >= 1.0 {
                    continue;
                }
                let (fx, fy) = (((x as f32 + 0.5) / 2.0 - 0.5).max(0.0), ((y as f32 + 0.5) / 2.0 - 0.5).max(0.0));
                let (x0, y0) = ((fx as usize).min(cw - 1), (fy as usize).min(ch - 1));
                let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                let rest = 1.0 - v[3];
                for c in 0..3 {
                    let a = up[y0 * cw + x0][c] * (1.0 - tx) + up[y0 * cw + x1][c] * tx;
                    let b = up[y1 * cw + x0][c] * (1.0 - tx) + up[y1 * cw + x1][c] * tx;
                    v[c] += rest * (a * (1.0 - ty) + b * ty);
                }
                v[3] = 1.0;
            }
        }
    }
    for (p, v) in px.data.chunks_exact_mut(4).zip(&levels[0].2) {
        p.copy_from_slice(&[v[0].round() as u8, v[1].round() as u8, v[2].round() as u8, 255]);
    }
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
