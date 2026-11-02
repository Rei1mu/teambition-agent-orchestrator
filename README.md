# TBA

## 项目目标

**让同一个项目在任何设备、任何 agent 上都能无缝继续开发，同时把 token 开销压到极致，并保证 agent 不会跑错关键流程。**

要解决的是四件事：

1. **多设备、不同 agent 无缝续作**
   项目背景（skill）和进展（记忆）放在 Teambition 和本地 `data/`，不绑定任何一台机器、任何一个 agent。换设备、换 agent，挂上同一个项目就能从上次的进度接着干，上下文跟着项目走，而不是跟着工具走。

2. **极致减少 token 开销**
   agent 不必每次通读仓库、翻聊天记录来重建背景。`tba prompt` 只把该项目的启用 skill、共享记忆和要领的任务打包成一段文案，token 全花在真正干活的地方。

3. **不让 agent 跑错关键流程**
   关键流程不能只写成文字。文字 skill 只告诉 agent「做什么」，管不住它「怎么做」。比如「重载 _new.exe 做测试」这件事：仅靠文字描述，agent 可能因为权限、沙盒、文件占用，杀不掉也覆盖不了 _new.exe，甚至自作主张开了个 _new_text01.exe；skill 没读完整，它就不重启；脚本和 bash 环境有差异，它每次还换着花样执行。
   所以易错的、依赖环境的、有严格顺序的关键流程，要收进命令里，而不是留在文字里。agent 只负责调用；先做什么、后做什么、失败怎么处理、做完怎么校验，由命令保证——同样的输入，同样的结果，不靠 agent 的自觉。

4. **流程化、指标化**
   每个项目固定五个阶段（工作流状态）：`未完成` → `进行中` → `待回复` → `已解决` → `已完成`。需求进来进「未完成」，agent 领了进「进行中」，需要你拍板时进「待回复」，做完进「已解决」，不做进「已完成」。另有两个用途标记状态 `skill` / `mem`，不占阶段；`tba claim` 只查「未完成」，领不到它们。agent 用 `tba claim` 领取创建时间最早的未完成任务，同一任务重复领返回原 claim_id，先记本地台账再移动状态——不重复、不跳步、不撞车。状态集中、口径统一，谁在做什么、积压多少、完成多少，看板上一眼可查：项目管理不靠嘴说，看数字。

## 它是什么

一个 Rust 二进制，三种用法：

- `tba` CLI：命令行。
- `tba serve`：MCP server（stdio）。任何支持 MCP 的 agent 挂载。
- `tba ui`：egui 窗口。卡片列项目，展开看 skill 和开关，复制 agent 文案。

数据放在 exe 同目录的 `data/`。所有 agent 共享这一个目录。

## 安装包要求（GitHub Actions 打包用）

**产物**：推 `v*` tag 触发 `.github/workflows/release.yml`，两个包挂到 Release：

| 文件 | target | 适用 |
|---|---|---|
| `tba-<版本>-x64-setup.exe` | `x86_64-pc-windows-msvc` | 普通 Windows 10/11 |
| `tba-<版本>-arm64-setup.exe` | `aarch64-pc-windows-msvc` | 骁龙本等 Windows on ARM |

两个都在 `windows-latest` 上用 MSVC 工具链构建（arm64 走交叉编译，VS 自带 ARM64 工具），
Inno Setup 打包，脚本在 `installer/tba.iss`。手动触发 workflow 只出 artifact，不发 Release。

**界面语言**：中英双语，默认中文。中文用 `installer/ChineseSimplified.isl`（跟着仓库走，不依赖 CI 环境），
英文用 Inno 自带的 `Default.isl`。任务描述走 `{cm:...}` 自定消息，两种语言各自正确。
`.iss` 带 UTF-8 BOM，否则中文会按 ANSI 解析成乱码。

安装时做三件事，缺一个 agent 就找不到命令：

1. 把 `tba.exe` 放到安装目录。
2. 把安装目录写进**用户 PATH**。
3. `data/` 不预置，首次运行自动生成（exe 同目录）。

**安装目录约定：`C:\Users\<用户名>\tba`**（Inno Setup 里写 `{%USERPROFILE}\tba`；没有 `{userprofile}` 这个常量）。

跨设备统一用这个位置，好处是：路径短、不需要管理员权限、`tba` 这个名字和目录名一致。
别装到 `%LOCALAPPDATA%\Programs\tba` 或别的地方——两个位置都进 PATH 会出现两个 `tba.exe`，
`where tba` 返回两条，agent 和人都分不清跑的是哪一个。

**快捷方式**：安装页有「Shortcuts」一组，两项都默认勾选，可以取消：

| 项 | 位置 | 可取消 |
|---|---|---|
| 开始菜单快捷方式 | `%APPDATA%\Microsoft\Windows\Start Menu\Programs\TBA\TBA.lnk` | 是 |
| 桌面快捷方式 | `%USERPROFILE%\Desktop\TBA.lnk` | 是 |
| 卸载入口 | 同上开始菜单目录 `Uninstall TBA.lnk` | 否，始终创建 |

卸载入口不跟着快捷方式走：取消了开始菜单图标，也得留一条卸载的路。

**PATH 写入（Inno Setup 示例）**

```ini
[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; \
  ValueData: "{olddata};{app}"; Check: NeedsAddPath('{app}')

[Code]
function NeedsAddPath(Param: string): Boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', OrigPath) then
  begin
    Result := True; exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;
```

