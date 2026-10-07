//! Interface for the geometry operations and the selection helpers that
//! need more than a slider: perspective handles, content-aware scale, and
//! turning a selection's outline into a text path.

use egui::{Context, Key, RichText, vec2};
use pf_core::{DocState, Document, segment};
use pf_core::text::{self, TextSpec};
use pf_core::transform::{PerspectiveMode, PerspectiveOp, Quad};

use crate::app::App;
use crate::tools::Tool;

/// A perspective change in progress: four corners being dragged on the canvas.
pub struct Warp {
    pub op: PerspectiveOp,
    pub quad: Quad,
    pub mode: PerspectiveMode,
    /// The corner being dragged.
    pub grab: Option<usize>,
    generation: u64,
    pending: bool,
    valid: bool,
    ready: Option<(u64, DocState)>,
}

impl Warp {
    fn can_apply(&self) -> bool {
        !self.pending && self.grab.is_none() && self.ready.as_ref().is_some_and(|(generation, _)| *generation == self.generation)
    }

    fn large(&self) -> bool {
        let frame = self.op.frame;
        let input = frame.width() as u64 * frame.height() as u64;
        let output = if self.mode == PerspectiveMode::Distort {
            let (x0, x1) = self.quad.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| (a.0.min(p.0 as f64), a.1.max(p.0 as f64)));
            let (y0, y1) = self.quad.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| (a.0.min(p.1 as f64), a.1.max(p.1 as f64)));
            ((x1.ceil() - x0.floor()) * (y1.ceil() - y0.floor())).max(0.0) as u64
        } else { input };
        input > 100_000 || output > 100_000
    }

    fn start_quad(op: &PerspectiveOp, mode: PerspectiveMode) -> Quad {
        let c = op.corners();
        match mode {
            PerspectiveMode::Distort => c,
            // Start inside the frame so all four handles are easy to grab.
            PerspectiveMode::Straighten => {
                let (cx, cy) = ((c[0].0 + c[2].0) / 2.0, (c[0].1 + c[2].1) / 2.0);
                c.map(|p| (cx + (p.0 - cx) * 0.7, cy + (p.1 - cy) * 0.7))
            }
        }
    }
}

pub struct ScaleDlg {
    w: u32,
    h: u32,
    max: (u32, u32),
}

impl App {
    pub fn open_perspective(&mut self) {
        match PerspectiveOp::begin(&self.doc) {
            Some(op) => {
                let mode = PerspectiveMode::Distort;
                let generation = pf_core::document::next_id();
                self.warp = Some(Warp {
                    quad: Warp::start_quad(&op, mode), op, mode, grab: None,
                    generation, pending: false, valid: true, ready: Some((generation, self.doc.begin())),
                });
            }
            None => self.toast("This layer is hidden, locked or too small"),
        }
    }

    /// Keep only the latest requested corners while a preview is rendering.
    pub fn warp_changed(&mut self) {
        if let Some(w) = &mut self.warp {
            w.generation = pf_core::document::next_id();
            w.pending = true;
            w.valid = true;
            w.ready = None;
        }
        self.schedule_perspective();
    }

    /// Start one preview at a time; corners moved while it runs replace the
    /// pending request rather than queuing obsolete full-image computations.
    pub fn schedule_perspective(&mut self) {
        if self.work.is_some() { return; }
        let Some(w) = &mut self.warp else { return };
        if !w.pending { return; }
        w.pending = false;
        let generation = w.generation;
        let (quad, mode, large) = (w.quad, w.mode, w.large());
        let mut op = w.op.clone();
        let state = self.doc.begin();
        let render = move || {
            let mut doc = Document::from_state(state);
            let state = op.update(&mut doc, quad, mode).then_some(doc.state);
            Ok(crate::jobs::Outcome::Perspective { generation, state })
        };
        if large {
            self.start_work("Rendering perspective preview…", render);
        } else if let Ok(crate::jobs::Outcome::Perspective { generation, state }) = render() {
            self.accept_perspective_preview(generation, state);
        }
    }

    pub fn accept_perspective_preview(&mut self, generation: u64, state: Option<DocState>) {
        let Some(w) = &mut self.warp else { return };
        if w.generation != generation { return; }
        w.valid = state.is_some();
        if let Some(state) = state {
            // Straightening keeps the original image under the handles until
            // Apply, so the user can continue identifying the source corners.
            if w.mode == PerspectiveMode::Distort {
                self.doc.state = state.clone();
                self.doc.mark_all_dirty();
            }
            w.ready = Some((generation, state));
        }
    }

    pub fn cancel_perspective(&mut self) {
        self.cancel_work();
        if let Some(w) = self.warp.take() {
            w.op.cancel(&mut self.doc);
        }
    }

    fn apply_perspective(&mut self) {
        if !self.warp.as_ref().is_some_and(Warp::can_apply) { return; }
        if let Some(w) = self.warp.take() {
            self.doc.state = w.ready.unwrap().1;
            self.doc.mark_all_dirty();
            w.op.finish(&mut self.doc);
        }
    }

