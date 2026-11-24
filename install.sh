#!/usr/bin/env sh
# peticiones: instalar, actualizar o desinstalar con un solo comando.
#
#     curl -fsSL https://raw.githubusercontent.com/airylp/peticiones/main/install.sh | sh
#
#   Pregunta qué hacer: si ya está instalado ofrece actualizar o desinstalar; si
#   no, instalarlo. También se puede indicar directamente (sin preguntas):
#     … | sh -s -- instalar        (instalar o actualizar)
#     … | sh -s -- compilar        (compilar en este equipo lo último de main)
#     … | sh -s -- desinstalar     (pregunta si borrar también la configuración)
#
#   Por defecto descarga la última versión publicada en GitHub Releases (segundos,
#   sin Rust). Si no hay versión para este equipo (p. ej. Linux ARM) o se pide
#   «compilar», descarga el código y lo compila (instala Rust con rustup si falta).
#   macOS: Peticiones.app en /Applications (o ~/Applications sin permiso).
#   Linux: ~/.local/bin/peticiones y entrada en el menú de aplicaciones.
#
# Variables opcionales: PETICIONES_REPO (usuario/repo), PETICIONES_VERSION (tag,
#   p. ej. v0.1.0; por defecto la más reciente), PETICIONES_REF (rama o tag a
#   compilar; main), PETICIONES_DESTINO (carpeta de instalación, para pruebas),
#   PETICIONES_IDIOMA (es|en|fr).
set -eu

REPO="${PETICIONES_REPO:-airylp/peticiones}"
REF="${PETICIONES_REF:-main}"
ACCION="${1:-}"
OS="$(uname)"

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
# Mensajes en el idioma elegido: m "español" "english" "français".
m() { case "${IDIOMA:-es}" in en) printf '%s' "$2" ;; fr) printf '%s' "$3" ;; *) printf '%s' "$1" ;; esac; }

hay_terminal() { (: < /dev/tty) 2>/dev/null; }
leer() { # leer "pregunta" → respuesta en $r (de la terminal: el script llega por la tubería de curl)
    printf '%s' "$1" > /dev/tty
    read -r r < /dev/tty || r=""
}
pregunta() {
    if hay_terminal; then
        leer "$1 $(m '[s/N]' '[y/N]' '[o/N]') "
        case "$r" in s|S|si|sí|Sí|y|Y|yes|o|O|oui) return 0 ;; esac
    fi
    return 1
}

if [ "$OS" = "Darwin" ]; then
    APP_DIRS="${PETICIONES_DESTINO:-/Applications $HOME/Applications}"
    CONFIG="$HOME/Library/Application Support/peticiones"
    CACHE="$HOME/Library/Caches/peticiones-compilacion"
    PLATAFORMA="macos-universal"
    EXT="zip"
elif [ "$OS" = "Linux" ]; then
    BIN_DIR="${PETICIONES_DESTINO:-$HOME/.local/bin}"
    DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
    CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/peticiones"
    CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/peticiones-compilacion"
    # Solo se publica para x86_64; en otras arquitecturas se compila.
    case "$(uname -m)" in
        x86_64 | amd64) PLATAFORMA="linux-x86_64" ;;
        *) PLATAFORMA="" ;;
    esac
    EXT="tar.gz"
