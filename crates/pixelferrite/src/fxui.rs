//! Interface for the geometry operations and the selection helpers that
//! need more than a slider: perspective handles, content-aware scale, and
//! turning a selection's outline into a text path.

use egui::{Context, Key, RichText, vec2};
use pf_core::segment;
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
}

impl Warp {
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
                self.warp = Some(Warp { quad: Warp::start_quad(&op, mode), op, mode, grab: None });
            }
            None => self.toast("This layer is hidden, locked or too small"),
        }
    }

    /// Re-render the preview after a corner moved (distort only; straighten
    /// shows just the outline until applied).
    pub fn warp_changed(&mut self) {
        if let Some(w) = &mut self.warp {
            if w.mode == PerspectiveMode::Distort {
                w.op.update(&mut self.doc, w.quad, w.mode);
            }
        }
    }

    pub fn perspective_dialog(&mut self, ctx: &Context) {
        let Some(w) = &mut self.warp else { return };
        let (mut apply, mut cancel) = (false, false);
        let was = w.mode;
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
            ui.add_space(8.0);
            ui.allocate_ui_with_layout(vec2(270.0, 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                apply = ui.button("Apply").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        apply |= ctx.input(|i| i.key_pressed(Key::Enter));
        cancel |= ctx.input(|i| i.key_pressed(Key::Escape));
        if w.mode != was {
            // Switching mode starts over from the untouched layer.
            w.quad = Warp::start_quad(&w.op, w.mode);
            let (quad, _) = (w.op.corners(), ());
            w.op.update(&mut self.doc, quad, PerspectiveMode::Distort);
        }
        if cancel {
            self.warp.take().unwrap().op.cancel(&mut self.doc);
        } else if apply {
            let mut w = self.warp.take().unwrap();
            if w.op.update(&mut self.doc, w.quad, w.mode) {
                w.op.finish(&mut self.doc);
            } else {
                w.op.cancel(&mut self.doc);
                self.toast("Those corners don't make a usable shape");
            }
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
            if self.doc.content_aware_scale(d.w, d.h) {
                self.fit_pending = true;
            }
        } else if close {
            self.scale_dlg = None;
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
