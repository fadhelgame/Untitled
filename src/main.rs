#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod arrange;
mod component;
mod doc;
mod export;
mod file;
mod find;
mod fonts;
mod layout;
mod outline;
mod svg_import;

use eframe::egui;

fn main() -> eframe::Result {
    // `--bench <keluaran.png>` menjalankan uji performa otomatis lalu keluar.
    let args: Vec<String> = std::env::args().collect();
    let bench = args
        .iter()
        .position(|a| a == "--bench")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);
    // `--rembg <masukan> <keluaran.png>`: remove background tanpa membuka jendela.
    if let Some(i) = args.iter().position(|a| a == "--rembg") {
        let result = match (args.get(i + 1), args.get(i + 2)) {
            (Some(input), Some(output)) => app::rembg::cli(input.as_ref(), output.as_ref()),
            _ => Err("usage: kanvas --rembg <input> <output.png>".into()),
        };
        if let Err(e) = &result {
            eprintln!("{e}");
        }
        std::process::exit(result.is_err() as i32);
    }
    let open = args
        .iter()
        .skip(1)
        .find(|a| a.ends_with(".kanvas"))
        .map(std::path::PathBuf::from);

    // Window icon (title bar, taskbar, Alt+Tab). The .exe file icon itself comes from `build.rs`.
    let icon = image::load_from_memory(include_bytes!("../assets/icon-256.png")).map(|i| i.to_rgba8()).ok();

    let viewport = egui::ViewportBuilder::default()
            .with_title("Kanvas")
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([720.0, 480.0]);
    let viewport = match icon {
        Some(i) => viewport.with_icon(egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() }),
        None => viewport,
    };
    let options = eframe::NativeOptions {
        viewport,
        // macOS: OpenGL vsync paints in uneven pairs on ProMotion displays and looks like 60 fps.
        // There the app follows the display's refresh clock itself; see `app::pacer`.
        vsync: !cfg!(target_os = "macos"),
        ..Default::default()
    };
    eframe::run_native(
        "Kanvas",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, bench, open)))),
    )
}
