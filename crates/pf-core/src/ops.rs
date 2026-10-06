//! Undoable document-level operations (layer management, selection, clipboard).

use std::sync::Arc;

use crate::blend::BlendMode;
use crate::buf::{Mask, Pixmap};
use crate::composite;
use crate::document::{DocState, Document, Layer, LayerId, Target};
use crate::fill::{self, FillMode};
use crate::geom::IRect;
use crate::selection::{self, Combine};

/// Pixels lifted by copy/cut, with their document position.
#[derive(Clone)]
pub struct Clip {
    pub pixels: Pixmap,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reorder {
    Forward,
    Backward,
    Front,
    Back,
}

impl Document {
    fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.state.layers.iter().any(|l| l.name == n);
        if !taken(base) {
            return base.to_owned();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
    }

    /// Insert `layer` above the active layer and make it active.
    pub fn insert_layer(&mut self, name: &str, mut layer: Layer) -> LayerId {
        let before = self.begin();
        layer.name = self.unique_name(&layer.name);
        let id = layer.id;
        let at = self.state.index_of(self.state.active).map_or(self.state.layers.len(), |i| i + 1);
        self.state.layers.insert(at, layer);
        self.state.active = id;
        self.target = Target::Pixels;
        self.commit(name, before);
        id
    }

    pub fn add_empty_layer(&mut self) -> LayerId {
        let px = Pixmap::new(self.state.width, self.state.height);
        self.insert_layer("New Layer", Layer::new("Layer", px, 0, 0))
    }

    /// Add an image as a new layer, centred on the canvas.
    pub fn add_image_layer(&mut self, name: &str, px: Pixmap) -> LayerId {
        let x = (self.state.width as i32 - px.w as i32) / 2;
        let y = (self.state.height as i32 - px.h as i32) / 2;
        self.insert_layer("Add Image", Layer::new(name, px, x, y))
    }

    /// Add an image as a new layer, stretched to fill `rect` (document space).
    pub fn add_image_scaled(&mut self, name: &str, px: &Pixmap, rect: IRect) -> LayerId {
        let scaled = crate::aiedit::resize(px, rect.width().max(1) as u32, rect.height().max(1) as u32, image::imageops::FilterType::Lanczos3);
        self.insert_layer("Add Image", Layer::new(name, scaled, rect.x0, rect.y0))
    }

    /// Add an image as a new layer that fills the selection: scaled (keeping
    /// its proportions) to cover the selection's bounds, centred, and cut to
    /// the selection's shape. `None` if nothing is selected.
    pub fn add_image_in_selection(&mut self, name: &str, px: &Pixmap) -> Option<LayerId> {
        let sel = self.state.selection.clone()?;
        let b = selection::bounds(&sel)?.intersect(self.canvas());
        if b.is_empty() || px.w == 0 || px.h == 0 {
            return None;
        }
        let (bw, bh) = (b.width() as f32, b.height() as f32);
        let s = (bw / px.w as f32).max(bh / px.h as f32);
        let (w, h) = (((px.w as f32 * s).round() as u32).max(b.width() as u32), ((px.h as f32 * s).round() as u32).max(b.height() as u32));
        let scaled = crate::aiedit::resize(px, w, h, image::imageops::FilterType::Lanczos3);
        let (ox, oy) = ((w as i32 - b.width()) / 2, (h as i32 - b.height()) / 2);
        let mut out = scaled.reframed(IRect::xywh(ox, oy, b.width() as u32, b.height() as u32), [0; 4]);
        for y in 0..b.height() {
            for x in 0..b.width() {
                let i = out.idx(x, y) + 3;
                out.data[i] = ((out.data[i] as u32 * sel.px(b.x0 + x, b.y0 + y)[0] as u32 + 127) / 255) as u8;
            }
        }
        Some(self.insert_layer("Insert Image", Layer::new(name, out, b.x0, b.y0)))
    }

    pub fn delete_layer(&mut self, id: LayerId) {
        let Some(i) = self.state.index_of(id) else { return };
        if self.state.layers.len() <= 1 {
            return;
        }
        let before = self.begin();
        self.state.layers.remove(i);
        if self.state.active == id {
            self.state.active = self.state.layers[i.saturating_sub(1).min(self.state.layers.len() - 1)].id;
        }
        self.commit("Delete Layer", before);
    }

    pub fn duplicate_layer(&mut self, id: LayerId) {
        let Some(src) = self.state.layer(id) else { return };
        let mut copy = src.clone();
        copy.id = crate::document::next_id();
        copy.name = format!("{} copy", src.name);
        self.state.active = id;
        self.insert_layer("Duplicate Layer", copy);
    }

