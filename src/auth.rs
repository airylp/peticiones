//! Autenticación: Basic, Digest (RFC 7616) y OAuth 2.0 (client credentials y
//! authorization code con PKCE, más refresh token).

use crate::http::{self, Options, Prepared};
use crate::model::{OAuth2, OAuthFlow};
use crate::store::Token;
use base64::Engine;
use md5::Md5;
use sha2::{Digest as _, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("sin generador aleatorio");
    hex(&b)
}

/// Parámetros `clave=valor` / `clave="valor"` de una cabecera de reto.
fn challenge_params(h: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let s = h.trim();
    let s = s.split_once(char::is_whitespace).map_or("", |(_, r)| r);
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        let k0 = i;
        while i < b.len() && b[i] != b'=' && b[i] != b',' {
            i += 1;
        }
        let key = s[k0..i].trim().to_ascii_lowercase();
        if i >= b.len() || b[i] != b'=' {
            continue;
        }
        i += 1;
        let val = if i < b.len() && b[i] == b'"' {
            i += 1;
            let mut v = String::new();
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                }
                let c = s[i..].chars().next().unwrap();
                v.push(c);
                i += c.len_utf8();
            }
            i += 1;
            v
        } else {
            let v0 = i;
            while i < b.len() && b[i] != b',' {
                i += 1;
            }
            s[v0..i].trim().to_string()
        };
        out.push((key, val));
    }
    out
}

/// Cabecera `Authorization: Digest …` para el reto `www_auth`.
/// Admite MD5, SHA-256 (y sus variantes -sess) con qop=auth o sin qop.
pub fn digest_header(www_auth: &str, user: &str, password: &str, method: &str, url: &str, body: &[u8]) -> Option<String> {
    digest_with_cnonce(www_auth, user, password, method, url, body, &random_hex(8))
}

fn digest_with_cnonce(www_auth: &str, user: &str, password: &str, method: &str, url: &str, body: &[u8], cnonce: &str) -> Option<String> {
    let p = challenge_params(www_auth);
    let get = |k: &str| p.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str());
    let realm = get("realm").unwrap_or("");
    let nonce = get("nonce")?;
    let algo = get("algorithm").unwrap_or("MD5");
    let (sha256, sess) = match algo.to_ascii_uppercase().as_str() {
        "MD5" => (false, false),
        "MD5-SESS" => (false, true),
        "SHA-256" => (true, false),
        "SHA-256-SESS" => (true, true),
        _ => return None,
    };
    let h = |s: &[u8]| if sha256 { hex(&Sha256::digest(s)) } else { hex(&Md5::digest(s)) };
    let qops: Vec<&str> = get("qop").map(|q| q.split(',').map(str::trim).collect()).unwrap_or_default();
    let qop = if qops.contains(&"auth") {
        Some("auth")
    } else if qops.contains(&"auth-int") {
        Some("auth-int")
    } else if qops.is_empty() {
        None
    } else {
        return None;
    };
    // URI = ruta + query de la URL.
    let after = url.split_once("://").map_or(url, |(_, r)| r);
    let uri = after.find('/').map_or("/", |i| &after[i..]);
    let uri = uri.split('#').next().unwrap_or(uri);
    let mut ha1 = h(format!("{user}:{realm}:{password}").as_bytes());
    if sess {
        ha1 = h(format!("{ha1}:{nonce}:{cnonce}").as_bytes());
    }
    let ha2 = if qop == Some("auth-int") { h(format!("{method}:{uri}:{}", h(body)).as_bytes()) } else { h(format!("{method}:{uri}").as_bytes()) };
    let nc = "00000001";
    let response = match qop {
        Some(q) => h(format!("{ha1}:{nonce}:{nc}:{cnonce}:{q}:{ha2}").as_bytes()),
        None => h(format!("{ha1}:{nonce}:{ha2}").as_bytes()),
    };
    let mut out = format!("Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", algorithm={algo}, response=\"{response}\"");
    if let Some(q) = qop {
        out.push_str(&format!(", qop={q}, nc={nc}, cnonce=\"{cnonce}\""));
    }
    if let Some(o) = get("opaque") {
        out.push_str(&format!(", opaque=\"{o}\""));
    }
    Some(out)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Clave con la que se guarda el token en los secretos.
