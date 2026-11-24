//! Interfaz: panel de espacios a la izquierda, pestañas de peticiones con su
//! editor y la respuesta abajo.

use crate::beautify;
use crate::icons::{Icon, IconButton, Tone};
use crate::http::{self, Options, Outcome};
use crate::model::{Auth, BodyKind, Environment, KeyPlace, METHODS, OAuthFlow, Pair, Request, SpaceFile, Var};
use crate::store::{self, Node, Secrets, Session, SessionSpace};
use crate::vars::Scope;
use eframe::egui::{self, *};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Guardado automático tras dejar de escribir.
const AUTOSAVE_AFTER: Duration = Duration::from_millis(800);
/// Cuerpo de respuesta mostrado como texto editable (seleccionable) hasta aquí;
/// más grande se pinta por líneas visibles.
const SMALL_BODY: usize = 256 << 10;

struct Space {
    dir: PathBuf,
    file: SpaceFile,
    secrets: Secrets,
    tree: Vec<Node>,
    env: Option<String>,
    expanded: bool,
    /// Error al leer `espacio.toml`.
    error: Option<String>,
}

impl Space {
    fn load(dir: PathBuf, env: Option<String>) -> Space {
        let (file, error) = match store::load_space_file(&dir) {
            Ok(f) => (f, None),
            Err(e) => (SpaceFile { id: String::new(), name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), ..Default::default() }, Some(e.to_string())),
        };
        let secrets = store::load_secrets(&file.id);
        let tree = store::scan_tree(&dir);
        let env = env.filter(|e| file.envs.iter().any(|x| &x.name == e));
        Space { dir, file, secrets, tree, env, expanded: true, error }
    }

    fn scope(&self) -> Scope {
        Scope::new(&self.file, self.env.as_deref(), &self.secrets)
    }

    fn save(&mut self) -> Result<(), String> {
        store::save_space_file(&self.dir, &self.file).map_err(|e| e.to_string())?;
        store::save_secrets(&self.file.id, &self.secrets).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReqTab {
    Params,
    Headers,
    Body,
    Auth,
    Notes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RespTab {
    Body,
    Headers,
    Sent,
}

struct Tab {
    space: PathBuf,
    path: PathBuf,
    req: Request,
    saved: Request,
    changed_at: Option<Instant>,
    sub: ReqTab,
    resp_tab: RespTab,
    pretty: bool,
    outcome: Option<Outcome>,
    pending: Option<(Receiver<Outcome>, Instant)>,
    /// Texto del cuerpo de la respuesta ya formateado: (bonito, texto, líneas).
    view: Option<(bool, String, Vec<(usize, usize)>)>,
    save_error: Option<String>,
}

impl Tab {
    fn title(&self) -> String {
        let n = if self.req.name.is_empty() { t!("Sin nombre").to_string() } else { self.req.name.clone() };
        if self.req != self.saved { format!("● {n}") } else { n }
    }
}

enum Dialog {
    /// Crear espacio: nombre y carpeta padre.
    NewSpace { name: String, parent: String },
    /// Pedir un nombre (nueva petición/carpeta, renombrar).
    Name { title: &'static str, value: String, action: NameAction },
    /// Borrar un archivo o carpeta.
    Delete { path: PathBuf },
    /// Ajustes del espacio (variables, entornos, auth, cabeceras).
    SpaceSettings { dir: PathBuf, file: SpaceFile, secrets: Secrets, env_sel: usize },
    Settings,
    Import(ImportDialog),
}

/// Diálogo «Importar»: texto pegado, archivo o URL; destino nuevo o existente.
pub struct ImportDialog {
    text: String,
    /// Archivo leído (su contenido no se muestra en el editor si es grande).
    file: Option<PathBuf>,
    url: String,
    parsed: Option<Result<crate::import::Imported, String>>,
    /// Destino: None = espacio nuevo; Some(carpeta del espacio) = existente.
    target: Option<PathBuf>,
    name: String,
    parent: String,
    /// Subcarpeta dentro de un espacio existente (vacía = raíz).
    folder: String,
}

impl ImportDialog {
    fn new(target: Option<PathBuf>) -> ImportDialog {
        let parent = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("peticiones").display().to_string()).unwrap_or_default();
        ImportDialog { text: String::new(), file: None, url: String::new(), parsed: None, target, name: String::new(), parent, folder: String::new() }
    }

    fn set_text(&mut self, text: String, file: Option<PathBuf>) {
        self.text = text;
        self.file = file;
        self.reparse();
    }

    fn reparse(&mut self) {
        self.parsed = (!self.text.trim().is_empty()).then(|| crate::import::parse(&self.text));
        if let Some(Ok(imp)) = &self.parsed {
            let n = if imp.name.is_empty() { t!("Importado").to_string() } else { imp.name.clone() };
            self.name = n.clone();
            // Una sola petición (curl) va a la raíz; una colección, a su carpeta.
            self.folder = if imp.count() > 1 || imp.items.iter().any(|i| matches!(i, crate::import::Item::Folder { .. })) { n } else { String::new() };
        }
    }
}

#[derive(Clone)]
enum NameAction {
    NewRequest { dir: PathBuf },
    NewFolder { dir: PathBuf },
    Rename { path: PathBuf },
}

/// Diálogo nativo en curso (rfd), esperado en otro hilo.
struct PendingPick {
    result: Arc<Mutex<Option<Option<PathBuf>>>>,
    purpose: Pick,
}

#[derive(Clone, Copy, PartialEq)]
enum Pick {
    OpenSpace,
    NewSpaceParent,
    SaveBody,
    /// Archivo a importar (curl, Postman, OpenAPI, WSDL).
    ImportFile,
    /// Carpeta padre de un espacio nuevo desde el diálogo de importar.
    ImportParent,
    /// Archivo .json donde exportar el espacio a Postman.
    ExportPostman,
}

pub struct App {
    spaces: Vec<Space>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    dialog: Option<Dialog>,
    pick: Option<PendingPick>,
    session: Session,
    session_dirty: bool,
    last_session_save: Instant,
    opts: Options,
    /// Modo captura (pruebas): ruta de salida y fotogramas pintados.
    screenshot: Option<String>,
    frames: u32,
    /// Ventana «Acerca de» y textura del logotipo.
    show_about: bool,
    logo: Option<TextureHandle>,
    /// Ventana «Atajos de teclado».
    show_keys: bool,
    /// Actualizar: ventana, comprobación en segundo plano y avisos flotantes.
    updater: Option<crate::update::Updater>,
    show_updater: bool,
    vigia: crate::estilo::Vigia,
    avisos: crate::estilo::Avisos,
    /// Espacio que se exporta a Postman (mientras se elige el archivo).
    export_dir: Option<PathBuf>,
    /// Descarga en curso de una URL para importar.
    fetch: Option<Receiver<Result<String, String>>>,
    /// Pruebas (PETICIONES_RATON): paso actual y posición del ratón simulado.
    test_step: usize,
    test_mouse: Pos2,
}

fn method_color(m: &str, dark: bool) -> Color32 {
    let (l, d) = match m {
        "GET" => ((0x1a, 0x7f, 0x37), (0x6b, 0xd3, 0x88)),
        "POST" => ((0xa0, 0x62, 0x00), (0xf0, 0xb4, 0x50)),
        "PUT" => ((0x1f, 0x5f, 0xbf), (0x7a, 0xae, 0xff)),
        "PATCH" => ((0x7a, 0x3d, 0xb8), (0xc0, 0x96, 0xf5)),
        "DELETE" => ((0xb3, 0x26, 0x1e), (0xff, 0x80, 0x78)),
        _ => ((0x55, 0x5b, 0x66), (0xa8, 0xb0, 0xbd)),
    };
    let (r, g, b) = if dark { d } else { l };
    Color32::from_rgb(r, g, b)
}

fn status_color(s: u16, dark: bool) -> Color32 {
    match (s / 100, dark) {
        (2, false) => Color32::from_rgb(0x1a, 0x7f, 0x37),
        (2, true) => Color32::from_rgb(0x6b, 0xd3, 0x88),
        (3, false) => Color32::from_rgb(0x1f, 0x5f, 0xbf),
        (3, true) => Color32::from_rgb(0x7a, 0xae, 0xff),
        (4, false) => Color32::from_rgb(0xa0, 0x62, 0x00),
        (4, true) => Color32::from_rgb(0xf0, 0xb4, 0x50),
        (_, false) => Color32::from_rgb(0xb3, 0x26, 0x1e),
        (_, true) => Color32::from_rgb(0xff, 0x80, 0x78),
    }
}

fn fmt_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1 << 20 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    }
}

fn fmt_ms(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 { format!("{ms:.0} ms") } else { format!("{:.2} s", ms / 1000.0) }
}

/// Tabla editable de pares con casilla. La última fila (vacía) es para
/// escribir una nueva: vive en la memoria de egui y solo pasa a `pairs` al
/// escribir en ella (así abrir la pestaña no modifica la petición).
/// Devuelve true si cambió algo.
fn pairs_editor(ui: &mut Ui, id: &str, pairs: &mut Vec<Pair>, key_hint: &str, value_hint: &str) -> bool {
    let mut changed = false;
    let mut remove = None;
    let w = ui.available_width();
    let new_id = ui.scope_id().with(("nueva", id));
    let mut new: (String, String) = ui.data_mut(|d| d.get_temp(new_id)).unwrap_or_default();
    // Filas con anchos fijos (en un Grid los TextEdit no pasan del ancho de
    // la columna del fotograma anterior y se quedan diminutos).
    let h = ui.spacing().interact_size.y;
    let kw = ((w - 70.0) * 0.36).max(110.0);
    let vw = (w - 70.0 - kw).max(150.0);
    for (i, p) in pairs.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            changed |= ui.add(Checkbox::without_text(&mut p.enabled)).changed();
            changed |= ui.add_sized([kw, h], TextEdit::singleline(&mut p.name).hint_text(key_hint).font(TextStyle::Monospace)).changed();
            changed |= ui.add_sized([vw, h], TextEdit::singleline(&mut p.value).hint_text(value_hint).font(TextStyle::Monospace)).changed();
            if ui.small_button("✖").on_hover_text(t!("Quitar")).clicked() {
                remove = Some(i);
            }
        });
    }
    ui.horizontal(|ui| {
        ui.add_enabled(false, Checkbox::without_text(&mut true));
        let a = ui.add_sized([kw, h], TextEdit::singleline(&mut new.0).hint_text(key_hint).font(TextStyle::Monospace));
        let b = ui.add_sized([vw, h], TextEdit::singleline(&mut new.1).hint_text(value_hint).font(TextStyle::Monospace));
        if a.changed() || b.changed() {
            pairs.push(Pair { name: std::mem::take(&mut new.0), value: std::mem::take(&mut new.1), enabled: true });
            // La fila nueva ocupa en el siguiente fotograma el sitio (y el id
            // automático) de esta: el foco y el cursor siguen ahí.
            changed = true;
        }
    });
    ui.data_mut(|d| d.insert_temp(new_id, new));
    if let Some(i) = remove {
        pairs.remove(i);
        changed = true;
    }
    changed
}

