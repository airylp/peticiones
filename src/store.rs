//! Lectura y escritura de espacios (carpetas) y de los secretos locales.
//!
//! Espacio en disco:
//! ```text
//! mi-api/
//!   espacio.toml          nombre, id, variables, entornos, auth común
//!   usuarios/listar.toml  una petición por archivo; las carpetas agrupan
//!   salud.toml
//! ```
//! Secretos: `<config>/secretos/<id>.toml`, fuera de la carpeta compartida.

use crate::model::{Request, SpaceFile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

pub const SPACE_FILE: &str = "espacio.toml";

/// Carpeta de configuración: `~/Library/Application Support/peticiones`
/// (macOS), `%APPDATA%\peticiones` (Windows) o `~/.config/peticiones`.
pub fn config_dir() -> Option<PathBuf> {
    let env = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support"))
    } else if cfg!(windows) {
        env("APPDATA")
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    };
    Some(base?.join("peticiones"))
}

/// Identificador aleatorio de 16 bytes en hex.
pub fn new_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("sin generador aleatorio");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Escribe por un temporal + rename: si se corta a medias no deja el archivo roto.
pub fn write_atomic(path: &Path, data: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

pub fn load_space_file(dir: &Path) -> io::Result<SpaceFile> {
    let s = std::fs::read_to_string(dir.join(SPACE_FILE))?;
    let mut f: SpaceFile = toml::from_str(&s).map_err(bad)?;
    if f.id.is_empty() {
        f.id = new_id();
    }
    if f.name.is_empty() {
        f.name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
    Ok(f)
}

pub fn save_space_file(dir: &Path, f: &SpaceFile) -> io::Result<()> {
    write_atomic(&dir.join(SPACE_FILE), &toml::to_string_pretty(f).map_err(bad)?)
}

pub fn load_request(path: &Path) -> io::Result<Request> {
    let s = std::fs::read_to_string(path)?;
    let mut r: Request = toml::from_str(&s).map_err(bad)?;
    if r.name.is_empty() {
        r.name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
    Ok(r)
}

pub fn save_request(path: &Path, r: &Request) -> io::Result<()> {
    write_atomic(path, &toml::to_string_pretty(r).map_err(bad)?)
}

/// Nombre de archivo a partir del nombre de la petición: sin caracteres que
/// den problemas en algún sistema.
pub fn file_name_for(name: &str) -> String {
    let mut s: String = name.trim().chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '-' }).collect();
    s = s.trim_matches(['.', ' ']).to_string();
    if s.is_empty() {
        s = "peticion".into();
    }
    s.truncate(80);
    s
}

/// Ruta libre `dir/base.toml`, `dir/base 2.toml`…
pub fn free_path(dir: &Path, base: &str) -> PathBuf {
    let base = file_name_for(base);
    let mut p = dir.join(format!("{base}.toml"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{base} {n}.toml"));
        n += 1;
    }
    p
}

/// Árbol de un espacio: carpetas y peticiones, en orden alfabético
/// (carpetas primero).
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Folder { name: String, path: PathBuf, children: Vec<Node> },
    Req { name: String, method: String, path: PathBuf },
}

impl Node {
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        match self {
            Node::Folder { path, .. } | Node::Req { path, .. } => path,
        }
    }
}

/// Lee el árbol sin cargar el contenido completo de cada petición más que
/// para su nombre y método. Ignora ocultos y `espacio.toml`.
pub fn scan_tree(dir: &Path) -> Vec<Node> {
    let mut folders = Vec::new();
    let mut reqs = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == SPACE_FILE {
            continue;
        }
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            folders.push(Node::Folder { children: scan_tree(&path), name, path });
        } else if ft.is_file() && path.extension().is_some_and(|x| x == "toml") {
            let (name, method) = match load_request(&path) {
                Ok(r) => (r.name, r.method),
                Err(_) => (name, "?".into()),
            };
            reqs.push(Node::Req { name, method, path });
        }
    }
    let key = |n: &Node| match n {
        Node::Folder { name, path, .. } | Node::Req { name, path, .. } => (name.to_lowercase(), path.file_stem().map(|s| s.to_os_string())),
    };
    folders.sort_by_key(key);
    reqs.sort_by_key(key);
    folders.extend(reqs);
    folders
}