**卸载时**从 PATH 移除同一路径，不删 `data/`（里面是 token、台账、项目绑定）。

**打包后校验**

```powershell
$env:PATH -split ';' -contains "$env:USERPROFILE\tba"
& "$env:USERPROFILE\tba\tba.exe" --version
```

**环境变量**（全部可选，都有默认值）

| 变量 | 作用 |
|---|---|
| `TBA_AGENT` | 会话标识，覆盖 `.tba/session` |
| `TBA_SKIP_GITBUTLER` | 跳过 GitButler 检查（排查用） |
| `TBA_MSVC_BIN` / `TBA_MSVC_LIB` / `TBA_SDK_LIB` | 重载能力的工具链路径 |
| `TBA_EXTRA_LIB` / `TBA_CARGO` | 同上，附加库目录 / cargo 路径 |

安装包**不写 token**。token 由 `tba login` 写进 `data/config.toml`，属于用户数据，升级时不要覆盖。

## 前置依赖：GitButler（优先级高于 Teambition）

TBA 每次启动**先过 GitButler 这一关**，再连 Teambition——agent 靠 GitButler CLI（`but`）管分支和提交，缺它后面全白跑。

```bat
tba gitbutler status     :: 看 CLI 装没装、什么版本、当前目录配没配
tba gitbutler install    :: 自动装到运行目录 data/bin（无需管理员）
tba gitbutler path       :: 打印 CLI 路径（脚本用）
```

- **检测顺序**：运行目录 `data/bin` → PATH → GitButler 桌面安装目录。装到运行目录不抢用户自己装的版本，删 `data/bin` 即卸载。
- **Windows 官方只发 MSI**（没有独立 CLI 二进制，官方 `curl -fsSL https://gitbutler.com/install.sh | sh` 也只支持 mac/linux）。自动安装走：下载 MSI zip → `Expand-Archive` 解压 → `msiexec /a /qn TARGETDIR=…` 管理抽取 → 拷贝 `but.exe` → `but --version` 验活。**不需要 UAC、不动注册表**，实测 10 秒左右装完。
- **git 是系统级依赖**，TBA 不自动装（Git for Windows 装 Program Files 要提权），检测到缺失时引导去 <https://git-scm.com/download/win>。
- 缺 GitButler/git 时，任何 `tba` 命令都会先打印引导再退出；排查问题可用 `TBA_SKIP_GITBUTLER=1` 强制跳过。
- 窗口里 CentralPanel 置顶有一张 GitButler 状态卡（版本/来源/工作区配置状态 + 一键安装），同样优先于项目列表。
- 当前目录如果是普通 git 仓库（没 `but setup` 过），状态卡会提示「未配置」；跑一次 `but setup` 即可纳入 GitButler 管理。

## 快速开始

第一次使用要先拿 Teambition 访问令牌。

**方式一：窗口（推荐）**

```bat
tba ui
```

没有令牌时窗口会显示引导面板：点「① 打开 Teambition 创建 Token」会自动打开浏览器，
把「查看详情」里复制的 Token ID 粘贴进输入框，点「③ 保存并连接」。

**方式二：命令行**

```bat
tba login                :: 自动打开 Token 页面并打印步骤
tba login --token <令牌>  :: 已有令牌直接登录
```

**怎么拿令牌**

令牌在 Teambition 的 User Token 页面创建：<https://open.teambition.com/user-mcp>

1. 用钉钉扫码登录 Teambition
2. 点「创建 Token」按钮
3. 填 Token 名称、有效期（建议选 1 年）、选择使用企业
4. 点确认，再点「查看详情」复制 Token ID

拿到之后：

```bat
tba login --token <你的Teambition访问令牌>
tba tools                :: 看 MCP 工具列表和角色识别结果
tba projects             :: 列项目
tba init --project <id>  :: 补齐 5 个阶段
```

token 存在 `data/config.toml`，登一次就行，重开、换 CLI/窗口/MCP 都不用再登。

**查看/切换配置**

```bat
tba config               :: 看当前配置（token 打码显示）
tba config --url <地址>   :: 切换 MCP 服务地址，token 不动
```

本地测试切到 mock 服务就用 `tba config --url http://127.0.0.1:8791/mcp`，不用手改文件，
已登录的 token 不会被覆盖。切回去：`tba config --url https://open.teambition.com/api/mcp/v2`。

每个项目必须有 7 个工作流状态：5 个阶段 + 2 个用途标记。

阶段（按生命周期顺序）：

`未完成` → `进行中` → `待回复` → `已解决` → `已完成`

用途标记：`skill`、`mem`。两者都是 kind=unset，不参与生命周期，只用来区分任务性质。

`tba init` 缺哪个建哪个。幂等。

首尾两个名字刻意对齐 **Teambition 新建项目自带的默认状态**（未完成 / 已完成）：
默认项目本来就有这两个，沿用就不用新建，也不会出现「未完成」和「待处理」两条并存的局面。

旧项目如果还留着改名前的那两个（「待处理」「关闭」），tba 不检测、不迁移、不提示，自己去网页处理。

> 注意：看板列（任务列表）无法通过 API 新建或改名（只有查询接口），需要的话在 Teambition 网页手动维护。

## skill

工作流状态是 `skill` 的任务就是一条 skill。标题 = skill 名，内容 = skill 正文。
`tba claim` 只查「未完成」状态，skill / mem 根本不在查询范围内，不会被领走。

