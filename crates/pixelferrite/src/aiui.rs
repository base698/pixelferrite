//! The AI side of the interface: the prompt dialog, the request in flight,
//! the history of past requests, and the settings dialog.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Color32, ColorImage, Context, Key, RichText, TextureHandle, TextureOptions, vec2};
use pf_core::aiedit::{self, AiJob, Source};
use pf_core::{IRect, Layer, Pixmap, io};

use crate::ai;
use crate::app::App;
use crate::store::{AiRecord, Settings as Saved, timestamp};

const WARN: Color32 = Color32::from_rgb(255, 170, 90);

fn document_session() -> u64 {
    static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let seed = SEED.get_or_init(|| {
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
        time ^ u64::from(std::process::id()).rotate_left(32)
    });
    seed.wrapping_add(pf_core::document::next_id()).max(1)
}

struct Prompt {
    text: String,
    cfg: ai::Config,
    focus: bool,
    source: Source,
    /// Exactly what would be sent, with the part that stays dimmed.
    preview: TextureHandle,
}

/// A request that is out with the model.
pub struct Run {
    rx: Receiver<Result<Completed, String>>,
    job: AiJob,
    prompt: String,
    started: f64,
    cancelled: Arc<AtomicBool>,
    pending: Option<Completed>,
}

struct Completed {
    pixels: Pixmap,
    history_error: Option<String>,
}

struct History {
    records: Vec<AiRecord>,
    selected: Option<String>,
    /// Loaded images of the selected request, by file name.
    images: HashMap<&'static str, Option<TextureHandle>>,
}

struct SettingsDlg {
    edit: Saved,
    show_key: bool,
}

pub struct AiUi {
    prompt: Option<Prompt>,
    run: Option<Run>,
    error: Option<String>,
    last_prompt: String,
    /// Remembered choice of what to send.
    source: Source,
    history: Option<History>,
    settings: Option<SettingsDlg>,
    document_session: u64,
}

impl Default for AiUi {
    fn default() -> Self {
        Self { prompt: None, run: None, error: None, last_prompt: String::new(), source: Source::Visible, history: None, settings: None, document_session: document_session() }
    }
}

impl AiUi {
    /// A dialog that should swallow keyboard shortcuts is showing.
    pub fn modal_open(&self) -> bool {
        self.prompt.is_some() || self.error.is_some() || self.settings.is_some()
    }

    /// The failure of the last request, for a caller that will report it itself.
    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    /// Lets the README screenshots show a neutral key location.
    #[cfg(test)]
    pub fn set_key_source(&mut self, source: &str) {
        if let Some(p) = &mut self.prompt {
            p.cfg.key_source = source.into();
        }
    }

    #[cfg(test)]
    pub fn prompt_open(&self) -> bool {
        self.prompt.is_some()
    }

    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// The image was replaced: an answer still on its way has nowhere to go
    /// (it is still saved, and can be added from the request history).
    pub fn document_changed(&mut self) {
        self.prompt = None;
        self.run = None;
        self.document_session = document_session();
    }
}

fn texture(ctx: &Context, name: &str, px: &Pixmap) -> TextureHandle {
    let img = ColorImage::from_rgba_unmultiplied([px.w as usize, px.h as usize], &px.data);
    ctx.load_texture(name, img, TextureOptions::LINEAR)
}