/// Crea un espacio vacío en `dir` (que puede existir) con un ejemplo.
pub fn create_space(dir: &Path, name: &str) -> io::Result<SpaceFile> {
    std::fs::create_dir_all(dir)?;
    if dir.join(SPACE_FILE).exists() {
        return load_space_file(dir);
    }
    let f = SpaceFile {
        id: new_id(),
        name: name.into(),
        vars: vec![crate::model::Var { name: "base".into(), value: "https://httpbin.org".into(), secret: false }],
        auth: crate::model::Auth::None,
        ..Default::default()
    };
    save_space_file(dir, &f)?;
    let ejemplo = Request { name: "Ejemplo".into(), url: "{{base}}/get".into(), ..Default::default() };
    save_request(&dir.join("Ejemplo.toml"), &ejemplo)?;
    Ok(f)
}

/// Token OAuth guardado (para no pedirlo en cada envío).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_type: String,
    /// Segundos Unix en que caduca (0 = desconocido).
    #[serde(default)]
    pub expires_at: u64,
}

/// Secretos de un espacio en este equipo: valores de las variables secretas
/// (del espacio y de cada entorno) y tokens OAuth obtenidos.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Secrets {
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    #[serde(default)]
    pub entornos: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub tokens: BTreeMap<String, Token>,
}

fn secrets_path(id: &str) -> Option<PathBuf> {
    // El id viene de un archivo que puede ser ajeno: solo hex.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(config_dir()?.join("secretos").join(format!("{id}.toml")))
}

pub fn load_secrets(id: &str) -> Secrets {
    secrets_path(id).and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_secrets(id: &str, s: &Secrets) -> io::Result<()> {
    let p = secrets_path(id).ok_or_else(|| bad("sin carpeta de configuración"))?;
    write_atomic(&p, &toml::to_string_pretty(s).map_err(bad)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Sesión: espacios abiertos, entorno elegido y petición activa.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(rename = "espacios", default)]
    pub spaces: Vec<SessionSpace>,
    #[serde(rename = "activa", default, skip_serializing_if = "Option::is_none")]
    pub active: Option<PathBuf>,
    #[serde(rename = "abiertas", default, skip_serializing_if = "Vec::is_empty")]
    pub open: Vec<PathBuf>,
    #[serde(rename = "idioma", default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(rename = "tema", default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(rename = "espera_segundos", default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(rename = "seguir_redirecciones", default, skip_serializing_if = "Option::is_none")]
    pub follow_redirects: Option<bool>,
    #[serde(rename = "verificar_tls", default, skip_serializing_if = "Option::is_none")]
    pub verify_tls: Option<bool>,
    /// Avisar de versiones nuevas (mira GitHub al abrir y cada 6 horas); `None` = sí.
    #[serde(rename = "buscar_actualizaciones", default, skip_serializing_if = "Option::is_none")]
    pub check_updates: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSpace {
    #[serde(rename = "ruta")]
    pub path: PathBuf,
    #[serde(rename = "entorno", default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
}

fn session_path() -> Option<PathBuf> {
    Some(config_dir()?.join("sesion.toml"))
}

pub fn load_session() -> Session {
    session_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_session(s: &Session) -> io::Result<()> {
    let p = session_path().ok_or_else(|| bad("sin carpeta de configuración"))?;
    write_atomic(&p, &toml::to_string_pretty(s).map_err(bad)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("peticiones-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn space_create_scan_and_save() {
        let d = tmp("espacio");
        let f = create_space(&d, "Mi API").unwrap();
        assert_eq!(f.id.len(), 32);
        std::fs::create_dir_all(d.join("usuarios")).unwrap();
        let r = Request { name: "Listar".into(), url: "{{base}}/users".into(), ..Default::default() };
        save_request(&free_path(&d.join("usuarios"), "Listar"), &r).unwrap();
        save_request(&free_path(&d.join("usuarios"), "Listar"), &r).unwrap();
        let tree = scan_tree(&d);
        let Node::Folder { name, children, .. } = &tree[0] else { panic!("{tree:?}") };
        assert_eq!(name, "usuarios");
        assert_eq!(children.len(), 2);
        assert!(children[1].path().ends_with("Listar 2.toml"));
        assert!(matches!(&tree[1], Node::Req { name, .. } if name == "Ejemplo"));
        assert_eq!(load_space_file(&d).unwrap(), f);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name_for("a/b:c?"), "a-b-c-");
        assert_eq!(file_name_for(" .. "), "peticion");
        assert!(secrets_path("../x").is_none());
    }
}
