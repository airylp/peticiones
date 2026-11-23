//! Preparar y enviar una petición.
//!
//! `prepare` resuelve variables, parámetros, cuerpo y la autenticación simple
//! (básica, bearer, API key); `send` la manda con ureq en el hilo que la llame
//! (la app la lanza en un hilo propio) y resuelve Digest y OAuth 2.0.

use crate::auth;
use crate::model::{Auth, BodyKind, Pair, Request, SpaceFile};
use crate::vars::Scope;
use std::time::{Duration, Instant};

/// Petición lista para enviarse: todo resuelto, sin variables.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prepared {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Autenticación que necesita red (Digest, OAuth) ya con variables resueltas.
    pub auth: Auth,
    /// Variables que no existen (se dejan como `{{x}}`).
    pub missing: Vec<String>,
}

impl Prepared {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn set_header(&mut self, name: &str, value: String) {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        self.headers.push((name.into(), value));
    }
}

/// Opciones de envío.
#[derive(Clone, Debug)]
pub struct Options {
    pub timeout: Duration,
    pub follow_redirects: bool,
    pub verify_tls: bool,
    /// Máximo de bytes de respuesta que se leen.
    pub max_body: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options { timeout: Duration::from_secs(30), follow_redirects: true, verify_tls: true, max_body: 256 << 20 }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub elapsed: Duration,
    /// URL final (tras redirecciones).
    pub url: String,
    pub http_version: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Codifica para query o formulario (RFC 3986: sin reservar = A-Z a-z 0-9 -._~).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Arregla lo que el usuario escribe a mano en la URL: espacios y caracteres
/// no ASCII se codifican (lo demás se respeta, incluido lo ya codificado).
/// Sin esquema se asume `http://`.
pub fn fix_url(s: &str) -> String {
    let s = s.trim();
    let mut out = String::with_capacity(s.len() + 8);
    if !s.contains("://") {
        out.push_str("http://");
    }
    for c in s.chars() {
        if c.is_ascii() && !c.is_ascii_control() && !" \"<>\\^`{|}".contains(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

pub fn add_query(url: &mut String, pairs: &[(String, String)]) {
    if pairs.is_empty() {
        return;
    }
    let frag = url.find('#').map(|i| url.split_off(i));
    let mut sep = if url.contains('?') { if url.ends_with('?') || url.ends_with('&') { "" } else { "&" } } else { "?" };
    for (k, v) in pairs {
        url.push_str(sep);
        url.push_str(&encode(k));
        if !v.is_empty() {
            url.push('=');
            url.push_str(&encode(v));
        }
        sep = "&";
    }
    if let Some(f) = frag {
        url.push_str(&f);
    }
}

fn active(pairs: &[Pair], scope: &Scope, missing: &mut Vec<String>) -> Vec<(String, String)> {
    pairs.iter().filter(|p| p.enabled && !p.name.trim().is_empty()).map(|p| (scope.apply(p.name.trim(), missing), scope.apply(&p.value, missing))).collect()
}

fn resolve_auth(a: &Auth, scope: &Scope, missing: &mut Vec<String>) -> Auth {
    let mut ap = |s: &String| scope.apply(s, missing);
    match a {
        Auth::Inherit | Auth::None => Auth::None,
        Auth::Basic { user, password } => Auth::Basic { user: ap(user), password: ap(password) },
        Auth::Bearer { token } => Auth::Bearer { token: ap(token) },
        Auth::ApiKey { name, value, place } => Auth::ApiKey { name: ap(name), value: ap(value), place: *place },
        Auth::Digest { user, password } => Auth::Digest { user: ap(user), password: ap(password) },
        Auth::OAuth2(o) => {
            let mut o = o.clone();
            for f in [&mut o.token_url, &mut o.auth_url, &mut o.client_id, &mut o.client_secret, &mut o.scope, &mut o.audience] {
                *f = scope.apply(f, missing);
            }
            Auth::OAuth2(o)
        }
    }
}

/// Resuelve todo lo que no necesita red.
pub fn prepare(req: &Request, space: &SpaceFile, scope: &Scope) -> Prepared {
    let mut missing = Vec::new();
    let mut url = fix_url(&scope.apply(&req.url, &mut missing));
    let params = active(&req.params, scope, &mut missing);
    add_query(&mut url, &params);

    let mut p = Prepared { method: req.method.trim().to_ascii_uppercase(), url, ..Default::default() };
    if p.method.is_empty() {
        p.method = "GET".into();
    }
    // Cabeceras del espacio primero; las de la petición mandan.
    for (k, v) in active(&space.headers, scope, &mut missing).into_iter().chain(active(&req.headers, scope, &mut missing)) {
        p.set_header(&k, v);
    }

    let b = &req.body;
    let soap_action = scope.apply(&b.soap_action, &mut missing);
    match b.kind {
        BodyKind::None => {}
        BodyKind::Form => {
            let f = active(&b.fields, scope, &mut missing);
            p.body = f.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&").into_bytes();
        }
        _ => p.body = scope.apply(&b.text, &mut missing).into_bytes(),
    }
    if p.header("content-type").is_none() {
        if let Some(ct) = b.kind.content_type(&soap_action) {
            p.headers.push(("Content-Type".into(), ct));
        }
    }
    if b.kind == BodyKind::Soap11 && p.header("soapaction").is_none() {
        p.headers.push(("SOAPAction".into(), format!("\"{soap_action}\"")));
    }

    let a = if req.auth == Auth::Inherit { &space.auth } else { &req.auth };
    let a = resolve_auth(a, scope, &mut missing);
    match &a {
        Auth::Basic { user, password } => p.set_header("Authorization", auth::basic(user, password)),
        Auth::Bearer { token } => p.set_header("Authorization", format!("Bearer {}", token.trim())),
        Auth::ApiKey { name, value, place } => match place {
            crate::model::KeyPlace::Header => p.set_header(name.trim(), value.clone()),
            crate::model::KeyPlace::Query => add_query(&mut p.url, &[(name.trim().to_string(), value.clone())]),
        },
        _ => {}
    }
    p.auth = a;
    p.missing = missing;
    p
}

fn agent(o: &Options) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().disable_verification(!o.verify_tls).build();
    let cfg = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(o.timeout))
        .max_redirects(if o.follow_redirects { 10 } else { 0 })
        .max_redirects_will_error(false)
        .save_redirect_history(true)
        .allow_non_standard_methods(true)
        .user_agent(concat!("peticiones/", env!("CARGO_PKG_VERSION")))
        .tls_config(tls)
        .build();
    ureq::Agent::new_with_config(cfg)
}

/// Envía tal cual (sin Digest ni OAuth): una sola ida y vuelta.
pub fn send_raw(p: &Prepared, o: &Options) -> Result<Response, String> {
    let agent = agent(o);
    let start = Instant::now();
    macro_rules! with_headers {
        ($b:expr) => {{
            let mut b = $b;
            for (k, v) in &p.headers {
                b = b.header_raw(k, v.as_bytes());
            }
            b
        }};
    }
    let url = p.url.as_str();
    let body = p.body.as_slice();
    let res = match p.method.as_str() {
        "GET" | "DELETE" | "HEAD" | "OPTIONS" | "TRACE" => {
            let b = match p.method.as_str() {
                "GET" => agent.get(url),
                "DELETE" => agent.delete(url),
                "HEAD" => agent.head(url),
                "OPTIONS" => agent.options(url),
                _ => agent.trace(url),
            };
            let b = with_headers!(b);
            if body.is_empty() { b.call() } else { b.force_send_body().send(body) }
        }
        "POST" => with_headers!(agent.post(url)).send(body),
        "PUT" => with_headers!(agent.put(url)).send(body),
        "PATCH" => with_headers!(agent.patch(url)).send(body),
        m => {
            let mut b = ureq::http::Request::builder().method(m).uri(url);
            for (k, v) in &p.headers {
                b = b.header(k.as_str(), v.as_str());
            }
            let req = b.body(body.to_vec()).map_err(|e| e.to_string())?;
            agent.run(req)
        }
    };
    let mut r = res.map_err(describe)?;
    let status = r.status();
    let headers = r.headers().iter().map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
    let http_version = format!("{:?}", r.version());
    use ureq::ResponseExt;
    let final_url = r.get_uri().to_string();
    let body = if p.method == "HEAD" { vec![] } else { r.body_mut().with_config().limit(o.max_body).read_to_vec().map_err(describe)? };
    Ok(Response { status: status.as_u16(), reason: status.canonical_reason().unwrap_or("").into(), headers, body, elapsed: start.elapsed(), url: final_url, http_version })
}

fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::HostNotFound => t!("No se encontró el servidor (DNS)").into(),
        ureq::Error::ConnectionFailed => t!("No se pudo conectar").into(),
        ureq::Error::Timeout(_) => t!("Se agotó el tiempo de espera").into(),
        ureq::Error::BodyExceedsLimit(n) => tf!("La respuesta pasa del límite ({n} bytes)", n = n),
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => t!("Conexión rechazada").into(),
        e => e.to_string(),
    }
}

