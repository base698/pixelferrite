mod ai;
mod about;
mod aiui;
mod app;
mod bridge;
mod canvas;
mod control;
mod fonts;
mod fxui;
mod panels;
mod store;
mod recovery;
mod jobs;
mod mcp;
mod tools;
mod view;

#[cfg(test)]
mod uitest;
#[cfg(test)]
mod app_regressions;
#[cfg(test)]
mod demo;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let all: Vec<String> = std::env::args().skip(1).collect();
    if all.first().is_some_and(|a| a == "mcp") {
        let port = all.iter().position(|a| a == "--port").and_then(|i| all.get(i + 1)).and_then(|p| p.parse().ok()).unwrap_or_else(|| store::Store::standard().load_settings().0.bridge.port);
        mcp::serve(port, all.iter().any(|a| a == "--headless"));
        return Ok(());
    }
    let mut file = None;
    let mut args = all.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: pixelferrite [FILE]\n       pixelferrite mcp [--headless] [--port N]\n\nOpens an image (.ora, .png, .jpg, .gif, .webp, ...) for editing.\n\n`pixelferrite mcp` runs a Model Context Protocol server on stdin/stdout for AI\nclients. It drives the running Pixelferrite window if there is one, and\notherwise (or with --headless) works on a document of its own. The backend\nstays fixed until the MCP client restarts. --port names a private local\nsocket; no network port is opened.\n\n-V, --version    Print build information");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("{}", about::build_info());
                return Ok(());
            }
            // Older macOS versions pass a process serial number when launched from Finder.
            _ if a.starts_with("-psn_") => {}
            _ => file = Some(PathBuf::from(a)),
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Pixelferrite")
            .with_inner_size([1400.0, 880.0])
            .with_min_inner_size([760.0, 480.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Pixelferrite", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, file)))))
}
