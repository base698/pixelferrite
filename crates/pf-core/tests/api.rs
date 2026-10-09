//! The JSON command API, driven the way an MCP client drives it.

use std::sync::Arc;

use base64::Engine;
use pf_core::api::{self, FontData, Host, Scratch};
use pf_core::{Document, composite, io};
use serde_json::{Value, json};

struct Fonts;
impl Host for Fonts {
    fn font(&mut self, _: &str, _: bool, _: bool) -> FontData {
        (Arc::new(epaint_default_fonts::UBUNTU_LIGHT.to_vec()), 0)
    }
    fn font_families(&mut self) -> Vec<String> {
        vec![]
    }
}

struct Session(Document, Scratch);
impl Session {
    fn new() -> Self {
        Session(Document::new(400, 300, Some([255; 4])), Scratch::default())
    }
    fn run(&mut self, c: Value) -> Result<Value, String> {
        api::execute(&mut self.0, &mut self.1, &mut Fonts, &c)
    }
    fn ok(&mut self, c: Value) -> Value {
        self.run(c.clone()).unwrap_or_else(|e| panic!("{c} failed: {e}"))
    }
    fn at(&self, x: i32, y: i32) -> [u8; 4] {
        composite::sample(&self.0.state, x, y).unwrap()
    }
}

#[test]
fn red_eye_from_commands() {
    let mut s = Session::new();
    // A face-coloured backdrop with a bright red pupil.
    s.ok(json!({"op": "fill", "color": "#d9a98c"}));
    s.ok(json!({"op": "select_ellipse", "rect": [180, 130, 220, 170]}));
    s.ok(json!({"op": "fill", "color": [230, 30, 40]}));
    s.ok(json!({"op": "deselect"}));
    assert_eq!(s.at(200, 150), [230, 30, 40, 255]);

    // An agent finds it by colour, then fixes just that box.
    let found = s.ok(json!({"op": "find_color", "color": "#e61e28", "tolerance": 30}));
    assert_eq!(found["bounds"], json!([180, 130, 220, 170]));
    assert!((found["centre"][0].as_f64().unwrap() - 200.0).abs() < 2.0);
    let fixed = s.ok(json!({"op": "red_eye", "rect": [170, 120, 230, 180]}));
    assert!(fixed["pixels_fixed"].as_u64().unwrap() > 1000);
    let pupil = s.at(200, 150);
    assert!(pupil[0] < 40 && pupil[0].abs_diff(pupil[1]) < 12, "pupil should be dark and neutral: {pupil:?}");
    assert_eq!(s.at(100, 100), [217, 169, 140, 255], "skin is left alone");
    s.ok(json!({"op": "undo"}));
    assert_eq!(s.at(200, 150), [230, 30, 40, 255]);
}

#[test]
fn layers_selection_paint_and_text() {
    let mut s = Session::new();
    let info = s.ok(json!({"op": "get_document_info"}));
    assert_eq!((info["width"].as_u64(), info["layers_bottom_first"].as_array().unwrap().len()), (Some(400), 1));
    let background = info["active_layer"].clone();

    let layer = s.ok(json!({"op": "add_layer", "name": "Paint"}))["layer"].clone();
    s.ok(json!({"op": "stroke", "tool": "brush", "points": [[50, 50], [350, 50]], "color": "#0000ff", "size": 20, "hardness": 1}));
    assert_eq!(s.at(200, 50), [0, 0, 255, 255]);
    assert_eq!(s.at(200, 100), [255, 255, 255, 255]);

    // "layer" on a command chooses what it works on, by id or by name.
    s.ok(json!({"op": "select_rect", "rect": [0, 200, 400, 300]}));
    s.ok(json!({"op": "fill", "color": "#00ff00", "layer": background}));
    assert_eq!(s.at(10, 250), [0, 255, 0, 255]);
    s.ok(json!({"op": "set_layer", "layer": "Paint", "opacity": 0.5, "blend": "multiply", "dy": 10}));
    let paint = s.ok(json!({"op": "get_layer_info", "layer": layer}));
    assert_eq!((paint["blend"].as_str(), paint["rect"][1].as_i64()), (Some("multiply"), Some(10)));
    assert_eq!(paint["painted_bounds"][1], json!(50), "the stroke's own extent, moved down with the layer");

    // Selections combine, feather and invert.
    s.ok(json!({"op": "select_rect", "rect": [0, 0, 100, 100]}));
    let both = s.ok(json!({"op": "select_ellipse", "rect": [300, 200, 400, 300], "mode": "add"}));
    assert_eq!(both["selection"]["bounds"], json!([0, 0, 400, 300]));
    let wand = s.ok(json!({"op": "select_wand", "x": 10, "y": 250, "all_layers": true}));
    assert_eq!(wand["selection"]["bounds"], json!([0, 200, 400, 300]));
    s.ok(json!({"op": "deselect"}));

    // Adjust changes colour inside the selection only.
    s.ok(json!({"op": "select_rect", "rect": [0, 200, 200, 300]}));
    s.ok(json!({"op": "adjust", "saturation": -1, "layer": background}));
    let (grey, green) = (s.at(50, 250), s.at(300, 250));
    assert!(grey[0] == grey[1] && grey[1] == grey[2], "{grey:?}");
    assert_eq!(green, [0, 255, 0, 255]);
    s.ok(json!({"op": "deselect"}));

    // Text is a layer that stays editable.
    let text = s.ok(json!({"op": "add_text", "text": "Hello", "x": 40, "y": 150, "size": 60, "color": "#aa0000"}));
    assert_eq!(text["text"]["text"], "Hello");
    let wide = s.ok(json!({"op": "set_text", "text": "Hello there", "size": 40}));
    assert_eq!((wide["text"]["text"].as_str(), wide["text"]["size"].as_f64()), (Some("Hello there"), Some(40.0)));
    let curved = s.ok(json!({"op": "add_text", "text": "round the bend", "size": 24, "path": [[20, 280], [200, 180], [380, 280]]}));
    assert_eq!(curved["text"]["follows_path"], true);

    // Filters by name; unknown ones say so.
    s.ok(json!({"op": "filter", "name": "gaussian_blur", "radius": 3, "layer": background}));
    assert!(s.run(json!({"op": "filter", "name": "heal"})).unwrap_err().contains("select"));
    assert!(s.run(json!({"op": "filter", "name": "sparkle"})).unwrap_err().contains("unknown filter"));

    // Locked layers refuse, with a way forward.
    s.ok(json!({"op": "set_layer", "layer": "Paint", "locked": true}));
    assert!(s.run(json!({"op": "fill", "color": "#000000"})).unwrap_err().contains("locked"));
}

