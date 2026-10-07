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
    doc.merge_down(pasted).unwrap();
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
        let imported = io::open(&p).unwrap();
        assert_eq!((imported.state.width, imported.state.height), (48, 32));
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
    use pf_core::text::FontRef;
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
    use pf_core::aiedit::{self, Source};
    let mut doc = Document::new(400, 300, Some(WHITE));
    let base = doc.state.active;

    // Whole layer: everything is sent, nothing is masked, the answer covers it all.
    let job = aiedit::prepare(&doc.state, Source::Layer, |_, _| (512, 384)).unwrap();
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
    let job = aiedit::prepare(&doc.state, Source::Layer, |w, h| (w * 2, h * 2)).unwrap();
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

    // "What you see": layers above and below are in the picture that is
    // sent, and the answer goes on top of all of them.
    let mut doc = Document::new(200, 100, Some(WHITE));
    let art = doc.add_image_layer("art", Pixmap::filled(100, 100, RED));
    doc.state.layer_mut(art).unwrap().x = 0;
    doc.state.active = doc.state.layers[0].id;
    doc.select("r", &selection::rect_mask(200, 100, IRect::new(100, 0, 200, 100)), Combine::Replace);
    let only = aiedit::prepare(&doc.state, Source::Layer, |w, h| (w, h)).unwrap();
    assert!(only.image.data.chunks_exact(4).all(|p| p == WHITE), "the layer alone is blank white");
    let job = aiedit::prepare(&doc.state, Source::Visible, |w, h| (w, h)).unwrap();
    assert_eq!(job.rect, IRect::new(52, 0, 200, 100));
    assert_eq!((job.image.px(10, 50), job.image.px(140, 50)), (RED, WHITE), "the art next to the selection is sent as context");
    let id = doc.insert_ai_result(&job, &Pixmap::filled(job.image.w, job.image.h, [0, 0, 255, 255]), "AI");
    assert_eq!(doc.state.layers.last().unwrap().id, id, "result sits above every layer");
    assert_eq!((px(&doc, 150, 50), px(&doc, 50, 50)), ([0, 0, 255, 255], RED));

    // Nothing to send when the selection misses the layer.
    doc.select("r", &selection::rect_mask(400, 300, IRect::new(0, 0, 10, 10)), Combine::Replace);
    let small = doc.add_image_layer("small", Pixmap::filled(20, 20, RED));
    doc.state.active = small;
    assert!(aiedit::prepare(&doc.state, Source::Layer, |w, h| (w, h)).is_none());
}

/// A white canvas with a red disc and some texture, for filter tests.
fn scene() -> Document {
    let mut px = Pixmap::filled(160, 120, WHITE);
    for y in 0..120 {
        for x in 0..160 {
            let n = ((x * 37 + y * 91) % 23) as u8;
            let base: [u8; 3] = if (x - 60) * (x - 60) + (y - 60) * (y - 60) < 900 { [200, 40, 40] } else { [120, 130, 140] };
            px.set(x, y, [base[0] + n, base[1] + n, base[2] + n, 255]);
        }
    }
    Document::from_pixmap(px, "scene")
}

