<!-- tba:bootstrap v0.2.67 由 tba 自动生成，删掉不影响功能 -->
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

## 详细流程

看 `docs/流程图.md`。需要时再读，不要一开始就整份加载。

<!-- tba:agent 由 tba 自动生成，删掉不影响功能 -->
## 任务队列入口

**工具**：`tba`（本机已装，在 PATH 里；项目根的 `tba.cmd` 等价）。直接执行，别搜 exe、别验证路径、别读 `.tba/`。

**开工**：用户说「开工」→ 跑 `tba`（不带参数）。有 `.tba/project` 就领任务；没有就列候选项目名交给用户拍板（agent 不许自己起名），用户点头后只跑`tba go --name "<选定的>"` 或 `tba sub --name "<选定的>"`——一条命令做完建/复用项目 + 补 7 状态 + 写绑定 + 补 AGENTS.md + 写 MCP 配置 + 当场开工/监听，项目链接原样转给用户。已给 `--name` / `--project` 时 tba 自动建好并开工，不用再问。退出码：0+claim_id=干活；0+EMPTY=队列空。别自建任务。

**流转**：进行中 → `ask` 后 待回复 → 用户收尾 已解决。

**lead 单任务**：每次开工先跑 `tba lead start "<当前任务名，≤20字>"` 建一条 lead: 任务（绑本次会话，一条，主代理自己的汇报/收尾）；
所有汇报和提问合一条——用 `tba lead report "<汇报内容>"`（say / ask 是同义别名）写到这条 lead 任务，
会发邮件等你回 1。子任务阶段性完成后也走 lead report 汇报（发邮件+Teambition），别只在 DSH 聊天框说一句；
工作全部做完后，最后一次 report 列清全部子任务状态。你回 1 收尾 lead 任务时，
系统自动把同会话还在「执行中」「待回复」的子代理任务收为「已解决」，把已断线的僵尸任务收为「已完成」。
不再用 start/start_task 自建多个内部任务，也不自己 done/close。

**交回**：做完两步，不许省：
① `tba note <claim_id> --text "改了什么/怎么验"`
② `tba ask <claim_id> --done`

**主代理收尾/要拍板必须当场 `tba ask` 发邮件，明确告诉用户去邮箱回 1；不能只在聊天框说回 1 而不发 ask。发完确认邮件已发，别让任务停在进行中空等。**

**派子代理先打 tag**：主代理收到 `tba sub` 简报后，先跑 DSH 的 `list_agents`**只看本会话**已有 agent-N（含 inactive 历史）。agent-N **按任务号分配，不按子代理实例数**：N = 本会话已分过号的任务数 + 1；同一个任务重开（失败/断线/换实例接手）复用原 agent-N，不占新号；send_message 冷恢复原实例号不变；关联性强的新任务（同一模块/延续上文/同一问题返工）也复用旧 agent，不管它现在是已完成还是在 `ask --done` 待回复，用 send_message 续活、沿用原 agent-N 与 tag，不新建 subagent 实例、不占新号；复用时不必重新打 tag（tag 已在原任务标题里）。只有全新无关任务才递增。默认复用更省 token（旧 agent 已有上下文，send_message 续活比新建重喂整段简报便宜）；只有旧 agent 明显跑偏、汇报质量差或上下文明显过长时才新建，不用刻意估算上下文长度。复用 agent 时 Teambition 任务也要复用，别让同 agent-N 出现两条 claim：原 claim 还在进行中/待回复时，用 `tba reply <旧claim_id>` 推回进行中 + `tba note <旧claim_id>` 写新需求 + send_message 续活，不再 `tba sub` 新建 claim；原 claim 已收尾（done/closed）不许 reply，只能新建 claim，但 tag 仍沿用原 agent-N（实例复用、不占新号），并在新任务里说明是续活。只有全新无关任务才新建 claim + 递增新号。本会话第一个任务 = agent-1。别跨会话、别参考别的项目/历史会话的任务。原因（用户原话）：前缀混了会导致本会话序号从别的会话接着数、打错 tag。`agent-N:` 由 `tba sub <claim_id> --agent-tag agent-N` 写进 Teambition 标题与 claim.note，subagent 工具的实例名（name 参数）跟 tag 是两回事，不用管；前缀由 `agent_tagged_title` 加，支持覆盖 swap（打错可重打纠正）。`lead:` 是主代理自己的汇报/收尾任务，`lead start` 自动加，一条，不打 agent tag、不派子代理。别给 lead 任务打 agent tag，别给子代理任务用 lead: 前缀。然后再用 subagent 工具把简报作为 prompt 派活。不许跳过打 tag 直接 subagent。派完再挂一条 `tba sub` 继续监听。

**`tba sub` 常驻监听必须后台执行**：它是常驻监听（空队列不退出），前台跑会挂住会话。挂监听时一律用后台 job（`run_in_background: true`），**不许前台等待**；前台跑只留给单次领任务的动作。After attaching, never poll it with `job_output` — move on; wake-ups arrive via the bridge.

**主代理不自己写、优先派子代理**：主代理（lead）以**省 token、省时间**为前提，动手活（改代码、改文件、查文件、调研、跑验证）**尽量不自己干**，合理拆给子代理：能由子代理独立完成的一律派子代理，只有需要本会话上下文的整合、收尾、拍板、`ask` 才自己来。派活仍按上面「派子代理先打 tag」规范执行。

