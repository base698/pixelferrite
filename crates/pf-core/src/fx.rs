//! The filters behind [`crate::filter::Filter`], except blur and edges.
//!
//! All of them work in place on `C` interleaved float channels of a `w` x `h`
//! image. With four channels the colour is premultiplied by alpha (0..=255
//! scale) and channel 3 is alpha; with one channel it is a plain mask.

use rayon::prelude::*;

use crate::buf::Pixmap;
use crate::filter::{box_h, box_v, gaussian_blur};

const LUMA: [f32; 3] = [0.299, 0.587, 0.114];

/// Straight (un-premultiplied) colour of a pixel as RGB, and its alpha 0..=1.
#[inline]
fn straight<const C: usize>(p: &[f32]) -> ([f32; 3], f32) {
    if C < 4 {
        return ([p[0]; 3], 1.0);
    }
    let a = p[3] / 255.0;
    if a <= 0.0 { ([0.0; 3], 0.0) } else { ([p[0] / a, p[1] / a, p[2] / a], a) }
}

#[inline]
fn store<const C: usize>(p: &mut [f32], rgb: [f32; 3], a: f32) {
    if C < 4 {
        p[0] = (LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2]).clamp(0.0, 255.0);
    } else {
        for c in 0..3 {
            p[c] = rgb[c].clamp(0.0, 255.0) * a;
        }
    }
}

#[inline]
fn luma(rgb: [f32; 3]) -> f32 {
    LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2]
}

/// Apply `f` to every pixel's straight colour.
fn map_color<const C: usize>(data: &mut [f32], f: impl Fn(usize, [f32; 3]) -> [f32; 3] + Sync) {
    data.par_chunks_exact_mut(C).enumerate().for_each(|(i, p)| {
        let (rgb, a) = straight::<C>(p);
        if a > 0.0 {
            store::<C>(p, f(i, rgb), a);
        }
    });
}

/// Change each pixel's brightness to `f(index, old)` while keeping its hue.
fn map_luma<const C: usize>(data: &mut [f32], f: impl Fn(usize, f32) -> f32 + Sync) {
    map_color::<C>(data, |i, rgb| {
        let y = luma(rgb);
        let ny = f(i, y).clamp(0.0, 255.0);
        if y > 1.0 {
            // Scale, but fall back towards a plain shift where scaling would clip.
            let k = ny / y;
            let scaled = rgb.map(|v| v * k);
            let over = scaled.iter().fold(0f32, |m, v| m.max(*v));
            if over <= 255.0 { scaled } else { rgb.map(|v| v + (ny - y)) }
        } else {
            [ny; 3]
        }
    });
}

fn luma_plane<const C: usize>(data: &[f32]) -> Vec<f32> {
    data.par_chunks_exact(C).map(|p| luma(straight::<C>(p).0)).collect()
}

/// 256-bin brightness histogram of the visible pixels.
fn histogram<const C: usize>(data: &[f32]) -> [f64; 256] {
    let mut h = [0f64; 256];
    for p in data.chunks_exact(C) {
        let (rgb, a) = straight::<C>(p);
        if a > 0.0 {
            h[luma(rgb).clamp(0.0, 255.0) as usize] += 1.0;
        }
    }
    h
}

pub fn unsharp_mask<const C: usize>(data: &mut Vec<f32>, w: usize, h: usize, radius: f32, amount: f32, threshold: f32) {
    let mut blurred = data.clone();
    gaussian_blur::<C>(&mut blurred, w, h, radius);
    data.par_chunks_exact_mut(C).zip(blurred.par_chunks_exact(C)).for_each(|(p, b)| {
        let colour = if C >= 4 { 3 } else { C };
        for c in 0..colour {
            let d = p[c] - b[c];
            if d.abs() >= threshold {
                let top = if C >= 4 { p[3] } else { 255.0 };
                p[c] = (p[c] + d * amount).clamp(0.0, top);
            }
        }
    });
}

/// Bilateral filter: smooths flat areas, keeps edges. Large radii sample a
/// sparse grid so the cost stays bounded.
pub fn surface_blur<const C: usize>(data: &mut Vec<f32>, w: usize, h: usize, radius: f32, tolerance: f32) {
    let r = (radius * 2.0).ceil().max(1.0) as isize;
    let step = (r / 4).max(1);
    let src = data.clone();
    let inv_s = 1.0 / (2.0 * radius.max(0.3) * radius.max(0.3));
    let inv_t = 1.0 / (2.0 * tolerance.max(0.5) * tolerance.max(0.5) * C as f32);
    data.par_chunks_mut(w * C).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let c0 = &src[(y * w + x) * C..][..C];
            let (mut acc, mut sum) = ([0f32; 4], 0f32);
            let mut dy = -r;
            while dy <= r {
                let sy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                let mut dx = -r;
                while dx <= r {
                    let sx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                    let p = &src[(sy * w + sx) * C..][..C];
                    let dist: f32 = (0..C).map(|c| (p[c] - c0[c]).powi(2)).sum();
                    let wgt = (-((dx * dx + dy * dy) as f32) * inv_s - dist * inv_t).exp();
                    for c in 0..C {
                        acc[c] += p[c] * wgt;
                    }
                    sum += wgt;
                    dx += step;
                }
                dy += step;
            }
            for c in 0..C {
                row[x * C + c] = acc[c] / sum;
            }
        }
    });
}