else
    fail "$(m 'En Windows usa install.ps1 (PowerShell: irm https://raw.githubusercontent.com/airylp/peticiones/main/install.ps1 | iex) o el instalador de https://github.com/airylp/peticiones/releases' 'On Windows use install.ps1 (PowerShell: irm https://raw.githubusercontent.com/airylp/peticiones/main/install.ps1 | iex) or the installer from https://github.com/airylp/peticiones/releases' "Sous Windows, utilisez install.ps1 (PowerShell : irm https://raw.githubusercontent.com/airylp/peticiones/main/install.ps1 | iex) ou l'installateur de https://github.com/airylp/peticiones/releases")"
fi
MARCA="$CONFIG/version-instalada"
# Tamaños del ícono en el tema hicolor (Linux).
ICON_SIZES="16 32 64 128 256 512"

# ------------------------------------------------------------ idioma
# PETICIONES_IDIOMA, el guardado en la sesión de la app o, si hay terminal y no
# se indicó acción, se pregunta (y se guarda para la app).
SESION="$CONFIG/sesion.toml"
IDIOMA="${PETICIONES_IDIOMA:-}"
GUARDAR_IDIOMA="$IDIOMA"
[ -n "$IDIOMA" ] || IDIOMA="$(sed -n 's/^idioma *= *"\(..\)".*/\1/p' "$SESION" 2>/dev/null | head -n 1)"
if [ -z "$IDIOMA" ]; then
    case "${LC_ALL:-${LANG:-}}" in en*) IDIOMA=en ;; fr*) IDIOMA=fr ;; *) IDIOMA=es ;; esac
    if [ -z "$ACCION" ] && hay_terminal; then
        printf 'Idioma / Language / Langue\n  1) Español\n  2) English\n  3) Français\n' > /dev/tty
        case "$IDIOMA" in en) def=2 ;; fr) def=3 ;; *) def=1 ;; esac
        leer "[1-3] (Enter = $def): "
        case "${r:-$def}" in 2) IDIOMA=en ;; 3) IDIOMA=fr ;; *) IDIOMA=es ;; esac
        GUARDAR_IDIOMA="$IDIOMA"
    fi
fi
case "$IDIOMA" in es|en|fr) ;; *) IDIOMA=es ;; esac

# Deja el idioma en la sesión de la app (clave de primer nivel: va al principio).
guardar_idioma() {
    [ -n "$GUARDAR_IDIOMA" ] || return 0
    mkdir -p "$CONFIG"
    if [ -f "$SESION" ] && grep -q '^idioma *=' "$SESION"; then
        sed "s/^idioma *=.*/idioma = \"$GUARDAR_IDIOMA\"/" "$SESION" > "$SESION.tmp" && mv "$SESION.tmp" "$SESION"
    else
        { printf 'idioma = "%s"\n' "$GUARDAR_IDIOMA"; cat "$SESION" 2>/dev/null || true; } > "$SESION.tmp" && mv "$SESION.tmp" "$SESION"
    fi
}

# ------------------------------------------------------------ estado
instalado() {
    if [ "$OS" = "Darwin" ]; then
        for d in $APP_DIRS; do [ -d "$d/Peticiones.app" ] && { echo "$d/Peticiones.app"; return 0; }; done
        return 1
    fi
    [ -x "$BIN_DIR/peticiones" ] && echo "$BIN_DIR/peticiones"
}

# Tag de la versión a instalar: la pedida o la más reciente (incluye prototipos).
version_remota() {
    if [ -n "${PETICIONES_VERSION:-}" ]; then echo "$PETICIONES_VERSION"; return; fi
    curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1
}

# ------------------------------------------------------------ desinstalar
desinstalar() {
    ruta="$(instalado || true)"
    if [ -z "$ruta" ]; then
        info "$(m 'peticiones no está instalado.' 'peticiones is not installed.' "peticiones n'est pas installé.")"
    else
        if [ "$OS" = "Darwin" ]; then
            pkill -x peticiones 2>/dev/null || true
            rm -rf "$ruta"
        else
            rm -f "$BIN_DIR/peticiones" "$DATA/applications/peticiones.desktop"
            for t in $ICON_SIZES; do rm -f "$DATA/icons/hicolor/${t}x${t}/apps/peticiones.png"; done
            command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA/applications" 2>/dev/null || true
        fi
        rm -f "$MARCA"
        info "$(m "Desinstalado: $ruta" "Uninstalled: $ruta" "Désinstallé : $ruta")"
    fi
    # La configuración incluye los secretos (contraseñas, tokens): solo si se confirma.
    # Los espacios (carpetas de peticiones) nunca se tocan.
    if [ -d "$CONFIG" ] && pregunta "$(m "¿Borrar también la configuración y los secretos guardados ($CONFIG)? Tus espacios no se tocan." "Also delete settings and saved secrets ($CONFIG)? Your spaces are not touched." "Supprimer aussi la configuration et les secrets enregistrés ($CONFIG) ? Vos espaces ne sont pas touchés.")"; then
        rm -rf "$CONFIG"
        info "$(m 'Configuración borrada.' 'Settings deleted.' 'Configuration supprimée.')"
    fi
}

