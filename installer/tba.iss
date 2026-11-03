; TBA 安装包脚本（Inno Setup 6）。
; 由 .github/workflows/release.yml 调用 iscc，用 /D 传入版本号与架构。
;
; 三条硬要求来自 README「安装包要求」一节，缺一个 agent 就找不到命令：
;   1. tba.exe 装到安装目录（默认 {%USERPROFILE}\tba，安装页可改）
;   2. 该目录写进用户 PATH（HKCU；安装页有勾选项，默认勾上）
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
; 路径默认短且固定：{%USERPROFILE}\tba 是历史默认位置，where tba 只会有一条。
; 但不再强制——用户可以把 exe 装到别的盘。升级时 Inno 会沿用上次选的目录
; （UsePreviousAppDir），不会又跳回默认值。
; 数据目录默认跟 exe 同级的 data\；换目录后 tba 会回落到 %USERPROFILE%\tba\data，
; 所以不会像重装一样丢 token（见 src/config.rs 的 data_dir）。
DefaultDirName={%USERPROFILE}\tba
DefaultGroupName=TBA
DisableDirPage=no
UsePreviousAppDir=yes
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
zh.TaskPath=把 tba 加到 PATH（新开的终端能直接敲 tba）
en.TaskPath=Add tba to PATH (so `tba` works in new terminals)
zh.TaskStartMenu=创建开始菜单快捷方式
en.TaskStartMenu=Create a Start Menu shortcut
zh.TaskDesktop=创建桌面快捷方式
en.TaskDesktop=Create a desktop shortcut
zh.IconUninstall=卸载 TBA
en.IconUninstall=Uninstall TBA
zh.RunApp=立即启动 TBA
en.RunApp=Launch TBA

[Tasks]
; PATH 单独做成一项：默认勾上（不勾的话命令行敲 tba 会找不到），但用户可以取消。
; 取消时卸载逻辑会把它从 PATH 摘掉，不会留下一条指向不存在目录的路径。
Name: "addtopath"; Description: "{cm:TaskPath}"; GroupDescription: "{cm:TaskGroup}"
; 两个快捷方式都默认勾选，用户可以取消。GroupDescription 相同 -> 同一页同一组。
Name: "startmenuicon"; Description: "{cm:TaskStartMenu}"; GroupDescription: "{cm:TaskGroup}"
Name: "desktopicon"; Description: "{cm:TaskDesktop}"; GroupDescription: "{cm:TaskGroup}"

[Files]
Source: "{#SrcExe}"; DestDir: "{app}"; Flags: ignoreversion

; PATH 不再用 [Registry] 的 `{olddata};{app}`：那种写法只能靠 Check 函数判断
; 「加过了没有」，实测重复安装会写两遍（Check 参数里的 {app} 不展开，永远判成没加过）。
; 改成下面 Code 段里的 AddToPath：读出来、按 ';' 拆开、去掉重复项、再整体写回。

[Code]
const
  EnvKey = 'Environment';

var
  PathOut: string;

// 比较用的规范化：去首尾空白、去尾部反斜杠、转小写。
// 不用 RemoveBackslashUnlessRoot / CompareText：老版本 Inno 不一定有，
// 装不上比装重复更糟。
function NormDir(S: string): string;
begin
  Result := Trim(S);
  while (Length(Result) > 1) and (Result[Length(Result)] = '') do
    Delete(Result, Length(Result), 1);
  Result := Lowercase(Result);
end;

procedure PushItem(It: string);
begin
  if PathOut <> '' then
    PathOut := PathOut + ';';
  PathOut := PathOut + It;
end;

// 从 Rest 里取下一项（按 ';' 切），Rest 自己往后挪。Inno 的 Pascal Script
// 没有 SplitString、也不支持嵌套过程，只能手写。
function NextItem(var Rest: string): string;
var
  P: Integer;
begin
  P := Pos(';', Rest);
  if P = 0 then
  begin
    Result := Rest;
    Rest := '';
  end
  else
  begin
    Result := Copy(Rest, 1, P - 1);
    Rest := Copy(Rest, P + 1, Length(Rest));
  end;
end;

// 把 Dir 写进用户 PATH：
//   - 已经有了 -> 只保留第一份（顺手把历史遗留的重复项清掉）
//   - 没有     -> 追加到末尾
// 为什么不直接用 [Registry] 的 `ValueData: "{olddata};{app}"` + `Check:`：
// 重复安装会写两遍（Check 收到的参数没展开，永远认为没加过）。这里自己拼。
procedure AddToPath(Dir: string);
var
  OrigPath, Rest, Item: string;
  Found: Boolean;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', OrigPath) then
    OrigPath := '';
  PathOut := '';
  Found := False;
  Rest := OrigPath;
  while Rest <> '' do
  begin
    Item := Trim(NextItem(Rest));
    if Item <> '' then
    begin
      if NormDir(Item) = NormDir(Dir) then
      begin
        if not Found then
        begin
          Found := True;
          PushItem(Dir);          // 统一成 {app} 的写法，后面重复的全跳过
        end;
      end
      else
        PushItem(Item);
    end;
  end;
  if not Found then
    PushItem(Dir);
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', PathOut);
end;

// 卸载只摘 PATH，不动 {app} 下的 data/：token 和台账是用户数据，升级重装都靠它。
// 所有匹配项都摘掉（历史重复项一并清理），其余项顺序不变。
procedure RemoveFromPath(Dir: string);
var
  OrigPath, Rest, Item: string;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', OrigPath) then
    Exit;
  PathOut := '';
  Rest := OrigPath;
  while Rest <> '' do
  begin
    Item := Trim(NextItem(Rest));
    if (Item <> '') and (NormDir(Item) <> NormDir(Dir)) then
      PushItem(Item);
  end;
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', PathOut);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    // 用户取消了「加到 PATH」就明确摘掉：升级时改主意也能生效。
    if WizardIsTaskSelected('addtopath') then
      AddToPath(ExpandConstant('{app}'))
    else
      RemoveFromPath(ExpandConstant('{app}'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