/// Non-local means: each pixel becomes an average of pixels whose
/// surroundings look alike, which removes noise but keeps texture and edges.
pub fn denoise<const C: usize>(data: &mut Vec<f32>, w: usize, h: usize, strength: f32) {
    const SEARCH: isize = 3;
    const PATCH: usize = 2;
    let src = data.clone();
    let n = w * h;
    let mut acc = vec![0f32; n * C];
    let mut wsum = vec![0f32; n];
    let h2 = (strength.max(0.1) * strength.max(0.1)) * C as f32;
    let mut dist = vec![0f32; n];
    let mut tmp = vec![0f32; n];
    for dy in -SEARCH..=SEARCH {
        for dx in -SEARCH..=SEARCH {
            let at = |x: usize, y: usize| {
                let sx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                let sy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                (sy * w + sx) * C
            };
            dist.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for x in 0..w {
                    let (a, b) = (&src[(y * w + x) * C..][..C], &src[at(x, y)..][..C]);
                    row[x] = (0..C).map(|c| (a[c] - b[c]).powi(2)).sum();
                }
            });
            // Averaging the differences over a patch compares neighbourhoods.
            box_h::<1>(&dist, &mut tmp, w, PATCH);
            box_v::<1>(&tmp, &mut dist, w, h, PATCH);
            acc.par_chunks_mut(w * C).zip(wsum.par_chunks_mut(w)).enumerate().for_each(|(y, (arow, wrow))| {
                for x in 0..w {
                    let wgt = (-dist[y * w + x] / h2).exp();
                    let p = &src[at(x, y)..][..C];
                    for c in 0..C {
                        arow[x * C + c] += p[c] * wgt;
                    }
                    wrow[x] += wgt;
                }
            });
        }
    }
    data.par_chunks_exact_mut(C).zip(acc.par_chunks_exact(C)).zip(&wsum).for_each(|((p, a), s)| {
        for c in 0..C {
            p[c] = a[c] / s;
        }
    });
}

/// Fill the selected pixels from what surrounds them, working inwards from
/// the edge of the hole. `hole` is the selection weight per pixel.
pub fn inpaint<const C: usize>(data: &mut [f32], w: usize, h: usize, radius: f32, hole: Option<&[f32]>) {
    let Some(hole) = hole else { return };
    let mut known: Vec<bool> = hole.iter().map(|v| *v < 0.5).collect();
    if known.iter().all(|k| *k) || known.iter().all(|k| !*k) {
        return;
    }
    let r = radius.round().clamp(1.0, 24.0) as isize;
    let neighbours = |i: usize| {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        [(-1, 0), (1, 0), (0, -1), (0, 1)]
            .into_iter()
            .filter(move |(dx, dy)| x + dx >= 0 && y + dy >= 0 && x + dx < w as isize && y + dy < h as isize)
            .map(move |(dx, dy)| ((y + dy) * w as isize + x + dx) as usize)
    };
    let mut front: Vec<usize> = (0..w * h).filter(|i| !known[*i] && neighbours(*i).any(|j| known[j])).collect();
    while !front.is_empty() {
        // Everything on the current rim is filled from pixels known before this ring.
        let filled: Vec<(usize, [f32; 4])> = front
            .par_iter()
            .map(|&i| {
                let (x, y) = ((i % w) as isize, (i / w) as isize);
                let (mut acc, mut sum) = ([0f32; 4], 0f32);
                for dy in -r..=r {
                    for dx in -r..=r {
                        let (sx, sy) = (x + dx, y + dy);
                        if sx < 0 || sy < 0 || sx >= w as isize || sy >= h as isize {
                            continue;
                        }
                        let j = sy as usize * w + sx as usize;
                        if known[j] {
                            let d2 = (dx * dx + dy * dy) as f32;
                            let wgt = 1.0 / (d2 * d2).max(1.0);
                            for c in 0..C {
                                acc[c] += data[j * C + c] * wgt;
                            }
                            sum += wgt;
                        }
                    }
                }
                (i, acc.map(|v| if sum > 0.0 { v / sum } else { 0.0 }))
            })
            .collect();
        for (i, v) in &filled {
            data[i * C..i * C + C].copy_from_slice(&v[..C]);
        }
        for (i, _) in &filled {
            known[*i] = true;
        }
        let mut next: Vec<usize> = front.iter().flat_map(|i| neighbours(*i)).filter(|j| !known[*j]).collect();
        next.sort_unstable();
        next.dedup();
        front = next;
    }
}

