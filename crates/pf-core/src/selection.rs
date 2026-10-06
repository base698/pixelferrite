//! Selection masks: shapes, flood ("magic wand") selection and outlines.

use crate::buf::{Mask, Pixmap};
use crate::geom::IRect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Replace,
    Add,
    Subtract,
    Intersect,
}

/// Bounding box of all non-zero pixels.
pub fn bounds(m: &Mask) -> Option<IRect> {
    let w = m.w as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, -1, -1);
    for (y, row) in m.data.chunks_exact(w.max(1)).enumerate() {
        let Some(a) = row.iter().position(|&v| v != 0) else { continue };
        let b = row.iter().rposition(|&v| v != 0).unwrap();
        x0 = x0.min(a as i32);
        x1 = x1.max(b as i32);
        y0 = y0.min(y as i32);
        y1 = y as i32;
    }
    (y1 >= 0).then(|| IRect::new(x0, y0, x1 + 1, y1 + 1))
}

/// Combine `new` into `cur`; returns `None` when the result selects nothing.
pub fn combine(cur: Option<&Mask>, new: &Mask, mode: Combine) -> Option<Mask> {
    let out = match (cur, mode) {
        (None, Combine::Replace | Combine::Add) | (Some(_), Combine::Replace) => new.clone(),
        (None, Combine::Subtract | Combine::Intersect) => return None,
        (Some(c), _) => {
            let mut out = c.clone();
            for (o, &n) in out.data.iter_mut().zip(&new.data) {
                *o = match mode {
                    Combine::Add => (*o).max(n),
                    Combine::Subtract => (*o).min(255 - n),
                    _ => (*o).min(n),
                };
            }
            out
        }
    };
    out.data.iter().any(|&v| v != 0).then_some(out)
}

pub fn invert(cur: Option<&Mask>, w: u32, h: u32) -> Option<Mask> {
    match cur {
        None => Some(Mask::filled(w, h, [255])),
        Some(c) => {
            let mut out = c.clone();
            out.data.iter_mut().for_each(|v| *v = 255 - *v);
            out.data.iter().any(|&v| v != 0).then_some(out)
        }
    }
}

pub fn rect_mask(w: u32, h: u32, r: IRect) -> Mask {
    let mut m = Mask::new(w, h);
    let r = r.intersect(m.rect());
    for y in r.y0..r.y1 {
        let i = m.idx(r.x0, y);
        m.data[i..i + r.width() as usize].fill(255);
    }
    m
}

/// Anti-aliased ellipse inscribed in the float box `(x0, y0)-(x1, y1)`.
pub fn ellipse_mask(w: u32, h: u32, x0: f32, y0: f32, x1: f32, y1: f32) -> Mask {
    let mut m = Mask::new(w, h);
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let (rx, ry) = ((x1 - x0).abs() * 0.5, (y1 - y0).abs() * 0.5);
    if rx < 0.5 || ry < 0.5 {
        return m;
    }
    let r = IRect::enclosing(x0, y0, x1, y1).intersect(m.rect());
    let rmin = rx.min(ry);
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let dx = (x as f32 + 0.5 - cx) / rx;
            let dy = (y as f32 + 0.5 - cy) / ry;
            let f = (dx * dx + dy * dy).sqrt();
            let a = ((1.0 - f) * rmin + 0.5).clamp(0.0, 1.0);
            if a > 0.0 {
                m.set(x, y, [(a * 255.0 + 0.5) as u8]);
            }
        }
    }
    m
}

/// Even-odd filled polygon (the lasso tool).
pub fn polygon_mask(w: u32, h: u32, pts: &[(f32, f32)]) -> Mask {
    let mut m = Mask::new(w, h);
    if pts.len() < 3 {
        return m;
    }
    let ymin = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor().max(0.0) as i32;
    let ymax = (pts.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil() as i32).min(h as i32);
    let mut xs: Vec<f32> = Vec::new();
    for y in ymin..ymax {
        let yc = y as f32 + 0.5;
        xs.clear();
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            if (a.1 <= yc) != (b.1 <= yc) {
                xs.push(a.0 + (yc - a.1) / (b.1 - a.1) * (b.0 - a.0));
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        for pair in xs.chunks_exact(2) {
            let xa = ((pair[0] - 0.5).ceil() as i32).max(0);
            let xb = ((pair[1] - 0.5).ceil() as i32).min(w as i32);
            if xa < xb {
                let i = m.idx(xa, y);
                m.data[i..i + (xb - xa) as usize].fill(255);
            }
        }
    }
    m
}

#[inline]
fn color_dist(a: [u8; 4], b: [u8; 4]) -> u8 {
    if a[3] == 0 && b[3] == 0 {
        return 0;
    }
    let d = |i: usize| a[i].abs_diff(b[i]);
    d(0).max(d(1)).max(d(2)).max(d(3))
}

/// Select pixels of `src` similar to the one at `seed` ("magic wand").
/// `bound` restricts the search area; the result is `src`-sized.
pub fn flood_mask(src: &Pixmap, seed: (i32, i32), tolerance: u8, contiguous: bool, bound: Option<IRect>) -> Mask {
    let mut m = Mask::new(src.w, src.h);
    let b = bound.map_or(src.rect(), |b| b.intersect(src.rect()));
    if !b.contains(seed.0, seed.1) {
        return m;
    }
    let refc = src.px(seed.0, seed.1);
    let hit = |x: i32, y: i32| color_dist(src.px(x, y), refc) <= tolerance;
    if !contiguous {
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                if hit(x, y) {
                    m.set(x, y, [255]);
                }
            }
        }
        return m;
    }
    // Scanline flood fill.
    let mut stack = vec![seed];
    while let Some((x, y)) = stack.pop() {
        if m.px(x, y)[0] != 0 || !hit(x, y) {
            continue;
        }
        let (mut xl, mut xr) = (x, x);
        while xl > b.x0 && hit(xl - 1, y) {
            xl -= 1;
        }
        while xr + 1 < b.x1 && hit(xr + 1, y) {
            xr += 1;
        }
        let i = m.idx(xl, y);
        m.data[i..=i + (xr - xl) as usize].fill(255);
        for ny in [y - 1, y + 1] {
            if ny < b.y0 || ny >= b.y1 {
                continue;
            }
            let mut in_run = false;
            for nx in xl..=xr {
                let ok = m.px(nx, ny)[0] == 0 && hit(nx, ny);
                if ok && !in_run {
                    stack.push((nx, ny));
                }
                in_run = ok;
            }
        }
    }
    m
}

/// Axis-aligned outline segments `[x0, y0, x1, y1]` (pixel-corner coordinates)
/// between selected (>= 128) and unselected pixels, with collinear runs merged.
pub fn outline(m: &Mask) -> Vec<[i32; 4]> {
    let (w, h) = (m.w as i32, m.h as i32);
    let on = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h && m.data[(y * w + x) as usize] >= 128;
    let mut segs = Vec::new();
    for y in 0..=h {
        let mut start = None;
        for x in 0..=w {
            let edge = x < w && on(x, y - 1) != on(x, y);
            match (edge, start) {
                (true, None) => start = Some(x),
                (false, Some(s)) => {
                    segs.push([s, y, x, y]);
                    start = None;
                }
                _ => {}
            }
        }
    }
    for x in 0..=w {
        let mut start = None;
        for y in 0..=h {
            let edge = y < h && on(x - 1, y) != on(x, y);
            match (edge, start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    segs.push([x, s, x, y]);
                    start = None;
                }
                _ => {}
            }
        }
    }
    segs
}
