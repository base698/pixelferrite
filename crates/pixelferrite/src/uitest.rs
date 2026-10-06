//! Headless end-to-end test: drives the real app with synthetic pointer input
//! and renders it offscreen. The rendered frame is written to
//! `target/uitest/app.png` for eyeballing. Set `PF_TEST_IMAGE` to start from a
//! real photo instead of the generated one.

use std::path::PathBuf;

use pf_core::selection::Combine;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use pf_core::{Document, Pixmap, Target, io};

use crate::app::{Action, App};
use crate::tools::{GradientFill, Tool};

fn button(h: &Harness<'_, App>, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
}

/// Press at the first point, move through the rest, release at the last.
fn drag(h: &mut Harness<'_, App>, pts: &[Pos2]) {
    h.hover_at(pts[0]);
    h.step();
    button(h, pts[0], true);
    h.step();
    for p in &pts[1..] {
        h.hover_at(*p);
        h.step();
    }
    button(h, *pts.last().unwrap(), false);
    h.step();
}

#[test]
fn paint_select_and_render() {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/uitest");
    std::fs::create_dir_all(&out).unwrap();
    let image = match std::env::var_os("PF_TEST_IMAGE") {
        Some(p) => PathBuf::from(p),
        None => {
            let mut px = Pixmap::new(900, 600);
            for y in 0..600 {
                for x in 0..900 {
                    px.set(x, y, [(x * 255 / 900) as u8, (y * 255 / 600) as u8, 150, 255]);
                }
            }
            let p = out.join("input.png");
            io::export(&Document::from_pixmap(px, "bg").state, &p).unwrap();
            p
        }
    };

    let mut h = Harness::builder()
        .with_size(vec2(1400.0, 880.0))
        .wgpu()
        .build_eframe(|cc| App::new(cc, Some(image)));
    h.run_steps(3);
    let c = h.state().view.vp.center();
    assert!(h.state().view.vp.width() > 500.0, "canvas should get most of the window");

    // Brush stroke on a new layer.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::NewLayer);
    h.state_mut().tool = Tool::Brush;
    h.state_mut().settings.fg = [225, 40, 60, 255];
    let wave: Vec<Pos2> = (0..14).map(|i| c + vec2(-220.0 + i as f32 * 30.0, -80.0 + (i as f32 * 0.9).sin() * 60.0)).collect();
    drag(&mut h, &wave);
    let (names, _) = h.state().doc.history();
    assert_eq!(names.last(), Some("Brush"));
    let layer = h.state().doc.state.active_layer().unwrap();
    assert!(layer.pixels.data.chunks_exact(4).any(|p| p[3] > 0), "stroke should have painted pixels");

    // Elliptical selection, then a gradient clipped to it.
    h.state_mut().tool = Tool::EllipseSelect;
    drag(&mut h, &[c + vec2(-120.0, 20.0), c + vec2(0.0, 100.0), c + vec2(90.0, 170.0)]);
    assert!(h.state().doc.state.selection.is_some());

    // Inspector selection modes: add a separate rectangle, then subtract it again.
    let count = |h: &Harness<'_, App>| h.state().doc.state.selection.as_ref().map_or(0, |m| m.data.iter().filter(|v| **v > 0).count());
    let ellipse = count(&h);
    let rect = [c + vec2(150.0, -150.0), c + vec2(190.0, -120.0), c + vec2(220.0, -90.0)];
    h.state_mut().tool = Tool::RectSelect;
    h.state_mut().settings.sel_mode = Combine::Add;
    drag(&mut h, &rect);
    assert!(count(&h) > ellipse, "add mode should grow the selection");
    h.state_mut().settings.sel_mode = Combine::Subtract;
    drag(&mut h, &rect);
    assert_eq!(count(&h), ellipse, "subtract mode should remove the rectangle again");
    h.state_mut().settings.sel_mode = Combine::Intersect;
    drag(&mut h, &[c + vec2(-200.0, -50.0), c + vec2(0.0, 100.0), c + vec2(200.0, 250.0)]);
    assert_eq!(count(&h), ellipse, "intersecting with a covering rectangle keeps the ellipse");
    h.state_mut().settings.sel_mode = Combine::Replace;
    h.state_mut().tool = Tool::Gradient;
    h.state_mut().settings.fg = [40, 90, 230, 255];
    h.state_mut().settings.gradient_fill = GradientFill::ForegroundToTransparent;
    drag(&mut h, &[c + vec2(-100.0, 40.0), c + vec2(0.0, 100.0), c + vec2(70.0, 150.0)]);

    // Mask from the selection, undo/redo through the UI action path.
    h.state_mut().run(&ctx, Action::AddMask);
    assert_eq!(h.state().doc.effective_target(), Target::Mask);
    let (names, pos) = h.state().doc.history();
    let names: Vec<String> = names.map(str::to_owned).collect();
    assert_eq!(names, ["New Layer", "Brush", "Elliptical Selection", "Rectangular Selection", "Rectangular Selection", "Rectangular Selection", "Gradient", "Add Mask"]);
    assert_eq!(pos, 8);
    h.state_mut().run(&ctx, Action::Undo);
    assert!(h.state().doc.state.active_layer().unwrap().mask.is_none());
    h.state_mut().run(&ctx, Action::Redo);

    // Pinch-zoom and two-finger rotate arrive as these events.
    h.hover_at(c);
    h.step();
    let z0 = h.state().view.zoom;
    h.event(Event::Zoom(1.25));
    h.event(Event::Rotate(0.15));
    h.step();
    let v = &h.state().view;
    assert!((v.zoom / z0 - 1.25).abs() < 0.01, "zoom {} -> {}", z0, v.zoom);
    assert!((v.rot - 0.15).abs() < 1e-3, "rot {}", v.rot);

    h.state_mut().tool = Tool::Brush;
    h.hover_at(c + vec2(220.0, 130.0));
    h.run_steps(2);
    h.render().unwrap().save(out.join("app.png")).unwrap();

    // Arrange tool: dragging moves the active layer as one undo step.
    h.state_mut().run(&ctx, Action::ResetRotation);
    h.state_mut().tool = Tool::Move;
    h.state_mut().settings.auto_select = false;
    let x0 = h.state().doc.state.active_layer().unwrap().x;
    drag(&mut h, &[c, c + vec2(40.0, 10.0), c + vec2(80.0, 20.0)]);
    assert!(h.state().doc.state.active_layer().unwrap().x > x0);
    let (names, _) = h.state().doc.history();
    assert_eq!(names.last(), Some("Move Layer"));
    h.run_steps(2);
    h.render().unwrap().save(out.join("arrange.png")).unwrap();
}

