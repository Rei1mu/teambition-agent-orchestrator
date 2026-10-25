# TBHub

## 项目目标

**让同一个项目在任何设备、任何 agent 上都能无缝继续开发，同时把 token 开销压到极致，并保证 agent 不会跑错关键流程。**

要解决的是四件事：

1. **多设备、不同 agent 无缝续作**
   项目背景（skill）和进展（记忆）放在 Teambition 和本地 `data/`，不绑定任何一台机器、任何一个 agent。换设备、换 agent，挂上同一个项目就能从上次的进度接着干，上下文跟着项目走，而不是跟着工具走。

2. **极致减少 token 开销**
   agent 不必每次通读仓库、翻聊天记录来重建背景。`tb prompt` 只把该项目的启用 skill、共享记忆和要领的任务打包成一段文案，token 全花在真正干活的地方。

3. **不让 agent 跑错关键流程**
   关键流程不能只写成文字。文字 skill 只告诉 agent「做什么」，管不住它「怎么做」。比如「重载 _new.exe 做测试」这件事：仅靠文字描述，agent 可能因为权限、沙盒、文件占用，杀不掉也覆盖不了 _new.exe，甚至自作主张开了个 _new_text01.exe；skill 没读完整，它就不重启；脚本和 bash 环境有差异，它每次还换着花样执行。
   所以易错的、依赖环境的、有严格顺序的关键流程，要收进命令里，而不是留在文字里。agent 只负责调用；先做什么、后做什么、失败怎么处理、做完怎么校验，由命令保证——同样的输入，同样的结果，不靠 agent 的自觉。

4. **流程化、指标化**
   每个项目固定五个阶段（工作流状态）：`待处理` → `进行中` → `待回复` → `已解决` → `关闭`。需求进来进「待处理」，agent 领了进「进行中」，需要你拍板时进「待回复」，做完进「已解决」，不做进「关闭」。skill 不占阶段，而是「任务类型」维度：带 `skill` 类型的任务就是 skill，`tb claim` 领任务时自动跳过。agent 用 `tb claim` 领取创建时间最早的待处理任务，同一任务重复领返回原 claim_id，先记本地台账再移动状态——不重复、不跳步、不撞车。状态集中、口径统一，谁在做什么、积压多少、完成多少，看板上一眼可查：项目管理不靠嘴说，看数字。

## 它是什么

一个 Rust 二进制，三种用法：

- `tb` CLI：命令行。
- `tb serve`：MCP server（stdio）。任何支持 MCP 的 agent 挂载。
- `tb ui`：egui 窗口。卡片列项目，展开看 skill 和开关，复制 agent 文案。

数据放在 exe 同目录的 `data/`。所有 agent 共享这一个目录。

## 快速开始

第一次使用要先拿 Teambition 访问令牌。

**方式一：窗口（推荐）**

```bat
tb ui
```

没有令牌时窗口会显示引导面板：点「① 打开 Teambition 创建 Token」会自动打开浏览器，
把「查看详情」里复制的 Token ID 粘贴进输入框，点「③ 保存并连接」。

**方式二：命令行**

```bat
tb login                :: 自动打开 Token 页面并打印步骤
tb login --token <令牌>  :: 已有令牌直接登录
```

**怎么拿令牌**

令牌在 Teambition 的 User Token 页面创建：<https://open.teambition.com/user-mcp>

1. 用钉钉扫码登录 Teambition
2. 点「创建 Token」按钮
3. 填 Token 名称、有效期（建议选 1 年）、选择使用企业
4. 点确认，再点「查看详情」复制 Token ID

拿到之后：

```bat
tb login --token <你的Teambition访问令牌>
tb tools                :: 看 MCP 工具列表和角色识别结果
tb projects             :: 列项目
tb init --project <id>  :: 补齐 5 个阶段
```

token 存在 `data/config.toml`，登一次就行，重开、换 CLI/窗口/MCP 都不用再登。

