# peticiones

Cliente de peticiones HTTP y SOAP de escritorio (Rust + egui), ligero, para probar APIs y
compartir colecciones como carpetas de texto.

## Instalar

macOS y Linux (descarga la última versión publicada; vuelve a ejecutarlo para actualizar o desinstalar):

```bash
curl -fsSL https://raw.githubusercontent.com/airylp/peticiones/main/install.sh | sh
```

Para compilarlo en tu equipo con lo último de `main` (instala Rust con rustup si falta):

```bash
curl -fsSL https://raw.githubusercontent.com/airylp/peticiones/main/install.sh | sh -s -- compilar
```

Si no hay versión publicada para tu equipo (p. ej. Linux ARM), el instalador compila automáticamente.

## Descargar

Prototipo en [Releases](https://github.com/airylp/peticiones/releases): macOS (universal), Windows y
Linux x86_64. Las compilaciones no están firmadas: en macOS ábrela la primera vez con clic derecho →
Abrir; en Windows, «Más información» → «Ejecutar de todas formas».

## Espacios

Un espacio es una carpeta que se puede versionar con git o compartir como zip:

```text
mi-api/
  espacio.toml           nombre, variables, entornos, autenticación y cabeceras comunes
  usuarios/listar.toml   una petición por archivo; las carpetas agrupan
  usuarios/crear.toml
```

Petición de ejemplo (`usuarios/crear.toml`):

```toml
nombre = "Crear usuario"
metodo = "POST"
url = "{{base}}/users"
cabeceras = [{ nombre = "Accept", valor = "application/json" }]

[cuerpo]
tipo = "json"          # ninguno | texto | json | xml | formulario | soap11 | soap12
texto = '{"nombre": "Ana"}'

[auth]
tipo = "heredar"       # heredar | ninguna | basica | bearer | apikey | digest | oauth2
```

### Secretos

Las variables marcadas como **secretas** no se guardan en el espacio: su valor vive solo en este
equipo (`~/.config/peticiones/secretos/<id>.toml`, en macOS `~/Library/Application Support/peticiones`),
igual que los tokens OAuth obtenidos. Así un espacio se comparte sin credenciales: quien lo recibe
pone sus propios valores.

## Funciones

- Métodos estándar y personalizados, parámetros, cabeceras, cuerpo JSON/XML/texto/formulario.
- SOAP 1.1 y 1.2 (Content-Type y SOAPAction automáticos, sobre de ejemplo).
- Autenticación: básica, Bearer, API key (cabecera o query), Digest (MD5/SHA-256),
  OAuth 2.0 client credentials y authorization code + PKCE (abre el navegador y recibe el
  código en `127.0.0.1`), con caché y renovación del token.
- Variables `{{nombre}}` del espacio y por entorno; dinámicas `{{$uuid}}`, `{{$timestamp}}`,
  `{{$isoTimestamp}}`.
- Importar: comandos curl (también pegándolos en la URL), colecciones y entornos de Postman,
  OpenAPI 3 / Swagger 2 (JSON o YAML) y WSDL 1.1 (una petición SOAP por operación con sobre de
  ejemplo), desde archivo, texto o URL. Exportar un espacio como colección de Postman v2.1.
- Respuesta formateada (JSON/XML), cabeceras, lo enviado y «Copiar como curl».
- Guardado automático; Ctrl+Enter envía, Ctrl+S guarda, Ctrl+W cierra la pestaña.
- Interfaz en español, inglés y francés.

## Compilar

```bash
cargo build --release
./target/release/peticiones
```

Pruebas: `cargo test --release` (las de red: `cargo test --release live -- --ignored`).
