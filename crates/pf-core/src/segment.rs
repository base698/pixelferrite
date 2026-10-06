//! Smart selection: subject cut-out (GrabCut), edge-bounded regions
//! (watershed) and tracing a selection's outline.

use std::collections::{HashMap, VecDeque};

use image::imageops::{self, FilterType};
use image::{GrayImage, RgbaImage};

use crate::buf::{Mask, Pixmap};
use crate::filter::gaussian_blur;
use crate::geom::IRect;
use crate::selection;

// ---------------------------------------------------------------- GrabCut

const K: usize = 5;

/// A mixture of `K` colour clusters (diagonal covariance).
struct Gmm {
    weight: [f32; K],
    mean: [[f32; 3]; K],
    var: [[f32; 3]; K],
}

impl Gmm {
    fn fit(samples: &[[f32; 3]]) -> Self {
        // k-means, seeded by spreading the centres over the samples.
        let n = samples.len().max(1);
        let mut mean: [[f32; 3]; K] = std::array::from_fn(|k| samples.get(k * n / K + n / (2 * K)).copied().unwrap_or([128.0; 3]));
        let mut assign = vec![0usize; samples.len()];
        let nearest = |mean: &[[f32; 3]; K], s: &[f32; 3]| {
            (0..K).min_by(|a, b| dist2(&mean[*a], s).total_cmp(&dist2(&mean[*b], s))).unwrap()
        };
        for _ in 0..6 {
            let (mut sum, mut cnt) = ([[0f32; 3]; K], [0f32; K]);
            for (s, a) in samples.iter().zip(&mut assign) {
                *a = nearest(&mean, s);
                cnt[*a] += 1.0;
                for c in 0..3 {
                    sum[*a][c] += s[c];
                }
            }
            for k in 0..K {
                if cnt[k] > 0.0 {
                    mean[k] = sum[k].map(|v| v / cnt[k]);
                }
            }
        }
        let (mut var, mut cnt) = ([[0f32; 3]; K], [0f32; K]);
        for (s, a) in samples.iter().zip(&assign) {
            cnt[*a] += 1.0;
            for c in 0..3 {
                var[*a][c] += (s[c] - mean[*a][c]).powi(2);
            }
        }
        for k in 0..K {
            var[k] = var[k].map(|v| if cnt[k] > 0.0 { v / cnt[k] + 16.0 } else { 1e4 });
        }
        Self { weight: cnt.map(|c| c / n as f32), mean, var }
    }

    /// Negative log likelihood of a colour.
    fn cost(&self, s: &[f32; 3]) -> f32 {
        let mut p = 0f32;
        for k in 0..K {
            if self.weight[k] > 0.0 {
                let e: f32 = (0..3).map(|c| (s[c] - self.mean[k][c]).powi(2) / self.var[k][c]).sum();
                let norm = (self.var[k][0] * self.var[k][1] * self.var[k][2]).sqrt();
                p += self.weight[k] * (-0.5 * e).exp() / norm;
            }
        }
        -(p.max(1e-30)).ln()
    }
}

fn dist2(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    (0..3).map(|c| (a[c] - b[c]).powi(2)).sum()
}

/// Dinic's max-flow, for the minimum cut between "subject" and "background".
struct Flow {
    head: Vec<u32>,
    next: Vec<u32>,
    to: Vec<u32>,
    cap: Vec<f32>,
}

const NONE: u32 = u32::MAX;

impl Flow {
    fn new(nodes: usize) -> Self {
        Self { head: vec![NONE; nodes], next: Vec::new(), to: Vec::new(), cap: Vec::new() }
    }

    fn edge(&mut self, a: usize, b: usize, ab: f32, ba: f32) {
        for (from, to, c) in [(a, b, ab), (b, a, ba)] {
            self.to.push(to as u32);
            self.cap.push(c);
            self.next.push(self.head[from]);
            self.head[from] = self.to.len() as u32 - 1;
        }
    }

