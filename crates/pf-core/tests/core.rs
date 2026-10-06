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

#[test]
fn gaussian_blur_filter() {
    use pf_core::filter::{self, Filter};
    // A hard black/white edge on a canvas-filling layer.
    let mut doc = Document::new(80, 40, Some(WHITE));
    doc.select("r", &selection::rect_mask(80, 40, IRect::new(0, 0, 40, 40)), Combine::Replace);
    doc.fill([0, 0, 0, 255]);
    doc.deselect();
    let steps = doc.history().1;
    assert!(filter::apply_filter(&mut doc, &Filter::GaussianBlur { radius: 4.0 }));
    assert_eq!(doc.history().1, steps + 1);
    // The edge is now a ramp, far sides are untouched, and nothing went transparent.
    let mid = px(&doc, 40, 20)[0];
    assert!((100..156).contains(&mid), "edge should be mid grey, got {mid}");
    assert!(px(&doc, 37, 20)[0] < mid && mid < px(&doc, 43, 20)[0]);
    assert_eq!(px(&doc, 2, 20), [0, 0, 0, 255]);
    assert_eq!(px(&doc, 78, 2), WHITE);
    assert!(doc.undo());
    assert_eq!(px(&doc, 40, 20), WHITE);
    assert_eq!(px(&doc, 39, 20), [0, 0, 0, 255]);

    // Limited to a selection: pixels outside it don't change.
    doc.select("r", &selection::rect_mask(80, 40, IRect::new(0, 0, 80, 20)), Combine::Replace);
    filter::apply_filter(&mut doc, &Filter::GaussianBlur { radius: 30.0 });
    assert_eq!(px(&doc, 39, 30), [0, 0, 0, 255]);
    assert_ne!(px(&doc, 39, 10), [0, 0, 0, 255]);

    // A small layer grows so the blur can spread past its old edge.
    let mut doc = Document::new(100, 100, None);
    let id = doc.add_image_layer("dot", Pixmap::filled(10, 10, RED));
    filter::apply_filter(&mut doc, &Filter::GaussianBlur { radius: 5.0 });
    let l = doc.state.layer(id).unwrap();
    assert!(l.pixels.w > 30, "layer should have grown, is {}", l.pixels.w);
    let halo = px(&doc, 40, 50);
    assert!(halo[3] > 0 && halo[3] < 255 && halo[0] > 250, "soft red halo, got {halo:?}");
    assert!(px(&doc, 50, 50)[3] < 255);
}

