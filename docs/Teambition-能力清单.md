# Teambition MCP 能力清单

## 1. 数据来源

- 来源：真实 Teambition 服务 `tools/list` 的返回，缓存在 `target/release/data/tools-cache.json`。
- 端点：`https://open.teambition.com/api/mcp/v2`。
- 工具总数：**150**。
- 所有工具名以 `teambition.` 为前缀。下文表格省略该前缀。

## 2. 先说结论：不能做的操作

### 2.1 任务列表（看板列 / task list）只有「查询」

| 想要的操作 | 有没有 | 对应工具 |
|---|---|---|
| 查询任务列表 | 有 | `project.task-list-query` |
| 新建任务列表 | **没有** | 无 |
| 重命名任务列表 | **没有** | 无 |
| 删除任务列表 | **没有** | 无 |
| 调整列顺序 | **没有** | 无 |

依据：`project.task-list-query` 的描述是「查询项目任务列表（API stage）」。150 个工具里名字含 `list` 的只有 7 个，其中与任务列表相关的只有它一个。`task.update-list` 与 `task.update-stage` 是「把任务移到某个列表」，不是「改列表本身」。

结论：**任务列表只能在 Teambition 网页端手工建、改名、删除。API 侧只读。**

### 2.2 三个容易被误认成「建列表」的工具

| 工具 | 实际作用 | 为什么容易混 |
|---|---|---|
| `project.task-group-query` | 查询**任务分组** | 分组和列表都是归类容器，但分组同样只有查询，没有增删改 |
| `project.smartgroup-create` | 创建**自定义视图**（看板/表格/列表/时间线） | 名字带 list，但建的是视图，不是看板列 |
| `project.sfc-create` | 创建**任务类型** | 曾被当成建列表用。sfc 是第四个维度，与列无关 |

### 2.3 任务列表相关的可行替代

| 目标 | 做法 |
|---|---|
| 让任务出现在某一列 | `task.update-list`（按列表 ID 或当前分组默认列表）或 `task.update-stage`（按列表名称/ID） |
| 看某列有哪些任务 | `project.task-list-query` 取列 ID，再用 `project.task-query` 按 `stageId` 过滤 |
| 换一种分组展示 | `smartgroup-create` 建视图，按标签/字段筛选，绕开固定列 |

## 3. 四个维度别混

Teambition 的任务有四个独立维度。工具名很像，混用会静默失败（返回成功但 0 条）。

| 维度 | 字段 | 含义 | 可写 |
|---|---|---|---|
| 看板列 | `stageId` | 任务列表 / task list | **否，只读** |
| 工作流状态 | `taskflowstatusId` / `tfsId` | TBHub 的「阶段」 | 是，`task.update-status` |
| 工作流 | `taskflowId` | 状态流的容器 | 是，taskflow 系列 |
| 任务类型 | `sfcId` | 任务类型（skill 类型就是它） | 是，`task.update-type` / `sfc-create` |

另两条实测经验：

1. 按阶段筛任务要用 `taskflowstatusId`。用 `stageId` 会 0 条，但接口报成功。
2. 工作流状态不能真正改名。`taskflow-update` 的 `tfsJson {"update":[...]}` 返回 200 但无效果。改名只能 `upsert` 建新状态 + `remove` 删旧状态。

## 4. 全量工具清单（150 个）

### 4.1 企业 / 身份 / 钉钉（7）

| 工具 | 说明 |
|---|---|
| `org.info` | 查询当前 Token 所属企业信息 |
| `user.me` | 查询当前用户的企业成员信息与钉钉上下文 |
| `member.query` | 按关键词、用户 ID、邮箱、工号查询企业成员 |
| `dingtalk.group-projects` | 钉钉群 openConversationId 映射为项目 ID |
| `dingtalk.project-groups` | 项目 ID 映射为关联钉钉群 |
| `dingtalk.user-to-dingtalk` | Teambition 用户映射为钉钉 staffId |
| `dingtalk.user-to-teambition` | 钉钉 staffId 映射为 Teambition 用户 ID |

### 4.2 文档（2）

| 工具 | 说明 |
|---|---|
| `docs.get` | 按层级路径读取模块、工作流或具体工具文档 |
| `docs.search` | 按目标搜索模块、工作流与工具文档 |

