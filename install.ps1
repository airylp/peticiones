# peticiones para Windows: descarga el instalador de la última versión y lo abre.
#
#   [Net.ServicePointManager]::SecurityProtocol = 'Tls12'; irm https://raw.githubusercontent.com/airylp/peticiones/main/install.ps1 | iex
#
# El instalador lo crea .github/workflows/release.yml en GitHub Releases.
# Descargado así no lleva la marca «de Internet», así que Windows no muestra
# el aviso de SmartScreen.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest va mucho más rápido sin barra
# Windows PowerShell 5.1 no activa TLS 1.2 por omisión (GitHub lo exige).
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = if ($env:PETICIONES_REPO) { $env:PETICIONES_REPO } else { 'airylp/peticiones' }
$lang = (Get-UICulture).TwoLetterISOLanguageName
function m($es, $en, $fr) { switch ($lang) { 'es' { $es } 'fr' { $fr } default { $en } } }

Write-Host (m '==> Buscando la última versión de peticiones...' '==> Looking for the latest version of peticiones...' '==> Recherche de la dernière version de peticiones...')
# La más reciente aunque sea «pre-release» (releases/latest no las incluye).
$rels = Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$repo/releases?per_page=1"
$rel = $rels[0]
if (-not $rel) { throw (m "$repo aún no tiene versiones publicadas." "$repo has no published versions yet." "$repo n'a pas encore de version publiée.") }
$asset = $rel.assets | Where-Object { $_.name -like '*setup*.exe' } | Select-Object -First 1
if (-not $asset) {
    throw (m "La versión $($rel.tag_name) no tiene instalador para Windows." "Version $($rel.tag_name) has no Windows installer." "La version $($rel.tag_name) n'a pas d'installateur Windows.")
}

$file = Join-Path $env:TEMP $asset.name
Write-Host (m "==> Descargando $($asset.name)..." "==> Downloading $($asset.name)..." "==> Téléchargement de $($asset.name)...")
Invoke-WebRequest -UseBasicParsing $asset.browser_download_url -OutFile $file

Write-Host (m '==> Abriendo el instalador...' '==> Opening the installer...' "==> Ouverture de l'installateur...")
try {
    $p = Start-Process -FilePath $file -PassThru -Wait
    if ($p.ExitCode -eq 0) {
        Write-Host (m "==> peticiones $($rel.tag_name) instalado." "==> peticiones $($rel.tag_name) installed." "==> peticiones $($rel.tag_name) installé.")
    }
} finally {
    Remove-Item $file -ErrorAction SilentlyContinue
}
