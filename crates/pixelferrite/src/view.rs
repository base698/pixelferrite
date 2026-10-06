//! Canvas view transform (pan / zoom / rotate) and the GPU-side copy of the
//! composited image.

use egui::emath::Rot2;
use egui::{Color32, ColorImage, Context, Mesh, Painter, Pos2, Rect, TextureHandle, TextureOptions, Vec2, pos2, vec2};
use pf_core::{DocState, IRect, composite};

pub const MIN_ZOOM: f32 = 0.02;
pub const MAX_ZOOM: f32 = 64.0;

/// Maps document pixels to screen points.
pub struct View {
    /// Physical screen pixels per document pixel (1.0 = 100%).
    pub zoom: f32,
    /// Canvas rotation in radians, clockwise.
    pub rot: f32,
    /// Document point shown at the centre of the viewport.
    pub center: Vec2,
    pub ppp: f32,
    pub vp: Rect,
}

impl View {
    pub fn new() -> Self {
        Self { zoom: 1.0, rot: 0.0, center: Vec2::ZERO, ppp: 1.0, vp: Rect::ZERO }
    }

    /// Screen points per document pixel.
    pub fn scale(&self) -> f32 {
        self.zoom / self.ppp
    }

    pub fn to_screen(&self, d: Pos2) -> Pos2 {
        self.vp.center() + Rot2::from_angle(self.rot) * ((d.to_vec2() - self.center) * self.scale())
    }

    pub fn to_doc(&self, s: Pos2) -> Pos2 {
        (self.center + Rot2::from_angle(-self.rot) * (s - self.vp.center()) / self.scale()).to_pos2()
    }

    /// Re-centre so document point `d` appears at screen point `s`.
    fn pin(&mut self, d: Pos2, s: Pos2) {
        self.center = d.to_vec2() - Rot2::from_angle(-self.rot) * (s - self.vp.center()) / self.scale();
    }

    pub fn zoom_about(&mut self, s: Pos2, factor: f32) {
        let d = self.to_doc(s);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.pin(d, s);
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        let s = self.vp.center();
        self.zoom_about(s, zoom / self.zoom);
    }

    pub fn rotate_about(&mut self, s: Pos2, delta: f32) {
        let d = self.to_doc(s);
        self.rot = (self.rot + delta + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        self.pin(d, s);
    }

    pub fn pan(&mut self, screen_delta: Vec2) {
        self.center -= Rot2::from_angle(-self.rot) * screen_delta / self.scale();
    }

    /// Centre the canvas and zoom so it fits with a margin (never above 100%).
    pub fn fit(&mut self, w: u32, h: u32) {
        let avail = (self.vp.size() - Vec2::splat(48.0)).max(Vec2::splat(16.0)) * self.ppp;
        self.zoom = (avail.x / w as f32).min(avail.y / h as f32).clamp(MIN_ZOOM, 1.0);
        self.rot = 0.0;
        self.center = vec2(w as f32, h as f32) * 0.5;
    }

    /// Document-space bounding box of what the viewport shows.
    pub fn visible_doc_rect(&self) -> Rect {
        let r = self.vp;
        Rect::from_points(&[
            self.to_doc(r.left_top()),
            self.to_doc(r.right_top()),
            self.to_doc(r.left_bottom()),
            self.to_doc(r.right_bottom()),
        ])
    }
}

const TILE: usize = 1024;
const LEVELS: usize = 3;
const OPTS: TextureOptions = TextureOptions {
    magnification: egui::TextureFilter::Nearest,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: None,
};

struct Level {
    w: usize,
    h: usize,
    cols: usize,
    tiles: Vec<TextureHandle>,
}

/// The composited document as tiled textures at full, half and quarter
/// resolution (so zoomed-out views don't shimmer). Only dirty regions are
/// recomposited and re-uploaded.
pub struct Display {
    w: u32,
    h: u32,
    levels: Vec<Level>,
}

impl Display {
    pub fn new() -> Self {
        Self { w: 0, h: 0, levels: Vec::new() }
    }

    fn rebuild(&mut self, ctx: &Context, w: u32, h: u32) {
        self.w = w;
        self.h = h;
        self.levels.clear();
        let (mut lw, mut lh) = (w as usize, h as usize);
        for li in 0..LEVELS {
            let (cols, rows) = (lw.div_ceil(TILE), lh.div_ceil(TILE));
            let mut tiles = Vec::with_capacity(cols * rows);
            for ty in 0..rows {
                for tx in 0..cols {
                    let size = [TILE.min(lw - tx * TILE), TILE.min(lh - ty * TILE)];
                    let img = ColorImage::filled(size, Color32::TRANSPARENT);
                    tiles.push(ctx.load_texture(format!("canvas-{li}-{tx}-{ty}"), img, OPTS));
                }
            }
            self.levels.push(Level { w: lw, h: lh, cols, tiles });
            lw = lw.div_ceil(2);
            lh = lh.div_ceil(2);
        }
    }

