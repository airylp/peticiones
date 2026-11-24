//! En Windows mete en el `.exe` el ícono (`windows/peticiones.ico`) y los datos
//! de versión que muestra el Explorador. En macOS y Linux no hace nada.

fn main() {
    println!("cargo:rerun-if-changed=windows/peticiones.ico");
    println!("cargo:rerun-if-env-changed=PETICIONES_COMMIT");
    println!("cargo:rerun-if-env-changed=PETICIONES_VERSION");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("windows/peticiones.ico")
            .set("ProductName", "peticiones")
            .set("FileDescription", "peticiones")
            .set("CompanyName", "airylp")
            .set("LegalCopyright", "MIT");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no se pudo añadir el ícono al .exe: {e}");
        }
    }
}
