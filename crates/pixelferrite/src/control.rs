//! Commands arriving from outside the window: the MCP bridge hands them to
//! the UI thread, and they run against the open document.

use pf_core::api;
use serde_json::{Value, json};

use crate::app::App;

/// Commands that only look, and so are safe while a dialog or drag is open.
fn read_only(op: &str) -> bool {
    matches!(op, "get_document_info" | "get_layer_info" | "get_canvas_screenshot" | "sample" | "find_color" | "list_fonts" | "ai_status")
}

impl App {
    /// Take queued bridge commands. Called once a frame.
    pub fn serve_bridge(&mut self) {
        if let Some(bridge) = self.bridge.take() {
            bridge.serve(|cmd| self.execute(cmd));
            self.bridge = Some(bridge);
        }
    }

    /// Runs a command from the MCP bridge.
    pub fn execute(&mut self, cmd: &Value) -> Result<Value, String> {
        api::validate_request(cmd)?;
        self.execute_command(cmd)
    }

    fn execute_command(&mut self, cmd: &Value) -> Result<Value, String> {
        let op = cmd["op"].as_str().ok_or("the command needs an \"op\"")?;
        if op == "batch" {
            let commands = cmd["commands"].as_array().ok_or("batch needs \"commands\"")?;
            let mut results = Vec::new();
            for (i, command) in commands.iter().enumerate() {
                results.push(self.execute_command(command).map_err(|e| format!("command {i} ({}) failed: {e}. The {i} before it were applied.", command["op"].as_str().unwrap_or("?")))?);
            }
            return Ok(json!(results));
        }
        if op == "ai_status" {
            // Report a failure here instead of leaving a dialog open for someone who may not be watching.
            let running = self.ai.running();
            let mut status = json!({"running": running});
            if !running {
                match self.ai.take_error() {
                    Some(e) => status["error"] = json!(e),
                    None => status["document"] = api::document_info(&self.doc),
                }
            }
            return Ok(status);
        }
        if read_only(op) {
            return api::execute(&mut self.doc, &mut self.api_scratch, &mut self.fonts, cmd);
        }
        // A dialog's preview or a drag holds a snapshot it will roll back to;
        // an outside edit made now would be lost or half-applied.
        if self.mutation_busy() || self.ai.running() {
            return Err("Pixelferrite is in the middle of something in its window (an open dialog, a drag, a preview or a running AI request). Finish or cancel it there, then try again. Looking (document info, screenshots, sample) still works.".into());
        }
        if op == "ai_edit" {
            let source = crate::mcp::ai_source(cmd)?;
            let prompt = cmd["prompt"].as_str().unwrap_or("").to_owned();
            if prompt.trim().is_empty() && self.doc.state.selection.is_none() {
                return Err("ai_edit needs a prompt unless a selection says where to extend the picture".into());
            }
            let cfg = crate::ai::Config::load(&self.saved.ai);
            if cfg.key.is_none() {
                return Err("No OpenAI key is set. Put OPENAI_API_KEY in a .env file or enter it in Pixelferrite's Settings.".into());
            }
            self.toast = None;
            let ctx = self.ctx.clone();
            self.send_to_ai(&ctx, prompt, cfg, source);
            if !self.ai.running() {
                // send_to_ai explains a refusal with a toast.
                return Err(self.toast.take().map_or("The request could not be started".to_owned(), |t| t.0));
            }
            return Ok(json!({"started": true}));
        }
        if let Some(doc) = api::replacement(cmd)? {
            if self.doc.modified && cmd["discard_unsaved"].as_bool() != Some(true) {
                return Err("The open image has unsaved changes. Save it first, or set \"discard_unsaved\": true to discard them.".into());
            }
            self.replace_document(doc, cmd["path"].as_str().filter(|_| op == "open").map(std::path::Path::new));
            return Ok(api::document_info(&self.doc));
        }
        let result = api::execute(&mut self.doc, &mut self.api_scratch, &mut self.fonts, cmd);
        // Cheap insurance that the window shows exactly what the command left behind.
        self.doc.mark_all_dirty();
        if result.is_ok() && op == "save" {
            self.saved_to_file();
        }
        result
    }
}