    /// Bring the textures up to date with `dirty` (document space).
    pub fn sync(&mut self, ctx: &Context, state: &DocState, mut dirty: IRect) {
        if (self.w, self.h) != (state.width, state.height) || self.levels.is_empty() {
            self.rebuild(ctx, state.width, state.height);
            dirty = state.canvas();
        }
        if dirty.is_empty() {
            return;
        }
        // Align to the coarsest level's pixel grid.
        let g = 1 << (LEVELS - 1);
        let r = IRect::new(dirty.x0 & !(g - 1), dirty.y0 & !(g - 1), (dirty.x1 + g - 1) & !(g - 1), (dirty.y1 + g - 1) & !(g - 1))
            .intersect(state.canvas());
        let (mut w, mut h) = (r.width() as usize, r.height() as usize);
        let mut rgba = vec![0u8; w * h * 4];
        composite::composite_rect(state, r, &mut rgba);

        // Bake the transparency checkerboard in.
        let mut px: Vec<Color32> = Vec::with_capacity(w * h);
        for (i, p) in rgba.chunks_exact(4).enumerate() {
            if p[3] == 255 {
                px.push(Color32::from_rgb(p[0], p[1], p[2]));
            } else {
                let (x, y) = (r.x0 as usize + i % w, r.y0 as usize + i / w);
                let bg = if (x / 8 + y / 8) % 2 == 0 { 255u32 } else { 204 };
                let a = p[3] as u32;
                let f = |c: u8| ((c as u32 * a + bg * (255 - a) + 127) / 255) as u8;
                px.push(Color32::from_rgb(f(p[0]), f(p[1]), f(p[2])));
            }
        }

        let (mut x0, mut y0) = (r.x0 as usize, r.y0 as usize);
        for li in 0..LEVELS {
            self.upload(li, x0, y0, w, h, &px);
            if li + 1 == LEVELS {
                break;
            }
            // Box-filter down 2x for the next level.
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            let mut next = Vec::with_capacity(nw * nh);
            for y in 0..nh {
                let (ya, yb) = (y * 2, (y * 2 + 1).min(h - 1));
                for x in 0..nw {
                    let (xa, xb) = (x * 2, (x * 2 + 1).min(w - 1));
                    let s = [px[ya * w + xa], px[ya * w + xb], px[yb * w + xa], px[yb * w + xb]];
                    let avg = |f: fn(&Color32) -> u8| (s.iter().map(|c| f(c) as u32).sum::<u32>() / 4) as u8;
                    next.push(Color32::from_rgb(avg(Color32::r), avg(Color32::g), avg(Color32::b)));
                }
            }
            px = next;
            (w, h, x0, y0) = (nw, nh, x0 / 2, y0 / 2);
        }
    }

    fn upload(&mut self, li: usize, x0: usize, y0: usize, w: usize, h: usize, px: &[Color32]) {
        let level = &mut self.levels[li];
        let (x1, y1) = ((x0 + w).min(level.w), (y0 + h).min(level.h));
        for ty in y0 / TILE..=(y1.max(1) - 1) / TILE {
            for tx in x0 / TILE..=(x1.max(1) - 1) / TILE {
                let (sx0, sy0) = (x0.max(tx * TILE), y0.max(ty * TILE));
                let (sx1, sy1) = (x1.min((tx + 1) * TILE), y1.min((ty + 1) * TILE));
                if sx0 >= sx1 || sy0 >= sy1 {
                    continue;
                }
                let mut sub = Vec::with_capacity((sx1 - sx0) * (sy1 - sy0));
                for y in sy0..sy1 {
                    let i = (y - y0) * w + (sx0 - x0);
                    sub.extend_from_slice(&px[i..i + (sx1 - sx0)]);
                }
                let img = ColorImage::new([sx1 - sx0, sy1 - sy0], sub);
                level.tiles[ty * level.cols + tx].set_partial([sx0 - tx * TILE, sy0 - ty * TILE], img, OPTS);
            }
        }
    }

    pub fn paint(&self, painter: &Painter, view: &View) {
        if self.levels.is_empty() {
            return;
        }
        let li = if view.zoom <= 0.25 { 2 } else if view.zoom <= 0.5 { 1 } else { 0 };
        let level = &self.levels[li];
        let s = (1 << li) as f32;
        let (dw, dh) = (self.w as f32, self.h as f32);
        for (i, tex) in level.tiles.iter().enumerate() {
            let (tx, ty) = (i % level.cols, i / level.cols);
            let [tw, th] = tex.size();
            let d0 = pos2((tx * TILE) as f32 * s, (ty * TILE) as f32 * s);
            let d1 = pos2(((tx * TILE + tw) as f32 * s).min(dw), ((ty * TILE + th) as f32 * s).min(dh));
            let corners = [d0, pos2(d1.x, d0.y), d1, pos2(d0.x, d1.y)].map(|p| view.to_screen(p));
            if !Rect::from_points(&corners).intersects(view.vp) {
                continue;
            }
            let mut mesh = Mesh::with_texture(tex.id());
            let uvs = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
            for (p, uv) in corners.into_iter().zip(uvs) {
                mesh.vertices.push(egui::epaint::Vertex { pos: p, uv, color: Color32::WHITE });
            }
            mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
            painter.add(egui::Shape::mesh(mesh));
        }
    }
}
