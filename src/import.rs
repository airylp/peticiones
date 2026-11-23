//! Importar peticiones desde curl, Postman (colección v2.0/v2.1 y entorno),
//! OpenAPI 3 / Swagger 2 (JSON o YAML) y WSDL 1.1; exportar un espacio como
//! colección de Postman v2.1.

use crate::model::{Auth, Body, BodyKind, Environment, KeyPlace, OAuth2, OAuthFlow, Pair, Request, SpaceFile, Var};
use serde_json::{Map, Value, json};
use std::path::Path;

/// Resultado de una importación, independiente del formato.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Imported {
    pub format: &'static str,
    pub name: String,
    pub vars: Vec<Var>,
    pub envs: Vec<Environment>,
    pub auth: Option<Auth>,
    pub items: Vec<Item>,
    /// Avisos (lo que no se pudo convertir).
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Folder { name: String, items: Vec<Item> },
    Req(Request),
}

impl Imported {
    pub fn count(&self) -> usize {
        fn c(v: &[Item]) -> usize {
            v.iter().map(|i| match i {
                Item::Folder { items, .. } => c(items),
                Item::Req(_) => 1,
            }).sum()
        }
        c(&self.items)
    }
}

/// Reconoce el formato y lo convierte.
pub fn parse(text: &str) -> Result<Imported, String> {
    let t = text.trim_start_matches('\u{feff}').trim();
    if t.is_empty() {
        return Err(t!("no hay nada que importar").into());
    }
    if t.starts_with("curl ") || t.starts_with("curl\t") || t == "curl" {
        let r = parse_curl(t)?;
        return Ok(Imported { format: "curl", name: r.name.clone(), items: vec![Item::Req(r)], ..Default::default() });
    }
    if t.starts_with('<') {
        return parse_wsdl(t);
    }
    let v: Value = if t.starts_with('{') {
        serde_json::from_str(t).map_err(|e| tf!("JSON no válido: {e}", e = e))?
    } else {
        serde_yaml_ng::from_str(t).map_err(|e| tf!("no se reconoce el formato (ni curl, JSON, YAML ni XML): {e}", e = e))?
    };
    if v.get("openapi").is_some() || v.get("swagger").is_some() {
        return parse_openapi(&v);
    }
    if v.get("info").and_then(|i| i.get("schema")).and_then(|s| s.as_str()).is_some_and(|s| s.contains("postman")) || v.get("item").is_some() {
        return Ok(parse_postman(&v));
    }
    if v.get("values").is_some_and(|x| x.is_array()) {
        return Ok(parse_postman_env(&v));
    }
    Err(t!("no se reconoce el formato: se admite curl, Postman, OpenAPI/Swagger y WSDL").into())
}

// ------------------------------------------------------------------ curl

/// Separa en palabras como lo haría un shell (comillas simples y dobles,
/// `$'…'`, barra invertida y continuaciones de línea `\` o `^`).
pub fn shell_words(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some('\n') => {}
                Some('\r') => {
                    if it.peek() == Some(&'\n') {
                        it.next();
                    }
                }
                Some(n) => {
                    cur.push(n);
                    has = true;
                }
                None => {}
            },
            // Continuación de cmd.exe.
            '^' if matches!(it.peek(), Some('\n') | Some('\r')) => {
                while matches!(it.peek(), Some('\n') | Some('\r')) {
                    it.next();
                }
            }
            '\'' => {
                has = true;
                loop {
                    match it.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => return Err(t!("comilla simple sin cerrar").into()),
                    }
                }
            }
            '$' if it.peek() == Some(&'\'') => {
                it.next();
                has = true;
                loop {
                    match it.next() {
                        Some('\'') => break,
                        Some('\\') => match it.next() {
                            Some('n') => cur.push('\n'),
                            Some('t') => cur.push('\t'),
                            Some('r') => cur.push('\r'),
                            Some(x) => cur.push(x),
                            None => break,
                        },
                        Some(x) => cur.push(x),
                        None => return Err(t!("comilla simple sin cerrar").into()),
                    }
                }
            }
            '"' => {
                has = true;
                loop {
                    match it.next() {
                        Some('"') => break,
                        Some('\\') => match it.next() {
                            Some(x @ ('"' | '\\' | '$' | '`')) => cur.push(x),
                            Some('\n') => {}
                            Some(x) => {
                                cur.push('\\');
                                cur.push(x);
                            }
                            None => break,
                        },
                        Some(x) => cur.push(x),
                        None => return Err(t!("comilla doble sin cerrar").into()),
                    }
                }
            }
            c if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    Ok(out)
}

/// Convierte un comando curl en una petición.
pub fn parse_curl(cmd: &str) -> Result<Request, String> {
    let w = shell_words(cmd)?;
    let mut r = Request::default();
    let mut method: Option<String> = None;
    let mut data: Vec<String> = Vec::new();
    let mut form_fields: Vec<Pair> = Vec::new();
    let mut get = false;
    let mut digest = false;
    let mut user: Option<String> = None;
    let mut i = 1;
    let mut url = String::new();
    while i < w.len() {
        let a = w[i].as_str();
        // `-XPOST`, `--request=POST` y `-HAccept: x` también valen.
        let (flag, inline) = if let Some(rest) = a.strip_prefix("--") {
            match rest.split_once('=') {
                Some((f, v)) => (format!("--{f}"), Some(v.to_string())),
                None => (a.to_string(), None),
            }
        } else if a.starts_with('-') && a.len() > 2 && "XHdubAeFo".contains(&a[1..2]) {
            (a[..2].to_string(), Some(a[2..].to_string()))
        } else {
            (a.to_string(), None)
        };
        let takes = matches!(
            flag.as_str(),
            "-X" | "--request" | "-H" | "--header" | "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" | "--data-urlencode" | "--json" | "-u" | "--user" | "-A" | "--user-agent" | "-b" | "--cookie" | "-e" | "--referer" | "-F" | "--form" | "--form-string" | "--url" | "-o" | "--output" | "--oauth2-bearer" | "-m" | "--max-time" | "--connect-timeout" | "-x" | "--proxy" | "-w" | "--write-out" | "--cacert" | "--cert" | "--key" | "-E"
        );
        let val = if takes {
            match inline {
                Some(v) => Some(v),
                None => {
                    i += 1;
                    w.get(i).cloned()
                }
            }
        } else {
            None
        };
        match flag.as_str() {
            "-X" | "--request" => method = val,
            "-H" | "--header" => {
                if let Some((k, v)) = val.as_deref().and_then(|h| h.split_once(':')) {
                    r.headers.push(Pair { name: k.trim().into(), value: v.trim().into(), enabled: true });
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" => data.push(val.unwrap_or_default()),
            "--data-urlencode" => {
                let v = val.unwrap_or_default();
                data.push(match v.split_once('=') {
                    Some((k, x)) => format!("{k}={}", crate::http::encode(x)),
                    None => crate::http::encode(&v),
                });
            }
            "--json" => {
                data.push(val.unwrap_or_default());
                r.headers.push(Pair { name: "Content-Type".into(), value: "application/json".into(), enabled: true });
                r.headers.push(Pair { name: "Accept".into(), value: "application/json".into(), enabled: true });
            }
            "-F" | "--form" | "--form-string" => {
                if let Some((k, v)) = val.as_deref().and_then(|f| f.split_once('=')) {
                    form_fields.push(Pair { name: k.into(), value: v.into(), enabled: true });
                }
            }
            "-u" | "--user" => user = val,
            "--digest" => digest = true,
            "-A" | "--user-agent" => r.headers.push(Pair { name: "User-Agent".into(), value: val.unwrap_or_default(), enabled: true }),
            "-b" | "--cookie" => r.headers.push(Pair { name: "Cookie".into(), value: val.unwrap_or_default(), enabled: true }),
            "-e" | "--referer" => r.headers.push(Pair { name: "Referer".into(), value: val.unwrap_or_default(), enabled: true }),
            "--oauth2-bearer" => r.auth = Auth::Bearer { token: val.unwrap_or_default() },
            "-G" | "--get" => get = true,
            "-I" | "--head" => method = Some("HEAD".into()),
            "--url" => url = val.unwrap_or_default(),
            _ if takes => {}
            f if f.starts_with('-') => {}
            _ => {
                if url.is_empty() {
                    url = a.to_string();
                }
            }
        }
        i += 1;
    }
    if url.is_empty() {
        return Err(t!("el comando curl no tiene URL").into());
    }
    if let Some(u) = user {
        let (us, pw) = u.split_once(':').map_or((u.clone(), String::new()), |(a, b)| (a.into(), b.into()));
        r.auth = if digest { Auth::Digest { user: us, password: pw } } else { Auth::Basic { user: us, password: pw } };
    } else if !matches!(r.auth, Auth::Bearer { .. }) {
        r.auth = Auth::None;
    }
    // Un Authorization: Bearer explícito se queda como cabecera (es lo que el usuario escribió).
    let body = data.join("&");
    if get && !body.is_empty() {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&body);
    } else if !body.is_empty() {
        let ct = r.headers.iter().find(|h| h.name.eq_ignore_ascii_case("content-type")).map(|h| h.value.to_ascii_lowercase()).unwrap_or_default();
        let t = body.trim_start();
        r.body.kind = if ct.contains("soap+xml") {
            BodyKind::Soap12
        } else if ct.contains("json") || ((t.starts_with('{') || t.starts_with('[')) && ct.is_empty()) {
            BodyKind::Json
        } else if ct.contains("xml") || (t.starts_with('<') && ct.is_empty()) {
            if r.headers.iter().any(|h| h.name.eq_ignore_ascii_case("soapaction")) { BodyKind::Soap11 } else { BodyKind::Xml }
        } else if ct.is_empty() || ct.contains("x-www-form-urlencoded") {
            // Lo que curl manda por defecto con -d.
            if ct.is_empty() {
                r.headers.push(Pair { name: "Content-Type".into(), value: "application/x-www-form-urlencoded".into(), enabled: true });
            }
            BodyKind::Text
        } else {
            BodyKind::Text
        };
        if r.body.kind == BodyKind::Soap11 {
            if let Some(p) = r.headers.iter().position(|h| h.name.eq_ignore_ascii_case("soapaction")) {
                r.body.soap_action = r.headers.remove(p).value.trim_matches('"').to_string();
            }
        }
        r.body.text = body;
    } else if !form_fields.is_empty() {
        r.body.kind = BodyKind::Form;
        r.body.fields = form_fields;
    }
    r.method = method.map(|m| m.to_ascii_uppercase()).unwrap_or_else(|| if !r.body.text.is_empty() || !r.body.fields.is_empty() { "POST".into() } else { "GET".into() });
    r.url = url;
    r.name = default_name(&r.method, &r.url);
    Ok(r)
}

/// «GET /users/1» a partir de la URL.
fn default_name(method: &str, url: &str) -> String {
    let after = url.split_once("://").map_or(url, |(_, r)| r);
    let path = after.find('/').map_or("/", |i| &after[i..]);
    let path = path.split(['?', '#']).next().unwrap_or("/");
    format!("{method} {path}")
}

// ------------------------------------------------------------------ Postman

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(x) => x.to_string(),
    }
}

