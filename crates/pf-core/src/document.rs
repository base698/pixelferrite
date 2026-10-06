use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::blend::BlendMode;
use crate::buf::{Mask, Pixmap};
use crate::geom::IRect;
use crate::text::TextSpec;

pub type LayerId = u64;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Process-wide unique id, used for layer ids and content revisions.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Which raster of a layer an edit applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Pixels,
    Mask,
}

/// A raster layer. `pixels` (and `mask`, which always has the same size) are
/// positioned at `(x, y)` in document space and may be smaller or larger than
/// the canvas. Buffers are `Arc`ed so cloning a layer for undo is cheap.
#[derive(Clone)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub blend: BlendMode,
    pub x: i32,
    pub y: i32,
    pub pixels: Arc<Pixmap>,
    pub mask: Option<Arc<Mask>>,
    pub mask_enabled: bool,
    /// Set while the pixels are still an untouched rendering of this text.
    pub text: Option<Arc<TextSpec>>,
    /// Changes whenever the layer's rasters change; used to refresh thumbnails.
    pub rev: u64,
}

impl Layer {
    pub fn new(name: impl Into<String>, pixels: Pixmap, x: i32, y: i32) -> Self {
        Self {
            id: next_id(),
            name: name.into(),
            visible: true,
            locked: false,
            opacity: 1.0,
            blend: BlendMode::Normal,
            x,
            y,
            pixels: Arc::new(pixels),
            mask: None,
            mask_enabled: true,
            text: None,
            rev: next_id(),
        }
    }

    /// Bounds in document space.
    pub fn rect(&self) -> IRect {
        IRect::xywh(self.x, self.y, self.pixels.w, self.pixels.h)
    }

    pub fn touch(&mut self) {
        self.rev = next_id();
    }

    /// Grow the layer's buffers so they cover `r` (document space).
    pub fn ensure_covers(&mut self, r: IRect) {
        let cur = self.rect();
        if cur.contains_rect(r) {
            return;
        }
        let new = cur.union(r);
        let local = new.translate(-self.x, -self.y);
        self.pixels = Arc::new(self.pixels.reframed(local, [0; 4]));
        if let Some(m) = &self.mask {
            self.mask = Some(Arc::new(m.reframed(local, [255])));
        }
        self.x = new.x0;
        self.y = new.y0;
        self.touch();
    }

    /// Mask coverage (0..=255) at a buffer coordinate, honouring `mask_enabled`.
    #[inline]
    pub fn mask_at(&self, bx: i32, by: i32) -> u8 {
        match &self.mask {
            Some(m) if self.mask_enabled => m.px(bx, by)[0],
            _ => 255,
        }
    }
}

/// Everything undo/redo needs to restore. Cloning is cheap (Arc'ed rasters).
#[derive(Clone)]
pub struct DocState {
    pub width: u32,
    pub height: u32,
    /// Bottom to top.
    pub layers: Vec<Layer>,
    /// Document-sized selection mask; `None` means nothing is selected.
    pub selection: Option<Arc<Mask>>,
    pub active: LayerId,
}

impl DocState {
    pub fn canvas(&self) -> IRect {
        IRect::xywh(0, 0, self.width, self.height)
    }

    pub fn index_of(&self, id: LayerId) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    pub fn active_layer(&self) -> Option<&Layer> {
        self.layer(self.active)
    }

    pub fn active_layer_mut(&mut self) -> Option<&mut Layer> {
        let id = self.active;
        self.layer_mut(id)
    }
}

enum Change {
    /// A region of one raster changed; geometry and everything else is the same.
    Patch { layer: LayerId, target: Target, rect: IRect, before: Vec<u8>, after: Vec<u8> },
    /// Anything else: keep both full states (cheap unless rasters diverge).
    State { before: Box<DocState>, after: Box<DocState> },
}

struct Step {
    name: String,
    change: Change,
    /// Later commits with the same key fold into this step.
    merge: Option<u64>,
}

impl Step {
    fn bytes(&self) -> usize {
        match &self.change {
            Change::Patch { before, after, .. } => before.len() + after.len(),
            Change::State { .. } => 0,
        }
    }
}

const MAX_STEPS: usize = 200;
const MAX_PATCH_BYTES: usize = 1 << 30;

pub struct Document {
    pub state: DocState,
    steps: Vec<Step>,
    /// Number of steps currently applied; `steps[pos..]` are redoable.
    pos: usize,
    dirty: IRect,
    /// Whether tools edit the active layer's pixels or its mask.
    pub target: Target,
    pub path: Option<PathBuf>,
    pub modified: bool,
}

impl Document {
    pub fn from_state(state: DocState) -> Self {
        let dirty = state.canvas();
        Self { state, steps: Vec::new(), pos: 0, dirty, target: Target::Pixels, path: None, modified: false }
    }

    /// New document with one layer, optionally filled with `background`.
    pub fn new(width: u32, height: u32, background: Option<[u8; 4]>) -> Self {
        let (name, px) = match background {
            Some(c) => ("Background", Pixmap::filled(width, height, c)),
            None => ("Layer", Pixmap::new(width, height)),
        };
        let layer = Layer::new(name, px, 0, 0);
        let active = layer.id;
        Self::from_state(DocState { width, height, layers: vec![layer], selection: None, active })
    }

    pub fn from_pixmap(px: Pixmap, name: &str) -> Self {
        let (width, height) = (px.w, px.h);
        let layer = Layer::new(name, px, 0, 0);
        let active = layer.id;
        Self::from_state(DocState { width, height, layers: vec![layer], selection: None, active })
    }

    pub fn canvas(&self) -> IRect {
        self.state.canvas()
    }