#[test]
fn text_layers() {
    use ab_glyph::FontRef;
    use pf_core::text::{self, TextSpec};
    let font = FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    let ink = |doc: &Document, r: IRect| {
        (r.y0..r.y1).flat_map(|y| (r.x0..r.x1).map(move |x| (x, y))).filter(|(x, y)| px(doc, *x, *y) != WHITE).count()
    };

    // Straight text sits on its baseline, to the right of the origin.
    let mut doc = Document::new(400, 300, Some(WHITE));
    let spec = TextSpec { text: "Hello".into(), size: 40.0, color: RED, ..Default::default() };
    let id = doc.add_text_layer(spec.clone(), (50, 150), &font);
    let l = doc.state.layer(id).unwrap();
    assert_eq!(l.text_origin(), Some((50, 150)));
    assert!(ink(&doc, IRect::new(50, 110, 200, 152)) > 200);
    assert_eq!(ink(&doc, IRect::new(0, 160, 400, 300)), 0);
    assert_eq!(ink(&doc, IRect::new(0, 0, 45, 300)), 0);

    // Editing re-renders in place; consecutive edits are one undo step.
    for (i, t) in ["Hello w", "Hello wo", "Hello world"].iter().enumerate() {
        let before = doc.begin();
        assert!(doc.update_text(id, TextSpec { text: (*t).into(), ..spec.clone() }, &font));
        doc.commit_merged("Edit Text", before, 7);
        assert_eq!(doc.history().1, 2, "edit {i} should merge");
    }
    assert_eq!(doc.state.layer(id).unwrap().text_origin(), Some((50, 150)));
    assert!(ink(&doc, IRect::new(200, 110, 300, 152)) > 100);
    doc.undo();
    assert_eq!(doc.state.layer(id).unwrap().text.as_ref().unwrap().text, "Hello");
    doc.redo();

    // Along a path: text on a vertical line runs downwards, rotated.
    let down = TextSpec { text: "Hello world".into(), path: vec![(0.0, 0.0), (0.0, 200.0)], ..spec.clone() };
    let before = doc.begin();
    doc.update_text(id, down, &font);
    doc.commit("Edit Text", before);
    assert_eq!(ink(&doc, IRect::new(120, 0, 400, 300)), 0, "nothing should be left of the old horizontal text");
    assert!(ink(&doc, IRect::new(40, 150, 100, 300)) > 200, "text should run down the path");
    assert!(ink(&doc, IRect::new(51, 150, 100, 300)) > 200, "glyph tops face right on a downward baseline");

    // A circle of text surrounds its centre without covering it.
    let ring: Vec<(f32, f32)> = (0..=64).map(|i| (i as f32 / 64.0 * std::f32::consts::TAU).sin_cos()).map(|(s, c)| (c * 80.0, s * 80.0)).collect();
    let mut ringed = Document::new(300, 300, Some(WHITE));
    ringed.add_text_layer(TextSpec { text: "round and round and round we go ".into(), size: 24.0, path: ring, ..Default::default() }, (150, 150), &font);
    assert_eq!(ink(&ringed, IRect::new(110, 110, 190, 190)), 0);
    for r in [IRect::new(0, 100, 75, 200), IRect::new(225, 100, 300, 200), IRect::new(100, 0, 200, 75), IRect::new(100, 225, 200, 300)] {
        assert!(ink(&ringed, r) > 50, "text should pass through {r:?}");
    }

    // Smoothing keeps the end points and removes jitter.
    let wobble: Vec<(f32, f32)> = (0..50).map(|i| (i as f32 * 4.0, if i % 2 == 0 { 0.4 } else { -0.4 })).collect();
    let s = text::smooth_path(&wobble, 1.5);
    assert_eq!((s[0], *s.last().unwrap()), (wobble[0], wobble[49]));
    assert!(s.len() < 10 && s.iter().all(|p| p.1.abs() <= 0.5));

    // Saved files keep the text editable; painting on it turns it into pixels.
    let dir = std::env::temp_dir().join(format!("pf-text-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let before = doc.begin();
    doc.update_text(id, TextSpec { text: "two\nlines & \"quotes\"".into(), ..spec.clone() }, &font);
    doc.commit("Edit Text", before);
    io::save(&mut doc, &dir.join("t.ora")).unwrap();
    let back = io::open(&dir.join("t.ora")).unwrap();
    let (a, b) = (doc.state.layer(id).unwrap(), back.state.layers.last().unwrap());
    assert_eq!(a.text, b.text);
    assert_eq!(a.text_origin(), b.text_origin());
    std::fs::remove_dir_all(&dir).unwrap();

    let mut s = Stroke::begin(&mut doc, PaintKind::Brush(RED), brush(8.0), (10.0, 10.0)).unwrap();
    s.line_to(&mut doc, (40.0, 10.0));
    s.finish(&mut doc);
    assert!(doc.state.layer(id).unwrap().text.is_none());
    doc.undo();
    assert!(doc.state.layer(id).unwrap().text.is_some(), "undoing the stroke brings the editable text back");
}

#[test]
fn edge_detection_filter() {
    use pf_core::filter::{self, EdgeParams, EdgeStyle, Filter, FilterOp};
    // A black disc on white: the only edge is its outline.
    let disc = || {
        let mut doc = Document::new(120, 120, Some(WHITE));
        doc.select("c", &selection::ellipse_mask(120, 120, 30.0, 30.0, 90.0, 90.0), Combine::Replace);
        doc.fill([0, 0, 0, 255]);
        doc.deselect();
        doc
    };
    let mut doc = disc();
    assert!(filter::apply_filter(&mut doc, &Filter::EdgeDetect(EdgeParams::default())));
    assert_eq!(px(&doc, 60, 60), WHITE, "the flat inside of the disc has no edges");
    assert_eq!(px(&doc, 5, 5), WHITE);
    let dark = |doc: &Document, x: i32, y: i32| (-3..=3).any(|d| px(doc, x + d, y)[0] < 90 || px(doc, x, y + d)[0] < 90);
    for (x, y) in [(30, 60), (90, 60), (60, 30), (60, 90)] {
        assert!(dark(&doc, x, y), "expected a line on the outline near {x},{y}");
    }
    let lines = doc.state.layers[0].pixels.data.chunks_exact(4).filter(|p| p[0] < 128).count();
    assert!((120..700).contains(&lines), "a thin outline, not a filled shape: {lines} dark pixels");

    // Thicker lines cover more; highlight keeps the picture and traces it.
    let mut thick = disc();
    filter::apply_filter(&mut thick, &Filter::EdgeDetect(EdgeParams { thickness: 5.0, ..Default::default() }));
    assert!(thick.state.layers[0].pixels.data.chunks_exact(4).filter(|p| p[0] < 128).count() > lines * 3);
    let mut hl = disc();
    let p = EdgeParams { style: EdgeStyle::Highlight, color: [255, 0, 0, 255], thickness: 3.0, ..Default::default() };
    filter::apply_filter(&mut hl, &Filter::EdgeDetect(p));
    assert_eq!(px(&hl, 60, 60), [0, 0, 0, 255]);
    assert_eq!(px(&hl, 5, 5), WHITE);
    assert!((-3..=3).any(|d| px(&hl, 30 + d, 60) == [255, 0, 0, 255]), "outline should be traced in red");

    // Or turn the edges into a selection and leave the pixels alone.
    let mut doc = disc();
    let op = FilterOp::begin(&doc).unwrap();
    op.select_edges(&mut doc, &EdgeParams { thickness: 3.0, ..Default::default() });
    assert_eq!(px(&doc, 60, 60), [0, 0, 0, 255]);
    let sel = doc.state.selection.clone().expect("edges selected");
    assert_eq!(sel.px(60, 60)[0], 0);
    assert!((-3..=3).any(|d| sel.px(30 + d, 60)[0] > 200));
}

#[test]
fn ai_edit_round_trip() {
    use pf_core::aiedit;
    let mut doc = Document::new(400, 300, Some(WHITE));
    let base = doc.state.active;

    // Whole layer: everything is sent, nothing is masked, the answer covers it all.
    let job = aiedit::prepare(&doc.state, |_, _| (512, 384)).unwrap();
    assert_eq!((job.rect, job.image.w, job.image.h), (IRect::new(0, 0, 400, 300), 512, 384));
    assert!(job.mask.is_none());
    let id = doc.insert_ai_result(&job, &Pixmap::filled(512, 384, RED), "AI: rome");
    let l = doc.state.layer(id).unwrap();
    assert_eq!((l.rect(), l.name.as_str()), (IRect::new(0, 0, 400, 300), "AI: rome"));
    assert_eq!(doc.state.layers.iter().map(|l| l.id).collect::<Vec<_>>(), [base, id]);
    assert_eq!(px(&doc, 399, 299), RED);
    assert!(doc.undo());
    assert_eq!(doc.state.layers.len(), 1);

    // Selection: context around it is sent, the mask marks it, and only it is replaced.
    doc.select("r", &selection::rect_mask(400, 300, IRect::new(150, 100, 250, 200)), Combine::Replace);
    let job = aiedit::prepare(&doc.state, |w, h| (w * 2, h * 2)).unwrap();
    assert!(job.rect.contains_rect(IRect::new(110, 60, 290, 240)), "should include surroundings: {:?}", job.rect);
    let mask = job.mask.as_ref().unwrap();
    assert_eq!((mask.w, mask.h), (job.image.w, job.image.h));
    let to_img = |x: i32, y: i32| ((x - job.rect.x0) * 2, (y - job.rect.y0) * 2);
    let (inside, outside) = (to_img(200, 150), to_img(120, 70));
    assert_eq!(mask.px(inside.0, inside.1)[3], 0, "selected area is transparent in the mask");
    assert_eq!(mask.px(outside.0, outside.1)[3], 255);
    let id = doc.insert_ai_result(&job, &Pixmap::filled(job.image.w, job.image.h, RED), "AI: no dog");
    assert_eq!(px(&doc, 200, 150), RED);
    assert_eq!(px(&doc, 120, 70), WHITE, "outside the selection is untouched");
    assert_eq!(px(&doc, 20, 20), WHITE);
    let edge = px(&doc, 150, 150);
    assert!(edge != RED && edge != WHITE, "the selection edge is feathered: {edge:?}");
    let l = doc.state.layer(id).unwrap();
    assert!(l.pixels.w < 140 && l.pixels.h < 140, "new layer is trimmed to the selection");

    // Nothing to send when the selection misses the layer.
    doc.select("r", &selection::rect_mask(400, 300, IRect::new(0, 0, 10, 10)), Combine::Replace);
    let small = doc.add_image_layer("small", Pixmap::filled(20, 20, RED));
    doc.state.active = small;
    assert!(aiedit::prepare(&doc.state, |w, h| (w, h)).is_none());
}
