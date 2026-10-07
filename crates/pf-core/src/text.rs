//! Text layers: layout (straight or along a path) and rasterisation.
//!
//! A text layer is an ordinary raster layer that also carries the
//! [`TextSpec`] it was rendered from, so it stays editable until something
//! paints on it.

use std::sync::Arc;

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use skrifa::{GlyphId, MetadataProvider, instance::{LocationRef, Size}, outline::{DrawSettings, OutlinePen}, raw::{ReadError, TableProvider, tables::kern::SubtableKind}};

use crate::buf::{Mask, Pixmap};
use crate::document::{Document, Layer, LayerId};
use crate::geom::IRect;

/// A borrowed OpenType face parsed by Fontations. The constructors also
/// accept a face index for installed font collections.
pub struct FontRef<'a> {
    inner: skrifa::FontRef<'a>,
    units_per_em: u16,
}

impl<'a> FontRef<'a> {
    pub fn try_from_slice(data: &'a [u8]) -> Result<Self, ReadError> {
        Self::try_from_slice_and_index(data, 0)
    }

    pub fn try_from_slice_and_index(data: &'a [u8], index: u32) -> Result<Self, ReadError> {
        let inner = skrifa::FontRef::from_index(data, index)?;
        let units_per_em = inner.head()?.units_per_em();
        if units_per_em == 0 {
            return Err(ReadError::MalformedData("font has zero units per em"));
        }
        inner.maxp()?;
        inner.cmap()?;
        Ok(Self { inner, units_per_em })
    }

    pub fn units_per_em(&self) -> Option<f32> {
        Some(self.units_per_em as f32)
    }

    pub fn glyph_id(&self, ch: char) -> GlyphId {
        self.inner.charmap().map(ch).unwrap_or(GlyphId::new(0))
    }

    pub fn h_advance_unscaled(&self, id: GlyphId) -> f32 {
        self.inner.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(id).unwrap_or(0.0)
    }

    /// Preserve the editor's horizontal, non-variable `kern` table behavior.
    pub fn kern_unscaled(&self, first: GlyphId, second: GlyphId) -> f32 {
        self.inner.kern().ok().and_then(|kern| {
            kern.subtables().filter_map(Result::ok)
                .filter(|s| s.is_horizontal() && !s.is_cross_stream() && !s.is_variable())
                .find_map(|s| match s.kind().ok()? {
                    SubtableKind::Format0(s) => s.kerning(first, second),
                    SubtableKind::Format2(s) => s.kerning(first, second),
                    SubtableKind::Format3(s) => s.kerning(first, second),
                    _ => None,
                })
        }).unwrap_or(0) as f32
    }

    fn glyph_bounds(&self, id: GlyphId) -> Option<(Point, Point)> {
        let bounds = self.inner.glyph_metrics(Size::unscaled(), LocationRef::default()).bounds(id)?;
        if bounds.x_min >= bounds.x_max || bounds.y_min >= bounds.y_max {
            return None;
        }
        Some((point(bounds.x_min, bounds.y_min), point(bounds.x_max, bounds.y_max)))
    }

    fn outline(&self, id: GlyphId) -> Option<Outline> {
        let glyphs = self.inner.outline_glyphs();
        let glyph = glyphs.get(id)?;
        let mut pen = CurvePen::default();
        glyph.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).ok()?;
        (!pen.curves.is_empty() && !pen.too_complex).then_some(Outline {
            curves: pen.curves,
        })
    }
}

enum OutlineCurve {
    Line(Point, Point),
    Quad(Point, Point, Point),
    Cubic(Point, Point, Point, Point),
}

struct Outline {
    curves: Vec<OutlineCurve>,
}

#[derive(Default)]
struct CurvePen {
    first: Option<Point>,
    current: Option<Point>,
    curves: Vec<OutlineCurve>,
    too_complex: bool,
}

impl CurvePen {
    fn push(&mut self, curve: OutlineCurve) {
        if self.curves.len() < 100_000 {
            self.curves.push(curve);
        } else {
            self.too_complex = true;
        }
    }
}

impl OutlinePen for CurvePen {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = point(x, y);
        self.first = Some(p);
        self.current = Some(p);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = point(x, y);
        if let Some(a) = self.current { self.push(OutlineCurve::Line(a, p)); }
        self.current = Some(p);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let p = point(x, y);
        if let Some(a) = self.current { self.push(OutlineCurve::Quad(a, point(cx, cy), p)); }
        self.current = Some(p);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let p = point(x, y);
        if let Some(a) = self.current { self.push(OutlineCurve::Cubic(a, point(cx0, cy0), point(cx1, cy1), p)); }
        self.current = Some(p);
    }
    fn close(&mut self) {
        if let (Some(a), Some(b)) = (self.current, self.first) {
            if a != b { self.push(OutlineCurve::Line(a, b)); }
        }
        self.current = self.first;
    }
}

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
    id: GlyphId,
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