fn pm_pairs(v: Option<&Value>) -> Vec<Pair> {
    v.and_then(|x| x.as_array()).map(|a| a.iter().filter(|p| p.is_object()).map(|p| Pair { name: s(p, "key"), value: s(p, "value"), enabled: !p.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false) }).collect()).unwrap_or_default()
}

/// Atributos de auth de Postman: `[{key, value}]` (v2.1) u objeto (v2.0).
fn pm_auth_attr(a: &Value, kind: &str, key: &str) -> String {
    match a.get(kind) {
        Some(Value::Array(v)) => v.iter().find(|x| s(x, "key") == key).map(|x| s(x, "value")).unwrap_or_default(),
        Some(o @ Value::Object(_)) => s(o, key),
        _ => String::new(),
    }
}

fn pm_auth(a: &Value) -> Option<Auth> {
    let kind = a.get("type")?.as_str()?;
    let g = |k: &str| pm_auth_attr(a, kind, k);
    Some(match kind {
        "noauth" => Auth::None,
        "basic" => Auth::Basic { user: g("username"), password: g("password") },
        "digest" => Auth::Digest { user: g("username"), password: g("password") },
        "bearer" => Auth::Bearer { token: g("token") },
        "apikey" => Auth::ApiKey { name: g("key"), value: g("value"), place: if g("in") == "query" { KeyPlace::Query } else { KeyPlace::Header } },
        "oauth2" => Auth::OAuth2(OAuth2 {
            flow: if g("grant_type") == "authorization_code_with_pkce" || g("grant_type") == "authorization_code" { OAuthFlow::AuthorizationCode } else { OAuthFlow::ClientCredentials },
            token_url: g("accessTokenUrl"),
            auth_url: g("authUrl"),
            client_id: g("clientId"),
            client_secret: g("clientSecret"),
            scope: g("scope"),
            redirect_uri: g("redirect_uri"),
            basic_credentials: g("client_authentication") == "header",
            ..Default::default()
        }),
        _ => return None,
    })
}

fn pm_request(name: &str, v: &Value, notes: &mut Vec<String>) -> Request {
    let mut r = Request { name: name.into(), auth: Auth::Inherit, ..Default::default() };
    // v2: `request` puede ser solo la URL.
    if let Some(u) = v.as_str() {
        r.url = u.into();
        return r;
    }
    r.method = v.get("method").and_then(|m| m.as_str()).unwrap_or("GET").to_ascii_uppercase();
    match v.get("url") {
        Some(Value::String(u)) => r.url = u.clone(),
        Some(u @ Value::Object(_)) => {
            let raw = s(u, "raw");
            // Los parámetros de la tabla de Postman ya van en `raw`: se separan
            // para editarlos en la tabla (los desactivados solo están en `query`).
            let base = raw.split_once('?').map_or(raw.as_str(), |(b, _)| b).to_string();
            let q = pm_pairs(u.get("query"));
            if q.is_empty() {
                r.url = raw;
            } else {
                r.url = base;
                r.params = q;
            }
        }
        _ => {}
    }
    r.headers = pm_pairs(v.get("header"));
    if let Some(b) = v.get("body") {
        match b.get("mode").and_then(|m| m.as_str()).unwrap_or("") {
            "raw" => {
                r.body.text = s(b, "raw");
                let lang = b.get("options").and_then(|o| o.get("raw")).map(|x| s(x, "language")).unwrap_or_default();
                let ct = r.headers.iter().find(|h| h.name.eq_ignore_ascii_case("content-type")).map(|h| h.value.to_ascii_lowercase()).unwrap_or_default();
                r.body.kind = if ct.contains("soap+xml") {
                    BodyKind::Soap12
                } else if lang == "json" || ct.contains("json") {
                    BodyKind::Json
                } else if lang == "xml" || ct.contains("xml") {
                    if let Some(p) = r.headers.iter().position(|h| h.name.eq_ignore_ascii_case("soapaction")) {
                        r.body.soap_action = r.headers.remove(p).value.trim_matches('"').to_string();
                        BodyKind::Soap11
                    } else {
                        BodyKind::Xml
                    }
                } else {
                    BodyKind::Text
                };
            }
            "urlencoded" => {
                r.body.kind = BodyKind::Form;
                r.body.fields = pm_pairs(b.get("urlencoded"));
            }
            "formdata" => {
                r.body.kind = BodyKind::Form;
                r.body.fields = pm_pairs(b.get("formdata"));
                notes.push(tf!("«{n}»: multipart/form-data se importó como formulario (los archivos adjuntos no se admiten aún)", n = name));
            }
            "graphql" => {
                let g = b.get("graphql").cloned().unwrap_or(Value::Null);
                let vars: Value = serde_json::from_str(&s(&g, "variables")).unwrap_or(json!({}));
                r.body.kind = BodyKind::Json;
                r.body.text = serde_json::to_string_pretty(&json!({"query": s(&g, "query"), "variables": vars})).unwrap_or_default();
            }
            _ => {}
        }
    }
    if let Some(a) = v.get("auth").and_then(pm_auth) {
        r.auth = a;
    }
    if let Some(d) = v.get("description") {
        r.notes = d.as_str().map(String::from).unwrap_or_else(|| s(d, "content"));
    }
    r
}

