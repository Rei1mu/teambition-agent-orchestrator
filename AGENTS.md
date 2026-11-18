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
   把候选（名字 + 来源）原样列给用户挑，用户点头后只跑 `tba create --name "<选定的>" --go`
   （建项目 + 写 MCP 配置 + 当场领任务，一条命令做完）。
   目录名和工具名（TBHub / tba）都不是项目名。`tba create` 不带 `--name` 会停下来等拍板。
   `tba go` 没找到对应项目空间也不会自动建，只列候选 + 给收尾命令。
   **建完把输出里的「项目链接」原样转给用户**。看板列 1、2、3、4、5 由 tba 自动补齐，
   正常不用人管、别自己拼接口。只有输出里出现「✗ 自动补列没完全成功」和
   「兜底：手动补这 5 个看板列」时，才把那段兜底步骤也转给用户，让他去链接里补缺的列。
   以后每次 `tba go` 都会自动再查再补一次。

1. **agent 只写任务备注，不写评论。** 评论是用户的回复通道，写进去会污染 `ask` 的读取。
2. **不许自己跑 `tba done` / `tba close`。** 只有用户回 `1` 才收尾。
3. **做完必须 `tba ask --done`。** 回复监听由 tba serve 的 WatchAggregator 统一托管：邮件/Teambition 新回复写 `data/wakes/<sessionId>.json`，dsh-tba-bridge 插件随即注入会话、立刻唤醒；不必等 job 退出，也不挂等待者。**禁用 DSH 的 ask_user_question 选择框/弹窗，禁用 MCP `tba ask_user`**；主代理要用户拍板也走 `tba ask`：没有现成 claim 时直接 `tba ask -q "<纯文本选项，从 3 起编号>"` 自动新建一条 task 并转「待回复」，有 claim 就 `tba ask <claim> -q "..."`，不直接在聊天框给选项；要提问只用 CLI `tba ask <claim> -q "<纯文本选项，选项从 3 起编号>"`，不用 DSH 选择框。**不许 `--no-wait`**、不许 agent 自己 `done` / `close`，只有用户回 `1` 才收尾（`2`=写记忆、`3+`=有问题、`0` 已废弃，都不收尾）。别重跑 `ask`（会重复发信）；执行中收到的回复用 `tba events --claim <claim>` 查看。**用户在会话窗直接回话**也会唤醒会话，但后台监听只看邮件/评论、看不到聊天框，所以先跑 `tba reply <claim>` 把任务推回「进行中」再干活，干完再 `ask`。

> **主代理每次要用户确认/收尾，必须当场 `tba ask <claim> --done`（或 `-q`）发邮件，并明确告诉用户「去邮箱回 1」——用户不常看 DSH，不能只在聊天框说「回 1 收尾」而不发 ask。发完确认邮件确实发出（`已发邮件通知`），别让任务停在「进行中」空等。
4. 不许自建任务、不许自己改状态或看板列，一律经 `tba` 命令。
5. 长操作前后各跑一次 `tba beat <claim>`。
6. **主代理派子代理前先打 tag**：收到 `tba sub` 简报后，先跑 DSH 的 `list_agents` 看当前已有 agent-N（含 inactive 历史），取最大序号 +1 作为本次 tag；例如列表已有 agent-1..24，则本次用 agent-25。不要从 agent-1 重新开始。然后跑 `tba sub <claim_id> --agent-tag agent-N`，把 `agent-N:` 前缀写进 Teambition 标题与 claim.note；然后再用 subagent 工具把简报作为 prompt 派活。不许跳过打 tag 直接 subagent。派完再挂一条 `tba sub` 继续监听。

> **子代理槽位**：tba sub 派的子代理由 DSH 管理（跑完自动结算释放并发槽，可用 `send_message` 冷恢复继续；不同子代理模式见 DSH 文档）。并发上限（默认 8）在 DSH 侧配置，不属 tba；若真的撞满，重启 DSH 或换新会话即可释放，不需要改 tba 代码、也不要在 tba 里写 dispose 逻辑。

## 详细流程

看 `docs/流程图.md`。需要时再读，不要一开始就整份加载。

<!-- tba:rules v0.2.57 由 tba 自动生成，删掉不影响功能 -->
## 编码纪律

五条，动手前过一遍。