/// Resultado de un envío completo.
#[derive(Debug, Default)]
pub struct Outcome {
    pub response: Option<Response>,
    pub error: Option<String>,
    /// Lo que realmente se envió (con la autenticación final).
    pub sent: Prepared,
    /// Token OAuth nuevo para guardar (clave, token).
    pub new_token: Option<(String, crate::store::Token)>,
    /// Mensajes informativos (p. ej. «token renovado»).
    pub notes: Vec<String>,
}

/// Envía resolviendo Digest (reintento tras el 401) y OAuth 2.0 (token de la
/// caché o uno nuevo).
pub fn send(mut p: Prepared, o: &Options, cached: Option<crate::store::Token>) -> Outcome {
    let mut out = Outcome::default();
    match p.auth.clone() {
        Auth::OAuth2(cfg) => match auth::oauth_token(&cfg, cached, o) {
            Ok((tok, fresh)) => {
                let kind = if tok.token_type.is_empty() || tok.token_type.eq_ignore_ascii_case("bearer") { "Bearer".to_string() } else { tok.token_type.clone() };
                p.set_header("Authorization", format!("{kind} {}", tok.access_token));
                if fresh {
                    out.notes.push(t!("Token OAuth nuevo").into());
                    out.new_token = Some((auth::token_key(&cfg), tok));
                }
            }
            Err(e) => {
                out.error = Some(tf!("OAuth: {e}", e = e));
                out.sent = p;
                return out;
            }
        },
        Auth::Digest { user, password } => {
            // Primer intento sin credenciales para recibir el reto.
            match send_raw(&p, o) {
                Ok(r) if r.status == 401 => {
                    if let Some(ch) = r.header("www-authenticate").filter(|h| h.trim_start().to_ascii_lowercase().starts_with("digest")) {
                        match auth::digest_header(ch, &user, &password, &p.method, &p.url, &p.body) {
                            Some(h) => p.set_header("Authorization", h),
                            None => out.notes.push(t!("Reto Digest no compatible").into()),
                        }
                    } else {
                        out.response = Some(r);
                        out.sent = p;
                        return out;
                    }
                }
                Ok(r) => {
                    out.response = Some(r);
                    out.sent = p;
                    return out;
                }
                Err(e) => {
                    out.error = Some(e);
                    out.sent = p;
                    return out;
                }
            }
        }
        _ => {}
    }
    match send_raw(&p, o) {
        Ok(r) => out.response = Some(r),
        Err(e) => out.error = Some(e),
    }
    out.sent = p;
    out
}