/// Quita la fila vacía que añade `pairs_editor` antes de guardar.
fn trim_pairs(v: &mut Vec<Pair>) {
    v.retain(|p| !(p.name.is_empty() && p.value.is_empty()));
}

fn clean(r: &Request) -> Request {
    let mut r = r.clone();
    trim_pairs(&mut r.params);
    trim_pairs(&mut r.headers);
    trim_pairs(&mut r.body.fields);
    r
}

/// Campo de texto de una línea con ancho completo (para auth y similares).
fn field(ui: &mut Ui, label: &str, value: &mut String, password: bool) -> bool {
    ui.label(label);
    // Una referencia a variable no es un secreto: se ve tal cual.
    let hide = password && !value.contains("{{");
    let r = ui.add(TextEdit::singleline(value).password(hide).desired_width(f32::INFINITY).font(TextStyle::Monospace));
    ui.end_row();
    r.changed()
}

fn auth_editor(ui: &mut Ui, a: &mut Auth, for_space: bool) -> bool {
    let mut changed = false;
    ComboBox::from_id_salt(("auth", for_space)).selected_text(a.label()).width(220.0).show_ui(ui, |ui| {
        for c in Auth::choices(for_space) {
            let label = c.label();
            if ui.selectable_label(a.same_kind(&c), label).clicked() && !a.same_kind(&c) {
                *a = c;
                changed = true;
            }
        }
    });
    ui.add_space(6.0);
    let hint = t!("Consejo: usa {{variables}} secretas para contraseñas y tokens; así el espacio se comparte sin credenciales.");
    Grid::new(("authgrid", for_space)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| match a {
        Auth::Inherit => {
            ui.label(t!("Se usa la autenticación configurada en el espacio."));
            ui.end_row();
        }
        Auth::None => {}
        Auth::Basic { user, password } | Auth::Digest { user, password } => {
            changed |= field(ui, t!("Usuario"), user, false);
            changed |= field(ui, t!("Contraseña"), password, true);
        }
        Auth::Bearer { token } => {
            changed |= field(ui, "Token", token, true);
        }
        Auth::ApiKey { name, value, place } => {
            changed |= field(ui, t!("Nombre"), name, false);
            changed |= field(ui, t!("Valor"), value, true);
            ui.label(t!("Enviar en"));
            ui.horizontal(|ui| {
                changed |= ui.radio_value(place, KeyPlace::Header, t!("Cabecera")).changed();
                changed |= ui.radio_value(place, KeyPlace::Query, t!("Parámetro de la URL")).changed();
            });
            ui.end_row();
        }
        Auth::OAuth2(o) => {
            ui.label(t!("Flujo"));
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut o.flow, OAuthFlow::ClientCredentials, "Client credentials").changed();
                changed |= ui.radio_value(&mut o.flow, OAuthFlow::AuthorizationCode, "Authorization code + PKCE").changed();
            });
            ui.end_row();
            if o.flow == OAuthFlow::AuthorizationCode {
                changed |= field(ui, t!("URL de autorización"), &mut o.auth_url, false);
            }
            changed |= field(ui, t!("URL del token"), &mut o.token_url, false);
            changed |= field(ui, "Client ID", &mut o.client_id, false);
            changed |= field(ui, "Client secret", &mut o.client_secret, true);
            changed |= field(ui, t!("Alcance (scope)"), &mut o.scope, false);
            changed |= field(ui, t!("Audiencia"), &mut o.audience, false);
            if o.flow == OAuthFlow::AuthorizationCode {
                changed |= field(ui, t!("URL de retorno"), &mut o.redirect_uri, false);
                ui.label("");
                ui.weak(t!("Vacía = http://127.0.0.1:<puerto libre>/callback. Regístrala tal cual en el proveedor."));
                ui.end_row();
            }
            ui.label("");
            changed |= ui.checkbox(&mut o.basic_credentials, t!("Enviar client ID/secret con Basic")).changed();
            ui.end_row();
        }
    });
    if !matches!(a, Auth::Inherit | Auth::None) {
        ui.add_space(6.0);
        ui.weak(hint);
    }
    changed
}

/// Líneas (inicio, fin) de un texto, para pintar solo las visibles.
fn line_spans(s: &str) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            v.push((start, i));
            start = i + 1;
        }
    }
    v.push((start, s.len()));
    v
}

