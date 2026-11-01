# tba 项目记忆 / skill 设计

> 状态：已实现（2026-10-27 定稿，37 测试通过、`cargo build --release` 零警告）  
> 一句话：**任务统一是普通 Task，靠工作流状态区分用途——`mem` = 一条项目记忆，`skill` = 一条 skill，正文都存任务备注（note）。**



---

## 1. 为什么最后是这个方案

这一路试了四个载体，每个都撞了墙。记下来，免得以后又绕回去。

### 1.1 任务备注（note）——能用，是最终方案

早期结论「`task.update` 的 `note` 会落到标题上、备注改不动」**是错的**（2026-10-26 记的，2026-10-27 实测推翻）：

- 只传 `{"taskId":.., "note": "..."}` → **备注更新成功，标题不变**；
- 5000 字写入完整读回，无截断；
- 混传 `content` + `note` 才会出问题（标题被写成正文）。

备注的优势是压倒性的：**内置字段**（不占自定义字段额度）、**网页任务详情直接可见可编辑**、不依赖任何插件式接口。

### 1.2 富文本自定义字段（rtf）——网页不渲染，已废弃

- `customfield-rtf-update` 有 bug：无论写多少，读回固定 **100 字符**；
- 绕法是 `customfield-update` + `jsonValue`（JSON 数组的字符串）；
- 单字段硬上限 **9999** 字符；
- **致命伤：Teambition 网页的任务详情不渲染 rtf 字段**，人在网页上看不到记忆；
- 而且 `customfield-create` 不接受 `type:"rtf"`，只能人工去网页建。

### 1.3 标签（tag）——没有创建权限，已废弃

- `task.update-tags` 能打**项目里已存在**的标签，打不存在的直接报  
  `tag name did not match a project tag (INVALID_ARGUMENT)`；
- 工具集里**没有 tag-create**，新项目要先让人去网页建标签——把「零手动」又破坏了。

### 1.4 任务类型（sfc）——自建类型是坏的，已废弃

- `sfc-create` 能建类型，但自建类型的 `scenariofields` 是空数组；
- 拿这种任务去建任务/打标签会报 `Scenariofieldconfig missed customfields (10032)`；
- 另外自定义字段有**数量上限**（主项目 24 个触顶，`FeatureLimit 10034`），  
  想「新项目自动建 text 字段」也会被卡。

### 1.5 工作流状态（tfs）——可以自由建，是最终答案

- `taskflow-update` 的 `tfsJson` 支持 upsert：  
  `{"upsert":[{"name":"mem","kind":"unset"}]}` → 状态建好，返回 200；
- `project.tfs-query` 能列出全部状态（含新建的），按名字就能拿到 id；
- 状态是**工作流的一等公民**：任务详情里能选、能筛、能建，且不占任何额度。

所以：**用状态当「用途标记」**。Task 类型不变，只是多了两个状态。

---

## 2. 数据模型

### 2.1 任务 = 一条记录

| 用途    | 任务类型     | 工作流状态              | 标题                | 备注       |
| ----- | -------- | ------------------ | ----------------- | -------- |
| 项目记忆  | Task（默认） | `mem`              | `{节名} · {正文首行摘要}` | 记忆正文     |
| skill | Task（默认） | `skill`            | skill 名字          | skill 正文 |
| 普通待办  | Task     | 待处理/进行中/待回复/已解决/关闭 | 任务标题              | 任务正文     |

**一条记忆 = 一条任务**（不是固定几个分片）：每次 `--append` 就新建一条，  
读取时把所有 `状态 = mem` 的任务按创建时间拼起来。

为什么这样比「固定分片 + 覆盖写」好：

- 写入是**纯 append**：不读旧内容、不改旧任务 → 两个 agent 同时写不会互相覆盖；
- 没有「先读再拼」这一步，就不会因为读失败而写坏；
- 单条上限 8000 字，一条记忆本来只该是几十到几百字的结论，够用。

