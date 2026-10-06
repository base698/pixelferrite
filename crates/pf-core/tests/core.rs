use pf_core::fill::{self, GradientOp, GradientParams, GradientShape};
use pf_core::paint::{BrushParams, PaintKind, Stroke};
use pf_core::selection::{self, Combine};
use pf_core::*;

const RED: [u8; 4] = [255, 0, 0, 255];
const WHITE: [u8; 4] = [255; 4];

fn brush(size: f32) -> BrushParams {
    BrushParams { size, hardness: 1.0, opacity: 1.0, flow: 1.0, spacing: 0.1 }
}

fn px(doc: &Document, x: i32, y: i32) -> [u8; 4] {
    composite::sample(&doc.state, x, y).unwrap()
}

#[test]
fn brush_stroke_undo_redo() {
    let mut doc = Document::new(64, 64, Some(WHITE));
    let mut s = Stroke::begin(&mut doc, PaintKind::Brush(RED), brush(8.0), (10.0, 32.0)).unwrap();
    s.line_to(&mut doc, (50.0, 32.0));
    s.finish(&mut doc);
    assert_eq!(px(&doc, 30, 32), RED);
    assert_eq!(px(&doc, 30, 10), WHITE);
    assert!(doc.undo());
    assert_eq!(px(&doc, 30, 32), WHITE);
    assert!(doc.redo());
    assert_eq!(px(&doc, 30, 32), RED);
}

#[test]
fn stroke_opacity_does_not_build_up() {
    let mut doc = Document::new(64, 64, Some(WHITE));
    let p = BrushParams { opacity: 0.5, ..brush(10.0) };
    let mut s = Stroke::begin(&mut doc, PaintKind::Brush([0, 0, 0, 255]), p, (10.0, 32.0)).unwrap();
    for _ in 0..4 {
        s.line_to(&mut doc, (50.0, 32.0));
        s.line_to(&mut doc, (10.0, 32.0));
    }
    s.finish(&mut doc);
    let v = px(&doc, 30, 32)[0];
    assert!((126..=129).contains(&v), "got {v}");
}

#[test]
fn painting_respects_selection_and_grows_small_layers() {
    let mut doc = Document::new(64, 64, Some(WHITE));
    let id = doc.add_image_layer("small", Pixmap::new(4, 4));
    doc.select("Select", &selection::rect_mask(64, 64, IRect::new(0, 0, 32, 64)), Combine::Replace);
    let mut s = Stroke::begin(&mut doc, PaintKind::Pencil(RED), brush(6.0), (5.0, 5.0)).unwrap();
    s.line_to(&mut doc, (60.0, 5.0));
    s.finish(&mut doc);
    assert_eq!(doc.state.layer(id).unwrap().rect(), doc.canvas());
    assert_eq!(px(&doc, 20, 5), RED);
    assert_eq!(px(&doc, 40, 5), WHITE);
    doc.undo();
    assert_eq!(doc.state.layer(id).unwrap().pixels.w, 4);
    assert_eq!(px(&doc, 20, 5), WHITE);
}

#[test]
fn eraser_mask_and_blend() {
    let mut doc = Document::new(32, 32, Some(WHITE));
    doc.add_empty_layer();
    doc.fill(RED);
    assert_eq!(px(&doc, 5, 5), RED);
    let id = doc.state.active;
    doc.add_mask(id);
    // Painting black on the mask hides the layer there.
    let mut s = Stroke::begin(&mut doc, PaintKind::Brush([0, 0, 0, 255]), brush(10.0), (16.0, 16.0)).unwrap();
    s.line_to(&mut doc, (17.0, 16.0));
    s.finish(&mut doc);
    assert_eq!(px(&doc, 16, 16), WHITE);
    assert_eq!(px(&doc, 2, 2), RED);
    doc.apply_mask(id);
    assert_eq!(px(&doc, 16, 16), WHITE);
    doc.state.layer_mut(id).unwrap().blend = BlendMode::Multiply;
    assert_eq!(px(&doc, 2, 2), RED);
    doc.target = Target::Pixels;
    let s = Stroke::begin(&mut doc, PaintKind::Eraser, brush(4.0), (2.0, 2.0)).unwrap();
    s.finish(&mut doc);
    assert_eq!(px(&doc, 2, 2), WHITE);
}

#[test]
fn magic_wand_bucket_and_outline() {
    let mut doc = Document::new(40, 40, Some(WHITE));
    doc.select("r", &selection::rect_mask(40, 40, IRect::new(10, 10, 20, 20)), Combine::Replace);
    doc.fill(RED);
    doc.deselect();
    let src = fill::sample_source(&doc.state, true).unwrap();
    let m = selection::flood_mask(&src, (15, 15), 10, true, None);
    assert_eq!(selection::bounds(&m), Some(IRect::new(10, 10, 20, 20)));
    assert_eq!(selection::outline(&m).len(), 4);
    assert!(fill::bucket(&mut doc, (0, 0), [0, 0, 255, 255], 10, true, true, 1.0));
    assert_eq!(px(&doc, 30, 30), [0, 0, 255, 255]);
    assert_eq!(px(&doc, 15, 15), RED);
}