### 4.3 项目（6）

| 工具 | 说明 |
|---|---|
| `project.query` | 按项目 ID、我参与的项目或企业级 TQL 查询项目（三选一） |
| `project.create` | 创建空白项目或按模板 ID 创建 |
| `project.create-from-template` | 从模板创建项目（按 ID 或名称匹配） |
| `project.update` | 更新项目名称、描述、封面、可见性、起止时间 |
| `project.archive` | 把项目放入回收站（可恢复） |
| `project.template-query` | 查询企业模板与官方模板 |

### 4.4 项目集 program（7）

| 工具 | 说明 |
|---|---|
| `program.create` | 创建企业级项目集 |
| `program.update` | 更新项目集基本信息 |
| `program.delete` | 删除项目集及关联虚拟项目，项目本身保留 |
| `program.query` | 查询可见项目集列表 |
| `program.project-add` | 批量把已有项目加入项目集，单次最多 1000 |
| `program.project-remove` | 从项目集移除单个项目（仅解关联） |
| `program.project-query` | 查询项目集内的项目列表 |

### 4.5 计划 plan（7）

| 工具 | 说明 |
|---|---|
| `plan.create` | 创建顶层计划或子计划 |
| `plan.query` | 查询可见计划，可按层级、父计划、ID 筛选 |
| `plan.status-query` | 查询计划状态与可执行的后续动作 |
| `plan.status-execute` | 执行发布、变更、取消变更等状态动作 |
| `plan.task-query` | 查询计划内任务，支持 TQL 与分页 |
| `plan.task-update` | 更新计划任务标题或日期（仅 AppToken） |
| `plan.task-archive` | 把计划任务移入回收站 |

### 4.6 任务（21）

| 工具 | 说明 |
|---|---|
| `task.create` | 创建任务或子任务，可指定分组、列表、类型、初始状态、自定义字段 |
| `task.query` | 按任务 ID、父任务 ID 或企业级 TQL 查询任务 |
| `project.task-query` | 项目内按 TQL、未完成或分配给我筛选任务 |
| `task.update` | 批量更新标题、备注、负责人、日期、优先级、迭代、简单字段 |
| `task.delete` | 永久删除任务（不可逆） |
| `task.archive` | 归档到回收站（可恢复） |
| `task.restore` | 从回收站恢复任务 |
| `task.move` | 把任务移到其他项目 |
| `task.update-status` | 更新工作流状态（按状态 ID 或名称） |
| `task.update-list` | 按列表 ID 或当前分组默认列表移动任务，限本项目 |
| `task.update-stage` | 按名称或 ID 把任务调整到指定任务列表 |
| `task.update-type` | 更新任务类型，会同步切换到目标类型绑定的工作流状态 |
| `task.update-parent` | 设为同项目目标任务的子任务（不能脱离父任务） |
| `task.update-members` | 添加、删除或完整替换任务参与者 |
| `task.update-tags` | 添加、删除或替换任务标签 |
| `task.update-node` | 更新当前节点的负责人与计划时间 |
| `task.dispatch` | 向多名执行人异步派发任务 |
| `task.user-search` | 按当前用户角色跨项目搜索企业任务 |
| `task.comment` | 发表评论，支持 Markdown、@提及、附件令牌 |
| `task.activity` | 获取任务动态（评论、状态变更、内容更新） |
| `task.html-show` | 渲染任务备注或富文本字段为完整 HTML |

### 4.7 任务类型 sfc（3）

| 工具 | 说明 |
|---|---|
| `project.sfc-query` | 查询项目任务类型，可选补工作流与状态 |
| `project.sfc-create` | 创建项目级或企业级任务类型，可为项目级绑定工作流 |
| `project.sfc-field-upsert` | 为任务类型绑定或更新自定义字段 |

### 4.8 工作流与状态（20）