/// Texto que se muestra del cuerpo: bonito si es JSON/XML y se puede.
fn body_text(r: &http::Response, pretty: bool) -> String {
    let raw = String::from_utf8_lossy(&r.body);
    if !pretty {
        return raw.into_owned();
    }
    let ct = r.header("content-type").unwrap_or("").to_ascii_lowercase();
    let t = raw.trim_start();
    if ct.contains("json") || t.starts_with('{') || t.starts_with('[') {
        if let Ok(s) = beautify::format_json(&raw, "  ") {
            return s;
        }
    }
    if ct.contains("xml") || ct.contains("html") || t.starts_with('<') {
        if let Ok(s) = beautify::format_xml(&raw, "  ", ct.contains("html")) {
            return s;
        }
    }
    raw.into_owned()
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> App {
        let screenshot = std::env::var("PETICIONES_SCREENSHOT").ok();
        let session = store::load_session();
        if let Some(l) = session.lang.as_deref().and_then(crate::i18n::Lang::from_code).or_else(crate::i18n::system) {
            crate::i18n::set(l);
        }
        cc.egui_ctx.set_theme(match session.theme.as_deref() {
            Some("claro") => ThemePreference::Light,
            Some("oscuro") => ThemePreference::Dark,
            _ => ThemePreference::System,
        });
        crate::estilo::aplicar(&cc.egui_ctx);
        if let Ok(t) = std::env::var("PETICIONES_TEMA") {
            cc.egui_ctx.set_theme(if t == "oscuro" { ThemePreference::Dark } else { ThemePreference::Light });
        }
        let mut opts = Options::default();
        if let Some(s) = session.timeout_secs {
            opts.timeout = Duration::from_secs(s.max(1));
        }
        opts.follow_redirects = session.follow_redirects.unwrap_or(true);
        opts.verify_tls = session.verify_tls.unwrap_or(true);
        let spaces = session.spaces.iter().filter(|s| s.path.join(store::SPACE_FILE).exists()).map(|s| Space::load(s.path.clone(), s.env.clone())).collect();
        let mut app = App {
            spaces,
            tabs: vec![],
            active: None,
            dialog: None,
            pick: None,
            session_dirty: false,
            last_session_save: Instant::now(),
            opts,
            screenshot,
            frames: 0,
            show_about: false,
            logo: None,
            show_keys: false,
            updater: None,
            show_updater: false,
            vigia: Default::default(),
            avisos: Default::default(),
            export_dir: None,
            fetch: None,
            test_step: 0,
            test_mouse: Pos2::ZERO,
            session,
        };
        let open = app.session.open.clone();
        for p in open {
            app.open_request(&p);
        }
        if let Some(a) = app.session.active.clone() {
            app.active = app.tabs.iter().position(|t| t.path == a).or(app.active);
        }
        app.session_dirty = false;
        app
    }

    /// Aviso flotante (común a las tres apps: `estilo::Avisos`).
    fn msg(&mut self, s: impl Into<String>) {
        self.avisos.auto(s);
    }

    fn open_updater(&mut self, ctx: &Context) {
        let running = self.updater.as_ref().is_some_and(|u| u.stage() == crate::update::Stage::Running);
        if !running {
            self.updater = Some(crate::update::Updater::check(ctx.clone()));
        }
        self.show_updater = true;
    }

    fn updater_window(&mut self, ctx: &Context) {
        let Some(u) = &self.updater else { return };
        if !self.show_updater {
            return;
        }
        let mut open = true;
        match crate::estilo::ventana_actualizar(ctx, u, &mut open, None) {
            crate::estilo::AccionActualizar::Ejecutar => u.run(ctx.clone()),
            crate::estilo::AccionActualizar::Reiniciar(p) => match crate::update::relaunch_after_exit(&p) {
                Ok(()) => ctx.send_viewport_cmd(ViewportCommand::Close),
                Err(e) => self.avisos.error(tf!("No se pudo abrir la versión nueva: {e}", e = e)),
            },
            crate::estilo::AccionActualizar::Nada => {}
        }
        if !open {
            self.show_updater = false;
        }
    }

    fn space_of(&self, dir: &Path) -> Option<usize> {
        self.spaces.iter().position(|s| s.dir == dir)
    }

    fn add_space(&mut self, dir: PathBuf) {
        if let Some(i) = self.space_of(&dir) {
            self.spaces[i].expanded = true;
            return;
        }
        if !dir.join(store::SPACE_FILE).exists() {
            // Una carpeta cualquiera se convierte en espacio (sin tocar lo que haya).
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let f = SpaceFile { id: store::new_id(), name, auth: Auth::None, ..Default::default() };
            if let Err(e) = store::save_space_file(&dir, &f) {
                self.msg(tf!("No se pudo crear el espacio: {e}", e = e));
                return;
            }
        }
        let s = Space::load(dir, None);
        if let Some(e) = &s.error {
            self.msg(tf!("espacio.toml no válido: {e}", e = e));
        }
        // Si no tenía id se le pone uno ahora (para sus secretos).
        if s.error.is_none() && !std::fs::read_to_string(s.dir.join(store::SPACE_FILE)).unwrap_or_default().contains("id =") {
            let _ = store::save_space_file(&s.dir, &s.file);
        }
        self.spaces.push(s);
        self.session_dirty = true;
    }

    fn refresh_tree(&mut self, dir: &Path) {
        if let Some(s) = self.spaces.iter_mut().find(|s| dir.starts_with(&s.dir)) {
            s.tree = store::scan_tree(&s.dir);
        }
    }

    fn open_request(&mut self, path: &Path) {
        if let Some(i) = self.tabs.iter().position(|t| t.path == path) {
            self.active = Some(i);
            return;
        }
        let Some(space) = self.spaces.iter().filter(|s| path.starts_with(&s.dir)).max_by_key(|s| s.dir.as_os_str().len()).map(|s| s.dir.clone()) else { return };
        match store::load_request(path) {
            Ok(req) => {
                self.tabs.push(Tab {
                    space,
                    path: path.to_path_buf(),
                    saved: req.clone(),
                    req,
                    changed_at: None,
                    sub: ReqTab::Params,
                    resp_tab: RespTab::Body,
                    pretty: true,
                    outcome: None,
                    pending: None,
                    view: None,
                    save_error: None,
                });
                self.active = Some(self.tabs.len() - 1);
                self.session_dirty = true;
            }
            Err(e) => self.msg(tf!("No se pudo abrir {f}: {e}", f = path.display(), e = e)),
        }
    }

    fn save_tab(&mut self, i: usize) {
        let t = &mut self.tabs[i];
        let r = clean(&t.req);
        match store::save_request(&t.path, &r) {
            Ok(()) => {
                let renamed = r.name != t.saved.name || r.method != t.saved.method;
                t.saved = t.req.clone();
                t.changed_at = None;
                t.save_error = None;
                if renamed {
                    let p = t.path.clone();
                    self.refresh_tree(&p);
                }
            }
            Err(e) => t.save_error = Some(e.to_string()),
        }
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs[i].req != self.tabs[i].saved {
            self.save_tab(i);
        }
        self.tabs.remove(i);
        self.active = match self.active {
            Some(a) if a > i => Some(a - 1),
            Some(a) if a == i => (!self.tabs.is_empty()).then(|| i.min(self.tabs.len() - 1)),
            a => a,
        };
        self.session_dirty = true;
    }

    fn send(&mut self, i: usize, ctx: &Context) {
        let t = &self.tabs[i];
        if t.pending.is_some() {
            return;
        }
        let Some(si) = self.space_of(&t.space) else { return };
        let sp = &self.spaces[si];
        let prepared = http::prepare(&clean(&t.req), &sp.file, &sp.scope());
        let cached = match &prepared.auth {
            Auth::OAuth2(c) => sp.secrets.tokens.get(&crate::auth::token_key(c)).cloned(),
            _ => None,
        };
        let opts = self.opts.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let out = http::send(prepared, &opts, cached);
            let _ = tx.send(out);
            ctx.request_repaint();
        });
        self.tabs[i].pending = Some((rx, Instant::now()));
    }

    /// Recoge las respuestas que hayan llegado.
    fn poll_responses(&mut self) {
        for i in 0..self.tabs.len() {
            let Some((rx, _)) = &self.tabs[i].pending else { continue };
            let Ok(out) = rx.try_recv() else { continue };
            self.tabs[i].pending = None;
            if let Some((key, tok)) = &out.new_token {
                if let Some(si) = self.space_of(&self.tabs[i].space.clone()) {
                    let s = &mut self.spaces[si];
                    s.secrets.tokens.insert(key.clone(), tok.clone());
                    let _ = store::save_secrets(&s.file.id, &s.secrets);
                }
            }
            self.tabs[i].view = None;
            self.tabs[i].outcome = Some(out);
        }
    }

    fn autosave(&mut self) {
        for i in 0..self.tabs.len() {
            if self.tabs[i].changed_at.is_some_and(|c| c.elapsed() >= AUTOSAVE_AFTER) {
                self.save_tab(i);
            }
        }
    }

    fn save_session(&mut self, force: bool) {
        if self.screenshot.is_some() || !(self.session_dirty || force) {
            return;
        }
        self.session.spaces = self.spaces.iter().map(|s| SessionSpace { path: s.dir.clone(), env: s.env.clone() }).collect();
        self.session.open = self.tabs.iter().map(|t| t.path.clone()).collect();
        self.session.active = self.active.and_then(|a| self.tabs.get(a)).map(|t| t.path.clone());
        let _ = store::save_session(&self.session);
        self.session_dirty = false;
        self.last_session_save = Instant::now();
    }

    /// Abre un diálogo nativo (carpeta, abrir o guardar según `purpose`) y
    /// espera la respuesta en otro hilo. El diálogo se crea aquí (hilo principal).
    fn pick_folder(&mut self, purpose: Pick, ctx: &Context) {
        if self.pick.is_some() {
            return;
        }
        type Fut = std::pin::Pin<Box<dyn std::future::Future<Output = Option<rfd::FileHandle>> + Send>>;
        let d = rfd::AsyncFileDialog::new();
        let fut: Fut = match purpose {
            Pick::OpenSpace => Box::pin(d.set_title(t!("Abrir espacio (carpeta)")).pick_folder()),
            Pick::NewSpaceParent | Pick::ImportParent => Box::pin(d.set_title(t!("Carpeta donde crear el espacio")).pick_folder()),
            Pick::SaveBody => Box::pin(d.set_title(t!("Guardar la respuesta")).set_file_name("respuesta").save_file()),
            Pick::ImportFile => Box::pin(d.set_title(t!("Importar")).add_filter(t!("Colecciones y especificaciones"), &["json", "yaml", "yml", "wsdl", "xml", "txt", "sh"]).add_filter(t!("Todos"), &["*"]).pick_file()),
            Pick::ExportPostman => {
                let name = self.export_dir.as_ref().and_then(|d| self.space_of(d)).map(|i| store::file_name_for(&self.spaces[i].file.name)).unwrap_or_else(|| "coleccion".into());
                Box::pin(d.set_title(t!("Exportar a Postman")).set_file_name(format!("{name}.postman_collection.json")).save_file())
            }
        };
        let result = Arc::new(Mutex::new(None));
        let r = result.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            *r.lock().unwrap() = Some(block_on(fut).map(|h| h.path().to_path_buf()));
            ctx.request_repaint();
        });
        self.pick = Some(PendingPick { result, purpose });
    }

    fn poll_pick(&mut self) {
        let Some(done) = self.pick.as_ref().and_then(|p| p.result.lock().unwrap().take()) else { return };
        let purpose = self.pick.take().unwrap().purpose;
        let Some(path) = done else { return };
        match purpose {
            Pick::OpenSpace => self.add_space(path),
            Pick::NewSpaceParent => {
                if let Some(Dialog::NewSpace { parent, .. }) = &mut self.dialog {
                    *parent = path.display().to_string();
                }
            }
            Pick::ImportFile => match std::fs::read(&path) {
                Ok(b) => {
                    let text = String::from_utf8_lossy(&b).into_owned();
                    if let Some(Dialog::Import(d)) = &mut self.dialog {
                        d.set_text(text, Some(path));
                    }
                }
                Err(e) => self.msg(e.to_string()),
            },
            Pick::ImportParent => {
                if let Some(Dialog::Import(d)) = &mut self.dialog {
                    d.parent = path.display().to_string();
                }
            }
            Pick::ExportPostman => {
                if let Some(i) = self.export_dir.take().and_then(|d| self.space_of(&d)) {
                    let s = &self.spaces[i];
                    match std::fs::write(&path, crate::import::export_postman(&s.file, &s.dir)) {
                        Ok(()) => self.msg(tf!("Exportado a {f}", f = path.display())),
                        Err(e) => self.msg(e.to_string()),
                    }
                }
            }
            Pick::SaveBody => {
                if let Some(r) = self.active.and_then(|a| self.tabs[a].outcome.as_ref()).and_then(|o| o.response.as_ref()) {
                    match std::fs::write(&path, &r.body) {
                        Ok(()) => self.msg(tf!("Guardado en {f}", f = path.display())),
                        Err(e) => self.msg(e.to_string()),
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ barra superior

    fn new_space_dialog(&mut self) {
        let parent = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| PathBuf::from(h).join("peticiones").display().to_string()).unwrap_or_default();
        self.dialog = Some(Dialog::NewSpace { name: String::new(), parent });
    }

    fn import_dialog(&mut self) {
        let target = self.active.map(|a| self.tabs[a].space.clone()).or_else(|| self.spaces.first().map(|s| s.dir.clone()));
        self.dialog = Some(Dialog::Import(ImportDialog::new(target)));
    }

    /// Barra de estado (común: atajos y «Acerca de» a la derecha): espacios,
    /// entorno y archivo de la petición activa.
    fn status_bar(&mut self, ui: &mut Ui) {
        let active = self.active.and_then(|a| self.tabs.get(a)).map(|t| (t.path.clone(), t.space.clone()));
        let n = self.spaces.len();
        let env = active.as_ref().and_then(|(_, sp)| self.space_of(sp)).and_then(|i| self.spaces[i].env.clone());
        let mut copied = false;
        crate::estilo::barra_estado(ui, &mut self.show_keys, &mut self.show_about, |ui| {
            ui.small(tf!("{n} espacios", n = n));
            if let Some(e) = env {
                ui.separator();
                ui.small(tf!("Entorno: {e}", e = e));
            }
            if let Some((path, _)) = active {
                ui.separator();
                let p = path.display().to_string();
                if ui.add(Label::new(RichText::new(&p).small().weak()).sense(Sense::click()).truncate()).on_hover_text(t!("Archivo de la petición · clic: copiar la ruta")).clicked() {
                    ui.ctx().copy_text(p);
                    copied = true;
                }
            }
        });
        if copied {
            self.avisos.info(t!("Ruta copiada"));
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.add(IconButton::new(Icon::Plus, Tone::File).label(t!("Espacio"))).on_hover_text(t!("Crear un espacio nuevo")).clicked() {
                self.new_space_dialog();
            }
            if ui.add(IconButton::new(Icon::Download, Tone::File).label(t!("Importar…"))).on_hover_text(t!("curl, Postman, OpenAPI/Swagger o WSDL")).clicked() {
                self.import_dialog();
            }
            if ui.add(IconButton::new(Icon::Open, Tone::File).label(t!("Abrir…"))).on_hover_text(t!("Abrir una carpeta de espacio (por ejemplo, un repositorio clonado)")).clicked() {
                self.pick_folder(Pick::OpenSpace, ui.ctx());
            }
            ui.separator();
            // Entorno del espacio de la petición activa.
            if let Some(si) = self.active.and_then(|a| self.space_of(&self.tabs[a].space)) {
                let s = &mut self.spaces[si];
                ui.label(t!("Entorno:"));
                let current = s.env.clone().unwrap_or_else(|| t!("(ninguno)").to_string());
                let mut changed = false;
                ComboBox::from_id_salt("env").selected_text(current).show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut s.env, None, t!("(ninguno)")).changed();
                    for e in &s.file.envs {
                        changed |= ui.selectable_value(&mut s.env, Some(e.name.clone()), &e.name).changed();
                    }
                });
                if changed {
                    self.session_dirty = true;
                }
                if ui.add(IconButton::new(Icon::Detail, Tone::Tool).label(t!("Variables"))).on_hover_text(t!("Variables, entornos y autenticación del espacio")).clicked() {
                    let s = &self.spaces[si];
                    self.dialog = Some(Dialog::SpaceSettings { dir: s.dir.clone(), file: s.file.clone(), secrets: s.secrets.clone(), env_sel: 0 });
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(IconButton::new(Icon::Gear, Tone::Neutral).selected(matches!(self.dialog, Some(Dialog::Settings)))).on_hover_text(t!("Ajustes")).clicked() {
                    self.dialog = if matches!(self.dialog, Some(Dialog::Settings)) { None } else { Some(Dialog::Settings) };
                }
                if crate::update::SUPPORTED {
                    let nueva = self.vigia.nueva(self.updater.as_ref());
                    if crate::estilo::boton_actualizar(ui, self.show_updater, nueva.as_deref()) {
                        let ctx = ui.ctx().clone();
                        if nueva.is_some() && self.updater.is_some() {
                            self.show_updater = true;
                        } else if nueva.is_some() {
                            self.updater = self.vigia.comprobacion.take();
                            self.show_updater = true;
                        } else {
                            self.open_updater(&ctx);
                        }
                    }
                }
            });
        });
    }

    // ------------------------------------------------------------ panel lateral

    fn sidebar(&mut self, ui: &mut Ui) {
        let active_path = self.active.and_then(|a| self.tabs.get(a)).map(|t| t.path.clone());
        let mut act: Option<TreeAct> = None;
        let mut export = None;
        let mut import_to = None;
        let mut close_space = None;
        let mut dialog = None;
        let mut reveal = None;
        let mut settings = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.spaces.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.weak(t!("Sin espacios.\nCrea uno o abre una carpeta."));
                });
            }
            for (si, s) in self.spaces.iter_mut().enumerate() {
                let head = ui.add(Button::new(RichText::new(format!("{} {}", if s.expanded { "⏷" } else { "⏵" }, s.file.name)).strong()).frame(false).wrap_mode(TextWrapMode::Truncate));
                if head.clicked() {
                    s.expanded = !s.expanded;
                }
                let dir = s.dir.clone();
                head.context_menu(|ui| {
                    if ui.button(t!("Nueva petición")).clicked() {
                        dialog = Some(Dialog::Name { title: t!("Nueva petición"), value: String::new(), action: NameAction::NewRequest { dir: dir.clone() } });
                        ui.close();
                    }
                    if ui.button(t!("Nueva carpeta")).clicked() {
                        dialog = Some(Dialog::Name { title: t!("Nueva carpeta"), value: String::new(), action: NameAction::NewFolder { dir: dir.clone() } });
                        ui.close();
                    }
                    if ui.button(t!("Variables y entornos…")).clicked() {
                        settings = Some(si);
                        ui.close();
                    }
                    if ui.button(t!("Importar aquí…")).clicked() {
                        import_to = Some(dir.clone());
                        ui.close();
                    }
                    if ui.button(t!("Exportar a Postman…")).clicked() {
                        export = Some(dir.clone());
                        ui.close();
                    }
                    if ui.button(t!("Mostrar la carpeta")).clicked() {
                        reveal = Some(dir.clone());
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(t!("Cerrar el espacio")).clicked() {
                        close_space = Some(si);
                        ui.close();
                    }
                });
                if let Some(e) = &s.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                if s.expanded {
                    ui.indent(("sp", si), |ui| {
                        tree_ui(ui, &s.tree, &dir, active_path.as_deref(), &mut act, &mut dialog);
                    });
                }
                ui.add_space(4.0);
            }
        });
        match act {
            Some(TreeAct::Open(p)) => self.open_request(&p),
            Some(TreeAct::Duplicate(p)) => self.duplicate(&p),
            Some(TreeAct::Curl(p)) => self.copy_curl(&p, ui.ctx()),
            None => {}
        }
        if let Some(d) = export {
            self.export_dir = Some(d);
            self.pick_folder(Pick::ExportPostman, ui.ctx());
        }
        if let Some(d) = import_to {
            self.dialog = Some(Dialog::Import(ImportDialog::new(Some(d))));
        }
        if let Some(d) = dialog {
            self.dialog = Some(d);
        }
        if let Some(si) = settings {
            let s = &self.spaces[si];
            self.dialog = Some(Dialog::SpaceSettings { dir: s.dir.clone(), file: s.file.clone(), secrets: s.secrets.clone(), env_sel: 0 });
        }
        if let Some(d) = reveal {
            let _ = crate::auth::open_browser(&d.display().to_string());
        }
        if let Some(si) = close_space {
            let dir = self.spaces[si].dir.clone();
            let mut i = 0;
            while i < self.tabs.len() {
                if self.tabs[i].space == dir {
                    self.close_tab(i);
                } else {
                    i += 1;
                }
            }
            self.spaces.remove(si);
            self.session_dirty = true;
        }
    }

    // ------------------------------------------------------------ editor

    fn tabs_strip(&mut self, ui: &mut Ui) {
        let mut close = None;
        ScrollArea::horizontal().id_salt("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, t) in self.tabs.iter().enumerate() {
                    let sel = self.active == Some(i);
                    let dark = ui.visuals().dark_mode;
                    let mut job = text::LayoutJob::default();
                    job.append(&t.req.method, 0.0, TextFormat { color: method_color(&t.req.method, dark), font_id: FontId::monospace(11.0), ..Default::default() });
                    job.append(&t.title(), 6.0, TextFormat { color: ui.visuals().text_color(), font_id: FontId::proportional(13.0), ..Default::default() });
                    let r = ui.add(Button::selectable(sel, job));
                    if r.clicked() {
                        self.active = Some(i);
                        self.session_dirty = true;
                    }
                    if r.middle_clicked() {
                        close = Some(i);
                    }
                    r.on_hover_text(t.path.display().to_string());
                    if ui.small_button("×").clicked() {
                        close = Some(i);
                    }
                    ui.add_space(4.0);
                }
            });
        });
        if let Some(i) = close {
            self.close_tab(i);
        }
    }

    fn editor(&mut self, ui: &mut Ui) {
        let Some(i) = self.active else {
            let logo = crate::estilo::logo(ui.ctx(), &mut self.logo);
            let acciones = [
                (Icon::Plus, t!("Nuevo espacio…"), t!("Crear un espacio nuevo")),
                (Icon::Download, t!("Importar…"), t!("curl, Postman, OpenAPI/Swagger o WSDL")),
                (Icon::Open, t!("Abrir carpeta…"), t!("Abrir una carpeta de espacio (por ejemplo, un repositorio clonado)")),
            ];
            let pie = if self.spaces.is_empty() { None } else { Some(t!("Abre una petición del panel o crea una nueva (clic derecho en el espacio).")) };
            match crate::estilo::bienvenida(ui, &logo, &acciones, pie) {
                Some(0) => self.new_space_dialog(),
                Some(1) => self.import_dialog(),
                Some(2) => self.pick_folder(Pick::OpenSpace, ui.ctx()),
                _ => {}
            }
            return;
        };
        let ctx = ui.ctx().clone();
        let mut do_send = false;
        let mut changed = false;
        let mut curl_note = None;
        {
            let t = &mut self.tabs[i];
            ui.horizontal(|ui| {
                ui.label(t!("Nombre"));
                changed |= ui.add(TextEdit::singleline(&mut t.req.name).desired_width(280.0)).changed();
                if let Some(e) = &t.save_error {
                    ui.colored_label(ui.visuals().error_fg_color, tf!("No se pudo guardar: {e}", e = e));
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let dark = ui.visuals().dark_mode;
                ComboBox::from_id_salt("method").width(90.0).selected_text(RichText::new(&t.req.method).color(method_color(&t.req.method, dark)).monospace().strong()).show_ui(ui, |ui| {
                    for m in METHODS {
                        if ui.selectable_label(t.req.method == m, RichText::new(m).color(method_color(m, dark)).monospace()).clicked() {
                            t.req.method = m.into();
                            changed = true;
                        }
                    }
                });
                let busy = t.pending.is_some();
                let send_w = 90.0;
                let url = ui.add(TextEdit::singleline(&mut t.req.url).hint_text("https://api.ejemplo.com/recurso  ·  {{base}}/ruta").font(TextStyle::Monospace).desired_width(ui.available_width() - send_w - 8.0));
                changed |= url.changed();
                // Pegar un comando curl en la URL lo convierte en la petición.
                if url.changed() && t.req.url.trim_start().starts_with("curl ") {
                    match crate::import::parse_curl(t.req.url.trim()) {
                        Ok(mut r) => {
                            r.name = if t.req.name.trim().is_empty() { r.name } else { t.req.name.clone() };
                            r.notes = std::mem::take(&mut t.req.notes);
                            t.req = r;
                            t.sub = if t.req.body.kind != BodyKind::None { ReqTab::Body } else { ReqTab::Headers };
                            curl_note = Some(t!("Comando curl convertido en petición"));
                        }
                        Err(e) => t.save_error = Some(e),
                    }
                }
                if url.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    do_send = true;
                }
                let _ = send_w;
                let b = ui.add_enabled(!busy, IconButton::new(Icon::Send, Tone::Live).label(if busy { t!("Enviando…") } else { t!("Enviar") }).framed());
                if b.on_hover_text("Ctrl+Enter").clicked() {
                    do_send = true;
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let n = |v: &Vec<Pair>| v.iter().filter(|p| p.enabled && !p.name.is_empty()).count();
                let tabs: [(ReqTab, String); 5] = [
                    (ReqTab::Params, with_count(t!("Parámetros"), n(&t.req.params))),
                    (ReqTab::Headers, with_count(t!("Cabeceras"), n(&t.req.headers))),
                    (ReqTab::Body, if t.req.body.kind == BodyKind::None { t!("Cuerpo").to_string() } else { format!("{} · {}", t!("Cuerpo"), t.req.body.kind.label()) }),
                    (ReqTab::Auth, format!("{} · {}", t!("Autenticación"), t.req.auth.label())),
                    (ReqTab::Notes, t!("Notas").to_string()),
                ];
                for (k, label) in tabs {
                    ui.selectable_value(&mut t.sub, k, label);
                }
            });
            ui.separator();
            let max_h = (ui.available_height() * 0.45).max(120.0);
            ScrollArea::vertical().id_salt("reqbody").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| match t.sub {
                ReqTab::Params => {
                    changed |= pairs_editor(ui, "params", &mut t.req.params, t!("parámetro"), t!("valor"));
                }
                ReqTab::Headers => {
                    changed |= pairs_editor(ui, "headers", &mut t.req.headers, t!("cabecera"), t!("valor"));
                }
                ReqTab::Body => {
                    ui.horizontal_wrapped(|ui| {
                        for k in BodyKind::ALL {
                            changed |= ui.radio_value(&mut t.req.body.kind, k, k.label()).changed();
                        }
                    });
                    if t.req.body.kind.is_soap() {
                        ui.horizontal(|ui| {
                            ui.label("SOAPAction");
                            changed |= ui.add(TextEdit::singleline(&mut t.req.body.soap_action).desired_width(f32::INFINITY).font(TextStyle::Monospace)).changed();
                        });
                        if t.req.body.text.trim().is_empty() && ui.button(t!("Insertar sobre de ejemplo")).clicked() {
                            t.req.body.text = soap_template(t.req.body.kind == BodyKind::Soap12);
                            changed = true;
                        }
                    }
                    match t.req.body.kind {
                        BodyKind::None => {
                            ui.weak(t!("Esta petición no envía cuerpo."));
                        }
                        BodyKind::Form => {
                            changed |= pairs_editor(ui, "form", &mut t.req.body.fields, t!("campo"), t!("valor"));
                        }
                        k => {
                            if matches!(k, BodyKind::Json | BodyKind::Xml | BodyKind::Soap11 | BodyKind::Soap12) && ui.add(IconButton::new(Icon::Sparkle, Tone::Tool).label(t!("Formatear")).small()).clicked() {
                                let r = if k == BodyKind::Json { beautify::format_json(&t.req.body.text, "  ") } else { beautify::format_xml(&t.req.body.text, "  ", false) };
                                match r {
                                    Ok(s) => {
                                        t.req.body.text = s;
                                        changed = true;
                                    }
                                    Err(e) => {
                                        let (l, c) = beautify::line_col(&t.req.body.text, e.pos);
                                        t.save_error = Some(tf!("línea {l}, columna {c}: {m}", l = l, c = c, m = e.msg));
                                    }
                                }
                            }
                            changed |= ui.add(TextEdit::multiline(&mut t.req.body.text).code_editor().desired_rows(10).desired_width(f32::INFINITY)).changed();
                        }
                    }
                }
                ReqTab::Auth => {
                    changed |= auth_editor(ui, &mut t.req.auth, false);
                }
                ReqTab::Notes => {
                    changed |= ui.add(TextEdit::multiline(&mut t.req.notes).desired_rows(6).desired_width(f32::INFINITY).hint_text(t!("Descripción, ejemplos, enlaces…"))).changed();
                }
            });
            if changed {
                t.changed_at = Some(Instant::now());
                ctx.request_repaint_after(AUTOSAVE_AFTER);
            }
        }
        if let Some(n) = curl_note {
            self.msg(n);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            do_send = true;
        }
        if do_send {
            self.send(i, &ctx);
        }
        ui.add_space(6.0);
        ui.separator();
        self.response_ui(ui, i);
    }

    fn response_ui(&mut self, ui: &mut Ui, i: usize) {
        let dark = ui.visuals().dark_mode;
        let mut save_body = false;
        let t = &mut self.tabs[i];
        if let Some((_, started)) = &t.pending {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tf!("Esperando respuesta… {t}", t = fmt_ms(started.elapsed())));
            });
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            return;
        }
        let Some(out) = &t.outcome else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.weak(t!("Pulsa «Enviar» (Ctrl+Enter) para ver la respuesta.")));
            return;
        };
        ui.horizontal(|ui| {
            if let Some(r) = &out.response {
                ui.label(RichText::new(format!("{} {}", r.status, r.reason)).strong().color(status_color(r.status, dark)));
                ui.separator();
                ui.label(fmt_ms(r.elapsed));
                ui.separator();
                ui.label(fmt_size(r.body.len()));
                ui.separator();
                ui.weak(&r.http_version);
                if r.url != out.sent.url && !r.url.is_empty() {
                    ui.separator();
                    ui.label(RichText::new(format!("→ {}", r.url)).weak()).on_hover_text(t!("URL final tras redirecciones"));
                }
            } else if let Some(e) = &out.error {
                ui.colored_label(ui.visuals().error_fg_color, format!("✖ {e}"));
            }
            for n in &out.notes {
                ui.separator();
                ui.weak(n);
            }
            if !out.sent.missing.is_empty() {
                ui.separator();
                ui.colored_label(Color32::from_rgb(0xd0, 0x8a, 0x10), tf!("Variables sin valor: {v}", v = out.sent.missing.join(", ")));
            }
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut t.resp_tab, RespTab::Body, t!("Cuerpo"));
            let nh = out.response.as_ref().map_or(0, |r| r.headers.len());
            ui.selectable_value(&mut t.resp_tab, RespTab::Headers, with_count(t!("Cabeceras"), nh));
            ui.selectable_value(&mut t.resp_tab, RespTab::Sent, t!("Enviado"));
            ui.separator();
            if t.resp_tab == RespTab::Body && out.response.is_some() {
                if ui.checkbox(&mut t.pretty, t!("Formatear")).changed() {
                    t.view = None;
                }
                if ui.small_button(t!("Copiar")).clicked() {
                    if let Some(r) = &out.response {
                        ui.ctx().copy_text(String::from_utf8_lossy(&r.body).into_owned());
                    }
                }
                if ui.small_button(t!("Guardar…")).clicked() {
                    save_body = true;
                }
            }
            if t.resp_tab == RespTab::Sent && ui.small_button(t!("Copiar como curl")).clicked() {
                ui.ctx().copy_text(http::to_curl(&out.sent));
            }
        });
        ui.separator();
        match t.resp_tab {
            RespTab::Body => {
                let Some(r) = &out.response else { return };
                if t.view.as_ref().is_none_or(|v| v.0 != t.pretty) {
                    let s = body_text(r, t.pretty);
                    let spans = line_spans(&s);
                    t.view = Some((t.pretty, s, spans));
                }
                let (_, text, spans) = t.view.as_ref().unwrap();
                if text.len() <= SMALL_BODY {
                    ScrollArea::both().id_salt("respbody").auto_shrink(false).show(ui, |ui| {
                        let mut s: &str = text;
                        ui.add(TextEdit::multiline(&mut s).code_editor().desired_width(f32::INFINITY));
                    });
                } else {
                    let row_h = ui.text_style_height(&TextStyle::Monospace);
                    ScrollArea::both().id_salt("respbody").auto_shrink(false).show_rows(ui, row_h, spans.len(), |ui, rows| {
                        for &(a, b) in &spans[rows] {
                            ui.add(Label::new(RichText::new(&text[a..b]).monospace()).extend());
                        }
                    });
                }
            }
            RespTab::Headers => {
                let Some(r) = &out.response else { return };
                ScrollArea::vertical().id_salt("resphead").auto_shrink(false).show(ui, |ui| {
                    Grid::new("rh").num_columns(2).striped(true).show(ui, |ui| {
                        for (k, v) in &r.headers {
                            ui.label(RichText::new(k).monospace().strong());
                            ui.add(Label::new(RichText::new(v).monospace()).wrap());
                            ui.end_row();
                        }
                    });
                });
            }
            RespTab::Sent => {
                let s = &out.sent;
                let mut text = format!("{} {}\n", s.method, s.url);
                for (k, v) in &s.headers {
                    text.push_str(&format!("{k}: {v}\n"));
                }
                if !s.body.is_empty() {
                    text.push('\n');
                    text.push_str(&String::from_utf8_lossy(&s.body[..s.body.len().min(SMALL_BODY)]));
                }
                ScrollArea::both().id_salt("sent").auto_shrink(false).show(ui, |ui| {
                    let mut t: &str = &text;
                    ui.add(TextEdit::multiline(&mut t).code_editor().desired_width(f32::INFINITY));
                });
            }
        }
        if save_body {
            self.pick_folder(Pick::SaveBody, ui.ctx());
        }
    }

    // ------------------------------------------------------------ diálogos

    fn dialogs(&mut self, ctx: &Context) {
        let Some(mut d) = self.dialog.take() else { return };
        let mut keep = true;
        match &mut d {
            Dialog::NewSpace { name, parent } => {
                let mut create = false;
                let mut browse = false;
                Modal::new(Id::NULL.with("newspace")).show(ctx, |ui| {
                    ui.heading(t!("Nuevo espacio"));
                    Grid::new("ns").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label(t!("Nombre"));
                        let r = ui.add(TextEdit::singleline(name).desired_width(320.0));
                        r.request_focus();
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            create = true;
                        }
                        ui.end_row();
                        ui.label(t!("Dentro de"));
                        ui.horizontal(|ui| {
                            ui.add(TextEdit::singleline(parent).desired_width(260.0));
                            browse = ui.button(t!("Elegir…")).clicked();
                        });
                        ui.end_row();
                    });
                    if !name.trim().is_empty() {
                        ui.weak(tf!("Se creará {p}", p = Path::new(parent.trim()).join(store::file_name_for(name)).display()));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        create |= ui.add_enabled(!name.trim().is_empty(), Button::new(t!("Crear"))).clicked();
                        if ui.button(t!("Cancelar")).clicked() {
                            keep = false;
                        }
                    });
                });
                if browse {
                    self.dialog = Some(d);
                    self.pick_folder(Pick::NewSpaceParent, ctx);
                    return;
                }
                if create {
                    let dir = expand_home(parent.trim()).join(store::file_name_for(name));
                    match store::create_space(&dir, name.trim()) {
                        Ok(_) => {
                            self.add_space(dir.clone());
                            self.open_request(&dir.join("Ejemplo.toml"));
                            keep = false;
                        }
                        Err(e) => self.msg(tf!("No se pudo crear el espacio: {e}", e = e)),
                    }
                }
            }
            Dialog::Name { title, value, action } => {
                let mut ok = false;
                Modal::new(Id::NULL.with("name")).show(ctx, |ui| {
                    ui.heading(*title);
                    let r = ui.add(TextEdit::singleline(value).desired_width(320.0));
                    r.request_focus();
                    ok = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ok |= ui.add_enabled(!value.trim().is_empty(), Button::new(t!("Aceptar"))).clicked();
                        if ui.button(t!("Cancelar")).clicked() {
                            keep = false;
                        }
                    });
                });
                if ok && !value.trim().is_empty() {
                    keep = false;
                    let name = value.trim().to_string();
                    match action.clone() {
                        NameAction::NewRequest { dir } => {
                            let p = store::free_path(&dir, &name);
                            let r = Request { name, ..Default::default() };
                            match store::save_request(&p, &r) {
                                Ok(()) => {
                                    self.refresh_tree(&dir);
                                    self.open_request(&p);
                                }
                                Err(e) => self.msg(e.to_string()),
                            }
                        }
                        NameAction::NewFolder { dir } => match std::fs::create_dir_all(dir.join(store::file_name_for(&name))) {
                            Ok(()) => self.refresh_tree(&dir),
                            Err(e) => self.msg(e.to_string()),
                        },
                        NameAction::Rename { path } => self.rename(&path, &name),
                    }
                }
            }
            Dialog::Delete { path } => {
                let mut yes = false;
                Modal::new(Id::NULL.with("delete")).show(ctx, |ui| {
                    ui.heading(t!("¿Borrar?"));
                    ui.label(path.display().to_string());
                    if path.is_dir() {
                        ui.colored_label(ui.visuals().warn_fg_color, t!("Se borrará la carpeta con todas sus peticiones."));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        yes = ui.button(t!("Borrar")).clicked();
                        if ui.button(t!("Cancelar")).clicked() {
                            keep = false;
                        }
                    });
                });
                if yes {
                    keep = false;
                    let p = path.clone();
                    self.tabs.retain(|t| !t.path.starts_with(&p));
                    self.active = (!self.tabs.is_empty()).then(|| self.active.unwrap_or(0).min(self.tabs.len() - 1));
                    let r = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                    if let Err(e) = r {
                        self.msg(e.to_string());
                    }
                    self.refresh_tree(&p);
                    self.session_dirty = true;
                }
            }
            Dialog::SpaceSettings { dir, file, secrets, env_sel } => {
                let mut save = false;
                let mut open = true;
                Window::new(tf!("Espacio «{n}»", n = file.name)).id(Id::NULL.with("spacesettings")).open(&mut open).default_size([720.0, 520.0]).collapsible(false).show(ctx, |ui| {
                    space_settings_ui(ui, file, secrets, env_sel);
                    ui.separator();
                    ui.horizontal(|ui| {
                        save = ui.button(t!("Guardar")).clicked();
                        if ui.button(t!("Cancelar")).clicked() {
                            keep = false;
                        }
                        ui.weak(t!("Los valores secretos se guardan solo en este equipo, fuera de la carpeta del espacio."));
                    });
                });
                if !open {
                    keep = false;
                }
                if save {
                    keep = false;
                    if let Some(si) = self.space_of(dir) {
                        let s = &mut self.spaces[si];
                        let mut f = file.clone();
                        trim_pairs(&mut f.headers);
                        f.vars.retain(|v| !v.name.trim().is_empty());
                        for e in &mut f.envs {
                            e.vars.retain(|v| !v.name.trim().is_empty());
                        }
                        // Los secretos salen del archivo compartido.
                        let mut sec = secrets.clone();
                        sec.variables.clear();
                        sec.entornos.clear();
                        for v in &mut f.vars {
                            if v.secret {
                                sec.variables.insert(v.name.clone(), std::mem::take(&mut v.value));
                            }
                        }
                        for e in &mut f.envs {
                            for v in &mut e.vars {
                                if v.secret {
                                    sec.entornos.entry(e.name.clone()).or_default().insert(v.name.clone(), std::mem::take(&mut v.value));
                                }
                            }
                        }
                        s.file = f;
                        s.secrets = sec;
                        if s.env.as_ref().is_some_and(|e| !s.file.envs.iter().any(|x| &x.name == e)) {
                            s.env = None;
                        }
                        if let Err(e) = s.save() {
                            self.msg(tf!("No se pudo guardar: {e}", e = e));
                        }
                    }
                }
            }
            Dialog::Import(imp) => {
                let mut open = true;
                let mut do_import = false;
                let mut pick = None;
                let mut fetch = None;
                let space_names: Vec<(PathBuf, String)> = self.spaces.iter().map(|s| (s.dir.clone(), s.file.name.clone())).collect();
                let fetching = self.fetch.is_some();
                Window::new(t!("Importar")).id(Id::NULL.with("import")).open(&mut open).default_size([680.0, 520.0]).collapsible(false).show(ctx, |ui| {
                    ui.weak(t!("Pega un comando curl, una colección o entorno de Postman, una especificación OpenAPI/Swagger (JSON o YAML) o un WSDL; o elige un archivo o una URL."));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.add(IconButton::new(Icon::Open, Tone::File).label(t!("Archivo…"))).clicked() {
                            pick = Some(Pick::ImportFile);
                        }
                        ui.label(t!("o URL:"));
                        let r = ui.add(TextEdit::singleline(&mut imp.url).hint_text("https://servicio/ws?wsdl · https://api/openapi.json").desired_width(ui.available_width() - 110.0));
                        let go = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if (ui.add_enabled(!fetching && !imp.url.trim().is_empty(), Button::new(if fetching { t!("Descargando…") } else { t!("Descargar") })).clicked() || go) && !fetching {
                            fetch = Some(imp.url.trim().to_string());
                        }
                    });
                    ui.add_space(4.0);
                    if let Some(f) = imp.file.clone() {
                        ui.horizontal(|ui| {
                            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            ui.label(tf!("Archivo: {f} ({n})", f = name, n = fmt_size(imp.text.len()))).on_hover_text(f.display().to_string());
                            if ui.small_button(t!("Quitar")).clicked() {
                                imp.file = None;
                                imp.text.clear();
                                imp.parsed = None;
                            }
                        });
                    } else if imp.text.len() > SMALL_BODY {
                        ui.horizontal(|ui| {
                            ui.label(tf!("Texto descargado ({n})", n = fmt_size(imp.text.len())));
                            if ui.small_button(t!("Quitar")).clicked() {
                                imp.text.clear();
                                imp.parsed = None;
                            }
                        });
                    } else {
                        let r = ScrollArea::vertical().id_salt("imptext").max_height(180.0).show(ui, |ui| ui.add(TextEdit::multiline(&mut imp.text).code_editor().desired_rows(8).desired_width(f32::INFINITY).hint_text("curl https://… / { \"info\": … } / openapi: 3.0 / <definitions …>"))).inner;
                        if r.changed() {
                            imp.reparse();
                        }
                    }
                    ui.separator();
                    match &imp.parsed {
                        None => {
                            ui.weak(t!("Nada que importar todavía."));
                        }
                        Some(Err(e)) => {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                        Some(Ok(p)) => {
                            ui.label(RichText::new(tf!("{f}: {n} peticiones", f = p.format, n = p.count())).strong());
                            if !p.vars.is_empty() || !p.envs.is_empty() {
                                let vars: Vec<&str> = p.vars.iter().map(|v| v.name.as_str()).collect();
                                let envs: Vec<&str> = p.envs.iter().map(|e| e.name.as_str()).collect();
                                ui.weak(tf!("Variables: {v} · Entornos: {e}", v = if vars.is_empty() { "—".into() } else { vars.join(", ") }, e = if envs.is_empty() { "—".into() } else { envs.join(", ") }));
                            }
                            if let Some(a) = &p.auth {
                                ui.weak(tf!("Autenticación: {a}", a = a.label()));
                            }
                            for n in &p.notes {
                                ui.colored_label(ui.visuals().warn_fg_color, format!("⚠ {n}"));
                            }
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label(t!("Destino"));
                                let label = match &imp.target {
                                    None => t!("Espacio nuevo").to_string(),
                                    Some(d) => space_names.iter().find(|(x, _)| x == d).map(|(_, n)| n.clone()).unwrap_or_default(),
                                };
                                ComboBox::from_id_salt("imptarget").selected_text(label).show_ui(ui, |ui| {
                                    ui.selectable_value(&mut imp.target, None, t!("Espacio nuevo"));
                                    for (d, n) in &space_names {
                                        ui.selectable_value(&mut imp.target, Some(d.clone()), n);
                                    }
                                });
                            });
                            Grid::new("impdest").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                                if imp.target.is_none() {
                                    ui.label(t!("Nombre"));
                                    ui.add(TextEdit::singleline(&mut imp.name).desired_width(320.0));
                                    ui.end_row();
                                    ui.label(t!("Dentro de"));
                                    ui.horizontal(|ui| {
                                        ui.add(TextEdit::singleline(&mut imp.parent).desired_width(260.0));
                                        if ui.button(t!("Elegir…")).clicked() {
                                            pick = Some(Pick::ImportParent);
                                        }
                                    });
                                    ui.end_row();
                                } else {
                                    ui.label(t!("Carpeta"));
                                    ui.add(TextEdit::singleline(&mut imp.folder).hint_text(t!("(raíz del espacio)")).desired_width(320.0));
                                    ui.end_row();
                                }
                            });
                            ui.add_space(6.0);
                            let ok = imp.target.is_some() || !imp.name.trim().is_empty();
                            do_import = ui.add_enabled(ok, Button::new(RichText::new(t!("Importar")).strong())).clicked();
                        }
                    }
                });
                if !open {
                    keep = false;
                }
                if let Some(u) = fetch {
                    let (tx, rx) = channel();
                    let ctx2 = ctx.clone();
                    let opts = self.opts.clone();
                    std::thread::spawn(move || {
                        let p = http::Prepared { method: "GET".into(), url: http::fix_url(&u), ..Default::default() };
                        let r = http::send_raw(&p, &opts).and_then(|r| if r.status < 400 { Ok(String::from_utf8_lossy(&r.body).into_owned()) } else { Err(format!("HTTP {} {}", r.status, r.reason)) });
                        let _ = tx.send(r);
                        ctx2.request_repaint();
                    });
                    self.fetch = Some(rx);
                }
                if do_import {
                    keep = false;
                    self.do_import(imp);
                }
                if let Some(p) = pick {
                    if keep && self.dialog.is_none() {
                        self.dialog = Some(d);
                    }
                    self.pick_folder(p, ctx);
                    return;
                }
            }
            Dialog::Settings => {
                let mut open = true;
                let label = crate::update::version_label();
                let version = crate::estilo::Version { etiqueta: &label, actualizable: crate::update::SUPPORTED };
                let mut theme = match self.session.theme.as_deref() {
                    Some("claro") => ThemePreference::Light,
                    Some("oscuro") => ThemePreference::Dark,
                    _ => ThemePreference::System,
                };
                let accion = crate::estilo::ventana_ajustes(ctx, &mut open, &version, |ui| {
                    use crate::estilo::{fila, rejilla, seccion};
                    seccion(ui, t!("General"));
                    rejilla(ui, "ajustes_general", |ui| {
                        if let Some(l) = crate::estilo::fila_idioma(ui) {
                            crate::i18n::set(l);
                            self.session.lang = Some(l.code().into());
                            self.session_dirty = true;
                        }
                        if crate::estilo::fila_tema(ui, &mut theme) {
                            self.session.theme = match theme {
                                ThemePreference::Light => Some("claro".into()),
                                ThemePreference::Dark => Some("oscuro".into()),
                                ThemePreference::System => None,
                            };
                            self.session_dirty = true;
                        }
                        if crate::update::SUPPORTED {
                            fila(ui, t!("Actualizaciones"), None, |ui| {
                                let mut on = self.session.check_updates.unwrap_or(true);
                                if ui.checkbox(&mut on, t!("Avisar de versiones nuevas (punto verde en ⬇; mira GitHub al abrir y cada 6 horas)")).changed() {
                                    self.session.check_updates = (!on).then_some(false);
                                    self.session_dirty = true;
                                }
                            });
                        }
                    });
                    seccion(ui, t!("Envío"));
                    rejilla(ui, "ajustes_envio", |ui| {
                        fila(ui, t!("Tiempo de espera"), None, |ui| {
                            let mut secs = self.opts.timeout.as_secs();
                            if ui.add(DragValue::new(&mut secs).range(1..=3600).suffix(" s")).changed() {
                                self.opts.timeout = Duration::from_secs(secs);
                                self.session.timeout_secs = Some(secs);
                                self.session_dirty = true;
                            }
                        });
                        fila(ui, t!("Redirecciones"), None, |ui| {
                            if ui.checkbox(&mut self.opts.follow_redirects, t!("Seguirlas")).changed() {
                                self.session.follow_redirects = Some(self.opts.follow_redirects);
                                self.session_dirty = true;
                            }
                        });
                        fila(ui, "TLS", None, |ui| {
                            if ui.checkbox(&mut self.opts.verify_tls, t!("Verificar certificados")).changed() {
                                self.session.verify_tls = Some(self.opts.verify_tls);
                                self.session_dirty = true;
                            }
                        });
                    });
                });
                if accion == crate::estilo::Accion::BuscarActualizaciones {
                    self.open_updater(ctx);
                }
                if accion == crate::estilo::Accion::Restablecer {
                    let d = Options::default();
                    self.opts.timeout = d.timeout;
                    self.opts.follow_redirects = d.follow_redirects;
                    self.opts.verify_tls = d.verify_tls;
                    self.session.theme = None;
                    self.session.lang = None;
                    self.session.timeout_secs = None;
                    self.session.follow_redirects = None;
                    self.session.verify_tls = None;
                    self.session.check_updates = None;
                    ctx.set_theme(ThemePreference::System);
                    crate::i18n::set(crate::i18n::system().unwrap_or(crate::i18n::Lang::Es));
                    self.session_dirty = true;
                }
                if !open {
                    keep = false;
                }
            }
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(d);
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            // Escape cierra lo de delante: diálogo, atajos o actualizar.
            if self.dialog.is_some() {
                self.dialog = None;
            } else if self.show_keys {
                self.show_keys = false;
            } else if self.show_updater && !self.updater.as_ref().is_some_and(|u| u.stage() == crate::update::Stage::Running) {
                self.show_updater = false;
            }
        }
    }

    /// Escribe lo importado en el destino elegido y abre lo creado.
    fn do_import(&mut self, imp: &ImportDialog) {
        let Some(Ok(parsed)) = &imp.parsed else { return };
        let (space_dir, dir) = match &imp.target {
            None => {
                let dir = expand_home(imp.parent.trim()).join(store::file_name_for(&imp.name));
                if dir.join(store::SPACE_FILE).exists() {
                    return self.msg(tf!("Ya existe un espacio en {d}", d = dir.display()));
                }
                let f = SpaceFile { id: store::new_id(), name: imp.name.trim().to_string(), auth: Auth::None, ..Default::default() };
                if let Err(e) = store::save_space_file(&dir, &f) {
                    return self.msg(tf!("No se pudo crear el espacio: {e}", e = e));
                }
                (dir.clone(), dir)
            }
            Some(d) => {
                let sub = imp.folder.trim();
                let dir = if sub.is_empty() { d.clone() } else { d.join(store::file_name_for(sub)) };
                (d.clone(), dir)
            }
        };
        match crate::import::write_into(&space_dir, &dir, parsed) {
            Ok(n) => {
                match self.space_of(&space_dir) {
                    // Se recarga el espacio (variables y auth nuevas) conservando el entorno.
                    Some(i) => {
                        let env = self.spaces[i].env.clone();
                        let expanded = self.spaces[i].expanded;
                        self.spaces[i] = Space::load(space_dir.clone(), env);
                        self.spaces[i].expanded = expanded;
                    }
                    None => self.add_space(space_dir.clone()),
                }
                self.msg(tf!("Importadas {n} peticiones", n = n));
                // Una sola petición: se abre.
                if n == 1 {
                    if let Some(p) = first_request(&store::scan_tree(&dir), &dir) {
                        self.open_request(&p);
                    }
                }
            }
            Err(e) => self.msg(tf!("No se pudo importar: {e}", e = e)),
        }
    }

    fn poll_fetch(&mut self) {
        let Some(rx) = &self.fetch else { return };
        let Ok(r) = rx.try_recv() else { return };
        self.fetch = None;
        match r {
            Ok(text) => {
                if let Some(Dialog::Import(d)) = &mut self.dialog {
                    d.set_text(text, None);
                }
            }
            Err(e) => self.msg(tf!("No se pudo descargar: {e}", e = e)),
        }
    }

    fn rename(&mut self, path: &Path, name: &str) {
        if path.is_dir() {
            let to = path.with_file_name(store::file_name_for(name));
            if let Err(e) = std::fs::rename(path, &to) {
                self.msg(e.to_string());
                return;
            }
            for t in &mut self.tabs {
                if let Ok(rest) = t.path.strip_prefix(path) {
                    t.path = to.join(rest);
                }
            }
        } else {
            // Petición: cambia el nombre interno y el del archivo.
            let to = store::free_path(path.parent().unwrap_or(Path::new(".")), name);
            let mut r = match self.tabs.iter().find(|t| t.path == path) {
                Some(t) => clean(&t.req),
                None => match store::load_request(path) {
                    Ok(r) => r,
                    Err(e) => return self.msg(e.to_string()),
                },
            };
            r.name = name.into();
            if let Err(e) = store::save_request(&to, &r).and_then(|_| std::fs::remove_file(path)) {
                self.msg(e.to_string());
                return;
            }
            for t in &mut self.tabs {
                if t.path == path {
                    t.path = to.clone();
                    t.req.name = name.into();
                    t.saved = t.req.clone();
                }
            }
        }
        self.refresh_tree(path);
        self.session_dirty = true;
    }

    /// Copia una petición del árbol como comando curl (con variables resueltas).
    fn copy_curl(&mut self, path: &Path, ctx: &Context) {
        let req = match self.tabs.iter().find(|t| t.path == path) {
            Some(t) => clean(&t.req),
            None => match store::load_request(path) {
                Ok(r) => r,
                Err(e) => return self.msg(e.to_string()),
            },
        };
        let Some(s) = self.spaces.iter().filter(|s| path.starts_with(&s.dir)).max_by_key(|s| s.dir.as_os_str().len()) else { return };
        let p = http::prepare(&req, &s.file, &s.scope());
        ctx.copy_text(http::to_curl(&p));
        self.msg(t!("Copiado como curl"));
    }

    fn duplicate(&mut self, path: &Path) {
        let Ok(mut r) = store::load_request(path) else { return };
        r.name = tf!("{n} (copia)", n = r.name);
        let p = store::free_path(path.parent().unwrap_or(Path::new(".")), &r.name);
        if store::save_request(&p, &r).is_ok() {
            self.refresh_tree(path);
            self.open_request(&p);
        }
    }

    /// Modo captura: tras unos fotogramas guarda la ventana (ancho, alto u32 LE
    /// + RGBA) y sale. PETICIONES_ENVIAR=1 envía la petición activa antes.
    /// «Acerca de» (ventana común a las tres aplicaciones: `estilo::acerca`).
    fn about_window(&mut self, ctx: &Context) {
        if !self.show_about {
            return;
        }
        let logo = crate::estilo::logo(ctx, &mut self.logo);
        let config = store::config_dir().map(|d| d.display().to_string()).unwrap_or_default();
        let label = crate::update::version_label();
        let version = crate::estilo::Version { etiqueta: &label, actualizable: crate::update::SUPPORTED };
        if crate::estilo::acerca(ctx, &mut self.show_about, &logo, &version, |ui| {
            crate::estilo::fila_carpeta(ui, t!("Configuración"), &config);
        }) == crate::estilo::Accion::BuscarActualizaciones
        {
            self.open_updater(ctx);
        }
    }

    fn screenshot_step(&mut self, ctx: &Context) {
        let Some(path) = self.screenshot.clone() else { return };
        self.frames += 1;
        // PETICIONES_ACERCA=1 / PETICIONES_AJUSTES=1: abre «Acerca de» / «Ajustes».
        if self.frames == 3 {
            self.show_about = std::env::var_os("PETICIONES_ACERCA").is_some();
            self.show_keys = std::env::var_os("PETICIONES_ATAJOS").is_some();
            if let Ok(m) = std::env::var("PETICIONES_AVISO") {
                self.msg(m);
            }
            if std::env::var_os("PETICIONES_AJUSTES").is_some() {
                self.dialog = Some(Dialog::Settings);
            }
        }
        if self.frames == 5 && std::env::var_os("PETICIONES_ENVIAR").is_some() {
            if let Some(a) = self.active {
                self.send(a, ctx);
            }
        }
        if let Ok(v) = std::env::var("PETICIONES_PESTANA") {
            if let Some(a) = self.active {
                let t = &mut self.tabs[a];
                t.sub = match v.as_str() {
                    "cabeceras" => ReqTab::Headers,
                    "cuerpo" => ReqTab::Body,
                    "auth" => ReqTab::Auth,
                    "notas" => ReqTab::Notes,
                    _ => ReqTab::Params,
                };
            }
        }
        if self.frames == 3 && std::env::var_os("PETICIONES_AJUSTES_ESPACIO").is_some() {
            if let Some(s) = self.spaces.first() {
                self.dialog = Some(Dialog::SpaceSettings { dir: s.dir.clone(), file: s.file.clone(), secrets: s.secrets.clone(), env_sel: 0 });
            }
        }
        // PETICIONES_IMPORTAR=archivo: abre «Importar» con su contenido.
        if self.frames == 3 {
            if let Some(f) = std::env::var_os("PETICIONES_IMPORTAR") {
                let mut d = ImportDialog::new(self.spaces.first().map(|s| s.dir.clone()));
                d.set_text(std::fs::read_to_string(&f).unwrap_or_default(), Some(PathBuf::from(f)));
                self.dialog = Some(Dialog::Import(d));
            }
        }
        let busy = self.tabs.iter().any(|t| t.pending.is_some()) || self.fetch.is_some();
        let scripted = std::env::var("PETICIONES_RATON").is_ok_and(|s| self.test_step < s.split(';').count());
        if self.frames < 40 || busy || scripted {
            ctx.request_repaint();
            return;
        }
        if self.frames < 1000 {
            self.frames = 1000;
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(ScreenshotCallback::new(move |img| {
                let mut out = Vec::with_capacity(8 + img.pixels.len() * 4);
                out.extend_from_slice(&(img.size[0] as u32).to_le_bytes());
                out.extend_from_slice(&(img.size[1] as u32).to_le_bytes());
                out.extend_from_slice(img.as_raw());
                std::fs::write(&path, out).unwrap();
                std::process::exit(0);
            })));
        }
    }
}

