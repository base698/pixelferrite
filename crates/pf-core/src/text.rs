//! Text layers: layout (straight or along a path) and rasterisation.
//!
//! A text layer is an ordinary raster layer that also carries the
//! [`TextSpec`] it was rendered from, so it stays editable until something
//! paints on it.

use std::sync::Arc;

pub use ab_glyph::FontRef;
use ab_glyph::{Font, OutlineCurve, point};
use ab_glyph_rasterizer::Rasterizer;

use crate::buf::{Mask, Pixmap};
use crate::document::{Document, Layer, LayerId};
use crate::geom::IRect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    pub fn name(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "center" => Align::Center,
            "right" => Align::Right,
            _ => Align::Left,
        }
    }
}

/// Everything needed to re-render a text layer. Coordinates are relative to
/// the text's origin: the start of the first baseline for straight text.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpec {
    pub text: String,
    /// Font family name; empty means the built-in font.
    pub font: String,
    pub bold: bool,
    pub italic: bool,
    /// Em size in pixels.
    pub size: f32,
    pub color: [u8; 4],
    pub align: Align,
    /// Extra space between letters, in pixels.
    pub tracking: f32,
    /// Line spacing as a multiple of `size`.
    pub leading: f32,
    /// Polyline the baseline follows; empty for straight text.
    pub path: Vec<(f32, f32)>,
    /// Distance along the path where the text starts.
    pub path_offset: f32,
    /// Where the rendered pixels sit relative to the origin (set by rendering).
    pub raster: (i32, i32),
}

impl Default for TextSpec {
    fn default() -> Self {
        Self {
            text: String::new(),
            font: String::new(),
            bold: false,
            italic: false,
            size: 72.0,
            color: [0, 0, 0, 255],
            align: Align::Left,
            tracking: 0.0,
            leading: 1.2,
            path: Vec::new(),
            path_offset: 0.0,
            raster: (0, 0),
        }
    }
}

/// A glyph placed on the page: scale, then rotate by `angle`, then move to `pos`.
struct Placed {
    id: ab_glyph::GlyphId,
    pos: (f32, f32),
    angle: f32,
}

/// A polyline with cumulative lengths, for walking along it by distance.
struct Path<'a> {
    pts: &'a [(f32, f32)],
    acc: Vec<f32>,
}

impl<'a> Path<'a> {
    fn new(pts: &'a [(f32, f32)]) -> Self {
        let mut acc = vec![0.0];
        for w in pts.windows(2) {
            acc.push(acc.last().unwrap() + (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1));
        }
        Self { pts, acc }
    }

    fn len(&self) -> f32 {
        *self.acc.last().unwrap()
    }

    /// Point at distance `s`; beyond either end the path continues straight.
    fn at(&self, s: f32) -> (f32, f32) {
        let n = self.pts.len();
        let i = self.acc.partition_point(|a| *a <= s).clamp(1, n - 1);
        let (a, b) = (self.pts[i - 1], self.pts[i]);
        let seg = (self.acc[i] - self.acc[i - 1]).max(1e-6);
        let t = (s - self.acc[i - 1]) / seg;
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }
}