# ------------------------------------------------------------ colocar
# Pone la app en su sitio (macOS) a partir de una Peticiones.app ya armada.
colocar_app() {
    src="$1"
    destino=""
    for d in $APP_DIRS; do
        mkdir -p "$d" 2>/dev/null || true
        if [ -w "$d" ]; then destino="$d"; break; fi
    done
    [ -n "$destino" ] || fail "$(m "No hay permiso para escribir en: $APP_DIRS" "No permission to write to: $APP_DIRS" "Pas de permission d'écriture dans : $APP_DIRS")"
    abierta=0
    # PETICIONES_NO_CERRAR (lo pone el actualizador de la app): no se cierra la app
    # abierta; la versión nueva se abre al pulsar «Reiniciar ahora».
    if [ -z "${PETICIONES_NO_CERRAR:-}" ] && pgrep -x peticiones >/dev/null 2>&1; then
        abierta=1
        osascript -e 'quit app "Peticiones"' >/dev/null 2>&1 || pkill -x peticiones || true
        sleep 1
    fi
    rm -rf "$destino/Peticiones.app"
    mv "$src" "$destino/Peticiones.app"
    /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$destino/Peticiones.app" 2>/dev/null || true
    ruta="$destino/Peticiones.app"
    if [ "$abierta" = 1 ]; then open "$ruta" || true; fi
}

# Pone el ejecutable en su sitio (Linux) y crea la entrada del menú.
colocar_bin() {
    mkdir -p "$BIN_DIR"
    # Copia + rename: funciona aunque la app esté abierta.
    cp "$1" "$BIN_DIR/.peticiones.nuevo"
    chmod +x "$BIN_DIR/.peticiones.nuevo"
    mv "$BIN_DIR/.peticiones.nuevo" "$BIN_DIR/peticiones"
    ruta="$BIN_DIR/peticiones"
    if [ -z "${PETICIONES_DESTINO:-}" ]; then
        # Ícono (la «P» de cubos) generado por la propia app, escrito de forma atómica.
        ic="$(mktemp -d)"
        if "$ruta" --iconset "$ic" 2>/dev/null; then
            for t in $ICON_SIZES; do
                case $t in 64) f=icon_32x32@2x.png ;; *) f="icon_${t}x${t}.png" ;; esac
                d="$DATA/icons/hicolor/${t}x${t}/apps"
                mkdir -p "$d" && cp "$ic/$f" "$d/.peticiones.png.nuevo" && mv -f "$d/.peticiones.png.nuevo" "$d/peticiones.png"
            done
            touch "$DATA/icons/hicolor" 2>/dev/null || true
        fi
        rm -rf "$ic"
        mkdir -p "$DATA/applications"
        cat > "$DATA/applications/peticiones.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Peticiones
Comment=$(m 'Cliente de peticiones HTTP y SOAP' 'HTTP and SOAP request client' 'Client de requêtes HTTP et SOAP')
Exec=$ruta
Icon=peticiones
Terminal=false
Categories=Development;Network;
StartupWMClass=peticiones
DESKTOP
        command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA/applications" 2>/dev/null || true
    fi
    case ":$PATH:" in *":$BIN_DIR:"*) ;; *) info "$(m "Añade $BIN_DIR a tu PATH para abrirlo con «peticiones»." "Add $BIN_DIR to your PATH to run it as “peticiones”." "Ajoutez $BIN_DIR à votre PATH pour le lancer avec «peticiones».")" ;; esac
}

terminar() { # terminar versión
    mkdir -p "$CONFIG"
    echo "$1" > "$MARCA"
    guardar_idioma
    info "$(m "Instalado $1 en $ruta" "Installed $1 in $ruta" "$1 installé dans $ruta")"
    # Para el actualizador de la app (no depende del idioma).
    echo "RUTA_INSTALADA=$ruta"
}