fn transform_point(g: &Placed, scale: f32, p: Point) -> (f32, f32) {
    let (sin, cos) = g.angle.sin_cos();
    // Font units are y-up; the page is y-down.
    let (x, y) = (p.x * scale, -p.y * scale);
    (g.pos.0 + x * cos - y * sin, g.pos.1 + x * sin + y * cos)
}

fn bounded_layout(spec: &TextSpec, font: &FontRef<'_>) -> Option<(Vec<(Placed, IRect)>, IRect)> {
    crate::io::limits::validate_text(spec).ok()?;
    let scale = spec.size / font.units_per_em().unwrap_or(1000.0);
    let mut glyphs = Vec::new();
    let mut bounds = IRect::EMPTY;
    for g in layout(spec, font) {
        let Some((min, max)) = font.glyph_bounds(g.id) else { continue };
        let corners = [min, point(max.x, min.y), max, point(min.x, max.y)].map(|p| transform_point(&g, scale, p));
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in corners {
            if !x.is_finite() || !y.is_finite() { return None; }
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let r = IRect::enclosing(x0 - 1.0, y0 - 1.0, x1 + 1.0, y1 + 1.0);
        bounds = bounds.union(r);
        let (w, h) = (bounds.x1 as i64 - bounds.x0 as i64, bounds.y1 as i64 - bounds.y0 as i64);
        if w <= 0 || h <= 0 || w > u32::MAX as i64 || h > u32::MAX as i64 { return None; }
        crate::io::limits::validate_dimensions(w as u32, h as u32).ok()?;
        crate::io::limits::validate_offset(bounds.x0, bounds.y0).ok()?;
        glyphs.push((g, r));
    }
    if bounds.is_empty() { bounds = IRect::xywh(0, 0, 1, 1); }
    Some((glyphs, bounds))
}

/// Validate and measure the raster frame without allocating pixel buffers.
/// Empty text occupies one transparent pixel; unsupported sizes return None.
pub fn render_bounds(spec: &TextSpec, font: &FontRef<'_>) -> Option<IRect> {
    bounded_layout(spec, font).map(|(_, bounds)| bounds)
}

/// Render `spec` to pixels. Returns them with their position relative to the
/// text origin; empty or unsupported text gives a 1x1 transparent pixmap.
pub fn render(spec: &TextSpec, font: &FontRef<'_>) -> (Pixmap, (i32, i32)) {
    let Some((glyphs, bounds)) = bounded_layout(spec, font) else {
        return (Pixmap::new(1, 1), (0, 0));
    };
    if glyphs.is_empty() {
        return (Pixmap::new(1, 1), (0, 0));
    }
    let scale = spec.size / font.units_per_em().unwrap_or(1000.0);
    let mut cov = Mask::new(bounds.width() as u32, bounds.height() as u32);
    // Keep only one glyph's outlines alive at once, after validating the
    // complete text frame and before allocating that glyph's rasterizer.
    for (g, r) in &glyphs {
        let Some(outline) = font.outline(g.id) else { continue };
        let mut ras = Rasterizer::new(r.width() as usize, r.height() as usize);
        let local = |p: Point| {
            let (x, y) = transform_point(g, scale, p);
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
    /// Validate layout, document capacity and placement before allocating a
    /// text raster. Use this for text entered through the editor.
    pub fn try_add_text_layer(&mut self, mut spec: TextSpec, origin: (i32, i32), font: &FontRef<'_>) -> crate::io::Result<LayerId> {
        let bounds = render_bounds(&spec, font).ok_or_else(|| crate::io::Error::Format("text exceeds the supported layout limits".into()))?;
        self.check_layer_capacity(bounds.width() as u32, bounds.height() as u32)?;
        let x = origin.0.checked_add(bounds.x0).ok_or_else(|| crate::io::Error::Format("text position overflow".into()))?;
        let y = origin.1.checked_add(bounds.y0).ok_or_else(|| crate::io::Error::Format("text position overflow".into()))?;
        crate::io::limits::validate_offset(x, y)?;
        let (px, off) = render(&spec, font);
        spec.raster = off;
        let mut layer = Layer::new("Text", px, x, y);
        layer.text = Some(Arc::new(spec));
        self.try_insert_layer("Add Text", layer)
    }

    /// Add a text layer with its origin at `origin` (document space).
    /// The caller must have validated the text layout and document capacity;
    /// user-entered text should use [`Self::try_add_text_layer`].
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
        let Some(bounds) = render_bounds(&spec, font) else { return false };
        let Some(layer) = self.state.layer(id) else { return false };
        if layer.locked {
            return false;
        }
        let Some(origin) = layer.text_origin() else { return false };
        let old = layer.rect();
        let (Some(x), Some(y)) = (origin.0.checked_add(bounds.x0), origin.1.checked_add(bounds.y0)) else { return false };
        if crate::io::limits::validate_offset(x, y).is_err()
            || self.check_layer_resize(id, bounds.width() as u32, bounds.height() as u32).is_err() {
            return false;
        }
        let (px, off) = render(&spec, font);
        spec.raster = off;
        let new = IRect::xywh(x, y, px.w, px.h);
        let layer = self.state.layer_mut(id).unwrap();
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