#[test]
fn clone_smudge_gradient() {
    let mut doc = Document::new(64, 64, Some(WHITE));
    doc.select("r", &selection::rect_mask(64, 64, IRect::new(0, 0, 16, 64)), Combine::Replace);
    doc.fill(RED);
    doc.deselect();
    let mut s = Stroke::begin(&mut doc, PaintKind::Clone { dx: -40, dy: 0 }, brush(8.0), (48.0, 20.0)).unwrap();
    s.line_to(&mut doc, (50.0, 20.0));
    s.finish(&mut doc);
    assert_eq!(px(&doc, 48, 20), RED);

    let mut s = Stroke::begin(&mut doc, PaintKind::Smudge { strength: 0.9 }, brush(10.0), (10.0, 40.0)).unwrap();
    s.line_to(&mut doc, (26.0, 40.0));
    s.finish(&mut doc);
    let p = px(&doc, 20, 40);
    assert!(p[1] < 200, "red should be dragged into white, got {p:?}");
    doc.undo();
    assert_eq!(px(&doc, 20, 40), WHITE);

    let mut g = GradientOp::begin(&mut doc).unwrap();
    let gp = GradientParams { shape: GradientShape::Linear, from: [0, 0, 0, 255], to: WHITE, opacity: 1.0 };
    g.update(&mut doc, (0.0, 0.0), (64.0, 0.0), &gp);
    g.finish(&mut doc);
    assert!(px(&doc, 1, 30)[0] < 10 && px(&doc, 62, 30)[0] > 245);
    let mid = px(&doc, 32, 30)[0];
    assert!((120..136).contains(&mid), "got {mid}");
}

#[test]
fn layer_ops_and_clipboard() {
    let mut doc = Document::new(32, 32, Some(WHITE));
    let top = doc.add_empty_layer();
    doc.fill(RED);
    doc.select("r", &selection::ellipse_mask(32, 32, 4.0, 4.0, 20.0, 20.0), Combine::Replace);
    let clip = doc.cut().unwrap();
    assert_eq!(px(&doc, 12, 12), WHITE);
    doc.deselect();
    let pasted = doc.paste(&clip);
    assert_eq!(px(&doc, 12, 12), RED);
    doc.reorder_layer(pasted, ops::Reorder::Back);
    assert_eq!(doc.state.layers[0].id, pasted);
    doc.reorder_layer(pasted, ops::Reorder::Front);
    doc.merge_down(pasted);
    assert_eq!(doc.state.layers.len(), 2);
    assert_eq!(doc.state.active, top);
    assert_eq!(px(&doc, 12, 12), RED);
    doc.crop(IRect::new(8, 8, 24, 24));
    assert_eq!((doc.state.width, doc.state.height), (16, 16));
    assert_eq!(px(&doc, 4, 4), RED);
    let (names, pos) = doc.history();
    assert_eq!(names.count(), pos);
    doc.jump_to(0);
    assert_eq!(doc.state.layers.len(), 1);
    assert_eq!((doc.state.width, px(&doc, 12, 12)), (32, WHITE));
}

#[test]
fn ora_roundtrip_and_export() {
    let dir = std::env::temp_dir().join(format!("pf-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut doc = Document::new(48, 32, Some(WHITE));
    let id = doc.add_image_layer("Sm & <all>", Pixmap::filled(10, 10, RED));
    doc.add_mask(id);
    {
        let l = doc.state.layer_mut(id).unwrap();
        l.opacity = 0.5;
        l.blend = BlendMode::Multiply;
    }
    let path = dir.join("t.ora");
    io::save(&mut doc, &path).unwrap();
    let back = io::open(&path).unwrap();
    assert_eq!(back.state.layers.len(), 2);
    let l = &back.state.layers[1];
    assert_eq!((l.name.as_str(), l.x, l.y, l.opacity, l.blend), ("Sm & <all>", 19, 11, 0.5, BlendMode::Multiply));
    assert!(l.mask.is_some());
    assert_eq!(composite::flatten(&back.state).data, composite::flatten(&doc.state).data);
    for e in io::EXPORT_EXTENSIONS {
        let p = dir.join(format!("out.{e}"));
        io::export(&doc.state, &p).unwrap();
        let img = io::load_pixmap(&p).unwrap();
        assert_eq!((img.w, img.h), (48, 32));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