#[test]
fn tone_and_repair_filters() {
    use pf_core::filter::{self, Filter};
    let spread = |doc: &Document| {
        let d = &doc.state.layers[0].pixels.data;
        let lum: Vec<f32> = d.chunks_exact(4).map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).collect();
        (lum.iter().cloned().fold(f32::MAX, f32::min), lum.iter().cloned().fold(f32::MIN, f32::max))
    };
    let noise = |doc: &Document| {
        // Mean difference between horizontal neighbours in a flat grey corner.
        let p = &doc.state.layers[0].pixels;
        (100..150).map(|x| (p.px(x, 100)[1] as f32 - p.px(x + 1, 100)[1] as f32).abs()).sum::<f32>() / 50.0
    };
    let run = |f: Filter| {
        let mut doc = scene();
        assert!(filter::apply_filter(&mut doc, &f), "{f:?}");
        assert_eq!(doc.history().0.last(), Some(f.name()));
        doc
    };
    let plain = scene();
    let (lo, hi) = spread(&plain);
    assert!(lo > 60.0 && hi < 200.0);

    let (alo, ahi) = spread(&run(Filter::AutoContrast { clip: 0.5 }));
    assert!(ahi > 243.0 && ahi - alo > hi - lo + 30.0, "auto contrast should stretch the range: {alo}..{ahi}");
    let (elo, ehi) = spread(&run(Filter::Equalize { amount: 1.0 }));
    assert!(ehi - elo > hi - lo + 40.0);
    let (clo, chi) = spread(&run(Filter::LocalContrast { clip: 3.0, tiles: 4 }));
    assert!(chi - clo > hi - lo, "local contrast widens the range: {clo}..{chi}");

    for f in [Filter::Threshold { level: 110.0 }, Filter::AdaptiveThreshold { radius: 12.0, offset: 4.0 }] {
        let doc = run(f);
        assert!(doc.state.layers[0].pixels.data.chunks_exact(4).all(|p| (p[0] == 0 || p[0] == 255) && p[0] == p[1] && p[3] == 255), "{f:?} gives pure black and white");
    }
    let t = run(Filter::Threshold { level: 110.0 });
    assert_eq!((px(&t, 60, 60)[0], px(&t, 140, 20)[0]), (0, 255), "dark red disc below the level, grey above");

    assert!(noise(&run(Filter::UnsharpMask { radius: 1.5, amount: 1.5, threshold: 0.0 })) > noise(&plain) * 1.5);
    for f in [Filter::SurfaceBlur { radius: 4.0, tolerance: 30.0 }, Filter::Denoise { strength: 25.0 }] {
        let doc = run(f);
        assert!(noise(&doc) < noise(&plain) * 0.6, "{f:?} should smooth the grain: {} vs {}", noise(&doc), noise(&plain));
        let (a, b) = (px(&doc, 60, 60), px(&doc, 140, 60));
        assert!(a[0] > 170 && a[1] < 90 && b[0] < 160, "{f:?} keeps the disc distinct from the background");
    }

    // Matching colours moves the average towards the reference.
    let (mean, _) = pf_core::fx::color_stats(&Pixmap::filled(8, 8, [40, 60, 200, 255])).unwrap();
    let m = run(Filter::MatchColors { mean, dev: [40.0, 10.0, 10.0], amount: 1.0 });
    let p = px(&m, 140, 20);
    assert!(p[2] > p[0] + 60, "grey should have turned blue: {p:?}");

    // Lens distortion moves the picture but keeps its centre.
    let l = run(Filter::LensDistortion { amount: 0.5 });
    assert_ne!(l.state.layers[0].pixels.data, plain.state.layers[0].pixels.data);
    assert!(px(&l, 80, 60)[0].abs_diff(px(&plain, 80, 60)[0]) < 30);

    // Healing needs a selection, and fills it from around it.
    let mut doc = scene();
    doc.select("c", &selection::ellipse_mask(160, 120, 24.0, 24.0, 96.0, 96.0), Combine::Replace);
    assert!(filter::apply_filter(&mut doc, &Filter::Inpaint { radius: 6.0 }));
    let healed = px(&doc, 60, 60);
    assert!(healed[0] < 160 && healed[0].abs_diff(healed[2]) < 40, "the red disc should be gone: {healed:?}");
    assert_eq!(px(&doc, 150, 10), px(&plain, 150, 10), "outside the selection is untouched");
}

