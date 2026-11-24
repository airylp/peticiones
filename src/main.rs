// En Windows, sin ventana de consola junto a la app.
#![cfg_attr(windows, windows_subsystem = "windows")]
// Primero: sus macros t!/tf! se usan en todos los demás módulos.
#[macro_use]
mod i18n;
mod app;
mod auth;
mod beautify;
mod estilo;
mod http;
mod icon;
mod icons;
mod import;
mod model;
mod store;
mod update;
mod vars;

/// Identidad de peticiones para los módulos comunes (logotipo, «Acerca de»…).
pub const APP: icon::Marca = icon::Marca {
    nombre: "peticiones",
    descripcion: || t!("Cliente HTTP y SOAP para probar APIs"),
    letra: icon::LETRA_P,
    color: icon::NARANJA,
    hecho_con: "Rust · egui / eframe · ureq",
};

fn main() -> eframe::Result {
    let args: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    // Empaquetado: `peticiones --iconset carpeta` (macOS) y `peticiones --ico archivo.ico` (Windows).
    match args.first().and_then(|a| a.to_str()) {
        Some("--iconset") => {
            let dir = args.get(1).cloned().unwrap_or_else(|| "peticiones.iconset".into());
            icon::write_iconset(&dir).expect("no se pudo escribir el iconset");
            return Ok(());
        }
        Some("--ico") => {
            let path = args.get(1).cloned().unwrap_or_else(|| "peticiones.ico".into());
            std::fs::write(&path, icon::ico()).expect("no se pudo escribir el .ico");
            return Ok(());
        }
        _ => {}
    }
    // En modo captura (pruebas) la ventana no se activa: no roba el teclado.
    let testing = std::env::var_os("PETICIONES_SCREENSHOT").is_some();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_active(!testing)
            .with_title("Peticiones")
            .with_app_id("peticiones")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 480.0])
            .with_icon(std::sync::Arc::new(icon::app_icon())),
        persist_window: !testing,
        ..Default::default()
    };
    eframe::run_native("peticiones", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
