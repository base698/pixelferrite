//! Stroke-based tools: brush, pencil, eraser, clone stamp and smudge.

use std::sync::Arc;

use crate::buf::{Mask, Pixmap, luma, over};
use crate::document::{DocState, Document, LayerId, Target};
use crate::geom::IRect;

#[derive(Clone, Copy, Debug)]
pub struct BrushParams {
    /// Diameter in document pixels.
    pub size: f32,
    /// 0 = fully soft falloff, 1 = hard edge.
    pub hardness: f32,
    /// Maximum coverage of the whole stroke.
    pub opacity: f32,
    /// Coverage added per dab.
    pub flow: f32,
    /// Dab spacing as a fraction of `size`.
    pub spacing: f32,
}

#[derive(Clone, Copy, Debug)]
pub enum PaintKind {
    Brush([u8; 4]),
    /// Hard, aliased dabs.
    Pencil([u8; 4]),
    Eraser,
    /// Paint with pixels from `(x + dx, y + dy)` of the layer as it was at stroke start.
    Clone { dx: i32, dy: i32 },
    Smudge { strength: f32 },
}

impl PaintKind {
    fn name(&self) -> &'static str {
        match self {
            PaintKind::Brush(_) => "Brush",
            PaintKind::Pencil(_) => "Pencil",
            PaintKind::Eraser => "Erase",
            PaintKind::Clone { .. } => "Clone Stamp",
            PaintKind::Smudge { .. } => "Smudge",
        }
    }
}

/// An in-progress stroke. Dabs accumulate into a per-stroke coverage buffer and
/// the layer is re-derived from its stroke-start snapshot, so overlapping dabs
/// never exceed the stroke opacity.
pub struct Stroke {
    before: DocState,
    layer: LayerId,
    target: Target,
    kind: PaintKind,
    p: BrushParams,
    src_px: Arc<Pixmap>,
    src_mask: Option<Arc<Mask>>,
    cov: Vec<u16>,
    off: (i32, i32),
    /// Paintable area in buffer coordinates.
    clip: IRect,
    sel: Option<Arc<Mask>>,
    last: (f32, f32),
    until_next: f32,
    dirty: IRect,
    pending: IRect,
    carry: Vec<[f32; 4]>,
    have_carry: bool,
}

impl Stroke {
    /// Start a stroke on the active layer at `pos` (document space).
    /// Returns `None` if the layer can't be painted on.
    pub fn begin(doc: &mut Document, kind: PaintKind, p: BrushParams, pos: (f32, f32)) -> Option<Stroke> {
        let before = doc.begin();
        let canvas = doc.canvas();
        let mut target = doc.effective_target();
        if matches!(kind, PaintKind::Clone { .. } | PaintKind::Smudge { .. }) {
            target = Target::Pixels;
        }
        let sel = doc.state.selection.clone();
        let active = doc.state.active_layer()?;
        let frame = active.rect().union(canvas);
        doc.check_layer_resize(active.id, frame.width() as u32, frame.height() as u32).ok()?;
        let layer = doc.state.active_layer_mut()?;
        if !layer.visible || layer.locked {
            return None;
        }
        if !layer.ensure_covers(canvas) {
            return None;
        }
        if target == Target::Pixels {
            layer.text = None;
        }
        let off = (layer.x, layer.y);
        let n = layer.pixels.w as usize * layer.pixels.h as usize;
        let mut s = Stroke {
            before,
            layer: layer.id,
            target,
            kind,
            p,
            src_px: layer.pixels.clone(),
            src_mask: layer.mask.clone(),
            cov: if matches!(kind, PaintKind::Smudge { .. }) { Vec::new() } else { vec![0; n] },
            off,
            clip: canvas.translate(-off.0, -off.1),
            sel,
            last: pos,
            until_next: 0.0,
            dirty: IRect::EMPTY,
            pending: IRect::EMPTY,
            carry: Vec::new(),
            have_carry: false,
        };
        s.dab(doc, pos.0, pos.1);
        s.until_next = s.step();
        s.flush(doc);
        Some(s)
    }

    fn step(&self) -> f32 {
        match self.kind {
            PaintKind::Smudge { .. } => (self.p.size * 0.05).max(1.0),
            PaintKind::Pencil(_) => (self.p.size * 0.1).clamp(0.5, 4.0),
            _ => (self.p.size * self.p.spacing).max(0.5),
        }
    }