#[test]
fn geometry() {
    use pf_core::transform::{self, PerspectiveMode, PerspectiveOp};
    let h = transform::homography([(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], [(0.0, 0.0), (20.0, 0.0), (20.0, 5.0), (0.0, 5.0)]).unwrap();
    assert!((h[0] - 2.0).abs() < 1e-6 && (h[4] - 0.5).abs() < 1e-6 && h[6].abs() < 1e-9);
    assert!(transform::homography([(0.0, 0.0); 4], [(0.0, 0.0); 4]).is_none());

    // Distort: pull the top-right corner in; the layer becomes a trapezoid.
    let mut doc = Document::new(200, 200, None);
    let id = doc.add_image_layer("sq", Pixmap::filled(100, 100, RED));
    let mut op = PerspectiveOp::begin(&doc).unwrap();
    assert_eq!(op.corners(), [(50.0, 50.0), (150.0, 50.0), (150.0, 150.0), (50.0, 150.0)]);
    let quad = [(50.0, 50.0), (150.0, 90.0), (150.0, 150.0), (50.0, 150.0)];
    assert!(op.update(&mut doc, quad, PerspectiveMode::Distort));
    assert_eq!(px(&doc, 60, 60), RED);
    assert_eq!(px(&doc, 140, 60)[3], 0, "the corner that moved down leaves a gap above it");
    assert_eq!(px(&doc, 140, 120), RED);
    op.finish(&mut doc);
    assert_eq!(doc.history().0.last(), Some("Perspective"));
    // Straighten: marking that same trapezoid squares it back up.
    let mut op = PerspectiveOp::begin(&doc).unwrap();
    assert!(op.update(&mut doc, quad, PerspectiveMode::Straighten));
    let l = doc.state.layer(id).unwrap();
    assert_eq!(l.rect(), IRect::new(50, 50, 150, 150));
    assert_eq!((px(&doc, 140, 60), px(&doc, 60, 140)), (RED, RED));
    op.cancel(&mut doc);
    assert_eq!(px(&doc, 140, 60)[3], 0);

    // Content-aware scale: the plain background goes, the detailed stripe stays.
    let mut img = Pixmap::filled(120, 40, WHITE);
    for y in 0..40 {
        for x in 50..70 {
            img.set(x, y, if (x + y) % 2 == 0 { RED } else { [0, 0, 255, 255] });
        }
    }
    let mut doc = Document::from_pixmap(img, "s");
    assert!(doc.content_aware_scale(80, 40));
    assert_eq!((doc.state.width, doc.state.height), (80, 40), "a single full-canvas layer takes the canvas with it");
    let busy = doc.state.layers[0].pixels.data.chunks_exact(4).filter(|p| p[..3] != [255, 255, 255]).count();
    assert_eq!(busy, 20 * 40, "none of the detailed stripe should have been removed");
    assert!(doc.content_aware_scale(80, 30));
    assert_eq!(doc.state.height, 30);
    assert!(!doc.content_aware_scale(200, 200), "it never enlarges");
}

#[test]
fn smart_selection() {
    use pf_core::segment;
    // A red disc on blue-grey: boxing it selects the disc, not the box.
    let img = scene().state.layers[0].pixels.clone();
    let m = segment::grabcut(&img, IRect::new(20, 20, 100, 100));
    assert!(m.px(60, 60)[0] > 200, "disc centre is subject");
    assert!(m.px(24, 24)[0] < 50 && m.px(96, 96)[0] < 50, "box corners are background");
    assert_eq!(m.px(140, 60)[0], 0);
    let area = m.data.iter().filter(|v| **v > 127).count() as f32;
    let disc = std::f32::consts::PI * 30.0 * 30.0;
    assert!((area - disc).abs() < disc * 0.2, "selected {area} px, the disc is {disc}");

    // Regions follow edges: the disc and the background are different regions.
    let mut flat = Pixmap::filled(160, 120, WHITE);
    for y in 0..120 {
        for x in 0..160 {
            if (x - 60) * (x - 60) + (y - 60) * (y - 60) < 900 {
                flat.set(x, y, RED);
            }
        }
    }
    let r = segment::watershed(&flat, 0.5);
    let (inside, outside) = (r.label_at(60.0, 60.0).unwrap(), r.label_at(140.0, 20.0).unwrap());
    assert_ne!(inside, outside);
    assert_eq!(r.label_at(70.0, 55.0), Some(inside));
    assert!(r.label_at(500.0, 5.0).is_none());
    let mask = r.mask(&[inside], 160, 120);
    assert!(mask.px(60, 60)[0] == 255 && mask.px(140, 20)[0] == 0);
    let got = mask.data.iter().filter(|v| **v > 0).count() as f32;
    assert!((got - disc).abs() < disc * 0.35, "region {got} px vs disc {disc}");

    // Contours: a rectangle's outline is one clockwise loop of its perimeter.
    let sel = selection::rect_mask(50, 40, IRect::new(10, 5, 30, 25));
    let loops = segment::contours(&sel);
    assert_eq!(loops.len(), 1);
    let l = &loops[0];
    assert_eq!(l.len(), 80);
    let area2: f32 = (0..l.len()).map(|i| { let (a, b) = (l[i], l[(i + 1) % l.len()]); a.0 * b.1 - b.0 * a.1 }).sum();
    assert_eq!(area2, 800.0, "positive area means clockwise with y pointing down");
    let both = selection::combine(Some(&sel), &selection::rect_mask(50, 40, IRect::new(40, 30, 45, 35)), Combine::Add).unwrap();
    let loops = segment::contours(&both);
    assert_eq!((loops.len(), loops[0].len(), loops[1].len()), (2, 80, 20));
}

#[test]
fn ai_edit_fills_empty_canvas() {
    use pf_core::aiedit::{Source, prepare};
    // A red block in the middle of an otherwise empty canvas.
    let mut doc = Document::new(300, 100, None);
    doc.add_image_scaled("block", &Pixmap::filled(10, 10, [200, 30, 30, 255]), IRect::new(100, 0, 200, 100)).unwrap();
    let job = prepare(&doc.state, Source::Visible, |w, h| (w, h)).unwrap();
    assert!(job.image.data.chunks_exact(4).all(|p| p[3] == 255), "nothing see-through is sent");
    assert_eq!(job.image.px(150, 50), [200, 30, 30, 255], "what was there is untouched");
    let edge = job.image.px(5, 50);
    assert!(edge[0] > 150 && edge[1] < 80, "empty space takes on the nearby colour: {edge:?}");
}