# ------------------------------------------------------------ instalar (descarga)
instalar() {
    command -v curl >/dev/null 2>&1 || fail "$(m 'Hace falta curl.' 'curl is required.' 'curl est nécessaire.')"
    if [ -z "$PLATAFORMA" ]; then
        info "$(m "No hay versión publicada para $(uname -m): se compila en este equipo." "No published build for $(uname -m): building on this machine." "Pas de version publiée pour $(uname -m) : compilation sur cette machine.")"
        compilar
        return
    fi
    tag="$(version_remota || true)"
    if [ -z "$tag" ]; then
        if pregunta "$(m "No se encontró ninguna versión publicada. ¿Compilar en este equipo?" "No published version found. Build on this machine?" "Aucune version publiée. Compiler sur cette machine ?")"; then compilar; return; fi
        fail "$(m "No se encontró ninguna versión publicada en https://github.com/$REPO/releases" "No published version found at https://github.com/$REPO/releases" "Aucune version publiée sur https://github.com/$REPO/releases")"
    fi
    if [ "$(cat "$MARCA" 2>/dev/null || true)" = "$tag" ] && instalado >/dev/null && [ "$ACCION" != "instalar" ]; then
        info "$(m "Ya tienes la versión más reciente ($tag)." "You already have the latest version ($tag)." "Vous avez déjà la dernière version ($tag).")"
        return 0
    fi
    archivo="peticiones-${tag#v}-$PLATAFORMA.$EXT"
    url="https://github.com/$REPO/releases/download/$tag/$archivo"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    info "$(m "Descargando $archivo…" "Downloading $archivo…" "Téléchargement de $archivo…")"
    if ! curl -fL --progress-bar -o "$tmp/$archivo" "$url"; then
        if pregunta "$(m "No se pudo descargar $url. ¿Compilar en este equipo?" "Could not download $url. Build on this machine?" "Impossible de télécharger $url. Compiler sur cette machine ?")"; then compilar; return; fi
        fail "$(m "No se pudo descargar $url" "Could not download $url" "Impossible de télécharger $url")"
    fi
    if [ "$OS" = "Darwin" ]; then
        ditto -x -k "$tmp/$archivo" "$tmp/x"
        [ -d "$tmp/x/Peticiones.app" ] || fail "$(m 'El archivo descargado no contiene Peticiones.app' 'The download does not contain Peticiones.app' "Le téléchargement ne contient pas Peticiones.app")"
        # Sin firma de Apple: se quita la cuarentena para que abra sin avisos.
        xattr -dr com.apple.quarantine "$tmp/x/Peticiones.app" 2>/dev/null || true
        colocar_app "$tmp/x/Peticiones.app"
    else
        tar -xzf "$tmp/$archivo" -C "$tmp"
        colocar_bin "$tmp/peticiones-${tag#v}-$PLATAFORMA/peticiones"
    fi
    terminar "$tag"
}