fn pm_items(v: &Value, notes: &mut Vec<String>) -> Vec<Item> {
    let mut out = Vec::new();
    for it in v.as_array().into_iter().flatten() {
        let name = s(it, "name");
        if let Some(children) = it.get("item") {
            out.push(Item::Folder { name, items: pm_items(children, notes) });
            if it.get("auth").is_some() {
                notes.push(tf!("la carpeta «{n}» tenía su propia autenticación: revisa sus peticiones", n = s(it, "name")));
            }
        } else if let Some(req) = it.get("request") {
            out.push(Item::Req(pm_request(&name, req, notes)));
        }
    }
    out
}

fn parse_postman(v: &Value) -> Imported {
    let mut notes = Vec::new();
    let items = pm_items(v.get("item").unwrap_or(&Value::Null), &mut notes);
    let vars = v.get("variable").and_then(|x| x.as_array()).map(|a| a.iter().map(|p| Var { name: s(p, "key"), value: s(p, "value"), secret: s(p, "type") == "secret" }).collect()).unwrap_or_default();
    let name = v.get("info").map(|i| s(i, "name")).unwrap_or_default();
    if v.get("event").is_some() || v.to_string().contains("\"listen\"") {
        notes.push(t!("los scripts (pre-request/tests) de Postman no se importan").into());
    }
    Imported { format: "Postman", name, vars, auth: v.get("auth").and_then(pm_auth), items, notes, ..Default::default() }
}

fn parse_postman_env(v: &Value) -> Imported {
    let vars = v["values"].as_array().into_iter().flatten().filter(|p| p.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true)).map(|p| Var { name: s(p, "key"), value: s(p, "value"), secret: s(p, "type") == "secret" }).collect();
    let name = s(v, "name");
    Imported { format: t!("entorno de Postman"), envs: vec![Environment { name: name.clone(), vars }], name, ..Default::default() }
}

