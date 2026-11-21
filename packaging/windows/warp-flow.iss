; Inno Setup 6 script — per-user installer for Warp Flow.
; build.ps1 passes /DMyAppVersion=<version from version.toml>; a manual
; `ISCC warp-flow.iss` must pass it too, so there is no stale fallback here.

#ifndef MyAppVersion
  #error MyAppVersion is required: pass /DMyAppVersion=<version from version.toml>
#endif

#define MyAppName "Warp Flow"
#define MyAppExeName "warp-flow.exe"

#ifndef MySourceDir
#define MySourceDir "out"
#endif
#ifndef MyOutputDir
#define MyOutputDir "out"
#endif

[Setup]
AppId={{B4F0D6A2-9C3E-4A7B-8E15-7D2C9F6A1B30}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#MyOutputDir}
OutputBaseFilename=warp-flow-{#MyAppVersion}-setup-x64
SetupIconFile=warp-flow.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2
SolidCompression=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
    GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#MySourceDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; \
    Flags: nowait postinstall skipifsilent