    /// The effective edit target: `Mask` only if the active layer has one.
    pub fn effective_target(&self) -> Target {
        match self.state.active_layer() {
            Some(l) if l.mask.is_some() => self.target,
            _ => Target::Pixels,
        }
    }

    // ---- dirty tracking (document space) ----

    pub fn mark_dirty(&mut self, r: IRect) {
        self.dirty = self.dirty.union(r.intersect(self.canvas()));
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty = self.canvas();
    }

    pub fn take_dirty(&mut self) -> IRect {
        std::mem::take(&mut self.dirty)
    }

    // ---- history ----

    /// Snapshot to pass to [`Self::commit`] / [`Self::commit_patch`] after mutating `state`.
    pub fn begin(&self) -> DocState {
        self.state.clone()
    }

    fn push(&mut self, name: &str, change: Change) {
        self.steps.truncate(self.pos);
        self.steps.push(Step { name: name.to_owned(), change, merge: None });
        let mut bytes: usize = self.steps.iter().map(Step::bytes).sum();
        let mut drop = self.steps.len().saturating_sub(MAX_STEPS);
        while bytes > MAX_PATCH_BYTES && drop < self.steps.len() - 1 {
            bytes -= self.steps[drop].bytes();
            drop += 1;
        }
        self.steps.drain(..drop);
        self.pos = self.steps.len();
        self.modified = true;
    }

    /// Record an arbitrary change as one undo step.
    pub fn commit(&mut self, name: &str, before: DocState) {
        self.push(name, Change::State { before: Box::new(before), after: Box::new(self.state.clone()) });
        self.mark_all_dirty();
    }

    /// Like [`Self::commit`] for changes that don't affect the composite
    /// (selection, layer names), so nothing is marked dirty.
    pub fn commit_quiet(&mut self, name: &str, before: DocState) {
        self.push(name, Change::State { before: Box::new(before), after: Box::new(self.state.clone()) });
    }

    /// Like [`Self::commit`], but consecutive commits with the same `key` and
    /// name become one undo step (typing, dragging a slider).
    pub fn commit_merged(&mut self, name: &str, before: DocState, key: u64) {
        self.mark_all_dirty();
        if self.pos == self.steps.len() {
            if let Some(Step { name: n, change: Change::State { after, .. }, merge: Some(k) }) = self.steps.last_mut() {
                if *k == key && n == name {
                    *after = Box::new(self.state.clone());
                    self.modified = true;
                    return;
                }
            }
        }
        self.push(name, Change::State { before: Box::new(before), after: Box::new(self.state.clone()) });
        self.steps.last_mut().unwrap().merge = Some(key);
    }

    /// Record a change confined to `rect` (buffer coordinates) of one raster of
    /// one layer. Falls back to a full-state step if the layer was reframed.
    pub fn commit_patch(&mut self, name: &str, before: DocState, layer: LayerId, target: Target, rect: IRect) {
        if rect.is_empty() {
            self.state = before;
            return;
        }
        let same_geom = match (before.layer(layer), self.state.layer(layer)) {
            (Some(a), Some(b)) => {
                a.rect() == b.rect() && a.text.is_some() == b.text.is_some() && (target == Target::Pixels || a.mask.is_some())
            }
            _ => false,
        };
        if !same_geom {
            return self.commit(name, before);
        }
        let (old, new) = (before.layer(layer).unwrap(), self.state.layer(layer).unwrap());
        let (b, a) = match target {
            Target::Pixels => (old.pixels.crop_bytes(rect), new.pixels.crop_bytes(rect)),
            Target::Mask => {
                (old.mask.as_ref().unwrap().crop_bytes(rect), new.mask.as_ref().unwrap().crop_bytes(rect))
            }
        };
        self.push(name, Change::Patch { layer, target, rect, before: b, after: a });
    }

    fn apply(&mut self, i: usize, undo: bool) {
        match &self.steps[i].change {
            Change::State { before, after } => {
                self.state = if undo { (**before).clone() } else { (**after).clone() };
                self.dirty = self.state.canvas();
            }
            Change::Patch { layer, target, rect, before, after } => {
                let bytes = if undo { before } else { after };
                if let Some(l) = self.state.layers.iter_mut().find(|l| l.id == *layer) {
                    match target {
                        Target::Pixels => Arc::make_mut(&mut l.pixels).write_bytes(*rect, bytes),
                        Target::Mask => {
                            if let Some(m) = &mut l.mask {
                                Arc::make_mut(m).write_bytes(*rect, bytes)
                            }
                        }
                    }
                    l.touch();
                    let r = rect.translate(l.x, l.y);
                    self.mark_dirty(r);
                }
            }
        }
        self.modified = true;
    }

    pub fn can_undo(&self) -> bool {
        self.pos > 0
    }

    pub fn can_redo(&self) -> bool {
        self.pos < self.steps.len()
    }

    pub fn undo(&mut self) -> bool {
        if !self.can_undo() {
            return false;
        }
        self.pos -= 1;
        self.apply(self.pos, true);
        true
    }

    pub fn redo(&mut self) -> bool {
        if !self.can_redo() {
            return false;
        }
        self.apply(self.pos, false);
        self.pos += 1;
        true
    }

    /// Step names, oldest first, and how many of them are currently applied.
    pub fn history(&self) -> (impl Iterator<Item = &str>, usize) {
        (self.steps.iter().map(|s| s.name.as_str()), self.pos)
    }

    /// Undo/redo until exactly `pos` steps are applied.
    pub fn jump_to(&mut self, pos: usize) {
        let pos = pos.min(self.steps.len());
        while self.pos > pos {
            self.undo();
        }
        while self.pos < pos {
            self.redo();
        }
    }
}
