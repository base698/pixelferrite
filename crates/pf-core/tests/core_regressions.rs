use std::sync::Arc;

use pf_core::{*, filter::{self, Filter}, paint::{BrushParams, PaintKind, Stroke}, ops::MergeError, text::TextSpec};

const RED: [u8; 4] = [255, 0, 0, 255];
const WHITE: [u8; 4] = [255; 4];

fn brush() -> BrushParams {
    BrushParams { size: 5.0, hardness: 1.0, opacity: 1.0, flow: 1.0, spacing: 0.1 }
}

fn sample(d: &Document) -> [u8; 4] {
    composite::sample(&d.state, 0, 0).unwrap()
}

#[test]
fn merge_rejects_backdrop_dependent_and_hidden_layers_without_changes() {
    let mut d = Document::new(1, 1, Some(RED));
    let middle = d.add_image_layer("Multiply", Pixmap::filled(1, 1, [128, 128, 128, 255]));
    d.state.layer_mut(middle).unwrap().blend = BlendMode::Multiply;
    let top = d.add_image_layer("White", Pixmap::filled(1, 1, WHITE));
    let before = (d.revision(), d.history().1, sample(&d));
    assert_eq!(d.merge_down(top), Err(MergeError::BackdropDependent));
    assert_eq!((d.revision(), d.history().1, sample(&d)), before);
    assert_eq!(d.state.layers.len(), 3);
    d.state.layer_mut(top).unwrap().visible = false;
    let before = sample(&d);
    assert_eq!(d.merge_down(top), Err(MergeError::Hidden));
    assert_eq!(sample(&d), before);
    assert_eq!(d.state.layers.len(), 3);
}

