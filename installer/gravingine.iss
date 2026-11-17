; Inno Setup script for the Gravingine Image Optimizer installer.
; Built by scripts/build-release.sh, which passes:
;   /DAppVersion=<version from Cargo.toml>
;   /DSourceDir=<folder with gravingine-image-optimizer.exe, README.md, LICENSE.txt>
;   /DSetupIcon=<the app's .ico>   /DOutputDir=<dist folder>
;
; Installs for the current user only (no admin, no UAC prompt): into
; %LOCALAPPDATA%\Programs\Gravingine Image Optimizer, with a Start menu shortcut, an optional
; desktop shortcut, and an entry in Settings > Apps for uninstalling.

#ifndef AppVersion
  #error AppVersion must be defined (/DAppVersion=x.y.z)
#endif

#define AppName "Gravingine Image Optimizer"
#define AppExe "gravingine-image-optimizer.exe"
#define Repo "https://github.com/srisar/gravingine-image-optimizer"

[Setup]
; Never change AppId: Windows uses it to recognise upgrades and uninstalls of this app.
AppId={{5EB01B13-A43C-4AC9-A690-BBB63DBDAC27}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Gravitide Pvt Ltd
AppPublisherURL=https://gravitide.dev
AppSupportURL={#Repo}
AppUpdatesURL={#Repo}/releases
AppCopyright=Copyright (c) 2026 Gravitide Pvt Ltd
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}

; Per-user install: {autopf} is %LOCALAPPDATA%\Programs without admin rights.
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0

; Close a running copy before upgrading, and start it again afterwards.
CloseApplications=yes
RestartApplications=yes

SetupIconFile={#SetupIcon}
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=gravingine-image-optimizer-v{#AppVersion}-windows-x64-setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
