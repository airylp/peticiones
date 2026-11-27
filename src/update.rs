//! Actualizar peticiones desde la propia interfaz.
//!
//! Compara la versión con la última publicada en GitHub Releases (incluye
//! prototipos, como `install.sh`).
//!
//! macOS y Linux: si se pide, ejecuta el mismo `install.sh` que se usa para
//! instalar (descarga la versión publicada y reemplaza la app instalada). La app
//! abierta sigue funcionando hasta que se reinicia.
//!
//! Windows: descarga el instalador publicado (`…-setup.exe`) y lo ejecuta en
//! silencio cuando la app se cierra; después se vuelve a abrir.

use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Repositorio en GitHub (usuario/repo). `PETICIONES_REPO` lo cambia.
fn repo() -> String {
    std::env::var("PETICIONES_REPO").unwrap_or_else(|_| "airylp/peticiones".to_string())
}

/// ¿Se puede actualizar desde la app en esta plataforma?
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "linux", windows));

/// En Windows se actualiza con el instalador publicado.
pub const WINDOWS: bool = cfg!(windows);

/// Actualizar compila el código (buscador, conector) o descarga lo publicado (peticiones).
pub const COMPILES: bool = false;

/// Versión publicada con la que se compiló (tag, p. ej. «v0.2.0»; la graba el
/// flujo de GitHub que crea el instalador de Windows).
pub fn current_version() -> Option<&'static str> {
    use std::sync::OnceLock;
    static V: OnceLock<Option<&'static str>> = OnceLock::new();
    *V.get_or_init(|| {
        if let Some(v) = option_env!("PETICIONES_VERSION").filter(|s| !s.is_empty()) {
            return Some(v);
        }
        // Instalada con install.sh: la versión que dejó en la configuración.
        install_dir()?;
        let v = std::fs::read_to_string(crate::store::config_dir()?.join("version-instalada")).ok()?;
        let v = v.trim();
        (v.starts_with('v') && !v.contains('@')).then(|| &*Box::leak(v.to_string().into_boxed_str()))
    })
}

/// Sin ventana de consola (Windows: la app no tiene consola y cada programa
/// de consola que lance abriría una).
fn hidden(c: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// De la respuesta JSON de `releases?per_page=1` (o `releases/latest`): tag de
/// la primera versión y URL de su instalador de Windows (`…setup….exe`), si hay.
fn parse_release(json: &str) -> Option<(String, Option<String>)> {
    let field = |from: &str, key: &str| -> Option<String> {
        let i = from.find(&format!("\"{key}\""))?;
        let rest = from[i + key.len() + 2..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
        Some(rest[..rest.find('"')?].to_string())
    };
    let tag = field(json, "tag_name")?;
    let mut rest = json;
    while let Some(i) = rest.find("\"browser_download_url\"") {
        let url = field(&rest[i..], "browser_download_url")?;
        if url.contains(&format!("/download/{tag}/")) && url.to_ascii_lowercase().ends_with(".exe") && url.to_ascii_lowercase().contains("setup") {
            return Some((tag, Some(url)));
        }
        rest = &rest[i + 1..];
    }
    Some((tag, None))
}

/// Commit con el que se compiló (lo graban `install.sh` / `empaquetar-mac.sh`).
pub fn current_commit() -> Option<&'static str> {
    option_env!("PETICIONES_COMMIT").filter(|s| !s.is_empty())
}

pub fn version_label() -> String {
    if let Some(v) = current_version() {
        return match current_commit() {
            Some(c) => format!("{v} ({c})"),
            None => v.to_string(),
        };
    }
    match current_commit() {
        Some(c) => format!("{} ({c})", env!("CARGO_PKG_VERSION")),
        None => tf!("{v} (compilación local)", v = env!("CARGO_PKG_VERSION")),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Checking,
    /// Hay una versión más nueva (commit remoto).
    Available(String),
    UpToDate(String),
    Running,
    /// Instalada: ruta de la app (macOS) o del ejecutable (Linux).
    Done(PathBuf),
    Failed(String),
}

pub struct Updater {
    pub stage: Arc<Mutex<Stage>>,
    pub log: Arc<Mutex<Vec<String>>>,
    /// Windows: URL del instalador de la versión publicada.
    installer: Arc<Mutex<Option<String>>>,
}

/// PATH con cargo y Homebrew: al abrir la app desde el escritorio el PATH es mínimo.
fn path_env() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:{}", std::env::var("PATH").unwrap_or_default())
}

/// Dónde instalar: la carpeta de la .app que corre (macOS) o del ejecutable
/// (Linux, si no está dentro de un `target/` de compilación).
fn install_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(target_os = "macos") {
        let bundle = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))?;
        return bundle.parent().map(PathBuf::from);
    }
    let dir = exe.parent()?;
    if dir.components().any(|c| c.as_os_str() == "target") {
        return None;
    }
    Some(dir.to_path_buf())
}

