#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod doc;
mod export;
mod fonts;

use eframe::egui;

fn main() -> eframe::Result {
    // `--bench <keluaran.png>` menjalankan uji performa otomatis lalu keluar.
    let args: Vec<String> = std::env::args().collect();
    let bench = args
        .iter()
        .position(|a| a == "--bench")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);
    let open = args
        .iter()
        .skip(1)
        .find(|a| a.ends_with(".kanvas"))
        .map(std::path::PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kanvas")
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Kanvas",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, bench, open)))),
    )
}
