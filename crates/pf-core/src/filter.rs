//! Filters: image adjustments applied to the active layer (or its mask),
//! limited to the selection when there is one.
//!
//! To add a filter, add a [`Filter`] variant and handle it in `name`,
//! `margin` and `run`. Previewing, selection clipping and undo come from
//! [`FilterOp`].

use std::sync::Arc;

use rayon::prelude::*;

use crate::buf::{Buf, Mask};
use crate::document::{DocState, Document, LayerId, Target};
use crate::geom::IRect;
use crate::selection;

/// How detected edges are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeStyle {
    /// Dark lines on white, like a line drawing.
    Lines,
    /// Light lines on black.
    LinesOnBlack,
    /// The image itself with its edges traced in a colour.
    Highlight,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeParams {
    /// Edge strength (0..=255 scale) needed to start a line; lower finds more.
    pub threshold: f32,
    /// Blur applied before looking for edges; higher ignores fine texture.
    pub smoothing: f32,
    /// Line width in pixels.
    pub thickness: f32,
    pub style: EdgeStyle,
    /// Line colour for [`EdgeStyle::Highlight`].
    pub color: [u8; 4],
}

impl Default for EdgeParams {
    fn default() -> Self {
        Self { threshold: 40.0, smoothing: 1.4, thickness: 1.0, style: EdgeStyle::Lines, color: [255, 220, 0, 255] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Filter {
    /// `radius` is the standard deviation in pixels.
    GaussianBlur { radius: f32 },
    /// Canny edge detection, drawn as lines or highlights.
    EdgeDetect(EdgeParams),
}

impl Filter {
    pub fn name(&self) -> &'static str {
        match self {
            Filter::GaussianBlur { .. } => "Gaussian Blur",
            Filter::EdgeDetect(_) => "Edge Detection",
        }
    }

    /// How far (in pixels) the filter reads from and spreads into its surroundings.
    fn margin(&self) -> i32 {
        match self {
            Filter::GaussianBlur { radius } => (radius.max(0.0) * 3.0).ceil() as i32 + 1,
            Filter::EdgeDetect(p) => (p.smoothing.max(0.0) * 3.0 + p.thickness).ceil() as i32 + 4,
        }
    }

    /// Filter `C` interleaved channels of a `w` x `h` float image in place.
    fn run<const C: usize>(&self, data: &mut Vec<f32>, w: usize, h: usize) {
        match self {
            Filter::GaussianBlur { radius } => gaussian_blur::<C>(data, w, h, *radius),
            Filter::EdgeDetect(p) => draw_edges::<C>(data, w, h, p),
        }
    }
}

/// Interactive filter: call [`FilterOp::update`] whenever the parameters
/// change (it re-renders from the snapshot each time), then `finish` or `cancel`.
pub struct FilterOp {
    before: DocState,
    layer: LayerId,
    target: Target,
    applied: bool,
    /// Area the previous preview covered, so a smaller one repaints it.
    last: IRect,
}

impl FilterOp {
    pub fn begin(doc: &Document) -> Option<Self> {
        let layer = doc.state.active_layer()?;
        if !layer.visible || layer.locked {
            return None;
        }
        Some(Self { before: doc.begin(), layer: layer.id, target: doc.effective_target(), applied: false, last: IRect::EMPTY })
    }

    pub fn update(&mut self, doc: &mut Document, f: &Filter) {
        let Some(orig) = self.before.layer(self.layer) else { return };
        let Some(i) = doc.state.index_of(self.layer) else { return };
        let mut layer = orig.clone();
        let old_rect = layer.rect();
        let margin = f.margin();
        let canvas = self.before.canvas();
        let sel = self.before.selection.as_deref();
        let target = if layer.mask.is_some() { self.target } else { Target::Pixels };

        if target == Target::Pixels {
            layer.text = None;
            // Give layers smaller than the canvas room for the effect to
            // spread into; a layer that fills the canvas keeps its edges.
            if !old_rect.contains_rect(canvas) {
                layer.ensure_covers(old_rect.expand(margin).intersect(canvas.union(old_rect)));
            }
        }
        let (ox, oy) = (layer.x, layer.y);
        let bounds = layer.rect();
        let roi = match sel.map(selection::bounds) {
            Some(Some(b)) => b.intersect(bounds),
            Some(None) => IRect::EMPTY,
            None => bounds,
        };
        if !roi.is_empty() {
            // Read a margin around the region so its edges see their real neighbours.
            let src = roi.expand(margin).intersect(bounds);
            let (src_l, roi_l) = (src.translate(-ox, -oy), roi.translate(-ox, -oy));
            match (target, &mut layer.mask) {
                (Target::Mask, Some(mask)) => apply::<1>(Arc::make_mut(mask), f, src_l, roi_l, sel, (ox, oy), false),
                _ => apply::<4>(Arc::make_mut(&mut layer.pixels), f, src_l, roi_l, sel, (ox, oy), true),
            }
        }
        layer.touch();
        doc.state.layers[i] = layer;
        doc.mark_dirty(old_rect.union(bounds).union(self.last));
        self.last = bounds;
        self.applied = true;
    }

    pub fn finish(self, doc: &mut Document, f: &Filter) {
        if self.applied {
            doc.commit(f.name(), self.before);
        }
    }

    pub fn cancel(self, doc: &mut Document) {
        doc.state = self.before;
        doc.mark_all_dirty();
    }

    /// Instead of changing any pixels, select the edges `p` finds in the
    /// layer (within the current selection, if there is one).
    pub fn select_edges(self, doc: &mut Document, p: &EdgeParams) {
        let found = self.before.layer(self.layer).map(|l| {
            let px = crate::composite::layer_on_canvas(&self.before, l);
            let (w, h) = (px.w as usize, px.h as usize);
            let mut data: Vec<f32> = px.data.iter().map(|v| *v as f32).collect();
            for p in data.chunks_exact_mut(4) {
                let a = p[3] / 255.0;
                (p[0], p[1], p[2]) = (p[0] * a, p[1] * a, p[2] * a);
            }
            let cov = edge_map(&gray_of::<4>(&data), w, h, p);
            Mask::from_raw(px.w, px.h, cov.iter().map(|v| (v * 255.0 + 0.5) as u8).collect())
        });
        let old = self.before.selection.clone();
        self.cancel(doc);
        if let Some(m) = found {
            let sel = match old.as_deref() {
                Some(s) => selection::combine(Some(s), &m, selection::Combine::Intersect),
                None => selection::bounds(&m).map(|_| m),
            };
            doc.set_selection("Select Edges", sel);
        }
    }
}

/// Gaussian-blur a mask, e.g. to feather a selection.
pub fn blur_mask(m: &Mask, sigma: f32) -> Mask {
    let mut data: Vec<f32> = m.data.iter().map(|v| *v as f32).collect();
    gaussian_blur::<1>(&mut data, m.w as usize, m.h as usize, sigma);
    Mask::from_raw(m.w, m.h, data.iter().map(|v| (v + 0.5).clamp(0.0, 255.0) as u8).collect())
}

/// One-shot filter, as a single undo step. Returns false if the layer can't be edited.
pub fn apply_filter(doc: &mut Document, f: &Filter) -> bool {
    let Some(mut op) = FilterOp::begin(doc) else { return false };
    op.update(doc, f);
    op.finish(doc, f);
    true
}

/// Run `f` over `src` of `buf` and write the result back inside `roi`,
/// blended through the selection. `premul` treats channel 3 as alpha.
fn apply<const C: usize>(
    buf: &mut Buf<C>,
    f: &Filter,
    src: IRect,
    roi: IRect,
    sel: Option<&Mask>,
    off: (i32, i32),
    premul: bool,
) {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let stride = buf.w as usize * C;
    let mut data = vec![0f32; w * h * C];
    data.par_chunks_mut(w * C).enumerate().for_each(|(y, row)| {
        let s = (src.y0 as usize + y) * stride + src.x0 as usize * C;
        for (o, p) in row.chunks_exact_mut(C).zip(buf.data[s..s + w * C].chunks_exact(C)) {
            let a = if premul { p[C - 1] as f32 / 255.0 } else { 1.0 };
            for c in 0..C {
                o[c] = if premul && c == C - 1 { p[c] as f32 } else { p[c] as f32 * a };
            }
        }
    });
    f.run::<C>(&mut data, w, h);
    let data = &data;
    buf.data.par_chunks_mut(stride).enumerate().skip(roi.y0 as usize).take(roi.height() as usize).for_each(|(y, row)| {
        let sy = y - src.y0 as usize;
        for x in roi.x0..roi.x1 {
            let k = sel.map_or(1.0, |s| s.get(x + off.0, y as i32 + off.1).map_or(0.0, |v| v[0] as f32 / 255.0));
            if k <= 0.0 {
                continue;
            }
            let v = &data[(sy * w + (x - src.x0) as usize) * C..][..C];
            let out = &mut row[x as usize * C..][..C];
            let inv = if premul { if v[C - 1] > 0.0 { 255.0 / v[C - 1] } else { 0.0 } } else { 1.0 };
            for c in 0..C {
                let n = if premul && c == C - 1 { v[c] } else { v[c] * inv };
                // Fully transparent results keep the old colour so nothing bleeds in later.
                let n = if premul && c != C - 1 && inv == 0.0 { out[c] as f32 } else { n };
                out[c] = (out[c] as f32 + (n - out[c] as f32) * k + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    });
}

/// Gaussian blur with edge pixels repeated outwards. Small radii use the
/// exact kernel; larger ones three box blurs, which is visually identical and
/// costs the same at any radius.
fn gaussian_blur<const C: usize>(data: &mut Vec<f32>, w: usize, h: usize, sigma: f32) {
    if sigma < 0.05 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0f32; data.len()];
    if sigma < 2.0 {
        let r = (sigma * 3.0).ceil() as usize;
        let mut k: Vec<f32> = (0..=2 * r).map(|i| (-((i as f32 - r as f32).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
        let sum: f32 = k.iter().sum();
        k.iter_mut().for_each(|v| *v /= sum);
        convolve_h::<C>(data, &mut tmp, w, &k);
        convolve_v::<C>(&tmp, data, w, h, &k);
        return;
    }
    for r in box_radii(sigma) {
        box_h::<C>(data, &mut tmp, w, r);
        box_v::<C>(&tmp, data, w, h, r);
    }
}

/// Radii of three box blurs that together approximate a gaussian of `sigma`.
fn box_radii(sigma: f32) -> [usize; 3] {
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n) / (-4.0 * wl as f32 - 4.0)).round() as i32;
    std::array::from_fn(|i| ((if (i as i32) < m { wl } else { wl + 2 }) as usize - 1) / 2)
}

fn convolve_h<const C: usize>(src: &[f32], dst: &mut [f32], w: usize, k: &[f32]) {
    let r = (k.len() / 2) as isize;
    dst.par_chunks_mut(w * C).zip(src.par_chunks(w * C)).for_each(|(d, s)| {
        for x in 0..w {
            let mut acc = [0f32; C];
            for (i, kv) in k.iter().enumerate() {
                let sx = (x as isize + i as isize - r).clamp(0, w as isize - 1) as usize;
                for c in 0..C {
                    acc[c] += s[sx * C + c] * kv;
                }
            }
            d[x * C..x * C + C].copy_from_slice(&acc);
        }
    });
}

fn convolve_v<const C: usize>(src: &[f32], dst: &mut [f32], w: usize, h: usize, k: &[f32]) {
    let r = (k.len() / 2) as isize;
    dst.par_chunks_mut(w * C).enumerate().for_each(|(y, d)| {
        d.fill(0.0);
        for (i, kv) in k.iter().enumerate() {
            let sy = (y as isize + i as isize - r).clamp(0, h as isize - 1) as usize;
            for (o, s) in d.iter_mut().zip(&src[sy * w * C..(sy + 1) * w * C]) {
                *o += s * kv;
            }
        }
    });
}

fn box_h<const C: usize>(src: &[f32], dst: &mut [f32], w: usize, r: usize) {
    let norm = 1.0 / (2 * r + 1) as f32;
    let at = |x: isize| x.clamp(0, w as isize - 1) as usize * C;
    dst.par_chunks_mut(w * C).zip(src.par_chunks(w * C)).for_each(|(d, s)| {
        let mut acc = [0f32; C];
        for i in -(r as isize)..=r as isize {
            for c in 0..C {
                acc[c] += s[at(i) + c];
            }
        }
        for x in 0..w {
            let (add, sub) = (at((x + r + 1) as isize), at(x as isize - r as isize));
            for c in 0..C {
                d[x * C + c] = acc[c] * norm;
                acc[c] += s[add + c] - s[sub + c];
            }
        }
    });
}

fn box_v<const C: usize>(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let n = w * C;
    let norm = 1.0 / (2 * r + 1) as f32;
    let row = |y: isize| {
        let y = y.clamp(0, h as isize - 1) as usize;
        &src[y * n..(y + 1) * n]
    };
    let mut acc = vec![0f32; n];
    for i in -(r as isize)..=r as isize {
        acc.iter_mut().zip(row(i)).for_each(|(a, s)| *a += s);
    }
    for y in 0..h {
        let (add, sub) = (row((y + r + 1) as isize), row(y as isize - r as isize));
        let d = &mut dst[y * n..(y + 1) * n];
        for x in 0..n {
            d[x] = acc[x] * norm;
            acc[x] += add[x] - sub[x];
        }
    }
}

/// Brightness (0..=255) of each pixel; premultiplied colour is read as if on white.
fn gray_of<const C: usize>(data: &[f32]) -> Vec<f32> {
    data.par_chunks_exact(C)
        .map(|p| if C >= 4 { 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2] + (255.0 - p[3]) } else { p[0] })
        .collect()
}

/// Canny edge detection. Returns line coverage in 0..=1 per pixel.
fn edge_map(gray: &[f32], w: usize, h: usize, p: &EdgeParams) -> Vec<f32> {
    if w < 3 || h < 3 {
        return vec![0.0; w * h];
    }
    let mut g = gray.to_vec();
    gaussian_blur::<1>(&mut g, w, h, p.smoothing);

    // Sobel gradient, then keep only pixels that are the local maximum
    // across the edge, so lines come out one pixel wide.
    let at = |x: usize, y: usize| g[y * w + x];
    let mut grad = vec![(0f32, 0f32); w * h];
    grad.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (y0, y1) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let gx = at(x1, y0) + 2.0 * at(x1, y) + at(x1, y1) - at(x0, y0) - 2.0 * at(x0, y) - at(x0, y1);
            let gy = at(x0, y1) + 2.0 * at(x, y1) + at(x1, y1) - at(x0, y0) - 2.0 * at(x, y0) - at(x1, y0);
            row[x] = (gx / 4.0, gy / 4.0);
        }
    });
    let mag = |x: isize, y: isize| {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
            return 0.0;
        }
        let (gx, gy) = grad[y as usize * w + x as usize];
        gx.hypot(gy)
    };
    let (hi, lo) = (p.threshold.max(0.5), p.threshold.max(0.5) * 0.4);
    // 0 = not an edge, 1 = weak (kept only if connected to a strong one), 2 = strong.
    let mut class = vec![0u8; w * h];
    class.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (gx, gy) = grad[y * w + x];
            let m = gx.hypot(gy);
            if m < lo {
                continue;
            }
            // Step one pixel along the gradient, rounded to the nearest of 8 directions.
            let t = gy.atan2(gx) / std::f32::consts::FRAC_PI_4;
            let (dx, dy) = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)][(t.round() as i32).rem_euclid(8) as usize];
            let (xi, yi) = (x as isize, y as isize);
            if m >= mag(xi + dx, yi + dy) && m > mag(xi - dx, yi - dy) {
                row[x] = if m >= hi { 2 } else { 1 };
            }
        }
    });
    let mut stack: Vec<usize> = class.iter().enumerate().filter(|(_, c)| **c == 2).map(|(i, _)| i).collect();
    while let Some(i) = stack.pop() {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx >= 0 && ny >= 0 && nx < w as isize && ny < h as isize {
                let j = ny as usize * w + nx as usize;
                if class[j] == 1 {
                    class[j] = 2;
                    stack.push(j);
                }
            }
        }
    }
    let mut cov: Vec<f32> = class.iter().map(|c| if *c == 2 { 1.0 } else { 0.0 }).collect();

    // Thicken, then soften the stair-steps a little.
    let r = ((p.thickness - 1.0) / 2.0).round().clamp(0.0, 32.0) as isize;
    if r > 0 {
        let mut tmp = vec![0f32; w * h];
        tmp.par_chunks_mut(w).zip(cov.par_chunks(w)).for_each(|(d, s)| {
            for x in 0..w as isize {
                d[x as usize] = (x - r..=x + r).filter(|i| *i >= 0 && *i < w as isize).fold(0.0, |m, i| s[i as usize].max(m));
            }
        });
        cov.par_chunks_mut(w).enumerate().for_each(|(y, d)| {
            for x in 0..w {
                d[x] = (y as isize - r..=y as isize + r).filter(|i| *i >= 0 && *i < h as isize).fold(0.0, |m, i| tmp[i as usize * w + x].max(m));
            }
        });
    }
    gaussian_blur::<1>(&mut cov, w, h, 0.55);
    cov.iter_mut().for_each(|v| *v = (*v * 1.9).min(1.0));
    cov
}

fn draw_edges<const C: usize>(data: &mut [f32], w: usize, h: usize, p: &EdgeParams) {
    let cov = edge_map(&gray_of::<C>(data), w, h, p);
    let line = p.color.map(|c| c as f32);
    data.par_chunks_exact_mut(C).zip(&cov).for_each(|(px, e)| {
        let (ink, paper) = match p.style {
            EdgeStyle::Lines => (0.0, 255.0),
            EdgeStyle::LinesOnBlack => (255.0, 0.0),
            EdgeStyle::Highlight => {
                // Trace over the original (premultiplied) pixel.
                let k = e * line[3] / 255.0;
                for c in 0..C {
                    let target = if C >= 4 { if c == 3 { 255.0 } else { line[c] } } else { 255.0 };
                    px[c] += (target - px[c]) * k;
                }
                return;
            }
        };
        let v = paper + (ink - paper) * e;
        for c in 0..C {
            px[c] = if C >= 4 && c == 3 { 255.0 } else { v };
        }
    });
}