    pub fn reorder_layer(&mut self, id: LayerId, how: Reorder) {
        let Some(i) = self.state.index_of(id) else { return };
        let top = self.state.layers.len() - 1;
        let to = match how {
            Reorder::Forward => (i + 1).min(top),
            Reorder::Backward => i.saturating_sub(1),
            Reorder::Front => top,
            Reorder::Back => 0,
        };
        self.move_layer_to(id, to);
    }

    /// Move a layer to stack index `to` (0 = bottom).
    pub fn move_layer_to(&mut self, id: LayerId, to: usize) {
        let Some(i) = self.state.index_of(id) else { return };
        let to = to.min(self.state.layers.len() - 1);
        if to == i {
            return;
        }
        let before = self.begin();
        let l = self.state.layers.remove(i);
        self.state.layers.insert(to, l);
        self.commit("Reorder Layer", before);
    }

    /// Merge the layer into the one below it.
    pub fn merge_down(&mut self, id: LayerId) {
        let Some(i) = self.state.index_of(id) else { return };
        if i == 0 {
            return;
        }
        let before = self.begin();
        let upper = self.state.layers.remove(i);
        let lower = &mut self.state.layers[i - 1];
        let rect = lower.rect().union(upper.rect());
        let mut base = lower.clone();
        base.blend = BlendMode::Normal;
        base.visible = true;
        let mut top = upper;
        top.visible = true;
        let tmp = DocState { width: 0, height: 0, layers: vec![base, top], selection: None, active: 0 };
        let mut out = Pixmap::new(rect.width() as u32, rect.height() as u32);
        composite::composite_rect(&tmp, rect, &mut out.data);
        lower.pixels = Arc::new(out);
        lower.mask = None;
        lower.text = None;
        lower.opacity = 1.0;
        lower.x = rect.x0;
        lower.y = rect.y0;
        lower.touch();
        self.state.active = lower.id;
        self.target = Target::Pixels;
        self.commit("Merge Down", before);
    }

    /// Flatten everything into one canvas-sized layer.
    pub fn flatten_image(&mut self) {
        let before = self.begin();
        let layer = Layer::new("Background", composite::flatten(&self.state), 0, 0);
        self.state.active = layer.id;
        self.state.layers = vec![layer];
        self.target = Target::Pixels;
        self.commit("Flatten Image", before);
    }

    pub fn flip_layer(&mut self, id: LayerId, horizontal: bool) {
        let before = self.begin();
        let Some(l) = self.state.layer_mut(id) else { return };
        l.text = None;
        let px = Arc::make_mut(&mut l.pixels);
        if horizontal { px.flip_h() } else { px.flip_v() }
        if let Some(m) = &mut l.mask {
            let m = Arc::make_mut(m);
            if horizontal { m.flip_h() } else { m.flip_v() }
        }
        l.touch();
        self.commit(if horizontal { "Flip Horizontal" } else { "Flip Vertical" }, before);
    }

    pub fn rename_layer(&mut self, id: LayerId, name: &str) {
        let before = self.begin();
        match self.state.layer_mut(id) {
            Some(l) if l.name != name && !name.trim().is_empty() => l.name = name.trim().to_owned(),
            _ => return,
        }
        self.commit_quiet("Rename Layer", before);
    }

    // ---- masks ----

    /// Add a mask to the layer: from the selection if there is one, else fully revealing.
    pub fn add_mask(&mut self, id: LayerId) {
        let before = self.begin();
        let sel = self.state.selection.clone();
        let Some(l) = self.state.layer_mut(id) else { return };
        if l.mask.is_some() {
            return;
        }
        let mut m = Mask::filled(l.pixels.w, l.pixels.h, [255]);
        if let Some(sel) = sel {
            for y in 0..m.h as i32 {
                for x in 0..m.w as i32 {
                    m.set(x, y, [sel.get(x + l.x, y + l.y).map_or(0, |v| v[0])]);
                }
            }
        }
        l.mask = Some(Arc::new(m));
        l.mask_enabled = true;
        l.touch();
        self.state.active = id;
        self.target = Target::Mask;
        self.commit("Add Mask", before);
    }

    pub fn delete_mask(&mut self, id: LayerId) {
        let before = self.begin();
        let Some(l) = self.state.layer_mut(id) else { return };
        if l.mask.take().is_none() {
            return;
        }
        l.touch();
        self.target = Target::Pixels;
        self.commit("Delete Mask", before);
    }

    /// Bake the mask into the layer's alpha and remove it.
    pub fn apply_mask(&mut self, id: LayerId) {
        let before = self.begin();
        let Some(l) = self.state.layer_mut(id) else { return };
        let Some(m) = l.mask.take() else { return };
        l.text = None;
        let px = Arc::make_mut(&mut l.pixels);
        for (p, &m) in px.data.chunks_exact_mut(4).zip(&m.data) {
            p[3] = ((p[3] as u32 * m as u32 + 127) / 255) as u8;
        }
        l.touch();
        self.target = Target::Pixels;
        self.commit("Apply Mask", before);
    }