    /// Extend the stroke to `pos` (document space).
    pub fn line_to(&mut self, doc: &mut Document, pos: (f32, f32)) {
        let (lx, ly) = self.last;
        let (dx, dy) = (pos.0 - lx, pos.1 - ly);
        let len = dx.hypot(dy);
        if len <= 0.0 {
            return;
        }
        let step = self.step();
        let mut t = self.until_next;
        while t <= len {
            self.dab(doc, lx + dx * t / len, ly + dy * t / len);
            t += step;
        }
        self.until_next = t - len;
        self.last = pos;
        self.flush(doc);
    }

    fn flush(&mut self, doc: &mut Document) {
        if self.pending.is_empty() {
            return;
        }
        doc.mark_dirty(self.pending.translate(self.off.0, self.off.1));
        if let Some(l) = doc.state.layer_mut(self.layer) {
            l.touch();
        }
        self.dirty = self.dirty.union(self.pending);
        self.pending = IRect::EMPTY;
    }

    /// Finish the stroke and record it as one undo step.
    pub fn finish(self, doc: &mut Document) {
        doc.commit_patch(self.kind.name(), self.before, self.layer, self.target, self.dirty);
    }

    /// Discard an unfinished stroke without adding or removing history.
    pub fn cancel(self, doc: &mut Document) {
        doc.state = self.before;
        doc.mark_all_dirty();
    }

    #[inline]
    fn sel_at(&self, bx: i32, by: i32) -> f32 {
        match &self.sel {
            Some(s) => s.px(bx + self.off.0, by + self.off.1)[0] as f32 * (1.0 / 255.0),
            None => 1.0,
        }
    }