/// Quita los códigos de color ANSI de una línea.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' && it.peek() == Some(&'[') {
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl Updater {
    /// Empieza comprobando la versión publicada.
    pub fn check(ctx: eframe::egui::Context) -> Self {
        let u = Updater { stage: Arc::new(Mutex::new(Stage::Checking)), log: Default::default(), installer: Default::default() };
        let (stage, installer) = (u.stage.clone(), u.installer.clone());
        std::thread::spawn(move || {
            let url = format!("https://api.github.com/repos/{}/releases?per_page=1", repo());
            let out = hidden(Command::new("curl").args(["-fsSL", "-H", "Accept: application/vnd.github+json", &url]).env("PATH", path_env())).output();
            let new = match out {
                Ok(o) if o.status.success() => match parse_release(&String::from_utf8_lossy(&o.stdout)) {
                    Some((tag, setup)) => {
                        *installer.lock().unwrap() = setup;
                        if current_version() == Some(tag.as_str()) { Stage::UpToDate(tag) } else { Stage::Available(tag) }
                    }
                    None => Stage::Failed(t!("respuesta inesperada de GitHub").into()),
                },
                Ok(o) => Stage::Failed(tf!("no se pudo consultar GitHub: {e}", e = String::from_utf8_lossy(&o.stderr).trim())),
                Err(e) => Stage::Failed(tf!("no se pudo ejecutar curl: {e}", e = e)),
            };
            *stage.lock().unwrap() = new;
            ctx.request_repaint();
        });
        u
    }

    /// Descarga e instala con `install.sh` (en segundo plano, con registro).
    pub fn run(&self, ctx: eframe::egui::Context) {
        *self.stage.lock().unwrap() = Stage::Running;
        self.log.lock().unwrap().clear();
        let (stage, log) = (self.stage.clone(), self.log.clone());
        if WINDOWS {
            // Descargar el instalador; se ejecuta al cerrar la app (`relaunch_after_exit`).
            let url = self.installer.lock().unwrap().clone();
            std::thread::spawn(move || {
                let new = match url {
                    None => Stage::Failed(t!("no hay instalador publicado").into()),
                    Some(url) => {
                        let target = std::env::temp_dir().join("peticiones-setup.exe");
                        log.lock().unwrap().push(format!("curl {url}"));
                        ctx.request_repaint();
                        match hidden(Command::new("curl").args(["-fsSL", "-o"]).arg(&target).arg(&url)).output() {
                            Ok(o) if o.status.success() && target.is_file() => {
                                log.lock().unwrap().push(format!("==> {}", target.display()));
                                Stage::Done(target)
                            }
                            Ok(o) => Stage::Failed(tf!("no se pudo descargar: {e}", e = String::from_utf8_lossy(&o.stderr).trim())),
                            Err(e) => Stage::Failed(tf!("no se pudo ejecutar curl: {e}", e = e)),
                        }
                    }
                };
                *stage.lock().unwrap() = new;
                ctx.request_repaint();
            });
            return;
        }
        // `PETICIONES_DESTINO` solo en pruebas: con él install.sh no crea la entrada
        // del menú ni los íconos (la instalación normal va a su sitio de siempre).
        let dest = std::env::var_os("PETICIONES_DESTINO").map(PathBuf::from);
        std::thread::spawn(move || {
            // `PETICIONES_INSTALL_SH` (pruebas) usa un install.sh local en vez del de GitHub.
            let script = match std::env::var("PETICIONES_INSTALL_SH") {
                Ok(p) => format!("exec 2>&1\nsh '{p}' instalar"),
                Err(_) => format!(
                    "exec 2>&1\nset -e\nF=\"${{TMPDIR:-/tmp}}/peticiones-install.sh\"\n# Se borra siempre, también si algo falla.\ntrap 'rm -f \"$F\"' EXIT INT TERM\ncurl -fsSL https://raw.githubusercontent.com/{}/main/install.sh -o \"$F\"\nsh \"$F\" instalar",
                    repo()
                ),
            };
            let mut cmd = Command::new("sh");
            cmd.args(["-c", &script]).env("PATH", path_env()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            if let Some(d) = &dest {
                cmd.env("PETICIONES_DESTINO", d);
            }
            // La app se reinicia sola al terminar («Reiniciar ahora»): el script no la cierra.
            cmd.env("PETICIONES_NO_CERRAR", "1");
            // Que la app abierta no se cierre: install.sh solo la reabre si la cerró él.
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    *stage.lock().unwrap() = Stage::Failed(tf!("no se pudo iniciar: {e}", e = e));
                    ctx.request_repaint();
                    return;
                }
            };
            let mut installed = None;
            if let Some(out) = child.stdout.take() {
                // Lectura tolerante: bytes no UTF-8 no cortan el registro (si se dejara
                // de leer, el proceso moriría por SIGPIPE). El progreso usa \r.
                let mut r = BufReader::new(out);
                let mut buf = Vec::new();
                // Fragmentos terminados en \n o en \r: curl reescribe su barra de
                // progreso con \r, y así el porcentaje llega en vivo (reemplaza la
                // línea anterior en vez de amontonarse).
                let mut bytes = std::io::Read::bytes(&mut r);
                let mut parcial = false;
                loop {
                    buf.clear();
                    let mut fin = 0u8;
                    for b in bytes.by_ref() {
                        let Ok(b) = b else { break };
                        if b == b'\n' || b == b'\r' {
                            fin = b;
                            break;
                        }
                        buf.push(b);
                    }
                    if fin == 0 && buf.is_empty() {
                        break;
                    }
                    let line = strip_ansi(&String::from_utf8_lossy(&buf));
                    if line.trim().is_empty() {
                        if fin == b'\n' {
                            parcial = false;
                        }
                        continue;
                    }
                    let reemplaza = std::mem::replace(&mut parcial, fin == b'\r');
                    // install.sh anuncia dónde quedó (línea sin traducir).
                    if let Some(p) = line.strip_prefix("RUTA_INSTALADA=") {
                        installed = Some(PathBuf::from(p.trim()));
                        continue;
                    }
                    let mut l = log.lock().unwrap();
                    if reemplaza {
                        l.pop();
                    }
                    l.push(line);
                    if l.len() > 5000 {
                        l.drain(..1000);
                    }
                    drop(l);
                    ctx.request_repaint();
                }
            }
            let ok = child.wait().map(|s| s.success()).unwrap_or(false);
            *stage.lock().unwrap() = match (ok, installed) {
                (true, Some(p)) => Stage::Done(p),
                (true, None) => Stage::Failed(t!("terminó sin instalar la app").into()),
                (false, _) => Stage::Failed(t!("falló (mira el registro)").into()),
            };
            ctx.request_repaint();
        });
    }

    pub fn stage(&self) -> Stage {
        self.stage.lock().unwrap().clone()
    }
}