    pub fn invert_mask(&mut self, id: LayerId) {
        let before = self.begin();
        let Some(l) = self.state.layer_mut(id) else { return };
        let Some(m) = &mut l.mask else { return };
        Arc::make_mut(m).data.iter_mut().for_each(|v| *v = 255 - *v);
        l.touch();
        self.commit("Invert Mask", before);
    }

    // ---- selection ----

    pub fn set_selection(&mut self, name: &str, sel: Option<Mask>) {
        if sel.is_none() && self.state.selection.is_none() {
            return;
        }
        let before = self.begin();
        self.state.selection = sel.map(Arc::new);
        self.commit_quiet(name, before);
    }

    pub fn select(&mut self, name: &str, shape: &Mask, mode: Combine) {
        let sel = selection::combine(self.state.selection.as_deref(), shape, mode);
        self.set_selection(name, sel);
    }

    pub fn select_all(&mut self) {
        let m = Mask::filled(self.state.width, self.state.height, [255]);
        self.set_selection("Select All", Some(m));
    }

    pub fn deselect(&mut self) {
        self.set_selection("Deselect", None);
    }

    pub fn invert_selection(&mut self) {
        let m = selection::invert(self.state.selection.as_deref(), self.state.width, self.state.height);
        self.set_selection("Invert Selection", m);
    }

    // ---- clipboard / pixels ----

    /// Copy the selected part of the active layer (or the whole layer).
    pub fn copy(&self) -> Option<Clip> {
        let l = self.state.active_layer()?;
        let area = match &self.state.selection {
            Some(s) => selection::bounds(s)?.intersect(l.rect()),
            None => l.rect(),
        };
        if area.is_empty() {
            return None;
        }
        let local = area.translate(-l.x, -l.y);
        let mut px = Pixmap::from_raw(area.width() as u32, area.height() as u32, l.pixels.crop_bytes(local));
        for y in 0..px.h as i32 {
            for x in 0..px.w as i32 {
                let mut k = l.mask_at(local.x0 + x, local.y0 + y) as u32;
                if let Some(s) = &self.state.selection {
                    k = k * s.px(area.x0 + x, area.y0 + y)[0] as u32 / 255;
                }
                let i = px.idx(x, y) + 3;
                px.data[i] = ((px.data[i] as u32 * k + 127) / 255) as u8;
            }
        }
        Some(Clip { pixels: px, x: area.x0, y: area.y0 })
    }

    /// Erase the selected area of the active layer (all of it with no selection).
    pub fn clear(&mut self) -> bool {
        let cov = fill::selection_or_all(self);
        fill::fill_mask(self, "Clear", &cov, FillMode::Erase, 1.0)
    }

    pub fn fill(&mut self, color: [u8; 4]) -> bool {
        let cov = fill::selection_or_all(self);
        fill::fill_mask(self, "Fill", &cov, FillMode::Color(color), 1.0)
    }

    pub fn cut(&mut self) -> Option<Clip> {
        let clip = self.copy()?;
        self.clear();
        Some(clip)
    }

    pub fn paste(&mut self, clip: &Clip) -> LayerId {
        self.insert_layer("Paste", Layer::new("Pasted Layer", clip.pixels.clone(), clip.x, clip.y))
    }

    // ---- canvas ----

    /// Crop the canvas to `r` (document space).
    pub fn crop(&mut self, r: IRect) {
        let r = r.intersect(self.canvas());
        if r.is_empty() || r == self.canvas() {
            return;
        }
        let before = self.begin();
        for l in &mut self.state.layers {
            l.x -= r.x0;
            l.y -= r.y0;
        }
        self.state.width = r.width() as u32;
        self.state.height = r.height() as u32;
        self.state.selection = None;
        self.commit("Crop", before);
    }

    /// Topmost visible layer with a non-transparent pixel at a document point.
    pub fn layer_at(&self, x: i32, y: i32) -> Option<LayerId> {
        let solid = |l: &Layer, x: i32, y: i32| {
            l.pixels.get(x - l.x, y - l.y).is_some_and(|p| p[3] > 8 && l.mask_at(x - l.x, y - l.y) > 8)
        };
        self.state.layers.iter().rev().filter(|l| l.visible).find_map(|l| {
            let hit = match &l.text {
                // Text is mostly gaps, so anywhere close to a letter counts.
                Some(t) => {
                    let r = (t.size * 0.5).clamp(4.0, 400.0) as i32;
                    let step = (r / 4).max(1) as usize;
                    (-r..=r).step_by(step).any(|dy| (-r..=r).step_by(step).any(|dx| solid(l, x + dx, y + dy)))
                }
                None => solid(l, x, y),
            };
            hit.then_some(l.id)
        })
    }
}
