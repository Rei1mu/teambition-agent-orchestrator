; TBA 安装包脚本（Inno Setup 6）。
; 由 .github/workflows/release.yml 调用 iscc，用 /D 传入版本号与架构。
;
; 三条硬要求来自 README「安装包要求」一节，缺一个 agent 就找不到命令：
;   1. tba.exe 装到 {%USERPROFILE}\tba
;   2. 该目录写进用户 PATH（HKCU）
;   3. 卸载时从 PATH 移除，但不删 data/（token、台账、项目绑定都在里面）
; reload.ps1 等资源已 include_str! 编进 exe，不用单独装。

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
; 路径必须短且固定：换位置会出现两个 tba.exe，where tba 返回两条。
; Inno 没有 {userprofile} 这个常量，用户主目录要写成环境变量 {%USERPROFILE}。
DefaultDirName={%USERPROFILE}\tba
DefaultGroupName=TBA
DisableProgramGroupPage=yes
; 装到用户目录，不需要管理员权限，双击就能装。
PrivilegesRequired=lowest
; 改完 PATH 广播 WM_SETTINGCHANGE，新开的终端能直接用 tba。
ChangesEnvironment=yes
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
; 中文放第一个 = 安装界面默认中文，英文留给非中文用户。
; ChineseSimplified.isl 是官方仓库里的译文，跟着仓库走，不依赖 CI 环境。
Name: "zh"; MessagesFile: "ChineseSimplified.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

; 自定文案走 cm，两种语言各自正确。硬编码在 [Tasks] 里就只能显示一种。
[CustomMessages]
zh.TaskGroup=创建快捷方式
en.TaskGroup=Shortcuts
zh.TaskStartMenu=创建开始菜单快捷方式
en.TaskStartMenu=Create a Start Menu shortcut
zh.TaskDesktop=创建桌面快捷方式
en.TaskDesktop=Create a desktop shortcut
zh.IconUninstall=卸载 TBA
en.IconUninstall=Uninstall TBA
zh.RunApp=立即启动 TBA
en.RunApp=Launch TBA

[Tasks]
; 两个快捷方式都默认勾选，用户可以取消。GroupDescription 相同 -> 同一页同一组。
Name: "startmenuicon"; Description: "{cm:TaskStartMenu}"; GroupDescription: "{cm:TaskGroup}"
Name: "desktopicon"; Description: "{cm:TaskDesktop}"; GroupDescription: "{cm:TaskGroup}"

[Files]
Source: "{#SrcExe}"; DestDir: "{app}"; Flags: ignoreversion

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
  ValueData: "{olddata};{app}"; Check: NeedsAddPath('{app}')

[Icons]
; 卸载入口无条件创建：取消了开始菜单图标也得留一条卸载的路。
Name: "{group}\TBA"; Filename: "{app}\tba.exe"; Tasks: startmenuicon
Name: "{group}\{cm:IconUninstall}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\TBA"; Filename: "{app}\tba.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\tba.exe"; Description: "{cm:RunApp}"; Flags: nowait postinstall skipifsilent

[Code]
const
  EnvKey = 'Environment';

function NeedsAddPath(Param: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

// 卸载只摘 PATH，不动 {app} 下的 data/：token 和台账是用户数据，升级重装都靠它。
procedure RemoveFromPath(Dir: string);
var
  OrigPath, NewPath: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', OrigPath) then
    exit;
  NewPath := ';' + OrigPath + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(NewPath));
  if P = 0 then
    exit;
  // 只删「;目录」这两截，留住后面的分号，中间项也不会粘在一起。
  Delete(NewPath, P, Length(Dir) + 1);
  if (Length(NewPath) > 0) and (NewPath[1] = ';') then
    Delete(NewPath, 1, 1);
  if (Length(NewPath) > 0) and (NewPath[Length(NewPath)] = ';') then
    Delete(NewPath, Length(NewPath), 1);
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', NewPath);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
