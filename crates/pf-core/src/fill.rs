//! Area fills: gradients, bucket fill, fill / clear selection.

use std::sync::Arc;

use rayon::prelude::*;

use crate::buf::{Mask, Pixmap, luma, over};
use crate::composite;
use crate::document::{DocState, Document, LayerId, Target};
use crate::geom::IRect;
use crate::selection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientShape {
    Linear,
    Radial,
}

#[derive(Clone, Copy, Debug)]
pub struct GradientParams {
    pub shape: GradientShape,
    pub from: [u8; 4],
    pub to: [u8; 4],
    pub opacity: f32,
}

/// Interactive gradient: call [`GradientOp::update`] while dragging (it
/// re-renders from the snapshot each time), then `finish` or `cancel`.
pub struct GradientOp {
    before: DocState,
    layer: LayerId,
    target: Target,
    src_px: Arc<Pixmap>,
    src_mask: Option<Arc<Mask>>,
    off: (i32, i32),
    clip: IRect,
    sel: Option<Arc<Mask>>,
    drawn: bool,
}

impl GradientOp {
    pub fn begin(doc: &mut Document) -> Option<Self> {
        let before = doc.begin();
        let canvas = doc.canvas();
        let target = doc.effective_target();
        let sel = doc.state.selection.clone();
        let layer = doc.state.active_layer_mut()?;
        if !layer.visible || layer.locked {
            return None;
        }
        layer.ensure_covers(canvas);
        if target == Target::Pixels || layer.mask.is_none() {
            layer.text = None;
        }
        let off = (layer.x, layer.y);
        let area = sel.as_deref().and_then(selection::bounds).unwrap_or(canvas).intersect(canvas);
        Some(Self {
            before,
            layer: layer.id,
            target,
            src_px: layer.pixels.clone(),
            src_mask: layer.mask.clone(),
            off,
            clip: area.translate(-off.0, -off.1),
            sel,
            drawn: false,
        })
    }

    /// Render the gradient from `a` to `b` (document space).
    pub fn update(&mut self, doc: &mut Document, a: (f32, f32), b: (f32, f32), p: &GradientParams) {
        let clip = self.clip;
        if clip.is_empty() {
            return;
        }
        let Some(layer) = doc.state.layer_mut(self.layer) else { return };
        let (ox, oy) = self.off;
        let (vx, vy) = (b.0 - a.0, b.1 - a.1);
        let len2 = (vx * vx + vy * vy).max(1e-6);
        let pm = |c: [u8; 4]| {
            let al = c[3] as f32 / 255.0;
            [c[0] as f32 * al, c[1] as f32 * al, c[2] as f32 * al, al]
        };
        let (c0, c1) = (pm(p.from), pm(p.to));
        let color_at = |x: i32, y: i32| -> [u8; 4] {
            let (px, py) = ((x + ox) as f32 + 0.5 - a.0, (y + oy) as f32 + 0.5 - a.1);
            let t = match p.shape {
                GradientShape::Linear => (px * vx + py * vy) / len2,
                GradientShape::Radial => ((px * px + py * py) / len2).sqrt(),
            }
            .clamp(0.0, 1.0);
            let c: [f32; 4] = std::array::from_fn(|i| c0[i] + (c1[i] - c0[i]) * t);
            if c[3] <= 0.0 {
                return [0; 4];
            }
            let q = |v: f32| (v / c[3] + 0.5).min(255.0) as u8;
            [q(c[0]), q(c[1]), q(c[2]), (c[3] * 255.0 + 0.5) as u8]
        };
        let sel = self.sel.as_deref();
        let alpha_at = |x: i32, y: i32| -> f32 {
            p.opacity * sel.map_or(1.0, |s| s.px(x + ox, y + oy)[0] as f32 / 255.0)
        };
        let (x0, n) = (clip.x0 as usize, clip.width() as usize);
        let rows = (clip.y0 as usize, clip.height() as usize);
        match (self.target, &mut layer.mask, &self.src_mask) {
            (Target::Mask, Some(mask), Some(src)) => {
                let w = src.w as usize;
                Arc::make_mut(mask).data.par_chunks_mut(w).enumerate().skip(rows.0).take(rows.1).for_each(
                    |(y, row)| {
                        for x in x0..x0 + n {
                            let (xi, yi) = (x as i32, y as i32);
                            let c = color_at(xi, yi);
                            let al = alpha_at(xi, yi) * c[3] as f32 / 255.0;
                            let s = src.px(xi, yi)[0] as f32;
                            row[x] = (s + (luma(c) as f32 - s) * al + 0.5) as u8;
                        }
                    },
                );
            }
            _ => {
                let src = &self.src_px;
                let w = src.w as usize * 4;
                Arc::make_mut(&mut layer.pixels).data.par_chunks_mut(w).enumerate().skip(rows.0).take(rows.1).for_each(
                    |(y, row)| {
                        for x in x0..x0 + n {
                            let (xi, yi) = (x as i32, y as i32);
                            let out = over(src.px(xi, yi), color_at(xi, yi), alpha_at(xi, yi));
                            row[x * 4..x * 4 + 4].copy_from_slice(&out);
                        }
                    },
                );
            }
        }
        layer.touch();
        self.drawn = true;
        doc.mark_dirty(clip.translate(ox, oy));
    }

