use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Color32, ColorImage, Context, Key, Modifiers, Pos2, TextureHandle, TextureOptions, ViewportCommand, vec2};
use pf_core::ops::{Clip, Reorder};
use pf_core::{DocState, Document, Layer, LayerId, io};

use crate::canvas::{self, Ants, Drag};
use crate::panels;
use crate::tools::{Settings, Tool};
use crate::view::{Display, View};

pub const BG_CANVAS: Color32 = Color32::from_rgb(26, 26, 28);
pub const BG_PANEL: Color32 = Color32::from_rgb(38, 38, 41);
pub const BG_BAR: Color32 = Color32::from_rgb(32, 32, 35);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    New,
    Open,
    AddImage,
    Save,
    SaveAs,
    Export,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Clear,
    FillForeground,
    SelectAll,
    Deselect,
    InvertSelection,
    Crop,
    NewLayer,
    DuplicateLayer,
    DeleteLayer,
    MergeDown,
    Flatten,
    AddMask,
    DeleteMask,
    ApplyMask,
    InvertMask,
    FlipH,
    FlipV,
    Reorder(Reorder),
    ZoomIn,
    ZoomOut,
    ZoomFit,
    Zoom100,
    ResetRotation,
}

pub struct Thumb {
    rev: u64,
    pub pixels: TextureHandle,
    pub mask: Option<TextureHandle>,
}

pub struct NewDoc {
    pub w: u32,
    pub h: u32,
    pub transparent: bool,
}

