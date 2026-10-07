use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Color32, ColorImage, Context, Key, Modifiers, Pos2, TextureHandle, TextureOptions, ViewportCommand, vec2};
use pf_core::filter::{EdgeParams, EdgeStyle, Filter, FilterOp};
use pf_core::ops::{Clip, Reorder};
use pf_core::text::{FontRef, TextSpec};
use pf_core::{DocState, Document, Layer, LayerId, io};

use crate::aiui::AiUi;
use crate::canvas::{self, Ants, Drag};
use crate::fonts::Fonts;
use crate::fxui::{ScaleDlg, Warp};
use crate::store::{Settings as Saved, Store};
use crate::panels;
use crate::recovery::{Recovery, Candidate};
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
    InsertInSelection,
    Save,
    SaveAs,
    Export,
    ExportCompatible,
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
    GaussianBlur,
    EdgeDetect,
    OpenFilter(Filter),
    Perspective,
    ContentAwareScale,
    SelectionToTextPath,
    AiPrompt,
    AiHistory,
    Settings,
    ClearRecent,
}

/// A labelled slider for a filter parameter; `log` suits sizes in pixels.
/// Returns whether the value changed.
fn slider(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, log: bool, suffix: &str, tip: &str) -> bool {
    ui.label(label);
    let r = ui.add(egui::Slider::new(v, range).logarithmic(log).suffix(suffix).max_decimals(2));
    if tip.is_empty() { r.changed() } else { r.on_hover_text(tip).changed() }
}