```bat
tba skill-push --project <id> --file SKILL.md   :: 新建或更新（同名覆盖）
tba skills --project <id>                       :: 列出名字 + 开关 + 任务 id
tba skill <名字> --project <id>                 :: 打印单个 skill 全文（agent 文案按需读它）
tba skill-rm --project <id> --name <名>         :: 删除（归档那条任务）
tba switch --project <id> --skill <名> --on
tba switch --project <id> --skill <名> --off
tba skill-tpl                                   :: 看内置模板有哪些（不联网）
tba skill-init --project <id>                   :: 推全部内置模板
tba skill-init --project <id> --name shot       :: 只推一个
```

开关只存本地 `data/switches.toml`。不回写 Teambition。

**GUI 里能编辑**：项目卡片展开后，每条 skill 后面有「编辑」「删除」和开关勾选框，
底下有「+ 新建 skill」。编辑窗写名字 + markdown 正文，保存即推到 Teambition（同名覆盖）。
删除 = 归档那条任务。

**内置模板**（`tba skill-init` 推上去，也可以自己在 GUI 里改）：

| 名字 | 一句话 | 内容 |
|---|---|---|
| `shot` | 截图 | `tba shot` 一条命令出图，说明压缩级别、禁止自己写 PowerShell |
| `git-main` | 提交并推送 | `tba git status` / `tba git main -m`，GitButler 项目自动走 `but` |
| `release` | 发版 | `tba publish --init` 生成 workflow，`--tag --watch` 打 tag 等 CI |
| `gui-notes` | 写 GUI 的注意事项 | 中文用黑体、字体要可选、防豆腐块、字号用 DragValue、改完必须截图自查 |

**省 token 的用法**：开工文案里只列 skill 名字（一行一个），agent 判断要用哪个才去
`tba skill <名字>` 读全文。所以正文可以写长，但名字要短。

## 截图

```bat
tba shot --out shot.png                        :: 全屏（默认 fast 压缩）
tba shot --title "记事本" --out n.png          :: 按窗口标题（包含匹配）
tba shot --hwnd 123456 --out n.png             :: 直接给句柄
tba shot --level high --out n.png              :: 更小（更慢），仍然无损
```

- BitBlt 抓屏，带 `CAPTUREBLT`，**后台窗口也能截**，不用把窗口提到前台。
- 指定窗口时先 `PrintWindow(PW_RENDERFULLCONTENT)`，失败回落窗口 DC。
- 编码走 `png` crate + 自适应行滤波 + `fdeflate`；不透明截图自动丢 alpha 存 RGB。
  实测 1920x1080 桌面：8 MB → **465 KB（fast，约 1.7 秒）** / **308 KB（high）**。
  两个级别都是无损，`fast` 已经是「极致压缩 + 快」的甜点。
- 目录不存在会自动建。

## 接单 / 提问 / 等回复 / 收尾

任务在两个维度上表示：**状态（工作流状态）表示"是否在执行"，看板列表示"归属"**。

| 局面 | 状态 | 看板列 | 球在谁那 | 算完成 |
|---|---|---|---|---|
| 队列中，没人接 | 未完成 | 1 未完成 | — | 否 |
| agent 正在干 | 进行中 | 2 进行中 | agent | 否 |
| **agent 等你回复** | **待回复** | **3 待回复** | **你** | 否 |
| 干完 | 已解决 | 4 已解决 | — | 是 |
| 作废 | 已完成 | 5 已完成 | — | 是 |
| **会话异常断** | 未完成 | 1 未完成 | 回到队列 | 否 |

`kind`（状态类型）分配：未完成=`start`（全任务流唯一）、进行中/待回复=`unset`、已解决/已完成=`end`。
**待回复是 `unset` 而不是 `end`**——球在你那不等于干完了，否则完成率会虚高。

```bat
tba start [--project <id>] --file <md> :: 开工：新建「进行中」任务并放进列「进行中」（文案里带了活时用）
tba take <链接|task_id>             :: 接单：进行中 + 列「进行中」
tba claim [--project <id>]         :: 从队列(列「未完成」)领最早的一条。省略 --project 走 .tba/project
tba note <claim_id> --text "..."    :: 追加到任务备注（agent 的唯一输出通道，评论留给用户）
tba ask <claim_id> -q "..."         :: 提问：问题写进备注 + 待回复 + 列「待回复」+ 发邮件 + 就地阻塞等回复
tba ask <claim_id> --done           :: 同上，用标准三选项（0 收尾 / 1 先写记忆再收尾 / 2 还有问题）
tba ask <claim_id> --done --no-wait :: 只问不等，立刻返回（一般不用，会话会断）
tba wait <claim_id>                 :: 单独等回复：ask 超时后重跑它，或只等外部收尾。没 ask 过会自动补一次
tba reply <claim_id>                :: 只看一眼：读回评论 → 进行中 + 列「进行中」（不阻塞）
tba inbox                           :: 列出所有待回复：谁在等、等了多久、问了什么、任务链接
tba hello [--name <tag>]            :: 登记会话标识（多会话并行时必须）
tba beat <claim_id>                 :: 续命（长构建/长测试前后调）
tba done <claim_id>                 :: 已解决 + 列「已解决」
tba close <claim_id>                :: 关闭（→ 已完成）+ 列「已完成」
tba release <claim_id> --reason "…"  :: 主动放弃：未完成 + 列「未完成」
tba show <链接|task_id>              :: 查一条任务真实的状态 + 看板列 + 是否完成
tba status                          :: 台账（含心跳年龄）
tba sweep                           :: 手动回收心跳超时的会话
```

