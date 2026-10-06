use rayon::prelude::*;

use crate::blend::BlendMode;
use crate::buf::Pixmap;
use crate::document::{DocState, Layer};
use crate::geom::IRect;

const INV: f32 = 1.0 / 255.0;

/// Accumulate one layer's row into `acc` (premultiplied RGBA floats).
/// `acc[i]` corresponds to document x = `x0 + i` on document row `y`.
fn layer_row(layer: &Layer, y: i32, x0: i32, acc: &mut [[f32; 4]]) {
    let by = y - layer.y;
    if by < 0 || by >= layer.pixels.h as i32 {
        return;
    }
    let xs = x0.max(layer.x);
    let xe = (x0 + acc.len() as i32).min(layer.x + layer.pixels.w as i32);
    if xs >= xe {
        return;
    }
    let n = (xe - xs) as usize;
    let p0 = layer.pixels.idx(xs - layer.x, by);
    let src = &layer.pixels.data[p0..p0 + n * 4];
    let mask = match &layer.mask {
        Some(m) if layer.mask_enabled => {
            let m0 = m.idx(xs - layer.x, by);
            Some(&m.data[m0..m0 + n])
        }
        _ => None,
    };
    let acc = &mut acc[(xs - x0) as usize..][..n];
    let op = layer.opacity * INV;
    for (i, (d, s)) in acc.iter_mut().zip(src.chunks_exact(4)).enumerate() {
        let mut sa = s[3] as f32 * op;
        if let Some(m) = mask {
            sa *= m[i] as f32 * INV;
        }
        if sa <= 0.0 {
            continue;
        }
        let sc = [s[0] as f32 * INV, s[1] as f32 * INV, s[2] as f32 * INV];
        let ba = d[3];
        let inv = 1.0 - sa;
        if layer.blend == BlendMode::Normal || ba <= 0.0 {
            for c in 0..3 {
                d[c] = sc[c] * sa + d[c] * inv;
            }
        } else {
            let bc = [d[0] / ba, d[1] / ba, d[2] / ba];
            let bl = layer.blend.blend(bc, sc);
            for c in 0..3 {
                d[c] = sa * ((1.0 - ba) * sc[c] + ba * bl[c]) + inv * d[c];
            }
        }
        d[3] = sa + ba * inv;
    }
}

/// Composite all visible layers over `rect` (document space; may extend past
/// the canvas) into `out` as straight-alpha RGBA8.
pub fn composite_rect(state: &DocState, rect: IRect, out: &mut [u8]) {
    let w = rect.width() as usize;
    if w == 0 {
        return;
    }
    assert_eq!(out.len(), w * rect.height() as usize * 4);
    let layers: Vec<&Layer> = state.layers.iter().filter(|l| l.visible && l.opacity > 0.0).collect();
    out.par_chunks_mut(w * 4).enumerate().for_each_init(
        || vec![[0f32; 4]; w],
        |acc, (row, out)| {
            acc.fill([0.0; 4]);
            let y = rect.y0 + row as i32;
            for l in &layers {
                layer_row(l, y, rect.x0, acc);
            }
            for (o, a) in out.chunks_exact_mut(4).zip(acc.iter()) {
                if a[3] <= 0.0 {
                    o.copy_from_slice(&[0; 4]);
                } else {
                    let k = 255.0 / a[3];
                    o[0] = (a[0] * k + 0.5).min(255.0) as u8;
                    o[1] = (a[1] * k + 0.5).min(255.0) as u8;
                    o[2] = (a[2] * k + 0.5).min(255.0) as u8;
                    o[3] = (a[3] * 255.0 + 0.5).min(255.0) as u8;
                }
            }
        },
    );
}

/// The flattened canvas.
pub fn flatten(state: &DocState) -> Pixmap {
    let mut out = Pixmap::new(state.width, state.height);
    composite_rect(state, state.canvas(), &mut out.data);
    out
}

/// One layer's pixels (mask applied, opacity ignored) resampled onto the canvas frame.
pub fn layer_on_canvas(state: &DocState, layer: &Layer) -> Pixmap {
    let mut l = layer.clone();
    l.opacity = 1.0;
    l.visible = true;
    l.blend = BlendMode::Normal;
    let tmp = DocState { layers: vec![l], selection: None, ..*state };
    flatten(&tmp)
}

/// Composite colour at a document pixel, if inside the canvas.
pub fn sample(state: &DocState, x: i32, y: i32) -> Option<[u8; 4]> {
    if !state.canvas().contains(x, y) {
        return None;
    }
    let mut px = [0u8; 4];
    composite_rect(state, IRect::new(x, y, x + 1, y + 1), &mut px);
    Some(px)
}