**查看/切换配置**

```bat
tb config               :: 看当前配置（token 打码显示）
tb config --url <地址>   :: 切换 MCP 服务地址，token 不动
```

本地测试切到 mock 服务就用 `tb config --url http://127.0.0.1:8791/mcp`，不用手改文件，
已登录的 token 不会被覆盖。切回去：`tb config --url https://open.teambition.com/api/mcp/v2`。

每个项目必须有 4 个任务阶段（工作流状态）：

`待处理` → `进行中` → `已解决` → `关闭`

`tb init` 缺哪个建哪个。幂等。

skill 不是阶段，而是「任务类型」(sfc)：项目里有一个名为 `skill` 的任务类型，
带这个类型的任务就是 skill。任务类型查不到会自动创建。

> 注意：看板列（任务列表）无法通过 API 新建或改名（只有查询接口），需要的话在 Teambition 网页手动维护。

## skill

带 `skill` 任务类型的任务就是一条 skill。标题 = skill 名，内容 = skill 正文。skill 任务建在「已解决」，
「待处理」只留真正待办的任务；`tb claim` 领任务时会自动跳过 skill 类型。

```bat
tb skill-push --project <id> --file SKILL.md
tb skills --project <id>
tb skill <名字> --project <id>          :: 打印单个 skill 全文（agent 文案按需读它）
tb switch --project <id> --skill <名> --on
tb switch --project <id> --skill <名> --off
```

开关只存本地 `data/switches.toml`。不回写 Teambition。

## 接单 / 提问 / 等回复 / 收尾

任务在两个维度上表示：**状态（工作流状态）表示"是否在执行"，看板列表示"归属"**。

| 局面 | 状态 | 看板列 | 球在谁那 | 算完成 |
|---|---|---|---|---|
| 队列中，没人接 | 待处理 | 1 待处理 | — | 否 |
| agent 正在干 | 进行中 | 2 进行中 | agent | 否 |
| **agent 等你回复** | **待回复** | **3 待回复** | **你** | 否 |
| 干完 | 已解决 | 4 已解决 | — | 是 |
| 作废 | 关闭 | 5 关闭 | — | 是 |
| **会话异常断** | 待处理 | 1 待处理 | 回到队列 | 否 |

`kind`（状态类型）分配：待处理=`start`（全任务流唯一）、进行中/待回复=`unset`、已解决/关闭=`end`。
**待回复是 `unset` 而不是 `end`**——球在你那不等于干完了，否则完成率会虚高。

```bat
tb start --project <id> --file <md>   :: 开工：新建「进行中」任务并放进列「进行中」（文案里带了活时用）
tb take <链接|task_id>             :: 接单：进行中 + 列「进行中」
tb claim --project <id>            :: 从队列(列「待处理」)领最早的一条
tb ask <claim_id> --question "..."  :: 提问：待回复 + 列「待回复」，问题写成任务评论
tb wait <claim_id>                 :: 同步等回复：阻塞轮询，你在 Teambition 回一句就返回正文并继续
tb reply <claim_id>                :: 只看一眼：读回评论 → 进行中 + 列「进行中」（不阻塞）
tb inbox                           :: 列出所有待回复：谁在等、等了多久、问了什么、任务链接
tb hello [--name <tag>]            :: 登记会话标识（多会话并行时必须）
tb beat <claim_id>                 :: 续命（长构建/长测试前后调）
tb done <claim_id>                 :: 已解决 + 列「已解决」
tb close <claim_id>                :: 关闭 + 列「关闭」
tb release <claim_id> --reason "…"  :: 主动放弃：待处理 + 列「待处理」
tb show <链接|task_id>              :: 查一条任务真实的状态 + 看板列 + 是否完成
tb status                          :: 台账（含心跳年龄）
tb sweep                           :: 手动回收心跳超时的会话
```

规则：

