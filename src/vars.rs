//! Sustitución de `{{variable}}`.
//!
//! Orden de búsqueda: entorno elegido → variables del espacio. Los valores
//! secretos salen del archivo local de secretos. Variables dinámicas:
//! `{{$uuid}}`, `{{$timestamp}}` (segundos Unix) y `{{$isoTimestamp}}`.

use crate::model::SpaceFile;
use crate::store::Secrets;
use std::collections::HashMap;

/// Valores ya resueltos, listos para sustituir.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub values: HashMap<String, String>,
}

impl Scope {
    pub fn new(space: &SpaceFile, env: Option<&str>, secrets: &Secrets) -> Scope {
        let mut values = HashMap::new();
        for v in &space.vars {
            let val = if v.secret { secrets.variables.get(&v.name).cloned().unwrap_or_default() } else { v.value.clone() };
            values.insert(v.name.clone(), val);
        }
        if let Some(e) = env.and_then(|n| space.envs.iter().find(|e| e.name == n)) {
            let sec = secrets.entornos.get(&e.name);
            for v in &e.vars {
                let val = if v.secret { sec.and_then(|m| m.get(&v.name)).cloned().unwrap_or_default() } else { v.value.clone() };
                values.insert(v.name.clone(), val);
            }
        }
        Scope { values }
    }

    /// Sustituye las variables; las que no existen se dejan tal cual y se
    /// añaden a `missing`. Un valor puede contener otras variables (hasta 5 niveles).
    pub fn apply(&self, s: &str, missing: &mut Vec<String>) -> String {
        self.apply_depth(s, missing, 0)
    }

    fn apply_depth(&self, s: &str, missing: &mut Vec<String>, depth: usize) -> String {
        if !s.contains("{{") {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find("{{") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 2..];
            let Some(j) = after.find("}}") else {
                out.push_str(&rest[i..]);
                return out;
            };
            let name = after[..j].trim();
            match self.lookup(name) {
                Some(v) if depth < 5 => out.push_str(&self.apply_depth(&v, missing, depth + 1)),
                Some(v) => out.push_str(&v),
                None => {
                    if !missing.iter().any(|m| m == name) {
                        missing.push(name.to_string());
                    }
                    out.push_str(&rest[i..i + 2 + j + 2]);
                }
            }
            rest = &after[j + 2..];
        }
        out.push_str(rest);
        out
    }

    fn lookup(&self, name: &str) -> Option<String> {
        if let Some(v) = self.values.get(name) {
            return Some(v.clone());
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        match name {
            "$uuid" | "$randomUUID" => Some(uuid4()),
            "$timestamp" => Some(now.as_secs().to_string()),
            "$isoTimestamp" => Some(iso8601(now.as_secs())),
            _ => None,
        }
    }
}

/// UUID v4 aleatorio.
pub fn uuid4() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("sin generador aleatorio");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Fecha UTC `AAAA-MM-DDTHH:MM:SSZ` (algoritmo de días civiles de H. Hinnant).
pub fn iso8601(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Environment, Var};

    #[test]
    fn env_overrides_space_and_secrets_fill_in() {
        let space = SpaceFile {
            vars: vec![Var { name: "base".into(), value: "http://a".into(), secret: false }, Var { name: "clave".into(), value: String::new(), secret: true }],
            envs: vec![Environment { name: "prod".into(), vars: vec![Var { name: "base".into(), value: "https://b".into(), secret: false }] }],
            ..Default::default()
        };
        let mut sec = Secrets::default();
        sec.variables.insert("clave".into(), "s3cr3t".into());
        let mut miss = vec![];
        let s = Scope::new(&space, Some("prod"), &sec);
        assert_eq!(s.apply("{{base}}/x?k={{ clave }}&{{nada}}", &mut miss), "https://b/x?k=s3cr3t&{{nada}}");
        assert_eq!(miss, vec!["nada"]);
        let s = Scope::new(&space, None, &sec);
        assert_eq!(s.apply("{{base}}", &mut miss), "http://a");
        assert_eq!(s.apply("{{$uuid}}", &mut miss).len(), 36);
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_795_000_000), "2026-11-18T11:06:40Z");
    }
}
