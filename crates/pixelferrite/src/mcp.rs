//! `pixelferrite mcp`: a Model Context Protocol server on stdin/stdout. It
//! looks at the document, takes screenshots and runs commands. It drives the
//! running app when there is one, and otherwise works on a document of its
//! own with no window.

use std::io::Write;

use pf_core::aiedit::{self, Source};
use pf_core::api::{self, Scratch};
use pf_core::{Document, io};
use serde_json::{Value, json};

use crate::ai;
use crate::fonts::Fonts;
use crate::store::Store;

/// The document used when no window is being driven.
struct Session {
    doc: Document,
    scratch: Scratch,
    fonts: Fonts,
}

impl Session {
    fn execute(&mut self, cmd: &Value) -> Result<Value, String> {
        api::validate_request(cmd)?;
        self.run(cmd)
    }

    fn run(&mut self, cmd: &Value) -> Result<Value, String> {
        match cmd["op"].as_str() {
            Some("batch") => {
                let commands = cmd["commands"].as_array().ok_or("batch needs \"commands\"")?;
                let mut results = Vec::new();
                for (i, command) in commands.iter().enumerate() {
                    results.push(self.run(command).map_err(|e| format!("command {i} ({}) failed: {e}. The {i} before it were applied.", command["op"].as_str().unwrap_or("?")))?);
                }
                Ok(json!(results))
            }
            Some("ai_edit") => self.ai_edit(cmd),
            Some("ai_status") => Ok(json!({"running": false})),
            _ => api::execute(&mut self.doc, &mut self.scratch, &mut self.fonts, cmd),
        }
    }

    /// With no window there is nothing to keep responsive, so this waits for the answer.
    fn ai_edit(&mut self, cmd: &Value) -> Result<Value, String> {
        let prompt = cmd["prompt"].as_str().unwrap_or("").to_owned();
        let source = ai_source(cmd)?;
        let cfg = ai::Config::load(&Store::standard().load_settings().0.ai);
        if cfg.key.is_none() {
            return Err("No OpenAI key is set. Put OPENAI_API_KEY in a .env file or enter it in Pixelferrite's Settings.".into());
        }
        ai::endpoint_origin(&cfg.base)?;
        let model = cfg.model.clone();
        let job = aiedit::prepare(&self.doc.state, source, |w, h| ai::request_size(&model, w, h)).ok_or("Nothing to send: the selection doesn't touch the image")?;
        let area = job.result_rect();
        self.doc.check_layer_capacity(area.width() as u32, area.height() as u32).map_err(|e| e.to_string())?;
        if prompt.trim().is_empty() && job.mask.is_none() {
            return Err("ai_edit needs a prompt unless a selection says where to extend the picture".into());
        }
        let png = io::encode_png(&job.image).map_err(|e| e.to_string())?;
        let mask = job.mask.as_ref().map(io::encode_png).transpose().map_err(|e| e.to_string())?;
        let reply = ai::edit(&cfg, &ai::full_prompt(&prompt, job.mask.is_some()), (job.image.w, job.image.h), &png, mask.as_deref())?;
        let name: String = format!("AI: {}", if prompt.trim().is_empty() { "extend" } else { prompt.trim() }).chars().take(60).collect();
        let layer = self.doc.insert_ai_result(&job, &reply.pixels, &name);
        Ok(json!({"layer": layer, "name": name, "rect": [area.x0, area.y0, area.x1, area.y1], "usage": reply.usage}))
    }
}

pub fn ai_source(cmd: &Value) -> Result<Source, String> {
    match cmd["source"].as_str().unwrap_or("visible") {
        "visible" => Ok(Source::Visible),
        "layer" => Ok(Source::Layer),
        other => Err(format!("source is visible or layer, not {other}")),
    }
}

struct Backend {
    port: u16,
    mode: Mode,
    session: Session,
}

#[derive(Debug, PartialEq)]
enum Mode {
    Undecided,
    Headless,
    Live(Option<String>),
}

