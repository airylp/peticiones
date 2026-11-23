//! Datos de un espacio y sus peticiones, tal como se guardan en disco.
//!
//! Un espacio es una carpeta: `espacio.toml` (nombre, variables, entornos,
//! autenticación común) y un `.toml` por petición, en subcarpetas si se quiere
//! agrupar. Todo es texto legible para poder compartirlo con git, un zip o una
//! carpeta en la nube. Los valores secretos NO se guardan aquí (ver `store`).

use serde::{Deserialize, Serialize};

/// Par nombre/valor con casilla (cabeceras, parámetros, campos de formulario).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pair {
    #[serde(rename = "nombre")]
    pub name: String,
    #[serde(rename = "valor", default)]
    pub value: String,
    #[serde(rename = "activo", default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}
fn is_false(b: &bool) -> bool {
    !*b
}

impl Pair {
    #[cfg(test)]
    pub fn new(name: &str, value: &str) -> Pair {
        Pair { name: name.into(), value: value.into(), enabled: true }
    }
}

/// Variable del espacio o de un entorno. Si es secreta, su valor vive en el
/// archivo local de secretos y en el espacio queda vacío.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Var {
    #[serde(rename = "nombre")]
    pub name: String,
    #[serde(rename = "valor", default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    #[serde(rename = "secreto", default, skip_serializing_if = "is_false")]
    pub secret: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    #[serde(rename = "nombre")]
    pub name: String,
    #[serde(rename = "variables", default)]
    pub vars: Vec<Var>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyKind {
    #[default]
    #[serde(rename = "ninguno")]
    None,
    #[serde(rename = "texto")]
    Text,
    #[serde(rename = "json")]
    Json,
    #[serde(rename = "xml")]
    Xml,
    /// application/x-www-form-urlencoded con `fields`.
    #[serde(rename = "formulario")]
    Form,
    /// Sobre SOAP 1.1: text/xml + cabecera SOAPAction.
    #[serde(rename = "soap11")]
    Soap11,
    /// Sobre SOAP 1.2: application/soap+xml;action="…".
    #[serde(rename = "soap12")]
    Soap12,
}

impl BodyKind {
    pub const ALL: [BodyKind; 7] = [BodyKind::None, BodyKind::Json, BodyKind::Xml, BodyKind::Text, BodyKind::Form, BodyKind::Soap11, BodyKind::Soap12];