代价：任务数会线性增长。所以 `--status` 会按节报条数，超过 `MEMORY_SECTION_LIMIT`（50）告警，  
由人决定什么时候合并淘汰。

### 2.2 节（section）

三个固定节：**红线 / 决策 / 踩坑**（`MEMORY_SECTIONS`，顺序即输出顺序）。

- 节名写在**标题开头**，是节名的唯一来源；
- `parse_memory_title` 只认「标题 == 节名」或「节名 + `·` / `：` / `:` 开头」，  
  认不出就当成普通任务——**宁可漏读，也不能把用户的普通任务读进记忆**；
- 用户手动改摘要（分隔符后面那段）不影响识别。

### 2.3 谁写

| 节  | 谁能写                     |
| -- | ----------------------- |
| 红线 | 只有用户（agent 要 `--force`） |
| 决策 | 只有用户（agent 要 `--force`） |
| 踩坑 | agent 可写                |

让 agent 自己定规矩等于没有规矩，所以红线/决策默认拒绝 agent。

### 2.4 本地配置

`data/projects/<id>/tb-memory.toml`（独立文件，不碰 `config.toml`）：

```toml
enabled = true       # 开关；关着时 tba memory 报错（不静默返回空）
state_name = "mem"   # 记忆状态名。留空 = mem
```

旧版本留下的 `sfc_name` / `cf_id` / `cf_name` / `shard_by_agent` / `task_id`  
会被 serde 忽略，不用手工清理（想干净就删掉这几行）。

---

## 3. 命令接口

```bash
tba memory --project <id>                          # 读：按节输出全部记忆
tba memory --project <id> --append "结论" --section 踩坑   # 加一条（新建一条 mem 任务）
tba memory --project <id> --file note.md --section 踩坑    # 从文件读（长文本）
tba memory --project <id> --overwrite "整节新内容" --section 踩坑 --force
tba memory --project <id> --on / --off             # 开关
tba memory --project <id> --status                 # 开关 / 状态 / 各节条数字数
tba memory --project <id> --init                   # 确保状态存在（幂等）
tba memory --project <id> --local / --import-local # 旧的本地记忆目录：查看 / 迁成踩坑记忆
```

`--overwrite` 的语义：**把这节的旧任务归档（进回收站、可恢复），再写一条新的**。  
Teambition 删任务不可逆，所以只归档不删。

`tba note <claim_id> --text "..."` 仍然写**任务评论**（不是备注）：  
备注是单值字段，追加要「先读再拼」且会互相覆盖；评论天然追加，适合会话流水账。

---

## 4. 实现要点（改这块之前先看）

### 4.1 识别：按状态查，不按标题前缀/标签/sfc

```rust
let state = ctx.find_state(project_id, "mem")?;   // project.tfs-query 按名字找
let tasks = ctx.list_tasks(project_id, &state)?;  // tql: taskflowstatusId = '<id>'
```

`list_tasks` 内部用 TQL `taskflowstatusId = 'sid'`，这是**工作流状态 id**，  
不是看板列 id（`stageId` 用错会返回 0 条但仍报成功，是最阴的坑）。

### 4.2 正文：必须走 `task.query` 批量读备注

`project.task-query`（列表）**不保证返回 note**，拿它当正文会退化成标题。  
所以：先列表拿 id，再 `teambition.task.query` 批量读（`taskId` 支持逗号分隔，  
`fields: ["taskId","content","note"]`），每批 30 个。

### 4.3 建任务：只能建在 start 状态

服务端硬规则：`task.create` 指定非 start 状态会报  
`taskflowstatus should be start kind`。所以统一走：

```
create_task(待处理) → update-status(tfsName = mem/skill)
```

封装在 `Ctx::create_marked_task(project, title, note, state)`。

### 4.4 建状态：`taskflow-update` 的 tfsJson