    pub fn perspective_dialog(&mut self, ctx: &Context) {
        let Some(w) = &mut self.warp else { return };
        let (mut apply, mut cancel) = (false, false);
        let was = w.mode;
        let can_apply = w.can_apply();
        egui::Window::new("Perspective").collapsible(false).resizable(false).default_pos(self.view.vp.left_bottom() + vec2(16.0, -170.0)).show(ctx, |ui| {
            ui.set_width(270.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut w.mode, PerspectiveMode::Distort, "Distort").on_hover_text("Drag the layer's corners");
                ui.selectable_value(&mut w.mode, PerspectiveMode::Straighten, "Straighten").on_hover_text("Mark something that should be a rectangle");
            });
            let hint = match w.mode {
                PerspectiveMode::Distort => "Drag the four corner handles to reshape the layer.",
                PerspectiveMode::Straighten => "Put the four handles on the corners of something that should be square-on (a page, a screen, a building front), then Apply.",
            };
            ui.label(RichText::new(hint).small().weak());
            if !w.valid {
                ui.label(RichText::new("Those corners don't make a usable shape or exceed the image limits.").small());
            } else if !can_apply {
                ui.label(RichText::new("Preparing the latest preview…").small().weak());
            }
            ui.add_space(8.0);
            ui.allocate_ui_with_layout(vec2(270.0, 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                apply = ui.add_enabled(can_apply, egui::Button::new("Apply")).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        apply |= can_apply && ctx.input(|i| i.key_pressed(Key::Enter));
        cancel |= ctx.input(|i| i.key_pressed(Key::Escape));
        if w.mode != was {
            // Switching mode starts over from the untouched layer.
            w.quad = Warp::start_quad(&w.op, w.mode);
            w.op.clone().cancel(&mut self.doc);
            self.warp_changed();
            apply = false;
        }
        if cancel {
            self.cancel_perspective();
        } else if apply {
            self.apply_perspective();
        }
    }

    pub fn open_content_scale(&mut self) {
        match self.doc.state.active_layer() {
            Some(l) if !l.locked => self.scale_dlg = Some(ScaleDlg { w: l.pixels.w, h: l.pixels.h, max: (l.pixels.w, l.pixels.h) }),
            _ => self.toast("This layer is locked"),
        }
    }

    pub fn content_scale_dialog(&mut self, ctx: &Context) {
        let Some(d) = &mut self.scale_dlg else { return };
        let (mut apply, mut close) = (false, false);
        egui::Modal::new(egui::Id::new("content-scale")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("Content-Aware Scale");
            ui.add_space(4.0);
            ui.label(RichText::new("Shrinks the layer by removing the least interesting strips, so the main subjects keep their shape. It can only make things smaller.").weak());
            ui.add_space(6.0);
            egui::Grid::new("scale-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Width");
                ui.add(egui::DragValue::new(&mut d.w).range(1..=d.max.0).suffix(format!(" of {} px", d.max.0)));
                ui.end_row();
                ui.label("Height");
                ui.add(egui::DragValue::new(&mut d.h).range(1..=d.max.1).suffix(format!(" of {} px", d.max.1)));
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Large changes on big images can take several seconds.").small().weak());
            ui.add_space(10.0);
            ui.allocate_ui_with_layout(vec2(320.0, 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                apply = ui.button("Apply").clicked() || ui.input(|i| i.key_pressed(Key::Enter));
                close = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
            });
        });
        if apply {
            let d = self.scale_dlg.take().unwrap();
            self.apply_content_scale(d.w, d.h);
        } else if close {
            self.scale_dlg = None;
        }
    }

    fn apply_content_scale(&mut self, width: u32, height: u32) {
        let before = self.doc.begin();
        let pixels = before.active_layer().map_or(0, |l| l.pixels.w as u64 * l.pixels.h as u64);
        if pixels <= 100_000 {
            if self.doc.content_aware_scale(width, height) { self.fit_pending = true; }
        } else {
            self.start_work("Content-aware scaling…", move || {
                let mut doc = pf_core::Document::from_state(before);
                if !doc.content_aware_scale(width, height) { return Err("The layer could not be scaled".to_owned()); }
                Ok(crate::jobs::Outcome::Edit { state: doc.state, label: "Content-Aware Scale" })
            });
        }
    }

    /// Run text around the outline of the selection: the active text layer
    /// takes it as its path, or a new text is started on it.
    pub fn selection_to_text_path(&mut self) {
        let Some(sel) = self.doc.state.selection.clone() else {
            return self.toast("Select a shape for the text to follow first");
        };
        let Some(outline) = segment::contours(&sel).into_iter().next() else { return };
        // Begin at the leftmost point so the text starts up the left side and runs over the top.
        let start = (0..outline.len()).min_by(|a, b| outline[*a].0.total_cmp(&outline[*b].0)).unwrap_or(0);
        let mut pts: Vec<(f32, f32)> = outline[start..].iter().chain(&outline[..=start]).copied().collect();
        pts = text::smooth_path(&pts, 1.5);
        let active = self.doc.state.active_layer().and_then(|l| Some((l.id, l.text.clone()?, l.text_origin()?)));
        match active {
            Some((id, spec, (ox, oy))) => {
                let rel = pts.iter().map(|p| (p.0 - ox as f32, p.1 - oy as f32)).collect();
                self.apply_text(id, TextSpec { path: rel, path_offset: 0.0, ..(*spec).clone() });
            }
            None => {
                let o = egui::pos2(pts[0].0.round(), pts[0].1.round());
                self.new_text(o, pts.iter().map(|p| (p.0 - o.x, p.1 - o.y)).collect());
            }
        }
        self.tool = Tool::Text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use egui_kittest::Harness;
    use pf_core::{Layer, Pixmap};

    fn harness() -> Harness<'static, App> {
        Harness::builder().with_size(egui::vec2(1400.0, 880.0)).build_eframe(|cc| {
            let mut app = App::new(cc, None);
            app.doc = Document::new(512, 512, Some([255, 0, 0, 255]));
            app
        })
    }

    fn finish(h: &mut Harness<'_, App>) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while h.state().work.is_some() || h.state().warp.as_ref().is_some_and(|w| w.pending) {
            assert!(Instant::now() < deadline, "geometry work did not finish");
            h.step();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn large_perspective_coalesces_previews_and_cancel_preserves_history() {
        let mut h = harness();
        h.state_mut().open_perspective();
        let original = h.state().doc.begin();
        let revision = h.state().doc.revision();
        let first = [(10.0, 0.0), (522.0, 0.0), (522.0, 512.0), (10.0, 512.0)];
        let latest = [(25.0, 30.0), (537.0, 30.0), (537.0, 542.0), (25.0, 542.0)];
        h.state_mut().warp.as_mut().unwrap().quad = first;
        h.state_mut().warp_changed();
        assert!(h.state().work.is_some());
        let first_generation = h.state().warp.as_ref().unwrap().generation;
        assert!(!h.state().warp.as_ref().unwrap().can_apply());
        h.state_mut().warp.as_mut().unwrap().quad = latest;
        h.state_mut().warp_changed();
        assert!(h.state().warp.as_ref().unwrap().pending);
        // Even a completed earlier job cannot enable Apply or replace pixels.
        h.state_mut().accept_perspective_preview(first_generation, Some(original.clone()));
        assert!(!h.state().warp.as_ref().unwrap().can_apply());
        h.state_mut().apply_perspective();
        assert!(h.state().warp.is_some());
        assert_eq!(h.state().doc.revision(), revision);
        finish(&mut h);
        assert!(h.state().warp.as_ref().unwrap().can_apply());
        let layer = h.state().doc.state.active_layer().unwrap();
        assert_eq!((layer.x, layer.y), (25, 30));
        h.state_mut().apply_perspective();
        assert_eq!(h.state().doc.history().0.last(), Some("Perspective"));
        assert!(h.state_mut().doc.undo());
        assert_eq!(h.state().doc.state.active_layer().unwrap().rect(), original.active_layer().unwrap().rect());

        // Cancelling another worker restores the snapshot and retains redo.
        h.state_mut().open_perspective();
        let revision = h.state().doc.revision();
        let history_position = h.state().doc.history().1;
        h.state_mut().warp.as_mut().unwrap().quad = first;
        h.state_mut().warp_changed();
        assert!(h.state().work.is_some());
        h.state_mut().cancel_perspective();
        finish(&mut h);
        assert!(h.state().warp.is_none());
        assert_eq!(h.state().doc.revision(), revision);
        assert_eq!(h.state().doc.history().1, history_position);
        assert_eq!(h.state().doc.state.active_layer().unwrap().pixels.data, original.active_layer().unwrap().pixels.data);
        assert!(h.state_mut().doc.redo());
    }

    #[test]
    fn large_layer_on_small_canvas_scales_in_the_background() {
        let mut h = harness();
        h.state_mut().doc = Document::new(16, 16, None);
        h.state_mut().doc.insert_layer("Image", Layer::new("Large", Pixmap::filled(512, 512, [255; 4]), 0, 0));
        h.state_mut().apply_content_scale(511, 512);
        assert!(h.state().work.is_some());
        assert_eq!(h.state().doc.state.active_layer().unwrap().pixels.w, 512);
        finish(&mut h);
        assert_eq!(h.state().doc.state.active_layer().unwrap().pixels.w, 511);
        assert_eq!((h.state().doc.state.width, h.state().doc.state.height), (16, 16));
        assert_eq!(h.state().doc.history().0.last(), Some("Content-Aware Scale"));
    }
}
