//! Top bar, layers sidebar, tool strip and inspector.

use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use egui_phosphor::regular as icon;
use pf_core::fill::GradientShape;
use pf_core::filter::Filter;
use pf_core::ops::Reorder;
use pf_core::selection::Combine;
use pf_core::text::{Align as TextAlign, TextSpec};
use pf_core::{BlendMode, LayerId, Target};

use crate::app::{Action, App};
use crate::tools::{GradientFill, Tool};
use crate::view::{MAX_ZOOM, MIN_ZOOM};

const DIM: Color32 = Color32::from_rgb(150, 150, 156);
const ROW_H: f32 = 54.0;

fn icon_button(ui: &mut Ui, glyph: &str, tip: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(glyph).size(17.0)).frame(false)).on_hover_text(tip)
}

fn cmd(key: &str) -> String {
    if cfg!(target_os = "macos") { format!("\u{2318}{key}") } else { format!("Ctrl+{key}") }
}

fn item(app: &mut App, ui: &mut Ui, label: &str, shortcut: &str, a: Action) {
    let mut b = egui::Button::new(label);
    if !shortcut.is_empty() {
        b = b.shortcut_text(shortcut);
    }
    if ui.add(b).clicked() {
        let ctx = ui.ctx().clone();
        app.run(&ctx, a);
        ui.close();
    }
}