    /// Saturate the graph; returns which nodes remain reachable from `s`.
    fn min_cut(&mut self, s: usize, t: usize) -> Vec<bool> {
        let n = self.head.len();
        let mut level = vec![-1i32; n];
        loop {
            level.fill(-1);
            level[s] = 0;
            let mut q = VecDeque::from([s]);
            while let Some(u) = q.pop_front() {
                let mut e = self.head[u];
                while e != NONE {
                    let v = self.to[e as usize] as usize;
                    if self.cap[e as usize] > 1e-6 && level[v] < 0 {
                        level[v] = level[u] + 1;
                        q.push_back(v);
                    }
                    e = self.next[e as usize];
                }
            }
            if level[t] < 0 {
                return level.iter().map(|l| *l >= 0).collect();
            }
            // Blocking flow by iterative depth-first search.
            let mut it = self.head.clone();
            loop {
                let mut path: Vec<u32> = Vec::new();
                let mut u = s;
                while u != t {
                    let mut advanced = false;
                    while it[u] != NONE {
                        let e = it[u] as usize;
                        let v = self.to[e] as usize;
                        if self.cap[e] > 1e-6 && level[v] == level[u] + 1 {
                            path.push(e as u32);
                            u = v;
                            advanced = true;
                            break;
                        }
                        it[u] = self.next[e];
                    }
                    if !advanced {
                        // Dead end: step back and skip the edge that led here.
                        match path.pop() {
                            Some(e) => {
                                u = self.to[(e ^ 1) as usize] as usize;
                                it[u] = self.next[e as usize];
                            }
                            None => break,
                        }
                    }
                }
                if u != t {
                    break;
                }
                let push = path.iter().map(|e| self.cap[*e as usize]).fold(f32::MAX, f32::min);
                for e in &path {
                    self.cap[*e as usize] -= push;
                    self.cap[(*e ^ 1) as usize] += push;
                }
            }
        }
    }
}

/// Select the subject inside `rect`: everything outside the rectangle is
/// taken to be background, and what's inside is split by colour and edges.
/// Returns a mask the size of `px`.
pub fn grabcut(px: &Pixmap, rect: IRect) -> Mask {
    let rect = rect.intersect(px.rect());
    let mut out = Mask::new(px.w, px.h);
    if rect.width() < 4 || rect.height() < 4 {
        return out;
    }
    // Work on the rectangle plus a border of known background, at a size
    // the cut can handle quickly.
    let pad = (rect.width().max(rect.height()) / 4).max(8);
    let area = rect.expand(pad).intersect(px.rect());
    let crop = px.reframed(area, [0; 4]);
    let s = (320.0 / crop.w.max(crop.h) as f32).min(1.0);
    let (w, h) = (((crop.w as f32 * s) as u32).max(8), ((crop.h as f32 * s) as u32).max(8));
    let small = imageops::resize(&RgbaImage::from_raw(crop.w, crop.h, crop.data.clone()).expect("buffer size"), w, h, FilterType::Triangle);
    let (w, h) = (w as usize, h as usize);
    // Transparent pixels read as a colour of their own.
    let col: Vec<[f32; 3]> = small
        .pixels()
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [0, 1, 2].map(|c| p[c] as f32 * a + 255.0 * (1.0 - a))
        })
        .collect();
    let inside = |x: usize, y: usize| {
        let (dx, dy) = (area.x0 as f32 + (x as f32 + 0.5) / s, area.y0 as f32 + (y as f32 + 0.5) / s);
        dx >= rect.x0 as f32 && dx < rect.x1 as f32 && dy >= rect.y0 as f32 && dy < rect.y1 as f32
    };
    let boxed: Vec<bool> = (0..w * h).map(|i| inside(i % w, i / w)).collect();
    if boxed.iter().all(|b| *b) || !boxed.iter().any(|b| *b) {
        // No background to learn from (the rectangle covers everything).
        return selection::rect_mask(px.w, px.h, rect);
    }

    // Neighbouring pixels of similar colour are expensive to separate.
    let pairs: Vec<(usize, usize)> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            [(x + 1 < w).then(|| (i, i + 1)), (y + 1 < h).then(|| (i, i + w))].into_iter().flatten()
        })
        .collect();
    let mean_d: f32 = pairs.iter().map(|(a, b)| dist2(&col[*a], &col[*b])).sum::<f32>() / pairs.len().max(1) as f32;
    let beta = 1.0 / (2.0 * mean_d.max(1.0));
    let smooth: Vec<f32> = pairs.iter().map(|(a, b)| 50.0 * (-beta * dist2(&col[*a], &col[*b])).exp()).collect();

    let mut fg = boxed.clone();
    for _ in 0..4 {
        let pick = |want: bool| -> Vec<[f32; 3]> { col.iter().zip(&fg).filter(|(_, f)| **f == want).map(|(c, _)| *c).collect() };
        let (fg_s, bg_s) = (pick(true), pick(false));
        if fg_s.len() < K || bg_s.len() < K {
            break;
        }
        let (gf, gb) = (Gmm::fit(&fg_s), Gmm::fit(&bg_s));
        let (src, sink) = (w * h, w * h + 1);
        let mut g = Flow::new(w * h + 2);
        for i in 0..w * h {
            // Cutting a pixel from the subject costs how unlike the background it is, and vice versa.
            let (to_fg, to_bg) = if boxed[i] { (gb.cost(&col[i]), gf.cost(&col[i])) } else { (0.0, 1e6) };
            g.edge(src, i, to_fg, 0.0);
            g.edge(i, sink, to_bg, 0.0);
        }
        for ((a, b), s) in pairs.iter().zip(&smooth) {
            g.edge(*a, *b, *s, *s);
        }
        let side = g.min_cut(src, sink);
        let next: Vec<bool> = (0..w * h).map(|i| side[i] && boxed[i]).collect();
        let same = next == fg;
        fg = next;
        if same {
            break;
        }
    }

    // Back to full size, with a slightly softened edge.
    let small_mask = GrayImage::from_raw(w as u32, h as u32, fg.iter().map(|f| if *f { 255 } else { 0 }).collect()).expect("buffer size");
    let big = imageops::resize(&small_mask, crop.w, crop.h, FilterType::CatmullRom);
    for y in 0..crop.h as i32 {
        for x in 0..crop.w as i32 {
            let v = big.as_raw()[(y as u32 * crop.w + x as u32) as usize];
            // Steepen the resampled edge so it isn't a wide blur.
            let v = ((v as f32 - 127.5) * 3.0 + 127.5).clamp(0.0, 255.0) as u8;
            if rect.contains(area.x0 + x, area.y0 + y) {
                out.set(area.x0 + x, area.y0 + y, [v]);
            }
        }
    }
    out
}