**省 token（主代理必读）**：
① 子代理回报的编译/测试数字**直接采信**，主代理只抽查一次，不重跑全套；
② 不读全量 diff——派活时就要求子代理回报「改了哪几个函数 + 关键 hunk」，主代理只按需看可疑处；
③ 一份活整包派出去（代码 + 文档文案 + 编译 + 实测 + 提交），主代理不接手其中任何一环；
④ 主代理只做：打 tag → 派活 → 汇总回复 → `ask` 收尾 → 部署一条命令；
⑤ 保留并行：每个子任务各自的 claim，谁先完成谁先 `ask`，**不要**等所有子任务汇总成一条再问。

**收到用户回复自己判断分派**：发出 `tba sub` 进监听后，用户可能在本会话或其他渠道回复；先 `tba reply <claim_id>` + `tba events --claim <claim_id>` 读回原话，再自己判断是「继续当前子代理任务」（`send_message` 转给现有子代理）还是「新任务派新子代理」（按上面打 tag 规范新起 agent-N）。别都当新任务，也别把新任务塞回旧子代理。

**回复监听**：`ask` 写问题进备注 + 转「待回复」+ 发邮件后立即返回；监听由 **tba serve 的 WatchAggregator 统一托管**。邮件/Teambition 新回复写 `data/wakes/<sessionId>.json`，dsh-tba-bridge 插件注入会话、立刻唤醒；1=收尾 2=写记忆 3+=有问题（0 已废弃按原文不收尾），只有用户回 1 才由聚合器 finish。**自定义问题（`-q`）的选项必须从 3 起编号（3/4/5…），1/2 是系统保留号**，拿 1/2 当自己的选项会让用户回 1 被直接收尾，tba 会拒绝发这种问题。选项写纯文本，**主代理要用户拍板也走 CLI `tba ask`：没有现成 claim 时直接 `tba ask -q "问题（纯文本选项，从 3 起编号）"`，tba 自动新建一条 task 并转「待回复」；有 claim 就 `tba ask <claim_id> -q "..."`。ask 写问题进任务备注 + 发邮件 + 转「待回复」/列3，回复由 tba serve 的 WatchAggregator 托管；不直接在聊天框给选项。**
提问/汇报只用 CLI `tba ask`（发邮件+Teambition）；别用 DSH `ask_user_question` 弹窗，也别用 MCP `tba ask_user`（唤醒旧会话）。
**查看回复**：执行中收到的回复用 `tba events --claim <claim_id>` 看分流卡（分类/字数/摘要），默认不读原文；要看原文才 `--full`。原因（用户原话）「主代理上下文不能太长，我怕他主代理先抽风或者出问题」。三层分流：① 纯数字收尾（closed/summarize）主代理自己做；② 实质需求（free/custom:N）转手子代理，原文由子代理读；③ 要拍板走 `tba ask -q`。正文永不主动进主代理上下文。不要后台挂起、不要等待者、不要 `--no-wait`、不要重跑 `ask`（会重复发信）。**会话窗直接回**：用户直接在聊天框打字也会唤醒会话，但后台监听只看邮件和评论、看不到它，所以先跑 `tba reply <claim_id>` 把任务推回「进行中」再干活，干完再 `ask`。只有用户能 `done`/`close`，放弃用 `tba release`。

**通用能力**（别自己拼脚本）：`doctor` 环境自检 / `git create` 建或连接 GitHub 仓库（**别装 gh**）/ `git main -m` 提交推送 / `publish` 发版 / `shot` 截图。先 `tba --help`。


## 编码纪律

五条，动手前过一遍。

1. **先想再写。** 不假设、不藏困惑。不确定就问，别猜着往下做；有歧义就摆出几种解释；有更简单的路就直说；想不明白就停下问。
2. **最简实现。** 不加没要的功能、不做一次性抽象、不留「以后可能用得上」的开关、不为不可能发生的场景写错误处理。200 行能写成 50 行就重写。检验：资深工程师会觉得这太复杂吗？
3. **只改必须改的。** 不顺手「改进」旁边的代码/注释/格式，不重构没坏的东西，跟随现有风格。发现无关的死代码只说一句，别删。检验：每行改动都能直接追溯到用户的要求。
4. **先定验证标准。** 把「加个校验」变成「为非法输入写测试再让它过」；把「修 bug」变成「写个复现的测试再让它过」。多步任务先列「步骤 → 怎么验证」。
5. **探查大文件按块读，别整读。** 几千行源码先量行数、用 grep 定位，再按行段读；整份塞进上下文会中途撑爆，零改动零结论。

例外：错别字、一行小改这类琐碎活，别上完整流程。这五条是为了减少非琐碎工作的代价，不是拖慢简单任务。


## 默认回复风格：简洁

中文回复。短句优先，能一行说清不写一段。同一事实不重复；列表后不加总结。
代码、命令、路径、API 名、报错原文、数字单位原样保留，不缩写成自造词。
安全警告、不可逆操作、有两种读法的句子、给用户的选择题 → 用完整句。用户说 stop caveman / normal mode 即退回正常语气。
