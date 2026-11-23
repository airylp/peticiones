#!/usr/bin/env sh
# peticiones: instalar, actualizar o desinstalar con un solo comando.
#
#     curl -fsSL https://raw.githubusercontent.com/airylp/peticiones/main/install.sh | sh
#
#   Pregunta qué hacer: si ya está instalado ofrece actualizar o desinstalar; si
#   no, instalarlo. También se puede indicar directamente (sin preguntas):
#     … | sh -s -- instalar        (instalar o actualizar)
#     … | sh -s -- desinstalar     (pregunta si borrar también la configuración)
#
#   Descarga la última versión publicada en GitHub Releases (no compila nada):
#   macOS: Peticiones.app en /Applications (o ~/Applications sin permiso).
#   Linux: ~/.local/bin/peticiones y entrada en el menú de aplicaciones.
#
# Variables opcionales: PETICIONES_REPO (usuario/repo), PETICIONES_VERSION (tag,
#   p. ej. v0.1.0; por defecto la más reciente), PETICIONES_DESTINO (carpeta de
#   instalación, para pruebas), PETICIONES_IDIOMA (es|en|fr).
set -eu

REPO="${PETICIONES_REPO:-airylp/peticiones}"
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
    PLATAFORMA="macos-universal"
    EXT="zip"
elif [ "$OS" = "Linux" ]; then
    BIN_DIR="${PETICIONES_DESTINO:-$HOME/.local/bin}"
    DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
    CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/peticiones"
    case "$(uname -m)" in
        x86_64 | amd64) PLATAFORMA="linux-x86_64" ;;
        *) fail "$(m "Por ahora solo hay versión de Linux para x86_64 (este equipo: $(uname -m))." "For now there is only a Linux build for x86_64 (this machine: $(uname -m))." "Pour l'instant il n'y a qu'une version Linux x86_64 (cette machine : $(uname -m)).")" ;;
    esac
    EXT="tar.gz"
else
    fail "$(m 'En Windows descarga el .zip de https://github.com/airylp/peticiones/releases' 'On Windows download the .zip from https://github.com/airylp/peticiones/releases' 'Sous Windows, téléchargez le .zip depuis https://github.com/airylp/peticiones/releases')"
fi
MARCA="$CONFIG/version-instalada"

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

# ------------------------------------------------------------ instalar
instalar() {
    command -v curl >/dev/null 2>&1 || fail "$(m 'Hace falta curl.' 'curl is required.' 'curl est nécessaire.')"
    tag="$(version_remota)"
    [ -n "$tag" ] || fail "$(m "No se encontró ninguna versión publicada en https://github.com/$REPO/releases" "No published version found at https://github.com/$REPO/releases" "Aucune version publiée sur https://github.com/$REPO/releases")"
    if [ "$(cat "$MARCA" 2>/dev/null || true)" = "$tag" ] && instalado >/dev/null && [ "$ACCION" != "instalar" ]; then
        info "$(m "Ya tienes la versión más reciente ($tag)." "You already have the latest version ($tag)." "Vous avez déjà la dernière version ($tag).")"
        return 0
    fi
    archivo="peticiones-${tag#v}-$PLATAFORMA.$EXT"
    url="https://github.com/$REPO/releases/download/$tag/$archivo"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    info "$(m "Descargando $archivo…" "Downloading $archivo…" "Téléchargement de $archivo…")"
    curl -fL --progress-bar -o "$tmp/$archivo" "$url" || fail "$(m "No se pudo descargar $url" "Could not download $url" "Impossible de télécharger $url")"

    if [ "$OS" = "Darwin" ]; then
        ditto -x -k "$tmp/$archivo" "$tmp/x"
        [ -d "$tmp/x/Peticiones.app" ] || fail "$(m 'El archivo descargado no contiene Peticiones.app' 'The download does not contain Peticiones.app' "Le téléchargement ne contient pas Peticiones.app")"
        # Sin firma de Apple: se quita la cuarentena para que abra sin avisos.
        xattr -dr com.apple.quarantine "$tmp/x/Peticiones.app" 2>/dev/null || true
        destino=""
        for d in $APP_DIRS; do
            mkdir -p "$d" 2>/dev/null || true
            if [ -w "$d" ]; then destino="$d"; break; fi
        done
        [ -n "$destino" ] || fail "$(m "No hay permiso para escribir en: $APP_DIRS" "No permission to write to: $APP_DIRS" "Pas de permission d'écriture dans : $APP_DIRS")"
        abierta=0
        if pgrep -x peticiones >/dev/null 2>&1; then
            abierta=1
            osascript -e 'quit app "Peticiones"' >/dev/null 2>&1 || pkill -x peticiones || true
            sleep 1
        fi
        rm -rf "$destino/Peticiones.app"
        mv "$tmp/x/Peticiones.app" "$destino/Peticiones.app"
        /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$destino/Peticiones.app" 2>/dev/null || true
        ruta="$destino/Peticiones.app"
        [ "$abierta" = 1 ] && open "$ruta" || true
    else
        tar -xzf "$tmp/$archivo" -C "$tmp"
        mkdir -p "$BIN_DIR"
        # Copia + rename: funciona aunque la app esté abierta.
        cp "$tmp/peticiones-${tag#v}-$PLATAFORMA/peticiones" "$BIN_DIR/.peticiones.nuevo"
        chmod +x "$BIN_DIR/.peticiones.nuevo"
        mv "$BIN_DIR/.peticiones.nuevo" "$BIN_DIR/peticiones"
        ruta="$BIN_DIR/peticiones"
        if [ -z "${PETICIONES_DESTINO:-}" ]; then
            mkdir -p "$DATA/applications"
            cat > "$DATA/applications/peticiones.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Peticiones
Comment=$(m 'Cliente de peticiones HTTP y SOAP' 'HTTP and SOAP request client' 'Client de requêtes HTTP et SOAP')
Exec=$ruta
Icon=applications-internet
Terminal=false
Categories=Development;Network;
StartupWMClass=peticiones
DESKTOP
            command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$DATA/applications" 2>/dev/null || true
        fi
        case ":$PATH:" in *":$BIN_DIR:"*) ;; *) info "$(m "Añade $BIN_DIR a tu PATH para abrirlo con «peticiones»." "Add $BIN_DIR to your PATH to run it as “peticiones”." "Ajoutez $BIN_DIR à votre PATH pour le lancer avec «peticiones».")" ;; esac
    fi
    mkdir -p "$CONFIG"
    echo "$tag" > "$MARCA"
    guardar_idioma
    info "$(m "Instalado $tag en $ruta" "Installed $tag in $ruta" "$tag installé dans $ruta")"
}

# ------------------------------------------------------------ menú
case "$ACCION" in
    instalar | install | actualizar | update) instalar ;;
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
    *) fail "$(m "Acción desconocida: $ACCION (usa instalar o desinstalar)" "Unknown action: $ACCION (use install or uninstall)" "Action inconnue : $ACCION (utilisez install ou uninstall)")" ;;
esac