pub fn top_bar(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        if icon_button(ui, icon::SIDEBAR_SIMPLE, "Show or hide layers").clicked() {
            app.show_layers = !app.show_layers;
        }
        ui.add_space(4.0);
        ui.menu_button("File", |ui| {
            item(app, ui, "New…", &cmd("N"), Action::New);
            item(app, ui, "Open…", &cmd("O"), Action::Open);
            ui.menu_button("Open Recent", |ui| {
                let recent = app.store.recent();
                if recent.is_empty() {
                    ui.label(RichText::new("No recent files").color(DIM));
                }
                for f in &recent {
                    let name = f.path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_owned();
                    let dir = f.path.parent().map(|d| d.display().to_string()).unwrap_or_default();
                    if ui.button(name).on_hover_text(format!("{dir}\nOpened {}", f.opened.replace('T', " ").trim_end_matches('Z'))).clicked() {
                        app.open_recent(&f.path);
                        ui.close();
                    }
                }
                if !recent.is_empty() {
                    ui.separator();
                    item(app, ui, "Clear List", "", Action::ClearRecent);
                }
            });
            item(app, ui, "Insert Image as New Layer…", &cmd("⇧O"), Action::AddImage);
            item(app, ui, "Insert Image into Selection…", "", Action::InsertInSelection);
            ui.separator();
            item(app, ui, "Save", &cmd("S"), Action::Save);
            item(app, ui, "Save As…", &cmd("⇧S"), Action::SaveAs);
            item(app, ui, "Export PNG / JPEG / GIF…", &cmd("⇧E"), Action::Export);
            item(app, ui, "Export Compatible OpenRaster…", "", Action::ExportCompatible);
            ui.separator();
            item(app, ui, "Settings…", "", Action::Settings);
        });
        ui.menu_button("Edit", |ui| {
            item(app, ui, "Undo", &cmd("Z"), Action::Undo);
            item(app, ui, "Redo", &cmd("⇧Z"), Action::Redo);
            ui.separator();
            item(app, ui, "Cut", &cmd("X"), Action::Cut);
            item(app, ui, "Copy", &cmd("C"), Action::Copy);
            item(app, ui, "Paste as New Layer", &cmd("V"), Action::Paste);
            item(app, ui, "Clear", "⌫", Action::Clear);
            item(app, ui, "Fill with Foreground", "⌥⌫", Action::FillForeground);
            ui.separator();
            item(app, ui, "Crop to Selection", "", Action::Crop);
        });
        ui.menu_button("Layer", |ui| {
            item(app, ui, "New Layer", &cmd("⇧N"), Action::NewLayer);
            item(app, ui, "Duplicate", &cmd("J"), Action::DuplicateLayer);
            item(app, ui, "Delete", "", Action::DeleteLayer);
            ui.separator();
            item(app, ui, "Add Mask", "", Action::AddMask);
            item(app, ui, "Invert Mask", "", Action::InvertMask);
            item(app, ui, "Apply Mask", "", Action::ApplyMask);
            item(app, ui, "Delete Mask", "", Action::DeleteMask);
            ui.separator();
            item(app, ui, "Bring Forward", &cmd("]"), Action::Reorder(Reorder::Forward));
            item(app, ui, "Send Backward", &cmd("["), Action::Reorder(Reorder::Backward));
            item(app, ui, "Flip Horizontal", "", Action::FlipH);
            item(app, ui, "Flip Vertical", "", Action::FlipV);
            ui.separator();
            item(app, ui, "Merge Down", &cmd("E"), Action::MergeDown);
            item(app, ui, "Flatten Image", "", Action::Flatten);
            ui.separator();
            item(app, ui, "Send to AI with Prompt…", "", Action::AiPrompt);
            item(app, ui, "AI Requests…", "", Action::AiHistory);
        });
        ui.menu_button("Select", |ui| {
            item(app, ui, "All", &cmd("A"), Action::SelectAll);
            item(app, ui, "Deselect", &cmd("D"), Action::Deselect);
            item(app, ui, "Invert", &cmd("⇧I"), Action::InvertSelection);
            ui.separator();
            item(app, ui, "Selection Outline to Text Path", "", Action::SelectionToTextPath);
        });
        ui.menu_button("Filter", |ui| {
            ui.menu_button("Blur & Sharpen", |ui| {
                item(app, ui, "Gaussian Blur…", "", Action::GaussianBlur);
                item(app, ui, "Surface Blur…", "", Action::OpenFilter(Filter::SurfaceBlur { radius: 4.0, tolerance: 25.0 }));
                item(app, ui, "Sharpen…", "", Action::OpenFilter(Filter::UnsharpMask { radius: 1.5, amount: 1.0, threshold: 0.0 }));
            });
            ui.menu_button("Repair", |ui| {
                item(app, ui, "Heal Selection…", "", Action::OpenFilter(Filter::Inpaint { radius: 6.0 }));
                item(app, ui, "Reduce Noise…", "", Action::OpenFilter(Filter::Denoise { strength: 12.0 }));
            });
            ui.menu_button("Tone & Color", |ui| {
                item(app, ui, "Auto Contrast…", "", Action::OpenFilter(Filter::AutoContrast { clip: 0.5 }));
                item(app, ui, "Equalize…", "", Action::OpenFilter(Filter::Equalize { amount: 1.0 }));
                item(app, ui, "Local Contrast…", "", Action::OpenFilter(Filter::LocalContrast { clip: 2.5, tiles: 8 }));
                ui.separator();
                item(app, ui, "Threshold…", "", Action::OpenFilter(Filter::Threshold { level: 128.0 }));
                item(app, ui, "Adaptive Threshold…", "", Action::OpenFilter(Filter::AdaptiveThreshold { radius: 20.0, offset: 6.0 }));
                ui.separator();
                item(app, ui, "Match Colors…", "", Action::OpenFilter(Filter::MatchColors { mean: [0.0; 3], dev: [0.0; 3], amount: 1.0 }));
            });
            ui.menu_button("Geometry", |ui| {
                item(app, ui, "Perspective…", "", Action::Perspective);
                item(app, ui, "Lens Distortion…", "", Action::OpenFilter(Filter::LensDistortion { amount: 0.15 }));
                item(app, ui, "Content-Aware Scale…", "", Action::ContentAwareScale);
            });
            ui.menu_button("Stylize", |ui| {
                item(app, ui, "Edge Detection…", "", Action::EdgeDetect);
            });
        });
        ui.menu_button("View", |ui| {
            item(app, ui, "Zoom In", &cmd("+"), Action::ZoomIn);
            item(app, ui, "Zoom Out", &cmd("−"), Action::ZoomOut);
            item(app, ui, "Zoom to Fit", &cmd("0"), Action::ZoomFit);
            item(app, ui, "Actual Size", &cmd("1"), Action::Zoom100);
            item(app, ui, "Reset Rotation", &cmd("⌥0"), Action::ResetRotation);
        });
        ui.separator();

        // Zoom control.
        let ctx = ui.ctx().clone();
        if icon_button(ui, icon::MINUS, "Zoom out").clicked() {
            app.run(&ctx, Action::ZoomOut);
        }
        let mut z = app.view.zoom;
        ui.spacing_mut().slider_width = 110.0;
        if ui.add(egui::Slider::new(&mut z, MIN_ZOOM..=MAX_ZOOM).logarithmic(true).show_value(false)).changed() {
            app.view.set_zoom(z);
        }
        if icon_button(ui, icon::PLUS, "Zoom in").clicked() {
            app.run(&ctx, Action::ZoomIn);
        }
        let pct = format!("{:.0}%", app.view.zoom * 100.0);
        if ui.add(egui::Button::new(RichText::new(pct).color(DIM)).frame(false)).on_hover_text("Zoom to fit").clicked() {
            app.run(&ctx, Action::ZoomFit);
        }
        if app.view.rot != 0.0 {
            let deg = format!("{} {:.0}°", icon::COMPASS, app.view.rot.to_degrees());
            if ui.add(egui::Button::new(RichText::new(deg).color(DIM)).frame(false)).on_hover_text("Reset rotation").clicked() {
                app.run(&ctx, Action::ResetRotation);
            }
        }
        ui.add_space(10.0);
        ui.label(RichText::new(app.doc_name()).strong().size(15.0).color(Color32::WHITE));
        if app.doc.modified {
            ui.label(RichText::new("Edited").color(DIM));
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, icon::SLIDERS_HORIZONTAL, "Show or hide tool options").clicked() {
                app.show_inspector = !app.show_inspector;
            }
            if icon_button(ui, icon::EXPORT, "Export PNG / JPEG / GIF").clicked() {
                app.run(&ctx, Action::Export);
            }
            if icon_button(ui, icon::FLOPPY_DISK, "Save").clicked() {
                app.run(&ctx, Action::Save);
            }
            if icon_button(ui, icon::FOLDER_OPEN, "Open").clicked() {
                app.run(&ctx, Action::Open);
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(app.doc.can_redo(), |ui| {
                if icon_button(ui, icon::ARROW_CLOCKWISE, "Redo").clicked() {
                    app.run(&ctx, Action::Redo);
                }
            });
            ui.add_enabled_ui(app.doc.can_undo(), |ui| {
                if icon_button(ui, icon::ARROW_COUNTER_CLOCKWISE, "Undo").clicked() {
                    app.run(&ctx, Action::Undo);
                }
            });
        });
    });
}