fn layout(spec: &TextSpec, font: &FontRef<'_>) -> Vec<Placed> {
    let scale = spec.size / font.units_per_em().unwrap_or(1000.0);
    // Each line as (glyph, x of its left edge, advance) plus the line's width.
    let line = |text: &str| {
        let mut out = Vec::new();
        let (mut x, mut prev) = (0.0f32, None);
        for ch in text.chars().filter(|c| !c.is_control()) {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                x += font.kern_unscaled(p, id) * scale;
            }
            let adv = font.h_advance_unscaled(id) * scale;
            out.push((id, x, adv));
            x += adv + spec.tracking;
            prev = Some(id);
        }
        let width = if out.is_empty() { 0.0 } else { x - spec.tracking };
        (out, width)
    };
    let mut placed = Vec::new();
    let pts: Vec<(f32, f32)> = spec.path.iter().copied().filter(|p| p.0.is_finite() && p.1.is_finite()).collect();
    let path = Path::new(&pts);
    if pts.len() >= 2 && path.len() > 0.5 {
        let (glyphs, width) = line(&spec.text.replace('\n', " "));
        let start = spec.path_offset
            + match spec.align {
                Align::Left => 0.0,
                Align::Center => (path.len() - width) / 2.0,
                Align::Right => path.len() - width,
            };
        for (id, x, adv) in glyphs {
            // Sit each glyph on the chord under it, so it turns smoothly.
            let (a, b) = (path.at(start + x), path.at(start + x + adv.max(1.0)));
            placed.push(Placed { id, pos: a, angle: (b.1 - a.1).atan2(b.0 - a.0) });
        }
    } else {
        for (i, text) in spec.text.split('\n').enumerate() {
            let (glyphs, width) = line(text);
            let x0 = match spec.align {
                Align::Left => 0.0,
                Align::Center => -width / 2.0,
                Align::Right => -width,
            };
            let y = i as f32 * spec.size * spec.leading;
            placed.extend(glyphs.into_iter().map(|(id, x, _)| Placed { id, pos: (x0 + x, y), angle: 0.0 }));
        }
    }
    placed
}

