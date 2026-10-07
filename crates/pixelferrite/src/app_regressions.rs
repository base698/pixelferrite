use std::{path::{Path, PathBuf}, sync::{Arc, mpsc}, time::{Duration, Instant}};
use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::{Harness, kittest::Queryable};
use pf_core::{Document, io, paint::{Stroke, PaintKind}, filter::Filter};
use crate::{app::{App, Action, NewDoc}, canvas::{self, Drag}, jobs::Outcome};

fn app() -> Harness<'static, App> {
    Harness::builder().with_size(vec2(1400.0, 880.0)).build_eframe(|cc| {
        let mut app = App::new(cc, None);
        app.doc = Document::new(128, 128, Some([255; 4]));
        app
    })
}

#[test]
fn about_menu_shows_and_copies_build_identity_without_editing_document() {
    let mut h = Harness::builder().with_size(vec2(1000.0, 700.0)).wgpu()
        .build_eframe(|cc| App::new(cc, None));
    h.run_steps(2);
    let before = h.state().doc.history().1;
    h.get_by_label("Help").click();
    h.run_steps(2);
    h.get_by_label("About Pixelferrite").click();
    h.run_steps(2);
    assert!(h.state().show_about);
    h.get_by_label(crate::about::COMMIT);
    h.get_by_label(crate::about::SOURCE_STATUS);
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/uitest/about-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    h.render().unwrap().save(out.join("about.png")).unwrap();
    h.get_by_label("Copy build info").click();
    h.step();
    assert!(h.output().platform_output.commands.iter().any(|command|
        matches!(command, egui::OutputCommand::CopyText(text) if text == &crate::about::build_info())));
    assert_eq!(h.state().doc.history().1, before);
    assert!(!h.state().doc.modified);
    h.get_by_label("Close").click();
    h.run_steps(2);
    assert!(!h.state().show_about);
}