/// Abre la versión recién instalada en cuanto termine este proceso: un `sh`
/// aparte espera a que muera el PID actual (así no conviven las dos y la sesión
/// ya está guardada) y entonces la abre. En macOS, con `open` y sin `-n`: abrir
/// otra instancia del mismo .app mientras la vieja seguía viva no funcionaba.
///
/// Windows: `installed` es el instalador descargado; se ejecuta en silencio
/// cuando la app ya terminó y él mismo vuelve a abrir peticiones.
pub fn relaunch_after_exit(installed: &std::path::Path) -> std::io::Result<()> {
    if WINDOWS {
        // El instalador no abre peticiones en modo silencioso (`skipifsilent`): lo abre
        // este script al terminar, con la misma ruta (la instalación conserva la carpeta).
        let exe = std::env::current_exe().unwrap_or_default();
        let ps = format!(
            // Al terminar el instalador se borra (estaba en la carpeta temporal).
            "Wait-Process -Id {} -Timeout 30 -ErrorAction SilentlyContinue; Start-Process -Wait -FilePath '{f}' -ArgumentList '/SILENT','/SUPPRESSMSGBOXES','/NORESTART'; Remove-Item -LiteralPath '{f}' -ErrorAction SilentlyContinue; Start-Process -FilePath '{e}'",
            std::process::id(),
            f = installed.display().to_string().replace('\'', "''"),
            e = exe.display().to_string().replace('\'', "''")
        );
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-Command", &ps]);
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        return hidden(&mut c).spawn().map(|_| ());
    }
    let open = if cfg!(target_os = "macos") { "exec open \"$2\"" } else { "exec \"$2\"" };
    let script = format!("i=0; while kill -0 \"$1\" 2>/dev/null && [ $i -lt 150 ]; do sleep 0.2; i=$((i+1)); done; {open}");
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(script).arg("sh").arg(std::process::id().to_string()).arg(installed);
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0); // que no le lleguen las señales del grupo de la app
    }
    c.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El relanzador espera a que termine el proceso y entonces abre el programa.
    #[cfg(target_os = "linux")]
    #[test]
    fn relanzar_al_salir() {
        let dir = std::env::temp_dir().join(format!("peticiones-relanzar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (prog, marca) = (dir.join("nuevo.sh"), dir.join("abierto"));
        std::fs::write(&prog, format!("#!/bin/sh\necho si > '{}'\n", marca.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Un hijo hace de «app vieja»: relanza y sale a los 0.5 s.
        let exe = std::env::current_exe().unwrap();
        let mut viejo = Command::new(exe)
            .args(["--exact", "update::tests::hijo_relanza", "--ignored", "--nocapture"])
            .env("PETICIONES_RELANZAR", &prog)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!marca.exists(), "se abrió antes de que terminara la vieja");
        viejo.wait().unwrap();
        let t = std::time::Instant::now();
        while !marca.exists() && t.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(marca.exists(), "no se abrió la versión nueva");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[ignore]
    fn hijo_relanza() {
        let Ok(p) = std::env::var("PETICIONES_RELANZAR") else { return };
        relaunch_after_exit(std::path::Path::new(&p)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
    }

    #[test]
    fn release_json() {
        let j = r#"[{"url":"x","tag_name": "v0.2.0","assets":[{"name":"a.zip","browser_download_url":"https://h/releases/download/v0.2.0/a.zip"},
            {"name":"peticiones-0.2.0-setup.exe","browser_download_url":"https://github.com/airylp/peticiones/releases/download/v0.2.0/peticiones-0.2.0-setup.exe"}]}]"#;
        assert_eq!(
            parse_release(j),
            Some(("v0.2.0".into(), Some("https://github.com/airylp/peticiones/releases/download/v0.2.0/peticiones-0.2.0-setup.exe".into())))
        );
        assert_eq!(parse_release(r#"[{"tag_name":"v1","assets":[]}]"#), Some(("v1".into(), None)));
        assert_eq!(parse_release("[]"), None);
    }

    #[test]
    fn ansi_codes_removed() {
        assert_eq!(strip_ansi("\u{1b}[1;34m==>\u{1b}[0m Instalado: /x/peticiones"), "==> Instalado: /x/peticiones");
    }

    /// Prueba completa (tarda: compila todo) con un install.sh local, instalando
    /// en una carpeta temporal:
    /// PETICIONES_INSTALL_SH=$PWD/install.sh PETICIONES_DESTINO=/tmp/x \
    ///   cargo test --release actualizar -- --ignored --nocapture
    #[test]
    #[ignore]
    fn actualizar_de_punta_a_punta() {
        let ctx = eframe::egui::Context::default();
        let u = Updater::check(ctx.clone());
        while u.stage() == Stage::Checking {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        println!("comprobación: {:?}", u.stage());
        u.run(ctx);
        let mut shown = 0;
        loop {
            {
                let log = u.log.lock().unwrap();
                for l in &log[shown..] {
                    println!("  | {l}");
                }
                shown = log.len();
            }
            match u.stage() {
                Stage::Running => std::thread::sleep(std::time::Duration::from_millis(200)),
                s => {
                    println!("final: {s:?}");
                    assert!(matches!(s, Stage::Done(_)));
                    break;
                }
            }
        }
    }
}
