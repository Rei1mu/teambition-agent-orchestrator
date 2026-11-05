<!-- tba:bootstrap 由 tba 自动生成，删掉不影响功能 -->
tba 是本机已安装的命令行工具，在 PATH 里。当前项目用它管任务队列。
**直接执行 `tba`。不要为了找它去搜 exe、不要验证路径、不要 `where tba`。**
跑不通就把错误原样报给用户。只有用户明确要求排查时才 `tba doctor`。
`tba --help` 看全部命令。

## 这个项目是什么

TBA（Teambition Agent）：把 Teambition 当任务队列的 CLI。Rust，单 crate，无外部服务依赖。

数据在 `C:\Users\user\tba\data\`（exe 同目录），所有 agent 共享这一份。

**离线模式**：`config.toml` 的 `offline = true` 或 `TBA_OFFLINE=1` 时完全不连 Teambition，
项目/能力/开工文案走本地缓存（`projects_cache.json`、`projects/<id>/skills.json`）。
要联网的命令会直接报 `当前是离线模式…` 并列出本地还能用的命令。改这个开关前先问用户。

## 编译与部署

**必须用 MSVC 环境**。Git Bash 直接 `cargo build` 会命中 PortableGit 的 GNU `link.exe` 而失败。

```powershell
$env:PATH = "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64;$env:PATH"
$env:LIB  = "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.44.35207\lib\x64;D:\d\winsdk_libs"
Set-Location D:\dev\mcp
cargo +stable-x86_64-pc-windows-msvc build --release --offline
```

部署。**没有目录联接，不覆盖就不生效**。替换前先关掉运行中的 `tba` 进程，否则文件被占用：

```powershell
Copy-Item D:\dev\mcp\target\release\tba.exe  C:\Users\user\tba\tba.exe  -Force
Copy-Item D:\dev\mcp\target\release\tbagui.exe C:\Users\user\tba\tbagui.exe -Force
```

`tbagui.exe` 是双击入口（GUI 子系统，`src/bin/tbagui.rs`）：用 `CREATE_NO_WINDOW` 拉起
`tba.exe ui`，所以快捷方式启动不会闪黑框。`tba.exe` 保持 console 子系统（CLI 要 stdout）。
两个都要部署：只换 `tba.exe` 时窗口照样能开（启动器只是个壳），但启动器本身的改动不会生效。

单测：同一条命令把 `build --release` 换成 `test`。

## 红线

0. **用户说「直连 / 不用 Teambition / 别等回复」时，用 `tba go -off <这次要做的事>` 开工**，
   不要 `tba go` 领任务。直连模式下：不调 `claim/take/start/ask/wait/beat/done/close/note`，
   有疑问直接在对话里问用户，干完直接说结论。

0. **接入项目时不许自己起名字。** 跑 `tba`（不带参数）扫候选，
   把候选（名字 + 来源）原样列给用户挑，用户点头后才 `tba create --name "<选定的>"`。
   目录名和工具名（TBHub / tba）都不是项目名。`tba create` 不带 `--name` 会停下来等拍板。

1. **agent 只写任务备注，不写评论。** 评论是用户的回复通道，写进去会污染 `ask` 的读取。
2. **不许自己跑 `tba done` / `tba close`。** 只有用户回 `0` 才收尾。
3. **做完必须 `tba ask --done`，然后等它返回。** 它问完就地阻塞等你回数字，**不许结束会话**；退出码 2 就改用 `tba wait <claim>` 接着等（别重跑 `ask`，会重发信），别断。
4. 不许自建任务、不许自己改状态或看板列，一律经 `tba` 命令。
5. 长操作前后各跑一次 `tba beat <claim>`。

## 详细流程

看 `docs/流程图.md`。需要时再读，不要一开始就整份加载。

<!-- tba:rules 由 tba 自动生成，删掉不影响功能 -->
## 编码纪律

四条，动手前过一遍。

1. **先想再写。** 不假设、不藏困惑。不确定就问，别猜着往下做；有歧义就摆出几种解释；有更简单的路就直说；想不明白就停下问。
2. **最简实现。** 不加没要的功能、不做一次性抽象、不留「以后可能用得上」的开关、不为不可能发生的场景写错误处理。200 行能写成 50 行就重写。检验：资深工程师会觉得这太复杂吗？
3. **只改必须改的。** 不顺手「改进」旁边的代码/注释/格式，不重构没坏的东西，跟随现有风格。发现无关的死代码只说一句，别删。检验：每行改动都能直接追溯到用户的要求。
4. **先定验证标准。** 把「加个校验」变成「为非法输入写测试再让它过」；把「修 bug」变成「写个复现的测试再让它过」。多步任务先列「步骤 → 怎么验证」。

例外：错别字、一行小改这类琐碎活，别上完整流程。这四条是为了减少非琐碎工作的代价，不是拖慢简单任务。


<!-- tba:entry 由 tba 自动生成，删掉不影响功能 -->
## 任务队列入口

**工具**：`tba`（本机已装，在 PATH 里；项目根的 `tba.cmd` 等价）。直接执行，别搜 exe、别验证路径、别读 `.tba/`。

**开工**：用户说「开工」→ 跑 `tba`（不带参数）。有 `.tba/project` 就领任务；没有就列候选项目名，交给用户拍板。退出码：0+claim_id=干活；0+EMPTY=队列空；3=没绑定项目；4=项目查不到，问用户。别自建任务。

**流转**：进行中·列2 → `ask` 后 待回复·列3 → 用户收尾 已解决·列4。

**交回**：做完两步，不许省：
① `tba note <claim_id> --text "改了什么/怎么验"`
② `tba ask <claim_id> --done`

`ask` 发邮件后**就地阻塞**等你回数字（0=收尾 1=写记忆 2=有问题）。退出码：0=收到回复；2=超时，改用 `tba wait <claim_id>`（别重跑 ask，会重发信）；3=已收尾。
**禁止 run_in_background，绝不结束会话**——断了没人接你回复。只有用户能 `done`/`close`，放弃用 `tba release`。

**通用能力**（别自己拼脚本）：`doctor` 环境自检 / `git create` 建或连接 GitHub 仓库（别装 gh）/ `git main -m` 提交推送 / `publish` 发版 / `shot` 截图。先 `tba --help`。

**编译 & 重载**：先 `tba setting` 看本项目的编译指令 / exe 名（存 Teambition 的 setting 任务），再 `reload build`。编译指令是空的就 `setting guess` 探测后 `setting set --compile "..."` 写回去。
**能力库**（本地、跨项目）：开工文案里已经列了「名字 + 什么时候用」，按需 `tba cap <名字>` 读正文；别去项目 skill 里找截图/发版这类通用流程。带【常驻】的是红线，动手前先看。
**编译不了先查工具链**：`tba toolchain` 会告诉你 MSVC / Windows SDK 缺哪个文件。缺了不要自己拼 PATH，也不要换 gnu——用 `tba toolchain set` 指定一次位置。