/// Stretch brightness so the darkest and lightest `clip` percent hit black and white.
pub fn auto_contrast<const C: usize>(data: &mut [f32], clip: f32) {
    let hist = histogram::<C>(data);
    let total: f64 = hist.iter().sum();
    if total == 0.0 {
        return;
    }
    let cut = total * (clip as f64 / 100.0).clamp(0.0, 0.4);
    let edge = |order: &[usize]| {
        let mut seen = 0.0;
        order.iter().copied().find(|i| {
            seen += hist[*i];
            seen > cut
        })
        .unwrap_or(0) as f32
    };
    let up: Vec<usize> = (0..256).collect();
    let down: Vec<usize> = (0..256).rev().collect();
    let (lo, hi) = (edge(&up), edge(&down));
    if hi - lo < 1.0 {
        return;
    }
    let k = 255.0 / (hi - lo);
    map_color::<C>(data, |_, rgb| rgb.map(|v| (v - lo) * k));
}

/// Histogram equalisation: spread brightness evenly over the range.
pub fn equalize<const C: usize>(data: &mut [f32], amount: f32) {
    let hist = histogram::<C>(data);
    let total: f64 = hist.iter().sum();
    if total == 0.0 {
        return;
    }
    let mut lut = [0f32; 256];
    let mut run = 0.0;
    for i in 0..256 {
        run += hist[i];
        lut[i] = (run / total * 255.0) as f32;
    }
    map_luma::<C>(data, |_, y| y + (lut[y.clamp(0.0, 255.0) as usize] - y) * amount);
}

/// CLAHE: equalise brightness within a grid of tiles, limiting how much any
/// tone can be amplified, and blend between tiles so no seams show.
pub fn local_contrast<const C: usize>(data: &mut [f32], w: usize, h: usize, clip: f32, tiles: u32) {
    let t = (tiles.clamp(2, 32) as usize).min(w).min(h).max(1);
    let y_plane = luma_plane::<C>(data);
    let alpha: Vec<bool> = data.chunks_exact(C).map(|p| C < 4 || p[3] > 0.0).collect();
    let (tw, th) = (w as f32 / t as f32, h as f32 / t as f32);
    let luts: Vec<[f32; 256]> = (0..t * t)
        .into_par_iter()
        .map(|ti| {
            let (tx, ty) = (ti % t, ti / t);
            let (x0, x1) = ((tx as f32 * tw) as usize, (((tx + 1) as f32 * tw) as usize).min(w));
            let (y0, y1) = ((ty as f32 * th) as usize, (((ty + 1) as f32 * th) as usize).min(h));
            let mut hist = [0f32; 256];
            let mut n = 0f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    if alpha[y * w + x] {
                        hist[y_plane[y * w + x].clamp(0.0, 255.0) as usize] += 1.0;
                        n += 1.0;
                    }
                }
            }
            let mut lut: [f32; 256] = std::array::from_fn(|i| i as f32);
            if n > 0.0 {
                // Clip the peaks and hand the excess out evenly.
                let limit = (clip.max(1.0) * n / 256.0).max(1.0);
                let excess: f32 = hist.iter().map(|v| (v - limit).max(0.0)).sum();
                let mut run = 0.0;
                for i in 0..256 {
                    run += hist[i].min(limit) + excess / 256.0;
                    lut[i] = run / n * 255.0;
                }
            }
            lut
        })
        .collect();
    map_luma::<C>(data, |i, y| {
        let (x, yy) = (i % w, i / w);
        // Position in tile-centre coordinates.
        let (fx, fy) = (((x as f32 + 0.5) / tw - 0.5).clamp(0.0, t as f32 - 1.0), ((yy as f32 + 0.5) / th - 0.5).clamp(0.0, t as f32 - 1.0));
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(t - 1), (y0 + 1).min(t - 1));
        let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
        let b = y.clamp(0.0, 255.0) as usize;
        let top = luts[y0 * t + x0][b] * (1.0 - ax) + luts[y0 * t + x1][b] * ax;
        let bot = luts[y1 * t + x0][b] * (1.0 - ax) + luts[y1 * t + x1][b] * ax;
        top * (1.0 - ay) + bot * ay
    });
}

pub fn threshold<const C: usize>(data: &mut [f32], level: f32) {
    map_color::<C>(data, |_, rgb| if luma(rgb) >= level { [255.0; 3] } else { [0.0; 3] });
}