pub fn tool_strip(app: &mut App, ui: &mut Ui) {
    if app.busy() {
        ui.disable();
    }
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        for t in Tool::STRIP {
            let Some(t) = t else {
                ui.add_space(10.0);
                continue;
            };
            let on = app.tool == t;
            let text = RichText::new(t.icon()).size(19.0).color(if on { Color32::WHITE } else { Color32::from_gray(190) });
            let b = egui::Button::new(text).selected(on).frame(on).min_size(vec2(32.0, 30.0));
            if ui.add(b).on_hover_text(format!("{} ({})", t.name(), t.key().name())).clicked() {
                app.tool = t;
            }
        }
    });
}

fn percent(ui: &mut Ui, label: &str, v: &mut f32) -> egui::Response {
    ui.label(RichText::new(label).color(DIM));
    ui.add(
        egui::Slider::new(v, 0.0..=1.0)
            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
            .custom_parser(|s| s.trim_end_matches('%').trim().parse::<f64>().ok().map(|v| v / 100.0)),
    )
}

fn size_slider(ui: &mut Ui, v: &mut f32, max: f32) {
    ui.label(RichText::new("Size").color(DIM));
    ui.add(egui::Slider::new(v, 1.0..=max).logarithmic(true).suffix(" px").fixed_decimals(0));
}

fn c32(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn colors(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().interact_size = vec2(44.0, 26.0);
        for (c, tip) in [(&mut app.settings.fg, "Foreground color"), (&mut app.settings.bg, "Background color")] {
            let mut col = c32(*c);
            if egui::color_picker::color_edit_button_srgba(ui, &mut col, egui::color_picker::Alpha::Opaque).on_hover_text(tip).changed() {
                *c = [col.r(), col.g(), col.b(), 255];
            }
        }
        if icon_button(ui, icon::SWAP, "Swap colors (X)").clicked() {
            std::mem::swap(&mut app.settings.fg, &mut app.settings.bg);
        }
        if icon_button(ui, icon::CIRCLE_HALF, "Black and white (D)").clicked() {
            (app.settings.fg, app.settings.bg) = ([0, 0, 0, 255], [255; 4]);
        }
    });
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.separator();
    ui.label(RichText::new(title).strong());
}

fn wide_button(ui: &mut Ui, text: &str, w: f32) -> egui::Response {
    ui.add_sized([w, 22.0], egui::Button::new(text))
}