规则：

1. `tba claim` 只领「**列「未完成」+ 状态未完成**」，并跳过台账里还被占着的任务。
2. 领任务自动跳过 `skill` 任务类型。
3. 先写本地台账，再改线上。改状态失败则台账标记 failed。领任务全程持文件锁，多会话并发不会抢到同一条。
4. **做完必须 `ask --done`，不能停在进行中**——`ask` 自带阻塞等回复，会话不会断。终结于 `ask` / `done` / `close` 之一。
5. 看板列名在 `data/config.toml` 里配（`tasklist_queue/claim/wait/done/closed`，默认**中文列名**）。
   列名不存在时只跳过移列、不阻断流程——状态才是硬约束。
   > 默认值从 `1/2/3/4` 改成了中文，是因为用数字在只有 4 列的旧项目上会把「已解决」错移进「已完成」列；
   > 中文名对不上只会「跳过移列」，安全得多。旧项目要么把网页列改成中文名，要么在 config 里显式写数字。
6. `take` / `start` 打印的「看板列: X -> Y」是**移完回读的真实列**，不是配置里的期望值。
   `tba start` 新建的任务由服务端落在「未分类」，所以 X 可能是「未分类」。

### 输出通道：备注给 agent，评论给你

两个通道，各管一边，不混用：

| 通道 | 谁写 | 内容 |
|---|---|---|
| **任务备注**（`note` 字段） | agent | 过程、结论、提问。`tba note` 往后追加，带时间戳分隔，不覆盖你写的原始需求 |
| **任务评论** | 只有你 | 回复数字、补充说明。agent 一律不写评论 |

为什么这么分：

1. 一个 agent 一条任务，备注不存在多个 agent 并发覆盖的问题。
2. 评论里只有你的话，`ask` 的等待不用再靠「内容是不是纯数字」去猜某条是谁发的。
3. 备注在网页任务详情第一屏可见，结论不会被评论淹没。

### 同步等回复：`tba ask` 自带阻塞

用户可能在外面，只在 Teambition 上回复，不会回到会话里发一句「继续」。所以 `ask` **问完就地阻塞**，
不返回——agent 只要跑了 `ask`，就卡在那里等，没有「问完忘了等」的中间态，会话也就不会断。

```bat
tba ask  <claim_id> -q "问题"     :: 问题写进备注 + 状态 → 待回复 + 列「待回复」+ 发邮件 + 阻塞等
tba ask  <claim_id> --done        :: 同上，用标准三选项
tba wait <claim_id>               :: 单独等（ask 超时后重跑它，或只等外部收尾）
```

- 单轮默认 **540 秒**主动返回（Bash 工具上限 600 秒，超了会被转后台、agent 拿不到结果）。
- 退出码：`0` 读到回复（照回复继续干）/ `2` 本轮没等到（**重跑同一条**接着等，别结束会话）/ `3` 任务在别处被收尾了。
- **为什么必须阻塞**：WorkBuddy 会话断了要手动点，或等一小时自动循环。所以 agent 不能自作主张结束会话。
- **`wait` 自愈**：如果手动跑了 `wait` 而没 `ask` 过，tb 自动补一次（问题写备注 + 转「待回复」+ 发邮件）。
  配置项 `wait_auto_ask`（默认开）可关；关掉后 `wait` 只等外部收尾，不读评论。
- 「待回复」永不参与心跳超时回收，所以挂多久都安全。
- 你回完之后**不需要在会话里再说任何话**，agent 自己会继续。
- **连 `ask` 都没跑就断了** → 300 秒后静默退回「未完成」+ 列1。不发信、不进「待回复」、`tba inbox` 看不到。
  这是故意的：没交付就不该占着任务，也不该拿假邮件打扰你。

#### 禁止把 `ask` 丢后台

`run_in_background` 是这个流程唯一能杀死它的操作：命令在后台跑着，会话却结束了，**没人接你的回复**。
任务卡在「待回复」，邮件发了也没人处理。

两道防线：

1. **文案写死**：`tba prompt` 生成的规则里明确「`ask` 前台阻塞，禁止 `run_in_background`」。
2. **孤儿检测 + 阻断**：`ask` / `wait` 阻塞期间每 15 秒写一次 `data/waits/<claim>.wait`。
   进程被杀就停更，超过 60 秒没有刷新即判定「等待中断」。这时候：
   - `tba inbox` 给这条任务标 `⚠ 等待中断（没人在等）`
   - **`tba g` / `tba claim` 拒绝领新任务**，退出码 `5`，要求先处理掉

第 2 条是「禁止」的真正含义——agent 挂后台不会立刻报错，但下次它想干活时会被拦住，必须先回去把等待接上。
任务状态本身不改（它确实还在等你回复），只是加了个「没人接」的标记。

#### 邮件配置

`data/config.toml` 里配 SMTP（发）+ IMAP（收）：

| 键 | 说明 |
|---|---|
| `smtp_host` / `smtp_port` | 发信。QQ 邮箱是 `smtp.qq.com` / `465` |
| `smtp_user` / `smtp_pass` | 账号 + **授权码**（不是登录密码） |
| `smtp_from` | 发件人。**留空 = 自动用 `smtp_user`**；也可写 `显示名 <邮箱>`。只写显示名会被服务端拒（QQ 返回 502） |
| `mail_to` | 收件人，多个用逗号或分号分隔 |
| `imap_host` / `imap_port` | 收信。不配就收不到回信 |
| `mail_on_ask` | 到「待回复」是否发信，默认开 |

