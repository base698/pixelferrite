use std::path::PathBuf;
use std::collections::HashSet;
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
    /// Returns false without changing the layer if the larger frame exceeds
    /// the image limits.
    pub fn ensure_covers(&mut self, r: IRect) -> bool {
        if self.x.checked_add(self.pixels.w as i32).is_none() || self.y.checked_add(self.pixels.h as i32).is_none() {
            return false;
        }
        let cur = self.rect();
        if cur.contains_rect(r) {
            return true;
        }
        let new = cur.union(r);
        let (w, h) = (new.x1 as i64 - new.x0 as i64, new.y1 as i64 - new.y0 as i64);
        if w <= 0 || h <= 0 || w > u32::MAX as i64 || h > u32::MAX as i64
            || crate::io::limits::validate_dimensions(w as u32, h as u32).is_err()
            || new.x0.unsigned_abs() > crate::io::limits::MAX_LAYER_OFFSET as u32
            || new.y0.unsigned_abs() > crate::io::limits::MAX_LAYER_OFFSET as u32 {
            return false;
        }
        let local = new.translate(-self.x, -self.y);
        self.pixels = Arc::new(self.pixels.reframed(local, [0; 4]));
        if let Some(m) = &self.mask {
            self.mask = Some(Arc::new(m.reframed(local, [255])));
        }
        if let Some(text) = &mut self.text {
            let text = Arc::make_mut(text);
            text.raster.0 += new.x0 - self.x;
            text.raster.1 += new.y0 - self.y;
        }
        (self.x, self.y) = (new.x0, new.y0);
        self.touch();
        true
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

const MAX_STEPS: usize = 200;
const DEFAULT_HISTORY_BYTES: usize = 1 << 30;

/// Count each shared raster only once. Passing the live document first
/// excludes buffers that would remain allocated even with no undo history.
fn raster_bytes(state: &DocState, seen: &mut HashSet<usize>) -> usize {
    let mut bytes = 0;
    let mut count = |key, len| {
        if seen.insert(key) {
            bytes += len;
        }
    };
    for l in &state.layers {
        count(Arc::as_ptr(&l.pixels) as usize, l.pixels.data.capacity());
        if let Some(mask) = &l.mask {
            count(Arc::as_ptr(mask) as usize, mask.data.capacity());
        }
    }
    if let Some(selection) = &state.selection {
        count(Arc::as_ptr(selection) as usize, selection.data.capacity());
    }
    bytes
}

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
    history_byte_limit: usize,
    revision: u64,
}

impl Document {
    pub fn from_state(state: DocState) -> Self {
        let dirty = state.canvas();
        Self { state, steps: Vec::new(), pos: 0, dirty, target: Target::Pixels, path: None, modified: false, history_byte_limit: DEFAULT_HISTORY_BYTES, revision: next_id() }
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

    /// Changes after every committed edit, undo or redo, including changes
    /// that only affect metadata or the selection.
    pub fn revision(&self) -> u64 {
        self.revision
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
        self.pos = self.steps.len();
        self.trim_history();
        self.modified = true;
        self.revision = next_id();
    }

    /// Raster and patch bytes retained only for undo/redo. Shared allocations
    /// and buffers already needed by the current document are not double-counted.
    pub fn history_bytes(&self) -> usize {
        let mut seen = HashSet::new();
        raster_bytes(&self.state, &mut seen);
        self.steps.iter().map(|step| match &step.change {
            Change::Patch { before, after, .. } => before.capacity() + after.capacity(),
            Change::State { before, after } => raster_bytes(before, &mut seen) + raster_bytes(after, &mut seen),
        }).sum()
    }

    /// Set the retained-raster budget and immediately discard excess history.
    /// An edit larger than the budget remains applied, but is not undoable.
    pub fn set_history_byte_limit(&mut self, bytes: usize) {
        self.history_byte_limit = bytes;
        self.trim_history();
    }

    fn trim_history(&mut self) {
        while !self.steps.is_empty() && (self.steps.len() > MAX_STEPS || self.history_bytes() > self.history_byte_limit) {
            if self.pos > 0 {
                self.steps.remove(0);
                self.pos -= 1;
            } else {
                self.steps.pop();
            }
        }
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
                    self.revision = next_id();
                    self.trim_history();
                    return;
                }
            }
        }
        self.push(name, Change::State { before: Box::new(before), after: Box::new(self.state.clone()) });
        if let Some(step) = self.steps.last_mut() {
            step.merge = Some(key);
        }
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
        self.revision = next_id();
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
        self.trim_history();
        true
    }

    pub fn redo(&mut self) -> bool {
        if !self.can_redo() {
            return false;
        }
        self.apply(self.pos, false);
        self.pos += 1;
        self.trim_history();
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
            if !self.undo() { break; }
        }
        while self.pos < pos {
            if !self.redo() { break; }
        }
    }
}