// -------------------------------------------------------------- Watershed

/// An image divided into regions that follow its edges.
pub struct Regions {
    w: usize,
    h: usize,
    /// Document pixels per label-map pixel.
    scale: f32,
    labels: Vec<u32>,
    pub count: u32,
}

impl Regions {
    /// The region under a document point.
    pub fn label_at(&self, x: f32, y: f32) -> Option<u32> {
        let (lx, ly) = ((x / self.scale) as i64, (y / self.scale) as i64);
        (lx >= 0 && ly >= 0 && (lx as usize) < self.w && (ly as usize) < self.h).then(|| self.labels[ly as usize * self.w + lx as usize])
    }

    /// A `w` x `h` mask (document size) of the given regions.
    pub fn mask(&self, labels: &[u32], w: u32, h: u32) -> Mask {
        let mut out = Mask::new(w, h);
        let wanted: std::collections::HashSet<u32> = labels.iter().copied().collect();
        for y in 0..h as usize {
            let ly = ((y as f32 + 0.5) / self.scale) as usize;
            if ly >= self.h {
                continue;
            }
            for x in 0..w as usize {
                let lx = ((x as f32 + 0.5) / self.scale) as usize;
                if lx < self.w && wanted.contains(&self.labels[ly * self.w + lx]) {
                    out.data[y * w as usize + x] = 255;
                }
            }
        }
        out
    }
}