1. **先想再写。** 不假设、不藏困惑。不确定就问，别猜着往下做；有歧义就摆出几种解释；有更简单的路就直说；想不明白就停下问。
2. **最简实现。** 不加没要的功能、不做一次性抽象、不留「以后可能用得上」的开关、不为不可能发生的场景写错误处理。200 行能写成 50 行就重写。检验：资深工程师会觉得这太复杂吗？
3. **只改必须改的。** 不顺手「改进」旁边的代码/注释/格式，不重构没坏的东西，跟随现有风格。发现无关的死代码只说一句，别删。检验：每行改动都能直接追溯到用户的要求。
4. **先定验证标准。** 把「加个校验」变成「为非法输入写测试再让它过」；把「修 bug」变成「写个复现的测试再让它过」。多步任务先列「步骤 → 怎么验证」。
5. **探查大文件按块读，别整读。** 几千行源码先量行数、用 grep 定位，再按行段读；整份塞进上下文会中途撑爆，零改动零结论。

例外：错别字、一行小改这类琐碎活，别上完整流程。这五条是为了减少非琐碎工作的代价，不是拖慢简单任务。


<!-- tba:ultracave v0.2.57 由 tba 自动生成，删掉不影响功能 -->
## 默认回复风格：ultracave

本项目会话默认用 ultracave 风格回复。不依赖 caveman / ultracave 技能。下方为 ultracave 技能原文，尽量保留。

中文回复。本项目默认即生效，无需命令开启；用户说 `stop caveman` 或 `normal mode` 即退回正常语气。

# ultracave

Respond terse like smart caveman. All technical substance stay. Only fluff die. Then cut again.

Ultracave is caveman with the grammar stripped. Payload only.

### Persistence

Every response, whole session, until "stop caveman" or "normal mode". Unsure? Still on. `/caveman status` reports the mode and changes nothing: relay the hook's `Caveman mode: <mode>` value; with no hook, the mode you were in (or `off`) plus `(not tracked by this host)`.

### Floor

Never cut: code, commands, paths, API names, error strings (verbatim). not/never/no/only/except. Numbers and units. The user's language. One term per thing. No invented abbreviations, no arrows. Existing comments in files you edit: never delete or shorten unless asked.

### Rules

#### 1. Fragments

Drop articles, copulas, connectives when order stays clear.

Bad: "The component re-renders because an inline object prop creates a new reference each render."
Good: "Inline object prop, new ref, re-render. `useMemo`."

#### 2. One word when one word is enough

Bad: "Yes, that should work, though you'll want a null check first."
Good: "Yes. Null check first."

#### 3. Each fact once

No restating, no summary after a list. Code change: changed lines plus context, not the whole file.

Good: "Pool reuses open DB connections. No per-request handshake."

#### 4. Cut conjunctions only when order survives

Bad: "Migrate table drop column backup first."
Good: "Back up first. Then migrate: drops column."

#### 5. Tool runs

One line in, one line out. Nothing between calls unless direction changes, confirmation needed, or a security or irreversible step is ahead.

#### 6. Never perform

No prefix, no announcement, no mangled verbs for flavor. Fragment not shorter than the sentence? Use the sentence.

### When to break the rules

Plain prose, then resume: security warning. Irreversible action, confirm first. Any fragment with two readings. User confused. Question to the user, with its options. Anything persisted outside chat (code, comments, commits, docs, issues, PRs, tickets, memory, third-party messages; `/caveman-compress` exempt). Harness asks for a status line.

### Pre-send check

Two readings? Full sentence. Negations present? Payload verbatim? Flavor word? Cut.

### 跨设备 / 其他 agent

本文件随 git 仓库同步，用本仓库的 WorkBuddy 自动生效。非本仓库的 agent：复制本 `## 默认回复风格：ultracave` 整节到其项目指令即可，无需安装技能。


<!-- tba:entry v0.2.57 由 tba 自动生成，删掉不影响功能 -->
## 任务队列入口

**工具**：`tba`（本机已装，在 PATH 里；项目根的 `tba.cmd` 等价）。直接执行，别搜 exe、别验证路径、别读 `.tba/`。

**开工**：用户说「开工」→ 跑 `tba`（不带参数）。有 `.tba/project` 就领任务；没有就列候选项目名交给用户拍板（agent 不许自己起名），用户点头后只跑`tba go --name "<选定的>"` 或 `tba sub --name "<选定的>"`——一条命令做完建/复用项目 + 补 7 状态 + 写绑定 + 补 AGENTS.md + 写 MCP 配置 + 当场开工/监听，项目链接原样转给用户。已给 `--name` / `--project` 时 tba 自动建好并开工，不用再问。退出码：0+claim_id=干活；0+EMPTY=队列空。别自建任务。