/// A filter being previewed in its dialog.
pub struct FilterDlg {
    pub(crate) op: FilterOp,
    pub(crate) filter: Filter,
    pub(crate) before: DocState,
    /// Parameters changed since the preview was last rendered.
    pub(crate) pending: bool,
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
    pub fonts: Fonts,
    /// Undo merge key for the current run of text edits.
    pub text_key: u64,
    /// Put the keyboard in the text box (and select its contents) next frame.
    pub text_focus: Option<bool>,
    /// The next drag with the type tool redraws the active text's path.
    pub text_repath: bool,
    text_ctx: (Tool, LayerId),
    pub filter: Option<FilterDlg>,
    pub warp: Option<Warp>,
    pub scale_dlg: Option<ScaleDlg>,
    /// Cached edge-bounded regions for the region selection tool.
    pub regions: Option<(u64, pf_core::segment::Regions)>,
    pub ai: AiUi,
    pub ctx: Context,
    pub store: Store,
    opened: Option<PathBuf>,
    pub saved: Saved,
    title: String,
    pub now: f64,
    pending_drops: Vec<PathBuf>,
    pub error: Option<String>,
    recovery: Option<Recovery>,
    recoveries: Vec<Candidate>,
    recovered_source: Option<Candidate>,
    pub(crate) work: Option<crate::jobs::Work>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> Self {
        setup_style(&cc.egui_ctx);
        // Tests get a throwaway folder so they never touch real settings.
        #[cfg(test)]
        let store = {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Store::at(&std::env::temp_dir().join(format!("pf-app-{}-{n}", std::process::id())))
        };
        #[cfg(not(test))]
        let store = Store::standard();
        let (saved, settings_error) = store.load_settings();
        let retention_error = store.prune_ai(saved.ai.keep_history).err().map(|e| format!("Couldn't apply AI history retention: {e}"));
        let recovery_root = store.data_dir.join("recovery");
        let recoveries = Recovery::discover(&recovery_root);
        let recovery = Recovery::new(&recovery_root);
        let recovery_error = recovery.as_ref().err().map(|e| format!("Crash recovery is unavailable: {e}"));
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
            fonts: Fonts::default(),
            text_key: 0,
            text_focus: None,
            text_repath: false,
            text_ctx: (Tool::Brush, 0),
            filter: None,
            warp: None,
            scale_dlg: None,
            regions: None,
            ai: AiUi::default(),
            ctx: cc.egui_ctx.clone(),
            store: store.clone(),
            opened: None,
            saved,
            title: String::new(),
            now: 0.0,
            pending_drops: Vec::new(),
            error: recovery_error.or(retention_error),
            recovery: recovery.ok(),
            recoveries,
            recovered_source: None,
            work: None,
        };
        if let Some(e) = settings_error {
            app.toast(format!("Couldn't read settings, using defaults. {e}"));
        }
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
        self.cancel_work();
        if let Some(recovery) = &mut self.recovery { recovery.clear(); }
        if let Some(old) = self.recovered_source.take() { old.discard(); }
        self.opened = None;
        self.doc = doc;
        self.doc.mark_all_dirty();
        self.thumbs.clear();
        self.fit_pending = true;
        self.clone_src = None;
        self.clone_off = None;
        self.prop = None;
        self.rename = None;
        self.filter = None;
        self.warp = None;
        self.scale_dlg = None;
        self.regions = None;
        self.ai.document_changed();
    }

    /// Re-render a text layer from `spec`. Edits made in a row to the same
    /// layer are a single undo step.
    pub fn apply_text(&mut self, id: LayerId, spec: TextSpec) {
        let (data, index) = self.fonts.face(&spec.font, spec.bold, spec.italic);
        let Ok(font) = FontRef::try_from_slice_and_index(&data, index) else { return };
        let before = self.doc.begin();
        if self.doc.update_text(id, spec, &font) {
            self.doc.commit_merged("Edit Text", before, self.text_key);
        }
    }

    /// Add a text layer at `origin` (document space), following `path` if it
    /// isn't empty, and start editing it.
    pub fn new_text(&mut self, origin: Pos2, path: Vec<(f32, f32)>) {
        let spec = TextSpec { text: "Text".to_owned(), path, path_offset: 0.0, ..self.settings.text.clone() };
        let (data, index) = self.fonts.face(&spec.font, spec.bold, spec.italic);
        let Ok(font) = FontRef::try_from_slice_and_index(&data, index) else { return };
        match self.doc.try_add_text_layer(spec, (origin.x.round() as i32, origin.y.round() as i32), &font) {
            Ok(_) => self.text_focus = Some(true),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    /// Every outside mutation (file drops, AI completion, history insertion) uses
    /// this gate so a preview snapshot can never roll back an unrelated edit.
    pub fn mutation_busy(&self) -> bool {
        self.busy() || !matches!(self.drag, Drag::None) || self.prop.is_some()
            || self.layer_drag.is_some() || self.new_doc.is_some() || self.scale_dlg.is_some()
            || self.ai.modal_open() || self.error.is_some() || !self.recoveries.is_empty()
    }

    /// A filter dialog or on-canvas operation owns the image for now.
    pub fn busy(&self) -> bool {
        self.filter.is_some() || self.warp.is_some() || self.background_active()
    }

    fn open_filter(&mut self, mut filter: Filter) {
        if filter.needs_selection() && self.doc.state.selection.is_none() {
            return self.toast("Select what to remove first, then use this to fill it in from its surroundings");
        }
        if let Filter::MatchColors { mean, dev, .. } = &mut filter {
            // Start from another layer's colours if there is one.
            let other = self.doc.state.layers.iter().rev().find(|l| l.id != self.doc.state.active && l.visible);
            if let Some(stats) = other.and_then(|l| pf_core::fx::color_stats(&l.pixels)) {
                (*mean, *dev) = stats;
            }
        }
        match FilterOp::begin(&self.doc) {
            Some(op) => {
                self.filter = Some(FilterDlg { op, filter, before: self.doc.begin(), pending: true });
                self.schedule_filter();
            }
            None => self.toast("This layer is hidden or locked"),
        }
    }

    fn filter_dialog(&mut self, ctx: &Context) {
        let computing = self.background_active();
        let Some(d) = &mut self.filter else { return };
        let (mut apply, mut cancel, mut changed, mut select) = (false, false, false, false);
        egui::Window::new(d.filter.name())
            .collapsible(false)
            .resizable(false)
            .default_pos(self.view.vp.left_bottom() + vec2(16.0, -150.0))
            .show(ctx, |ui| {
                ui.set_width(250.0);
                ui.spacing_mut().slider_width = 170.0;
                ui.add_enabled_ui(!computing, |ui| {
                match &mut d.filter {
                    Filter::GaussianBlur { radius } => {
                        ui.label("Radius");
                        let s = egui::Slider::new(radius, 0.1..=250.0).logarithmic(true).suffix(" px").max_decimals(1);
                        changed |= ui.add(s).changed();
                    }
                    Filter::EdgeDetect(p) => {
                        ui.horizontal(|ui| {
                            for (s, label) in [(EdgeStyle::Lines, "Lines"), (EdgeStyle::LinesOnBlack, "On black"), (EdgeStyle::Highlight, "Highlight")] {
                                changed |= ui.selectable_value(&mut p.style, s, label).changed();
                            }
                            if p.style == EdgeStyle::Highlight {
                                let mut c = Color32::from_rgba_unmultiplied(p.color[0], p.color[1], p.color[2], p.color[3]);
                                if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                                    p.color = [c.r(), c.g(), c.b(), 255];
                                    changed = true;
                                }
                            }
                        });
                        ui.label("Threshold").on_hover_text("Lower finds more edges");
                        changed |= ui.add(egui::Slider::new(&mut p.threshold, 2.0..=200.0).logarithmic(true).max_decimals(0)).changed();
                        ui.label("Smoothing").on_hover_text("Higher ignores fine texture");
                        changed |= ui.add(egui::Slider::new(&mut p.smoothing, 0.0..=8.0).suffix(" px").max_decimals(1)).changed();
                        ui.label("Line width");
                        changed |= ui.add(egui::Slider::new(&mut p.thickness, 1.0..=15.0).suffix(" px").max_decimals(0)).changed();
                        if ui.button("Select Edges Instead").on_hover_text("Leave the pixels alone and select the detected edges").clicked() {
                            select = true;
                        }
                    }
                    Filter::UnsharpMask { radius, amount, threshold } => {
                        changed |= slider(ui, "Amount", amount, 0.0..=5.0, false, "", "");
                        changed |= slider(ui, "Radius", radius, 0.2..=50.0, true, " px", "");
                        changed |= slider(ui, "Threshold", threshold, 0.0..=50.0, false, "", "Ignore differences smaller than this, so noise isn't sharpened");
                    }
                    Filter::SurfaceBlur { radius, tolerance } => {
                        changed |= slider(ui, "Radius", radius, 0.5..=20.0, true, " px", "");
                        changed |= slider(ui, "Edge tolerance", tolerance, 2.0..=120.0, true, "", "Colour differences above this are kept as edges");
                    }
                    Filter::Denoise { strength } => {
                        changed |= slider(ui, "Strength", strength, 1.0..=80.0, true, "", "");
                    }
                    Filter::Inpaint { radius } => {
                        changed |= slider(ui, "Sample radius", radius, 2.0..=24.0, false, " px", "");
                        ui.label(egui::RichText::new("Fills the selection from the pixels around it. Best for small things on plain or softly textured backgrounds; use Send to AI for big or detailed areas.").small().weak());
                    }
                    Filter::AutoContrast { clip } => {
                        changed |= slider(ui, "Clip", clip, 0.0..=5.0, false, " %", "Share of the darkest and lightest pixels allowed to go pure black / white");
                    }
                    Filter::Equalize { amount } => {
                        changed |= slider(ui, "Amount", amount, 0.0..=1.0, false, "", "");
                    }
                    Filter::LocalContrast { clip, tiles } => {
                        changed |= slider(ui, "Strength", clip, 1.0..=8.0, false, "", "");
                        ui.label("Grid");
                        changed |= ui.add(egui::Slider::new(tiles, 2..=16)).on_hover_text("More tiles adapts to smaller areas").changed();
                    }
                    Filter::Threshold { level } => {
                        changed |= slider(ui, "Level", level, 1.0..=254.0, false, "", "");
                    }
                    Filter::AdaptiveThreshold { radius, offset } => {
                        changed |= slider(ui, "Neighbourhood", radius, 2.0..=100.0, true, " px", "");
                        changed |= slider(ui, "Offset", offset, -40.0..=40.0, false, "", "Higher turns more of the picture white");
                    }
                    Filter::MatchColors { mean, dev, amount } => {
                        ui.label("Take colors from");
                        let active = self.doc.state.active;
                        ui.horizontal_wrapped(|ui| {
                            for l in self.doc.state.layers.iter().rev().filter(|l| l.id != active) {
                                if ui.button(&l.name).clicked() {
                                    if let Some(s) = pf_core::fx::color_stats(&l.pixels) {
                                        (*mean, *dev) = s;
                                        changed = true;
                                    }
                                }
                            }
                            if ui.button("Image file…").clicked() {
                                let exts: Vec<&str> = io::OPEN_EXTENSIONS.iter().copied().filter(|e| *e != "ora").collect();
                                if let Some(s) = rfd::FileDialog::new().add_filter("Images", &exts).pick_file().and_then(|p| io::load_pixmap(&p).ok()).and_then(|px| pf_core::fx::color_stats(&px)) {
                                    (*mean, *dev) = s;
                                    changed = true;
                                }
                            }
                        });
                        if *dev == [0.0; 3] {
                            ui.label(egui::RichText::new("Pick a layer or an image whose colors this layer should take on.").small().weak());
                        }
                        changed |= slider(ui, "Amount", amount, 0.0..=1.0, false, "", "");
                    }
                    Filter::LensDistortion { amount } => {
                        changed |= slider(ui, "Amount", amount, -0.6..=0.6, false, "", "Right straightens lines that bulge outwards, left ones that bow inwards");
                    }
                }
                });
                let scope = if self.doc.state.selection.is_some() { "the selection on" } else { "all of" };
                let what = if self.doc.effective_target() == pf_core::Target::Mask { "mask" } else { "layer" };
                ui.label(egui::RichText::new(format!("Applies to {scope} the active {what}.")).small().weak());
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(vec2(250.0, 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    apply = ui.add_enabled(!computing && !changed && !d.pending, egui::Button::new("Apply")).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if !ctx.egui_wants_keyboard_input() {
            apply |= !computing && !changed && !d.pending && ctx.input(|i| i.key_pressed(Key::Enter));
            cancel |= ctx.input(|i| i.key_pressed(Key::Escape));
        }
        if select {
            let d = self.filter.take().unwrap();
            if let Filter::EdgeDetect(p) = d.filter {
                d.op.cancel(&mut self.doc);
                let state = d.before;
                let target = self.doc.target;
                self.start_work("Selecting detected edges…", move || {
                    let mut doc = Document::from_state(state);
                    doc.target = target;
                    let op = FilterOp::begin(&doc).ok_or("The layer cannot be filtered")?;
                    op.select_edges(&mut doc, &p);
                    let mask = doc.state.selection.as_deref().cloned()
                        .unwrap_or_else(|| pf_core::Mask::new(doc.state.width, doc.state.height));
                    Ok(crate::jobs::Outcome::Selection { mask, mode: pf_core::selection::Combine::Replace, label: "Select Edges" })
                });
            }
        } else if cancel {
            self.cancel_work();
            self.filter.take().unwrap().op.cancel(&mut self.doc);
        } else if apply {
            let d = self.filter.take().unwrap();
            d.op.finish(&mut self.doc, &d.filter);
        } else {
            d.pending |= changed;
            // Slow filters wait for the slider to be let go.
            if d.pending && (!d.filter.slow() || !ctx.input(|i| i.pointer.any_down())) {
                self.schedule_filter();
            }
        }
    }

    /// Ask before throwing away unsaved work. True means go ahead.
    fn confirm_discard(&mut self) -> bool {
        if !self.doc.modified {
            return true;
        }
        let answer = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Unsaved changes")
            .set_description(format!("Save the changes to {}?", self.doc_name()))
            .set_buttons(rfd::MessageButtons::YesNoCancelCustom("Save".into(), "Discard".into(), "Cancel".into()))
            .show();
        match answer {
            rfd::MessageDialogResult::Custom(label) if label == "Save" => self.save(false),
            rfd::MessageDialogResult::Custom(label) if label == "Discard" => true,
            _ => false,
        }
    }

    pub fn open_path(&mut self, path: &Path) {
        match io::open(path) {
            Ok(doc) => {
                self.set_doc(doc);
                self.opened = Some(path.to_owned());
                self.store.add_recent(path);
            }
            Err(e) => {
                self.error = Some(format!("Couldn't open {}: {e}", path.display()));
                if !path.exists() {
                    self.store.remove_recent(path);
                }
            }
        }
    }

    /// Open a file from the recent list, after the usual unsaved-changes check.
    pub fn open_recent(&mut self, path: &Path) {
        if matches!(self.drag, Drag::None) && !self.busy() && self.confirm_discard() {
            self.open_path(path);
        }
    }

    /// The file this image came from, if any (also set for flat images,
    /// which have no save path).
    pub fn source_file(&self) -> Option<&Path> {
        self.doc.path.as_deref().or(self.opened.as_deref())
    }

    /// Insert an image file so it fills the selection, cut to its shape.
    pub fn insert_in_selection(&mut self, path: &Path) {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Layer").to_owned();
        match io::load_pixmap(path) {
            Ok(px) => {
                if self.doc.add_image_in_selection(&name, &px).is_none() {
                    self.toast("The selection is empty or the inserted image would exceed the document size limits");
                }
            }
            Err(e) => self.toast(format!("Couldn't open {}: {e}", path.display())),
        }
    }

    fn add_image(&mut self, path: &Path) {
        match io::load_pixmap(path) {
            Ok(px) => {
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Layer").to_owned();
                if let Err(e) = self.doc.check_layer_capacity(px.w, px.h) {
                    return self.error = Some(e.to_string());
                }
                let (x, y) = ((self.doc.state.width as i32 - px.w as i32) / 2, (self.doc.state.height as i32 - px.h as i32) / 2);
                if let Err(e) = self.doc.try_insert_layer("Add Image", Layer::new(name, px, x, y)) {
                    self.error = Some(e.to_string());
                }
            }
            Err(e) => self.toast(format!("Couldn't open {}: {e}", path.display())),
        }
    }

    fn save(&mut self, save_as: bool) -> bool {
        let path = match (&self.doc.path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("OpenRaster layered image", &["ora"])
                .set_file_name(format!("{}.ora", self.doc_name()))
                .save_file()
                .map(|p| if io::is_ora(&p) { p } else { p.with_extension("ora") }),
        };
        let Some(path) = path else { return false };
        match io::save(&mut self.doc, &path) {
            Ok(()) => {
                self.store.add_recent(&path);
                if let Some(recovery) = &mut self.recovery { recovery.clear(); }
                if let Some(old) = self.recovered_source.take() { old.discard(); }
                self.toast(format!("Saved {}", path.display()));
                true
            }
            Err(e) => { self.error = Some(format!("Save failed: {e}")); false },
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
        // While a filter is being previewed only the view may change.
        let view = matches!(a, Action::ZoomIn | Action::ZoomOut | Action::ZoomFit | Action::Zoom100 | Action::ResetRotation);
        if self.mutation_busy() && !view {
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
            Action::InsertInSelection => {
                if self.doc.state.selection.is_none() {
                    return self.toast("Select the area the image should fill first");
                }
                let exts: Vec<&str> = io::OPEN_EXTENSIONS.iter().copied().filter(|e| *e != "ora").collect();
                if let Some(p) = rfd::FileDialog::new().add_filter("Images", &exts).pick_file() {
                    self.insert_in_selection(&p);
                }
            }
            Action::Save => { self.save(false); },
            Action::SaveAs => { self.save(true); },
            Action::Export => self.export(),
            Action::ExportCompatible => {
                if let Some(path) = rfd::FileDialog::new().add_filter("Compatible OpenRaster", &["ora"])
                    .set_file_name(format!("{}-compatible.ora", self.doc_name())).save_file() {
                    let path = path.with_extension("ora");
                    match io::ora::export_compatible(&self.doc.state, &path) {
                        Ok(()) => self.toast("Exported compatible layers with masks baked into transparency"),
                        Err(e) => self.error = Some(format!("Export failed: {e}")),
                    }
                }
            },
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
                    if let Err(e) = self.doc.try_insert_layer("Paste", Layer::new("Pasted Layer", c.pixels, c.x, c.y)) {
                        self.error = Some(e.to_string());
                    } else { self.tool = Tool::Move; }
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
                match self.doc.check_layer_capacity(self.doc.state.width, self.doc.state.height) {
                    Ok(()) => { self.doc.add_empty_layer(); }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Action::DuplicateLayer => {
                if let Some(layer) = self.doc.state.layer(active) {
                    let mut copy = layer.clone();
                    copy.id = pf_core::document::next_id();
                    copy.name = format!("{} copy", copy.name);
                    if let Err(e) = self.doc.try_insert_layer("Duplicate Layer", copy) { self.error = Some(e.to_string()); }
                }
            },
            Action::DeleteLayer => self.doc.delete_layer(active),
            Action::MergeDown => {
                if let Err(e) = self.doc.merge_down(active) { self.toast(e.to_string()); }
            },
            Action::Flatten => self.doc.flatten_image(),
            Action::AddMask => {
                if let Some(layer) = self.doc.state.layer(active) {
                    let used: u64 = self.doc.state.layers.iter().map(|l| l.pixels.w as u64 * l.pixels.h as u64 * if l.mask.is_some() { 2 } else { 1 }).sum();
                    if layer.mask.is_none() && used + layer.pixels.w as u64 * layer.pixels.h as u64 > io::limits::MAX_DOCUMENT_PIXELS {
                        self.error = Some("The mask would exceed the document pixel budget".to_owned());
                    } else { self.doc.add_mask(active); }
                }
            },
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
            Action::GaussianBlur => self.open_filter(Filter::GaussianBlur { radius: 8.0 }),
            Action::EdgeDetect => self.open_filter(Filter::EdgeDetect(EdgeParams::default())),
            Action::OpenFilter(f) => self.open_filter(f),
            Action::Perspective => self.open_perspective(),
            Action::ContentAwareScale => self.open_content_scale(),
            Action::SelectionToTextPath => self.selection_to_text_path(),
            Action::AiPrompt => self.open_ai_prompt(),
            Action::AiHistory => self.open_ai_history(),
            Action::Settings => self.open_settings(),
            Action::ClearRecent => self.store.clear_recent(),
        }
    }

    fn shortcuts(&mut self, ctx: &Context) {
        const CMD: Modifiers = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let cmd_alt = Modifiers::COMMAND | Modifiers::ALT;
        // A focused text field gets its own select-all, undo and so on.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
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
        if self.busy() {
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

    fn recovery_dialog(&mut self, ctx: &Context) {
        let Some(candidate) = self.recoveries.first() else { return };
        let name = candidate.original.as_ref().map_or_else(|| "Untitled image".to_owned(), |p| p.display().to_string());
        let (mut recover, mut discard) = (false, false);
        egui::Modal::new(egui::Id::new("recover-document")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Recover unsaved work");
            ui.label(format!("An earlier session stopped with unsaved changes to {name}."));
            ui.label("Recovery snapshots are saved privately every 30 seconds while the document is idle.");
            ui.horizontal(|ui| {
                recover = ui.button("Recover").clicked();
                discard = ui.button("Discard Recovery").clicked();
            });
        });
        if recover {
            if !self.confirm_discard() { return; }
            match self.recoveries[0].load() {
                Ok(doc) => {
                    self.set_doc(doc);
                    // Keep the original recovery until the recovered document is saved
                    // or discarded; a second crash during recovery must not lose it.
                    let previous = self.recoveries.remove(0);
                    if let Some(recovery) = &mut self.recovery {
                        recovery.tick(&self.doc, true, self.now);
                    }
                    self.recovered_source = Some(previous);
                    self.toast("Recovered unsaved work. Save the document to keep it.");
                }
                Err(e) => self.error = Some(format!("Couldn't recover the image: {e}")),
            }
        } else if discard {
            let previous = self.recoveries.remove(0);
            previous.discard();
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
                match io::limits::validate_dimensions(nd.w, nd.h) {
                    Ok(_) => self.set_doc(Document::new(nd.w, nd.h, (!nd.transparent).then_some([255; 4]))),
                    Err(e) => self.error = Some(e.to_string()),
                }
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
        self.poll_work(&ctx);
        self.schedule_perspective();

        if self.new_doc.is_none() && self.scale_dlg.is_none() && !self.ai.modal_open() {
            self.shortcuts(&ctx);
        }
        // Switching tool or layer ends the current run of text edits.
        let text_ctx = (self.tool, self.doc.state.active);
        if text_ctx != self.text_ctx {
            self.text_ctx = text_ctx;
            self.text_key = pf_core::document::next_id();
            self.text_repath = false;
        }

        // Queue drops until an active operation commits or cancels. Never put a
        // new layer into a document whose preview owns a rollback snapshot.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter()
            .map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() && self.mutation_busy() {
            self.toast("Image queued until the current edit finishes");
        }
        self.pending_drops.extend(dropped);
        if !self.mutation_busy() {
            for p in std::mem::take(&mut self.pending_drops) {
                if io::is_ora(&p) {
                    if self.confirm_discard() { self.open_path(&p); }
                } else { self.add_image(&p); }
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            // Pending gestures have already changed pixels but have not committed
            // their dirty/history flag. Keep the window until the edit resolves.
            if self.busy() || !matches!(self.drag, Drag::None) || self.prop.is_some() || self.layer_drag.is_some() {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.toast("Finish or cancel the current edit before closing");
            } else if !self.confirm_discard() {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            } else {
                if let Some(recovery) = &mut self.recovery { recovery.clear(); }
                if let Some(old) = self.recovered_source.take() { old.discard(); }
                return;
            }
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
        self.filter_dialog(&ctx);
        self.perspective_dialog(&ctx);
        self.content_scale_dialog(&ctx);
        self.work_status(&ctx);
        self.ai_dialogs(&ctx);
        self.recovery_dialog(&ctx);
        let stable = !self.mutation_busy();
        if let Some(recovery) = &mut self.recovery {
            if let Some(error) = recovery.poll() { self.error = Some(error); }
            recovery.tick(&self.doc, stable, self.now);
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if let Some(error) = self.error.clone() {
            egui::Modal::new(egui::Id::new("document-error")).show(&ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("The operation could not be completed");
                ui.label(error);
                if ui.button("OK").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.error = None;
                }
            });
        }

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
