//! Polskie Studio Map — atlas warstw mapowych Polski w 3D (Windows, natywnie: Rust + wgpu + egui).
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod analysis;
mod app;
mod bake;
mod download;
mod fmt;
mod geo;
mod gpx;
mod heights;
mod landcover;
mod layers;
mod metrics;
mod mvt;
mod national;
mod osm;
mod pack;
mod render;
mod settings;
mod store;
mod sun;
mod trails;
mod ui;
mod updater;
mod world;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wgpu_hal=warn,wgpu_core=warn,naga=warn"),
    )
    .init();
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Polskie Studio Map")
            .with_app_id("psm")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1024.0, 640.0])
            .with_icon(icon),
        renderer: eframe::Renderer::Wgpu,
        vsync: settings::Settings::load().vsync,
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native(
        "Polskie Studio Map",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