fn escape(h: &mut Harness<'_, App>) {
    h.event(Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::Key { key: Key::Escape, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
}

fn finish_work(h: &mut Harness<'_, App>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while h.state().work.is_some() {
        assert!(Instant::now() < deadline, "background operation did not finish");
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn cancel_empty_stroke_preserves_previous_edit() {
    let mut h = app();
    let a = h.state_mut();
    a.doc.add_empty_layer();
    let old_pos = a.doc.history().1;
    a.drag = Drag::Stroke(Stroke::begin(&mut a.doc, PaintKind::Brush([0, 0, 0, 255]), a.settings.brush, (-100.0, -100.0)).unwrap());
    canvas::cancel(a);
    assert_eq!(a.doc.history().1, old_pos);
    assert_eq!(a.doc.state.layers.len(), 2);
}

#[test]
fn cancel_stroke_preserves_redo_branch() {
    let mut h = app();
    let a = h.state_mut();
    a.doc.add_empty_layer();
    a.doc.undo();
    a.drag = Drag::Stroke(Stroke::begin(&mut a.doc, PaintKind::Brush([0, 0, 0, 255]), a.settings.brush, (64.0, 64.0)).unwrap());
    canvas::cancel(a);
    assert_eq!(a.doc.history().0.collect::<Vec<_>>(), ["New Layer"]);
    assert!(a.doc.redo());
    assert_eq!(a.doc.state.layers.len(), 2);
}

#[derive(Debug)]
struct FileDrop(PathBuf);
impl egui::DroppedFile for FileDrop {
    fn path(&self) -> &Path { &self.0 }
    fn bytes(&self) -> Result<Vec<u8>, String> { std::fs::read(&self.0).map_err(|e| e.to_string()) }
}

#[test]
fn dropped_image_waits_for_filter_or_perspective_cancel_and_remains_undoable() {
    for action in [Action::GaussianBlur, Action::Perspective] {
        let mut h = app();
        let ctx = h.ctx.clone();
        let image = h.state().store.config_dir.join("drop.png");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        io::export(&Document::new(16, 16, Some([0, 200, 0, 255])).state, &image).unwrap();
        h.state_mut().run(&ctx, action);
        h.input_mut().dropped_files.push(Arc::new(FileDrop(image)));
        h.run_steps(2);
        assert_eq!(h.state().doc.state.layers.len(), 1, "drop must wait outside preview state");
        escape(&mut h);
        assert!(!h.state().busy());
        assert_eq!(h.state().doc.state.layers.len(), 2);
        assert_eq!(h.state().doc.history().0.collect::<Vec<_>>(), ["Add Image"]);
        h.state_mut().run(&ctx, Action::Undo);
        assert_eq!(h.state().doc.state.layers.len(), 1);
        h.state_mut().run(&ctx, Action::Redo);
        assert_eq!(h.state().doc.state.layers.len(), 2);
    }
}

#[test]
fn invalid_new_image_reports_error_without_replacing_document() {
    let mut h = app();
    let original = h.state().doc.state.active;
    h.state_mut().new_doc = Some(NewDoc { w: 16_384, h: 16_384, transparent: true });
    h.run_steps(2);
    h.get_by_label("Create").click();
    h.step();
    assert!(h.state().error.as_ref().is_some_and(|e| e.contains("dimensions")));
    assert_eq!(h.state().doc.state.active, original);
    assert_eq!(h.state().doc.state.width, 128);
}

#[test]
fn background_cancel_and_stale_completion_cannot_change_document() {
    for cancel in [true, false] {
        let mut h = app();
        let (release, wait) = mpsc::channel();
        let mut computed = Document::new(128, 128, Some([255; 4]));
        computed.add_empty_layer();
        assert!(h.state_mut().start_work("Test operation", move || {
            wait.recv().unwrap();
            Ok(Outcome::Edit { state: computed.state, label: "Test" })
        }));
        h.run_steps(2);
        assert!(h.state().background_active());
        if cancel {
            h.get_by_label("Cancel Operation").click();
            h.step();
        } else {
            // Simulate a document replacement arriving before a worker reply.
            h.state_mut().doc = Document::new(64, 64, None);
        }
        release.send(()).unwrap();
        finish_work(&mut h);
        assert_eq!(h.state().doc.state.layers.len(), 1);
        assert_eq!(h.state().doc.history().1, 0);
    }
}

#[test]
fn large_filter_preview_runs_in_background_and_cancel_restores_pixels() {
    let mut h = app();
    let ctx = h.ctx.clone();
    h.state_mut().doc = Document::new(1100, 1000, Some([70, 120, 160, 255]));
    let original = h.state().doc.state.active_layer().unwrap().pixels.clone();
    h.state_mut().run(&ctx, Action::OpenFilter(Filter::Threshold { level: 128.0 }));
    assert!(h.state().background_active());
    finish_work(&mut h);
    assert_ne!(h.state().doc.state.active_layer().unwrap().pixels.data, original.data);
    escape(&mut h);
    assert!(h.state().filter.is_none());
    assert_eq!(h.state().doc.state.active_layer().unwrap().pixels.data, original.data);
    assert_eq!(h.state().doc.history().1, 0);
}

#[test]
fn rejected_merge_explains_why_and_preserves_composite() {
    let mut h = app();
    let ctx = h.ctx.clone();
    let a = h.state_mut();
    let lower = a.doc.add_empty_layer();
    a.doc.state.layer_mut(lower).unwrap().blend = pf_core::BlendMode::Multiply;
    a.doc.add_empty_layer();
    let before = pf_core::composite::flatten(&a.doc.state);
    a.run(&ctx, Action::MergeDown);
    assert!(a.toast.as_ref().is_some_and(|(text, _)| text.contains("blend modes")));
    assert_eq!(a.doc.state.layers.len(), 3);
    assert_eq!(pf_core::composite::flatten(&a.doc.state).data, before.data);
}

fn pointer_button(h: &mut Harness<'_, App>, pos: egui::Pos2, pressed: bool) {
    h.input_mut().events.push(Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    });
    h.step();
}

fn request_close(h: &mut Harness<'_, App>) {
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default()
        .events.push(egui::ViewportEvent::Close);
    h.step();
    assert!(h.output().viewport_output[&egui::ViewportId::ROOT].commands.iter()
        .any(|command| matches!(command, egui::ViewportCommand::CancelClose)),
        "an unfinished edit must cancel the actual window-close event");
}

#[test]
fn window_close_keeps_uncommitted_stroke_until_release() {
    let mut h = app();
    let original = h.state().doc.state.active_layer().unwrap().pixels.clone();
    let pos = h.state().view.to_screen(egui::pos2(64.0, 64.0));
    h.input_mut().events.push(Event::PointerMoved(pos));
    h.step();
    pointer_button(&mut h, pos, true);
    assert!(matches!(h.state().drag, Drag::Stroke(_)));
    assert_ne!(h.state().doc.state.active_layer().unwrap().pixels.data, original.data);
    assert!(!h.state().doc.modified, "the stroke has not committed yet");

    request_close(&mut h);
    assert!(matches!(h.state().drag, Drag::Stroke(_)));
    assert_eq!(h.state().doc.history().1, 0);
    assert_ne!(h.state().doc.state.active_layer().unwrap().pixels.data, original.data);

    pointer_button(&mut h, pos, false);
    assert!(matches!(h.state().drag, Drag::None));
    assert!(h.state().doc.modified);
    assert_eq!(h.state().doc.history().1, 1);
    assert!(h.state_mut().doc.undo());
    assert_eq!(h.state().doc.state.active_layer().unwrap().pixels.data, original.data);
}

#[test]
fn window_close_keeps_uncommitted_property_until_release() {
    let mut h = app();
    // Hold the pointer outside the widgets so only the staged property is active.
    let pos = egui::pos2(-10.0, -10.0);
    pointer_button(&mut h, pos, true);
    let before = h.state().doc.begin();
    h.state_mut().doc.state.active_layer_mut().unwrap().opacity = 0.42;
    h.state_mut().prop = Some((egui::Id::new("close-opacity"), before));
    assert!(!h.state().doc.modified);

    request_close(&mut h);
    assert!(h.state().prop.is_some());
    assert_eq!(h.state().doc.state.active_layer().unwrap().opacity, 0.42);
    assert_eq!(h.state().doc.history().1, 0);

    pointer_button(&mut h, pos, false);
    assert!(h.state().prop.is_none());
    assert!(h.state().doc.modified);
    assert_eq!(h.state().doc.history().1, 1);
    assert!(h.state_mut().doc.undo());
    assert_eq!(h.state().doc.state.active_layer().unwrap().opacity, 1.0);
}

#[test]
fn move_drag_clamps_extreme_displacements_and_remains_saveable() {
    let limit = io::limits::MAX_LAYER_OFFSET;
    for sign in [-1, 1] {
        let mut h = app();
        let a = h.state_mut();
        a.tool = crate::tools::Tool::Move;
        a.settings.auto_select = false;
        let layer = a.doc.state.active_layer_mut().unwrap();
        (layer.x, layer.y) = (sign * limit, sign * limit);
        let center = h.state().view.vp.center();
        h.input_mut().events.push(Event::PointerMoved(center));
        h.step();
        pointer_button(&mut h, center, true);
        assert!(matches!(h.state().drag, Drag::Move { .. }));

        // The delta saturates an i32 before it is added to an already maximal
        // offset. This exercises both arithmetic overflow and document limits.
        let far = egui::pos2(sign as f32 * 1.0e12, sign as f32 * 1.0e12);
        h.input_mut().events.push(Event::PointerMoved(far));
        h.step();
        let layer = h.state().doc.state.active_layer().unwrap();
        assert_eq!((layer.x, layer.y), (sign * limit, sign * limit));
        let opposite = egui::pos2(-far.x, -far.y);
        h.input_mut().events.push(Event::PointerMoved(opposite));
        h.step();
        pointer_button(&mut h, opposite, false);
        let layer = h.state().doc.state.active_layer().unwrap();
        assert_eq!((layer.x, layer.y), (-sign * limit, -sign * limit));
        assert_eq!(h.state().doc.history().0.collect::<Vec<_>>(), ["Move Layer"]);
        io::limits::validate_document(&h.state().doc.state).unwrap();

        let path = h.state().store.config_dir.join("bounded-move.ora");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        io::ora::save(&h.state().doc.state, &path).unwrap();
        let reopened = io::open(&path).unwrap();
        let layer = reopened.state.active_layer().unwrap();
        assert_eq!((layer.x, layer.y), (-sign * limit, -sign * limit));
        std::fs::remove_file(path).unwrap();
        assert!(h.state_mut().doc.undo());
        let layer = h.state().doc.state.active_layer().unwrap();
        assert_eq!((layer.x, layer.y), (sign * limit, sign * limit));
    }
}

#[test]
fn select_edges_instead_preserves_pixels_and_commits_one_undoable_selection() {
    let mut h = app();
    let ctx = h.ctx.clone();
    let a = h.state_mut();
    let layer = a.doc.state.active_layer_mut().unwrap();
    let pixels = Arc::make_mut(&mut layer.pixels);
    for y in 0..128 {
        for x in 0..128 {
            pixels.set(x, y, if x < 64 { [20, 30, 40, 255] } else { [220, 210, 200, 255] });
        }
    }
    let original = a.doc.state.active_layer().unwrap().pixels.clone();
    a.doc.mark_all_dirty();
    a.run(&ctx, Action::EdgeDetect);
    finish_work(&mut h);
    h.run_steps(2);
    assert_ne!(h.state().doc.state.active_layer().unwrap().pixels.data, original.data,
        "the edge preview must be visible before selecting instead");

    h.get_by_label("Select Edges Instead").click();
    h.step();
    finish_work(&mut h);
    assert!(h.state().filter.is_none());
    assert_eq!(h.state().doc.state.active_layer().unwrap().pixels.data, original.data);
    let selected = h.state().doc.state.selection.as_ref().expect("split colors produce edges").clone();
    assert!(selected.data.iter().any(|v| *v != 0));
    assert!(selected.data.contains(&0));
    assert_eq!(h.state().doc.history().0.collect::<Vec<_>>(), ["Select Edges"]);
    assert_eq!(h.state().doc.history().1, 1);
    assert!(h.state_mut().doc.undo());
    assert!(h.state().doc.state.selection.is_none());
    assert_eq!(h.state().doc.state.active_layer().unwrap().pixels.data, original.data);
    assert!(h.state_mut().doc.redo());
    assert_eq!(h.state().doc.state.selection.as_ref().unwrap().data, selected.data);
}
