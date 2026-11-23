// En Windows, sin ventana de consola junto a la app.
#![cfg_attr(windows, windows_subsystem = "windows")]
// Primero: sus macros t!/tf! se usan en todos los demás módulos.
#[macro_use]
mod i18n;
mod app;
mod auth;
mod beautify;
mod http;
mod import;
mod model;
mod store;
mod vars;

fn main() -> eframe::Result {
    // En modo captura (pruebas) la ventana no se activa: no roba el teclado.
    let testing = std::env::var_os("PETICIONES_SCREENSHOT").is_some();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_active(!testing)
            .with_title("Peticiones")
            .with_app_id("peticiones")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 480.0]),
        persist_window: !testing,
        ..Default::default()
    };
    eframe::run_native("peticiones", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
