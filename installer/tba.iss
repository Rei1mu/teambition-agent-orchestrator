; TBA 安装包脚本（Inno Setup 6）。
; 由 .github/workflows/release.yml 调用 iscc，用 /D 传入版本号与架构。
; 只装 tba.exe：reload.ps1 等资源已 include_str! 编进 exe。

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef TargetArch
  #define TargetArch "x64"
#endif
#ifndef InnoArch
  #define InnoArch "x64compatible"
#endif
#ifndef SrcExe
  #define SrcExe "..\target\release\tba.exe"
#endif
#ifndef OutDir
  #define OutDir "..\dist"
#endif

[Setup]
; AppId 固定不变，升级时才能覆盖安装而不是装成两份。
AppId={{8F3A6C1E-4B27-4C9A-9E51-2D7B0C5A1F83}
AppName=TBA
AppVerName=TBA {#AppVersion}
AppVersion={#AppVersion}
AppPublisher=Rei1mu
AppPublisherURL=https://github.com/Rei1mu/teambition-agent-orchestrator
DefaultDirName={localappdata}\Programs\TBA
DefaultGroupName=TBA
DisableProgramGroupPage=yes
; 装到用户目录，不需要管理员权限，双击就能装。
PrivilegesRequired=lowest
OutputDir={#OutDir}
OutputBaseFilename=tba-{#AppVersion}-{#TargetArch}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed={#InnoArch}
ArchitecturesInstallIn64BitMode={#InnoArch}
UninstallDisplayIcon={app}\tba.exe
MinVersion=10.0

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional tasks:"

[Files]
Source: "{#SrcExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\TBA"; Filename: "{app}\tba.exe"
Name: "{group}\Uninstall TBA"; Filename: "{uninstallexe}"
Name: "{autodesktop}\TBA"; Filename: "{app}\tba.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\tba.exe"; Description: "Launch TBA"; Flags: nowait postinstall skipifsilent