| 工具 | 说明 |
|---|---|
| `project.taskflow-query` | 查询项目任务工作流 |
| `project.taskflow-create` | 创建普通工作流（只管状态流转） |
| `project.taskflow-nodeflow-create` | 创建节点工作流，自带默认节点 |
| `project.taskflow-update` | 编辑节点工作流：改名、节点增删改、状态管理 |
| `project.taskflow-update-name` | 仅重命名节点工作流 |
| `project.taskflow-node-query` | 查询工作流节点配置与前后节点关系 |
| `project.tfs-query` | 按当前项目有效工作流查询工作流状态（TBHub 的「阶段」） |
| `project.org-tfs-search` | 企业级跨项目搜索工作流状态 |
| `project.status-executor-query` | 查询某状态流转的有权执行人 |
| `project.status-validator-query` | 查询状态的流转字段校验规则 |
| `project.status-customfield-query` | 查询项目概览展示的自定义字段与当前值 |
| `project.status-customfield-update` | 设置项目概览自定义字段值 |
| `task.tfs-query` | 查询任务类型所绑定工作流的全部状态定义 |
| `task.tfs-available` | 按 rejectStatusIds 本地推算可流转状态（非后端权威） |
| `task.node-list` | 查询任务全部非隐藏节点实例 |
| `task.node-next` | 本地计算当前节点与直接后续节点 |
| `task.node-query` | 按节点 ID 查询节点详情，或查全部进行中节点 |
| `task.node-done` | 提交节点完成，满足条件后触发流转 |
| `task.node-redo` | 把已完成节点重置为进行中并重算运行态 |
| `task.node-customfield-update` | 在节点上下文中更新流转自定义字段 |

### 4.9 任务列表 / 分组 / 视图（6）

| 工具 | 说明 |
|---|---|
| `project.task-list-query` | **查询**项目任务列表（看板列）。**只有这一个，只读** |
| `project.task-group-query` | **查询**项目任务分组。同样只读 |
| `project.smartgroup-create` | 创建自定义视图 / 智能分组 |
| `project.smartgroup-get` | 按 ID 取视图配置详情（不含任务数据） |
| `project.smartgroup-search` | 搜索项目内视图 |
| `project.smartgroup-task-query` | 查询指定视图下的任务列表 |

### 4.10 自定义字段（9）

| 工具 | 说明 |
|---|---|
| `project.customfield-query` | 查询项目字段定义 |
| `project.customfield-create` | 创建字段，支持文本/数字/日期/单选/多选 |
| `project.deliverable-query` | 查询 type=work 的交付物定义 |
| `task.customfield-update` | 用 JSON 数组覆盖任务字段值 |
| `task.customfield-file-update` | 用已上传令牌覆盖文件字段 |
| `task.customfield-member-update` | 更新或清空成员字段 |
| `task.customfield-rtf-update` | Markdown 转富文本写入字段 |
| `task.customfield-table-query` | 查询表格字段的表头与全部行 |
| `task.customfield-table-update` | 表格字段的行与单元格批量操作 |

### 4.11 标签 / 优先级 / 角色 / 成员 / 关联（12）

| 工具 | 说明 |
|---|---|
| `project.tag-query` | 查询项目级与企业级标签 |
| `project.priority-query` | 查询任务优先级与默认优先级 |
| `project.role-query` | 查询项目角色 |
| `project.member-query` | 查询项目成员，自动遍历分页 |
| `project.member-add` | 批量添加 1–100 名成员 |
| `project.member-delete` | 批量移除 1–100 名成员 |
| `project.objectlink-query` | 查询项目关联（内部对象与外部链接） |
| `project.objectlink-create` | 创建外部网页或钉钉文档链接 |
| `project.objectlink-delete` | 软删除项目关联 |
| `task.objectlink-query` | 查询任务关联链接 |
| `task.objectlink-create` | 为任务关联内部对象或创建链接 |
| `task.objectlink-delete` | 按关联 ID 删除任务关联 |

### 4.12 文件（16）