/// La petición creada más recientemente bajo `dir` (para abrir lo importado).
fn first_request(nodes: &[Node], _dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    fn walk(nodes: &[Node], best: &mut Option<(std::time::SystemTime, PathBuf)>) {
        for n in nodes {
            match n {
                Node::Folder { children, .. } => walk(children, best),
                Node::Req { path, .. } => {
                    let t = std::fs::metadata(path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                    if best.as_ref().is_none_or(|(b, _)| t > *b) {
                        *best = Some((t, path.clone()));
                    }
                }
            }
        }
    }
    walk(nodes, &mut best);
    best.map(|(_, p)| p)
}

fn with_count(label: &str, n: usize) -> String {
    if n == 0 { label.to_string() } else { format!("{label} ({n})") }
}

fn expand_home(s: &str) -> PathBuf {
    match s.strip_prefix("~") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest.trim_start_matches('/')),
        None => PathBuf::from(s),
    }
}

fn soap_template(v12: bool) -> String {
    let ns = if v12 { "http://www.w3.org/2003/05/soap-envelope" } else { "http://schemas.xmlsoap.org/soap/envelope/" };
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<soap:Envelope xmlns:soap=\"{ns}\">\n  <soap:Header/>\n  <soap:Body>\n    <!-- operación -->\n  </soap:Body>\n</soap:Envelope>\n")
}