/// Options for the type tool. They edit the active text layer, or the style
/// for the next new text when another kind of layer is active.
fn text_options(app: &mut App, ui: &mut Ui, full: f32) {
    let active = app.doc.state.active_layer().and_then(|l| Some((l.id, (**l.text.as_ref()?).clone())));
    let orig = active.as_ref().map_or_else(|| app.settings.text.clone(), |(_, s)| s.clone());
    let mut spec = orig.clone();

    if active.is_some() {
        let out = egui::TextEdit::multiline(&mut spec.text).desired_rows(3).desired_width(full).hint_text("Type here").show(ui);
        if let Some(select_all) = app.text_focus.take() {
            out.response.request_focus();
            if select_all {
                let mut state = out.state;
                let end = egui::text::CCursor::new(spec.text.chars().count());
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
                state.store(ui.ctx(), out.response.id);
            }
        }
    } else {
        app.text_focus = None;
        ui.label(RichText::new("Click the image to add text, or drag to draw a path for the text to follow.").color(DIM));
    }

    section(ui, "Font");
    let shown = if spec.font.is_empty() { "Built-in (Ubuntu Light)" } else { spec.font.as_str() }.to_owned();
    egui::ComboBox::from_id_salt("font").selected_text(shown).width(full).height(360.0).show_ui(ui, |ui| {
        if ui.selectable_label(spec.font.is_empty(), "Built-in (Ubuntu Light)").clicked() {
            spec.font.clear();
        }
        let n = app.fonts.families().len();
        let row = ui.text_style_height(&egui::TextStyle::Button) + ui.spacing().button_padding.y * 2.0;
        let (first, last) = {
            // Only lay out the rows in view: there can be hundreds of fonts.
            let top = ui.clip_rect().top() - ui.cursor().top();
            let step = row + ui.spacing().item_spacing.y;
            let first = ((top / step).floor().max(0.0) as usize).min(n);
            (first, (first + (ui.clip_rect().height() / step) as usize + 2).min(n))
        };
        let step = row + ui.spacing().item_spacing.y;
        ui.add_space(first as f32 * step);
        for i in first..last {
            let name = &app.fonts.families()[i];
            if ui.selectable_label(spec.font == *name, name).clicked() {
                spec.font = name.clone();
            }
        }
        ui.add_space((n - last) as f32 * step);
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let toggle = |ui: &mut Ui, on: &mut bool, glyph: &str, tip: &str| {
            let b = egui::Button::new(RichText::new(glyph).size(16.0)).selected(*on);
            if ui.add_sized([30.0, 26.0], b).on_hover_text(tip).clicked() {
                *on = !*on;
            }
        };
        toggle(ui, &mut spec.bold, icon::TEXT_B, "Bold");
        toggle(ui, &mut spec.italic, icon::TEXT_ITALIC, "Italic");
        ui.add_space(8.0);
        for (a, glyph, tip) in [
            (TextAlign::Left, icon::TEXT_ALIGN_LEFT, "Align left"),
            (TextAlign::Center, icon::TEXT_ALIGN_CENTER, "Align center"),
            (TextAlign::Right, icon::TEXT_ALIGN_RIGHT, "Align right"),
        ] {
            let b = egui::Button::new(RichText::new(glyph).size(16.0)).selected(spec.align == a);
            if ui.add_sized([30.0, 26.0], b).on_hover_text(tip).clicked() {
                spec.align = a;
            }
        }
        ui.add_space(8.0);
        let mut col = c32(spec.color);
        if egui::color_picker::color_edit_button_srgba(ui, &mut col, egui::color_picker::Alpha::OnlyBlend).on_hover_text("Text color").changed() {
            spec.color = col.to_srgba_unmultiplied();
        }
    });
    size_slider(ui, &mut spec.size, 1000.0);
    ui.label(RichText::new("Letter spacing").color(DIM));
    ui.add(egui::Slider::new(&mut spec.tracking, -20.0..=200.0).suffix(" px").max_decimals(1));
    if spec.path.len() < 2 {
        ui.label(RichText::new("Line spacing").color(DIM));
        ui.add(egui::Slider::new(&mut spec.leading, 0.5..=3.0).max_decimals(2));
    }

    if active.is_some() {
        section(ui, "Path");
        if spec.path.len() >= 2 {
            let len: f32 = spec.path.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum();
            ui.label(RichText::new("Position along path").color(DIM));
            ui.add(egui::Slider::new(&mut spec.path_offset, -len..=len).suffix(" px").max_decimals(0));
        }
        let label = if spec.path.len() >= 2 { "Redraw Path" } else { "Draw a Path to Follow" };
        if ui.add_sized([full, 22.0], egui::Button::new(label).selected(app.text_repath)).clicked() {
            app.text_repath = !app.text_repath;
        }
        if app.text_repath {
            ui.label(RichText::new("Now drag across the image along the line the text should follow.").color(DIM).small());
        }
        if spec.path.len() >= 2 && wide_button(ui, "Straighten", full).clicked() {
            spec.path.clear();
            spec.path_offset = 0.0;
        }
    }

    if spec != orig {
        app.settings.text = TextSpec { text: String::new(), path: Vec::new(), path_offset: 0.0, raster: (0, 0), ..spec.clone() };
        if let Some((id, _)) = active {
            app.apply_text(id, spec);
        }
    }
}

