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