#[test]
fn screenshot_files_and_batches() {
    let mut s = Session::new();
    s.ok(json!({"op": "select_rect", "rect": [100, 100, 200, 200]}));
    s.ok(json!({"op": "fill", "color": "#ff0000"}));

    // Whole canvas fits the size asked for; a small region is enlarged.
    let shot = s.ok(json!({"op": "get_canvas_screenshot", "max_size": 200}));
    assert_eq!((shot["width"].as_u64(), shot["height"].as_u64()), (Some(200), Some(150)));
    let png = base64::engine::general_purpose::STANDARD.decode(shot["png_base64"].as_str().unwrap()).unwrap();
    let px = io::decode_image(&png).unwrap();
    assert_eq!((px.w, px.h), (200, 150));
    let (inside, outside) = (px.px(75, 75), px.px(10, 10));
    assert!(inside[0] > 240 && inside[1] < 10, "selected red stays bright: {inside:?}");
    assert!(outside[0] < 140, "unselected white is dimmed: {outside:?}");
    let zoom = s.ok(json!({"op": "get_canvas_screenshot", "region": [140, 140, 160, 160], "max_size": 512, "show_selection": false}));
    assert_eq!((zoom["width"].as_u64(), zoom["scale"].as_f64()), (Some(160), Some(8.0)));

    // Save layered, export flat, open again.
    let dir = std::env::temp_dir().join(format!("pf-api-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (ora, png) = (dir.join("work.ora"), dir.join("out.png"));
    s.ok(json!({"op": "add_layer", "name": "Top"}));
    s.ok(json!({"op": "save", "path": ora}));
    s.ok(json!({"op": "export", "path": png}));
    assert!(s.run(json!({"op": "save", "path": "relative.ora"})).unwrap_err().contains("absolute"));
    assert!(s.run(json!({"op": "save", "path": png})).unwrap_err().contains(".ora"));
    let reopened = s.ok(json!({"op": "open", "path": ora}));
    assert_eq!(reopened["layers_bottom_first"].as_array().unwrap().len(), 2);
    assert_eq!(s.at(150, 150), [255, 0, 0, 255]);
    let flat = s.ok(json!({"op": "open", "path": png}));
    assert_eq!(flat["layers_bottom_first"][0]["name"], "out");
    let added = s.ok(json!({"op": "add_image", "path": png, "rect": [0, 0, 100, 75]}));
    assert_eq!(added["rect"], json!([0, 0, 100, 75]));

    // A batch stops at the first error and says how far it got.
    let e = s.run(json!({"op": "batch", "commands": [{"op": "deselect"}, {"op": "select_rect", "rect": [5, 5, 5, 9]}, {"op": "fill", "color": "#000"}]})).unwrap_err();
    assert!(e.contains("command 1 (select_rect)") && e.contains("The 1 before it"), "{e}");
    assert!(s.run(json!({"op": "nonsense"})).unwrap_err().contains("unknown op"));
    std::fs::remove_dir_all(dir).unwrap();
}
