//! The real binary as an MCP client would run it: `pixelferrite mcp --headless`
//! over stdin/stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn headless_server_speaks_mcp() {
    let dir = std::env::temp_dir().join(format!("pf-mcp-stdio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pixelferrite"))
        .args(["mcp", "--headless"])
        // Never touch the real settings or a running app.
        .env("PIXELFERRITE_CONFIG_DIR", dir.join("config"))
        .env("PIXELFERRITE_DATA_DIR", dir.join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut id = 0;
    let mut request = |method: &str, params: Value| -> Value {
        id += 1;
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        reply["result"].clone()
    };

    let hello = request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    assert_eq!(hello["serverInfo"]["name"], "pixelferrite");
    let tools = request("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["get_document_info", "get_layer_info", "get_canvas_screenshot", "get_reference", "execute_pixelferrite_commands", "ai_edit"]);

    // Build a picture, export it, and look at it.
    let png = dir.join("out.png");
    let done = request("tools/call", json!({"name": "execute_pixelferrite_commands", "arguments": {"commands": [
        {"op": "new", "width": 200, "height": 100, "background": "#ffffff"},
        {"op": "select_rect", "rect": [0, 0, 100, 100]},
        {"op": "fill", "color": "#ff0000"},
        {"op": "deselect"},
        {"op": "add_text", "text": "hi", "x": 110, "y": 70, "size": 48},
        {"op": "export", "path": png},
    ]}}));
    assert_ne!(done["isError"], true, "{done}");
    let px = image_size(&std::fs::read(&png).unwrap());
    assert_eq!(px, (200, 100));
    let info = request("tools/call", json!({"name": "get_document_info", "arguments": {}}));
    let text = info["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("headless mode") && text.contains("\"width\": 200"), "{text}");
    let shot = request("tools/call", json!({"name": "get_canvas_screenshot", "arguments": {"region": [0, 0, 50, 50]}}));
    assert_eq!(shot["content"][0]["type"], "image");
    assert!(shot["content"][1]["text"].as_str().unwrap().contains("\"scale\":8"));

    // Errors come back as tool errors, not protocol errors.
    let bad = request("tools/call", json!({"name": "execute_pixelferrite_commands", "arguments": {"commands": [{"op": "select_rect", "rect": [1, 2]}]}}));
    assert_eq!(bad["isError"], true);
    assert!(bad["content"][0]["text"].as_str().unwrap().contains("expected a rectangle"));

    drop(stdin);
    assert!(child.wait().unwrap().success(), "the server exits cleanly when its input closes");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Width and height from a PNG header.
fn image_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    (u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap()))
}