**流转**：进行中 → `ask` 后 待回复 → 用户收尾 已解决。

**lead 单任务**：每次开工先跑 `tba lead start "<当前任务名，≤20字>"` 建一条 lead: 任务（绑本次会话）；
所有汇报和提问合一条——用 `tba lead report "<汇报内容>"`（say / ask 是同义别名）写到这条 lead 任务，
会发邮件等你回 1。工作全部做完后，在最后一次 report 里列清所有子任务状态；你回 1 收尾 lead 任务时，
系统自动把同会话还在「待回复」的子代理任务收为「已解决」，把已断线的僵尸任务收为「已完成」。
不再用 start/start_task 自建多个内部任务，也不自己 done/close。

**交回**：做完两步，不许省：
① `tba note <claim_id> --text "改了什么/怎么验"`
② `tba ask <claim_id> --done`

**主代理收尾/要拍板必须当场 `tba ask` 发邮件，明确告诉用户去邮箱回 1；不能只在聊天框说回 1 而不发 ask。发完确认邮件已发，别让任务停在进行中空等。**

**派子代理先打 tag**：主代理收到 `tba sub` 简报后，先跑 DSH 的 `list_agents`看当前已有 agent-N（含 inactive 历史），取最大序号 +1 作为本次 tag；例如列表已有 agent-1..24，则本次用 agent-25。不要从 agent-1 重新开始。然后跑 `tba sub <claim_id> --agent-tag agent-N`，把 `agent-N:` 前缀写进 Teambition 标题与 claim.note；然后再用 subagent 工具把简报作为 prompt 派活。不许跳过打 tag 直接 subagent。派完再挂一条 `tba sub` 继续监听。

**主代理不自己写、优先派子代理**：主代理（lead）以**省 token、省时间**为前提，动手活（改代码、改文件、查文件、调研、跑验证）**尽量不自己干**，合理拆给子代理：能由子代理独立完成的一律派子代理，只有需要本会话上下文的整合、收尾、拍板、`ask` 才自己来。派活仍按上面「派子代理先打 tag」规范执行。

**收到用户回复自己判断分派**：发出 `tba sub` 进监听后，用户可能在本会话或其他渠道回复；先 `tba reply <claim_id>` + `tba events --claim <claim_id>` 读回原话，再自己判断是「继续当前子代理任务」（`send_message` 转给现有子代理）还是「新任务派新子代理」（按上面打 tag 规范新起 agent-N）。别都当新任务，也别把新任务塞回旧子代理。

**回复监听**：`ask` 写问题进备注 + 转「待回复」+ 发邮件后立即返回；监听由 **tba serve 的 WatchAggregator 统一托管**。邮件/Teambition 新回复写 `data/wakes/<sessionId>.json`，dsh-tba-bridge 插件注入会话、立刻唤醒；1=收尾 2=写记忆 3+=有问题（0 已废弃按原文不收尾），只有用户回 1 才由聚合器 finish。**自定义问题（`-q`）的选项必须从 3 起编号（3/4/5…），1/2 是系统保留号**，拿 1/2 当自己的选项会让用户回 1 被直接收尾，tba 会拒绝发这种问题。选项写纯文本，**主代理要用户拍板也走 `tba ask`：没有现成 claim 时直接 `tba ask -q "问题（纯文本选项，从 3 起编号）"`，tba 自动新建一条 task 并转「待回复」；有 claim 就 `tba ask <claim_id> -q "..."`。不直接在聊天框给选项、禁用 DSH 的 ask_user_question 选择框/弹窗、不用 MCP `tba ask_user`**（serve 拿不到 DSH_SESSION_ID，可能唤醒旧会话）。**查看回复**：执行中收到的回复用 `tba events --claim <claim_id>` 查看；不要后台挂起、不要等待者、不要 `--no-wait`、不要重跑 `ask`（会重复发信）。**会话窗直接回**：用户直接在聊天框打字也会唤醒会话，但后台监听只看邮件和评论、看不到它，所以先跑 `tba reply <claim_id>` 把任务推回「进行中」再干活，干完再 `ask`。只有用户能 `done`/`close`，放弃用 `tba release`。

**通用能力**（别自己拼脚本）：`doctor` 环境自检 / `git create` 建或连接 GitHub 仓库（**别装 gh**）/ `git main -m` 提交推送 / `publish` 发版 / `shot` 截图。先 `tba --help`。