/// Black and white, deciding each pixel against the brightness around it;
/// copes with uneven lighting on scans and photos of pages.
pub fn adaptive_threshold<const C: usize>(data: &mut [f32], w: usize, h: usize, radius: f32, offset: f32) {
    let y = luma_plane::<C>(data);
    let mut mean = y.clone();
    gaussian_blur::<1>(&mut mean, w, h, radius.max(1.0));
    map_color::<C>(data, |i, _| if y[i] > mean[i] - offset { [255.0; 3] } else { [0.0; 3] });
}

fn to_ycc(rgb: [f32; 3]) -> [f32; 3] {
    let y = luma(rgb);
    [y, (rgb[2] - y) * 0.564, (rgb[0] - y) * 0.713]
}

fn from_ycc(c: [f32; 3]) -> [f32; 3] {
    let (r, b) = (c[0] + c[2] / 0.713, c[0] + c[1] / 0.564);
    [r, (c[0] - LUMA[0] * r - LUMA[2] * b) / LUMA[1], b]
}

fn ycc_stats(pixels: impl Iterator<Item = [f32; 3]>) -> Option<([f32; 3], [f32; 3])> {
    let (mut n, mut sum, mut sq) = (0f64, [0f64; 3], [0f64; 3]);
    for rgb in pixels {
        let c = to_ycc(rgb);
        for i in 0..3 {
            sum[i] += c[i] as f64;
            sq[i] += (c[i] * c[i]) as f64;
        }
        n += 1.0;
    }
    (n > 0.0).then(|| {
        let mean: [f64; 3] = std::array::from_fn(|i| sum[i] / n);
        (mean.map(|v| v as f32), std::array::from_fn(|i| ((sq[i] / n - mean[i] * mean[i]).max(0.0).sqrt()) as f32))
    })
}

/// Average colour and spread of an image's visible pixels, as the reference
/// for [`match_colors`].
pub fn color_stats(px: &Pixmap) -> Option<([f32; 3], [f32; 3])> {
    ycc_stats(px.data.chunks_exact(4).filter(|p| p[3] > 8).map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]))
}

/// Shift the picture's overall colour and contrast towards a reference's.
pub fn match_colors<const C: usize>(data: &mut [f32], mean: [f32; 3], dev: [f32; 3], amount: f32) {
    let Some((m, d)) = ycc_stats(data.chunks_exact(C).map(|p| straight::<C>(p)).filter(|(_, a)| *a > 0.0).map(|(rgb, _)| rgb)) else { return };
    map_color::<C>(data, |_, rgb| {
        let c = to_ycc(rgb);
        let moved: [f32; 3] = std::array::from_fn(|i| (c[i] - m[i]) * (dev[i] / d[i].max(1.0)).clamp(0.25, 4.0) + mean[i]);
        let out = from_ycc(moved);
        std::array::from_fn(|i| rgb[i] + (out[i] - rgb[i]) * amount)
    });
}

/// Sample `src` at a fractional position with bilinear filtering; outside is `edge`.
#[inline]
pub(crate) fn bilinear<const C: usize>(src: &[f32], w: usize, h: usize, x: f32, y: f32, clamp: bool) -> [f32; 4] {
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (ax, ay) = (fx - x0, fy - y0);
    let mut out = [0f32; 4];
    for (dx, dy, wgt) in [(0, 0, (1.0 - ax) * (1.0 - ay)), (1, 0, ax * (1.0 - ay)), (0, 1, (1.0 - ax) * ay), (1, 1, ax * ay)] {
        let (mut sx, mut sy) = (x0 as isize + dx, y0 as isize + dy);
        if clamp {
            (sx, sy) = (sx.clamp(0, w as isize - 1), sy.clamp(0, h as isize - 1));
        } else if sx < 0 || sy < 0 || sx >= w as isize || sy >= h as isize {
            continue;
        }
        let p = &src[(sy as usize * w + sx as usize) * C..][..C];
        for c in 0..C {
            out[c] += p[c] * wgt;
        }
    }
    out
}

/// Radial lens correction: positive `amount` pulls a bulging (barrel) image
/// straight, negative corrects pincushion.
pub fn lens_distortion<const C: usize>(data: &mut Vec<f32>, w: usize, h: usize, amount: f32) {
    let src = data.clone();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let norm = 1.0 / (cx * cx + cy * cy);
    let scale = 1.0 / (1.0 + amount * 0.5);
    data.par_chunks_mut(w * C).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let f = (1.0 + amount * (dx * dx + dy * dy) * norm) * scale;
            let v = bilinear::<C>(&src, w, h, cx + dx * f, cy + dy * f, C < 4);
            row[x * C..x * C + C].copy_from_slice(&v[..C]);
        }
    });
}
