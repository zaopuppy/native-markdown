mod app;
mod document;
mod markdown;
mod scroll;
mod theme;
mod zoom;

use app::NativeMarkdownApp;
use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([720.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Native Markdown",
        options,
        Box::new(|cc| Ok(Box::new(NativeMarkdownApp::new(cc)))),
    )
}
