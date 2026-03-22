mod actor;
mod app;
mod bridge;
mod canvas;
mod executor;
mod panels;
mod persistence;
mod registry;
mod render;
mod db;
mod debug_log;
mod render_ui;
mod scheme_convert;
mod scheme_engine;
mod theme;
mod types;
mod network;
mod ocapn;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("warn,wgpu_hal=off,wgpu_core=off,naga=off,wasm_canvas=info"),
    )
    .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 600.0])
            .with_title("WASM Canvas"),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "WASM Canvas",
        options,
        Box::new(|cc| Ok(Box::new(app::WasmCanvasApp::new(cc)))),
    )
}