# ------------------------------------------------------------ compilar
compilar() {
    command -v curl >/dev/null 2>&1 || fail "$(m 'Hace falta curl.' 'curl is required.' 'curl est nécessaire.')"
    command -v cc >/dev/null 2>&1 || {
        if [ "$OS" = "Darwin" ]; then
            fail "$(m 'Hace falta un compilador de C: ejecuta «xcode-select --install» y vuelve a intentarlo.' 'A C compiler is needed: run “xcode-select --install” and try again.' 'Un compilateur C est nécessaire : lancez «xcode-select --install» puis réessayez.')"
        else
            fail "$(m 'Hace falta un compilador de C (p. ej. «sudo apt install build-essential pkg-config libgtk-3-dev»).' 'A C compiler is needed (e.g. “sudo apt install build-essential pkg-config libgtk-3-dev”).' 'Un compilateur C est nécessaire (p. ex. «sudo apt install build-essential pkg-config libgtk-3-dev»).')"
        fi
    }
    # Rust: usa el instalado o lo instala con rustup en ~/.cargo (sin sudo).
    PATH="$HOME/.cargo/bin:$PATH"
    if ! command -v cargo >/dev/null 2>&1; then
        info "$(m 'Rust no encontrado; instalando con rustup en ~/.cargo' 'Rust not found; installing it with rustup in ~/.cargo' 'Rust introuvable ; installation avec rustup dans ~/.cargo')"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
    fi
    # rustup instalado pero sin toolchain por defecto: se elige la estable.
    if ! cargo --version >/dev/null 2>&1 && command -v rustup >/dev/null 2>&1; then
        rustup default stable || true
    fi
    cargo --version >/dev/null 2>&1 || fail "$(m 'Rust no funciona en este equipo (cargo --version falló).' 'Rust does not work on this machine (cargo --version failed).' 'Rust ne fonctionne pas sur cette machine (cargo --version a échoué).')"
    info "$(m 'Usando' 'Using' 'Utilisation de') $(cargo --version)"
    # Commit que se compila (para saber luego qué versión está instalada).
    sha="$(curl -fsSL "https://api.github.com/repos/$REPO/commits/$REF" 2>/dev/null | sed -n 's/^  "sha": *"\([0-9a-f]\{7\}\).*/\1/p' | head -n 1)"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    info "$(m "Descargando el código ($REF)…" "Downloading the source ($REF)…" "Téléchargement du code ($REF)…")"
    curl -fsSL "https://codeload.github.com/$REPO/tar.gz/$REF" | tar -xz -C "$tmp" || fail "$(m "No se pudo descargar el código de $REPO ($REF)" "Could not download the source of $REPO ($REF)" "Impossible de télécharger le code de $REPO ($REF)")"
    src="$(find "$tmp" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
    info "$(m 'Compilando (la primera vez tarda unos minutos)…' 'Building (the first time takes a few minutes)…' 'Compilation (la première fois prend quelques minutes)…')"
    # La caché acelera las actualizaciones; si pasa de 3 GB se vacía.
    if [ -d "$CACHE" ] && [ "$(du -sk "$CACHE" | cut -f1)" -gt 3145728 ]; then
        rm -rf "$CACHE" && info "$(m 'Caché de compilación vaciada (pasaba de 3 GB)' 'Build cache emptied (it was over 3 GB)' 'Cache de compilation vidé (il dépassait 3 Go)')"
    fi
    (cd "$src" && PETICIONES_COMMIT="$sha" CARGO_TARGET_DIR="$CACHE" cargo build --release --locked) || fail "$(m 'La compilación falló.' 'The build failed.' 'La compilation a échoué.')"
    version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$src/Cargo.toml" | head -n 1)"
    etiqueta="$REF${sha:+@$sha}"
    if [ "$OS" = "Darwin" ]; then
        app="$tmp/Peticiones.app"
        mkdir -p "$app/Contents/MacOS"
        cp "$CACHE/release/peticiones" "$app/Contents/MacOS/peticiones"
        mkdir -p "$app/Contents/Resources"
        if "$app/Contents/MacOS/peticiones" --iconset "$tmp/peticiones.iconset" && iconutil -c icns "$tmp/peticiones.iconset" -o "$app/Contents/Resources/peticiones.icns"; then :; fi
        cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Peticiones</string>
  <key>CFBundleDisplayName</key><string>Peticiones</string>
  <key>CFBundleIdentifier</key><string>io.github.airylp.peticiones</string>
  <key>CFBundleExecutable</key><string>peticiones</string>
  <key>CFBundleIconFile</key><string>peticiones</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
        codesign --force --deep -s - "$app" 2>/dev/null || true
        colocar_app "$app"
    else
        colocar_bin "$CACHE/release/peticiones"
    fi
    terminar "$etiqueta"
}

# ------------------------------------------------------------ menú
case "$ACCION" in
    instalar | install | actualizar | update) instalar ;;
    compilar | build) compilar ;;
    desinstalar | uninstall) desinstalar ;;
    "")
        if ruta="$(instalado)"; then
            actual="$(cat "$MARCA" 2>/dev/null || echo '?')"
            if hay_terminal; then
                printf '%s\n  1) %s\n  2) %s\n  3) %s\n' "$(m "peticiones $actual ya está instalado ($ruta)." "peticiones $actual is already installed ($ruta)." "peticiones $actual est déjà installé ($ruta).")" \
                    "$(m 'Actualizar' 'Update' 'Mettre à jour')" "$(m 'Desinstalar' 'Uninstall' 'Désinstaller')" "$(m 'Cancelar' 'Cancel' 'Annuler')" > /dev/tty
                leer "[1-3] (Enter = 1): "
                case "${r:-1}" in
                    1) instalar ;;
                    2) desinstalar ;;
                    *) info "$(m 'Nada que hacer.' 'Nothing to do.' 'Rien à faire.')" ;;
                esac
            else
                instalar
            fi
        elif hay_terminal; then
            if pregunta "$(m '¿Instalar peticiones?' 'Install peticiones?' 'Installer peticiones ?')"; then instalar; else info "$(m 'Cancelado.' 'Cancelled.' 'Annulé.')"; fi
        else
            instalar
        fi
        ;;
    *) fail "$(m "Acción desconocida: $ACCION (usa instalar, compilar o desinstalar)" "Unknown action: $ACCION (use install, build or uninstall)" "Action inconnue : $ACCION (utilisez install, build ou uninstall)")" ;;
esac