1. `tb claim` 只领「**列「待处理」+ 状态待处理**」，并跳过台账里还被占着的任务。
2. 领任务自动跳过 `skill` 任务类型。
3. 先写本地台账，再改线上。改状态失败则台账标记 failed。领任务全程持文件锁，多会话并发不会抢到同一条。
4. **每条任务必须终结于 `ask` / `done` / `close` 之一**，不能半路停手。
5. 看板列名在 `data/config.toml` 里配（`tasklist_queue/claim/wait/done/closed`，默认**中文列名**）。
   列名不存在时只跳过移列、不阻断流程——状态才是硬约束。
   > 默认值从 `1/2/3/4` 改成了中文，是因为用数字在只有 4 列的旧项目上会把「已解决」错移进「关闭」列；
   > 中文名对不上只会「跳过移列」，安全得多。旧项目要么把网页列改成中文名，要么在 config 里显式写数字。
6. `take` / `start` 打印的「看板列: X -> Y」是**移完回读的真实列**，不是配置里的期望值。
   `tb start` 新建的任务由服务端落在「未分类」，所以 X 可能是「未分类」。

### 同步等回复：`tb wait`

用户可能在外面，只在 Teambition 上回复，不会回到会话里发一句「继续」。所以 agent 提问后不要结束本轮，
而是**阻塞等**：

```bat
tb ask  <claim_id> -q "问题"     :: 状态 → 待回复 + 列「待回复」
tb wait <claim_id>               :: 阻塞轮询，你回一句就返回正文，状态自动回「进行中 + 列进行中」
```

- 单轮默认 **540 秒**主动返回（Bash 工具上限 600 秒，超了会被转后台、agent 拿不到结果）。
- 退出码：`0` 读到回复（照回复继续干）/ `2` 本轮没等到（**再跑一次**接着等）/ `3` 任务在别处被收尾了。
- 「待回复」永不参与心跳超时回收，所以挂多久都安全。
- 你回完之后**不需要在会话里再说任何话**，agent 自己会继续。

### 两种开工方式（文案双分支）

`tb prompt` 生成的 agent 文案分两条路，agent 只按文案走，不自己判断：

| 文案里有没有派活 | agent 第一条命令 | 结果 |
|---|---|---|
| 没带任务（你只复制了卡片） | `tb claim --project <id>` | 从列「待处理」队列领最早的一条 |
| 带了任务（`tb prompt --task "…"` / UI 里填了「本次任务」） | `tb start --project <id> --file <md>` | 在列「进行中」新建一条「进行中」任务，由 agent 干 |

`tb start` 的两个细节：

- 标题取 `--title` > 正文首个 `# 标题` 行 > 文件名；正文落盘到 `data/tasks/<标题>.md`，agent 用 `--file` 读。
- **同名幂等**：本项目里已有同名任务的活动 claim 时直接复用，不会因为点了两次复制就建出两条。

### 多会话并行（WorkBuddy 开两个 + dsh 再开一个）

台账和 `data/` 是跟 exe 走的，所有会话天然共享，**不需要额外同步**。要做的只有一件事：
给每个会话一个不同的标识，否则两件坏事会发生——A 会话活着会把 B 会话的僵尸任务一起续命（永远回收不掉），
`sweep` 的「会话存活豁免」也会互相顶。

```bat
tb hello --name wb-a     :: 在 A 会话的工作目录里跑一次
tb hello --name wb-b     :: B 会话换个名字
tb hello                 :: 不介意共用就自动生成（s-3f2a1c）
```

标识写入 `<当前目录>/.tbhub/session`，之后该目录下所有 `tb` 命令自动带上（也可用环境变量 `TB_AGENT` 覆盖）。
**同一目录开两个会话必须显式 `--name` 区分**，否则后一个会覆盖前一个。

### 会话断开怎么自动退回队列

四层防线：