/// Comando curl equivalente (para copiar).
pub fn to_curl(p: &Prepared) -> String {
    fn q(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
    let mut s = String::from("curl");
    if p.method != "GET" || !p.body.is_empty() && p.method != "POST" {
        s.push_str(&format!(" -X {}", p.method));
    }
    s.push(' ');
    s.push_str(&q(&p.url));
    for (k, v) in &p.headers {
        s.push_str(&format!(" \\\n  -H {}", q(&format!("{k}: {v}"))));
    }
    if let Auth::Digest { user, password } = &p.auth {
        s.push_str(&format!(" \\\n  --digest -u {}", q(&format!("{user}:{password}"))));
    }
    if !p.body.is_empty() {
        s.push_str(&format!(" \\\n  --data-raw {}", q(&String::from_utf8_lossy(&p.body))));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Body, KeyPlace, Var};

    fn space() -> SpaceFile {
        SpaceFile { vars: vec![Var { name: "base".into(), value: "https://api.test".into(), secret: false }], headers: vec![Pair::new("X-App", "1")], auth: Auth::Bearer { token: "{{tok}}".into() }, ..Default::default() }
    }

    #[test]
    fn prepare_resolves_everything() {
        let sp = space();
        let mut scope = Scope::new(&sp, None, &Default::default());
        scope.values.insert("tok".into(), "abc".into());
        let mut r = Request { method: "post".into(), url: "{{base}}/buscar?x=1#frag".into(), params: vec![Pair::new("q", "a b&c"), Pair { enabled: false, ..Pair::new("no", "") }], ..Default::default() };
        r.body = Body { kind: BodyKind::Form, fields: vec![Pair::new("ñ", "1 2")], ..Default::default() };
        let p = prepare(&r, &sp, &scope);
        assert_eq!(p.method, "POST");
        assert_eq!(p.url, "https://api.test/buscar?x=1&q=a%20b%26c#frag");
        assert_eq!(p.header("authorization"), Some("Bearer abc"));
        assert_eq!(p.header("x-app"), Some("1"));
        assert_eq!(p.header("content-type"), Some("application/x-www-form-urlencoded"));
        assert_eq!(p.body, b"%C3%B1=1%202");
        assert!(p.missing.is_empty());
    }

    #[test]
    fn soap_and_api_key() {
        let sp = SpaceFile::default();
        let scope = Scope::default();
        let r = Request {
            url: "localhost:8080/ws".into(),
            method: "POST".into(),
            body: Body { kind: BodyKind::Soap11, text: "<x/>".into(), soap_action: "urn:Get".into(), ..Default::default() },
            auth: Auth::ApiKey { name: "key".into(), value: "v".into(), place: KeyPlace::Query },
            ..Default::default()
        };
        let p = prepare(&r, &sp, &scope);
        assert_eq!(p.url, "http://localhost:8080/ws?key=v");
        assert_eq!(p.header("SOAPAction"), Some("\"urn:Get\""));
        assert_eq!(p.header("content-type"), Some("text/xml; charset=utf-8"));
        let mut r12 = r.clone();
        r12.body.kind = BodyKind::Soap12;
        let p = prepare(&r12, &sp, &scope);
        assert_eq!(p.header("content-type"), Some("application/soap+xml; charset=utf-8; action=\"urn:Get\""));
        assert!(p.header("soapaction").is_none());
        assert!(to_curl(&p).contains("--data-raw '<x/>'"));
    }

    #[test]
    fn url_fixes() {
        assert_eq!(fix_url(" https://a.b/c d?ñ=1 "), "https://a.b/c%20d?%C3%B1=1");
    }

    /// Contra httpbin.org (red): `cargo test --release live -- --ignored`.
    #[test]
    #[ignore]
    fn live_httpbin() {
        let sp = SpaceFile::default();
        let scope = Scope::default();
        let o = Options::default();
        for (algo, auth) in [("MD5", "auth"), ("SHA-256", "auth")] {
            let r = Request { url: format!("https://httpbin.org/digest-auth/{auth}/ana/s3cr3t/{algo}"), auth: Auth::Digest { user: "ana".into(), password: "s3cr3t".into() }, ..Default::default() };
            let out = send(prepare(&r, &sp, &scope), &o, None);
            assert_eq!(out.response.as_ref().map(|r| r.status), Some(200), "{algo}: {:?}", out.error);
        }
        let r = Request { url: "https://httpbin.org/basic-auth/ana/x".into(), auth: Auth::Basic { user: "ana".into(), password: "x".into() }, ..Default::default() };
        assert_eq!(send(prepare(&r, &sp, &scope), &o, None).response.unwrap().status, 200);
        let r = Request {
            method: "POST".into(),
            url: "https://httpbin.org/anything".into(),
            body: Body { kind: BodyKind::Soap11, text: "<soap:Envelope/>".into(), soap_action: "urn:Hora".into(), ..Default::default() },
            ..Default::default()
        };
        let res = send(prepare(&r, &sp, &scope), &o, None).response.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&res.body).unwrap();
        assert_eq!(v["headers"]["Soapaction"], "\"urn:Hora\"");
        assert_eq!(v["data"], "<soap:Envelope/>");
        // GET con cuerpo (force_send_body) y método no estándar.
        let r = Request { method: "PROPFIND".into(), url: "https://httpbin.org/anything".into(), body: Body { kind: BodyKind::Text, text: "hola".into(), ..Default::default() }, ..Default::default() };
        let res = send(prepare(&r, &sp, &scope), &o, None);
        assert!(res.response.is_some(), "{:?}", res.error);
    }
}