    pub fn label(self) -> &'static str {
        match self {
            BodyKind::None => t!("Sin cuerpo"),
            BodyKind::Text => t!("Texto"),
            BodyKind::Json => "JSON",
            BodyKind::Xml => "XML",
            BodyKind::Form => t!("Formulario"),
            BodyKind::Soap11 => "SOAP 1.1",
            BodyKind::Soap12 => "SOAP 1.2",
        }
    }

    /// Content-Type que se pone si el usuario no puso uno.
    pub fn content_type(self, soap_action: &str) -> Option<String> {
        Some(match self {
            BodyKind::None => return None,
            BodyKind::Text => "text/plain; charset=utf-8".into(),
            BodyKind::Json => "application/json".into(),
            BodyKind::Xml => "application/xml; charset=utf-8".into(),
            BodyKind::Form => "application/x-www-form-urlencoded".into(),
            BodyKind::Soap11 => "text/xml; charset=utf-8".into(),
            BodyKind::Soap12 if soap_action.is_empty() => "application/soap+xml; charset=utf-8".into(),
            BodyKind::Soap12 => format!("application/soap+xml; charset=utf-8; action=\"{soap_action}\""),
        })
    }

    pub fn is_soap(self) -> bool {
        matches!(self, BodyKind::Soap11 | BodyKind::Soap12)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Body {
    #[serde(rename = "tipo", default)]
    pub kind: BodyKind,
    #[serde(rename = "texto", default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(rename = "campos", default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Pair>,
    #[serde(rename = "soap_action", default, skip_serializing_if = "String::is_empty")]
    pub soap_action: String,
}

/// Dónde va la API key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyPlace {
    #[default]
    #[serde(rename = "cabecera")]
    Header,
    #[serde(rename = "query")]
    Query,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OAuthFlow {
    #[default]
    #[serde(rename = "client_credentials")]
    ClientCredentials,
    /// Authorization code con PKCE: abre el navegador y recibe el código en 127.0.0.1.
    #[serde(rename = "authorization_code")]
    AuthorizationCode,
}

/// Autenticación. Todos los campos admiten `{{variables}}`: lo normal es que
/// contraseñas y secretos sean variables secretas, así el archivo se comparte
/// sin credenciales.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tipo")]
pub enum Auth {
    /// Usa la del espacio.
    #[default]
    #[serde(rename = "heredar")]
    Inherit,
    #[serde(rename = "ninguna")]
    None,
    #[serde(rename = "basica")]
    Basic {
        #[serde(rename = "usuario", default)]
        user: String,
        #[serde(rename = "clave", default)]
        password: String,
    },
    #[serde(rename = "bearer")]
    Bearer {
        #[serde(default)]
        token: String,
    },
    #[serde(rename = "apikey")]
    ApiKey {
        #[serde(rename = "nombre", default)]
        name: String,
        #[serde(rename = "valor", default)]
        value: String,
        #[serde(rename = "en", default)]
        place: KeyPlace,
    },
    #[serde(rename = "digest")]
    Digest {
        #[serde(rename = "usuario", default)]
        user: String,
        #[serde(rename = "clave", default)]
        password: String,
    },
    #[serde(rename = "oauth2")]
    OAuth2(OAuth2),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuth2 {
    #[serde(rename = "flujo", default)]
    pub flow: OAuthFlow,
    #[serde(rename = "url_token", default)]
    pub token_url: String,
    #[serde(rename = "url_autorizacion", default, skip_serializing_if = "String::is_empty")]
    pub auth_url: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
    #[serde(rename = "alcance", default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    #[serde(rename = "audiencia", default, skip_serializing_if = "String::is_empty")]
    pub audience: String,
    /// Authorization code: URL de retorno registrada en el proveedor
    /// (`http://127.0.0.1:PUERTO/ruta`). Vacía = puerto libre al azar.
    #[serde(rename = "url_retorno", default, skip_serializing_if = "String::is_empty")]
    pub redirect_uri: String,
    /// Mandar client_id/secret con Basic en vez de en el cuerpo.
    #[serde(rename = "credenciales_basic", default, skip_serializing_if = "is_false")]
    pub basic_credentials: bool,
}

impl Auth {

    pub fn label(&self) -> &'static str {
        match self {
            Auth::Inherit => t!("Heredar del espacio"),
            Auth::None => t!("Ninguna"),
            Auth::Basic { .. } => t!("Básica"),
            Auth::Bearer { .. } => "Bearer",
            Auth::ApiKey { .. } => "API key",
            Auth::Digest { .. } => "Digest",
            Auth::OAuth2(_) => "OAuth 2.0",
        }
    }

    /// Una de cada tipo, vacía (para el selector).
    pub fn choices(for_space: bool) -> Vec<Auth> {
        let mut v = vec![
            Auth::None,
            Auth::Basic { user: String::new(), password: String::new() },
            Auth::Bearer { token: String::new() },
            Auth::ApiKey { name: "X-API-Key".into(), value: String::new(), place: KeyPlace::Header },
            Auth::Digest { user: String::new(), password: String::new() },
            Auth::OAuth2(OAuth2::default()),
        ];
        if !for_space {
            v.insert(0, Auth::Inherit);
        }
        v
    }

    pub fn same_kind(&self, o: &Auth) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(o)
    }
}

pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Una petición (un archivo `.toml`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    #[serde(rename = "nombre", default)]
    pub name: String,
    #[serde(rename = "metodo", default = "get")]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(rename = "parametros", default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Pair>,
    #[serde(rename = "cabeceras", default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<Pair>,
    #[serde(rename = "cuerpo", default)]
    pub body: Body,
    #[serde(default)]
    pub auth: Auth,
    #[serde(rename = "notas", default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

fn get() -> String {
    "GET".into()
}

impl Default for Request {
    fn default() -> Self {
        Request { name: String::new(), method: get(), url: String::new(), params: vec![], headers: vec![], body: Body::default(), auth: Auth::Inherit, notes: String::new() }
    }
}

/// `espacio.toml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpaceFile {
    /// Identificador estable: con él se encuentran los secretos locales aunque
    /// la carpeta cambie de sitio o de nombre.
    pub id: String,
    #[serde(rename = "nombre", default)]
    pub name: String,
    #[serde(rename = "variables", default, skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<Var>,
    #[serde(rename = "entornos", default, skip_serializing_if = "Vec::is_empty")]
    pub envs: Vec<Environment>,
    /// Autenticación que heredan las peticiones.
    #[serde(default = "no_auth")]
    pub auth: Auth,
    #[serde(rename = "cabeceras", default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<Pair>,
}

fn no_auth() -> Auth {
    Auth::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_toml_roundtrip_is_readable() {
        let r = Request {
            name: "Crear".into(),
            method: "POST".into(),
            url: "{{base}}/users".into(),
            headers: vec![Pair::new("Accept", "application/json")],
            body: Body { kind: BodyKind::Json, text: "{\"a\":1}".into(), ..Default::default() },
            auth: Auth::Basic { user: "ana".into(), password: "{{clave}}".into() },
            ..Default::default()
        };
        let s = toml::to_string(&r).unwrap();
        assert!(s.contains("metodo = \"POST\""), "{s}");
        assert!(s.contains("tipo = \"basica\""), "{s}");
        assert_eq!(toml::from_str::<Request>(&s).unwrap(), r);
        // Un archivo mínimo escrito a mano también vale.
        let m: Request = toml::from_str("url = \"http://x\"").unwrap();
        assert_eq!(m.method, "GET");
        assert_eq!(m.auth, Auth::Inherit);
    }
}