    pub fn finish(self, doc: &mut Document) {
        let rect = if self.drawn { self.clip } else { IRect::EMPTY };
        doc.commit_patch("Gradient", self.before, self.layer, self.target, rect);
    }

    pub fn cancel(self, doc: &mut Document) {
        doc.state = self.before;
        doc.mark_all_dirty();
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FillMode {
    Color([u8; 4]),
    Erase,
}

/// Fill (or erase) the active layer through a document-sized coverage mask.
/// Returns false if nothing could be changed.
pub fn fill_mask(doc: &mut Document, name: &str, cov: &Mask, mode: FillMode, opacity: f32) -> bool {
    let Some(area) = selection::bounds(cov) else { return false };
    let before = doc.begin();
    let canvas = doc.canvas();
    let target = doc.effective_target();
    let Some(layer) = doc.state.active_layer_mut() else { return false };
    if layer.locked {
        return false;
    }
    if matches!(mode, FillMode::Color(_)) {
        layer.ensure_covers(canvas);
    }
    let (ox, oy) = (layer.x, layer.y);
    let rect = area.intersect(layer.rect()).translate(-ox, -oy);
    if rect.is_empty() {
        doc.state = before;
        return false;
    }
    match (target, &mut layer.mask) {
        (Target::Mask, Some(mask)) => {
            let mask = Arc::make_mut(mask);
            let v = match mode {
                FillMode::Color(c) => luma(c) as f32,
                FillMode::Erase => 0.0,
            };
            for y in rect.y0..rect.y1 {
                for x in rect.x0..rect.x1 {
                    let a = cov.px(x + ox, y + oy)[0] as f32 / 255.0 * opacity;
                    let s = mask.px(x, y)[0] as f32;
                    mask.set(x, y, [(s + (v - s) * a + 0.5) as u8]);
                }
            }
        }
        _ => {
            layer.text = None;
            let px = Arc::make_mut(&mut layer.pixels);
            for y in rect.y0..rect.y1 {
                for x in rect.x0..rect.x1 {
                    let a = cov.px(x + ox, y + oy)[0] as f32 / 255.0 * opacity;
                    if a <= 0.0 {
                        continue;
                    }
                    let s = px.px(x, y);
                    let out = match mode {
                        FillMode::Color(c) => over(s, c, a),
                        FillMode::Erase => [s[0], s[1], s[2], (s[3] as f32 * (1.0 - a) + 0.5) as u8],
                    };
                    px.set(x, y, out);
                }
            }
        }
    }
    layer.touch();
    let id = layer.id;
    doc.mark_dirty(rect.translate(ox, oy));
    doc.commit_patch(name, before, id, target, rect);
    true
}

/// The current selection, or the whole canvas when nothing is selected.
pub fn selection_or_all(doc: &Document) -> Mask {
    match &doc.state.selection {
        Some(s) => (**s).clone(),
        None => Mask::filled(doc.state.width, doc.state.height, [255]),
    }
}

/// Paint bucket: flood from `seed` over similar colours and fill with `color`.
pub fn bucket(
    doc: &mut Document,
    seed: (i32, i32),
    color: [u8; 4],
    tolerance: u8,
    contiguous: bool,
    all_layers: bool,
    opacity: f32,
) -> bool {
    let Some(src) = sample_source(&doc.state, all_layers) else { return false };
    let flood = selection::flood_mask(&src, seed, tolerance, contiguous, None);
    let cov = match &doc.state.selection {
        Some(sel) => match selection::combine(Some(sel), &flood, selection::Combine::Intersect) {
            Some(m) => m,
            None => return false,
        },
        None => flood,
    };
    fill_mask(doc, "Fill", &cov, FillMode::Color(color), opacity)
}

/// Canvas-sized pixels that colour-based tools (wand, bucket) look at.
pub fn sample_source(state: &DocState, all_layers: bool) -> Option<Pixmap> {
    if all_layers {
        Some(composite::flatten(state))
    } else {
        state.active_layer().map(|l| composite::layer_on_canvas(state, l))
    }
}
