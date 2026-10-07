//! One background image operation per window. Results are accepted only for
//! the document revision that started them; cancellation never edits history.
use std::sync::mpsc::{Receiver, TryRecvError};
use egui::Context;
use pf_core::{DocState, Document, Mask, filter::{Filter, FilterOp}, selection::Combine, segment::Regions};

use crate::app::App;

pub enum Outcome {
    Filter { state: DocState, op: FilterOp },
    Perspective { generation: u64, state: Option<DocState> },
    Edit { state: DocState, label: &'static str },
    Selection { mask: Mask, mode: Combine, label: &'static str },
    Regions { regions: Regions, key: u64, mask: Mask, mode: Combine },
}

pub struct Work {
    pub label: &'static str,
    pub cancelled: bool,
    revision: u64,
    before: DocState,
    rx: Receiver<Result<Outcome, String>>,
}

impl App {
    pub fn background_active(&self) -> bool { self.work.as_ref().is_some_and(|w| !w.cancelled) }

    pub fn start_work(&mut self, label: &'static str, f: impl FnOnce() -> Result<Outcome, String> + Send + 'static) -> bool {
        if self.work.is_some() {
            self.toast("The previous image operation is still finishing. Please try again shortly.");
            return false;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        self.work = Some(Work { label, cancelled: false, revision: self.doc.revision(), before: self.doc.begin(), rx });
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .unwrap_or_else(|_| Err("The image operation could not be completed".to_owned()));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        true
    }

    pub fn cancel_work(&mut self) {
        if let Some(work) = &mut self.work { work.cancelled = true; }
    }

    pub fn poll_work(&mut self, ctx: &Context) {
        let Some(work) = &self.work else { return };
        let outcome = match work.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => Err("The image worker stopped unexpectedly".to_owned()),
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
        };
        let work = self.work.take().unwrap();
        if work.cancelled || work.revision != self.doc.revision() { return; }
        match outcome {
            Ok(Outcome::Filter { state, op }) => {
                if let Some(filter) = &mut self.filter {
                    self.doc.state = state;
                    self.doc.mark_all_dirty();
                    filter.op = op;
                }
            }
            Ok(Outcome::Perspective { generation, state }) => self.accept_perspective_preview(generation, state),
            Ok(Outcome::Edit { state, label }) => {
                self.doc.state = state;
                self.doc.commit(label, work.before);
                self.fit_pending = true;
            }
            Ok(Outcome::Selection { mask, mode, label }) => self.doc.select(label, &mask, mode),
            Ok(Outcome::Regions { regions, key, mask, mode }) => {
                self.regions = Some((key, regions));
                self.doc.select("Region Selection", &mask, mode);
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub fn work_status(&mut self, ctx: &Context) {
        let Some(work) = &self.work else { return };
        let label = work.label;
        let cancelled = work.cancelled;
        let mut cancel = false;
        egui::Area::new(egui::Id::new("image-work"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -76.0])
            .order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(if cancelled { "Cancelled; finishing background computation" } else { label });
                        if !cancelled { cancel = ui.button("Cancel Operation").clicked(); }
                    });
                });
            });
        if cancel || (self.background_active() && ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
            self.cancel_work();
            if let Some(filter) = self.filter.take() { filter.op.cancel(&mut self.doc); }
            if let Some(warp) = self.warp.take() { warp.op.cancel(&mut self.doc); }
            self.toast("Operation cancelled; no changes applied");
        }
    }

    pub fn schedule_filter(&mut self) {
        if self.work.is_some() { return; }
        let Some(dialog) = &mut self.filter else { return };
        if !dialog.pending { return; }
        dialog.pending = false;
        let state = dialog.before.clone();
        let filter: Filter = dialog.filter;
        let target = self.doc.target;
        // Small previews remain immediate, avoiding worker scheduling latency
        // for tiny assets; photo-sized work never blocks the interface.
        let pixels: u64 = state.layers.iter().map(|l| l.pixels.w as u64 * l.pixels.h as u64).sum();
        if pixels <= 1_000_000 && !filter.slow() {
            dialog.op.update(&mut self.doc, &filter);
        } else {
            self.start_work("Rendering filter preview…", move || {
                let mut doc = Document::from_state(state);
                doc.target = target;
                let mut op = FilterOp::begin(&doc).ok_or("The layer cannot be filtered")?;
                op.update(&mut doc, &filter);
                Ok(Outcome::Filter { state: doc.state, op })
            });
        }
    }
}