发信失败会**写进任务备注**（`⚠ 邮件通知没发出去` + 具体错误），不再静默。
以前只打一行 stderr，agent 把命令丢后台时那行字谁也看不见。

**邮件正文带完整任务备注**——你在外面收信时，光看到一个「已完成，请确认」是判断不了的，
得能看到 agent 到底改了什么、结论是什么。所以正文结构是：

```
任务「xxx」需要你回复。

======== 任务备注（完整） ========

#### 2026-10-31 15:20
改了三处……
（agent 写的全部过程和结论）

================================

链接：https://www.teambition.com/task/xxx

直接回复本邮件即可（请保留标题里的 [TB clm-xxx]）。
```

| 命令 | 作用 |
|---|---|
| `tba mail status` | 看配置（能发/能收、发件人、收件人） |
| `tba mail test` | 发一封测试信，验证 SMTP |
| `tba mail check` | 查一次回信，顺便列出 INBOX 最近 5 封 |
| `tba mail resend <claim_id>` | **重发某条任务的通知信**（带完整备注）。发信失败过、或想在邮箱里再要一份上下文时用 |

#### 等待与轮询（agent 在「待回复」里等多久）

`tba ask` 问完就地阻塞等回复。三个时间参数都能改，默认值：

| 配置键 | 默认 | 管什么 |
|---|---|---|
| `wait_timeout_secs` | **540** | 单轮最长等多久。到点返回**退出码 2**，agent 重跑同一条接着等，会话不断 |
| `wait_interval_secs` | **15** | 多久查一次评论。数字回复走这条通道，所以要快 |
| `mail_poll_secs` | **20** | 多久查一次邮箱回信。IMAP 一次往返比读一条评论贵，所以比评论慢 |

540 秒不是随便定的：Bash 工具上限 600 秒，留 60 秒余量让它主动返回，
否则被 auto-background 后 agent 拿不到结果。

改法三选一：

1. **GUI**：`tba ui` → 左侧「⚙ 设置」→ 下拉到「等待与轮询」，三个输入框 +「保存等待参数」。
2. **改文件**：`data/config.toml` 里直接写上面三个键。
3. **临时覆盖**：`tba wait <claim_id> --timeout 300 --interval 10`（只影响这一次）。

下限保护：`wait_timeout_secs` ≥ 30、`wait_interval_secs` ≥ 5、`mail_poll_secs` ≥ 5。
填 0 或个位数会被夹到下限——轮询太快会变成忙等，把 Teambition 接口和邮箱服务器打到限流。

`tba mail status` 最后一行会回显当前生效的三个值。

### 两种开工方式（文案双分支）

`tba prompt` 生成的 agent 文案分两条路，agent 只按文案走，不自己判断：

| 文案里有没有派活 | agent 第一条命令 | 结果 |
|---|---|---|
| 没带任务（你只复制了卡片） | `tba claim --project <id>` | 从列「未完成」队列领最早的一条 |
| 带了任务（`tba prompt --task "…"` / UI 里填了「本次任务」） | `tba start --project <id> --file <md>` | 在列「进行中」新建一条「进行中」任务，由 agent 干 |

`tba start` 的两个细节：

- 标题取 `--title` > 正文首个 `# 标题` 行 > 文件名；正文落盘到 `data/tasks/<标题>.md`，agent 用 `--file` 读。
- **同名幂等**：本项目里已有同名任务的活动 claim 时直接复用，不会因为点了两次复制就建出两条。

### 一句话开工：`tba g`

不复制文案也能开工。在项目目录里跑 `tba g`（别名 `go` / `r` / `run`），它做三件事：

1. 按 `.tba/project` 认出项目。
2. 从列「未完成」领最早一条。
3. 打印任务正文 + 完整流程规则。

#### 列1 是最高优先级：拖回去就能重接

**任务被拖到列1，不管台账里是什么状态，一律重接。**

这条是「会话断了怎么恢复」的答案。agent 断了、卡住了、换设备了，你把任务拖回列1，下次谁跑 `tba g` 谁就接上。

实现上做了三件事：

| 行为 | 为什么 |
|---|---|
| 列1 的任务不再按台账过滤 | 以前台账里有 `waiting` 就跳过，拖回列1 也领不到 |
| 旧 claim 标 `reverted` 再新建 | 一条任务同时只有一条活动记录，留痕能看出「被拖回列1 重来过」 |
| 等待中断检查跳过列1 的任务 | 拖回列1 = 用户已经放弃那次等待，不该再拦 |

`tba take <任务链接>` 同理：指定的是列1 的任务就重接，否则沿用原 claim（幂等）。

**退出码是分支依据**，agent 不用解析文本：

| 输出 | 退出码 | 含义 | agent 该做什么 |
|---|---|---|---|
| `NO_BINDING <目录>` | 3 | 当前目录没绑定项目，且不是 git 仓库 | 停。不领任务，不建项目 |
| `NO_PROJECT <id 或 名字>` | 4 | 绑定失效，或目录名在 Teambition 上没匹配 | 问用户要不要初始化 |
| `EMPTY <项目名>` | 0 | 队列空 | 停 |
| `claim_id: …` | 0 | 领到了 | 照输出的流程干 |
| `BLOCKED` | 5 | 手上有「在等回复但没人接」的任务 | 先 `tba wait <claim>` 接回来，或 `tba release <claim>` 退回队列 |

