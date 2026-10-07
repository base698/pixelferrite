//! Offline AI integration tests. Servers bind only to loopback with fixture keys.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use pf_core::{Document, composite, selection};

fn harness() -> Harness<'static, App> {
    Harness::builder().with_size(vec2(1400.0, 880.0)).build_eframe(|cc| {
        let mut app = App::new(cc, None);
        app.doc = Document::new(128, 128, Some([255; 4]));
        app
    })
}

fn queue_answer(app: &mut App) {
    let job = aiedit::prepare(&app.doc.state, Source::Visible, |w, h| (w, h)).unwrap();
    let (tx, rx) = channel();
    tx.send(Ok(Completed { pixels: Pixmap::filled(16, 16, [0, 200, 0, 255]), history_error: None })).unwrap();
    app.ai.run = Some(Run { rx, job, prompt: "fixture".into(), started: 0.0, cancelled: Arc::new(AtomicBool::new(false)), pending: None });
}

#[test]
fn completed_answer_waits_for_preview_and_drag() {
    let mut h = harness();
    let ctx = h.ctx.clone();
    let app = h.state_mut();
    queue_answer(app);
    app.open_perspective();
    assert!(app.warp.is_some());
    app.ai_poll(&ctx);
    assert!(app.ai.run.is_some());
    assert_eq!(app.doc.state.layers.len(), 1);
    app.warp.take().unwrap().op.cancel(&mut app.doc);
    let before = app.doc.begin();
    app.drag = crate::canvas::Drag::Move { before, start: egui::Pos2::ZERO, orig: (0, 0), moved: false };
    app.ai_poll(&ctx);
    assert!(app.ai.run.is_some());
    crate::canvas::cancel(app);
    app.ai_poll(&ctx);
    assert!(app.ai.run.is_none());
    assert_eq!(app.doc.state.layers.len(), 2);
    assert_eq!(app.doc.history().0.last(), Some("AI Edit"));
}

#[test]
fn ready_answer_survives_a_full_document_until_retry() {
    let mut h = harness();
    let ctx = h.ctx.clone();
    let app = h.state_mut();
    queue_answer(app);
    for _ in 1..io::limits::MAX_LAYERS {
        app.doc.state.layers.push(Layer::new("capacity fixture", Pixmap::new(1, 1), 0, 0));
    }
    app.ai_poll(&ctx);
    assert!(app.ai.run.as_ref().unwrap().pending.is_some());
    assert_eq!(app.doc.state.layers.len(), io::limits::MAX_LAYERS);
    app.doc.state.layers.pop();
    h.run_steps(2);
    h.get_by_label("Add Result").click();
    h.run_steps(2);
    assert!(h.state().ai.run.is_none());
    assert_eq!(h.state().doc.state.layers.len(), io::limits::MAX_LAYERS);
    assert_eq!(h.state().doc.state.active_layer().unwrap().name, "AI: fixture");
}

#[test]
fn changed_endpoint_cannot_reauthorize_an_existing_key_by_saving_settings() {
    let mut h = harness();
    let app = h.state_mut();
    let mut settings = app.saved.clone();
    settings.ai.api_key = "fixture-key-for-openai".into();
    settings.ai.base_url = "https://changed.example/v1".into();
    app.store.save_settings(&settings).unwrap();
    app.open_settings();
    assert!(app.ai.settings.as_ref().unwrap().edit.ai.api_key.is_empty(), "a new destination requires a freshly entered key");
}