pub fn inspector(app: &mut App, ui: &mut Ui) {
    if app.busy() {
        ui.disable();
    }
    let ctx = ui.ctx().clone();
    let snap = app.doc.begin();
    let tool = app.tool;
    ui.label(RichText::new(tool.name()).strong().size(15.0).color(Color32::WHITE));
    ui.add_space(2.0);
    ui.separator();
    let full = ui.available_width();
    let half = (full - 8.0) / 2.0;
    ui.spacing_mut().slider_width = full - 62.0;

    if !matches!(tool, Tool::Move | Tool::Hand | Tool::Zoom | Tool::Text) && !tool.is_selection() {
        colors(app, ui);
        if app.doc.effective_target() == Target::Mask && tool != Tool::Eyedropper {
            ui.label(RichText::new("Editing the layer mask: dark hides, light reveals.").color(DIM).small());
        }
        ui.add_space(4.0);
    }

    match tool {
        Tool::Move => {
            ui.horizontal(|ui| {
                for (glyph, label, r) in [
                    (icon::ARROW_LINE_DOWN, "Back", Reorder::Back),
                    (icon::ARROW_LINE_UP, "Front", Reorder::Front),
                    (icon::ARROW_DOWN, "Backward", Reorder::Backward),
                    (icon::ARROW_UP, "Forward", Reorder::Forward),
                ] {
                    ui.vertical(|ui| {
                        ui.set_width((full - 24.0) / 4.0);
                        ui.vertical_centered(|ui| {
                            if ui.add_sized([ui.available_width(), 22.0], egui::Button::new(glyph)).clicked() {
                                app.run(&ctx, Action::Reorder(r));
                            }
                            ui.label(RichText::new(label).small().color(DIM));
                        });
                    });
                }
            });
            if let Some(l) = app.doc.state.active_layer_mut() {
                section(ui, "Size");
                ui.label(format!("W: {} px     H: {} px", l.pixels.w, l.pixels.h));
                section(ui, "Position");
                let mut moved = None;
                ui.horizontal(|ui| {
                    ui.label("X:");
                    let rx = ui.add_enabled(!l.locked, egui::DragValue::new(&mut l.x).range(-pf_core::io::limits::MAX_LAYER_OFFSET..=pf_core::io::limits::MAX_LAYER_OFFSET).suffix(" px"));
                    ui.label("Y:");
                    let ry = ui.add_enabled(!l.locked, egui::DragValue::new(&mut l.y).range(-pf_core::io::limits::MAX_LAYER_OFFSET..=pf_core::io::limits::MAX_LAYER_OFFSET).suffix(" px"));
                    moved = Some((rx, ry));
                });
                let (mut locked, mut visible) = (l.locked, l.visible);
                if let Some((rx, ry)) = moved {
                    app.track(&rx, &snap, "Move Layer");
                    app.track(&ry, &snap, "Move Layer");
                }
                section(ui, "Flip");
                ui.horizontal(|ui| {
                    if wide_button(ui, &format!("{}  Horizontal", icon::FLIP_HORIZONTAL), half).clicked() {
                        app.run(&ctx, Action::FlipH);
                    }
                    if wide_button(ui, &format!("{}  Vertical", icon::FLIP_VERTICAL), half).clicked() {
                        app.run(&ctx, Action::FlipV);
                    }
                });
                ui.add_space(6.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.add_sized([half, 22.0], egui::Button::new(if locked { "Unlock" } else { "Lock" })).clicked() {
                        locked = !locked;
                    }
                    if ui.add_sized([half, 22.0], egui::Button::new(if visible { "Hide" } else { "Show" })).clicked() {
                        visible = !visible;
                    }
                });
                if let Some(l) = app.doc.state.active_layer_mut() {
                    if (l.locked, l.visible) != (locked, visible) {
                        (l.locked, l.visible) = (locked, visible);
                        app.doc.commit("Layer Properties", snap.clone());
                    }
                }
                ui.add_space(6.0);
                ui.separator();
                if wide_button(ui, "Merge Down", full).clicked() {
                    app.run(&ctx, Action::MergeDown);
                }
            }
            ui.add_space(6.0);
            ui.separator();
            ui.checkbox(&mut app.settings.auto_select, "Auto Select layer under pointer");
        }
        Tool::Brush | Tool::Pencil | Tool::Eraser | Tool::Smudge | Tool::Clone => {
            let p = app.settings.params_mut(tool).unwrap();
            size_slider(ui, &mut p.size, 1000.0);
            if tool != Tool::Pencil {
                let mut soft = 1.0 - p.hardness;
                if percent(ui, "Softness", &mut soft).changed() {
                    p.hardness = 1.0 - soft;
                }
            }
            if tool != Tool::Smudge {
                percent(ui, "Opacity", &mut p.opacity);
            }
            if matches!(tool, Tool::Brush | Tool::Eraser | Tool::Clone) {
                percent(ui, "Flow", &mut p.flow);
            }
            if tool == Tool::Smudge {
                percent(ui, "Strength", &mut app.settings.smudge_strength);
            }
            if tool == Tool::Clone {
                section(ui, "Source");
                let alt = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
                let msg = match (app.clone_src, app.clone_off) {
                    (None, _) => format!("{alt}-click the image to choose where to clone from."),
                    (Some(_), None) => "Source set. Paint to clone.".to_owned(),
                    (Some(_), Some(_)) => format!("Cloning. {alt}-click to pick a new source."),
                };
                ui.label(RichText::new(msg).color(DIM));
            }
            ui.add_space(8.0);
            ui.label(RichText::new("[ and ] change the size.").color(DIM).small());
        }
        Tool::Gradient => {
            let s = &mut app.settings;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.gradient_shape, GradientShape::Linear, "Linear");
                ui.selectable_value(&mut s.gradient_shape, GradientShape::Radial, "Radial");
            });
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.gradient_fill, GradientFill::ForegroundToBackground, "Color to color");
                ui.selectable_value(&mut s.gradient_fill, GradientFill::ForegroundToTransparent, "To transparent");
            });
            percent(ui, "Opacity", &mut s.gradient_opacity);
            ui.add_space(8.0);
            ui.label(RichText::new("Drag on the image to draw the gradient.").color(DIM).small());
        }
        Tool::SubjectSelect => {
            ui.label(RichText::new("Drag a box around a subject. Everything outside the box is treated as background, and the subject inside is separated from it by color and edges.").color(DIM));
            ui.add_space(4.0);
            ui.checkbox(&mut app.settings.sample_all_layers, "Look at all layers");
        }
        Tool::RegionSelect => {
            ui.label(RichText::new("Click an area to select it up to its edges; drag to sweep up several.").color(DIM));
            ui.add_space(4.0);
            percent(ui, "Detail", &mut app.settings.region_detail).on_hover_text("Higher splits the picture into smaller regions");
            ui.checkbox(&mut app.settings.sample_all_layers, "Look at all layers");
        }
        Tool::Bucket | Tool::MagicWand | Tool::QuickSelect => {
            let s = &mut app.settings;
            if tool == Tool::QuickSelect {
                size_slider(ui, &mut s.quick_size, 500.0);
            }
            ui.label(RichText::new("Tolerance").color(DIM));
            ui.add(egui::Slider::new(&mut s.tolerance, 0..=255));
            if tool == Tool::Bucket {
                percent(ui, "Opacity", &mut s.fill_opacity);
            }
            if tool != Tool::QuickSelect {
                ui.checkbox(&mut s.contiguous, "Contiguous");
            }
            ui.checkbox(&mut s.sample_all_layers, "Sample all layers");
        }
        Tool::Eyedropper => {
            ui.label(RichText::new("Click the image to pick the foreground color.").color(DIM));
        }
        Tool::Text => text_options(app, ui, full),
        Tool::Hand => {
            ui.label(RichText::new("Drag to pan. Hold Space with any tool to pan temporarily.").color(DIM));
        }
        Tool::Zoom => {
            ui.label(RichText::new("Click to zoom in, Option/Alt-click to zoom out, or drag sideways.").color(DIM));
        }
        _ => {}
    }

    if tool.is_selection() {
        section(ui, "Mode");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let w = (full - 12.0) / 4.0;
            let modes = [
                (Combine::Replace, icon::SQUARE, "New selection"),
                (Combine::Add, icon::UNITE, "Add to selection (Shift)"),
                (Combine::Subtract, icon::SUBTRACT, "Subtract from selection (Alt/Option)"),
                (Combine::Intersect, icon::INTERSECT, "Intersect with selection (Shift+Alt/Option)"),
            ];
            for (m, glyph, tip) in modes {
                let on = app.settings.sel_mode == m;
                let b = egui::Button::new(RichText::new(glyph).size(16.0)).selected(on);
                if ui.add_sized([w, 26.0], b).on_hover_text(tip).clicked() {
                    app.settings.sel_mode = m;
                }
            }
        });
        section(ui, "Selection");
        ui.horizontal(|ui| {
            if wide_button(ui, "All", half).clicked() {
                app.run(&ctx, Action::SelectAll);
            }
            if wide_button(ui, "Deselect", half).clicked() {
                app.run(&ctx, Action::Deselect);
            }
        });
        ui.horizontal(|ui| {
            if wide_button(ui, "Invert", half).clicked() {
                app.run(&ctx, Action::InvertSelection);
            }
            if wide_button(ui, "Crop to", half).clicked() {
                app.run(&ctx, Action::Crop);
            }
        });
        if wide_button(ui, "Mask Layer with Selection", full).clicked() {
            app.run(&ctx, Action::AddMask);
        }
        ui.add_space(8.0);
        let alt = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
        ui.label(RichText::new(format!("Hold Shift to add, {alt} to subtract, both to intersect, whatever the mode.")).color(DIM).small());
    }

    if matches!(tool, Tool::Hand | Tool::Zoom) {
        section(ui, "View");
        ui.horizontal(|ui| {
            if wide_button(ui, "Fit", half).clicked() {
                app.run(&ctx, Action::ZoomFit);
            }
            if wide_button(ui, "100%", half).clicked() {
                app.run(&ctx, Action::Zoom100);
            }
        });
        if wide_button(ui, "Reset Rotation", full).clicked() {
            app.run(&ctx, Action::ResetRotation);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Pinch to zoom, two-finger rotate, scroll to pan. Ctrl-scroll zooms and Alt-scroll rotates with a mouse.").color(DIM).small());
    }
}