/// Render `spec` to pixels. Returns them with their position relative to the
/// text origin; empty text gives a 1x1 transparent pixmap at the origin.
pub fn render(spec: &TextSpec, font: &FontRef<'_>) -> (Pixmap, (i32, i32)) {
    let scale = spec.size / font.units_per_em().unwrap_or(1000.0);
    let glyphs: Vec<_> = layout(spec, font)
        .into_iter()
        .filter_map(|g| {
            let o = font.outline(g.id)?;
            let (sin, cos) = g.angle.sin_cos();
            // Font units are y-up; the page is y-down.
            let tf = move |p: ab_glyph::Point| {
                let (x, y) = (p.x * scale, -p.y * scale);
                (g.pos.0 + x * cos - y * sin, g.pos.1 + x * sin + y * cos)
            };
            let b = o.bounds;
            let corners = [point(b.min.x, b.min.y), point(b.max.x, b.min.y), point(b.max.x, b.max.y), point(b.min.x, b.max.y)].map(tf);
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for (x, y) in corners {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
            (x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite())
                .then(|| (o, tf, IRect::enclosing(x0 - 1.0, y0 - 1.0, x1 + 1.0, y1 + 1.0)))
        })
        .collect();
    let bounds = glyphs.iter().fold(IRect::EMPTY, |r, g| r.union(g.2));
    // Refuse absurd sizes rather than exhausting memory.
    if bounds.is_empty() || bounds.width() as i64 * bounds.height() as i64 > 1 << 28 {
        return (Pixmap::new(1, 1), (0, 0));
    }
    let mut cov = Mask::new(bounds.width() as u32, bounds.height() as u32);
    for (outline, tf, r) in &glyphs {
        let mut ras = Rasterizer::new(r.width() as usize, r.height() as usize);
        let local = |p: ab_glyph::Point| {
            let (x, y) = tf(p);
            point(x - r.x0 as f32, y - r.y0 as f32)
        };
        for c in &outline.curves {
            match *c {
                OutlineCurve::Line(a, b) => ras.draw_line(local(a), local(b)),
                OutlineCurve::Quad(a, b, c) => ras.draw_quad(local(a), local(b), local(c)),
                OutlineCurve::Cubic(a, b, c, d) => ras.draw_cubic(local(a), local(b), local(c), local(d)),
            }
        }
        let (ox, oy) = (r.x0 - bounds.x0, r.y0 - bounds.y0);
        ras.for_each_pixel_2d(|x, y, v| {
            let i = cov.idx(x as i32 + ox, y as i32 + oy);
            // Glyphs that overlap on a tight curve shouldn't double up.
            cov.data[i] = cov.data[i].max((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        });
    }
    let mut px = Pixmap::new(cov.w, cov.h);
    let c = spec.color;
    for (p, v) in px.data.chunks_exact_mut(4).zip(&cov.data) {
        p.copy_from_slice(&[c[0], c[1], c[2], ((*v as u32 * c[3] as u32 + 127) / 255) as u8]);
    }
    (px, (bounds.x0, bounds.y0))
}

/// Turn a hand-drawn stroke into a smooth path: drop the jitter, then round
/// the corners. `tolerance` is how far (in pixels) the result may stray.
pub fn smooth_path(pts: &[(f32, f32)], tolerance: f32) -> Vec<(f32, f32)> {
    let mut keep = vec![false; pts.len()];
    if pts.len() < 3 {
        return pts.to_vec();
    }
    // Ramer-Douglas-Peucker.
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (pa, pb) = (pts[a], pts[b]);
        let (dx, dy) = (pb.0 - pa.0, pb.1 - pa.1);
        let len = dx.hypot(dy);
        // For a closed loop the ends coincide: measure from that point instead of a line.
        let off = |p: (f32, f32)| if len > 1e-3 { ((p.0 - pa.0) * dy - (p.1 - pa.1) * dx).abs() / len } else { (p.0 - pa.0).hypot(p.1 - pa.1) };
        let far = (a + 1..b)
            .map(|i| (i, off(pts[i])))
            .max_by(|x, y| x.1.total_cmp(&y.1));
        if let Some((i, d)) = far {
            if d > tolerance {
                keep[i] = true;
                stack.push((a, i));
                stack.push((i, b));
            }
        }
    }
    let mut out: Vec<(f32, f32)> = pts.iter().zip(&keep).filter(|(_, k)| **k).map(|(p, _)| *p).collect();
    // Chaikin corner cutting, keeping the end points.
    for _ in 0..4 {
        if out.len() < 3 {
            break;
        }
        let mut next = vec![out[0]];
        for w in out.windows(2) {
            let mix = |t: f32| (w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t);
            next.push(mix(0.25));
            next.push(mix(0.75));
        }
        next.push(*out.last().unwrap());
        out = next;
    }
    out
}

impl Layer {
    /// Document position of this text layer's origin.
    pub fn text_origin(&self) -> Option<(i32, i32)> {
        self.text.as_ref().map(|t| (self.x - t.raster.0, self.y - t.raster.1))
    }
}

impl Document {
    /// Add a text layer with its origin at `origin` (document space).
    pub fn add_text_layer(&mut self, mut spec: TextSpec, origin: (i32, i32), font: &FontRef<'_>) -> LayerId {
        let (px, off) = render(&spec, font);
        spec.raster = off;
        let mut layer = Layer::new("Text", px, origin.0 + off.0, origin.1 + off.1);
        layer.text = Some(Arc::new(spec));
        self.insert_layer("Add Text", layer)
    }

    /// Re-render a text layer from `spec`, keeping its origin. Not an undo
    /// step by itself: wrap it in `begin` / `commit`.
    pub fn update_text(&mut self, id: LayerId, mut spec: TextSpec, font: &FontRef<'_>) -> bool {
        let Some(layer) = self.state.layer_mut(id) else { return false };
        let Some(origin) = layer.text_origin() else { return false };
        let old = layer.rect();
        let (px, off) = render(&spec, font);
        spec.raster = off;
        let new = IRect::xywh(origin.0 + off.0, origin.1 + off.1, px.w, px.h);
        if let Some(m) = &layer.mask {
            layer.mask = Some(Arc::new(m.reframed(new.translate(-old.x0, -old.y0), [255])));
        }
        layer.pixels = Arc::new(px);
        (layer.x, layer.y) = (new.x0, new.y0);
        layer.text = Some(Arc::new(spec));
        layer.touch();
        self.mark_dirty(old.union(new));
        true
    }
}
