; Instalador de peticiones para Windows (Inno Setup 6).
; Lo compila .github/workflows/release.yml:  iscc /DAppVersion=0.2.0 windows\peticiones.iss
; Necesita target\release\peticiones.exe ya compilado.
;
; Se instala para el usuario (sin pedir permisos de administrador), en
; %LOCALAPPDATA%\Programs\peticiones.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
; Identificador fijo: con él Windows reconoce las actualizaciones y el desinstalador.
AppId={{84019047-07A4-4084-BF80-A734DACBFEC7}
AppName=peticiones
AppVersion={#AppVersion}
AppVerName=peticiones {#AppVersion}
AppPublisher=airylp
AppPublisherURL=https://github.com/airylp/peticiones
AppSupportURL=https://github.com/airylp/peticiones/issues
AppUpdatesURL=https://github.com/airylp/peticiones/releases
DefaultDirName={autopf}\peticiones
DefaultGroupName=peticiones
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\target\instalador
OutputBaseFilename=peticiones-{#AppVersion}-setup
SetupIconFile=peticiones.ico
UninstallDisplayIcon={app}\peticiones.exe
UninstallDisplayName=peticiones
LicenseFile=..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Si peticiones está abierto, el instalador ofrece cerrarlo (Restart Manager).
CloseApplications=yes
RestartApplications=no
ShowLanguageDialog=auto

[Languages]
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\peticiones.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\peticiones"; Filename: "{app}\peticiones.exe"
Name: "{autodesktop}\peticiones"; Filename: "{app}\peticiones.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\peticiones.exe"; Description: "{cm:LaunchProgram,peticiones}"; Flags: nowait postinstall skipifsilent

[Code]
// Guarda el idioma elegido en el instalador como idioma de la app (si aún no
// hay uno elegido), igual que install.sh en macOS/Linux: `idioma = "xx"` al
// principio de sesion.toml.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Dir, F, Rest: String;
  Lines: TArrayOfString;
  I: Integer;
  Has: Boolean;
begin
  if CurStep <> ssPostInstall then
    Exit;
  Dir := ExpandConstant('{userappdata}\peticiones');
  F := Dir + '\sesion.toml';
  Has := False;
  Rest := '';
  if LoadStringsFromFile(F, Lines) then
    for I := 0 to GetArrayLength(Lines) - 1 do
    begin
      if Pos('idioma', Trim(Lines[I])) = 1 then
        Has := True;
      Rest := Rest + Lines[I] + #13#10;
    end;
  if not Has then
  begin
    ForceDirectories(Dir);
    SaveStringToFile(F, 'idioma = "' + ActiveLanguage + '"' + #13#10 + Rest, False);
  end;
end;
