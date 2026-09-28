#[cfg(not(target_arch = "wasm32"))]
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

/// The web build's entry point, run by the JS `trunk` generates: the app
/// draws into `index.html`'s canvas, and files come in through the import
/// button or by dropping them onto the page.
#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast as _;

    eframe::WebLogger::init(log::LevelFilter::Info).ok();

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window().and_then(|w| w.document()).expect("no document");
        let canvas = document
            .get_element_by_id("rapid_analyzer_canvas")
            .expect("index.html has no #rapid_analyzer_canvas")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("#rapid_analyzer_canvas is not a canvas");

        let started = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(rapid_analyzer::app::App::new(cc, Vec::new())))),
            )
            .await;

        // The page shows a loading message until the app replaces it; if the
        // app never starts, say why instead of leaving it spinning.
        if let Some(loading) = document.get_element_by_id("loading") {
            match started {
                Ok(()) => loading.remove(),
                Err(e) => {
                    loading.set_inner_html("rapid-analyzer failed to start. The browser console has the details.");
                    log::error!("failed to start: {e:?}");
                }
            }
        }
    });
}