/// Exporta un espacio a colección de Postman v2.1 (las variables secretas van vacías).
pub fn export_postman(space: &SpaceFile, dir: &Path) -> String {
    fn pairs(v: &[Pair]) -> Value {
        Value::Array(v.iter().map(|p| {
            let mut o = json!({"key": p.name, "value": p.value});
            if !p.enabled {
                o["disabled"] = json!(true);
            }
            o
        }).collect())
    }
    fn kv(a: &[(&str, &str)]) -> Value {
        Value::Array(a.iter().map(|(k, v)| json!({"key": k, "value": v, "type": "string"})).collect())
    }
    fn auth(a: &Auth) -> Option<Value> {
        Some(match a {
            Auth::Inherit => return None,
            Auth::None => json!({"type": "noauth"}),
            Auth::Basic { user, password } => json!({"type": "basic", "basic": kv(&[("username", user), ("password", password)])}),
            Auth::Digest { user, password } => json!({"type": "digest", "digest": kv(&[("username", user), ("password", password)])}),
            Auth::Bearer { token } => json!({"type": "bearer", "bearer": kv(&[("token", token)])}),
            Auth::ApiKey { name, value, place } => json!({"type": "apikey", "apikey": kv(&[("key", name), ("value", value), ("in", if *place == KeyPlace::Query { "query" } else { "header" })])}),
            Auth::OAuth2(o) => {
                let grant = if o.flow == OAuthFlow::AuthorizationCode { "authorization_code_with_pkce" } else { "client_credentials" };
                json!({"type": "oauth2", "oauth2": kv(&[("grant_type", grant), ("accessTokenUrl", &o.token_url), ("authUrl", &o.auth_url), ("clientId", &o.client_id), ("clientSecret", &o.client_secret), ("scope", &o.scope), ("redirect_uri", &o.redirect_uri), ("client_authentication", if o.basic_credentials { "header" } else { "body" })])})
            }
        })
    }
    fn request(r: &Request) -> Value {
        let mut headers = r.headers.clone();
        if r.body.kind.is_soap() || matches!(r.body.kind, BodyKind::Json | BodyKind::Xml) {
            if !headers.iter().any(|h| h.name.eq_ignore_ascii_case("content-type")) {
                if let Some(ct) = r.body.kind.content_type(&r.body.soap_action) {
                    headers.push(Pair { name: "Content-Type".into(), value: ct, enabled: true });
                }
            }
            if r.body.kind == BodyKind::Soap11 && !r.body.soap_action.is_empty() {
                headers.push(Pair { name: "SOAPAction".into(), value: format!("\"{}\"", r.body.soap_action), enabled: true });
            }
        }
        let mut raw = r.url.clone();
        let active: Vec<&Pair> = r.params.iter().filter(|p| p.enabled).collect();
        if !active.is_empty() {
            raw.push(if raw.contains('?') { '&' } else { '?' });
            raw.push_str(&active.iter().map(|p| if p.value.is_empty() { p.name.clone() } else { format!("{}={}", p.name, p.value) }).collect::<Vec<_>>().join("&"));
        }
        let mut o = json!({"method": r.method, "header": pairs(&headers), "url": {"raw": raw, "query": pairs(&r.params)}});
        match r.body.kind {
            BodyKind::None => {}
            BodyKind::Form => o["body"] = json!({"mode": "urlencoded", "urlencoded": pairs(&r.body.fields)}),
            k => {
                let lang = match k {
                    BodyKind::Json => "json",
                    BodyKind::Xml | BodyKind::Soap11 | BodyKind::Soap12 => "xml",
                    _ => "text",
                };
                o["body"] = json!({"mode": "raw", "raw": r.body.text, "options": {"raw": {"language": lang}}});
            }
        }
        if let Some(a) = auth(&r.auth) {
            o["auth"] = a;
        }
        if !r.notes.is_empty() {
            o["description"] = json!(r.notes);
        }
        o
    }
    fn walk(nodes: &[crate::store::Node]) -> Vec<Value> {
        nodes.iter().filter_map(|n| match n {
            crate::store::Node::Folder { name, children, .. } => Some(json!({"name": name, "item": walk(children)})),
            crate::store::Node::Req { path, .. } => crate::store::load_request(path).ok().map(|r| json!({"name": r.name, "request": request(&r)})),
        }).collect()
    }
    let mut col = Map::new();
    col.insert("info".into(), json!({"name": space.name, "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}));
    col.insert("item".into(), Value::Array(walk(&crate::store::scan_tree(dir))));
    col.insert("variable".into(), Value::Array(space.vars.iter().map(|v| json!({"key": v.name, "value": if v.secret { "" } else { &v.value }})).collect()));
    if let Some(a) = auth(&space.auth) {
        col.insert("auth".into(), a);
    }
    serde_json::to_string_pretty(&Value::Object(col)).unwrap_or_default()
}

// ------------------------------------------------------------------ OpenAPI / Swagger

/// Resuelve `$ref` locales (`#/components/schemas/X`).
fn deref<'a>(root: &'a Value, v: &'a Value) -> &'a Value {
    let mut cur = v;
    for _ in 0..20 {
        let Some(r) = cur.get("$ref").and_then(|x| x.as_str()) else { return cur };
        let Some(path) = r.strip_prefix("#/") else { return cur };
        let mut t = root;
        for part in path.split('/') {
            let part = part.replace("~1", "/").replace("~0", "~");
            match t.get(&part) {
                Some(x) => t = x,
                None => return &Value::Null,
            }
        }
        cur = t;
    }
    cur
}

/// Ejemplo a partir de un esquema JSON (OpenAPI).
fn sample(root: &Value, schema: &Value, depth: usize) -> Value {
    let sc = deref(root, schema);
    if depth > 8 {
        return Value::Null;
    }
    if let Some(e) = sc.get("example") {
        return e.clone();
    }
    if let Some(e) = sc.get("examples").and_then(|e| e.as_array()).and_then(|a| a.first()) {
        return e.clone();
    }
    if let Some(d) = sc.get("default") {
        return d.clone();
    }
    if let Some(e) = sc.get("enum").and_then(|e| e.as_array()).and_then(|a| a.first()) {
        return e.clone();
    }
    if let Some(all) = sc.get("allOf").and_then(|a| a.as_array()) {
        let mut m = Map::new();
        for p in all {
            if let Value::Object(o) = sample(root, p, depth + 1) {
                m.extend(o);
            }
        }
        return Value::Object(m);
    }
    for k in ["oneOf", "anyOf"] {
        if let Some(first) = sc.get(k).and_then(|a| a.as_array()).and_then(|a| a.first()) {
            return sample(root, first, depth + 1);
        }
    }
    let ty = match sc.get("type") {
        Some(Value::String(t)) => t.as_str(),
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).find(|t| *t != "null").unwrap_or("null"),
        _ if sc.get("properties").is_some() => "object",
        _ if sc.get("items").is_some() => "array",
        _ => "",
    };
    match ty {
        "object" => {
            let mut m = Map::new();
            if let Some(props) = sc.get("properties").and_then(|p| p.as_object()) {
                for (k, p) in props {
                    if deref(root, p).get("readOnly").and_then(|x| x.as_bool()) == Some(true) {
                        continue;
                    }
                    m.insert(k.clone(), sample(root, p, depth + 1));
                }
            }
            Value::Object(m)
        }
        "array" => json!([sample(root, sc.get("items").unwrap_or(&Value::Null), depth + 1)]),
        "integer" => json!(0),
        "number" => json!(0.0),
        "boolean" => json!(false),
        "string" => json!(match sc.get("format").and_then(|f| f.as_str()).unwrap_or("") {
            "date" => "2026-01-31",
            "date-time" => "2026-01-31T12:00:00Z",
            "email" => "persona@ejemplo.com",
            "uuid" => "3fa85f64-5717-4562-b3fc-2c963f66afa6",
            "uri" | "url" => "https://ejemplo.com",
            "binary" | "byte" => "",
            _ => "texto",
        }),
        _ => Value::Null,
    }
}

fn param_value(root: &Value, p: &Value) -> String {
    let v = p.get("example").cloned().or_else(|| p.get("schema").map(|s| sample(root, s, 0))).or_else(|| p.get("default").cloned()).unwrap_or(Value::Null);
    match v {
        Value::String(s) => s,
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

fn parse_openapi(root: &Value) -> Result<Imported, String> {
    let swagger2 = root.get("swagger").is_some();
    let info = root.get("info").cloned().unwrap_or(Value::Null);
    let mut out = Imported { format: if swagger2 { "Swagger 2" } else { "OpenAPI" }, name: s(&info, "title"), ..Default::default() };
    // Servidor → variable `base`.
    let base = if swagger2 {
        let scheme = root.get("schemes").and_then(|x| x.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()).unwrap_or("https");
        let host = s(root, "host");
        if host.is_empty() { s(root, "basePath") } else { format!("{scheme}://{host}{}", s(root, "basePath")) }
    } else {
        let srv = root.get("servers").and_then(|x| x.as_array()).and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        let mut u = s(&srv, "url");
        if let Some(vars) = srv.get("variables").and_then(|v| v.as_object()) {
            for (k, v) in vars {
                u = u.replace(&format!("{{{k}}}"), &s(v, "default"));
            }
        }
        u
    };
    out.vars.push(Var { name: "base".into(), value: base.trim_end_matches('/').to_string(), secret: false });
    if let Some(servers) = root.get("servers").and_then(|x| x.as_array()).filter(|a| a.len() > 1) {
        for (i, sv) in servers.iter().enumerate() {
            let name = { let d = s(sv, "description"); if d.is_empty() { tf!("servidor {n}", n = i + 1) } else { d } };
            out.envs.push(Environment { name, vars: vec![Var { name: "base".into(), value: s(sv, "url").trim_end_matches('/').to_string(), secret: false }] });
        }
    }

    // Seguridad global → auth del espacio (con variables secretas).
    let schemes = if swagger2 { root.get("securityDefinitions") } else { root.get("components").and_then(|c| c.get("securitySchemes")) };
    let global = root.get("security").and_then(|s| s.as_array()).and_then(|a| a.first()).and_then(|o| o.as_object()).and_then(|o| o.keys().next().cloned());
    let pick = global.or_else(|| schemes.and_then(|s| s.as_object()).and_then(|o| o.keys().next().cloned()));
    if let (Some(name), Some(schemes)) = (pick, schemes) {
        let sc = deref(root, &schemes[&name]);
        let ty = s(sc, "type");
        let secret = |n: &str| Var { name: n.into(), value: String::new(), secret: true };
        let auth = match ty.as_str() {
            "http" if s(sc, "scheme").eq_ignore_ascii_case("bearer") => {
                out.vars.push(secret("token"));
                Some(Auth::Bearer { token: "{{token}}".into() })
            }
            "http" | "basic" if ty == "basic" || s(sc, "scheme").eq_ignore_ascii_case("basic") => {
                out.vars.push(Var { name: "usuario".into(), value: String::new(), secret: false });
                out.vars.push(secret("clave"));
                Some(Auth::Basic { user: "{{usuario}}".into(), password: "{{clave}}".into() })
            }
            "http" if s(sc, "scheme").eq_ignore_ascii_case("digest") => {
                out.vars.push(Var { name: "usuario".into(), value: String::new(), secret: false });
                out.vars.push(secret("clave"));
                Some(Auth::Digest { user: "{{usuario}}".into(), password: "{{clave}}".into() })
            }
            "apiKey" => {
                out.vars.push(secret("api_key"));
                Some(Auth::ApiKey { name: s(sc, "name"), value: "{{api_key}}".into(), place: if s(sc, "in") == "query" { KeyPlace::Query } else { KeyPlace::Header } })
            }
            "oauth2" => {
                // OpenAPI 3: flows.{clientCredentials|authorizationCode}; Swagger 2: flow + tokenUrl.
                let (flow, f) = if swagger2 {
                    (if s(sc, "flow") == "accessCode" { OAuthFlow::AuthorizationCode } else { OAuthFlow::ClientCredentials }, sc.clone())
                } else if let Some(f) = sc.get("flows").and_then(|f| f.get("clientCredentials")) {
                    (OAuthFlow::ClientCredentials, f.clone())
                } else if let Some(f) = sc.get("flows").and_then(|f| f.get("authorizationCode")) {
                    (OAuthFlow::AuthorizationCode, f.clone())
                } else {
                    (OAuthFlow::ClientCredentials, Value::Null)
                };
                let scope = f.get("scopes").and_then(|x| x.as_object()).map(|o| o.keys().cloned().collect::<Vec<_>>().join(" ")).unwrap_or_default();
                out.vars.push(Var { name: "client_id".into(), value: String::new(), secret: false });
                out.vars.push(secret("client_secret"));
                Some(Auth::OAuth2(OAuth2 { flow, token_url: s(&f, "tokenUrl"), auth_url: s(&f, "authorizationUrl"), client_id: "{{client_id}}".into(), client_secret: "{{client_secret}}".into(), scope, ..Default::default() }))
            }
            _ => None,
        };
        out.auth = auth;
    }

    // Operaciones, agrupadas por la primera etiqueta.
    let mut folders: Vec<(String, Vec<Item>)> = Vec::new();
    let mut loose = Vec::new();
    let paths = root.get("paths").and_then(|p| p.as_object()).cloned().unwrap_or_default();
    for (path, item) in &paths {
        let item = deref(root, item);
        let common: Vec<Value> = item.get("parameters").and_then(|p| p.as_array()).cloned().unwrap_or_default();
        for m in ["get", "post", "put", "patch", "delete", "head", "options", "trace"] {
            let Some(op) = item.get(m) else { continue };
            let mut r = Request { method: m.to_ascii_uppercase(), auth: Auth::Inherit, ..Default::default() };
            let summary = s(op, "summary");
            let op_id = s(op, "operationId");
            r.name = if !summary.is_empty() { summary } else if !op_id.is_empty() { op_id } else { format!("{} {path}", r.method) };
            r.notes = s(op, "description");
            // `{id}` del path → `{{id}}`: variable que se rellena en el espacio o a mano.
            let mut url_path = String::new();
            let mut rest = path.as_str();
            while let Some(i) = rest.find('{') {
                url_path.push_str(&rest[..i]);
                match rest[i..].find('}') {
                    Some(j) => {
                        url_path.push_str(&format!("{{{{{}}}}}", &rest[i + 1..i + j]));
                        rest = &rest[i + j + 1..];
                    }
                    None => {
                        url_path.push_str(&rest[i..]);
                        rest = "";
                    }
                }
            }
            url_path.push_str(rest);
            r.url = format!("{{{{base}}}}{url_path}");
            let mut params: Vec<Value> = common.clone();
            params.extend(op.get("parameters").and_then(|p| p.as_array()).cloned().unwrap_or_default());
            for p in &params {
                let p = deref(root, p);
                let required = p.get("required").and_then(|x| x.as_bool()).unwrap_or(false);
                let name = s(p, "name");
                match s(p, "in").as_str() {
                    "query" => r.params.push(Pair { value: param_value(root, p), name, enabled: required }),
                    "header" => r.headers.push(Pair { value: param_value(root, p), name, enabled: true }),
                    "path" => {
                        let v = param_value(root, p);
                        if !v.is_empty() && !out.vars.iter().any(|x| x.name == name) {
                            out.vars.push(Var { name: name.clone(), value: v, secret: false });
                        }
                    }
                    "body" => {
                        r.body.kind = BodyKind::Json;
                        r.body.text = serde_json::to_string_pretty(&sample(root, p.get("schema").unwrap_or(&Value::Null), 0)).unwrap_or_default();
                    }
                    "formData" => {
                        r.body.kind = BodyKind::Form;
                        r.body.fields.push(Pair { value: param_value(root, p), name, enabled: true });
                    }
                    _ => {}
                }
            }
            if let Some(rb) = op.get("requestBody").map(|x| deref(root, x)) {
                if let Some(content) = rb.get("content").and_then(|c| c.as_object()) {
                    let pick = content.keys().find(|k| k.contains("json")).or_else(|| content.keys().find(|k| k.contains("x-www-form-urlencoded"))).or_else(|| content.keys().find(|k| k.contains("xml"))).or_else(|| content.keys().next()).cloned();
                    if let Some(ct) = pick {
                        let media = &content[&ct];
                        let ex = media.get("example").cloned().or_else(|| media.get("examples").and_then(|e| e.as_object()).and_then(|o| o.values().next()).map(|e| deref(root, e).get("value").cloned().unwrap_or(Value::Null))).unwrap_or_else(|| sample(root, media.get("schema").unwrap_or(&Value::Null), 0));
                        if ct.contains("x-www-form-urlencoded") {
                            r.body.kind = BodyKind::Form;
                            if let Value::Object(o) = &ex {
                                r.body.fields = o.iter().map(|(k, v)| Pair { name: k.clone(), value: v.as_str().map(String::from).unwrap_or_else(|| v.to_string()), enabled: true }).collect();
                            }
                        } else if ct.contains("json") {
                            r.body.kind = BodyKind::Json;
                            r.body.text = serde_json::to_string_pretty(&ex).unwrap_or_default();
                        } else if ct.contains("xml") {
                            r.body.kind = BodyKind::Xml;
                            r.body.text = ex.as_str().unwrap_or("").to_string();
                        } else {
                            r.body.kind = BodyKind::Text;
                            r.body.text = ex.as_str().map(String::from).unwrap_or_default();
                            if !r.headers.iter().any(|h| h.name.eq_ignore_ascii_case("content-type")) {
                                r.headers.push(Pair { name: "Content-Type".into(), value: ct.clone(), enabled: true });
                            }
                        }
                    }
                }
            }
            // Operación sin seguridad (`security: []`).
            if op.get("security").and_then(|x| x.as_array()).is_some_and(|a| a.is_empty()) {
                r.auth = Auth::None;
            }
            let tag = op.get("tags").and_then(|t| t.as_array()).and_then(|a| a.first()).and_then(|t| t.as_str()).map(String::from);
            match tag {
                Some(t) => match folders.iter_mut().find(|(n, _)| *n == t) {
                    Some((_, v)) => v.push(Item::Req(r)),
                    None => folders.push((t, vec![Item::Req(r)])),
                },
                None => loose.push(Item::Req(r)),
            }
        }
    }
    out.items = folders.into_iter().map(|(name, items)| Item::Folder { name, items }).collect();
    out.items.extend(loose);
    if out.items.is_empty() {
        return Err(t!("la especificación no tiene operaciones").into());
    }
    Ok(out)
}

// ------------------------------------------------------------------ WSDL

const NS_SOAP11: &str = "http://schemas.xmlsoap.org/wsdl/soap/";
const NS_SOAP12: &str = "http://schemas.xmlsoap.org/wsdl/soap12/";
const NS_XSD: &str = "http://www.w3.org/2001/XMLSchema";

fn local(q: &str) -> &str {
    q.rsplit(':').next().unwrap_or(q)
}

/// Esquemas XSD del WSDL: elementos y tipos globales por nombre local.
struct Schemas<'a, 'i> {
    elements: Vec<(String, String, roxmltree::Node<'a, 'i>)>,
    types: Vec<(String, roxmltree::Node<'a, 'i>)>,
}

impl<'a, 'i> Schemas<'a, 'i> {
    fn element(&self, name: &str) -> Option<&(String, String, roxmltree::Node<'a, 'i>)> {
        self.elements.iter().find(|(n, _, _)| n == local(name))
    }
    fn ty(&self, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
        self.types.iter().find(|(n, _)| n == local(name)).map(|(_, n)| *n)
    }

    /// Hijos de ejemplo de un tipo complejo (secuencias, all, choice, extensiones).
    fn type_body(&self, ty: roxmltree::Node, indent: usize, depth: usize, out: &mut String) {
        if depth > 8 {
            return;
        }
        for c in ty.children().filter(|c| c.is_element() && c.tag_name().namespace() == Some(NS_XSD)) {
            match c.tag_name().name() {
                "sequence" | "all" | "choice" => {
                    let first_only = c.tag_name().name() == "choice";
                    for (i, e) in c.children().filter(|e| e.is_element()).enumerate() {
                        if first_only && i > 0 {
                            break;
                        }
                        match e.tag_name().name() {
                            "element" => self.element_sample(e, indent, depth + 1, out),
                            "sequence" | "choice" | "all" => self.type_body_of_group(e, indent, depth + 1, out),
                            _ => {}
                        }
                    }
                }
                "complexContent" | "simpleContent" => {
                    for ext in c.children().filter(|e| e.is_element() && matches!(e.tag_name().name(), "extension" | "restriction")) {
                        if let Some(base) = ext.attribute("base").and_then(|b| self.ty(b)) {
                            self.type_body(base, indent, depth + 1, out);
                        }
                        self.type_body(ext, indent, depth + 1, out);
                    }
                }
                _ => {}
            }
        }
    }

    fn type_body_of_group(&self, g: roxmltree::Node, indent: usize, depth: usize, out: &mut String) {
        for e in g.children().filter(|e| e.is_element() && e.tag_name().name() == "element") {
            self.element_sample(e, indent, depth, out);
        }
    }

    /// `<tns:nombre>?</tns:nombre>` o con hijos si es complejo.
    fn element_sample(&self, e: roxmltree::Node, indent: usize, depth: usize, out: &mut String) {
        let pad = "  ".repeat(indent);
        // Referencia a un elemento global.
        if let Some(r) = e.attribute("ref") {
            if let Some((_, _, g)) = self.element(r) {
                if depth < 9 {
                    self.element_sample(*g, indent, depth + 1, out);
                }
            }
            return;
        }
        let name = e.attribute("name").unwrap_or("elemento");
        let optional = e.attribute("minOccurs") == Some("0");
        let note = if optional { "<!-- opcional -->" } else { "" };
        let inline = e.children().find(|c| c.is_element() && c.tag_name().name() == "complexType");
        let complex = inline.or_else(|| e.attribute("type").and_then(|t| self.ty(t)).filter(|t| t.tag_name().name() == "complexType"));
        match complex {
            Some(ct) => {
                let mut inner = String::new();
                self.type_body(ct, indent + 1, depth, &mut inner);
                if inner.is_empty() {
                    out.push_str(&format!("{pad}<tns:{name}/>{note}\n"));
                } else {
                    out.push_str(&format!("{pad}<tns:{name}>{note}\n{inner}{pad}</tns:{name}>\n"));
                }
            }
            None => {
                let ty = e.attribute("type").map(local).unwrap_or("string");
                out.push_str(&format!("{pad}<tns:{name}>?</tns:{name}>{note}<!-- {ty} -->\n"));
            }
        }
    }
}

fn kids<'a, 'i>(n: roxmltree::Node<'a, 'i>, tag: &str) -> Vec<roxmltree::Node<'a, 'i>> {
    n.children().filter(|c| c.is_element() && c.tag_name().name() == tag).collect()
}

fn parse_wsdl(text: &str) -> Result<Imported, String> {
    let doc = roxmltree::Document::parse_with_options(text, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).map_err(|e| tf!("XML no válido: {e}", e = e))?;
    let defs = doc.root_element();
    if defs.tag_name().name() != "definitions" {
        if defs.tag_name().name() == "description" {
            return Err(t!("WSDL 2.0 no está soportado (solo WSDL 1.1)").into());
        }
        return Err(t!("el XML no es un WSDL (falta <definitions>)").into());
    }
    let tns = defs.attribute("targetNamespace").unwrap_or("").to_string();
    let mut out = Imported { format: "WSDL", name: defs.attribute("name").unwrap_or("").to_string(), ..Default::default() };

    let mut schemas = Schemas { elements: vec![], types: vec![] };
    for sch in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "schema" && n.tag_name().namespace() == Some(NS_XSD)) {
        let ns = sch.attribute("targetNamespace").unwrap_or(&tns).to_string();
        for c in sch.children().filter(|c| c.is_element()) {
            let Some(name) = c.attribute("name") else { continue };
            match c.tag_name().name() {
                "element" => schemas.elements.push((name.into(), ns.clone(), c)),
                "complexType" | "simpleType" => schemas.types.push((name.into(), c)),
                _ => {}
            }
        }
        if sch.children().any(|c| c.is_element() && matches!(c.tag_name().name(), "import" | "include") && c.attribute("schemaLocation").is_some()) {
            out.notes.push(t!("el WSDL importa esquemas externos (schemaLocation): los cuerpos de ejemplo pueden quedar incompletos").into());
        }
    }
    let messages = kids(defs, "message");
    let port_types = kids(defs, "portType");

    // Puertos del servicio: (nombre del puerto, binding, dirección).
    let mut ports = Vec::new();
    for svc in kids(defs, "service") {
        if out.name.is_empty() {
            out.name = svc.attribute("name").unwrap_or("").into();
        }
        for p in kids(svc, "port") {
            let addr = p.children().find(|c| c.is_element() && c.tag_name().name() == "address").and_then(|a| a.attribute("location")).unwrap_or("").to_string();
            ports.push((p.attribute("name").unwrap_or("").to_string(), local(p.attribute("binding").unwrap_or("")).to_string(), addr));
        }
    }
    let bindings: Vec<_> = kids(defs, "binding").into_iter().filter(|b| b.children().any(|c| c.is_element() && matches!(c.tag_name().namespace(), Some(NS_SOAP11) | Some(NS_SOAP12)))).collect();
    if bindings.is_empty() {
        return Err(t!("el WSDL no tiene bindings SOAP").into());
    }
    let first_addr = ports.first().map(|p| p.2.clone()).unwrap_or_default();
    out.vars.push(Var { name: "base".into(), value: first_addr.clone(), secret: false });

    for b in &bindings {
        let soap12 = b.children().any(|c| c.is_element() && c.tag_name().namespace() == Some(NS_SOAP12));
        let style_default = b.children().find(|c| c.is_element() && c.tag_name().name() == "binding").and_then(|c| c.attribute("style")).unwrap_or("document");
        let bname = b.attribute("name").unwrap_or("");
        let pt = port_types.iter().find(|p| p.attribute("name") == b.attribute("type").map(local));
        // URL: la del puerto de este binding (variable propia si difiere de la primera).
        let addr = ports.iter().find(|p| p.1 == bname).map(|p| p.2.clone()).unwrap_or_default();
        let url = if addr == first_addr || addr.is_empty() { "{{base}}".to_string() } else { addr };
        let mut items = Vec::new();
        for op in kids(*b, "operation") {
            let name = op.attribute("name").unwrap_or("");
            let soap_op = op.children().find(|c| c.is_element() && c.tag_name().name() == "operation");
            let action = soap_op.and_then(|o| o.attribute("soapAction")).unwrap_or("").to_string();
            let style = soap_op.and_then(|o| o.attribute("style")).unwrap_or(style_default);
            // Mensaje de entrada → sus partes.
            let msg = pt.and_then(|pt| kids(*pt, "operation").into_iter().find(|o| o.attribute("name") == Some(name))).and_then(|o| kids(o, "input").into_iter().next()).and_then(|i| i.attribute("message")).map(local).and_then(|m| messages.iter().find(|x| x.attribute("name") == Some(m)));
            let parts = msg.map(|m| kids(*m, "part")).unwrap_or_default();
            let mut body = String::new();
            let mut ns = tns.clone();
            if style == "rpc" {
                body.push_str(&format!("    <tns:{name}>\n"));
                for p in &parts {
                    let pn = p.attribute("name").unwrap_or("parte");
                    match p.attribute("element").and_then(|e| schemas.element(e)) {
                        Some((_, _, e)) => schemas.element_sample(*e, 3, 0, &mut body),
                        None => body.push_str(&format!("      <{pn}>?</{pn}><!-- {} -->\n", p.attribute("type").map(local).unwrap_or("string"))),
                    }
                }
                body.push_str(&format!("    </tns:{name}>\n"));
                if let Some(n) = soap_op.and_then(|_| op.descendants().find(|d| d.tag_name().name() == "body").and_then(|d| d.attribute("namespace"))) {
                    ns = n.to_string();
                }
            } else {
                for p in &parts {
                    if let Some((_, ens, e)) = p.attribute("element").and_then(|e| schemas.element(e)) {
                        ns = ens.clone();
                        schemas.element_sample(*e, 2, 0, &mut body);
                    }
                }
            }
            let env_ns = if soap12 { "http://www.w3.org/2003/05/soap-envelope" } else { "http://schemas.xmlsoap.org/soap/envelope/" };
            let text = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<soap:Envelope xmlns:soap=\"{env_ns}\" xmlns:tns=\"{ns}\">\n  <soap:Header/>\n  <soap:Body>\n{body}  </soap:Body>\n</soap:Envelope>\n");
            items.push(Item::Req(Request {
                name: name.into(),
                method: "POST".into(),
                url: url.clone(),
                body: Body { kind: if soap12 { BodyKind::Soap12 } else { BodyKind::Soap11 }, text, soap_action: action, ..Default::default() },
                auth: Auth::Inherit,
                ..Default::default()
            }));
        }
        let label = if soap12 { format!("{bname} (SOAP 1.2)") } else { format!("{bname} (SOAP 1.1)") };
        out.items.push(Item::Folder { name: label, items });
    }
    // Un solo binding: sin carpeta intermedia.
    if out.items.len() == 1 {
        if let Some(Item::Folder { items, .. }) = out.items.pop() {
            out.items = items;
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ escribir en un espacio

/// Escribe lo importado en la carpeta `dir` de un espacio (que ya existe) y
/// añade al espacio las variables, entornos y autenticación que no tenga.
/// Devuelve cuántas peticiones se crearon.
pub fn write_into(space_dir: &Path, dir: &Path, imp: &Imported) -> std::io::Result<usize> {
    let mut sf = crate::store::load_space_file(space_dir)?;
    // Variables que ya existen con otro valor (p. ej. `base` de otra API): las
    // importadas se renombran a `nombre_carpeta` y se cambian en las peticiones.
    let suffix: String = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default().chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let suffix = if suffix.is_empty() || dir == space_dir { "importada".to_string() } else { suffix };
    let mut renames: Vec<(String, String)> = Vec::new();
    for v in &imp.vars {
        if sf.vars.iter().any(|x| x.name == v.name && !x.secret && x.value != v.value && !v.value.is_empty()) {
            let mut n = format!("{}_{suffix}", v.name);
            let mut k = 2;
            while sf.vars.iter().any(|x| x.name == n) {
                n = format!("{}_{suffix}{k}", v.name);
                k += 1;
            }
            renames.push((v.name.clone(), n));
        }
    }
    let mut imp = imp.clone();
    if !renames.is_empty() {
        fn fix(items: &mut [Item], renames: &[(String, String)]) {
            for it in items {
                match it {
                    Item::Folder { items, .. } => fix(items, renames),
                    Item::Req(r) => {
                        let Ok(mut text) = toml::to_string(r) else { continue };
                        for (a, b) in renames {
                            text = text.replace(&format!("{{{{{a}}}}}"), &format!("{{{{{b}}}}}"));
                        }
                        if let Ok(nr) = toml::from_str(&text) {
                            *r = nr;
                        }
                    }
                }
            }
        }
        fix(&mut imp.items, &renames);
        for v in &mut imp.vars {
            if let Some((_, b)) = renames.iter().find(|(a, _)| *a == v.name) {
                v.name = b.clone();
            }
        }
        for e in &mut imp.envs {
            for v in &mut e.vars {
                if let Some((_, b)) = renames.iter().find(|(a, _)| *a == v.name) {
                    v.name = b.clone();
                }
            }
        }
    }
    let imp = &imp;
    fn walk(dir: &Path, items: &[Item], n: &mut usize) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for it in items {
            match it {
                Item::Folder { name, items } => {
                    let mut d = dir.join(crate::store::file_name_for(name));
                    let mut k = 2;
                    while d.exists() && !d.is_dir() {
                        d = dir.join(format!("{} {k}", crate::store::file_name_for(name)));
                        k += 1;
                    }
                    walk(&d, items, n)?;
                }
                Item::Req(r) => {
                    let mut r = r.clone();
                    if r.name.trim().is_empty() {
                        r.name = default_name(&r.method, &r.url);
                    }
                    crate::store::save_request(&crate::store::free_path(dir, &r.name), &r)?;
                    *n += 1;
                }
            }
        }
        Ok(())
    }
    let mut n = 0;
    walk(dir, &imp.items, &mut n)?;
    let mut sec = crate::store::load_secrets(&sf.id);
    let mut changed = false;
    for v in &imp.vars {
        if !sf.vars.iter().any(|x| x.name == v.name) {
            let mut v = v.clone();
            if v.secret && !v.value.is_empty() {
                sec.variables.insert(v.name.clone(), std::mem::take(&mut v.value));
            }
            sf.vars.push(v);
            changed = true;
        }
    }
    for e in &imp.envs {
        let mut e = e.clone();
        for v in &mut e.vars {
            if v.secret && !v.value.is_empty() {
                sec.entornos.entry(e.name.clone()).or_default().insert(v.name.clone(), std::mem::take(&mut v.value));
            }
        }
        match sf.envs.iter_mut().find(|x| x.name == e.name) {
            Some(x) => x.vars = e.vars,
            None => sf.envs.push(e),
        }
        changed = true;
    }
    if let Some(a) = &imp.auth {
        if matches!(sf.auth, Auth::None | Auth::Inherit) {
            sf.auth = a.clone();
            changed = true;
        }
    }
    if changed {
        crate::store::save_space_file(space_dir, &sf)?;
        crate::store::save_secrets(&sf.id, &sec)?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_from_browser_devtools() {
        let c = r#"curl 'https://api.test/v1/users?x=1' \
  -H 'accept: application/json' \
  -H "Authorization: Bearer abc" \
  --data-raw '{"nombre":"Ana"}' \
  --compressed"#;
        let r = parse_curl(c).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.url, "https://api.test/v1/users?x=1");
        assert_eq!(r.headers.len(), 2);
        assert_eq!(r.headers[1].value, "Bearer abc");
        assert_eq!(r.body.kind, BodyKind::Json);
        assert_eq!(r.body.text, r#"{"nombre":"Ana"}"#);
        assert_eq!(r.name, "POST /v1/users");

        let r = parse_curl("curl -XPUT -u ana:x --digest https://h/a -d a=1 -d b=2").unwrap();
        assert_eq!(r.method, "PUT");
        assert_eq!(r.auth, Auth::Digest { user: "ana".into(), password: "x".into() });
        assert_eq!(r.body.text, "a=1&b=2");
        assert!(r.headers.iter().any(|h| h.value == "application/x-www-form-urlencoded"));

        let r = parse_curl("curl -G https://h/s --data-urlencode 'q=a b'").unwrap();
        assert_eq!((r.method.as_str(), r.url.as_str()), ("GET", "https://h/s?q=a%20b"));

        let r = parse_curl("curl https://h/ws -H 'Content-Type: text/xml' -H 'SOAPAction: \"urn:X\"' -d '<e/>'").unwrap();
        assert_eq!(r.body.kind, BodyKind::Soap11);
        assert_eq!(r.body.soap_action, "urn:X");
        assert!(parse_curl("curl -H x").is_err());
    }

    #[test]
    fn postman_collection() {
        let j = r#"{
          "info": {"name": "Tienda", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
          "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{tok}}", "type": "string"}]},
          "variable": [{"key": "base", "value": "https://t.test"}],
          "item": [
            {"name": "Productos", "item": [
              {"name": "Listar", "request": {"method": "GET", "header": [], "url": {"raw": "{{base}}/p?limit=10&off", "query": [{"key": "limit", "value": "10"}, {"key": "off", "value": null, "disabled": true}]}}},
              {"name": "Crear", "request": {"method": "POST", "header": [{"key": "Content-Type", "value": "application/json"}],
                 "body": {"mode": "raw", "raw": "{\"a\":1}", "options": {"raw": {"language": "json"}}},
                 "auth": {"type": "basic", "basic": [{"key": "username", "value": "u"}, {"key": "password", "value": "p"}]},
                 "url": "{{base}}/p"}}
            ]},
            {"name": "Form", "request": {"method": "POST", "url": "{{base}}/f", "body": {"mode": "urlencoded", "urlencoded": [{"key": "x", "value": "1"}]}}}
          ]
        }"#;
        let imp = parse(j).unwrap();
        assert_eq!(imp.format, "Postman");
        assert_eq!(imp.name, "Tienda");
        assert_eq!(imp.count(), 3);
        assert_eq!(imp.auth, Some(Auth::Bearer { token: "{{tok}}".into() }));
        let Item::Folder { items, .. } = &imp.items[0] else { panic!() };
        let Item::Req(l) = &items[0] else { panic!() };
        assert_eq!(l.url, "{{base}}/p");
        assert_eq!(l.params.len(), 2);
        assert!(!l.params[1].enabled);
        assert_eq!(l.auth, Auth::Inherit);
        let Item::Req(c) = &items[1] else { panic!() };
        assert_eq!(c.body.kind, BodyKind::Json);
        assert_eq!(c.auth, Auth::Basic { user: "u".into(), password: "p".into() });
        let Item::Req(f) = &imp.items[1] else { panic!() };
        assert_eq!(f.body.kind, BodyKind::Form);

        // Ida y vuelta: exportar un espacio con esto y volver a importarlo.
        let d = std::env::temp_dir().join(format!("peticiones-test-pm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        crate::store::save_space_file(&d, &SpaceFile { id: crate::store::new_id(), name: "Tienda".into(), ..Default::default() }).unwrap();
        assert_eq!(write_into(&d, &d, &imp).unwrap(), 3);
        let sf = crate::store::load_space_file(&d).unwrap();
        assert_eq!(sf.auth, Auth::Bearer { token: "{{tok}}".into() });
        let again = parse(&export_postman(&sf, &d)).unwrap();
        assert_eq!(again.count(), 3);
        // Al leer la carpeta: carpetas primero y por nombre (Crear, Listar).
        let Item::Folder { items, .. } = &again.items[0] else { panic!("{:?}", again.items) };
        let Item::Req(l2) = &items[1] else { panic!() };
        assert_eq!(l2.url, "{{base}}/p");
        assert_eq!(l2.params, l.params);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn postman_environment() {
        let imp = parse(r#"{"name": "prod", "values": [{"key": "base", "value": "https://p", "enabled": true}, {"key": "clave", "value": "x", "type": "secret", "enabled": true}]}"#).unwrap();
        assert_eq!(imp.envs[0].name, "prod");
        assert!(imp.envs[0].vars[1].secret);
    }

    #[test]
    fn openapi_yaml() {
        let y = r#"
openapi: 3.0.3
info: {title: Mascotas, version: "1"}
servers:
  - url: https://{entorno}.pets.test/v1
    variables: {entorno: {default: api}}
security: [{token: []}]
components:
  securitySchemes:
    token: {type: http, scheme: bearer}
  schemas:
    Pet:
      type: object
      required: [name]
      properties:
        id: {type: integer, readOnly: true}
        name: {type: string, example: Toby}
        tags: {type: array, items: {type: string}}
        born: {type: string, format: date}
paths:
  /pets/{petId}:
    parameters:
      - {name: petId, in: path, required: true, schema: {type: integer, example: 7}}
    get:
      tags: [pets]
      operationId: getPet
      parameters:
        - {name: fields, in: query, schema: {type: string}}
        - {name: X-Trace, in: header, schema: {type: string, default: abc}}
    put:
      tags: [pets]
      summary: Actualizar
      requestBody:
        content:
          application/json:
            schema: {$ref: '#/components/schemas/Pet'}
  /health:
    get:
      security: []
"#;
        let imp = parse(y).unwrap();
        assert_eq!(imp.format, "OpenAPI");
        assert_eq!(imp.vars[0].value, "https://api.pets.test/v1");
        assert!(imp.vars.iter().any(|v| v.name == "petId" && v.value == "7"));
        assert!(imp.vars.iter().any(|v| v.name == "token" && v.secret));
        assert_eq!(imp.auth, Some(Auth::Bearer { token: "{{token}}".into() }));
        let Item::Folder { name, items } = &imp.items[0] else { panic!() };
        assert_eq!(name, "pets");
        let Item::Req(g) = &items[0] else { panic!() };
        assert_eq!((g.name.as_str(), g.url.as_str()), ("getPet", "{{base}}/pets/{{petId}}"));
        assert!(!g.params[0].enabled);
        assert_eq!(g.headers[0].value, "abc");
        let Item::Req(p) = &items[1] else { panic!() };
        let body: Value = serde_json::from_str(&p.body.text).unwrap();
        assert_eq!(body, json!({"name": "Toby", "tags": ["texto"], "born": "2026-01-31"}));
        let Item::Req(h) = &imp.items[1] else { panic!() };
        assert_eq!(h.auth, Auth::None);
    }

    const WSDL: &str = r#"<?xml version="1.0"?>
<definitions name="Calculadora" targetNamespace="http://tempuri.org/"
  xmlns="http://schemas.xmlsoap.org/wsdl/" xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/"
  xmlns:soap12="http://schemas.xmlsoap.org/wsdl/soap12/" xmlns:tns="http://tempuri.org/" xmlns:s="http://www.w3.org/2001/XMLSchema">
  <types>
    <s:schema elementFormDefault="qualified" targetNamespace="http://tempuri.org/">
      <s:element name="Add">
        <s:complexType><s:sequence>
          <s:element minOccurs="1" maxOccurs="1" name="intA" type="s:int"/>
          <s:element minOccurs="0" name="opts" type="tns:Opts"/>
        </s:sequence></s:complexType>
      </s:element>
      <s:complexType name="Opts"><s:sequence><s:element name="round" type="s:boolean"/></s:sequence></s:complexType>
      <s:element name="AddResponse"><s:complexType><s:sequence><s:element name="AddResult" type="s:int"/></s:sequence></s:complexType></s:element>
    </s:schema>
  </types>
  <message name="AddSoapIn"><part name="parameters" element="tns:Add"/></message>
  <message name="AddSoapOut"><part name="parameters" element="tns:AddResponse"/></message>
  <portType name="CalcSoap"><operation name="Add"><input message="tns:AddSoapIn"/><output message="tns:AddSoapOut"/></operation></portType>
  <binding name="CalcSoap" type="tns:CalcSoap">
    <soap:binding transport="http://schemas.xmlsoap.org/soap/http"/>
    <operation name="Add"><soap:operation soapAction="http://tempuri.org/Add" style="document"/><input><soap:body use="literal"/></input></operation>
  </binding>
  <binding name="CalcSoap12" type="tns:CalcSoap">
    <soap12:binding transport="http://schemas.xmlsoap.org/soap/http"/>
    <operation name="Add"><soap12:operation soapAction="http://tempuri.org/Add" style="document"/><input><soap12:body use="literal"/></input></operation>
  </binding>
  <service name="Calc">
    <port name="CalcSoap" binding="tns:CalcSoap"><soap:address location="http://calc.test/calc.asmx"/></port>
    <port name="CalcSoap12" binding="tns:CalcSoap12"><soap12:address location="http://calc.test/calc.asmx"/></port>
  </service>
</definitions>"#;

    #[test]
    fn wsdl_document_literal() {
        let imp = parse(WSDL).unwrap();
        assert_eq!(imp.format, "WSDL");
        assert_eq!(imp.vars[0].value, "http://calc.test/calc.asmx");
        assert_eq!(imp.items.len(), 2);
        let Item::Folder { name, items } = &imp.items[0] else { panic!() };
        assert_eq!(name, "CalcSoap (SOAP 1.1)");
        let Item::Req(r) = &items[0] else { panic!() };
        assert_eq!(r.body.kind, BodyKind::Soap11);
        assert_eq!(r.body.soap_action, "http://tempuri.org/Add");
        assert_eq!(r.url, "{{base}}");
        assert!(r.body.text.contains("xmlns:tns=\"http://tempuri.org/\""), "{}", r.body.text);
        assert!(r.body.text.contains("<tns:Add>\n      <tns:intA>?</tns:intA><!-- int -->\n      <tns:opts><!-- opcional -->\n        <tns:round>?</tns:round><!-- boolean -->\n      </tns:opts>\n    </tns:Add>"), "{}", r.body.text);
        // El sobre generado es XML válido.
        assert!(crate::beautify::format_xml(&r.body.text, "  ", false).is_ok());
        let Item::Folder { items, .. } = &imp.items[1] else { panic!() };
        let Item::Req(r12) = &items[0] else { panic!() };
        // En un espacio que ya tiene otra `base`, la del WSDL se renombra.
        let d = std::env::temp_dir().join(format!("peticiones-test-wsdl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        crate::store::save_space_file(&d, &SpaceFile { id: crate::store::new_id(), vars: vec![Var { name: "base".into(), value: "https://otra".into(), secret: false }], ..Default::default() }).unwrap();
        assert_eq!(write_into(&d, &d.join("Calc"), &imp).unwrap(), 2);
        let sf = crate::store::load_space_file(&d).unwrap();
        assert_eq!(sf.vars[1], Var { name: "base_calc".into(), value: "http://calc.test/calc.asmx".into(), secret: false });
        let r = crate::store::load_request(&d.join("Calc/CalcSoap (SOAP 1.1)/Add.toml")).unwrap();
        assert_eq!(r.url, "{{base_calc}}");
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(r12.body.kind, BodyKind::Soap12);
        assert!(r12.body.text.contains("http://www.w3.org/2003/05/soap-envelope"));
    }

    fn fetch(url: &str) -> String {
        let p = crate::http::Prepared { method: "GET".into(), url: url.into(), ..Default::default() };
        String::from_utf8(crate::http::send_raw(&p, &Default::default()).unwrap().body).unwrap()
    }

    /// Red: `cargo test --release live -- --ignored`. WSDL real → enviar Add.
    #[test]
    #[ignore]
    fn live_wsdl_and_openapi() {
        let imp = parse(&fetch("http://www.dneonline.com/calculator.asmx?WSDL")).unwrap();
        assert_eq!(imp.count(), 8, "{imp:?}");
        let Item::Folder { items, .. } = &imp.items[0] else { panic!() };
        let Item::Req(add) = items.iter().find(|i| matches!(i, Item::Req(r) if r.name == "Add")).unwrap() else { panic!() };
        let mut add = add.clone();
        add.body.text = add.body.text.replacen(">?<", ">2<", 1).replacen(">?<", ">40<", 1);
        let space = SpaceFile { vars: imp.vars.clone(), ..Default::default() };
        let scope = crate::vars::Scope::new(&space, None, &Default::default());
        let out = crate::http::send(crate::http::prepare(&add, &space, &scope), &Default::default(), None);
        let r = out.response.expect("sin respuesta");
        let body = String::from_utf8_lossy(&r.body);
        assert!(body.contains("<AddResult>42</AddResult>"), "{} {body}", r.status);

        let api = parse(&fetch("https://petstore3.swagger.io/api/v3/openapi.json")).unwrap();
        assert!(api.count() >= 15, "{}", api.count());
        assert!(matches!(api.auth, Some(Auth::OAuth2(_)) | Some(Auth::ApiKey { .. })), "{:?}", api.auth);
    }
}