pub fn token_key(c: &OAuth2) -> String {
    let flow = match c.flow {
        OAuthFlow::ClientCredentials => "cc",
        OAuthFlow::AuthorizationCode => "ac",
    };
    hex(&Sha256::digest(format!("{flow}\n{}\n{}\n{}\n{}", c.token_url, c.client_id, c.scope, c.audience).as_bytes())[..12])
}

/// Token válido: el de la caché si no caduca en 30 s, renovado con el refresh
/// token, o uno nuevo. Devuelve (token, es_nuevo).
pub fn oauth_token(c: &OAuth2, cached: Option<Token>, o: &Options) -> Result<(Token, bool), String> {
    if let Some(t) = &cached {
        if !t.access_token.is_empty() && (t.expires_at == 0 || t.expires_at > now() + 30) {
            return Ok((t.clone(), false));
        }
        if !t.refresh_token.is_empty() {
            let mut f = vec![("grant_type", "refresh_token".to_string()), ("refresh_token", t.refresh_token.clone())];
            if !c.scope.is_empty() {
                f.push(("scope", c.scope.clone()));
            }
            if let Ok(mut nt) = token_request(c, f, o) {
                if nt.refresh_token.is_empty() {
                    nt.refresh_token = t.refresh_token.clone();
                }
                return Ok((nt, true));
            }
        }
    }
    let t = match c.flow {
        OAuthFlow::ClientCredentials => {
            let mut f = vec![("grant_type", "client_credentials".to_string())];
            if !c.scope.is_empty() {
                f.push(("scope", c.scope.clone()));
            }
            if !c.audience.is_empty() {
                f.push(("audience", c.audience.clone()));
            }
            token_request(c, f, o)?
        }
        OAuthFlow::AuthorizationCode => authorization_code(c, o)?,
    };
    Ok((t, true))
}

/// POST al endpoint de tokens con el formulario dado.
fn token_request(c: &OAuth2, mut fields: Vec<(&str, String)>, o: &Options) -> Result<Token, String> {
    if c.token_url.trim().is_empty() {
        return Err(t!("falta la URL del token").into());
    }
    let mut p = Prepared { method: "POST".into(), url: http::fix_url(&c.token_url), ..Default::default() };
    p.headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
    p.headers.push(("Accept".into(), "application/json".into()));
    if c.basic_credentials {
        p.headers.push(("Authorization".into(), basic(&http::encode(&c.client_id), &http::encode(&c.client_secret))));
    } else {
        fields.push(("client_id", c.client_id.clone()));
        if !c.client_secret.is_empty() {
            fields.push(("client_secret", c.client_secret.clone()));
        }
    }
    p.body = fields.iter().map(|(k, v)| format!("{}={}", http::encode(k), http::encode(v))).collect::<Vec<_>>().join("&").into_bytes();
    let r = http::send_raw(&p, o)?;
    let text = String::from_utf8_lossy(&r.body);
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| tf!("respuesta {s} no es JSON: {t}", s = r.status, t = text.chars().take(300).collect::<String>()))?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    if r.status >= 400 || v.get("access_token").is_none() {
        let e = s("error");
        let d = s("error_description");
        return Err(if e.is_empty() { tf!("estado {s}: {t}", s = r.status, t = text.chars().take(300).collect::<String>()) } else { format!("{e} {d}").trim().to_string() });
    }
    let exp = v.get("expires_in").and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
    Ok(Token { access_token: s("access_token"), refresh_token: s("refresh_token"), token_type: s("token_type"), expires_at: if exp > 0 { now() + exp } else { 0 } })
}

/// Abre una URL en el navegador del sistema.
pub fn open_browser(url: &str) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|_| ())
}

