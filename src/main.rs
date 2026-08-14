fn main() -> eframe::Result {
    env_logger::init();

    let initial_files: Vec<std::path::PathBuf> = std::env::args().skip(1).map(std::path::PathBuf::from).collect();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 950.0])
            .with_title("rapid-analyzer"),
        ..Default::default()
    };

    eframe::run_native(
        "rapid-analyzer",
        native_options,
        Box::new(|cc| Ok(Box::new(rapid_analyzer::app::App::new(cc, initial_files)))),
    )
}