#[test]
fn text_on_path_and_blur() {
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/uitest");
    std::fs::create_dir_all(&out).unwrap();
    let mut h = Harness::builder().with_size(vec2(1400.0, 880.0)).wgpu().build_eframe(|cc| App::new(cc, None));
    h.run_steps(3);
    let c = h.state().view.vp.center();
    let ctx = h.ctx.clone();
    let text = |h: &Harness<'_, App>| h.state().doc.state.active_layer().unwrap().text.clone();
    let type_text = |h: &mut Harness<'_, App>, s: &str| {
        h.run_steps(2);
        h.event(Event::Text(s.to_owned()));
        h.run_steps(2);
    };

    // A click places straight text; typing replaces the placeholder.
    h.state_mut().tool = Tool::Text;
    h.state_mut().settings.text.size = 90.0;
    h.run_steps(2);
    drag(&mut h, &[c + vec2(-330.0, -230.0)]);
    assert_eq!(text(&h).unwrap().text, "Text");
    type_text(&mut h, "Pixelferrite");
    assert_eq!(text(&h).unwrap().text, "Pixelferrite");
    let straight = h.state().doc.state.active;

    // A drag draws a path and the new text follows it.
    let wave: Vec<Pos2> = (0..=60).map(|i| i as f32 / 60.0).map(|t| c + vec2(-360.0 + t * 720.0, 120.0 - (t * std::f32::consts::PI).sin() * 230.0)).collect();
    drag(&mut h, &wave);
    assert_ne!(h.state().doc.state.active, straight);
    assert!(text(&h).unwrap().path.len() > 4, "dragging should give the text a path");
    h.state_mut().settings.text.color = [200, 40, 60, 255];
    type_text(&mut h, "Text that follows the path you draw, around the bend");
    let t = text(&h).unwrap();
    assert!(t.text.ends_with("bend") && t.path.len() > 4);
    let (names, _) = h.state().doc.history();
    assert_eq!(names.collect::<Vec<_>>(), ["Add Text", "Edit Text", "Add Text", "Edit Text"], "typing is one undo step per text");
    h.run_steps(2);
    h.render().unwrap().save(out.join("text.png")).unwrap();

    // Clicking existing text selects it for editing instead of adding more.
    let n = h.state().doc.state.layers.len();
    drag(&mut h, &[c + vec2(-250.0, -250.0)]);
    assert_eq!((h.state().doc.state.active, h.state().doc.state.layers.len()), (straight, n));

    // The arrange tool grabs text even when the pointer is between letters.
    h.state_mut().tool = Tool::Move;
    h.run_steps(2);
    let top = h.state().doc.state.layers.last().unwrap().id;
    h.state_mut().doc.state.active = h.state().doc.state.layers[0].id;
    let at = |h: &Harness<'_, App>, id| {
        let l = h.state().doc.state.layer(id).unwrap();
        (l.x, l.y)
    };
    let (was, grab) = (at(&h, straight), c + vec2(-250.0, -250.0));
    drag(&mut h, &[grab, grab + vec2(20.0, 30.0), grab + vec2(40.0, 60.0)]);
    assert_eq!(h.state().doc.state.active, straight, "clicking the text should select its layer");
    assert!(at(&h, straight).0 > was.0 && at(&h, straight).1 > was.1, "and dragging should move it");
    assert_eq!(h.state().doc.state.layer(straight).unwrap().text_origin().map(|o| o.1 > 0), Some(true));
    let was = at(&h, top);
    drag(&mut h, &[wave[30], wave[30] + vec2(0.0, 40.0)]);
    assert_eq!(h.state().doc.state.active, top, "path text is grabbed along its curve");
    assert!(at(&h, top).1 > was.1);
    h.state_mut().doc.state.active = straight;

    // Gaussian blur previews live, and Cancel puts everything back.
    let sharp = h.state().doc.state.active_layer().unwrap().pixels.clone();
    h.state_mut().run(&ctx, Action::GaussianBlur);
    h.run_steps(2);
    assert!(h.state().filter.is_some());
    assert_ne!(h.state().doc.state.active_layer().unwrap().pixels.data, sharp.data, "preview should already be blurred");
    h.render().unwrap().save(out.join("blur.png")).unwrap();
    let key = |h: &mut Harness<'_, App>, key| {
        h.event(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        h.run_steps(2);
    };
    key(&mut h, egui::Key::Escape);
    assert!(h.state().filter.is_none());
    assert_eq!(h.state().doc.state.active_layer().unwrap().pixels.data, sharp.data);
    assert!(text(&h).is_some());

    // Apply makes it one undo step and turns the text into pixels.
    h.state_mut().run(&ctx, Action::GaussianBlur);
    h.run_steps(2);
    key(&mut h, egui::Key::Enter);
    assert!(h.state().filter.is_none());
    let (names, _) = h.state().doc.history();
    assert_eq!(names.last(), Some("Gaussian Blur"));
    assert!(text(&h).is_none());
    h.state_mut().run(&ctx, Action::Undo);
    assert!(text(&h).is_some());
}