领到任务后自动往当前目录补 `AGENTS.md`，两块内容，各自用标记检测、缺哪块补哪块，**绝不覆盖你原有的内容**：

| 块 | 标记 | 内容 |
|---|---|---|
| 任务队列入口 | `<!-- tba:entry` | 什么时候跑 `tba g`、四个退出码、看板列流转、做完怎么交回 |
| 编码纪律 | `<!-- tba:rules` | Karpathy 那 4 条：先想再写 / 最简实现 / 只改必须改的 / 先定验证标准 |

领到任务时，`tba g` 的输出**第一段就是「交回」提示**，带着填好的 `claim_id`，然后才是任务正文。
实测 agent 读完正文就直接开干、干完就结束回合，沉在后面的流程文案它不翻——所以提示必须放最前面。

两块分开检测的好处：**已经生成过入口的老项目，下次 `tba g` 也会自动补上后加的编码纪律**，不会被「已有入口」挡住。

编码纪律为什么放 `AGENTS.md` 而不是塞进 prompt：这四条是通用编码习惯，跟任务流程无关，而且每条都要解释才管用。
塞进 prompt 每次会话固定多 1500 字；`AGENTS.md` 是 agent 进项目就会读的约定文件，成本几乎为零。

补上之后用户说「开工」就够了；换项目、换设备没有入口文件时，`tba g` 照样能跑，只是多打几个字符。

- `--no-entry` 不补入口文件。
- `--lang en` 用英文规则（省 token）。
- 项目来源优先级：`--project` > `.tba/project` 绑定 > 按目录名去线上找同名项目。
- 换设备时没有 `.tba/project`：只要目录名和 Teambition 项目同名，自动认回来并补上绑定。
- 旧目录 `.tbhub/` 第一次被读到时自动改名成 `.tba/`，内容不动。改名失败（权限 / 占用）就继续用旧的。
- 既没绑定、又不在 git 仓库里：直接退 3。**不猜、不建**。

### 多会话并行（WorkBuddy 开两个 + dsh 再开一个）

台账和 `data/` 是跟 exe 走的，所有会话天然共享，**不需要额外同步**。要做的只有一件事：
给每个会话一个不同的标识，否则两件坏事会发生——A 会话活着会把 B 会话的僵尸任务一起续命（永远回收不掉），
`sweep` 的「会话存活豁免」也会互相顶。

```bat
tba hello --name wb-a     :: 在 A 会话的工作目录里跑一次
tba hello --name wb-b     :: B 会话换个名字
tba hello                 :: 不介意共用就自动生成（s-3f2a1c）
```

标识写入 `<当前目录>/.tba/session`，之后该目录下所有 `tba` 命令自动带上（也可用环境变量 `TBA_AGENT` 覆盖）。
**同一目录开两个会话必须显式 `--name` 区分**，否则后一个会覆盖前一个。

### 会话断开怎么自动退回队列

四层防线：

1. **正常收尾**：`ask` / `done` / `close`，skill 里强制。
2. **同步等待中**：agent 在 `tba ask` 里挂着，会话没结束就一直在（「待回复」本来就不参与回收）。
3. **进程退出清理**：`tba serve` 每 30 秒写会话心跳；会话结束（窗口关闭/进程退出）时自动把本会话 `running` 的任务退回 未完成 + 列「未完成」。`waiting` 的不动（那是在等你）。
4. **心跳超时兜底**：`running` 且心跳超过 `heartbeat_timeout_secs`（默认 300 秒）→ 判定会话已死 → 退回 未完成 + 列「未完成」，并在任务上留一条评论说明。任何 `tba` 命令启动时顺带扫一次。

另外：**任何 `tba` 命令启动时都会给本会话占着的 running 任务刷一次心跳**（`sweep` 除外），
所以长构建期间只要中途跑过一次 tba 就不会被误判。

强杀（进程被直接终止）时第 3 层不执行，靠第 4 层兜底；如果之后**再也没有任何 tba 命令被调用**，回滚会等到下次有人跑 `tba` 时才发生——这是没有常驻进程时的固有限制。

## 通用能力

「通用」= 跟 Teambition 无关，任何项目都能用。目的都是**别让 agent 现场摸索**：
分支叫什么、远端是哪个、CI 跑没跑完、截图怎么截、环境缺什么。

### git / GitButler

```bat
tba git status                        :: 分支 / 脏文件 / 远端领先落后 / 是不是 GitButler 项目
tba git main -m "feat: xxx"           :: 提交并推送（默认推到 origin/HEAD 指向的分支）
tba git main -m "xxx" --land          :: GitButler 项目里再跑 but land，直接落到目标分支
tba git main -m "xxx" --dry           :: 只打印会跑哪些命令
tba git tag --tag v1.2.3              :: 打 tag 并推
```

自动判断走哪条路：

| 仓库 | 执行的命令 |
|---|---|
| GitButler 项目（`but status` 能跑通） | `but commit -m` → `but push` →（`--land` 时）`but land` |
| 普通仓库 | `git add -A` → `git commit -m` → `git push origin HEAD:<分支>` |

为什么不统一走裸 git：GitButler 的改动挂在虚拟分支上，裸 `git commit` 会让工作区
在 GUI 里显示成游离提交，用户下次打开会看到一堆怪东西。

### 发版（GitHub Actions）

```bat
tba publish --init                    :: 生成 .github/workflows/release.yml
tba publish --tag v0.2.0 --watch      :: 打 tag -> 推 -> 等 CI -> 列产物
tba publish                           :: 不给 --tag 就读 Cargo.toml 的 version
```

