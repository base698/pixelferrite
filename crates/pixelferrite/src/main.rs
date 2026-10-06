mod ai;
mod aiui;
mod app;
mod canvas;
mod fonts;
mod fxui;
mod panels;
mod store;
mod tools;
mod view;

#[cfg(test)]
mod uitest;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let mut file = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: pixelferrite [FILE]\n\nOpens an image (.ora, .png, .jpg, .gif, .webp, ...) for editing.");
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