/// Flujo authorization code + PKCE (S256): escucha en 127.0.0.1, abre el
/// navegador y espera el código (máx. 3 minutos).
fn authorization_code(c: &OAuth2, o: &Options) -> Result<Token, String> {
    if c.auth_url.trim().is_empty() {
        return Err(t!("falta la URL de autorización").into());
    }
    let (port, path) = parse_redirect(&c.redirect_uri)?;
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| tf!("no se pudo escuchar en el puerto {p}: {e}", p = port, e = e))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = if c.redirect_uri.trim().is_empty() { format!("http://127.0.0.1:{port}{path}") } else { c.redirect_uri.trim().to_string() };

    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_hex(32));
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_hex(12);
    let mut url = http::fix_url(&c.auth_url);
    let mut q = vec![
        ("response_type".to_string(), "code".to_string()),
        ("client_id".into(), c.client_id.clone()),
        ("redirect_uri".into(), redirect.clone()),
        ("state".into(), state.clone()),
        ("code_challenge".into(), challenge),
        ("code_challenge_method".into(), "S256".into()),
    ];
    if !c.scope.is_empty() {
        q.push(("scope".into(), c.scope.clone()));
    }
    if !c.audience.is_empty() {
        q.push(("audience".into(), c.audience.clone()));
    }
    http::add_query(&mut url, &q);
    open_browser(&url).map_err(|e| tf!("no se pudo abrir el navegador: {e}", e = e))?;

    let code = wait_for_code(&listener, &path, &state, Duration::from_secs(180))?;
    token_request(c, vec![("grant_type", "authorization_code".into()), ("code", code), ("redirect_uri", redirect), ("code_verifier", verifier)], o)
}

fn parse_redirect(r: &str) -> Result<(u16, String), String> {
    let r = r.trim();
    if r.is_empty() {
        return Ok((0, "/callback".into()));
    }
    let rest = r.strip_prefix("http://").ok_or_else(|| t!("la URL de retorno debe ser http://127.0.0.1:PUERTO/… o http://localhost:PUERTO/…").to_string())?;
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let (name, port) = host.rsplit_once(':').unwrap_or((host, "80"));
    if !matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
        return Err(t!("la URL de retorno debe ser http://127.0.0.1:PUERTO/… o http://localhost:PUERTO/…").into());
    }
    Ok((port.parse().map_err(|_| t!("puerto no válido en la URL de retorno").to_string())?, path))
}

fn query_param(q: &str, key: &str) -> Option<String> {
    q.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| decode(v))
}