`--init` 生成的 workflow 就是本仓库在用的那套：**windows x64 + windows arm64（MSVC 交叉编译）**
两个 job，有 `installer/*.iss` 就出 `setup.exe`，没有就出 zip，tag 推上去自动发 Release。
已有同名文件且内容不同时会拒绝覆盖，确认加 `--force`。

`--watch` 会轮询 GitHub API（token 取 `GH_TOKEN` / `GITHUB_TOKEN`，再退回 git 凭据），
默认最多等 300 秒，然后打印每个 job 的结论和 Release 产物清单。
拿不到 token 或查不到时不报错退出——tag 已经推成功了，只提示「自己去 Actions 页看」。

> 名字是 `publish` 不是 `release`：`tba release <claim_id>` 是「退回队列」，占着这个名字。

### 自检

```bat
tba doctor
```

一次列出：exe 路径 / 版本 / PATH 里有没有它 / 令牌有没有 / MCP 地址 / 数据目录 /
git 版本 / `but` 版本 / 当前是不是 GitButler 项目 / 有哪些中文字体 / 三个轮询参数。
排查「为什么跑不通」时先跑这个，别一个个试。

### 重载

关键流程不靠文字描述，固化成一个命令：**编译 → 杀旧 → 复制校验 → 启动**。

```bat
tba reload build      :: 重载（编译+替换+启动）
tba reload restart    :: 只重启，不编译
tba reload kill       :: 杀掉运行副本
tba reload status     :: 看是否在跑
```

窗口里在左侧「通用能力 → 重载」，可改项目目录 / exe 基名 / 运行副本路径，四个按钮同上，
还能直接看 `target/reload.log` 末尾。

约定：

| 项 | 说明 |
|---|---|
| 退出码 | 唯一真值。`0` 成功，`1` 失败 |
| 成功输出 | `OK <pid>` / `OK` / `RUNNING <pid>` / `STOPPED` |
| 失败输出 | stderr 一行 `ERR <step>:<msg>` |
| cargo 输出 | 只进 `<root>\target\reload.log`，不占 agent 上下文 |
| 运行副本 | 缺省 `<root>\target\release\<name>_test.exe`（跟产物同目录，`data/` 只有一份），cargo 产物从不直接运行 |
| 自己重载自己 | 自动转后台：脚本等本进程退出后再编译，绕开 exe 文件锁 |

其他项目也能用（这就是「通用」的意思）：

```bat
tba reload build --root D:\other\project
tba reload build --root D:\other\project --name myapp --live D:\other\project\run\myapp_test.exe
```

脚本随二进制内置，运行时释放到 `data/capabilities/reload.ps1`，不用单独维护。
工具链路径可用环境变量覆盖：`TBA_MSVC_BIN` / `TBA_MSVC_LIB` / `TBA_SDK_LIB` / `TBA_EXTRA_LIB` / `TBA_CARGO`。

> 杀软对 `*_test.exe` 这类名字比较敏感，若误报拦截启动，用 `--live` 换个名字即可：
> `tba reload build --live D:\dev\mcp\target\release\tb_run.exe`

## 共享记忆

```bat
tba memory --project <id> --write "进展：..."
tba memory --project <id>                 :: 读
```

文件在 `data/projects/<id>/memory/`。

## agent 文案

```bat
tba prompt --project <id>                              :: 不给任务 → 文案让 agent 去队列(列「未完成」)领
tba prompt --project <id> --task "# 标题
正文…"                                                 :: 给了任务 → 文案让 agent 用 tba start 新建
tba prompt --project <id> --task-file .\todo.md        :: 任务正文从文件读
```

或窗口里点卡片 → 两个复制按钮：

| 按钮 | 行为 | agent 拿到后 |
|---|---|---|
| `复制agent文案(本次任务)` | 带上卡片上「本次任务」输入框里填的内容 | 跑 `tba start --project … --file …`，在列「进行中」新建任务 |
| `复制agent文案(领列1任务)` | **强制丢弃**输入框里的残留草稿 | 跑 `tba claim --project …`，去列「未完成」领队列任务 |

两个按钮的差别是**刻意的**：点「领列1任务」时哪怕输入框里还留着上次写的草稿，文案里也不会出现——否则 agent 会自建任务而不是去领，行为就跑偏了。

### 省 token 设计（英文速查表 + skill 按需读）

文案是发给 agent 的、人基本不读，所以按 token 成本优化过：

- **闭环指令是英文速查表**。实测同义内容中文约 882 token、英文约 540 token（cl100k）——中文字在 BPE 分词里普遍 1 字 1+ token，英文 1 token 能装好几个字母。阶段名（未完成/进行中/…）保持中文，那是 Teambition 上的真实名字，改了 agent 对不上。
- **skill 不内联全文，只列名字**。以前每次发文案都把全部启用 skill 的全文贴进去，skill 越多越大越贵；现在文案只给名字列表 + 一条读取命令，agent 用到哪个读哪个：

```
Enabled skills (3): 部署流程, 代码规范, 发布 checklist
Read one on demand (do NOT read all): `tba skill <name> --project <id>`
```

```bat
tba skill 部署流程 --project <id>   :: 打印该 skill 全文
```

不变量一条没丢（只经 tba 改状态/列、别自建任务、每条必须终结于 ask/done/close、长操作前后 beat、待回复永不超时、心跳超时退回队列、退出码 0/2/3 语义），由单测 `workflow_keeps_key_invariants` 守着；`prompt_with_huge_skill_stays_small` 防体积回潮。测量脚本：`tools/token_cmp.py`。