impl Backend {
    fn new(port: u16, headless: bool) -> Self {
        let session = Session { doc: Document::new(1600, 1200, Some([255; 4])), scratch: Scratch::default(), fonts: Fonts::default() };
        Self { port, mode: if headless { Mode::Headless } else { Mode::Undecided }, session }
    }

    fn run(&mut self, cmd: &Value) -> Result<Value, String> {
        self.run_with(cmd, crate::bridge::call)
    }

    fn run_with(&mut self, cmd: &Value, connect: impl FnOnce(u16, &Value, Option<&str>) -> Result<crate::bridge::CallReply, crate::bridge::CallError>) -> Result<Value, String> {
        if self.mode == Mode::Headless {
            return self.session.execute(cmd);
        }
        let expected = match &self.mode {
            Mode::Live(id) => id.as_deref(),
            _ => None,
        };
        match connect(self.port, cmd, expected) {
            Ok(reply) => {
                self.mode = Mode::Live(Some(reply.instance));
                reply.result
            }
            Err(e) if self.mode == Mode::Undecided && e.app_absent() => {
                // Only an initial absence, before connecting or sending anything,
                // selects a headless document. This choice lasts for the session.
                self.mode = Mode::Headless;
                self.session.execute(cmd)
            }
            Err(e) => {
                if !matches!(self.mode, Mode::Live(Some(_))) {
                    self.mode = Mode::Live(e.instance);
                }
                let outcome = if e.may_have_executed { " The command outcome is unknown; look at the document before retrying." } else { " No command was sent." };
                Err(format!("Pixelferrite app connection failed: {}.{outcome} The MCP session has not switched to another document. Restart the MCP client to select a new backend, or use --headless explicitly.", e.source))
            }
        }
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "get_document_info",
            "description": "Get the Pixelferrite document: canvas size, file, layers (bottom first, with ids, names, rectangles, opacity, blend mode, masks and text), the active layer, the selection's bounds, and recent history. Call this first; never assume the document is empty.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "get_layer_info",
            "description": "Details of one layer by id or exact name: its frame, the bounds of what is actually painted on it, mask, and for text layers the text and its font settings.",
            "inputSchema": {"type": "object", "properties": {"layer": {"description": "Layer id or name; the active layer when omitted."}}}
        },
        {
            "name": "get_canvas_screenshot",
            "description": "Render the image to look at it. Without arguments it shows the whole canvas. \"region\" [x0,y0,x1,y1] zooms into part of it (small regions are enlarged up to 8x, so use this to inspect details such as eyes or edges). \"layer\" shows one layer alone. The selection is shown bright with the rest dimmed unless \"show_selection\" is false. The result states the region and scale so image positions convert back to document pixels: document = region origin + image pixel / scale.",
            "inputSchema": {"type": "object", "properties": {
                "region": {"type": "array", "items": {"type": "number"}, "description": "[x0,y0,x1,y1] in document pixels"},
                "max_size": {"type": "integer", "description": "Longest side of the returned image, 64-2048 (default 1024)"},
                "layer": {"description": "Layer id or name to show alone"},
                "show_selection": {"type": "boolean"}
            }}
        },
        {
            "name": "get_reference",
            "description": "The full list of Pixelferrite commands with their arguments. Read this once before editing anything.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "execute_pixelferrite_commands",
            "description": "Run editing commands in order; each is one undo step and execution stops at the first error. Commands are JSON objects with an \"op\": new, open, save, export, export_layer, undo, redo, crop, flatten, content_aware_scale, sample, find_color, list_fonts, add_layer, add_image, delete_layer, duplicate_layer, set_active, set_layer, reorder_layer, merge_down, flip_layer, resize_layer, perspective, add_mask, delete_mask, apply_mask, invert_mask, select_all, deselect, invert_selection, select_rect, select_ellipse, select_polygon, select_wand, select_color, select_subject, select_regions, feather_selection, grow_selection, fill, clear, bucket, stroke, gradient, copy, cut, paste, filter, adjust, red_eye, add_text, set_text. Call get_reference for the arguments of each.",
            "inputSchema": {"type": "object", "properties": {"commands": {"type": "array", "items": {"type": "object"}, "description": "Command objects, each with an \"op\"."}}, "required": ["commands"]}
        },
        {
            "name": "ai_edit",
            "description": "Send the image (or, with a selection, the selected area and its surroundings) to OpenAI's image model with a prompt, and add the answer as a new layer cut to the selection. Use it for what the other commands cannot do: removing objects from detailed backgrounds, adding things, restyling, extending a picture into empty canvas. This uses the user's OpenAI key, costs money on every call and uploads the image, so prefer the ordinary commands when they can do the job. It waits for the answer, usually under a minute.",
            "inputSchema": {"type": "object", "properties": {
                "prompt": {"type": "string", "description": "What to do. May be empty when a selection marks empty canvas to extend into."},
                "source": {"type": "string", "enum": ["visible", "layer"], "description": "visible (default): everything as it looks. layer: the active layer alone."}
            }, "required": ["prompt"]}
        }
    ])
}