/// Decodifica `%XX` y `+`.
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => {
                let v = |c: u8| (c as char).to_digit(16).unwrap() as u8;
                out.push(v(b[i + 1]) * 16 + v(b[i + 2]));
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn wait_for_code(listener: &TcpListener, path: &str, state: &str, limit: Duration) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let end = Instant::now() + limit;
    while Instant::now() < end {
        let (mut stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        let _ = BufReader::new(&stream).read_line(&mut line);
        let target = line.split_whitespace().nth(1).unwrap_or("");
        let (p, q) = target.split_once('?').unwrap_or((target, ""));
        if p != path {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        let (ok, msg, res) = if let Some(e) = query_param(q, "error") {
            let d = query_param(q, "error_description").unwrap_or_default();
            (false, format!("{e} {d}"), Err(format!("{e} {d}").trim().to_string()))
        } else if query_param(q, "state").as_deref() != Some(state) {
            (false, t!("estado (state) no coincide").to_string(), Err(t!("estado (state) no coincide").to_string()))
        } else if let Some(code) = query_param(q, "code") {
            (true, t!("Autorizado. Ya puedes cerrar esta pestaña y volver a peticiones.").to_string(), Ok(code))
        } else {
            (false, t!("la respuesta no trae código").to_string(), Err(t!("la respuesta no trae código").to_string()))
        };
        let html = format!("<!doctype html><meta charset=utf-8><title>peticiones</title><body style=\"font:16px system-ui;margin:3em\"><h2>{}</h2><p>{}</p>", if ok { "✔" } else { "✖" }, html_escape(&msg));
        let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len()).as_bytes());
        return res;
    }
    Err(t!("se agotó el tiempo esperando la autorización en el navegador").into())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_rfc2617_example() {
        // Ejemplo de la RFC 2617 §3.5.
        let ch = r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let h = digest_with_cnonce(ch, "Mufasa", "Circle Of Life", "GET", "http://www.nowhere.org/dir/index.html", b"", "0a4f113b").unwrap();
        assert!(h.contains("response=\"6629fae49393a05397450978507c4ef1\""), "{h}");
        assert!(h.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""));
    }

    #[test]
    fn digest_sha256_rfc7616() {
        // Ejemplo de la RFC 7616 §3.9.1 (SHA-256).
        let ch = r#"Digest realm="http-auth@example.org", qop="auth, auth-int", algorithm=SHA-256, nonce="7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v", opaque="FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS""#;
        let h = digest_with_cnonce(ch, "Mufasa", "Circle of Life", "GET", "http://www.example.org/dir/index.html", b"", "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ").unwrap();
        assert!(h.contains("response=\"753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1\""), "{h}");
    }

    #[test]
    fn basic_and_decode() {
        assert_eq!(basic("Aladdin", "open sesame"), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert_eq!(decode("a%20b+c%C3%B1%"), "a b cñ%");
        assert_eq!(parse_redirect("http://localhost:8765/cb").unwrap(), (8765, "/cb".into()));
        assert!(parse_redirect("https://evil.com/cb").is_err());
    }

    /// Flujo authorization code completo contra un servidor falso: el
    /// «navegador» es un hilo que sigue la redirección.
    #[test]
    fn auth_code_listener_accepts_matching_state() {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET /callback?code=abc%2B1&state=xyz HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut r = String::new();
            let _ = std::io::Read::read_to_string(&mut s, &mut r);
            r
        });
        assert_eq!(wait_for_code(&l, "/callback", "xyz", Duration::from_secs(5)).unwrap(), "abc+1");
        assert!(h.join().unwrap().contains("200 OK"));
    }

    /// Servidor de tokens falso: responde una vez con `body` y devuelve lo recibido.
    fn fake_token_server(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://127.0.0.1:{}/token", l.local_addr().unwrap().port());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = vec![0u8; 8192];
            let mut got = Vec::new();
            // Cabeceras + cuerpo (Content-Length) en una o varias lecturas.
            loop {
                let n = std::io::Read::read(&mut s, &mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let t = String::from_utf8_lossy(&got).to_string();
                if let Some(i) = t.find("\r\n\r\n") {
                    let len = t.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    if got.len() >= i + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
            String::from_utf8_lossy(&got).into_owned()
        });
        (url, h)
    }

    #[test]
    fn client_credentials_and_cache() {
        let (url, h) = fake_token_server(r#"{"access_token":"t1","token_type":"bearer","expires_in":3600}"#);
        let c = OAuth2 { token_url: url, client_id: "app".into(), client_secret: "s e".into(), scope: "leer".into(), basic_credentials: true, ..Default::default() };
        let (t, fresh) = oauth_token(&c, None, &Options::default()).unwrap();
        assert!(fresh);
        assert_eq!(t.access_token, "t1");
        assert!(t.expires_at > now() + 3000);
        let req = h.join().unwrap();
        assert!(req.contains("grant_type=client_credentials&scope=leer"), "{req}");
        assert!(req.contains(&format!("authorization: {}", basic("app", "s%20e"))) || req.contains(&format!("Authorization: {}", basic("app", "s%20e"))), "{req}");
        // Token vigente: no se pide otro.
        let (t2, fresh) = oauth_token(&c, Some(t.clone()), &Options::default()).unwrap();
        assert!(!fresh);
        assert_eq!(t2, t);
    }

    #[test]
    fn oauth_error_is_readable() {
        let (url, h) = fake_token_server(r#"{"error":"invalid_client","error_description":"mal secreto"}"#);
        let c = OAuth2 { token_url: url, client_id: "app".into(), ..Default::default() };
        let e = oauth_token(&c, None, &Options::default()).unwrap_err();
        assert_eq!(e, "invalid_client mal secreto");
        h.join().unwrap();
    }
}