    fn dab(&mut self, doc: &mut Document, cx: f32, cy: f32) {
        if let PaintKind::Smudge { strength } = self.kind {
            return self.smudge_dab(doc, cx, cy, strength);
        }
        let r = (self.p.size * 0.5).max(0.5);
        let pencil = matches!(self.kind, PaintKind::Pencil(_));
        let (mut bcx, mut bcy) = (cx - self.off.0 as f32, cy - self.off.1 as f32);
        if pencil {
            // Snap so dabs are pixel-symmetric: odd sizes centre on a pixel, even on a corner.
            let odd = (self.p.size.round() as i32).max(1) % 2 == 1;
            let snap = |v: f32| if odd { v.floor() + 0.5 } else { v.round() };
            (bcx, bcy) = (snap(bcx), snap(bcy));
        }
        let rect = IRect::enclosing(bcx - r - 1.0, bcy - r - 1.0, bcx + r + 1.0, bcy + r + 1.0).intersect(self.clip);
        if rect.is_empty() {
            return;
        }
        let Some(layer) = doc.state.layer_mut(self.layer) else { return };
        let bw = self.src_px.w as usize;
        let hard = self.p.hardness.clamp(0.0, 1.0);
        let inner = r * hard;
        let (flow, opacity) = (self.p.flow, self.p.opacity);
        let mut px = match self.target {
            Target::Pixels => Some(Arc::make_mut(&mut layer.pixels)),
            Target::Mask => None,
        };
        let mut mk = match (self.target, &mut layer.mask) {
            (Target::Mask, Some(m)) => Some(Arc::make_mut(m)),
            _ => None,
        };
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                let (dx, dy) = (x as f32 + 0.5 - bcx, y as f32 + 0.5 - bcy);
                let d = (dx * dx + dy * dy).sqrt();
                let m = if pencil {
                    if d <= r { 1.0 } else { 0.0 }
                } else {
                    let edge = (r - d + 0.5).clamp(0.0, 1.0);
                    if d <= inner || hard >= 1.0 {
                        edge
                    } else {
                        let t = ((d - inner) / (r - inner)).min(1.0);
                        edge * (1.0 - t * t * (3.0 - 2.0 * t))
                    }
                };
                if m <= 0.0 {
                    continue;
                }
                let ci = y as usize * bw + x as usize;
                let c0 = self.cov[ci] as f32 * (1.0 / 65535.0);
                let c1 = if pencil { c0.max(m) } else { c0 + (1.0 - c0) * m * flow };
                let q = (c1 * 65535.0 + 0.5) as u16;
                if q <= self.cov[ci] {
                    continue;
                }
                self.cov[ci] = q;
                let a = c1 * opacity * self.sel_at(x, y);
                if let Some(px) = px.as_deref_mut() {
                    let s = self.src_px.px(x, y);
                    let out = match self.kind {
                        PaintKind::Brush(c) | PaintKind::Pencil(c) => over(s, c, a),
                        PaintKind::Eraser => [s[0], s[1], s[2], (s[3] as f32 * (1.0 - a) + 0.5) as u8],
                        PaintKind::Clone { dx, dy } => match self.src_px.get(x + dx, y + dy) {
                            Some(c) => {
                                // Replace rather than layer over, so cloned transparency carries too.
                                let l = |s: u8, c: u8| (s as f32 + (c as f32 - s as f32) * a + 0.5) as u8;
                                if s[3] == 0 {
                                    [c[0], c[1], c[2], l(0, c[3])]
                                } else if c[3] == 0 {
                                    [s[0], s[1], s[2], l(s[3], 0)]
                                } else {
                                    [l(s[0], c[0]), l(s[1], c[1]), l(s[2], c[2]), l(s[3], c[3])]
                                }
                            }
                            None => s,
                        },
                        PaintKind::Smudge { .. } => s,
                    };
                    px.set(x, y, out);
                } else if let (Some(mk), Some(src)) = (mk.as_deref_mut(), &self.src_mask) {
                    let s = src.px(x, y)[0] as f32;
                    let v = match self.kind {
                        PaintKind::Brush(c) | PaintKind::Pencil(c) => luma(c) as f32,
                        _ => 0.0,
                    };
                    mk.set(x, y, [(s + (v - s) * a + 0.5) as u8]);
                }
            }
        }
        self.pending = self.pending.union(rect);
    }

    fn smudge_dab(&mut self, doc: &mut Document, cx: f32, cy: f32, strength: f32) {
        let r = (self.p.size * 0.5).max(1.0);
        let ri = r.ceil() as i32;
        let side = (ri * 2 + 1) as usize;
        let (bcx, bcy) = ((cx - self.off.0 as f32).floor() as i32, (cy - self.off.1 as f32).floor() as i32);
        let Some(layer) = doc.state.layer_mut(self.layer) else { return };
        let px = Arc::make_mut(&mut layer.pixels);
        let premul = |p: [u8; 4]| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
        };
        if !self.have_carry {
            self.carry = vec![[0.0; 4]; side * side];
            for j in 0..side as i32 {
                for i in 0..side as i32 {
                    if let Some(p) = px.get(bcx - ri + i, bcy - ri + j) {
                        self.carry[j as usize * side + i as usize] = premul(p);
                    }
                }
            }
            self.have_carry = true;
            return;
        }
        let hard = self.p.hardness.clamp(0.0, 0.99);
        let inner = r * hard;
        let mut touched = IRect::EMPTY;
        for j in 0..side as i32 {
            for i in 0..side as i32 {
                let (x, y) = (bcx - ri + i, bcy - ri + j);
                let slot = j as usize * side + i as usize;
                if !self.clip.contains(x, y) {
                    continue;
                }
                let cur = premul(px.px(x, y));
                let d = (((i - ri) * (i - ri) + (j - ri) * (j - ri)) as f32).sqrt();
                let m = if d <= inner {
                    1.0
                } else {
                    let t = ((d - inner) / (r - inner)).min(1.0);
                    1.0 - t * t * (3.0 - 2.0 * t)
                };
                let s = strength * m * self.sel_at(x, y);
                if s <= 0.0 {
                    self.carry[slot] = cur;
                    continue;
                }
                let c = self.carry[slot];
                let n = [
                    cur[0] + (c[0] - cur[0]) * s,
                    cur[1] + (c[1] - cur[1]) * s,
                    cur[2] + (c[2] - cur[2]) * s,
                    cur[3] + (c[3] - cur[3]) * s,
                ];
                self.carry[slot] = n;
                let out = if n[3] <= 0.001 {
                    [0; 4]
                } else {
                    let q = |v: f32| (v / n[3] + 0.5).clamp(0.0, 255.0) as u8;
                    [q(n[0]), q(n[1]), q(n[2]), (n[3] * 255.0 + 0.5).min(255.0) as u8]
                };
                px.set(x, y, out);
                touched = touched.union(IRect::new(x, y, x + 1, y + 1));
            }
        }
        self.pending = self.pending.union(touched);
    }
}
