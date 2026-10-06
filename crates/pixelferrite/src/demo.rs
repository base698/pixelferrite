//! Renders the README screenshots by driving the real app offscreen.
//!
//! ```sh
//! PF_DEMO_ART=/path/to/art cargo test --release -p pixelferrite readme_ -- --ignored
//! ```
//!
//! `PF_DEMO_ART` holds `The Starry Night.jpg`, `Girl with a Pearl Earring.jpg` and
//! `Wanderer above the Sea of Fog.jpg` (public-domain
//! scans from Wikimedia Commons). The two `readme_ai_*` tests call OpenAI with
//! the configured key. Frames are written to `target/demo/` as PNG.

use std::path::PathBuf;

use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use pf_core::aiedit::Source;
use pf_core::filter::{EdgeParams, Filter};
use pf_core::selection::{self, Combine};
use pf_core::{Document, IRect, io};

use crate::app::{Action, App};
use crate::tools::Tool;

fn art(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("PF_DEMO_ART").expect("set PF_DEMO_ART to the folder with the demo paintings")).join(name)
}

fn out(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/demo");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn launch(file: Option<PathBuf>) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(vec2(1400.0, 880.0)).with_pixels_per_point(2.0).wgpu().build_eframe(|cc| App::new(cc, file));
    h.run_steps(4);
    h
}

fn shot(h: &mut Harness<'static, App>, name: &str) {
    h.run_steps(3);
    h.render().unwrap().save(out(name)).unwrap();
}

fn at(h: &Harness<'static, App>, x: f32, y: f32) -> Pos2 {
    h.state().view.to_screen(pos2(x, y))
}