| 工具 | 说明 |
|---|---|
| `project.file-list` | 列出项目文件库文件与文件夹 |
| `project.file-search` | 跨目录按关键词搜索文件与文件夹 |
| `project.file-detail-query` | 按文件 ID 批量查详情与签名下载链接 |
| `project.file-batch-detail` | 按 resourceId 批量查文件详情，一次最多 50 个 |
| `project.file-create` | 登记已上传的 fileToken 为新文件 |
| `project.file-move` | 项目内移动单个文件 |
| `project.file-name-update` | 重命名文件 |
| `project.file-main-version-set` | 切换文件的主版本 |
| `project.file-version-update` | 为文件追加上传新版本 |
| `project.folder-create` | 创建文件夹，可指定父级 |
| `project.folder-move` | 移动文件夹，支持跨项目 |
| `project.folder-title-update` | 重命名文件夹 |
| `project.folder-visibility-update` | 修改文件夹可见范围 |
| `project.upload-token-create` | 创建文件上传凭证 |
| `task.file-query` | 按资源 ID、评论附件、字段查文件元数据 |
| `task.upload-token-create` | 为评论或文件字段创建附件上传凭证 |

### 4.13 迭代 sprint（8）

| 工具 | 说明 |
|---|---|
| `project.sprint-query` | 查询项目迭代 |
| `project.sprint-create` | 创建未开始迭代 |
| `project.sprint-update` | 更新迭代基本信息，不改状态 |
| `project.sprint-start` | 未开始 → 进行中 |
| `project.sprint-complete` | 进行中 → 已完成 |
| `project.sprint-restart` | 已完成 → 进行中 |
| `project.sprint-lock-update` | 更新迭代锁定配置 |
| `project.sprint-delete` | 硬删除迭代，不可恢复 |

### 4.14 统计与报表（11）

| 工具 | 说明 |
|---|---|
| `project.project-stats` | 单项目内按维度聚合统计任务 |
| `project.org-stats` | 跨项目 / 企业级聚合统计 |
| `project.project-column-map` | 项目级统计可用字段映射 |
| `project.org-column-map` | 企业级统计可用字段映射 |
| `project.project-filter-options` | 项目统计筛选字段可选值 |
| `project.org-filter-options` | 企业统计筛选字段可选值 |
| `project.org-customfield-search` | 搜索企业自定义字段 |
| `project.report-list` | 列出项目自定义图表 |
| `project.report-data` | 查询单个自定义图表的数据 |
| `project.task-count` | 按与 task-query 相同的条件独立计数 |
| `project.task-progress-status` | 查询「任务进展」应用启用状态 |

### 4.15 依赖与进展（6）

| 工具 | 说明 |
|---|---|
| `task.dependency-query` | 查询任务的前置与后置依赖 |
| `task.dependency-create` | 创建前置到后置的依赖关系 |
| `task.dependency-delete` | 按依赖 ID 删除依赖关系 |
| `task.trace-query` | 按新到旧分页查询任务进展 |
| `task.trace-create` | 创建文本进展并通知提醒人 |
| `task.trace-update` | 修改最新一条进展 |

### 4.16 工时 worktime（9）

| 工具 | 说明 |
|---|---|
| `worktime.create` | 为任务的一个或多个日期创建实际工时 |
| `worktime.list` | 查询任务的全部实际工时 |
| `worktime.update` | 更新实际工时记录 |
| `worktime.plantime-create` | 创建或累加计划工时 |
| `worktime.plantime-list` | 查询任务的未删除计划工时 |
| `worktime.plantime-update` | 更新计划工时记录 |
| `worktime.plantime-delete` | 标记删除一条计划工时 |
| `worktime.plantime-sum` | 汇总计划工时总量与明细 |
| `worktime.plantime-aggregation` | 按日期范围与成员聚合计划工时 |

## 5. TBHub 用到的工具映射

`target/release/data/config.toml` 的 `[tool_overrides]` 固定了以下映射：

| TBHub 角色 | 工具 |
|---|---|
| list_projects / get_project | `project.query` |
| list_taskflows | `project.taskflow-query` |
| create_taskflow | `project.taskflow-create` |
| list_stages（阶段） | `project.tfs-query` |
| create_stage（改状态名） | `project.taskflow-update`（upsert + remove） |
| create_task | `task.create` |
| update_task | `task.update` |
| move_task（改状态） | `task.update-status` |
| list_tasks | `project.task-query` |
| get_task | `task.query` |

## 6. 一句话总结

Teambition API 覆盖项目、任务、工作流、任务类型、字段、文件、工时、统计，写能力很强。
但**看板列（任务列表）和任务分组是只读的**，只能在网页端维护。
