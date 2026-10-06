//! Geometry: perspective warps and content-aware (seam carving) scaling.

use std::sync::Arc;

use rayon::prelude::*;

use crate::buf::{Buf, Mask, Pixmap};
use crate::document::{DocState, Document, LayerId};
use crate::fx::bilinear;
use crate::geom::IRect;

pub type Quad = [(f32, f32); 4];

/// The 3x3 matrix taking each `src` corner to the matching `dst` corner
/// (row-major), or `None` if the corners are degenerate.
pub fn homography(src: Quad, dst: Quad) -> Option<[f64; 9]> {
    // Eight equations in the eight unknowns h0..h7 (h8 = 1).
    let mut m = [[0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = (src[i].0 as f64, src[i].1 as f64);
        let (u, v) = (dst[i].0 as f64, dst[i].1 as f64);
        m[i * 2] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[i * 2 + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for col in 0..8 {
        let pivot = (col..8).max_by(|a, b| m[*a][col].abs().total_cmp(&m[*b][col].abs()))?;
        if m[pivot][col].abs() < 1e-9 {
            return None;
        }
        m.swap(col, pivot);
        for row in 0..8 {
            if row != col {
                let k = m[row][col] / m[col][col];
                for c in col..9 {
                    m[row][c] -= k * m[col][c];
                }
            }
        }
    }
    let h: [f64; 8] = std::array::from_fn(|i| m[i][8] / m[i][i]);
    Some([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0])
}

fn project(h: &[f64; 9], x: f64, y: f64) -> (f64, f64) {
    let w = h[6] * x + h[7] * y + h[8];
    ((h[0] * x + h[1] * y + h[2]) / w, (h[3] * x + h[4] * y + h[5]) / w)
}

/// Resample `src` into a `w` x `h` buffer; `back` maps an output pixel
/// centre to a source position. Pixels that map outside are left empty.
fn warp<const C: usize>(src: &Buf<C>, w: u32, h: u32, back: impl Fn(f64, f64) -> (f64, f64) + Sync) -> Buf<C> {
    let premul = C == 4;
    let (sw, sh) = (src.w as usize, src.h as usize);
    let data: Vec<f32> = src
        .data
        .par_chunks_exact(C)
        .flat_map_iter(|p| {
            let a = if premul { p[C - 1] as f32 / 255.0 } else { 1.0 };
            (0..C).map(move |c| if premul && c == C - 1 { p[c] as f32 } else { p[c] as f32 * a })
        })
        .collect();
    let mut out = Buf::<C>::new(w, h);
    out.data.par_chunks_mut(w as usize * C).enumerate().for_each(|(y, row)| {
        for x in 0..w as usize {
            let (sx, sy) = back(x as f64 + 0.5, y as f64 + 0.5);
            if !(sx.is_finite() && sy.is_finite()) || sx < -1.0 || sy < -1.0 || sx > sw as f64 + 1.0 || sy > sh as f64 + 1.0 {
                continue;
            }
            let v = bilinear::<C>(&data, sw, sh, sx as f32, sy as f32, false);
            let inv = if premul { if v[C - 1] > 0.0 { 255.0 / v[C - 1] } else { 0.0 } } else { 1.0 };
            for c in 0..C {
                let n = if premul && c == C - 1 { v[c] } else { v[c] * inv };
                row[x * C + c] = (n + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    });
    out
}

/// How the four dragged corners are interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerspectiveMode {
    /// The layer's corners move to the dragged corners.
    Distort,
    /// The dragged corners mark something that should be a rectangle (a
    /// page, a screen): it is pulled square to fill the layer's frame.
    Straighten,
}

/// Interactive perspective change of one layer: `update` as corners move
/// (it re-renders from the snapshot), then `finish` or `cancel`.
pub struct PerspectiveOp {
    before: DocState,
    pub layer: LayerId,
    /// The layer's frame when the operation began (document space).
    pub frame: IRect,
}

impl PerspectiveOp {
    pub fn begin(doc: &Document) -> Option<Self> {
        let l = doc.state.active_layer()?;
        if !l.visible || l.locked || l.pixels.w < 2 || l.pixels.h < 2 {
            return None;
        }
        Some(Self { before: doc.begin(), layer: l.id, frame: l.rect() })
    }

    /// Corners of the layer's frame: top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self) -> Quad {
        let r = self.frame;
        [(r.x0 as f32, r.y0 as f32), (r.x1 as f32, r.y0 as f32), (r.x1 as f32, r.y1 as f32), (r.x0 as f32, r.y1 as f32)]
    }

    /// Re-render with the corners at `quad` (document space). Returns false
    /// if the corners don't describe a usable shape.
    pub fn update(&mut self, doc: &mut Document, quad: Quad, mode: PerspectiveMode) -> bool {
        let Some(orig) = self.before.layer(self.layer) else { return false };
        let Some(i) = doc.state.index_of(self.layer) else { return false };
        let frame = self.corners();
        let (from, to) = match mode {
            PerspectiveMode::Distort => (frame, quad),
            PerspectiveMode::Straighten => (quad, frame),
        };
        // Output covers wherever the frame's corners land (the frame itself when straightening).
        let Some(fwd) = homography(from, to) else { return false };
        let Some(inv) = homography(to, from) else { return false };
        let landed = frame.map(|p| project(&fwd, p.0 as f64, p.1 as f64));
        let out = match mode {
            PerspectiveMode::Straighten => self.frame,
            PerspectiveMode::Distort => {
                let (x0, x1) = landed.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                let (y0, y1) = landed.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                IRect::enclosing(x0 as f32, y0 as f32, x1 as f32, y1 as f32)
            }
        };
        if out.is_empty() || out.width() as i64 * out.height() as i64 > 1 << 27 {
            return false;
        }
        let (lx, ly) = (orig.x as f64, orig.y as f64);
        let (ox, oy) = (out.x0 as f64, out.y0 as f64);
        let back = move |x: f64, y: f64| {
            let (sx, sy) = project(&inv, x + ox, y + oy);
            (sx - lx, sy - ly)
        };
        let mut layer = orig.clone();
        layer.pixels = Arc::new(warp::<4>(&orig.pixels, out.width() as u32, out.height() as u32, back));
        layer.mask = orig.mask.as_ref().map(|m| Arc::new(warp::<1>(m, out.width() as u32, out.height() as u32, back)));
        (layer.x, layer.y) = (out.x0, out.y0);
        layer.text = None;
        layer.touch();
        doc.state.layers[i] = layer;
        doc.mark_all_dirty();
        true
    }

    pub fn finish(self, doc: &mut Document) {
        doc.commit("Perspective", self.before);
    }

    pub fn cancel(self, doc: &mut Document) {
        doc.state = self.before;
        doc.mark_all_dirty();
    }
}

/// Remove `n` vertical seams (connected top-to-bottom paths of least visual
/// importance) from a row-major image of `C`-byte pixels.
fn carve_columns<const C: usize>(data: &mut Vec<u8>, w: &mut usize, h: usize, n: usize, also: &mut Option<Vec<u8>>) {
    for _ in 0..n {
        let cw = *w;
        if cw <= 2 {
            break;
        }
        let lum = |x: usize, y: usize| {
            let p = &data[(y * cw + x) * C..][..C];
            if C >= 4 { (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) * p[3] as f32 / 255.0 + (255 - p[3]) as f32 } else { p[0] as f32 }
        };
        // Energy: how much the picture changes at each pixel.
        let mut cost = vec![0f32; cw * h];
        cost.par_chunks_mut(cw).enumerate().for_each(|(y, row)| {
            for x in 0..cw {
                let gx = lum((x + 1).min(cw - 1), y) - lum(x.saturating_sub(1), y);
                let gy = lum(x, (y + 1).min(h - 1)) - lum(x, y.saturating_sub(1));
                row[x] = gx.abs() + gy.abs();
            }
        });
        // Cheapest path to each pixel from the top.
        for y in 1..h {
            let (prev, cur) = cost.split_at_mut(y * cw);
            let prev = &prev[(y - 1) * cw..];
            for x in 0..cw {
                let best = prev[x].min(prev[x.saturating_sub(1)]).min(prev[(x + 1).min(cw - 1)]);
                cur[x] += best;
            }
        }
        let mut x = (0..cw).min_by(|a, b| cost[(h - 1) * cw + a].total_cmp(&cost[(h - 1) * cw + b])).unwrap();
        let mut seam = vec![0usize; h];
        for y in (0..h).rev() {
            seam[y] = x;
            if y > 0 {
                let row = &cost[(y - 1) * cw..y * cw];
                x = (x.saturating_sub(1)..=(x + 1).min(cw - 1)).min_by(|a, b| row[*a].total_cmp(&row[*b])).unwrap();
            }
        }
        let cut = |buf: &mut Vec<u8>, c: usize| {
            let mut out = Vec::with_capacity((cw - 1) * h * c);
            for y in 0..h {
                let row = &buf[y * cw * c..(y + 1) * cw * c];
                out.extend_from_slice(&row[..seam[y] * c]);
                out.extend_from_slice(&row[(seam[y] + 1) * c..]);
            }
            *buf = out;
        };
        cut(data, C);
        if let Some(m) = also {
            cut(m, 1);
        }
        *w = cw - 1;
    }
}

fn transpose<const C: usize>(data: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; data.len()];
    for y in 0..h {
        for x in 0..w {
            out[(x * h + y) * C..][..C].copy_from_slice(&data[(y * w + x) * C..][..C]);
        }
    }
    out
}

/// Shrink an image to `new_w` x `new_h` by removing its least interesting
/// seams, so the important content keeps its shape. Never enlarges.
pub fn seam_carve(px: &Pixmap, mask: Option<&Mask>, new_w: u32, new_h: u32) -> (Pixmap, Option<Mask>) {
    let (mut w, mut h) = (px.w as usize, px.h as usize);
    let (tw, th) = ((new_w as usize).clamp(1, w), (new_h as usize).clamp(1, h));
    let mut data = px.data.clone();
    let mut m = mask.map(|m| m.data.clone());
    let cut = w - tw;
    carve_columns::<4>(&mut data, &mut w, h, cut, &mut m);
    if h > th {
        // Rows are columns of the transposed image.
        let mut t = transpose::<4>(&data, w, h);
        let mut tm = m.as_ref().map(|m| transpose::<1>(m, w, h));
        let cut = h - th;
        carve_columns::<4>(&mut t, &mut h, w, cut, &mut tm);
        data = transpose::<4>(&t, h, w);
        m = tm.map(|tm| transpose::<1>(&tm, h, w));
    }
    (Pixmap::from_raw(w as u32, h as u32, data), m.map(|m| Mask::from_raw(w as u32, h as u32, m)))
}

impl Document {
    /// Content-aware scale of the active layer down to `new_w` x `new_h`.
    /// If that layer is the whole image (the only layer, filling the canvas)
    /// the canvas shrinks with it.
    pub fn content_aware_scale(&mut self, new_w: u32, new_h: u32) -> bool {
        let canvas = self.canvas();
        let whole = self.state.layers.len() == 1;
        let Some(l) = self.state.active_layer() else { return false };
        if l.locked || (new_w >= l.pixels.w && new_h >= l.pixels.h) {
            return false;
        }
        let whole = whole && l.rect() == canvas;
        let before = self.begin();
        let (px, mask) = seam_carve(&l.pixels, l.mask.as_deref(), new_w, new_h);
        let (w, h) = (px.w, px.h);
        let l = self.state.active_layer_mut().unwrap();
        l.pixels = Arc::new(px);
        l.mask = mask.map(Arc::new);
        l.text = None;
        l.touch();
        if whole {
            (self.state.width, self.state.height) = (w, h);
            self.state.selection = None;
        }
        self.commit("Content-Aware Scale", before);
        true
    }
}