/// Árbol de un espacio. Clic abre; menú contextual crea, renombra, duplica o borra.
/// Acción pedida desde el árbol (se aplica al terminar de pintarlo).
enum TreeAct {
    Open(PathBuf),
    Duplicate(PathBuf),
    Curl(PathBuf),
}

fn tree_ui(ui: &mut Ui, nodes: &[Node], parent: &Path, active: Option<&Path>, act: &mut Option<TreeAct>, dialog: &mut Option<Dialog>) {
    let space = parent.to_path_buf();
    for n in nodes {
        match n {
            Node::Folder { name, path, children } => {
                let id = ui.make_persistent_id(path);
                let r = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                    .show_header(ui, |ui| {
                        ui.label(RichText::new(name).strong());
                    })
                    .body(|ui| tree_ui(ui, children, &space, active, act, dialog));
                r.1.response.context_menu(|ui| {
                    if ui.button(t!("Nueva petición")).clicked() {
                        *dialog = Some(Dialog::Name { title: t!("Nueva petición"), value: String::new(), action: NameAction::NewRequest { dir: path.clone() } });
                        ui.close();
                    }
                    if ui.button(t!("Nueva carpeta")).clicked() {
                        *dialog = Some(Dialog::Name { title: t!("Nueva carpeta"), value: String::new(), action: NameAction::NewFolder { dir: path.clone() } });
                        ui.close();
                    }
                    if ui.button(t!("Renombrar…")).clicked() {
                        *dialog = Some(Dialog::Name { title: t!("Renombrar"), value: name.clone(), action: NameAction::Rename { path: path.clone() } });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(t!("Borrar…")).clicked() {
                        *dialog = Some(Dialog::Delete { path: path.clone() });
                        ui.close();
                    }
                });
            }
            Node::Req { name, method, path } => {
                let dark = ui.visuals().dark_mode;
                let mut job = text::LayoutJob::default();
                let m = if method.len() > 6 { &method[..6] } else { method.as_str() };
                job.append(&format!("{m:<6}"), 0.0, TextFormat { color: method_color(method, dark), font_id: FontId::monospace(11.0), ..Default::default() });
                job.append(name, 6.0, TextFormat { color: ui.visuals().text_color(), font_id: FontId::proportional(13.0), ..Default::default() });
                let sel = active == Some(path.as_path());
                let r = ui.add(Button::selectable(sel, job).wrap_mode(TextWrapMode::Truncate));
                if r.clicked() {
                    *act = Some(TreeAct::Open(path.clone()));
                }
                r.context_menu(|ui| {
                    if ui.button(t!("Renombrar…")).clicked() {
                        *dialog = Some(Dialog::Name { title: t!("Renombrar"), value: name.clone(), action: NameAction::Rename { path: path.clone() } });
                        ui.close();
                    }
                    if ui.button(t!("Duplicar")).clicked() {
                        *act = Some(TreeAct::Duplicate(path.clone()));
                        ui.close();
                    }
                    if ui.button(t!("Copiar como curl")).clicked() {
                        *act = Some(TreeAct::Curl(path.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(t!("Borrar…")).clicked() {
                        *dialog = Some(Dialog::Delete { path: path.clone() });
                        ui.close();
                    }
                });
            }
        }
    }
}

fn space_settings_ui(ui: &mut Ui, file: &mut SpaceFile, secrets: &mut Secrets, env_sel: &mut usize) {
    // Al abrir, los secretos se muestran en su sitio (se separan al guardar).
    for v in &mut file.vars {
        if v.secret && v.value.is_empty() {
            if let Some(s) = secrets.variables.get(&v.name) {
                v.value = s.clone();
            }
        }
    }
    for e in &mut file.envs {
        for v in &mut e.vars {
            if v.secret && v.value.is_empty() {
                if let Some(s) = secrets.entornos.get(&e.name).and_then(|m| m.get(&v.name)) {
                    v.value = s.clone();
                }
            }
        }
    }
    ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        Grid::new("spn").num_columns(2).show(ui, |ui| {
            ui.label(t!("Nombre"));
            ui.add(TextEdit::singleline(&mut file.name).desired_width(320.0));
            ui.end_row();
        });
        ui.add_space(8.0);
        CollapsingHeader::new(RichText::new(t!("Variables del espacio")).strong()).default_open(true).show(ui, |ui| {
            vars_editor(ui, "vars", &mut file.vars);
        });
        CollapsingHeader::new(RichText::new(t!("Entornos")).strong()).default_open(true).show(ui, |ui| {
            ui.weak(t!("Cada entorno puede redefinir variables (por ejemplo, base en desarrollo y en producción)."));
            ui.horizontal_wrapped(|ui| {
                for (i, e) in file.envs.iter().enumerate() {
                    ui.selectable_value(env_sel, i, &e.name);
                }
                if ui.add(IconButton::new(Icon::Plus, Tone::File).label(t!("Entorno"))).clicked() {
                    file.envs.push(Environment { name: tf!("entorno {n}", n = file.envs.len() + 1), vars: vec![] });
                    *env_sel = file.envs.len() - 1;
                }
            });
            if let Some(e) = file.envs.get_mut(*env_sel) {
                let mut remove = false;
                ui.horizontal(|ui| {
                    ui.label(t!("Nombre"));
                    ui.add(TextEdit::singleline(&mut e.name).desired_width(200.0));
                    remove = ui.button(t!("Quitar entorno")).clicked();
                });
                vars_editor(ui, "envvars", &mut e.vars);
                if remove {
                    file.envs.remove(*env_sel);
                    *env_sel = 0;
                }
            }
        });
        CollapsingHeader::new(RichText::new(t!("Autenticación común")).strong()).default_open(true).show(ui, |ui| {
            ui.weak(t!("La usan las peticiones con «Heredar del espacio»."));
            auth_editor(ui, &mut file.auth, true);
        });
        CollapsingHeader::new(RichText::new(t!("Cabeceras comunes")).strong()).default_open(false).show(ui, |ui| {
            pairs_editor(ui, "spheaders", &mut file.headers, t!("cabecera"), t!("valor"));
        });
    });
}

fn vars_editor(ui: &mut Ui, id: &str, vars: &mut Vec<Var>) {
    if vars.last().is_none_or(|v| !v.name.is_empty()) {
        vars.push(Var::default());
    }
    let mut remove = None;
    let n = vars.len();
    let h = ui.spacing().interact_size.y;
    let w = ui.available_width();
    let nw = ((w - 110.0) * 0.3).max(120.0);
    let vw = (w - 110.0 - nw).max(160.0);
    ui.horizontal(|ui| {
        ui.add_sized([nw, h], Label::new(RichText::new(t!("Nombre")).weak()));
        ui.add_sized([vw, h], Label::new(RichText::new(t!("Valor")).weak()));
        ui.weak(t!("Secreta"));
    });
    let _ = id;
    for (i, v) in vars.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add_sized([nw, h], TextEdit::singleline(&mut v.name).hint_text(t!("nombre")).font(TextStyle::Monospace));
            ui.add_sized([vw, h], TextEdit::singleline(&mut v.value).password(v.secret).font(TextStyle::Monospace));
            ui.checkbox(&mut v.secret, "").on_hover_text(t!("Su valor se guarda solo en este equipo"));
            if i + 1 < n && ui.small_button("✖").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        vars.remove(i);
    }
}

/// Ejecutor mínimo: espera un futuro aparcando el hilo.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        if let std::task::Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

impl eframe::App for App {
    fn persist_egui_memory(&self) -> bool {
        false
    }

    /// Pruebas: PETICIONES_RATON='x,y;d;u;t:texto;k:Ctrl+S;r' mueve el ratón,
    /// pulsa (d) y suelta (u), escribe, pulsa una tecla o hace clic derecho (r):
    /// un paso por fotograma desde el 10 (solo en modo captura).
    fn raw_input_hook(&mut self, _ctx: &Context, raw: &mut RawInput) {
        if self.screenshot.is_none() || !(10..1000).contains(&self.frames) {
            return;
        }
        let Ok(script) = std::env::var("PETICIONES_RATON") else { return };
        let Some(step) = script.split(';').nth(self.test_step) else { return };
        self.test_step += 1;
        let pos = self.test_mouse;
        match step.trim() {
            st if st.starts_with("t:") => raw.events.push(Event::Text(st[2..].to_string())),
            st if st.starts_with("k:") => {
                let mut modifiers = Modifiers::NONE;
                let mut name = &st[2..];
                loop {
                    if let Some(n) = name.strip_prefix("Shift+") {
                        modifiers.shift = true;
                        name = n;
                    } else if let Some(n) = name.strip_prefix("Ctrl+") {
                        modifiers = modifiers | Modifiers::COMMAND;
                        name = n;
                    } else {
                        break;
                    }
                }
                if let Some(key) = Key::from_name(name) {
                    for pressed in [true, false] {
                        raw.events.push(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
                    }
                }
            }
            "d" | "u" => raw.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: step.trim() == "d", modifiers: Modifiers::NONE }),
            "r" => {
                for pressed in [true, false] {
                    raw.events.push(Event::PointerButton { pos, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
                }
            }
            xy => {
                if let Some((x, y)) = xy.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                    self.test_mouse = pos2(x, y);
                    raw.events.push(Event::PointerMoved(self.test_mouse));
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_responses();
        self.poll_pick();
        self.poll_fetch();
        self.autosave();
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) {
            if let Some(a) = self.active {
                self.save_tab(a);
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::W)) {
            if let Some(a) = self.active {
                self.close_tab(a);
            }
        }
        let (settings_key, keys_key) = crate::estilo::atajos_comunes(&ctx);
        if settings_key {
            self.dialog = if matches!(self.dialog, Some(Dialog::Settings)) { None } else { Some(Dialog::Settings) };
        }
        if keys_key {
            self.show_keys = !self.show_keys;
        }
        self.vigia.tick(&ctx, self.session.check_updates.unwrap_or(true) && self.screenshot.is_none());
        Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(2.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        self.status_bar(ui);
        Panel::left("sidebar").resizable(true).default_size(250.0).min_size(160.0).show(ui, |ui| self.sidebar(ui));
        CentralPanel::default().show(ui, |ui| {
            if !self.tabs.is_empty() {
                self.tabs_strip(ui);
                ui.separator();
            }
            self.editor(ui);
        });
        self.dialogs(&ctx);
        self.about_window(&ctx);
        self.updater_window(&ctx);
        crate::estilo::ventana_atajos(
            &ctx,
            &mut self.show_keys,
            &[(
                t!("Peticiones"),
                vec![
                    (crate::estilo::tecla("Ctrl+Enter"), t!("Enviar la petición")),
                    (crate::estilo::tecla("Ctrl+S"), t!("Guardar la petición")),
                    (crate::estilo::tecla("Ctrl+W"), t!("Cerrar la pestaña")),
                    (t!("Pegar un curl en la URL").to_string(), t!("Lo convierte en petición")),
                ],
            )],
        );
        self.avisos.mostrar(&ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            for i in 0..self.tabs.len() {
                if self.tabs[i].req != self.tabs[i].saved {
                    self.save_tab(i);
                }
            }
            self.save_session(true);
        } else if self.session_dirty && self.last_session_save.elapsed() > Duration::from_secs(2) {
            self.save_session(false);
        }
        self.screenshot_step(&ctx);
    }
}
