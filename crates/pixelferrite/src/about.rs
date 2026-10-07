//! Build identity is captured at compilation, never read from the runtime checkout.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMIT: &str = env!("PIXELFERRITE_COMMIT");
pub const SOURCE_STATUS: &str = env!("PIXELFERRITE_SOURCE_STATUS");
pub const TARGET: &str = env!("PIXELFERRITE_TARGET");
pub const PROFILE: &str = env!("PIXELFERRITE_PROFILE");

pub fn build_info() -> String {
    format!("Pixelferrite {VERSION}\nCommit: {COMMIT}\nSource status: {SOURCE_STATUS}\nTarget: {TARGET}\nBuild: {PROFILE}")
}

pub fn window(ctx: &egui::Context, open: &mut bool) {
    if !*open { return; }
    let mut close = false;
    egui::Window::new("About Pixelferrite")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(490.0)
        .default_pos(ctx.content_rect().center() - egui::vec2(260.0, 160.0))
        .show(ctx, |ui| {
            ui.heading("Pixelferrite");
            ui.label("A layered image editor.");
            ui.add_space(12.0);
            egui::Grid::new("build-info").spacing([16.0, 8.0]).show(ui, |ui| {
                for (label, value) in [("Version", VERSION), ("Commit", COMMIT),
                    ("Source status", SOURCE_STATUS), ("Target", TARGET), ("Build", PROFILE)] {
                    ui.label(label);
                    ui.add(egui::Label::new(egui::RichText::new(value).monospace()).selectable(true));
                    ui.end_row();
                }
            });
            if SOURCE_STATUS == "Local changes" {
                ui.add_space(8.0);
                ui.label("This build includes local changes beyond the displayed commit.");
            } else if SOURCE_STATUS == "Unknown" {
                ui.add_space(8.0);
                ui.label("Source identity could not be fully verified when this app was built.");
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Copy build info").clicked() { ctx.copy_text(build_info()); }
                if COMMIT != "Unknown" {
                    ui.hyperlink_to("View commit on GitHub", format!("https://github.com/base698/pixelferrite/commit/{COMMIT}"));
                }
                close = ui.button("Close").clicked();
            });
        });
    if close { *open = false; }
}