/// Shrink (never enlarge) `w` x `h` to fit in a square of `max`.
fn fit(w: u32, h: u32, max: f32) -> (u32, u32) {
    let s = (max / w.max(h).max(1) as f32).min(1.0);
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

fn reveal(path: &std::path::Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(opener).arg(path).spawn();
}

fn buttons_right(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(vec2(width, 24.0), egui::Layout::right_to_left(egui::Align::Center), add);
}

impl App {
    pub fn open_ai_prompt(&mut self) {
        self.open_ai_prompt_with(None);
    }

    /// A picture of exactly what `source` would send right now.
    fn ai_preview(&self, source: Source) -> Option<TextureHandle> {
        let mut job = aiedit::prepare(&self.doc.state, source, |w, h| fit(w, h, 560.0))?;
        // Preview: checkerboard under transparency, and dim what won't change.
        for (i, p) in job.image.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % job.image.w, i as u32 / job.image.w);
            let bg = if (x / 8 + y / 8) % 2 == 0 { 235u32 } else { 200 };
            let keep = job.mask.as_ref().map_or(0, |m| m.data[i * 4 + 3] as u32);
            let dim = 255 - keep * 150 / 255;
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = (((*c as u32 * a + bg * (255 - a)) / 255) * dim / 255) as u8;
            }
            p[3] = 255;
        }
        Some(texture(&self.ctx, "ai-preview", &job.image))
    }

    fn open_ai_prompt_with(&mut self, text: Option<String>) {
        if self.mutation_busy() { return self.toast("Finish the current operation before opening the AI prompt"); }
        if self.ai.run.is_some() {
            return self.toast("The AI is still working on the last request");
        }
        // Send what the user is looking at unless they say otherwise.
        let source = self.ai.source;
        let Some((source, preview)) = [source, Source::Visible].into_iter().find_map(|s| Some((s, self.ai_preview(s)?))) else {
            return self.toast("Nothing to send: the selection is outside the image");
        };
        let cfg = ai::Config::load(&self.saved.ai);
        self.ai.prompt = Some(Prompt { text: text.unwrap_or_else(|| self.ai.last_prompt.clone()), cfg, focus: true, source, preview });
    }

    /// Send the active layer (or the selected part of it) to the model. The
    /// request is recorded before upload only when history is enabled.
    pub fn send_to_ai(&mut self, ctx: &Context, prompt: String, cfg: ai::Config, source: Source) {
        if self.mutation_busy() || self.ai.run.is_some() {
            return self.toast("Finish the current operation before sending another image");
        }
        if let Err(e) = ai::endpoint_origin(&cfg.base) { return self.toast(e); }
        let model = cfg.model.clone();
        let Some(job) = aiedit::prepare(&self.doc.state, source, |w, h| ai::request_size(&model, w, h)) else {
            return self.toast("Nothing to send: the selection doesn't touch this layer");
        };
        let result_rect = job.result_rect();
        if let Err(e) = self.doc.check_layer_capacity(result_rect.width() as u32, result_rect.height() as u32) {
            return self.toast(format!("Free layer capacity before sending this request: {e}"));
        }
        let r = job.rect;
        let mut record = AiRecord {
            id: self.store.new_ai_id(),
            created: timestamp(),
            prompt: prompt.clone(),
            sent_prompt: ai::full_prompt(&prompt, job.mask.is_some()),
            model: cfg.model.clone(),
            quality: cfg.quality.clone().unwrap_or_default(),
            size: [job.image.w, job.image.h],
            document: self.source_file().map(|p| p.display().to_string()).unwrap_or_default(),
            layer: self.doc.state.layer(job.layer).map(|l| l.name.clone()).unwrap_or_default(),
            region: [r.x0, r.y0, r.x1, r.y1],
            source: if source == Source::Visible { "visible" } else { "layer" }.to_owned(),
            selection: job.mask.is_some(),
            canvas_size: [self.doc.state.width, self.doc.state.height],
            document_session: self.ai.document_session,
            source_layer: Some(job.layer),
            ..Default::default()
        };
        let keep = self.saved.ai.keep_history;
        let token = match self.store.begin_ai_record(&record, keep) {
            Ok(token) => token,
            Err(e) => { self.toast(format!("History could not be saved: {e}")); None }
        };
        let (tx, rx) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_job = job.clone();
        let (store, text, flag, ctx2) = (self.store.clone(), record.sent_prompt.clone(), cancelled.clone(), ctx.clone());
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let mut history_error = None;
            let result = (|| {
                let png = io::encode_png(&worker_job.image).map_err(|e| e.to_string())?;
                let mask_png = worker_job.mask.as_ref().map(io::encode_png).transpose().map_err(|e| e.to_string())?;
                if let Err(e) = store.record_ai(token.as_ref(), |s| {
                    s.write_ai_file(&record.id, "input.png", &png)?;
                    if let Some(m) = &mask_png { s.write_ai_file(&record.id, "mask.png", m)?; }
                    Ok(())
                }) { history_error = Some(e.to_string()); }
                ai::edit(&cfg, &text, (worker_job.image.w, worker_job.image.h), &png, mask_png.as_deref())
            })();
            record.duration_ms = started.elapsed().as_millis() as u64;
            record.status = if flag.load(Ordering::Relaxed) { "discarded" } else if result.is_ok() { "done" } else { "error" }.to_owned();
            if let Some(token) = token.as_ref() {
                let persist = (|| -> Result<(), String> {
                    let rendered = match &result {
                        Ok(reply) => {
                            let layer = worker_job.result_layer(&reply.pixels, "AI result");
                            record.result_origin = Some([layer.x, layer.y]);
                            record.usage = reply.usage.clone();
                            Some(io::encode_png(&layer.pixels).map_err(|e| e.to_string())?)
                        }
                        Err(e) => { record.error = e.clone(); None }
                    };
                    store.record_ai(Some(token), |s| {
                        if let Ok(reply) = &result { s.write_ai_file(&record.id, "output.png", &reply.file)?; }
                        if let Some(png) = &rendered { s.write_ai_file(&record.id, "result.png", png)?; }
                        s.write_ai_record(&record)
                    }).map_err(|e| e.to_string())
                })();
                if let Err(e) = persist { history_error = Some(e); }
                if let Err(e) = store.prune_ai_current() { history_error = Some(e.to_string()); }
            }
            let _ = tx.send(result.map(|r| Completed { pixels: r.pixels, history_error }));
            ctx2.request_repaint();
        });
        self.ai.last_prompt = prompt.clone();
        self.ai.run = Some(Run { rx, job, prompt, started: self.now, cancelled, pending: None });
        self.refresh_ai_history();
    }

    pub fn open_ai_history(&mut self) {
        match &self.ai.history {
            Some(_) => self.ai.history = None,
            None => {
                self.ai.history = Some(History { records: Vec::new(), selected: None, images: HashMap::new() });
                self.refresh_ai_history();
            }
        }
    }

    fn refresh_ai_history(&mut self) {
        if let Some(h) = &mut self.ai.history {
            h.records = self.store.ai_records();
            if h.selected.as_ref().is_none_or(|id| !h.records.iter().any(|r| r.id == *id)) || self.ai.run.is_some() {
                h.selected = h.records.first().map(|r| r.id.clone());
            }
            h.images.clear();
        }
    }

    pub fn open_settings(&mut self) {
        let mut edit = self.store.load_settings().0;
        if !edit.ai.api_key.is_empty() && ai::Config::load(&edit.ai).key.is_none() {
            edit.ai.api_key.clear();
            edit.ai.api_key_origin.clear();
        }
        self.ai.settings = Some(SettingsDlg { edit, show_key: false });
    }

    pub fn ai_dialogs(&mut self, ctx: &Context) {
        self.ai_prompt_dialog(ctx);
        self.ai_poll(ctx);
        self.ai_history_window(ctx);
        self.settings_dialog(ctx);

        if let Some(e) = &self.ai.error {
            let mut ok = false;
            egui::Modal::new(egui::Id::new("ai-error")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("The AI edit didn't work");
                ui.add_space(6.0);
                ui.label(e);
                ui.add_space(10.0);
                buttons_right(ui, 420.0, |ui| {
                    ok = ui.button("OK").clicked() || ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Escape));
                });
            });
            if ok {
                self.ai.error = None;
            }
        }
    }

    fn ai_prompt_dialog(&mut self, ctx: &Context) {
        let layer = self.doc.state.active_layer().map_or("layer", |l| l.name.as_str()).to_owned();
        let selection = self.doc.state.selection.is_some();
        let layers = self.doc.state.layers.iter().filter(|l| l.visible).count();
        let Some(p) = &mut self.ai.prompt else { return };
        let (mut send, mut close) = (false, false);
        let was = p.source;
        egui::Modal::new(egui::Id::new("ai-prompt")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Send to AI");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Send");
                ui.selectable_value(&mut p.source, Source::Visible, "Everything visible").on_hover_text("The image as you see it, all layers merged. The result goes on top.");
                ui.selectable_value(&mut p.source, Source::Layer, format!("Only \u{201c}{layer}\u{201d}")).on_hover_text("This layer by itself; the AI won't see the others. The result goes just above it.");
            });
            let mut what = "This is exactly what the AI will see.".to_owned();
            if selection {
                what += " Only the bright (selected) area is replaced; the rest is context.";
            }
            if p.source == Source::Layer && layers > 1 {
                what += " Other layers are not included.";
            }
            ui.label(RichText::new(what).weak());
            ui.add_space(4.0);
            let size = p.preview.size_vec2();
            let scale = (440.0 / size.x).min(200.0 / size.y).min(1.0);
            ui.vertical_centered(|ui| ui.image((p.preview.id(), size * scale)));
            ui.add_space(6.0);
            let edit = egui::TextEdit::multiline(&mut p.text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text(if selection {
                    "What should go in the selected area? e.g. \u{201c}remove the dog\u{201d}. Leave empty to just extend the picture into it."
                } else {
                    "What should change? e.g. \u{201c}make this look like Rome\u{201d}"
                });
            let r = ui.add(edit);
            if std::mem::take(&mut p.focus) {
                r.request_focus();
            }
            ui.add_space(4.0);
            match &p.cfg.key {
                Some(_) => {
                    let destination = ai::endpoint_origin(&p.cfg.base).unwrap_or_else(|e| e);
                    let note = format!("Uploads to {destination} ({}), using the key from {}. Usually takes up to a minute.", p.cfg.model, p.cfg.key_source);
                    ui.label(RichText::new(note).small().weak());
                }
                None => {
                    ui.colored_label(WARN, "No OpenAI key found.");
                    ui.label(RichText::new(if p.cfg.key_source.starts_with("The endpoint changed") { p.cfg.key_source.as_str() } else { "Add one in File > Settings, then open this dialog again." }).small());
                }
            }
            ui.add_space(10.0);
            let endpoint = ai::endpoint_origin(&p.cfg.base);
            if let Err(e) = &endpoint { ui.colored_label(WARN, e); }
            if self.saved.ai.keep_history == 0 { ui.label(RichText::new("History is off: this request and its images will not be saved locally.").small()); }
            let ready = p.cfg.key.is_some() && endpoint.is_ok() && (selection || !p.text.trim().is_empty());
            buttons_right(ui, 440.0, |ui| {
                let cmd_enter = ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                send = (ui.add_enabled(ready, egui::Button::new("Send")).on_hover_text("\u{2318}/Ctrl + Enter").clicked() || cmd_enter) && ready;
                close = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
            });
        });
        if p.source != was {
            let source = p.source;
            match self.ai_preview(source) {
                Some(t) => {
                    self.ai.prompt.as_mut().unwrap().preview = t;
                    self.ai.source = source;
                }
                None => {
                    self.ai.prompt.as_mut().unwrap().source = was;
                    self.toast("The selection doesn't touch that layer");
                }
            }
        }
        if send {
            let p = self.ai.prompt.take().unwrap();
            self.ai.source = p.source;
            self.send_to_ai(ctx, p.text.trim().to_owned(), p.cfg, p.source);
        } else if close {
            self.ai.last_prompt = self.ai.prompt.take().unwrap().text;
        }
    }

    /// Collect the answer if it has arrived; otherwise show progress.
    fn ai_poll(&mut self, ctx: &Context) {
        if self.mutation_busy() {
            if self.ai.run.is_some() { ctx.request_repaint_after(std::time::Duration::from_millis(250)); }
            return;
        }
        let Some(run) = &mut self.ai.run else { return };
        if run.pending.is_some() {
            let (mut retry, mut discard) = (false, false);
            egui::Area::new(egui::Id::new("ai-ready")).anchor(egui::Align2::CENTER_BOTTOM, [0.0, -64.0]).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::new().fill(Color32::from_black_alpha(225)).corner_radius(8).inner_margin(12).show(ui, |ui| {
                    ui.label("AI result is ready. Free some layer capacity, then add it.");
                    ui.horizontal(|ui| {
                        retry = ui.button("Add Result").clicked();
                        discard = ui.button("Discard Result").clicked();
                    });
                });
            });
            if discard { self.ai.run = None; return; }
            if !retry { return; }
        }
        let received = run.pending.take().map(|done| Ok(Ok(done))).unwrap_or_else(|| run.rx.try_recv());
        let outcome = match received {
            Ok(r) => r,
            Err(TryRecvError::Disconnected) => Err("The request stopped unexpectedly.".to_owned()),
            Err(TryRecvError::Empty) => {
                let secs = (self.now - run.started) as u32;
                let mut cancel = false;
                egui::Area::new(egui::Id::new("ai-status")).anchor(egui::Align2::CENTER_BOTTOM, [0.0, -64.0]).order(egui::Order::Foreground).show(ctx, |ui| {
                    egui::Frame::new().fill(Color32::from_black_alpha(225)).corner_radius(8).inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new(format!("AI is working\u{2026} {secs} s")).color(Color32::WHITE));
                            cancel = ui.small_button("Discard when ready").on_hover_text("Stops adding the result to this document. The request may still finish and be billed.").clicked();
                        });
                    });
                });
                if cancel {
                    run.cancelled.store(true, Ordering::Relaxed);
                    self.ai.run = None;
                    self.toast("Result will not be added. The API request may still finish and be billed.");
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
                return;
            }
        };
        if outcome.is_ok() {
            let r = run.job.result_rect();
            if let Err(e) = self.doc.check_layer_capacity(r.width() as u32, r.height() as u32) {
                run.pending = outcome.ok();
                self.toast(format!("AI result kept in memory until a layer can be added: {e}"));
                return;
            }
        }
        let run = self.ai.run.take().unwrap();
        match outcome {
            Ok(done) => {
                let short: String = if run.prompt.is_empty() { "extend".to_owned() } else { run.prompt.chars().take(28).collect() };
                self.doc.insert_ai_result(&run.job, &done.pixels, &format!("AI: {short}"));
                self.toast(match done.history_error {
                    Some(e) => format!("AI result added, but its history could not be saved: {e}"),
                    None => "AI result added as a new layer".into(),
                });
            }
            Err(e) => self.ai.error = Some(e),
        }
        self.refresh_ai_history();
    }

    fn ai_history_window(&mut self, ctx: &Context) {
        let can_insert = !self.mutation_busy();
        let Some(h) = &mut self.ai.history else { return };
        let store = self.store.clone();
        let (mut open, mut delete, mut reuse, mut add, mut clear) = (true, None, None, None, false);
        egui::Window::new("AI Requests").open(&mut open).default_pos(self.view.vp.left_top() + vec2(24.0, 24.0)).default_size([780.0, 520.0]).min_width(560.0).show(ctx, |ui| {
            if ui.button("Clear All History").on_hover_text("Deletes saved prompts and images, including recordings from requests still running.").clicked() { clear = true; }
            if h.records.is_empty() {
                ui.label(RichText::new("No requests yet. Use Layer > Send to AI with Prompt\u{2026}").weak());
                return;
            }
            egui::Panel::left("ai-list").resizable(false).exact_size(250.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for r in &h.records {
                        let mark = match r.status.as_str() {
                            "done" => egui_phosphor::regular::CHECK,
                            "error" => egui_phosphor::regular::WARNING,
                            "cancelled" | "discarded" => egui_phosphor::regular::X,
                            _ => egui_phosphor::regular::DOTS_THREE,
                        };
                        let when = r.created.replace('T', " ").trim_end_matches('Z').to_owned();
                        let first: String = r.prompt.lines().next().unwrap_or("").chars().take(60).collect();
                        let on = h.selected.as_deref() == Some(r.id.as_str());
                        let text = format!("{mark}  {first}\n{when} UTC");
                        if ui.add_sized([ui.available_width(), 38.0], egui::Button::selectable(on, RichText::new(text).small())).clicked() && !on {
                            h.selected = Some(r.id.clone());
                            h.images.clear();
                        }
                    }
                });
            });
            let Some(r) = h.records.iter().find(|r| Some(r.id.as_str()) == h.selected.as_deref()) else { return };
            egui::ScrollArea::vertical().show(ui, |ui| {
                let shown = if r.prompt.is_empty() { "(no prompt: extend the picture)" } else { r.prompt.as_str() };
                ui.add(egui::Label::new(RichText::new(shown).strong()).wrap()).on_hover_text(format!("Sent to the model as:\n{}", if r.sent_prompt.is_empty() { &r.prompt } else { &r.sent_prompt }));
                let status = match r.status.as_str() {
                    "done" => "Done".to_owned(),
                    "error" => "Failed".to_owned(),
                    "cancelled" | "discarded" => "Not added (the API request may still be billed)".to_owned(),
                    _ => "Sent, no answer recorded".to_owned(),
                };
                let secs = r.duration_ms as f32 / 1000.0;
                ui.label(RichText::new(format!("{status} \u{b7} {} \u{b7} {}\u{d7}{} px \u{b7} {secs:.1} s", r.model, r.size[0], r.size[1])).small().weak());
                // Paths under the home folder read better, and screenshot better, as ~/...
                let home = std::env::var("HOME").unwrap_or_default();
                let short = match r.document.strip_prefix(&home) {
                    Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
                    _ => r.document.clone(),
                };
                let from = if short.is_empty() { "an unsaved image" } else { short.as_str() };
                let part = if r.selection { "the selection in" } else { "all of" };
                let what = if r.source == "visible" { "everything visible".to_owned() } else { format!("only layer \u{201c}{}\u{201d}", r.layer) };
                ui.label(RichText::new(format!("Sent {part} {from}: {what}")).small().weak());
                if !r.error.is_empty() {
                    ui.colored_label(WARN, &r.error);
                }
                ui.add_space(6.0);
                let dir = store.ai_path(&r.id);
                let w = ((ui.available_width() - 12.0) / 2.0).max(120.0);
                ui.horizontal_top(|ui| {
                    for (file, label) in [("input.png", "Sent"), ("output.png", "Came back")] {
                        let tex = h.images.entry(file).or_insert_with(|| store.ai_file(&r.id, file).and_then(|path| io::load_pixmap(&path).ok()).map(|px| texture(ui.ctx(), &format!("ai-{file}"), &px)));
                        ui.vertical(|ui| {
                            ui.set_width(w);
                            ui.label(RichText::new(label).small());
                            match tex {
                                Some(t) => {
                                    let s = t.size_vec2();
                                    ui.image((t.id(), s * (w / s.x).min(360.0 / s.y).min(1.0)));
                                }
                                None => {
                                    ui.label(RichText::new("(no image)").weak());
                                }
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let available = store.ai_file(&r.id, "result.png").is_some() || (!r.selection && store.ai_file(&r.id, "output.png").is_some());
                    if ui.add_enabled(can_insert && available, egui::Button::new("Add Result as Layer")).on_hover_text(if r.selection && !available { "This older request has no saved clipped layer. Its raw response is available through Show Files." } else { "Restore the saved layer with its original selection clipping." }).clicked() {
                        add = Some(r.clone());
                    }
                    if ui.button("Reuse Prompt").clicked() {
                        reuse = Some(r.prompt.clone());
                    }
                    if ui.button("Show Files").clicked() {
                        reveal(&dir);
                    }
                    if ui.button("Delete").clicked() {
                        delete = Some(r.id.clone());
                    }
                });
            });
        });
        if !open {
            self.ai.history = None;
        }
        if clear {
            if let Err(e) = self.store.clear_ai() { self.toast(format!("Could not clear AI history: {e}")); }
            self.refresh_ai_history();
        }
        if let Some(id) = delete {
            if let Err(e) = self.store.delete_ai(&id) { self.toast(format!("Could not delete AI history: {e}")); }
            self.refresh_ai_history();
        }
        if let Some(text) = reuse {
            self.open_ai_prompt_with(Some(text));
        }
        if let Some(r) = add {
            if let Err(e) = self.restore_ai_record(&r) { self.toast(e); }
        }
    }

    fn restore_ai_record(&mut self, r: &AiRecord) -> Result<(), String> {
        if self.mutation_busy() { return Err("Finish the current operation before adding this result.".into()); }
        let exact = r.result_origin.is_some() && self.store.ai_file(&r.id, "result.png").is_some();
        if r.selection && !exact {
            return Err("This older request has no saved clipped layer. Its raw response is available through Show Files.".into());
        }
        let file = self.store.ai_file(&r.id, if exact { "result.png" } else { "output.png" }).ok_or("The saved image is missing or is not a regular file.")?;
        let px = io::load_pixmap(&file).map_err(|e| format!("Couldn't load the result: {e}"))?;
        let same_session = r.document_session != 0 && r.document_session == self.ai.document_session;
        let same_file = !r.document.is_empty() && r.document == self.source_file().map(|p| p.display().to_string()).unwrap_or_default();
        let same_size = r.canvas_size == [self.doc.state.width, self.doc.state.height] || r.canvas_size == [0; 2];
        let positioned = same_size && (same_session || same_file);
        let short: String = r.prompt.chars().take(28).collect();
        let name = format!("AI: {short}");
        if positioned {
            if r.source == "visible" {
                if let Some(l) = self.doc.state.layers.last() { self.doc.state.active = l.id; }
            } else if let Some(id) = r.source_layer.filter(|id| same_session && self.doc.state.layer(*id).is_some()) {
                self.doc.state.active = id;
            }
            if let Some([x, y]) = r.result_origin.filter(|_| exact) {
                io::limits::validate_offset(x, y).map_err(|e| e.to_string())?;
                self.doc.try_insert_layer("AI History", Layer::new(&name, px, x, y)).map_err(|e| e.to_string())?;
            } else {
                let [x0, y0, x1, y1] = r.region;
                let width = u32::try_from(i64::from(x1) - i64::from(x0)).map_err(|_| "Invalid saved image width")?;
                let height = u32::try_from(i64::from(y1) - i64::from(y0)).map_err(|_| "Invalid saved image height")?;
                io::limits::validate_dimensions(width, height).map_err(|e| e.to_string())?;
                io::limits::validate_offset(x0, y0).map_err(|e| e.to_string())?;
                let region = IRect::new(x0, y0, x1, y1);
                if region.is_empty() { return Err("The saved request has an invalid image area.".into()); }
                self.doc.add_image_scaled(&name, &px, region).map_err(|e| e.to_string())?;
            }
        } else {
            let x = (self.doc.state.width as i32 - px.w as i32) / 2;
            let y = (self.doc.state.height as i32 - px.h as i32) / 2;
            self.doc.try_insert_layer("AI History", Layer::new(&name, px, x, y)).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn settings_dialog(&mut self, ctx: &Context) {
        let path = self.store.config_file();
        let Some(d) = &mut self.ai.settings else { return };
        let (mut save, mut close) = (false, false);
        egui::Modal::new(egui::Id::new("settings")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Settings");
            ui.add_space(6.0);
            ui.label(RichText::new("AI (OpenAI)").strong());
            egui::Grid::new("settings-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("API URL");
                let before_url = d.edit.ai.base_url.clone();
                ui.add(egui::TextEdit::singleline(&mut d.edit.ai.base_url).desired_width(270.0).hint_text("https://api.openai.com/v1"));
                if d.edit.ai.base_url != before_url { d.edit.ai.api_key.clear(); d.edit.ai.api_key_origin.clear(); }
                ui.end_row();
                ui.label("API key");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.edit.ai.api_key).password(!d.show_key).desired_width(270.0).hint_text("sk-\u{2026}"));
                    ui.toggle_value(&mut d.show_key, "Show");
                });
                ui.end_row();
                ui.label("Model");
                ui.add(egui::TextEdit::singleline(&mut d.edit.ai.model).desired_width(270.0).hint_text(ai::DEFAULT_MODEL));
                ui.end_row();
                ui.label("Quality");
                ui.horizontal(|ui| {
                    for (v, label) in [("", "Auto"), ("low", "Low"), ("medium", "Medium"), ("high", "High")] {
                        ui.selectable_value(&mut d.edit.ai.quality, v.to_owned(), label);
                    }
                });
                ui.end_row();
                ui.label("Keep");
                ui.add(egui::DragValue::new(&mut d.edit.ai.keep_history).range(0..=5000).suffix(" past requests"));
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.checkbox(&mut d.edit.bridge.enabled, "Allow local MCP clients to control this app")
                .on_hover_text("Lets programs running as you, such as Claude Code through `pixelferrite mcp`, edit the open document. Takes effect after restarting Pixelferrite.");
            ui.label(RichText::new("Set Keep to 0 to disable history and erase saved requests. Changing the API URL clears the old key; enter the key for the new destination.").small().weak());
            let live = ai::Config::load(&d.edit.ai);
            let note = match &live.key {
                Some(_) if live.key_source != "Pixelferrite settings" => format!("The key from {} will be used while the settings key is empty.", live.key_source),
                Some(_) => "Higher quality costs more per request.".to_owned(),
                None => "No key yet: AI edits won't work until one is set.".to_owned(),
            };
            ui.label(RichText::new(note).small().weak());
            ui.label(RichText::new(format!("Saved in {}", path.display())).small().weak());
            ui.add_space(10.0);
            buttons_right(ui, 440.0, |ui| {
                save = ui.button("Save").clicked();
                close = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
            });
        });
        if save {
            let d = self.ai.settings.as_mut().unwrap();
            let base = if d.edit.ai.base_url.trim().is_empty() { "https://api.openai.com/v1" } else { d.edit.ai.base_url.trim() };
            match ai::endpoint_origin(base) {
                Ok(origin) => d.edit.ai.api_key_origin = origin,
                Err(e) => { self.toast(e); return; }
            }
            let d = self.ai.settings.take().unwrap();
            match self.store.save_settings(&d.edit) {
                Ok(()) => {
                    if let Err(e) = self.store.prune_ai(d.edit.ai.keep_history) { self.toast(format!("Could not apply history retention: {e}")); }
                    self.saved = d.edit;
                    self.refresh_ai_history();
                },
                Err(e) => { self.ai.settings = Some(d); self.toast(format!("Couldn't save settings: {e}")); },
            }
        } else if close {
            self.ai.settings = None;
        }
    }
}

#[cfg(test)]
#[path = "ai_regressions.rs"]
mod regressions;