1. **正常收尾**：`ask` / `done` / `close`，skill 里强制。
2. **同步等待中**：agent 在 `tb wait` 里挂着，会话没结束就一直在（「待回复」本来就不参与回收）。
3. **进程退出清理**：`tb serve` 每 30 秒写会话心跳；会话结束（窗口关闭/进程退出）时自动把本会话 `running` 的任务退回 待处理 + 列「待处理」。`waiting` 的不动（那是在等你）。
4. **心跳超时兜底**：`running` 且心跳超过 `heartbeat_timeout_secs`（默认 300 秒）→ 判定会话已死 → 退回 待处理 + 列「待处理」，并在任务上留一条评论说明。任何 `tb` 命令启动时顺带扫一次。

另外：**任何 `tb` 命令启动时都会给本会话占着的 running 任务刷一次心跳**（`sweep` 除外），
所以长构建期间只要中途跑过一次 tb 就不会被误判。

强杀（进程被直接终止）时第 3 层不执行，靠第 4 层兜底；如果之后**再也没有任何 tb 命令被调用**，回滚会等到下次有人跑 `tb` 时才发生——这是没有常驻进程时的固有限制。

## 通用能力：重载

关键流程不靠文字描述，固化成一个命令：**编译 → 杀旧 → 复制校验 → 启动**。

```bat
tb reload build      :: 重载（编译+替换+启动）
tb reload restart    :: 只重启，不编译
tb reload kill       :: 杀掉运行副本
tb reload status     :: 看是否在跑
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
tb reload build --root D:\other\project
tb reload build --root D:\other\project --name myapp --live D:\other\project\run\myapp_test.exe
```

脚本随二进制内置，运行时释放到 `data/capabilities/reload.ps1`，不用单独维护。
工具链路径可用环境变量覆盖：`TB_MSVC_BIN` / `TB_MSVC_LIB` / `TB_SDK_LIB` / `TB_EXTRA_LIB` / `TB_CARGO`。

> 杀软对 `*_test.exe` 这类名字比较敏感，若误报拦截启动，用 `--live` 换个名字即可：
> `tb reload build --live D:\dev\mcp\target\release\tb_run.exe`

## 共享记忆

```bat
tb memory --project <id> --write "进展：..."
tb memory --project <id>                 :: 读
```

文件在 `data/projects/<id>/memory/`。

## agent 文案

```bat
tb prompt --project <id>                              :: 不给任务 → 文案让 agent 去队列(列「待处理」)领
tb prompt --project <id> --task "# 标题
正文…"                                                 :: 给了任务 → 文案让 agent 用 tb start 新建
tb prompt --project <id> --task-file .\todo.md        :: 任务正文从文件读
```

或窗口里点卡片 → 两个复制按钮：

| 按钮 | 行为 | agent 拿到后 |
|---|---|---|
| `复制agent文案(本次任务)` | 带上卡片上「本次任务」输入框里填的内容 | 跑 `tb start --project … --file …`，在列「进行中」新建任务 |
| `复制agent文案(领列1任务)` | **强制丢弃**输入框里的残留草稿 | 跑 `tb claim --project …`，去列「待处理」领队列任务 |

两个按钮的差别是**刻意的**：点「领列1任务」时哪怕输入框里还留着上次写的草稿，文案里也不会出现——否则 agent 会自建任务而不是去领，行为就跑偏了。

### 省 token 设计（英文速查表 + skill 按需读）

文案是发给 agent 的、人基本不读，所以按 token 成本优化过：

- **闭环指令是英文速查表**。实测同义内容中文约 882 token、英文约 540 token（cl100k）——中文字在 BPE 分词里普遍 1 字 1+ token，英文 1 token 能装好几个字母。阶段名（待处理/进行中/…）保持中文，那是 Teambition 上的真实名字，改了 agent 对不上。
- **skill 不内联全文，只列名字**。以前每次发文案都把全部启用 skill 的全文贴进去，skill 越多越大越贵；现在文案只给名字列表 + 一条读取命令，agent 用到哪个读哪个：