#[test]
fn history_restores_exact_clipped_pixels_and_position() {
    let mut h = harness();
    let app = h.state_mut();
    app.doc.set_selection("Select", Some(selection::ellipse_mask(128, 128, 50.0, 50.0, 78.0, 78.0)));
    let job = aiedit::prepare(&app.doc.state, Source::Visible, |w, h| (w, h)).unwrap();
    let answer = Pixmap::filled(32, 32, [0, 200, 0, 255]);
    let layer = job.result_layer(&answer, "fixture");
    let r = AiRecord {
        id: "history-fixture".into(), selection: true, result_origin: Some([layer.x, layer.y]),
        source: "visible".into(), document_session: app.ai.document_session, canvas_size: [128, 128],
        ..Default::default()
    };
    app.store.write_ai_record(&r).unwrap();
    app.store.write_ai_file(&r.id, "result.png", &io::encode_png(&layer.pixels).unwrap()).unwrap();
    app.doc.insert_ai_result(&job, &answer, "fixture");
    let expected = app.doc.state.layers.last().unwrap().clone();
    let composite_before = composite::flatten(&app.doc.state);
    app.doc.undo();
    app.doc.deselect();
    app.restore_ai_record(&r).unwrap();
    let actual = app.doc.state.layers.last().unwrap();
    assert_eq!((actual.x, actual.y, &actual.pixels.data), (expected.x, expected.y, &expected.pixels.data));
    assert_eq!(composite::flatten(&app.doc.state).data, composite_before.data);
    assert_eq!(composite::sample(&app.doc.state, 8, 8), Some([255; 4]));

    app.open_perspective();
    assert!(app.restore_ai_record(&r).is_err(), "history insertion must not bypass the mutation gate");
}

#[test]
fn legacy_masked_history_never_inserts_unclipped_context() {
    let mut h = harness();
    let app = h.state_mut();
    let r = AiRecord { id: "legacy-fixture".into(), version: 1, selection: true, ..Default::default() };
    app.store.write_ai_record(&r).unwrap();
    app.store.write_ai_file(&r.id, "output.png", &io::encode_png(&Pixmap::filled(16, 16, [0, 200, 0, 255])).unwrap()).unwrap();
    assert!(app.restore_ai_record(&r).unwrap_err().contains("older request"));
    assert_eq!(app.doc.state.layers.len(), 1);
}

#[test]
fn requests_respect_history_off_and_persist_exact_results() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    use base64::Engine;
    for (keep, clear_in_flight) in [(0, false), (2, false), (2, true)] {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (answer_ready, answer_gate) = channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut request = Vec::new();
        loop {
            let mut buf = [0; 65536];
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
            if let Some(end) = text.find("\r\n\r\n") {
                let len: usize = text.split("content-length:").nth(1).unwrap().lines().next().unwrap().trim().parse().unwrap();
                if request.len() >= end + 4 + len { break; }
            }
        }
        answer_gate.recv().unwrap();
        let png = io::encode_png(&Pixmap::filled(16, 16, [0, 200, 0, 255])).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let body = format!("{{\"data\":[{{\"b64_json\":\"{encoded}\"}}]}}");
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let mut h = harness();
    let ctx = h.ctx.clone();
    h.state_mut().saved.ai.keep_history = keep;
    h.state_mut().doc.set_selection("Select", Some(selection::ellipse_mask(128, 128, 40.0, 40.0, 80.0, 80.0)));
    h.state_mut().send_to_ai(&ctx, "fixture".into(), ai::Config { key: Some("fixture-key".into()), model: "gpt-image-2".into(), base, quality: None, key_source: "test".into() }, Source::Visible);
    if clear_in_flight {
        assert_eq!(h.state().store.ai_records().len(), 1);
        h.state().store.clear_ai().unwrap();
    }
    answer_ready.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.state().ai.run.is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        h.state_mut().ai_poll(&ctx);
    }
    server.join().unwrap();
    assert!(h.state().ai.run.is_none());
    assert_eq!(h.state().doc.state.layers.len(), 2);
    if keep == 0 || clear_in_flight {
        assert!(!h.state().store.ai_dir().exists(), "history-disabled requests must never create disk artifacts");
    } else {
        let records = h.state().store.ai_records();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        let layer = h.state().doc.state.active_layer().unwrap();
        assert_eq!(record.result_origin, Some([layer.x, layer.y]));
        let saved = io::load_pixmap(&h.state().store.ai_path(&record.id).join("result.png")).unwrap();
        assert_eq!(saved.data, layer.pixels.data, "persist the exact clipped and feathered layer");
        for file in ["input.png", "mask.png", "output.png", "result.png"] {
            assert!(h.state().store.ai_path(&record.id).join(file).is_file());
        }
    }
    }
}