fn call(b: &mut Backend, name: &str, args: &Value) -> Result<Value, String> {
    let text = |v: Value| json!([{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap()}]);
    let with_op = |op: &str| {
        let mut cmd = if args.is_object() { args.clone() } else { json!({}) };
        cmd["op"] = json!(op);
        cmd
    };
    match name {
        "get_document_info" => {
            let mut v = b.run(&json!({"op": "get_document_info"}))?;
            v["running_in"] = json!(if matches!(b.mode, Mode::Live(_)) { "the Pixelferrite app (changes appear live in its window)" } else { "headless mode (a separate document with no window); use open, save and export to work with files" });
            Ok(text(v))
        }
        "get_layer_info" => Ok(text(b.run(&with_op("get_layer_info"))?)),
        "get_canvas_screenshot" => {
            let mut v = b.run(&with_op("get_canvas_screenshot"))?;
            let png = v["png_base64"].take();
            v.as_object_mut().unwrap().remove("png_base64");
            Ok(json!([{"type": "image", "data": png, "mimeType": "image/png"}, {"type": "text", "text": v.to_string()}]))
        }
        "get_reference" => Ok(json!([{"type": "text", "text": api::REFERENCE}])),
        "execute_pixelferrite_commands" => Ok(text(b.run(&json!({"op": "batch", "commands": args["commands"]}))?)),
        "ai_edit" => {
            let started = b.run(&with_op("ai_edit"))?;
            if started["started"] != true {
                return Ok(text(started));
            }
            // The app answers at once and works in the background, so its window stays usable.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(750));
                let status = b.run(&json!({"op": "ai_status"}))?;
                if status["running"] != true {
                    return match status["error"].as_str() {
                        Some(e) => Err(e.to_owned()),
                        None => Ok(text(status)),
                    };
                }
                if std::time::Instant::now() > deadline {
                    return Err("The AI request is still running in the app after five minutes; check its window.".into());
                }
            }
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

pub fn serve(port: u16, headless: bool) {
    let mut b = Backend::new(port, headless);
    let stdout = std::io::stdout();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    loop {
        let line = match crate::bridge::read_frame(&mut input, crate::bridge::MAX_REQUEST_BYTES) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                eprintln!("MCP input rejected: {e}");
                break;
            }
        };
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else { continue };
        let id = msg["id"].clone();
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "pixelferrite", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Pixelferrite is a layered image editor. Read get_reference once, start with get_document_info, look with get_canvas_screenshot (zoom in with region), edit with execute_pixelferrite_commands, and check the result with another screenshot. ai_edit is a paid request to OpenAI; use it only when the ordinary commands cannot do the job.",
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => Ok(match call(&mut b, msg["params"]["name"].as_str().unwrap_or(""), &msg["params"]["arguments"]) {
                Ok(content) => json!({"content": content}),
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            }),
            m if id.is_null() || m.starts_with("notifications/") => continue,
            m => Err(format!("method not found: {m}")),
        };
        let reply = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": e}}),
        };
        let mut out = stdout.lock();
        if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{CallError, CallReply};
    use std::io::{Error, ErrorKind};

    fn failure(kind: ErrorKind, connected: bool, may_have_executed: bool) -> CallError {
        CallError { source: Error::new(kind, "test transport failure"), connected, instance: connected.then(|| "window-a".to_owned()), may_have_executed }
    }

    #[test]
    fn initial_absence_selects_and_pins_headless() {
        let mut b = Backend::new(0, false);
        let cmd = json!({"op": "get_document_info"});
        b.run_with(&cmd, |_, _, _| Err(failure(ErrorKind::NotFound, false, false))).unwrap();
        assert_eq!(b.mode, Mode::Headless);
        b.run_with(&cmd, |_, _, _| panic!("must not switch to an app that opened later")).unwrap();
    }

    #[test]
    fn live_session_never_falls_back_after_disconnect() {
        let mut b = Backend::new(0, false);
        let cmd = json!({"op": "get_document_info"});
        b.run_with(&cmd, |_, _, expected| {
            assert!(expected.is_none());
            Ok(CallReply { instance: "window-a".to_owned(), result: Ok(json!({})) })
        })
        .unwrap();
        let error = b
            .run_with(&cmd, |_, _, expected| {
                assert_eq!(expected, Some("window-a"));
                Err(failure(ErrorKind::ConnectionRefused, false, false))
            })
            .unwrap_err();
        assert!(error.contains("No command was sent"));
        assert_eq!(b.mode, Mode::Live(Some("window-a".to_owned())));
    }

    #[test]
    fn connected_and_uncertain_failures_pin_live_without_replaying() {
        for (kind, connected, sent) in [(ErrorKind::TimedOut, true, true), (ErrorKind::InvalidData, true, false), (ErrorKind::PermissionDenied, false, false)] {
            let mut b = Backend::new(0, false);
            let error = b.run_with(&json!({"op": "deselect"}), |_, _, _| Err(failure(kind, connected, sent))).unwrap_err();
            assert!(matches!(b.mode, Mode::Live(_)));
            assert_eq!(error.contains("outcome is unknown"), sent);
            assert!(b.run_with(&json!({"op": "deselect"}), |_, _, _| Err(failure(ErrorKind::NotFound, false, false))).is_err());
            assert!(matches!(b.mode, Mode::Live(_)));
        }
    }

    #[test]
    fn headless_tools_edit_and_show_a_document() {
        let mut b = Backend::new(0, true);
        let info = call(&mut b, "get_document_info", &Value::Null).unwrap();
        assert!(info[0]["text"].as_str().unwrap().contains("headless mode"));
        let commands = json!({"commands": [{"op": "select_rect", "rect": [0, 0, 800, 1200]}, {"op": "fill", "color": "#102030"}]});
        call(&mut b, "execute_pixelferrite_commands", &commands).unwrap();
        let shot = call(&mut b, "get_canvas_screenshot", &json!({"max_size": 128, "show_selection": false})).unwrap();
        assert_eq!((shot[0]["type"].as_str(), shot[0]["mimeType"].as_str()), (Some("image"), Some("image/png")));
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, shot[0]["data"].as_str().unwrap()).unwrap();
        let px = io::decode_image(&png).unwrap();
        assert_eq!((px.w, px.h, px.px(10, 10), px.px(120, 10)), (128, 96, [16, 32, 48, 255], [255, 255, 255, 255]));
        assert!(!shot[1]["text"].as_str().unwrap().contains("png_base64"));
        assert!(call(&mut b, "get_reference", &Value::Null).unwrap()[0]["text"].as_str().unwrap().contains("select_subject"));
        assert!(call(&mut b, "execute_pixelferrite_commands", &json!({"commands": [{"op": "wobble"}]})).unwrap_err().contains("unknown op"));
        // Every op the tool description promises is one the reference explains.
        let listed = tools()[4]["description"].as_str().unwrap().to_owned();
        let ops = listed.split("\"op\": ").nth(1).unwrap().split(". Call").next().unwrap();
        for op in ops.split(", ") {
            assert!(api::REFERENCE.contains(&format!("\"op\":\"{op}\"")), "{op} is not in the reference");
        }
    }
}