```
Enabled skills (3): 部署流程, 代码规范, 发布 checklist
Read one on demand (do NOT read all): `tb skill <name> --project <id>`
```

```bat
tb skill 部署流程 --project <id>   :: 打印该 skill 全文
```

不变量一条没丢（只经 tb 改状态/列、别自建任务、每条必须终结于 ask/done/close、长操作前后 beat、待回复永不超时、心跳超时退回队列、退出码 0/2/3 语义），由单测 `workflow_keeps_key_invariants` 守着；`prompt_with_huge_skill_stays_small` 防体积回潮。测量脚本：`tools/token_cmp.py`。

文案含：任务标题/正文、已启用 skill 名字列表、共享记忆目录、开工命令（`claim` 或 `start`）、收尾规则。

`--task` / `--task-file` 的正文会落盘到 `data/tasks/<标题>.md`，文案里给出绝对路径，agent 直接用 `tb start --file` 读，不用你贴正文。

## 窗口设置（字号）

窗口左侧栏 → `⚙ 设置`，可以调字号（12–40，默认 **22**）。egui 自带约 14pt，中文在这个尺寸下很吃力，所以默认给 22。

改完立刻生效（正文、侧栏、按钮一起等比放大），并落盘到 `data/ui.toml`：

```toml
font_size = 22.0
```

> 界面偏好单独存 `data/ui.toml`，不写 `config.toml`——后者存着访问令牌和工具映射，整体覆写风险太大。

## MCP 接入

在 agent 的 MCP 配置里加：

```json
{
  "mcpServers": {
    "tbhub": {
      "command": "tb.exe",
      "args": ["serve"]
    }
  }
}
```

工具：list_projects / list_project_skills / set_switch / list_tasks / ensure_stages / claim_task / complete_task / close_task / read_memory / write_memory / get_agent_prompt。

## 工具识别不准时

Teambition MCP 的工具名由服务端动态下发。自动识别靠名字关键词。不准时：

1. `tb tools` 看真实工具名。
2. 编辑 `data/config.toml`，在 `[tool_overrides]` 里指定，例如：

```toml
[tool_overrides]
list_projects = "真实工具名"
claim_task = "真实工具名"
```

可用角色：list_projects / get_project / list_taskflows / get_taskflow / create_taskflow / list_stages / create_stage / create_task / update_task / move_task / list_tasks / get_task。

改完运行 `tb tools --refresh`。

## 排查参数问题

真实 Teambition MCP 的工具名长这样：`teambition.project.query`、`teambition.task.query`、`teambition.taskflowstatus.query`。自动识别按名字关键词打分，上面这些都能对上。

要看某个工具到底要什么参数：

```bat
tb tools --refresh --json
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

`tb` 已经按这些实现：`list_projects` 自动传 `my=true`、`list_tasks` 自动拼 TQL 并带 `isArchived = false`、按 `nextPageToken` 翻页、skill 正文写进 `note`。

如果换工具后遇到同类报错，把 `tb tools --refresh --json` 的输出发出来，按 schema 补参数即可。

## OAuth（可选）

`data/config.toml` 填 client_id / client_secret / authorize_url / token_url 后，`tb login` 走本地回环授权。默认用 User Token 最省事。

## 本地测试（不连真实 Teambition）

项目带一个模拟 Teambition MCP 服务，用来验证全流程：

```bat
python tools\mock_tb_mcp.py 8791
```

然后把 `data/config.toml` 的 `mcp_url` 改成 `http://127.0.0.1:8791/mcp`，`access_token` 随便填个非空值。
模拟服务内置 2 个项目、11 个工具，故意把 Alpha 项目的阶段设成只有 2 个，用来验证 `tb init` 自动补齐。

注意：模拟服务是**多线程内存版**，重启后状态全丢（阶段、任务、skill 都回初始值），
但本地开关和台账不受影响。