/// Split an image into regions along its edges (watershed by immersion).
/// `detail` is how fine the regions are: low values merge small features.
pub fn watershed(px: &Pixmap, detail: f32) -> Regions {
    let scale = (px.w.max(px.h) as f32 / 900.0).max(1.0);
    let (w, h) = (((px.w as f32 / scale) as u32).max(1), ((px.h as f32 / scale) as u32).max(1));
    let small = imageops::resize(&RgbaImage::from_raw(px.w, px.h, px.data.clone()).expect("buffer size"), w, h, FilterType::Triangle);
    let (w, h) = (w as usize, h as usize);
    // Smooth each colour channel, then measure how fast colour changes.
    let detail = detail.clamp(0.0, 1.0);
    let sigma = 1.0 + (1.0 - detail) * 5.0;
    let mut chan: [Vec<f32>; 3] = std::array::from_fn(|c| small.pixels().map(|p| p[c] as f32 * p[3] as f32 / 255.0 + (255 - p[3]) as f32).collect());
    for c in &mut chan {
        gaussian_blur::<1>(c, w, h, sigma);
    }
    let levels = 24.0 + detail * 104.0;
    let grad: Vec<u8> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (x0, x1, y0, y1) = (x.saturating_sub(1), (x + 1).min(w - 1), y.saturating_sub(1), (y + 1).min(h - 1));
            let g: f32 = chan.iter().map(|c| (c[y * w + x1] - c[y * w + x0]).abs() + (c[y1 * w + x] - c[y0 * w + x]).abs()).sum();
            // Compress strong edges; quantise so shallow basins merge.
            ((g / 3.0).sqrt() / 16.0 * levels).min(255.0) as u8
        })
        .collect();

    // Visit pixels from the flattest to the steepest: each level first
    // extends the regions it touches, then starts new ones in what's left.
    let mut by_level: Vec<Vec<u32>> = vec![Vec::new(); 256];
    for (i, g) in grad.iter().enumerate() {
        by_level[*g as usize].push(i as u32);
    }
    let mut labels = vec![0u32; w * h];
    let mut count = 0u32;
    let neighbours = |i: usize| {
        let (x, y) = (i % w, i / w);
        [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)].into_iter().flatten()
    };
    let mut queue = VecDeque::new();
    for (level, pixels) in by_level.iter().enumerate() {
        for &i in pixels {
            if let Some(l) = neighbours(i as usize).map(|j| labels[j]).find(|l| *l != 0) {
                labels[i as usize] = l;
                queue.push_back(i as usize);
            }
        }
        let grow = |queue: &mut VecDeque<usize>, labels: &mut Vec<u32>| {
            while let Some(i) = queue.pop_front() {
                for j in neighbours(i) {
                    if labels[j] == 0 && grad[j] as usize == level {
                        labels[j] = labels[i];
                        queue.push_back(j);
                    }
                }
            }
        };
        grow(&mut queue, &mut labels);
        for &i in pixels {
            if labels[i as usize] == 0 {
                count += 1;
                labels[i as usize] = count;
                queue.push_back(i as usize);
                grow(&mut queue, &mut labels);
            }
        }
    }
    Regions { w, h, scale, labels, count }
}

// ---------------------------------------------------------------- Contours

/// The outlines of a selection as closed loops of points (pixel corners),
/// longest first. Each loop runs clockwise on screen around what it encloses.
pub fn contours(m: &Mask) -> Vec<Vec<(f32, f32)>> {
    let segs = selection::outline(m);
    let on = |x: i32, y: i32| m.get(x, y).is_some_and(|v| v[0] >= 128);
    // Direct every edge so the selected side is on its right (clockwise).
    let mut from: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
    for [x0, y0, x1, y1] in segs {
        // Collinear runs were merged; split back into unit edges to keep junctions exact.
        let (dx, dy) = ((x1 - x0).signum(), (y1 - y0).signum());
        let (mut x, mut y) = (x0, y0);
        while (x, y) != (x1, y1) {
            let (nx, ny) = (x + dx, y + dy);
            let (lo, hi) = if (x, y) < (nx, ny) { ((x, y), (nx, ny)) } else { ((nx, ny), (x, y)) };
            let increasing = if dy == 0 { on(lo.0, lo.1) } else { on(lo.0 - 1, lo.1) };
            let (a, b) = if increasing { (lo, hi) } else { (hi, lo) };
            from.entry(a).or_default().push(b);
            (x, y) = (nx, ny);
        }
    }
    let mut loops = Vec::new();
    while let Some((&start, _)) = from.iter().find(|(_, v)| !v.is_empty()) {
        let mut pts = vec![start];
        let mut cur = start;
        loop {
            let Some(next) = from.get_mut(&cur).and_then(|v| v.pop()) else { break };
            if next == start {
                break;
            }
            pts.push(next);
            cur = next;
        }
        if pts.len() >= 4 {
            loops.push(pts.into_iter().map(|p| (p.0 as f32, p.1 as f32)).collect::<Vec<_>>());
        }
    }
    loops.sort_by_key(|l: &Vec<(f32, f32)>| std::cmp::Reverse(l.len()));
    loops
}