#[test]
fn edge_detection_and_ai_edit() {
    use std::io::{Read, Write};
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/uitest");
    std::fs::create_dir_all(&out).unwrap();
    let mut h = Harness::builder().with_size(vec2(1400.0, 880.0)).wgpu().build_eframe(|cc| App::new(cc, None));
    h.run_steps(3);
    let c = h.state().view.vp.center();
    let ctx = h.ctx.clone();
    let key = |h: &mut Harness<'_, App>, key| {
        h.event(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        h.run_steps(2);
    };

    // Something with edges: two filled shapes.
    for (tool, color, a, b) in [
        (Tool::EllipseSelect, [220, 60, 50, 255], vec2(-300.0, -200.0), vec2(-20.0, 80.0)),
        (Tool::RectSelect, [40, 90, 200, 255], vec2(40.0, -80.0), vec2(320.0, 220.0)),
    ] {
        h.state_mut().tool = tool;
        h.state_mut().settings.fg = color;
        drag(&mut h, &[c + a, c + (a + b) * 0.5, c + b]);
        h.state_mut().run(&ctx, Action::FillForeground);
    }
    h.state_mut().run(&ctx, Action::Deselect);
    let before = h.state().doc.state.layers[0].pixels.clone();

    // Edge detection previews, and can become a selection instead.
    h.state_mut().run(&ctx, Action::EdgeDetect);
    h.run_steps(2);
    let px = h.state().doc.state.layers[0].pixels.clone();
    let dark = px.data.chunks_exact(4).filter(|p| p[0] < 128).count();
    assert!(dark > 500 && dark < px.data.len() / 4 / 20, "lines on white expected, {dark} dark pixels");
    h.render().unwrap().save(out.join("edges.png")).unwrap();
    key(&mut h, egui::Key::Enter);
    let (names, _) = h.state().doc.history();
    assert_eq!(names.last(), Some("Edge Detection"));
    h.state_mut().run(&ctx, Action::Undo);
    assert_eq!(h.state().doc.state.layers[0].pixels.data, before.data);

    // The prompt dialog explains what will be sent.
    h.state_mut().tool = Tool::RectSelect;
    drag(&mut h, &[c + vec2(-250.0, -150.0), c, c + vec2(-60.0, 40.0)]);
    h.state_mut().run(&ctx, Action::AiPrompt);
    h.run_steps(3);
    assert!(h.state().ai.prompt_open());
    h.event(Event::Text("remove the red circle".into()));
    h.run_steps(2);
    h.render().unwrap().save(out.join("ai-prompt.png")).unwrap();
    key(&mut h, egui::Key::Escape);
    assert!(!h.state().ai.prompt_open());

    // Sending: a stand-in server answers with solid green; it arrives as a
    // new layer that only covers the selection.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let (mut req, mut buf) = (Vec::new(), [0u8; 65536]);
        loop {
            let n = s.read(&mut buf).unwrap();
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req).to_ascii_lowercase();
            if let (Some(head), Some(len)) = (text.find("\r\n\r\n"), text.split("content-length:").nth(1)) {
                if req.len() >= head + 4 + len.lines().next().unwrap().trim().parse::<usize>().unwrap() {
                    break;
                }
            }
        }
        let png = io::encode_png(&Pixmap::filled(256, 256, [0, 200, 0, 255])).unwrap();
        let json = format!("{{\"data\":[{{\"b64_json\":\"{}\"}}]}}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png));
        write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).unwrap();
    });
    let cfg = crate::ai::Config { key: Some("sk-test".into()), model: "gpt-image-2".into(), base, quality: None, key_source: "a test".into() };
    let layers = h.state().doc.state.layers.len();
    h.state_mut().send_to_ai(&ctx, "remove the red circle".into(), cfg, pf_core::aiedit::Source::Visible);
    assert!(h.state().ai.running());
    h.run_steps(2);
    h.render().unwrap().save(out.join("ai-working.png")).unwrap();
    for _ in 0..200 {
        if !h.state().ai.running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        h.step();
    }
    let st = &h.state().doc.state;
    assert_eq!(st.layers.len(), layers + 1, "the answer should arrive as a new layer");
    let new = st.layers.last().unwrap();
    assert_eq!(new.name, "AI: remove the red circle");
    let sel = pf_core::selection::bounds(st.selection.as_ref().unwrap()).unwrap();
    assert!(sel.expand(24).contains_rect(new.rect()), "new layer {:?} should sit on the selection {:?}", new.rect(), sel);
    let mid = ((sel.x0 + sel.x1) / 2, (sel.y0 + sel.y1) / 2);
    assert_eq!(pf_core::composite::sample(st, mid.0, mid.1), Some([0, 200, 0, 255]));
    assert_eq!(pf_core::composite::sample(st, sel.x0 - 40, mid.1), Some(before.px(sel.x0 - 40, mid.1)), "outside the selection is unchanged");
    h.run_steps(2);
    h.render().unwrap().save(out.join("ai-result.png")).unwrap();

    // The request was recorded with what was sent and what came back.
    let store = h.state().store.clone();
    let recs = store.ai_records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert_eq!((r.status.as_str(), r.prompt.as_str(), r.layer.as_str(), r.selection), ("done", "remove the red circle", "Background", true));
    assert_eq!(r.source, "visible");
    assert_eq!([r.region[2] - r.region[0], r.region[3] - r.region[1]].map(|v| v > 300), [true, true]);
    for f in ["request.json", "input.png", "mask.png", "output.png"] {
        assert!(store.ai_path(&r.id).join(f).exists(), "{f} should be saved");
    }
    let sent = io::load_pixmap(&store.ai_path(&r.id).join("input.png")).unwrap();
    assert_eq!([sent.w, sent.h], r.size);
    assert!(sent.data.chunks_exact(4).any(|p| p[0] > 200 && p[1] < 90), "the red circle should be in what was sent");
    h.state_mut().run(&ctx, Action::AiHistory);
    h.run_steps(4);
    h.render().unwrap().save(out.join("ai-history.png")).unwrap();
    h.state_mut().run(&ctx, Action::AiHistory);

    // Settings save to the config file and come back.
    h.state_mut().run(&ctx, Action::Settings);
    h.run_steps(3);
    h.render().unwrap().save(out.join("settings.png")).unwrap();
    key(&mut h, egui::Key::Escape);
    let mut s = h.state().saved.clone();
    s.ai.quality = "low".into();
    store.save_settings(&s).unwrap();
    assert_eq!(store.load_settings().0.ai.quality, "low");

    // Inserting an image into the selection fills it and takes its shape.
    let photo = out.join("insert.png");
    io::export(&Document::from_pixmap(Pixmap::filled(300, 100, [250, 200, 0, 255]), "p").state, &photo).unwrap();
    h.state_mut().tool = Tool::EllipseSelect;
    drag(&mut h, &[c + vec2(60.0, -60.0), c + vec2(150.0, 60.0), c + vec2(300.0, 200.0)]);
    let n = h.state().doc.state.layers.len();
    h.state_mut().insert_in_selection(&photo);
    let st = &h.state().doc.state;
    let sel = pf_core::selection::bounds(st.selection.as_ref().unwrap()).unwrap();
    let new = st.layers.last().unwrap();
    assert_eq!((st.layers.len(), new.rect(), new.name.as_str()), (n + 1, sel, "insert"));
    assert_eq!(new.pixels.px(sel.width() / 2, sel.height() / 2), [250, 200, 0, 255]);
    assert_eq!(new.pixels.px(1, 1)[3], 0, "corners outside the ellipse stay clear");
    h.run_steps(2);
    h.render().unwrap().save(out.join("insert.png.render.png")).unwrap();

    // Opened files show up under Open Recent, newest first.
    assert!(store.recent().is_empty());
    h.state_mut().open_path(&photo);
    h.state_mut().open_path(&out.join("input.png"));
    let recent = store.recent();
    assert_eq!(recent.iter().map(|f| f.path.file_name().unwrap().to_str().unwrap()).collect::<Vec<_>>(), ["input.png", "insert.png"]);
    h.state_mut().open_path(&out.join("does-not-exist.png"));
    assert_eq!(store.recent().len(), 2);
}