#[test]
fn merge_rejects_aggregate_budget_overflow_without_mutation() {
    // Shared raster storage keeps this valid 50-Mpixel document cheap to build.
    let base = Layer::new("Shared", Pixmap::filled(512, 1024, RED), 0, 0);
    let mut layers = (0..100).map(|_| {
        let mut layer = base.clone();
        layer.id = document::next_id();
        layer
    }).collect::<Vec<_>>();
    layers[98].x = -8192;
    layers[99].x = 7680;
    let active = layers[99].id;
    let mut d = Document::from_state(DocState {
        width: 512, height: 1024, layers, selection: None, active,
    });
    io::limits::validate_document(&d.state).unwrap();
    let before = d.begin();
    let revision = d.revision();
    let history = d.history().0.map(str::to_owned).collect::<Vec<_>>();
    let history_pos = d.history().1;

    // The merged raster fits by itself, but together with the other 98 layers
    // would raise the document to 65 Mpixels, beyond the 64-Mpixel budget.
    assert_eq!(d.merge_down(active), Err(MergeError::TooLarge));
    assert_eq!(d.revision(), revision);
    assert_eq!(d.history().1, history_pos);
    assert_eq!(d.history().0.collect::<Vec<_>>(), history.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!((d.state.width, d.state.height, d.state.active), (before.width, before.height, before.active));
    assert_eq!(d.state.layers.len(), before.layers.len());
    for (after, before) in d.state.layers.iter().zip(&before.layers) {
        assert_eq!((after.id, after.rect(), after.rev), (before.id, before.rect(), before.rev));
        assert!(Arc::ptr_eq(&after.pixels, &before.pixels));
        assert!(after.mask.is_none());
    }
}

#[test]
fn supported_merges_preserve_composites_and_undo() {
    // Every blend mode is safe when there is no additional backdrop.
    for mode in BlendMode::ALL {
        let mut d = Document::new(1, 1, Some([160, 70, 20, 190]));
        d.state.active_layer_mut().unwrap().blend = BlendMode::Screen;
        let top = d.add_image_layer("Top", Pixmap::filled(1, 1, [30, 120, 190, 160]));
        d.state.layer_mut(top).unwrap().blend = mode;
        let before = sample(&d);
        d.merge_down(top).unwrap();
        assert_eq!(sample(&d), before, "{mode:?}");
        assert_eq!(d.state.layers[0].blend, BlendMode::Normal);
        assert!(d.undo());
        assert_eq!(d.state.layers.len(), 2);
        assert_eq!(sample(&d), before);
        assert!(d.redo());
        assert_eq!(sample(&d), before);
    }
    // Normal layers are associative even above a separate backdrop.
    let mut d = Document::new(1, 1, Some(RED));
    d.add_image_layer("Gray", Pixmap::filled(1, 1, [90, 90, 90, 128]));
    let top = d.add_image_layer("White", Pixmap::filled(1, 1, [255, 255, 255, 80]));
    let before = sample(&d);
    d.merge_down(top).unwrap();
    assert!(sample(&d).iter().zip(before).all(|(a, b)| a.abs_diff(b) <= 1));
}

#[test]
fn editing_a_text_mask_preserves_origin_and_future_text_edits() {
    let font = text::FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    let mut d = Document::new(400, 300, None);
    let spec = TextSpec { text: "Hello".into(), size: 40.0, ..Default::default() };
    let id = d.add_text_layer(spec.clone(), (80, 150), &font);
    d.add_mask(id);
    let original_frame = d.state.layer(id).unwrap().rect();
    let stroke = Stroke::begin(&mut d, PaintKind::Brush([0, 0, 0, 255]), brush(), (90.0, 140.0)).unwrap();
    stroke.finish(&mut d);
    assert_eq!(d.state.layer(id).unwrap().text_origin(), Some((80, 150)));
    assert!(d.undo());
    assert_eq!(d.state.layer(id).unwrap().rect(), original_frame);
    assert_eq!(d.state.layer(id).unwrap().text_origin(), Some((80, 150)));
    assert!(d.redo());
    assert_eq!(d.state.layer(id).unwrap().text_origin(), Some((80, 150)));
    assert!(d.update_text(id, TextSpec { text: "Hello world".into(), ..spec }, &font));
    assert_eq!(d.state.layer(id).unwrap().text_origin(), Some((80, 150)));
}

#[test]
fn cancelling_a_stroke_restores_text_geometry_and_preserves_redo() {
    let font = text::FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    let mut d = Document::new(100, 100, None);
    let id = d.add_text_layer(TextSpec { text: "A".into(), size: 20.0, ..Default::default() }, (20, 50), &font);
    d.rename_layer(id, "Renamed");
    d.undo();
    let before = d.begin();
    let revision = d.revision();
    let history = d.history().0.map(str::to_owned).collect::<Vec<_>>();
    let stroke = Stroke::begin(&mut d, PaintKind::Brush(RED), brush(), (20.0, 40.0)).unwrap();
    stroke.cancel(&mut d);
    assert_eq!(d.revision(), revision);
    assert_eq!(d.state.layer(id).unwrap().rect(), before.layer(id).unwrap().rect());
    assert_eq!(d.state.layer(id).unwrap().text, before.layer(id).unwrap().text);
    assert_eq!(d.state.layer(id).unwrap().pixels.data, before.layer(id).unwrap().pixels.data);
    assert_eq!(d.history().0.collect::<Vec<_>>(), history.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(d.redo());
    assert_eq!(d.state.layer(id).unwrap().name, "Renamed");
}

#[test]
fn selected_blur_is_invariant_to_invisible_rgb() {
    let run = |hidden: [u8; 4]| {
        let mut p = Pixmap::new(2, 1);
        p.set(0, 0, RED);
        p.set(1, 0, hidden);
        let mut d = Document::from_pixmap(p, "Test");
        let mut sel = Mask::new(2, 1);
        sel.set(1, 0, [128]);
        d.set_selection("Partial", Some(sel));
        filter::apply_filter(&mut d, &Filter::GaussianBlur { radius: 1.0 });
        assert_eq!(d.state.active_layer().unwrap().pixels.px(0, 0), RED);
        d.state.active_layer().unwrap().pixels.px(1, 0)
    };
    let out = run([0, 0, 255, 0]);
    assert_eq!(out, run([0, 255, 0, 0]));
    assert_eq!(&out[..3], &RED[..3]);
    assert!((35..=40).contains(&out[3]));
}

#[test]
fn image_scaling_is_invariant_to_invisible_rgb() {
    let run = |hidden: [u8; 4]| {
        let mut p = Pixmap::new(2, 1);
        p.set(0, 0, RED);
        p.set(1, 0, hidden);
        let mut d = Document::new(1, 1, None);
        d.add_image_scaled("Scaled", &p, IRect::new(0, 0, 1, 1)).unwrap();
        sample(&d)
    };
    assert_eq!(run([0, 0, 255, 0]), [255, 0, 0, 128]);
    assert_eq!(run([0, 255, 0, 0]), [255, 0, 0, 128]);
}

#[test]
fn history_budget_releases_distinct_rasters_and_keeps_remaining_undo_valid() {
    let mut d = Document::new(4, 4, Some([128, 128, 128, 255]));
    d.set_history_byte_limit(64 * 2);
    let mut buffers = vec![Arc::downgrade(&d.state.active_layer().unwrap().pixels)];
    for i in 0..20 {
        let before = d.begin();
        Arc::make_mut(&mut d.state.active_layer_mut().unwrap().pixels).data[0] = i;
        d.commit("Change", before);
        buffers.push(Arc::downgrade(&d.state.active_layer().unwrap().pixels));
        assert!(d.history_bytes() <= 128);
    }
    assert_eq!(buffers.iter().filter(|w| w.strong_count() > 0).count(), 3);
    assert_eq!(d.history().1, 2);
    assert!(d.undo());
    assert_eq!(d.state.active_layer().unwrap().pixels.data[0], 18);
    assert!(d.undo());
    assert_eq!(d.state.active_layer().unwrap().pixels.data[0], 17);
    assert!(!d.undo());
    assert!(d.redo());
    assert!(d.redo());
    assert_eq!(d.state.active_layer().unwrap().pixels.data[0], 19);
    // Reducing the budget while undone must leave valid history navigation.
    d.undo();
    d.set_history_byte_limit(0);
    assert_eq!(d.history_bytes(), 0);
    assert!(!d.can_undo() && !d.can_redo());
    assert_eq!(d.state.active_layer().unwrap().pixels.data[0], 18);
}

#[test]
fn history_budget_counts_masks_selections_and_merged_state_edits() {
    let mut d = Document::new(4, 4, Some(WHITE));
    let id = d.state.active;
    d.add_mask(id);
    // Metadata-only snapshots share all live rasters and should be free.
    assert_eq!(d.history_bytes(), 0);
    d.rename_layer(id, "Name");
    assert_eq!(d.history_bytes(), 0);
    d.set_history_byte_limit(16);
    d.invert_mask(id);
    assert_eq!(d.history_bytes(), 16);
    d.invert_mask(id);
    assert_eq!(d.history_bytes(), 16);
    d.set_selection("First", Some(Mask::filled(4, 4, [255])));
    d.set_selection("Second", Some(Mask::filled(4, 4, [128])));
    assert!(d.history_bytes() <= 16);
    let before = d.begin();
    Arc::make_mut(&mut d.state.active_layer_mut().unwrap().pixels).data[0] = 12;
    d.commit_merged("Large", before, 77);
    assert!(d.history_bytes() <= 16);
    assert_eq!(d.state.active_layer().unwrap().pixels.data[0], 12);
}

#[test]
fn history_jump_does_not_redo_when_budget_eviction_shifts_indices() {
    let mut d = Document::new(4, 4, Some(WHITE));
    d.set_history_byte_limit(1024);
    for value in [1, 2] {
        let before = d.begin();
        Arc::make_mut(&mut d.state.active_layer_mut().unwrap().pixels).data[0] = value;
        d.commit("Paint", before);
    }
    let before = d.begin();
    d.state.active_layer_mut().unwrap().pixels = Arc::new(Pixmap::filled(16, 16, RED));
    d.commit("Resize", before);
    assert_eq!(d.history().1, 3);
    d.jump_to(2);
    assert_eq!(d.state.active_layer().unwrap().pixels.w, 4);
    assert_eq!(d.state.active_layer().unwrap().pixels.data[0], 2);
    assert!(d.history_bytes() <= 1024);
    assert!(d.redo());
    assert_eq!(d.state.active_layer().unwrap().pixels.w, 16);
}

#[test]
fn locked_layers_reject_content_and_structural_edits() {
    let mut d = Document::new(3, 1, None);
    Arc::make_mut(&mut d.state.active_layer_mut().unwrap().pixels).set(0, 0, RED);
    let id = d.state.active;
    d.add_mask(id);
    d.state.active_layer_mut().unwrap().locked = true;
    let before = d.begin();
    let history = d.history().1;
    d.flip_layer(id, true);
    d.invert_mask(id);
    d.apply_mask(id);
    d.delete_mask(id);
    assert!(!d.fill(WHITE));
    assert!(d.cut().is_none());
    assert!(!filter::apply_filter(&mut d, &Filter::Threshold { level: 128.0 }));
    assert_eq!(d.state.layer(id).unwrap().pixels.data, before.layer(id).unwrap().pixels.data);
    assert_eq!(d.state.layer(id).unwrap().mask.as_ref().unwrap().data, before.layer(id).unwrap().mask.as_ref().unwrap().data);
    assert_eq!(d.history().1, history);
    let top = d.add_empty_layer();
    assert_eq!(d.merge_down(top), Err(MergeError::Locked));
    d.delete_layer(id);
    d.move_layer_to(id, 1);
    d.flatten_image();
    assert_eq!(d.state.layers.len(), 2);
    assert_eq!(d.state.layers[0].id, id);
}

#[test]
fn cut_copies_the_mask_when_the_mask_is_selected() {
    let mut d = Document::new(2, 1, Some(RED));
    let id = d.state.active;
    d.add_mask(id);
    Arc::make_mut(d.state.active_layer_mut().unwrap().mask.as_mut().unwrap()).set(0, 0, [90]);
    let clip = d.cut().unwrap();
    assert_eq!(clip.pixels.px(0, 0), [90, 90, 90, 255]);
    assert_eq!(clip.pixels.px(1, 0), WHITE);
    assert_eq!(d.state.active_layer().unwrap().pixels.px(0, 0), RED);
    assert_eq!(d.state.active_layer().unwrap().mask.as_ref().unwrap().px(0, 0), [0]);
    assert!(d.undo());
    assert_eq!(d.state.active_layer().unwrap().mask.as_ref().unwrap().px(0, 0), [90]);
}

#[test]
fn content_aware_scale_reaches_one_pixel_and_keeps_mask_in_sync() {
    let mut d = Document::new(3, 3, Some(RED));
    let id = d.state.active;
    d.add_mask(id);
    assert!(d.content_aware_scale(1, 1));
    assert_eq!((d.state.width, d.state.height), (1, 1));
    let l = d.state.active_layer().unwrap();
    assert_eq!((l.pixels.w, l.pixels.h), (1, 1));
    assert_eq!((l.mask.as_ref().unwrap().w, l.mask.as_ref().unwrap().h), (1, 1));
    assert_eq!(sample(&d), RED);
    assert!(d.undo());
    assert_eq!((d.state.width, d.state.height), (3, 3));
}

#[test]
fn oversized_layer_growth_is_refused_without_partial_changes() {
    let mut d = Document::new(2, 2, None);
    let layer = d.state.active_layer_mut().unwrap();
    let rect = layer.rect();
    let rev = layer.rev;
    assert!(!layer.ensure_covers(IRect::new(-1_000_000, 0, 2, 2)));
    assert_eq!(layer.rect(), rect);
    assert_eq!(layer.rev, rev);
    layer.x = 20_000;
    assert!(Stroke::begin(&mut d, PaintKind::Brush(RED), brush(), (0.0, 0.0)).is_none());
    assert_eq!(d.state.active_layer().unwrap().x, 20_000);
}

#[test]
fn scaled_imports_reject_extreme_dimensions_before_allocating() {
    let mut d = Document::new(1, 1024, None);
    let pixels = Pixmap::new(1024, 1);
    let history = d.history().1;
    assert!(d.add_image_scaled("Huge", &pixels, IRect::new(0, 0, i32::MAX, i32::MAX)).is_err());
    assert_eq!(d.history().1, history);
    d.select_all();
    // Filling this tall, narrow selection would otherwise scale the source
    // into a million-pixel-wide intermediate before cropping it.
    assert!(d.add_image_in_selection("Huge intermediate", &pixels).is_none());
    assert_eq!(d.state.layers.len(), 1);
}

#[test]
fn checked_layer_insertion_rejects_excess_layers_before_commit() {
    let mut d = Document::new(1, 1, None);
    for _ in 1..io::limits::MAX_LAYERS {
        d.add_empty_layer();
    }
    let revision = d.revision();
    assert!(d.check_layer_capacity(1, 1).is_err());
    assert!(d.try_insert_layer("Too many", Layer::new("Extra", Pixmap::new(1, 1), 0, 0)).is_err());
    assert_eq!(d.revision(), revision);
    assert_eq!(d.state.layers.len(), io::limits::MAX_LAYERS);
}

#[test]
fn document_revision_tracks_commits_merged_edits_undo_and_redo() {
    let mut d = Document::new(1, 1, Some(RED));
    let mut previous = d.revision();
    for name in ["First", "Second"] {
        let before = d.begin();
        d.state.active_layer_mut().unwrap().name = name.into();
        d.commit_merged("Rename", before, 3);
        assert!(d.revision() > previous);
        previous = d.revision();
    }
    assert_eq!(d.history().1, 1);
    assert!(d.undo());
    assert!(d.revision() > previous);
    previous = d.revision();
    assert!(d.redo());
    assert!(d.revision() > previous);
}

#[test]
fn persisted_ai_result_layer_matches_inserted_pixels_and_placement() {
    let mut d = Document::new(80, 60, Some(WHITE));
    d.set_selection("Area", Some(selection::rect_mask(80, 60, IRect::new(20, 15, 40, 35))));
    let job = aiedit::prepare(&d.state, aiedit::Source::Visible, |w, h| (w, h)).unwrap();
    let output = Pixmap::filled(job.image.w, job.image.h, RED);
    let persisted = job.clone().result_layer(&output, "AI");
    let id = d.insert_ai_result(&job, &output, "AI");
    let inserted = d.state.layer(id).unwrap();
    assert_eq!(persisted.rect(), inserted.rect());
    assert_eq!(persisted.pixels.data, inserted.pixels.data);
    assert!(persisted.pixels.data.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
}

#[test]
fn fontations_preserves_baseline_metrics_and_text_frames() {
    let font = text::FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    // Values captured from the editor's previous parser using this bundled font.
    assert_eq!(font.units_per_em(), Some(1000.0));
    assert_eq!(font.h_advance_unscaled(font.glyph_id('A')), 641.0);
    assert_eq!(font.h_advance_unscaled(font.glyph_id('V')), 624.0);
    assert_eq!(font.kern_unscaled(font.glyph_id('A'), font.glyph_id('V')), 0.0);
    for (content, width, height, offset, coverage) in [
        ("A", 28, 30, (-1, -29), 45383u64),
        ("AV", 53, 30, (-1, -29), 83624),
        ("Hello", 91, 35, (2, -33), 165017),
        ("Hello\nworld", 102, 83, (-1, -33), 335609),
    ] {
        let spec = TextSpec { text: content.into(), size: 40.0, ..Default::default() };
        let bounds = text::render_bounds(&spec, &font).unwrap();
        assert_eq!(bounds, IRect::xywh(offset.0, offset.1, width, height), "{content:?}");
        let (px, got_offset) = text::render(&spec, &font);
        assert_eq!((px.w, px.h, got_offset), (width, height, offset));
        let got_coverage: u64 = px.data.chunks_exact(4).map(|p| p[3] as u64).sum();
        assert!(got_coverage.abs_diff(coverage) < 500, "{content:?}: {got_coverage} vs {coverage}");
    }
    assert!(text::FontRef::try_from_slice(b"not a font").is_err());
    assert!(text::FontRef::try_from_slice_and_index(epaint_default_fonts::UBUNTU_LIGHT, 1).is_err());
}

#[test]
fn oversized_text_is_rejected_before_rasterizing_or_changing_the_layer() {
    let font = text::FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    let mut d = Document::new(20, 20, None);
    let id = d.try_add_text_layer(TextSpec { text: "A".into(), size: 10.0, ..Default::default() }, (2, 12), &font).unwrap();
    let before = d.state.layer(id).unwrap().clone();
    let too_wide = TextSpec { text: "A".repeat(128), size: 4096.0, ..Default::default() };
    assert!(text::render_bounds(&too_wide, &font).is_none());
    assert!(d.try_add_text_layer(too_wide.clone(), (0, 0), &font).is_err());
    assert!(!d.update_text(id, too_wide, &font));
    assert_eq!(d.state.layer(id).unwrap().pixels.data, before.pixels.data);
    assert_eq!(d.state.layer(id).unwrap().text, before.text);
    let invalid = TextSpec { text: "A".into(), size: f32::INFINITY, ..Default::default() };
    assert!(text::render_bounds(&invalid, &font).is_none());
    assert!(d.try_add_text_layer(invalid, (0, 0), &font).is_err());
}