文案含：任务标题/正文、已启用 skill 名字列表、共享记忆目录、开工命令（`claim` 或 `start`）、收尾规则。

`--task` / `--task-file` 的正文会落盘到 `data/tasks/<标题>.md`，文案里给出绝对路径，agent 直接用 `tba start --file` 读，不用你贴正文。

## 窗口设置（字号 / 字体）

窗口左侧栏 → `设置`。

**字号**：可左右拖的数字框（DragValue），按住往右拖就变大、往左拖变小，一格 1px；
旁边还有 `-` / `+` 各一号。范围 12–40，默认 **18**。不用滑块——滑块拖到 18 这种精确值很难。

改完立刻生效（正文、侧栏、按钮一起等比放大），并落盘到 `data/ui.toml`：

```toml
font_size = 18.0
font = ""          # 空 = 自动（黑体优先）；也可填 simhei / msyh / deng / simsun / msjh ...
```

> 界面偏好单独存 `data/ui.toml`，不写 `config.toml`——后者存着访问令牌和工具映射，整体覆写风险太大。

**中文字体可选**：设置页「中文字体」下拉列本机已装的中文字体，没装的标「（未安装）」。
选中后整窗立刻换字体。存的是 key 不是路径，换机器不会因为路径不同而失效。

字体链：**选中的字体** → 其余中文字体（黑体 / 雅黑 / 等线 / 宋体 / 正黑 / 苹方 / Noto）
→ `seguisym.ttf`（符号）。只装一个字体时，它缺的字形会渲染成豆腐块（口）——
之前只装等线，界面上到处是口，就是这个原因。

**控制台窗口**：exe 是 console 子系统（CLI 要 stdout），双击时 Windows 会先给一个黑窗。
进 GUI 前调 `FreeConsole()` 把它摘掉，所以双击只看到窗口。CLI 子命令不受影响。

## MCP 接入

在 agent 的 MCP 配置里加：

```json
{
  "mcpServers": {
    "tba": {
      "command": "tba.exe",
      "args": ["serve"]
    }
  }
}
```

工具：list_projects / list_project_skills / set_switch / list_tasks / ensure_stages / claim_task / complete_task / close_task / read_memory / write_memory / get_agent_prompt。

## 工具识别不准时

Teambition MCP 的工具名由服务端动态下发。自动识别靠名字关键词。不准时：

1. `tba tools` 看真实工具名。
2. 编辑 `data/config.toml`，在 `[tool_overrides]` 里指定，例如：

```toml
[tool_overrides]
list_projects = "真实工具名"
claim_task = "真实工具名"
```

可用角色：list_projects / get_project / list_taskflows / get_taskflow / create_taskflow / list_stages / create_stage / create_task / update_task / move_task / list_tasks / get_task。

改完运行 `tba tools --refresh`。

## 排查参数问题

真实 Teambition MCP 的工具名长这样：`teambition.project.query`、`teambition.task.query`、`teambition.taskflowstatus.query`。自动识别按名字关键词打分，上面这些都能对上。

要看某个工具到底要什么参数：

```bat
tba tools --refresh --json
```

会打印每个工具的完整 `inputSchema`。

### 已经确认的真实语义

从官方文档（Teambition MCP / Skill 使用指南 + TQL 参考）确认：

| 项 | 真实行为 |
|---|---|
| 服务地址 | `https://open.teambition.com/api/mcp/v2`，Streamable HTTP |
| 授权 | 默认 OAuth + PKCE（issuer `account.teambition.com`，scope 只有 `user`）；User Token 也能用 |
| 项目查询 | `teambition.project.query` 要求 `projectId` / `tql` / `my` 至少给一个 |
| 任务查询 | 走 **TQL**。`stageId`、`taskflowstatusId`、`projectId` 是 **TQL 字段**，不是独立的工具参数 |
| 任务标题 | 在 **`content` 字段**里，正文在 `note` |
| 分页 | 结束条件是 **`nextPageToken`** 为空 |
| 回收站 | 任务查询必须默认带 `isArchived = false`，否则会把回收站任务混进来 |
| 环境变量 | `TEAMBITION_MCP_HOST` / `TEAMBITION_MCP_TOKEN` 可覆盖默认地址和令牌 |

`tba` 已经按这些实现：`list_projects` 自动传 `my=true`、`list_tasks` 自动拼 TQL 并带 `isArchived = false`、按 `nextPageToken` 翻页、skill 正文写进 `note`。

如果换工具后遇到同类报错，把 `tba tools --refresh --json` 的输出发出来，按 schema 补参数即可。

## OAuth（可选）

`data/config.toml` 填 client_id / client_secret / authorize_url / token_url 后，`tba login` 走本地回环授权。默认用 User Token 最省事。

## 本地测试（不连真实 Teambition）

项目带一个模拟 Teambition MCP 服务，用来验证全流程：

```bat
python tools\mock_tb_mcp.py 8791
```

然后把 `data/config.toml` 的 `mcp_url` 改成 `http://127.0.0.1:8791/mcp`，`access_token` 随便填个非空值。
模拟服务内置 2 个项目、11 个工具，故意把 Alpha 项目的阶段设成只有 2 个，用来验证 `tba init` 自动补齐。

注意：模拟服务是**多线程内存版**，重启后状态全丢（阶段、任务、skill 都回初始值），
但本地开关和台账不受影响。
