; Inno Setup script. Build with: ISCC /DAppVersion=<version> packaging\windows.iss
; Expects target\release\claude-code-ln.exe; writes the installer to dist\.
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "Claude Code Session Links"

[Setup]
AppId={{8E0B6C1E-6C2B-4E7B-9B0E-3D2B7C1A5F10}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=XDflight
AppPublisherURL=https://github.com/XDflight/claude-code-ln
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
; Per-user install: no administrator rights needed.
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=claude-code-ln-{#AppVersion}-windows-x64-setup
SetupIconFile=icon.ico
UninstallDisplayIcon={app}\icon.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "..\target\release\claude-code-ln.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "icon.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\claude-code-ln.exe"; IconFilename: "{app}\icon.ico"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\claude-code-ln.exe"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\claude-code-ln.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