enum RowAct {
    Select(LayerId, Target),
    Visible(LayerId, bool),
    ToggleMask(LayerId),
    Rename(LayerId, String),
    Menu(LayerId, Action),
    Drop(LayerId, usize),
}

pub fn layers(app: &mut App, ui: &mut Ui) {
    if app.busy() {
        ui.disable();
    }
    let ctx = ui.ctx().clone();
    let snap = app.doc.begin();

    egui::Frame::new().inner_margin(egui::Margin { left: 10, right: 8, top: 10, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Layers").strong().size(15.0).color(Color32::WHITE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, icon::PLUS, "New layer").clicked() {
                    app.run(&ctx, Action::NewLayer);
                }
                if icon_button(ui, icon::TRASH, "Delete layer").clicked() {
                    app.run(&ctx, Action::DeleteLayer);
                }
                if icon_button(ui, icon::COPY, "Duplicate layer").clicked() {
                    app.run(&ctx, Action::DuplicateLayer);
                }
                if icon_button(ui, icon::CIRCLE_HALF, "Add mask (from the selection, if any)").clicked() {
                    app.run(&ctx, Action::AddMask);
                }
            });
        });
        ui.add_space(4.0);
        if let Some(l) = app.doc.state.active_layer_mut() {
            let mut blend = l.blend;
            let mut opacity = l.opacity;
            let mut resp = None;
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("blend").width(104.0).selected_text(blend.name()).show_ui(ui, |ui| {
                    for m in BlendMode::ALL {
                        ui.selectable_value(&mut blend, m, m.name());
                    }
                });
                ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
                resp = Some(
                    ui.add(
                        egui::Slider::new(&mut opacity, 0.0..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                            .custom_parser(|s| s.trim_end_matches('%').trim().parse::<f64>().ok().map(|v| v / 100.0)),
                    )
                    .on_hover_text("Layer opacity"),
                );
            });
            l.opacity = opacity;
            if blend != l.blend {
                l.blend = blend;
                app.doc.commit("Blend Mode", snap.clone());
            }
            if let Some(r) = resp {
                app.track(&r, &snap, "Layer Opacity");
            }
        }
    });
    ui.separator();

    let mut acts: Vec<RowAct> = Vec::new();
    let n = snap.layers.len();
    let target = app.doc.effective_target();
    let hist_h = 150.0;
    egui::ScrollArea::vertical().max_height((ui.available_height() - hist_h).max(ROW_H)).auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let top = ui.cursor().top();
        let width = ui.available_width();
        for (disp, layer) in snap.layers.iter().rev().enumerate() {
            let id = layer.id;
            let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click_and_drag());
            let active = id == snap.active;
            let p = ui.painter().clone();
            if active {
                p.rect_filled(rect.shrink2(vec2(5.0, 1.0)), 7.0, Color32::from_rgb(58, 60, 68));
            } else if resp.hovered() {
                p.rect_filled(rect.shrink2(vec2(5.0, 1.0)), 7.0, Color32::from_rgb(46, 46, 50));
            }

            // Thumbnails: layer pixels, then the mask if there is one.
            let (pix_tex, mask_tex) = {
                let t = app.thumb(&ctx, layer);
                ((t.pixels.id(), t.pixels.size_vec2()), t.mask.as_ref().map(|m| m.id()))
            };
            let slot = |i: f32| Rect::from_min_size(pos2(rect.left() + 14.0 + i * 46.0, rect.top() + 7.0), Vec2::splat(40.0));
            let fit = |slot: Rect, size: Vec2| Rect::from_center_size(slot.center(), size * (40.0 / size.x.max(size.y)));
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let ring = |r: Rect, on: bool| {
                let s = if on { Stroke::new(2.0, Color32::from_rgb(64, 140, 255)) } else { Stroke::new(1.0, Color32::from_black_alpha(120)) };
                p.rect_stroke(r.expand(1.0), 3.0, s, StrokeKind::Outside);
            };
            let pr = fit(slot(0.0), pix_tex.1);
            p.image(pix_tex.0, pr, uv, if layer.visible { Color32::WHITE } else { Color32::from_gray(90) });
            ring(pr, active && layer.mask.is_some() && target == Target::Pixels);
            let mut text_x = slot(1.0).left() + 2.0;
            if let Some(mt) = mask_tex {
                let mr = fit(slot(1.0), pix_tex.1);
                p.image(mt, mr, uv, if layer.mask_enabled { Color32::WHITE } else { Color32::from_gray(90) });
                ring(mr, active && target == Target::Mask);
                if !layer.mask_enabled {
                    p.line_segment([mr.left_top(), mr.right_bottom()], Stroke::new(2.0, Color32::from_rgb(230, 70, 70)));
                }
                text_x += 46.0;
                let m = ui.interact(mr, ui.id().with(("mask", id)), Sense::click());
                if m.clicked() {
                    acts.push(if ui.input(|i| i.modifiers.shift) { RowAct::ToggleMask(id) } else { RowAct::Select(id, Target::Mask) });
                }
                m.on_hover_text("Layer mask. Click to paint on it, Shift-click to disable.");
            }

            // Name (double-click to rename) and size.
            let name_rect = Rect::from_min_max(pos2(text_x, rect.top() + 8.0), pos2(rect.right() - 36.0, rect.top() + 28.0));
            let mut renaming = false;
            if let Some((rid, buf, focused)) = &mut app.rename {
                if *rid == id {
                    renaming = true;
                    let te = ui.put(name_rect, egui::TextEdit::singleline(buf));
                    if !*focused {
                        te.request_focus();
                        *focused = true;
                    } else if te.lost_focus() {
                        acts.push(RowAct::Rename(id, buf.clone()));
                    }
                }
            }
            if !renaming {
                let col = if layer.visible { Color32::from_gray(235) } else { DIM };
                let galley = p.layout_no_wrap(layer.name.clone(), FontId::proportional(13.5), col);
                p.with_clip_rect(name_rect.expand2(vec2(0.0, 4.0))).galley(name_rect.left_top(), galley, col);
            }
            let lock = if layer.locked { format!("  {}", icon::LOCK_SIMPLE) } else { String::new() };
            p.text(
                pos2(text_x, rect.top() + 30.0),
                Align2::LEFT_TOP,
                if layer.text.is_some() { format!("{}  Text{lock}", icon::TEXT_T) } else { format!("{} × {} px{lock}", layer.pixels.w, layer.pixels.h) },
                FontId::proportional(11.0),
                DIM,
            );

            // Visibility.
            let mut vis = layer.visible;
            let cb = Rect::from_center_size(pos2(rect.right() - 22.0, rect.center().y), Vec2::splat(18.0));
            if ui.put(cb, egui::Checkbox::without_text(&mut vis)).on_hover_text("Show or hide layer").changed() {
                acts.push(RowAct::Visible(id, vis));
            }

            if resp.double_clicked() {
                app.rename = Some((id, layer.name.clone(), false));
            } else if resp.clicked() || resp.drag_started() {
                acts.push(RowAct::Select(id, Target::Pixels));
            }
            // Drag to reorder: show where the layer would land.
            if resp.dragged() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let slot = (((pos.y - top) / ROW_H).round() as usize).min(n);
                    app.layer_drag = Some((id, slot));
                    let y = top + slot as f32 * ROW_H;
                    ui.painter().hline(rect.x_range().shrink(8.0), y, Stroke::new(2.0, Color32::from_rgb(64, 140, 255)));
                }
            }
            if resp.drag_stopped() {
                if let Some((did, slot)) = app.layer_drag.take() {
                    let new_disp = if slot > disp { slot - 1 } else { slot };
                    acts.push(RowAct::Drop(did, n - 1 - new_disp.min(n - 1)));
                }
            }
            // Widgets placed inside the row moved the layout cursor; put it
            // back at the row's bottom edge so rows don't overlap.
            let short = rect.bottom() - ui.cursor().top();
            if short > 0.0 {
                ui.add_space(short);
            }
            resp.context_menu(|ui| {
                let mut m = |ui: &mut Ui, label: &str, a: Action| {
                    if ui.button(label).clicked() {
                        acts.push(RowAct::Menu(id, a));
                        ui.close();
                    }
                };
                m(ui, "Duplicate", Action::DuplicateLayer);
                m(ui, "Delete", Action::DeleteLayer);
                m(ui, "Merge Down", Action::MergeDown);
                ui.separator();
                if layer.mask.is_some() {
                    m(ui, "Invert Mask", Action::InvertMask);
                    m(ui, "Apply Mask", Action::ApplyMask);
                    m(ui, "Delete Mask", Action::DeleteMask);
                } else {
                    m(ui, "Add Mask", Action::AddMask);
                }
                ui.separator();
                m(ui, "Bring to Front", Action::Reorder(Reorder::Front));
                m(ui, "Send to Back", Action::Reorder(Reorder::Back));
                ui.separator();
                m(ui, "Send to AI with Prompt…", Action::AiPrompt);
            });
        }
    });

    for a in acts {
        match a {
            RowAct::Select(id, t) => {
                app.doc.state.active = id;
                app.doc.target = t;
            }
            RowAct::Visible(id, v) => {
                if let Some(l) = app.doc.state.layer_mut(id) {
                    l.visible = v;
                    app.doc.commit(if v { "Show Layer" } else { "Hide Layer" }, snap.clone());
                }
            }
            RowAct::ToggleMask(id) => {
                if let Some(l) = app.doc.state.layer_mut(id) {
                    l.mask_enabled = !l.mask_enabled;
                    app.doc.commit("Toggle Mask", snap.clone());
                }
            }
            RowAct::Rename(id, name) => {
                app.doc.rename_layer(id, &name);
                app.rename = None;
            }
            RowAct::Menu(id, a) => {
                app.doc.state.active = id;
                app.run(&ctx, a);
            }
            RowAct::Drop(id, to) => app.doc.move_layer_to(id, to),
        }
    }

    // History.
    ui.separator();
    egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 2)).show(ui, |ui| {
        ui.label(RichText::new(format!("{}  History", icon::CLOCK_COUNTER_CLOCKWISE)).strong());
        ui.label(RichText::new(format!("{:.1} MiB retained", app.doc.history_bytes() as f64 / (1024.0 * 1024.0))).small().color(DIM))
            .on_hover_text("Undo retains at most 200 steps and 1 GiB of extra raster data. Older steps are dropped when either limit is reached.");
        let (names, pos) = app.doc.history();
        let names: Vec<String> = names.map(str::to_owned).collect();
        let mut jump = None;
        egui::ScrollArea::vertical().id_salt("history").auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            if ui.selectable_label(pos == 0, RichText::new("Start of retained history").color(DIM)).clicked() {
                jump = Some(0);
            }
            for (i, name) in names.iter().enumerate() {
                let text = RichText::new(name).color(if i < pos { Color32::from_gray(225) } else { Color32::from_gray(110) });
                if ui.selectable_label(i + 1 == pos, text).clicked() {
                    jump = Some(i + 1);
                }
            }
        });
        if let Some(j) = jump {
            app.doc.jump_to(j);
        }
    });
}
