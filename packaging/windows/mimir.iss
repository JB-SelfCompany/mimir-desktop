; Mimir per-user installer (Inno Setup 6). Built by .github/workflows/release.yml:
;   ISCC /DVersion=1.0.0 /DTag=v1.0.0 /DSourceDir=<dir with mimir-desktop.exe, ffmpeg.exe, licenses> packaging\windows\mimir.iss
; The mimir:// keys and the AppUserModelID match what src/platform/mod.rs writes at runtime; uninstall removes them.
; The user's data dir is not touched.
#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Tag
  #define Tag "v" + Version
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\mimir-desktop"
#endif

[Setup]
AppId=Mimir.Desktop
AppName=Mimir
AppVersion={#Version}
AppPublisher=JB-SelfCompany
AppPublisherURL=https://github.com/JB-SelfCompany/mimir-desktop
VersionInfoVersion={#Version}
PrivilegesRequired=lowest
DefaultDirName={userpf}\Mimir
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=mimir.ico
UninstallDisplayIcon={app}\mimir.ico
UninstallDisplayName=Mimir
OutputDir=.
OutputBaseFilename=mimir-desktop-{#Tag}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs
Source: "mimir.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; The AppUserModelID is the one our toasts use (platform::notify::APP_ID): Windows shows them as «Mimir».
Name: "{userprograms}\Mimir"; Filename: "{app}\mimir-desktop.exe"; IconFilename: "{app}\mimir.ico"; AppUserModelID: "Mimir.Desktop"

[Registry]
Root: HKCU; Subkey: "Software\Classes\mimir"; ValueType: string; ValueData: "URL:Mimir link"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\mimir"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\mimir\DefaultIcon"; ValueType: string; ValueData: """{app}\mimir-desktop.exe"",0"
Root: HKCU; Subkey: "Software\Classes\mimir\shell\open\command"; ValueType: string; ValueData: """{app}\mimir-desktop.exe"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\Mimir.Desktop"; ValueType: string; ValueName: "DisplayName"; ValueData: "Mimir"; Flags: uninsdeletekey

[Run]
Filename: "{app}\mimir-desktop.exe"; Description: "{cm:LaunchProgram,Mimir}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Left behind by the self-updater (src/platform/update.rs).
Type: files; Name: "{app}\mimir-desktop.exe.old"
Type: files; Name: "{app}\mimir-desktop.exe.new"
Type: files; Name: "{app}\mimir-desktop.exe.download"