pub struct App {
    pub doc: Document,
    pub view: View,
    pub display: Display,
    pub fit_pending: bool,
    pub tool: Tool,
    pub settings: Settings,
    pub drag: Drag,
    pub zoom_scrubbed: bool,
    pub pointer: (Pos2, bool),
    pub clone_src: Option<Pos2>,
    pub clone_off: Option<(i32, i32)>,
    pub clipboard: Option<Clip>,
    pub thumbs: HashMap<LayerId, Thumb>,
    pub ants: Ants,
    pub show_layers: bool,
    pub show_inspector: bool,
    pub toast: Option<(String, f64)>,
    pub new_doc: Option<NewDoc>,
    pub rename: Option<(LayerId, String, bool)>,
    /// Snapshot from before a slider drag started, committed when it ends.
    pub prop: Option<(egui::Id, DocState)>,
    pub layer_drag: Option<(LayerId, usize)>,
    pub last_rotate: f64,
    title: String,
    now: f64,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> Self {
        setup_style(&cc.egui_ctx);
        let mut app = Self {
            doc: Document::new(1600, 1200, Some([255; 4])),
            view: View::new(),
            display: Display::new(),
            fit_pending: true,
            tool: Tool::Brush,
            settings: Settings::default(),
            drag: Drag::None,
            zoom_scrubbed: false,
            pointer: (Pos2::ZERO, false),
            clone_src: None,
            clone_off: None,
            clipboard: None,
            thumbs: HashMap::new(),
            ants: Ants::default(),
            show_layers: true,
            show_inspector: true,
            toast: None,
            new_doc: None,
            rename: None,
            prop: None,
            layer_drag: None,
            last_rotate: 0.0,
            title: String::new(),
            now: 0.0,
        };
        if let Some(f) = file {
            app.open_path(&f);
        }
        app
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), self.now + 3.5));
    }

    /// Last pointer position over the canvas and whether Alt was held.
    pub fn ctx_pointer(&self) -> (Pos2, bool) {
        self.pointer
    }

    pub fn doc_name(&self) -> String {
        match &self.doc.path {
            Some(p) => p.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled").to_owned(),
            None => "Untitled".to_owned(),
        }
    }

    fn set_doc(&mut self, doc: Document) {
        canvas::cancel(self);
        self.doc = doc;
        self.doc.mark_all_dirty();
        self.thumbs.clear();
        self.fit_pending = true;
        self.clone_src = None;
        self.clone_off = None;
        self.prop = None;
        self.rename = None;
    }

    /// Ask before throwing away unsaved work. True means go ahead.
    fn confirm_discard(&self) -> bool {
        if !self.doc.modified {
            return true;
        }
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Unsaved changes")
            .set_description(format!("\u{201c}{}\u{201d} has unsaved changes. Discard them?", self.doc_name()))
            .set_buttons(rfd::MessageButtons::OkCancelCustom("Discard".into(), "Cancel".into()))
            .show()
            == rfd::MessageDialogResult::Custom("Discard".into())
    }

    pub fn open_path(&mut self, path: &Path) {
        match io::open(path) {
            Ok(doc) => self.set_doc(doc),
            Err(e) => self.toast(format!("Couldn't open {}: {e}", path.display())),
        }
    }

    fn add_image(&mut self, path: &Path) {
        match io::load_pixmap(path) {
            Ok(px) => {
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Layer").to_owned();
                self.doc.add_image_layer(&name, px);
            }
            Err(e) => self.toast(format!("Couldn't open {}: {e}", path.display())),
        }
    }

    fn save(&mut self, save_as: bool) {
        let path = match (&self.doc.path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("OpenRaster layered image", &["ora"])
                .set_file_name(format!("{}.ora", self.doc_name()))
                .save_file()
                .map(|p| if io::is_ora(&p) { p } else { p.with_extension("ora") }),
        };
        let Some(path) = path else { return };
        match io::save(&mut self.doc, &path) {
            Ok(()) => self.toast(format!("Saved {}", path.display())),
            Err(e) => self.toast(format!("Save failed: {e}")),
        }
    }

    fn export(&mut self) {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("PNG image", &["png"])
            .add_filter("JPEG image", &["jpg", "jpeg"])
            .add_filter("GIF image", &["gif"])
            .set_file_name(format!("{}.png", self.doc_name()))
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("png");
        }
        match io::export(&self.doc.state, &path) {
            Ok(()) => self.toast(format!("Exported {}", path.display())),
            Err(e) => self.toast(format!("Export failed: {e}")),
        }
    }

    pub fn run(&mut self, ctx: &Context, a: Action) {
        if !matches!(self.drag, Drag::None) {
            return;
        }
        let active = self.doc.state.active;
        match a {
            Action::New => self.new_doc = Some(NewDoc { w: self.doc.state.width, h: self.doc.state.height, transparent: false }),
            Action::Open => {
                if self.confirm_discard() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Images", io::OPEN_EXTENSIONS).pick_file() {
                        self.open_path(&p);
                    }
                }
            }
            Action::AddImage => {
                let exts: Vec<&str> = io::OPEN_EXTENSIONS.iter().copied().filter(|e| *e != "ora").collect();
                for p in rfd::FileDialog::new().add_filter("Images", &exts).pick_files().unwrap_or_default() {
                    self.add_image(&p);
                }
            }
            Action::Save => self.save(false),
            Action::SaveAs => self.save(true),
            Action::Export => self.export(),
            Action::Undo => {
                self.doc.undo();
            }
            Action::Redo => {
                self.doc.redo();
            }
            Action::Copy | Action::Cut => {
                let clip = if a == Action::Cut { self.doc.cut() } else { self.doc.copy() };
                if let Some(c) = clip {
                    self.clipboard = Some(c);
                    // The OS only reports a paste keystroke when the clipboard
                    // holds text, so leave a marker there.
                    ctx.copy_text("Pixelferrite layer".to_owned());
                }
            }
            Action::Paste => match self.clipboard.clone() {
                Some(c) => {
                    self.doc.paste(&c);
                    self.tool = Tool::Move;
                }
                None => self.toast("Nothing to paste"),
            },
            Action::Clear => {
                self.doc.clear();
            }
            Action::FillForeground => {
                self.doc.fill(self.settings.fg);
            }
            Action::SelectAll => self.doc.select_all(),
            Action::Deselect => self.doc.deselect(),
            Action::InvertSelection => self.doc.invert_selection(),
            Action::Crop => match self.doc.state.selection.as_deref().and_then(pf_core::selection::bounds) {
                Some(r) => {
                    self.doc.crop(r);
                    self.fit_pending = true;
                }
                None => self.toast("Select an area to crop to first"),
            },
            Action::NewLayer => {
                self.doc.add_empty_layer();
            }
            Action::DuplicateLayer => self.doc.duplicate_layer(active),
            Action::DeleteLayer => self.doc.delete_layer(active),
            Action::MergeDown => self.doc.merge_down(active),
            Action::Flatten => self.doc.flatten_image(),
            Action::AddMask => self.doc.add_mask(active),
            Action::DeleteMask => self.doc.delete_mask(active),
            Action::ApplyMask => self.doc.apply_mask(active),
            Action::InvertMask => self.doc.invert_mask(active),
            Action::FlipH => self.doc.flip_layer(active, true),
            Action::FlipV => self.doc.flip_layer(active, false),
            Action::Reorder(r) => self.doc.reorder_layer(active, r),
            Action::ZoomIn => self.view.set_zoom(self.view.zoom * 1.5),
            Action::ZoomOut => self.view.set_zoom(self.view.zoom / 1.5),
            Action::ZoomFit => self.fit_pending = true,
            Action::Zoom100 => self.view.set_zoom(1.0),
            Action::ResetRotation => {
                let (c, r) = (self.view.vp.center(), self.view.rot);
                self.view.rotate_about(c, -r);
            }
        }
    }

    fn shortcuts(&mut self, ctx: &Context) {
        const CMD: Modifiers = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let cmd_alt = Modifiers::COMMAND | Modifiers::ALT;
        // Longer chords first: `consume_key` ignores extra Shift/Alt.
        let table: [(Modifiers, Key, Action); 24] = [
            (cmd_shift, Key::Z, Action::Redo),
            (cmd_shift, Key::S, Action::SaveAs),
            (cmd_shift, Key::I, Action::InvertSelection),
            (cmd_shift, Key::N, Action::NewLayer),
            (cmd_shift, Key::E, Action::Export),
            (cmd_shift, Key::O, Action::AddImage),
            (cmd_alt, Key::Num0, Action::ResetRotation),
            (CMD, Key::Z, Action::Undo),
            (CMD, Key::Y, Action::Redo),
            (CMD, Key::S, Action::Save),
            (CMD, Key::O, Action::Open),
            (CMD, Key::N, Action::New),
            (CMD, Key::E, Action::MergeDown),
            (CMD, Key::A, Action::SelectAll),
            (CMD, Key::D, Action::Deselect),
            (CMD, Key::J, Action::DuplicateLayer),
            (CMD, Key::Num0, Action::ZoomFit),
            (CMD, Key::Num1, Action::Zoom100),
            (CMD, Key::Plus, Action::ZoomIn),
            (CMD, Key::Equals, Action::ZoomIn),
            (CMD, Key::Minus, Action::ZoomOut),
            (CMD, Key::CloseBracket, Action::Reorder(Reorder::Forward)),
            (CMD, Key::OpenBracket, Action::Reorder(Reorder::Backward)),
            (Modifiers::ALT, Key::Backspace, Action::FillForeground),
        ];
        for (m, k, a) in table {
            if ctx.input_mut(|i| i.consume_key(m, k)) {
                self.run(ctx, a);
            }
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        for e in events {
            match e {
                egui::Event::Copy => self.run(ctx, Action::Copy),
                egui::Event::Cut => self.run(ctx, Action::Cut),
                egui::Event::Paste(_) => self.run(ctx, Action::Paste),
                _ => {}
            }
        }
        let key = |k: Key| ctx.input(|i| i.modifiers.is_none() && i.key_pressed(k));
        if key(Key::Delete) || key(Key::Backspace) {
            self.run(ctx, Action::Clear);
        }
        if matches!(self.drag, Drag::None) {
            for t in Tool::STRIP.into_iter().flatten() {
                if key(t.key()) {
                    self.tool = t;
                }
            }
        }
        if key(Key::X) {
            std::mem::swap(&mut self.settings.fg, &mut self.settings.bg);
        }
        if key(Key::D) {
            (self.settings.fg, self.settings.bg) = ([0, 0, 0, 255], [255; 4]);
        }
        let tool = self.tool;
        let grow = if key(Key::CloseBracket) { 1.25 } else if key(Key::OpenBracket) { 0.8 } else { 1.0 };
        if grow != 1.0 {
            let size = match tool {
                Tool::QuickSelect => Some(&mut self.settings.quick_size),
                _ => self.settings.params_mut(tool).map(|p| &mut p.size),
            };
            if let Some(s) = size {
                *s = (*s * grow).clamp(1.0, 2000.0);
                if grow > 1.0 { *s = s.ceil() } else { *s = s.floor().max(1.0) }
            }
        }
    }

    /// Layer thumbnail textures, refreshed when the layer's content revision changes.
    pub fn thumb(&mut self, ctx: &Context, layer: &Layer) -> &Thumb {
        let stale = self.thumbs.get(&layer.id).is_none_or(|t| t.rev != layer.rev);
        if stale {
            let (w, h) = (layer.pixels.w.max(1) as f32, layer.pixels.h.max(1) as f32);
            let s = (80.0 / w.max(h)).min(1.0);
            let size = [((w * s).round() as usize).max(1), ((h * s).round() as usize).max(1)];
            let at = |x: usize, y: usize| {
                (((x as f32 + 0.5) / size[0] as f32 * w) as i32, ((y as f32 + 0.5) / size[1] as f32 * h) as i32)
            };
            let mut px = Vec::with_capacity(size[0] * size[1]);
            let mut mk = Vec::with_capacity(size[0] * size[1]);
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let (sx, sy) = at(x, y);
                    let p = layer.pixels.get(sx, sy).unwrap_or([0; 4]);
                    let bg = if (x / 6 + y / 6) % 2 == 0 { 250u32 } else { 205 };
                    let a = p[3] as u32;
                    let f = |c: u8| ((c as u32 * a + bg * (255 - a)) / 255) as u8;
                    px.push(Color32::from_rgb(f(p[0]), f(p[1]), f(p[2])));
                    if let Some(m) = &layer.mask {
                        mk.push(Color32::from_gray(m.get(sx, sy).map_or(0, |v| v[0])));
                    }
                }
            }
            let tex = |name: String, px: Vec<Color32>| ctx.load_texture(name, ColorImage::new(size, px), TextureOptions::LINEAR);
            let t = Thumb {
                rev: layer.rev,
                pixels: tex(format!("thumb-{}", layer.id), px),
                mask: layer.mask.is_some().then(|| tex(format!("mask-{}", layer.id), mk)),
            };
            self.thumbs.insert(layer.id, t);
        }
        &self.thumbs[&layer.id]
    }

    /// Fold a continuously-edited property (slider drag) into a single undo
    /// step. `snap` must be the state from before the widget ran this frame.
    pub fn track(&mut self, resp: &egui::Response, snap: &DocState, name: &str) {
        if resp.changed() {
            self.doc.mark_all_dirty();
            if self.prop.is_none() {
                self.prop = Some((resp.id, snap.clone()));
            }
        }
        if self.prop.as_ref().is_some_and(|(id, _)| *id == resp.id) && !resp.dragged() {
            let (_, before) = self.prop.take().unwrap();
            self.doc.commit(name, before);
        }
    }

    fn new_doc_dialog(&mut self, ctx: &Context) {
        let Some(nd) = &mut self.new_doc else { return };
        let mut create = false;
        let mut close = false;
        egui::Modal::new(egui::Id::new("new-doc")).show(ctx, |ui| {
            ui.set_width(260.0);
            ui.heading("New Image");
            ui.add_space(8.0);
            egui::Grid::new("new-doc-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Width");
                ui.add(egui::DragValue::new(&mut nd.w).range(1..=16384).suffix(" px"));
                ui.end_row();
                ui.label("Height");
                ui.add(egui::DragValue::new(&mut nd.h).range(1..=16384).suffix(" px"));
                ui.end_row();
                ui.label("Background");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut nd.transparent, false, "White");
                    ui.selectable_value(&mut nd.transparent, true, "Transparent");
                });
                ui.end_row();
            });
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                create = ui.button("Create").clicked() || ui.input(|i| i.key_pressed(Key::Enter));
                close = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape));
            });
        });
        if create {
            let nd = self.new_doc.take().unwrap();
            if self.confirm_discard() {
                self.set_doc(Document::new(nd.w, nd.h, (!nd.transparent).then_some([255; 4])));
            }
        } else if close {
            self.new_doc = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.now = ctx.input(|i| i.time);

        if self.new_doc.is_none() {
            self.shortcuts(&ctx);
        }

        // Files dropped on the window become new layers.
        for f in ctx.input(|i| i.raw.dropped_files.clone()) {
            {
                let p = f.path().to_path_buf();
                if p.as_os_str().is_empty() {
                    continue;
                }
                if io::is_ora(&p) {
                    if self.confirm_discard() {
                        self.open_path(&p);
                    }
                } else {
                    self.add_image(&p);
                }
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) && !self.confirm_discard() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }

        let title = format!("{}{}", self.doc_name(), if self.doc.modified { " \u{2014} Edited" } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(format!("{title} \u{2013} Pixelferrite")));
            self.title = title;
        }

        let bar = egui::Frame::new().fill(BG_BAR).inner_margin(egui::Margin::symmetric(10, 6));
        egui::Panel::top("top-bar").frame(bar).show(ui, |ui| panels::top_bar(self, ui));
        if self.show_layers {
            let frame = egui::Frame::new().fill(BG_PANEL);
            egui::Panel::left("layers").frame(frame).exact_size(272.0).resizable(false).show(ui, |ui| {
                panels::layers(self, ui);
            });
        }
        let strip = egui::Frame::new().fill(BG_BAR).inner_margin(egui::Margin::symmetric(4, 8));
        egui::Panel::right("tool-strip").frame(strip).exact_size(42.0).resizable(false).show(ui, |ui| {
            panels::tool_strip(self, ui);
        });
        if self.show_inspector {
            let frame = egui::Frame::new().fill(BG_PANEL).inner_margin(egui::Margin::symmetric(14, 10));
            egui::Panel::right("inspector").frame(frame).exact_size(264.0).resizable(false).show(ui, |ui| {
                panels::inspector(self, ui);
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG_CANVAS)).show(ui, |ui| {
            canvas::canvas(self, ui);
        });

        // A slider snapshot must never outlive its pointer interaction (the
        // widget may have disappeared mid-drag).
        if self.prop.is_some() && !ctx.input(|i| i.pointer.any_down()) {
            if let Some((_, before)) = self.prop.take() {
                self.doc.commit("Adjust", before);
            }
        }

        self.new_doc_dialog(&ctx);

        if let Some((msg, until)) = &self.toast {
            if self.now < *until {
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -28.0])
                    .order(egui::Order::Tooltip)
                    .interactable(false)
                    .show(&ctx, |ui| {
                        egui::Frame::new()
                            .fill(Color32::from_black_alpha(215))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(14, 8))
                            .show(ui, |ui| ui.label(egui::RichText::new(msg).color(Color32::WHITE)));
                    });
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            } else {
                self.toast = None;
            }
        }
    }
}

fn setup_style(ctx: &Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    ctx.all_styles_mut(|s| {
        let v = &mut s.visuals;
        v.panel_fill = BG_PANEL;
        v.window_fill = Color32::from_rgb(44, 44, 48);
        v.extreme_bg_color = Color32::from_rgb(24, 24, 26);
        v.selection.bg_fill = Color32::from_rgb(47, 111, 222);
        v.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(64, 64, 69);
        v.widgets.inactive.bg_fill = Color32::from_rgb(78, 78, 84);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(80, 80, 86);
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(54, 54, 58));
        v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(200, 200, 205));
        v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(225, 225, 230));
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(8.0, 3.0);
        s.spacing.slider_width = 150.0;
        s.interaction.tooltip_delay = 0.4;
    });
}