fn drag(h: &mut Harness<'static, App>, pts: &[Pos2]) {
    let button = |h: &Harness<'static, App>, pos, pressed| h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
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

fn key(h: &mut Harness<'static, App>, key: egui::Key) {
    h.event(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
}

/// Opens the prompt dialog for a picture of it, then sends for real and waits.
fn ask_ai(h: &mut Harness<'static, App>, prompt: &str, name: &str) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::AiPrompt);
    h.run_steps(3);
    h.state_mut().ai.set_key_source(".env");
    h.event(Event::Text(prompt.into()));
    shot(h, &format!("{name}-prompt.png"));
    key(h, egui::Key::Escape);

    let mut cfg = crate::ai::Config::load(&h.state().saved.ai);
    assert!(cfg.key.is_some(), "no OpenAI key configured");
    cfg.quality = Some(std::env::var("PF_DEMO_QUALITY").unwrap_or_else(|_| "medium".into()));
    h.state_mut().send_to_ai(&ctx, prompt.into(), cfg, Source::Visible);
    for _ in 0..3000 {
        if !h.state().ai.running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        h.step();
    }
    assert!(!h.state().ai.running(), "the request timed out");
    let rec = h.state().store.ai_records().into_iter().next().expect("a recorded request");
    println!("{name}: {} in {} ms, usage {:?}, error {:?}", rec.status, rec.duration_ms, rec.usage, rec.error);
    assert_eq!(rec.status, "done");
    shot(h, &format!("{name}-selected.png"));
    h.state_mut().run(&ctx, Action::Deselect);
    shot(h, &format!("{name}-result.png"));
    io::export(&h.state().doc.state, &out(&format!("{name}-flat.png"))).unwrap();
}

#[test]
#[ignore = "renders README screenshots; needs PF_DEMO_ART"]
fn readme_text_and_filters() {
    let mut h = launch(Some(art("The Starry Night.jpg")));
    let ctx = h.ctx.clone();

    // Edge detection, previewed live in its dialog.
    h.state_mut().tool = Tool::Move;
    h.state_mut().run(&ctx, Action::OpenFilter(Filter::EdgeDetect(EdgeParams::default())));
    shot(&mut h, "edges.png");
    key(&mut h, egui::Key::Escape);

    // Perspective, with a corner pulled in.
    h.state_mut().run(&ctx, Action::Perspective);
    h.run_steps(2);
    for (from, to) in [((1920.0, 0.0), (1640.0, 210.0)), ((1920.0, 1520.0), (1700.0, 1330.0))] {
        let (a, b) = (at(&h, from.0, from.1), at(&h, to.0, to.1));
        drag(&mut h, &[a, a + (b - a) * 0.5, b]);
    }
    shot(&mut h, "perspective.png");
    key(&mut h, egui::Key::Escape);

    // Text along a drawn path, sweeping over the hills.
    h.state_mut().tool = Tool::Text;
    h.state_mut().settings.text.size = 116.0;
    h.state_mut().settings.text.color = [255, 238, 170, 255];
    h.run_steps(2);
    let path: Vec<Pos2> = (0..=60).map(|i| i as f32 / 60.0).map(|t| at(&h, 640.0 + t * 1180.0, 1010.0 - (t * std::f32::consts::PI).sin() * 150.0)).collect();
    drag(&mut h, &path);
    h.run_steps(2);
    h.event(Event::Text("The Starry Night, 1889".into()));
    h.hover_at(at(&h, 1200.0, 1420.0));
    shot(&mut h, "text-path.png");
}

#[test]
#[ignore = "renders README screenshots; needs PF_DEMO_ART"]
fn readme_select_subject() {
    let mut h = launch(Some(art("Girl with a Pearl Earring.jpg")));
    let ctx = h.ctx.clone();
    h.state_mut().tool = Tool::SubjectSelect;
    let (a, b) = (at(&h, 250.0, 130.0), at(&h, 1840.0, 2270.0));
    drag(&mut h, &[a, a + (b - a) * 0.5, b]);
    h.hover_at(at(&h, 1700.0, 300.0));
    shot(&mut h, "subject.png");

    // The selection becomes a mask, over a new colour behind her.
    h.state_mut().run(&ctx, Action::AddMask);
    h.state_mut().run(&ctx, Action::Deselect);
    shot(&mut h, "subject-mask.png");
}

#[test]
#[ignore = "calls OpenAI; renders README screenshots; needs PF_DEMO_ART"]
fn readme_ai_extend() {
    // A portrait painting in the middle of a wide canvas, empty either side.
    let px = io::load_pixmap(&art("Wanderer above the Sea of Fog.jpg")).unwrap();
    let mut doc = Document::new(1536, 1024, None);
    doc.add_image_scaled("Wanderer above the Sea of Fog", &px, IRect::new(368, 0, 1168, 1024));
    let file = out("wanderer-wide.ora");
    io::save(&mut doc, &file).unwrap();
    let mut h = launch(Some(file));
    for r in [IRect::new(0, 0, 380, 1024), IRect::new(1156, 0, 1536, 1024)] {
        h.state_mut().doc.select("Rectangular Selection", &selection::rect_mask(1536, 1024, r), Combine::Add);
    }
    h.state_mut().tool = Tool::RectSelect;
    shot(&mut h, "extend-before.png");
    ask_ai(&mut h, "continue the sea of fog and the distant peaks", "extend");
}

#[test]
#[ignore = "calls OpenAI; renders README screenshots; needs PF_DEMO_ART"]
fn readme_ai_remove() {
    let mut h = launch(Some(art("The Starry Night.jpg")));
    // A loose lasso around the tree, down to the bottom edge.
    h.state_mut().tool = Tool::Lasso;
    let loop_: Vec<Pos2> = [(235, 1515), (210, 1100), (225, 700), (240, 450), (290, 150), (345, 12), (545, 12), (625, 420), (735, 820), (865, 1120), (925, 1515)]
        .iter()
        .map(|&(x, y)| at(&h, x as f32, y as f32))
        .collect();
    drag(&mut h, &loop_);
    ask_ai(&mut h, "remove the cypress tree", "remove");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::AiHistory);
    shot(&mut h, "ai-history.png");
}