```rust
json!({"projectId": pid, "taskflowId": tf,
       "tfsJson": "{\"upsert\":[{\"name\":\"mem\",\"kind\":\"unset\"}]}"})
```

- `tfsJson` 是**字符串**，不是对象；
- `kind` 用 `unset`：既不是 start 也不是 end，不参与生命周期统计；
- 该工具是 `additionalProperties:false`，只能传 projectId / taskflowId / tfsJson。

### 4.5 改标题和改备注分两次调用

`update_task_content` 里：非空标题 → 单独一次 `{"content": ...}`；  
非空备注 → 单独一次 `{"note": ...}`。**不混传**，行为才确定。

### 4.6 涉及文件

| 文件              | 职责                                                                                                                                                                           |
| --------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/config.rs` | `MEMORY_STATE` / `SKILL_STATE` / 节常量 / 上限 / `TbMemory`                                                                                                                       |
| `src/ops.rs`    | `ensure_state` / `find_state` / `create_marked_task` / `list_memory_entries` / `append_memory` / `overwrite_memory` / `project_memory_status` / `list_skills` / `push_skill` |
| `src/cmds.rs`   | `tba memory` 分支                                                                                                                                                               |
| `src/ui.rs`     | GUI 记忆面板（开关 + 状态 + 初始化按钮）                                                                                                                                                    |

---

## 5. 已知限制

1. **备注单条上限**：实测 5000 字完整读回，硬上限设 8000、软上限 6000；  
   超硬上限直接报错让用户拆成几条（绝不静默截断）。
2. **任务数会增长**：一条记忆一条任务。`--status` 报条数，超过 50 条/节告警。  
   合并手段就是 `--overwrite`（归档旧的 + 写一条汇总）。
3. **状态是项目级的**：`mem` / `skill` 状态对所有 Task 可见，人在网页上可以手动  
   把普通任务拖进 mem 状态——那种任务的标题解析不出节名，所以**不会被读进记忆**。
4. **`tba claim` 不会领到记忆/skill**：它只查「待处理」状态，mem/skill 任务在别的状态，  
   天然不在候选里。
5. **agent 名不固定**：不做按 agent 分片（会话级名字每次都变，会导致任务爆炸）。

---

## 6. 迁移记录（2026-10-27）

从「RTF 分片」迁到「状态 mem + 备注」，实际做过的事：

1. 用 `taskflow-update` 的 tfsJson 建出 `skill`、`mem` 两个状态  
   （`skill` = `6ae013c6…`，`mem` = `6ae013c8…`）。
2. 旧分片任务（`项目记忆 1/2/3 · 节名`，rtf 字段里存内容）：  
   内容读出来重新 `--append` 成 mem 任务，然后把旧任务**归档**（没有删）。
3. 旧 skill 任务（sfc = 归档的 `skill` 类型、正文为空）归档；  
   真正有正文的那条「通用能力方案」改状态为 `skill`。
4. `tb-memory.toml` 精简成 `enabled` + `state_name`。

**旧的自定义字段（rtf「富文本」/ text「TB文本测试」）没有删**——  
API 删不了字段，要清只能网页手动；它们现在不再被 tba 使用。

---

## 7. 验收标准

- [x] `tba memory --init` 幂等，识别/创建 mem 状态
- [x] `--append` 新建一条 mem 任务，标题 `{节名} · {摘要}`，备注=正文
- [x] 多条记忆按创建时间聚合输出，按节分组
- [x] 红线/决策不加 `--force` 被拒；未知节被拒
- [x] `--overwrite` 归档旧条目再写新的
- [x] `--status` 报各节条数/字数
- [x] `tba skills` / `tba skill <名字>` 走状态 skill + 备注正文
- [x] 37 测试通过、`cargo build --release` 零警告
- [ ] 人在网页上确认：mem/skill 状态的任务详情能看到备注正文（备注是内置字段，必然显示）
