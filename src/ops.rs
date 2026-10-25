use crate::config::*;
use crate::store::{new_id, Claim, Store};
use crate::tb::Tb;
use crate::tools::*;
use serde_json::json;


// 高层业务操作。CLI / MCP / UI 三处共用。

// 工作流状态的 kind 三态（Teambition）：start=开始 / unset=进行中 / end=结束。
// 只有 kind=end 的任务才算「已完成」(isDone=true)。按 TBHub 的阶段名固定分配：
//   待处理=start（新任务落这里）、进行中=unset、已解决/关闭=end。
// 约束：一个任务流只能有一个 start，若已被别的状态占用，待处理退化为 unset。
fn stage_kind(name: &str, has_start: bool) -> &'static str {
    match name {
        STAGE_TODO => {
            if has_start {
                "unset"
            } else {
                "start"
            }
        }
        // 待回复 = 球在用户那，但任务没完成。必须 unset，
        // 用 end 会被算成「已完成」，完成率就假了。
        STAGE_DOING | STAGE_WAIT => "unset",
        _ => "end",
    }
}

// 秒 -> 人话。秒级阈值下别显示成「1 分钟」，容易让人以为算错了。
pub fn human_age(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 86400 {
        format!("{} 天", s / 86400)
    } else if s >= 3600 {
        format!("{} 小时", s / 3600)
    } else if s >= 60 {
        format!("{} 分钟", s / 60)
    } else {
        format!("{} 秒", s.max(1))
    }
}

// ---- 会话标识 ----
//
// 多会话并行时必须能区分「谁在干哪条」，否则两件事会坏：
//   1. sweep 的「会话还活着就豁免」会互相顶——A 会话活着，B 会话的僵尸任务
//      就永远回收不掉（因为共用一个 agent 名，心跳文件是同一份）。
//   2. 任何 tb 命令顺带批量续命时，会把别人的任务也续上。
//
// 解析顺序：TB_AGENT 环境变量 > 向上找 .tbhub/session 文件 > "cli"。
// 用文件而不是环境变量作主通道，是因为 agent 每次跑命令都是新 shell，
// 环境变量不跨调用保留，而文件在。
pub fn current_agent() -> String {
    if let Ok(v) = std::env::var("TB_AGENT") {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        let p = d.join(".tbhub").join("session");
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Some(t) = s
                .lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty() && !l.starts_with('#'))
            {
                return t.to_string();
            }
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    "cli".to_string()
}

// 生成并记住当前目录的会话标识（tb hello）。
// 同一目录要跑多个会话，就用 --name 分别指定。
pub fn set_session(name: Option<&str>) -> Result<(String, std::path::PathBuf), String> {
    let tag = match name.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => {
            // 没有 rand 依赖，用纳秒时间戳 + pid 凑一个够用的短标识。
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            format!("s-{:06x}", (nanos ^ ((std::process::id() as u64) << 13)) & 0xff_ffff)
        }
    };
    let dir = std::env::current_dir().map_err(|e| format!("拿不到当前目录: {e}"))?;
    let d = dir.join(".tbhub");
    std::fs::create_dir_all(&d).map_err(|e| format!("创建 .tbhub 目录失败: {e}"))?;
    let path = d.join("session");
    std::fs::write(&path, format!("{tag}\n")).map_err(|e| format!("写会话标识失败: {e}"))?;
    Ok((tag, path))
}

pub struct Ctx {
    pub tb: Tb,
    pub map: ToolMap,
}

impl Ctx {
    pub fn new(store: Store, refresh: bool) -> Result<Ctx, String> {
        let mut tb = Tb::new(store)?;
        tb.discover(refresh)?;
        let overrides = tb.cfg.tool_overrides.clone();
        let map = ToolMap::resolve(&tb.tools, &overrides);
        Ok(Ctx { tb, map })
    }

    fn need(&self, role: &str, name: &Option<String>) -> Result<String, String> {
        name.clone().ok_or_else(|| {
            format!(
                "MCP 工具列表里找不到「{role}」角色的工具。请运行 tb tools 查看真实工具名，\
                 然后在 data/config.toml 的 [tool_overrides] 里手工指定，例如 {role} = \"真实工具名\""
            )
        })
    }

    pub fn list_projects(&mut self) -> Result<Vec<ProjectInfo>, String> {
        let tool = self.need("list_projects", &self.map.list_projects)?;
        // teambition.project.query 要求 projectId / tql / my 至少给一个。
        // 文档推荐用 TQL 的 involveMembers = me() 查"我参与的项目"，
        // 但 my=true 也被服务端明确接受，放第一位最省事。
        let candidates: [serde_json::Value; 5] = [
            json!({"my": true}),
            json!({"tql": "involveMembers = me() AND isTemplate = false"}),
            json!({"my": true, "pageSize": 100}),
            json!({"tql": "isTemplate = false"}),
            json!({}),
        ];
        let mut last_err = String::new();
        for base in candidates {
            match self.list_projects_page(&tool, base) {
                Ok(list) => {
                    let mut out = list;
                    out.sort_by(|a, b| a.name.cmp(&b.name));
                    out.dedup_by(|a, b| a.id == b.id);
                    return Ok(out);
                }
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    // 带分页地拉项目。服务端返回 nextPageToken 就继续翻。
    fn list_projects_page(
        &mut self,
        tool: &str,
        base: serde_json::Value,
    ) -> Result<Vec<ProjectInfo>, String> {
        let mut out: Vec<ProjectInfo> = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..20 {
            let mut args = base.clone();
            if let Some(t) = &page_token {
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("nextPageToken".to_string(), json!(t));
                }
            }
            let res = self.tb.call(tool, args)?;
            let next = get_str(&res, &["nextPageToken", "next_page_token", "pageToken"]);
            let items: Vec<ProjectInfo> = extract_list(&res, 3)
                .iter()
                .filter_map(parse_project)
                .collect();
            out.extend(items);
            match next {
                Some(t) if !t.is_empty() => page_token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    // 找项目的任务流 id。
    fn taskflow_id(&mut self, project_id: &str) -> Result<String, String> {
        if let Some(t) = &self.map.get_project {
            let res = self
                .tb
                .call_smart(t, &[("projectId", json!(project_id))])?;
            if let Some(id) = get_str(
                &res,
                &["taskflowId", "taskflow_id", "taskflow", "flowId", "flow_id"],
            ) {
                return Ok(id);
            }
            // 有时包了一层
            let inner = extract_list(&res, 2).into_iter().next();
            if let Some(inner) = inner {
                if let Some(id) = get_str(
                    &inner,
                    &["taskflowId", "taskflow_id", "taskflow", "flowId", "flow_id"],
                ) {
                    return Ok(id);
                }
            }
        }
        if let Some(t) = &self.map.list_taskflows {
            let res = self
                .tb
                .call_smart(t, &[("projectId", json!(project_id))])?;
            if let Some(first) = extract_list(&res, 3).into_iter().next() {
                if let Some(id) = get_str(&first, &["id", "_id", "taskflowId"]) {
                    return Ok(id);
                }
            }
        }
        Err("找不到项目的任务流。项目可能还没有任务流。".to_string())
    }

    pub fn list_stages(&mut self, project_id: &str) -> Result<Vec<StageInfo>, String> {
        let tf = self.taskflow_id(project_id)?;
        let mut out: Vec<StageInfo> = Vec::new();
        if let Some(t) = &self.map.list_stages {
            let res = self.tb.call_smart(
                t,
                &[
                    ("projectId", json!(project_id)),
                    ("taskflowId", json!(tf)),
                ],
            )?;
            out = extract_list(&res, 3)
                .iter()
                .filter_map(parse_stage)
                .collect();
        }
        if out.is_empty() {
            if let Some(t) = &self.map.get_taskflow {
                let res = self
                    .tb
                    .call_smart(t, &[("taskflowId", json!(tf)), ("projectId", json!(project_id))])?;
                out = extract_list(&res, 3)
                    .iter()
                    .filter_map(parse_stage)
                    .collect();
            }
        }
        out.sort_by_key(|s| s.name.clone());
        out.dedup_by(|a, b| a.id == b.id);
        Ok(out)
    }

    // 建一个工作流状态（= TBHub 的阶段）。
    //
    // 真实 Teambition 用 project.taskflow-update 的 tfsJson upsert：
    //   {"upsert":[{"name":"X","kind":"end"}]}
    // 该工具 schema 是 additionalProperties:false，只能传
    // projectId / taskflowId / tfsJson，**传 name 会被整体拒掉**
    // （name 在那个工具里是「重命名工作流」，语义完全不对）。
    // mock 用 taskflowstatus.create，直接收 name/title。
    // 两套参数形状不兼容，按工具名分派，别混发。
    fn create_stage(
        &mut self,
        project_id: &str,
        tf: &str,
        name: &str,
        kind: &str,
    ) -> Result<(), String> {
        let tools = self.tool_candidates(
            &self.map.create_stage,
            &[
                "teambition.project.taskflow-update",
                "teambition.taskflowstatus.create",
            ],
        );
        let tfs = json!({"upsert": [{"name": name, "kind": kind}]}).to_string();
        let mut last_err = String::new();
        for t in &tools {
            let args = if t.contains("taskflow-update") {
                json!({"projectId": project_id, "taskflowId": tf, "tfsJson": tfs})
            } else {
                json!({"projectId": project_id, "taskflowId": tf,
                       "name": name, "title": name, "kind": kind})
            };
            match self.tb.call(t, args) {
                Ok(_) => return Ok(()),
                Err(e) => last_err = e,
            }
        }
        Err(if last_err.is_empty() {
            "找不到可用的建阶段工具".to_string()
        } else {
            last_err
        })
    }

    // 补齐 4 个阶段（工作流状态 / tfs）。缺哪个建哪个。返回最终阶段列表。
    //
    // kind 三态（实测 + 官方文档）：start(开始) / unset(进行中) / end(结束)。
    // 按阶段名固定分配：待处理=start、进行中=unset、已解决/关闭=end。
    // 约束：start 整个任务流只能有一个（建第二个报 ShouldOnlyOneStartStatus），
    // 所以若 start 已被别的状态占用，待处理退化为 unset。
    pub fn ensure_stages(&mut self, project_id: &str) -> Result<Vec<StageInfo>, String> {
        let tf = self.taskflow_id(project_id)?;
        let mut stages = self.list_stages(project_id)?;
        let mut created: Vec<String> = Vec::new();
        for want in ALL_STAGES {
            if stages.iter().any(|s| s.name == want) {
                continue;
            }
            let has_start = stages.iter().any(|s| s.kind == "start");
            let kind = stage_kind(want, has_start);
            self.create_stage(project_id, &tf, want, kind)?;
            created.push(want.to_string());
        }
        // 重新拉一次，保证 id 准确
        let fresh = self.list_stages(project_id)?;
        if !fresh.is_empty() {
            stages = fresh;
        }
        if !created.is_empty() {
            eprintln!("已创建阶段: {}", created.join("、"));
        }
        Ok(stages)
    }

    pub fn find_stage<'a>(&self, stages: &'a [StageInfo], name: &str) -> Option<&'a StageInfo> {
        stages.iter().find(|s| s.name == name)
    }

    // 解析项目绑定的「skill」任务类型 id（sfc）。
    //
    // skill 不再是阶段，而是「任务类型」(sfc)。每个项目按名字解析同名 skill 类型：
    //   1) 先 sfc-query 按 projectId 拉全部类型，精确匹配 name == "skill"；
    //   2) 查不到就用 sfc-create 建一个（带 projectId + taskflowId，项目绑定）。
    // 注意必须精确等值匹配：还有个历史误建的「项目skill」类型，包含 "skill"
    // 但不是同一个，不能用 contains 匹配。
    pub fn resolve_skill_type_id(&mut self, project_id: &str) -> Result<String, String> {
        // 1) 先查。只传 projectId：sfc-query 按项目过滤即可，
        //    且不能传 taskflowId——build_args 的模糊匹配会把它错配到 id 属性上。
        if let Some(t) = &self.map.list_task_types {
            if let Ok(res) = self.tb.call_smart(t, &[("projectId", json!(project_id))]) {
                for item in extract_list(&res, 3) {
                    let name =
                        get_str(&item, &["name", "title", "sfcName"]).unwrap_or_default();
                    if name == TASK_TYPE_SKILL {
                        if let Some(id) = get_str(&item, &["id", "_id", "sfcId"]) {
                            return Ok(id);
                        }
                    }
                }
            }
        }
        // 2) 建（只有这一步才需要 taskflowId）
        let tf = self.taskflow_id(project_id)?;
        let t = self
            .map
            .create_task_type
            .clone()
            .ok_or("MCP 工具集里找不到「任务类型」(sfc) 查询/创建工具，无法初始化 skill 类型")?;
        let res = self.tb.call(
            &t,
            json!({"projectId": project_id, "taskflowId": tf, "name": TASK_TYPE_SKILL}),
        )?;
        get_str(&res, &["id", "_id", "sfcId"])
            .ok_or_else(|| format!("创建 skill 任务类型失败，无法解析 id: {res}"))
    }

    pub fn list_tasks(
        &mut self,
        project_id: &str,
        stage: &StageInfo,
    ) -> Result<Vec<TaskInfo>, String> {
        let tool = self.need("list_tasks", &self.map.list_tasks)?;
        // Teambition 查任务走 TQL。**阶段 id 是工作流状态 id（tfs）**，
        // TQL 字段名是 taskflowstatusId（tfsId 也行）。
        // 用 stageId 查会返回 0 条但**仍然报成功**——stageId 在任务对象里
        // 是「任务列表」id，跟阶段不是一回事。这是最阴的坑，所以
        // taskflowstatusId 必须放第一位，stageId 只能垫底。
        let sid = stage.id.replace('\'', "");
        // 前两条是精确筛选：成功就认，空也认（这个阶段真的没任务）。
        let precise: [serde_json::Value; 2] = [
            json!({"projectId": project_id,
                   "tql": format!("taskflowstatusId = '{sid}' AND isArchived = false")}),
            json!({"projectId": project_id,
                   "tql": format!("tfsId = '{sid}' AND isArchived = false")}),
        ];
        // 后三条是粗筛：只有筛出东西才算数，空就继续换。
        let loose: [serde_json::Value; 3] = [
            json!({"projectId": project_id,
                   "tql": format!("stageId = '{sid}' AND isArchived = false")}),
            json!({"projectId": project_id, "tql": format!("isArchived = false")}),
            json!({"projectId": project_id}),
        ];
        let mut last_err = String::new();
        for (i, base) in precise.into_iter().chain(loose).enumerate() {
            match self.list_tasks_page(&tool, base) {
                Ok(list) => {
                    // 粗筛可能把别的阶段也带回来，按阶段 id 收敛。
                    // stage_id 为空说明服务端没回阶段信息，保留。
                    let mut out: Vec<TaskInfo> = list
                        .into_iter()
                        .filter(|t| t.stage_id.is_empty() || t.stage_id == stage.id)
                        .collect();
                    // 按创建时间最早。创建时间为空的排最后。
                    out.sort_by(|a, b| {
                        let ea = a.created.is_empty();
                        let eb = b.created.is_empty();
                        match (ea, eb) {
                            (true, true) => std::cmp::Ordering::Equal,
                            (true, false) => std::cmp::Ordering::Greater,
                            (false, true) => std::cmp::Ordering::Less,
                            (false, false) => a.created.cmp(&b.created),
                        }
                    });
                    // 精确筛选（前两条）成功即返回，空也返回；
                    // 粗筛空结果不可信，继续换下一种查法。
                    if i < 2 || !out.is_empty() {
                        return Ok(out);
                    }
                }
                Err(e) => last_err = e,
            }
        }
        if last_err.is_empty() {
            Ok(Vec::new())
        } else {
            Err(last_err)
        }
    }

    // 带分页地拉任务。nextPageToken 为空才算翻完。
    fn list_tasks_page(
        &mut self,
        tool: &str,
        base: serde_json::Value,
    ) -> Result<Vec<TaskInfo>, String> {
        let mut out: Vec<TaskInfo> = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..50 {
            let mut args = base.clone();
            if let Some(t) = &page_token {
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("nextPageToken".to_string(), json!(t));
                }
            }
            let res = self.tb.call(tool, args)?;
            let next = get_str(&res, &["nextPageToken", "next_page_token", "pageToken"]);
            let items: Vec<TaskInfo> = extract_list(&res, 3)
                .iter()
                .filter_map(parse_task)
                .collect();
            out.extend(items);
            match next {
                Some(t) if !t.is_empty() => page_token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    // 拉项目下的全部任务（不按阶段过滤）。用于按任务类型(sfc)筛 skill。
    pub fn list_all_tasks(&mut self, project_id: &str) -> Result<Vec<TaskInfo>, String> {
        let tool = self.need("list_tasks", &self.map.list_tasks)?;
        let candidates: [serde_json::Value; 2] = [
            json!({"projectId": project_id, "tql": "isArchived = false"}),
            json!({"projectId": project_id}),
        ];
        let mut last_err = String::new();
        for base in candidates {
            match self.list_tasks_page(&tool, base) {
                Ok(list) => return Ok(list),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    pub fn create_task(
        &mut self,
        project_id: &str,
        stage: &StageInfo,
        title: &str,
        content: &str,
        sfc_id: &str,
    ) -> Result<String, String> {
        let tool = self.need("create_task", &self.map.create_task)?;
        // Teambition：content 是标题，note 是正文。
        // task.create 的 schema 是 additionalProperties:false，
        // **传 stageId 会被整体拒掉**（stageId 是任务列表 id，不是阶段 id），
        // 只能用 taskflowstatusId / tfsName 指定工作流状态。
        // sfcId 是「任务类型」维度（第 4 维），skill 任务就靠它标记；
        // 普通任务不传（用默认 Task 类型）。
        let mut direct: Vec<serde_json::Value> = Vec::new();
        if sfc_id.is_empty() {
            direct.push(json!({"projectId": project_id, "taskflowstatusId": stage.id,
                   "content": title, "note": content}));
            direct.push(json!({"projectId": project_id, "tfsName": stage.name,
                   "content": title, "note": content}));
        } else {
            direct.push(json!({"projectId": project_id, "taskflowstatusId": stage.id,
                   "sfcId": sfc_id, "content": title, "note": content}));
            direct.push(json!({"projectId": project_id, "tfsName": stage.name,
                   "sfcId": sfc_id, "content": title, "note": content}));
        }
        let mut last_err = String::new();
        for args in &direct {
            match self.tb.call(&tool, args.clone()) {
                Ok(res) => {
                    if let Some(id) = get_str(&res, &["id", "_id", "taskId", "task_id"]) {
                        return Ok(id);
                    }
                    last_err = format!("任务已创建但无法解析 id: {}", res);
                }
                Err(e) => last_err = e,
            }
        }
        // 真实 Teambition 规定：任务只能建在 kind=start 的工作流状态里，
        // 建到别的状态会报 "taskflowstatus should be start kind"。
        // 而 4 个阶段里只有一个能是 start（服务端限制），其余阶段就要走
        // 「先建（落默认 start 态）→ 再挪到目标阶段」两步。
        let mut fallback = json!({"projectId": project_id, "content": title, "note": content});
        if !sfc_id.is_empty() {
            fallback["sfcId"] = json!(sfc_id);
        }
        let res = self
            .tb
            .call(&tool, fallback)
            .map_err(|e| {
                format!(
                    "直接建到「{}」失败（{}）；退化为「先建再挪」也失败：{}",
                    stage.name, last_err, e
                )
            })?;
        let id = get_str(&res, &["id", "_id", "taskId", "task_id"])
            .ok_or_else(|| format!("任务已创建但无法解析 id: {}", res))?;
        self.move_task(&id, stage)?;
        Ok(id)
    }

    pub fn update_task_content(
        &mut self,
        task_id: &str,
        title: &str,
        content: &str,
    ) -> Result<(), String> {
        let tool = self.need("update_task", &self.map.update_task)?;
        let candidates: [serde_json::Value; 3] = [
            json!({"taskId": task_id, "content": title, "note": content}),
            json!({"taskId": task_id, "title": title, "content": content}),
            json!({"taskId": task_id, "content": content}),
        ];
        let mut last_err = String::new();
        for args in candidates {
            match self.tb.call(&tool, args) {
                Ok(_) => return Ok(()),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    // 在已发现的工具列表里按名字找工具。服务端工具集会变，
    // 不能只认 ToolMap 里那一个映射。
    fn find_tool(&self, name: &str) -> Option<String> {
        self.tb
            .tools
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.name.clone())
    }

    // 去重保序地收集候选工具名：映射优先，再按已知工具名补。
    fn tool_candidates(&self, mapped: &Option<String>, extra: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(m) = mapped {
            if !m.is_empty() && !out.iter().any(|x| x == m) {
                out.push(m.clone());
            }
        }
        for e in extra {
            if let Some(n) = self.find_tool(e) {
                if !out.iter().any(|x| *x == n) {
                    out.push(n);
                }
            }
        }
        out
    }

    // 改工作流状态。已经是目标状态就跳过。
    // 真实服务端把任务设成「它当前的状态」会报 internal error
    // （contract_violation），不是幂等成功——所以必须自己挡掉 no-op。
    pub fn move_task_if_changed(
        &mut self,
        task_id: &str,
        current_stage_id: &str,
        to: &StageInfo,
    ) -> Result<(), String> {
        if !current_stage_id.is_empty() && current_stage_id == to.id {
            return Ok(());
        }
        self.move_task(task_id, to)
    }

    pub fn move_task(&mut self, task_id: &str, to: &StageInfo) -> Result<(), String> {
        // 真实 Teambition：task.update-status，按 taskflowstatusId 或 tfsName。
        // 注意 task.move 是「把任务移到另一个项目」，不是改阶段，别混用。
        // mock：task.move。两套都试，谁成功算谁。
        let tools = self.tool_candidates(
            &self.map.move_task,
            &[
                "teambition.task.update-status",
                "teambition.task.move",
                "teambition.task.update",
            ],
        );
        let arg_shapes: [serde_json::Value; 3] = [
            json!({"taskId": task_id, "taskflowstatusId": to.id}),
            json!({"taskId": task_id, "tfsName": to.name}),
            json!({"taskId": task_id, "id": task_id,
                   "taskflowstatusId": to.id, "tfsName": to.name}),
        ];
        let mut last_err = String::new();
        for t in &tools {
            for args in &arg_shapes {
                match self.tb.call(t, args.clone()) {
                    Ok(_) => return Ok(()),
                    Err(e) => last_err = e,
                }
            }
        }
        Err(if last_err.is_empty() {
            "找不到可用的任务状态流转工具".to_string()
        } else {
            last_err
        })
    }

    // ---- 看板列（任务列表）----
    //
    // 四个维度别混：
    //   taskflowstatusId(tfs) = 工作流状态 = TBHub 的阶段（能建能改）
    //   stageId / taskListId  = 任务列表 = 看板列（只能查 + 把任务挪进去）
    //   sfcId                 = 任务类型
    //   tasklistId(分组)      = 任务的容器分组，跟列不是一回事

    fn find_tool_suffix(&self, suf: &str) -> Option<String> {
        self.tb
            .tools
            .iter()
            .map(|t| t.name.clone())
            .find(|n| n.to_lowercase().ends_with(suf))
    }

    // 拉项目的看板列（id + 列名）。API 没有建列接口，列只能网页手动建。
    pub fn task_lists(&mut self, project_id: &str) -> Result<Vec<ListInfo>, String> {
        let tool = self
            .find_tool_suffix("task-list-query")
            .ok_or("MCP 工具集里找不到看板列查询工具（task-list-query）")?;
        let res = self
            .tb
            .call_smart(&tool, &[("projectId", json!(project_id))])?;
        let mut out: Vec<ListInfo> = Vec::new();
        for item in extract_list(&res, 3) {
            let id = get_str(&item, &["id", "_id", "taskListId", "stageId"]);
            let name = get_str(&item, &["name", "title"]);
            if let (Some(id), Some(name)) = (id, name) {
                if !out.iter().any(|l| l.id == id) {
                    out.push(ListInfo { id, name });
                }
            }
        }
        Ok(out)
    }

    // 把任务挪到指定列名的看板列。
    //
    // 返回 Ok(true)=已在目标列或移成功，Ok(false)=没移（列名不存在 / 工具缺失 /
    // 接口拒），调用方只打警告，**绝不因为移列失败而中断流程**。
    //
    // 先查列清单再动：列名不存在就直接返回，别白打一次接口；
    // 已经在目标列也直接返回——真实服务端对「挪到它已经所在的列」不保证成功。
    pub fn move_to_list(
        &mut self,
        task_id: &str,
        project_id: &str,
        list_name: &str,
    ) -> Result<bool, String> {
        if list_name.trim().is_empty() {
            return Ok(false);
        }
        let Some(tool) = self.map.move_to_list.clone() else {
            return Ok(false);
        };
        let lists = self.task_lists(project_id).unwrap_or_default();
        let Some(target) = lists.iter().find(|l| l.name == list_name) else {
            // 列不存在（可能被改名了）——跳过，不阻断。
            return Ok(false);
        };
        // 已经在目标列，直接算成功（真实服务端不保证 no-op 挪列成功）。
        let cur_list = self.task_list_id(project_id, task_id);
        if !cur_list.is_empty() && cur_list == target.id {
            return Ok(true);
        }
        // 1) 按列名
        let by_name = json!({"taskId": task_id, "stageName": list_name, "projectId": project_id});
        if self.tb.call(&tool, by_name).is_ok() {
            return Ok(true);
        }
        // 2) 按列 id
        let by_id = json!({"taskId": task_id, "taskListId": target.id});
        if self.tb.call(&tool, by_id).is_ok() {
            return Ok(true);
        }
        let by_stage = json!({"taskId": task_id, "stageId": target.id, "projectId": project_id});
        Ok(self.tb.call(&tool, by_stage).is_ok())
    }

    // 取任务当前所在的看板列 id。
    //
    // task.query 不返回 stageId（fields 也不支持 stageId，传了报 unsupported），
    // 只有 project.task-query 的 TQL 会带 stageId。所以按 id 精确查一条：
    //   tql = "id = '<taskId>'"
    // 查不到返回空串（表示这个数据源没有看板列维度）。
    pub fn task_list_id(&mut self, project_id: &str, task_id: &str) -> String {
        let Ok(tool) = self.need("list_tasks", &self.map.list_tasks) else {
            return String::new();
        };
        let args = json!({
            "projectId": project_id,
            "tql": format!("id = '{task_id}'"),
            "pageSize": 5,
        });
        match self.tb.call(&tool, args) {
            Ok(res) => extract_list(&res, 3)
                .iter()
                .filter_map(parse_task)
                .find(|t| t.id == task_id)
                .map(|t| t.list_id)
                .unwrap_or_default(),
            Err(_) => String::new(),
        }
    }

    // 任务当前所在看板列的名字。两步查询合成一步，免得调用处嵌套借用。
    pub fn task_list_name(&mut self, project_id: &str, task_id: &str) -> String {
        let list_id = self.task_list_id(project_id, task_id);
        self.list_name_of(project_id, &list_id)
    }

    // 列 id -> 列名。查不到返回空串（表示数据源没有看板列维度）。
    pub fn list_name_of(&mut self, project_id: &str, list_id: &str) -> String {        if list_id.is_empty() {
            return String::new();
        }
        self.task_lists(project_id)
            .unwrap_or_default()
            .into_iter()
            .find(|l| l.id == list_id)
            .map(|l| l.name)
            .unwrap_or_default()
    }

    // ---- 任务动态 / 评论 ----

    // 读任务的评论（按时间正序）。
    pub fn read_comments(&mut self, task_id: &str) -> Result<Vec<CommentInfo>, String> {
        let tool = self
            .map
            .read_activity
            .clone()
            .ok_or("MCP 工具集里找不到任务动态工具（task.activity），读不到回复")?;
        let res = self
            .tb
            .call(&tool, json!({"taskId": task_id, "actions": "comment"}))?;
        let mut out: Vec<CommentInfo> = Vec::new();
        for item in extract_list(&res, 3) {
            let action = get_str(&item, &["action"]).unwrap_or_default();
            // actions 参数已经筛过，但 mock / 别家实现可能不认，再挡一次。
            if !action.is_empty() && action != "comment" {
                continue;
            }
            let id = get_str(&item, &["id", "_id"]).unwrap_or_default();
            let author = get_str(
                &item,
                &["creatorName", "creator", "creatorId", "author", "userId"],
            )
            .unwrap_or_default();
            let created = get_str(&item, &["createTime", "created", "updateTime"])
                .unwrap_or_default();
            let text = comment_text(&item);
            if text.is_empty() {
                continue;
            }
            // 分页/多次拉取可能重复，按评论 id 去重。
            if !id.is_empty() && out.iter().any(|c| c.id == id) {
                continue;
            }
            out.push(CommentInfo {
                id,
                author,
                created,
                text,
            });
        }
        Ok(out)
    }

    // 发评论。返回 (评论 id, 服务端时间戳)——时间戳用来给 tb reply 划「只读这之后」的界线。
    pub fn post_comment(
        &mut self,
        task_id: &str,
        md: &str,
    ) -> Result<(String, String), String> {
        let tool = self
            .map
            .comment
            .clone()
            .ok_or("MCP 工具集里找不到发评论工具（task.comment）")?;
        let res = self.tb.call(
            &tool,
            json!({"taskId": task_id, "content": md, "renderMode": "markdown"}),
        )?;
        let id = get_str(&res, &["id", "_id"]).unwrap_or_default();
        let ts = get_str(&res, &["createTime", "created", "updateTime"]).unwrap_or_default();
        Ok((id, ts))
    }

    // ---- 单任务读取 ----

    pub fn task_info(&mut self, task_id: &str) -> Result<TaskInfo, String> {
        let tool = self.need("get_task", &self.map.get_task)?;
        let res = self
            .tb
            .call_smart(&tool, &[("taskId", json!(task_id))])?;
        if let Some(t) = parse_task(&res) {
            if t.id == task_id {
                return Ok(t);
            }
        }
        for item in extract_list(&res, 3) {
            if let Some(t) = parse_task(&item) {
                if t.id == task_id {
                    return Ok(t);
                }
            }
        }
        Err(format!("查不到任务 {task_id} 的详情"))
    }

    pub fn task_project_id(&mut self, task_id: &str) -> Result<String, String> {
        let tool = self.need("get_task", &self.map.get_task)?;
        let res = self
            .tb
            .call_smart(&tool, &[("taskId", json!(task_id))])?;
        if let Some(p) = get_str(&res, &["projectId", "project_id"]) {
            return Ok(p);
        }
        for item in extract_list(&res, 3) {
            if let Some(p) = get_str(&item, &["projectId", "project_id"]) {
                return Ok(p);
            }
        }
        Err(format!("任务 {task_id} 上读不到 projectId"))
    }
}

// ---- 领任务 ----

pub fn claim(
    store: &Store,
    project_id: &str,
    name_filter: Option<&str>,
    agent: &str,
) -> Result<Claim, String> {
    let mut ctx = Ctx::new(store.clone(), false)?;
    let stages = ctx.ensure_stages(project_id)?;
    let todo = ctx
        .find_stage(&stages, STAGE_TODO)
        .ok_or("缺少「待处理」阶段")?;
    let tasks = ctx.list_tasks(project_id, todo)?;
    // skill 是「任务类型」不是阶段，但它也可能落在「待处理」里；
    // 领任务只领普通任务（Task 类型），按 sfc 把 skill 类型剔掉。
    // 解析不到 skill 类型时（工具集不支持 sfc）就不过滤，保证 claim 不因此失败。
    let skill_type_id = ctx.resolve_skill_type_id(project_id).ok();
    let cfg = ctx.tb.cfg.clone();
    // 只领「列1 + 待处理」。列1 是没人接的队列；列2 是 agent 的地盘——
    // 正在等用户回复的任务（待处理 + 列2）也在待处理状态，绝不能重复领。
    let queue_id = ctx
        .task_lists(project_id)
        .unwrap_or_default()
        .into_iter()
        .find(|l| l.name == cfg.tasklist_queue)
        .map(|l| l.id);
    // 按创建时间最早。创建时间为空的排最后。
    // 注意：台账过滤和「选中并记账」必须放在同一把锁里（见下），
    // 否则两个会话并发 claim 会同时选中同一条。
    let candidates: Vec<TaskInfo> = tasks
        .into_iter()
        .filter(|t| match &skill_type_id {
            Some(sid) => t.sfc_id != *sid,
            None => true,
        })
        // 列信息可用就按列收敛；取不到列（数据源没这一维）就交给台账判断。
        .filter(|t| match &queue_id {
            Some(qid) => t.list_id.is_empty() || t.list_id == *qid,
            None => true,
        })
        .collect();
    let doing = ctx
        .find_stage(&stages, STAGE_DOING)
        .ok_or("缺少「进行中」阶段")?
        .clone();

    // ---- 临界区：读台账 -> 选任务 -> 写台账 ----
    // 锁外已经做完全部网络查询，这里只碰本地文件，持锁时间很短。
    let lock = store.lock_claims(30)?;
    let mut tasks = candidates;
    // 本地台账里还占着的（running / waiting）一律跳过。
    tasks.retain(|t| store.find_active_claim(&t.id).is_none());
    tasks.sort_by(|a, b| {
        let ea = a.created.is_empty();
        let eb = b.created.is_empty();
        match (ea, eb) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => a.created.cmp(&b.created),
        }
    });
    let picked = match name_filter {
        Some(n) => tasks.iter().find(|t| t.title.contains(n)),
        None => tasks.first(),
    };
    let picked = picked.ok_or_else(|| {
        format!(
            "「待处理」里没有可领的任务。注意 claim 只领看板列「{}」里的任务——\
             列「{}」是 agent 的地盘（进行中 / 待回复），不会被重复领。\
             要把某条任务交给 agent，把它拖到列「{}」，或直接用 `tb take <任务链接>`。",
            cfg.tasklist_queue, cfg.tasklist_claim, cfg.tasklist_queue
        )
    })?;

    // 幂等：同一任务已有未结束的领取记录，直接返回。
    if let Some(c) = store.find_active_claim(&picked.id) {
        return Ok(c);
    }

    // 已经按队列列过滤过了，所以来源列就是队列列本身，不必再发一次查询。
    let from_list = if picked.list_id.is_empty() {
        String::new()
    } else {
        cfg.tasklist_queue.clone()
    };

    let claim = Claim {
        claim_id: new_id("clm"),
        task_id: picked.id.clone(),
        task_name: picked.title.clone(),
        project_id: project_id.to_string(),
        stage_from: STAGE_TODO.to_string(),
        stage_to: STAGE_DOING.to_string(),
        agent: agent.to_string(),
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        state: "running".to_string(),
        created_at: chrono::Local::now().to_rfc3339(),
        updated_at: chrono::Local::now().to_rfc3339(),
        note: String::new(),
        heartbeat_at: chrono::Local::now().to_rfc3339(),
        tasklist_from: from_list,
        ask_at: String::new(),
        question: String::new(),
    };
    // 先记账，再移动。移动失败则标记 failed。
    store.append_claim(&claim)?;
    // 记账完成即可放锁：后面的改状态/移列是网络调用，不该占着锁。
    drop(lock);
    let picked_id = picked.id.clone();
    let picked_stage = picked.stage_id.clone();
    match ctx.move_task_if_changed(&picked_id, &picked_stage, &doing) {
        Ok(()) => {
            // 状态是关键，列只是视觉分组：移列失败只警告，不算领失败。
            if !ctx
                .move_to_list(&picked_id, project_id, &cfg.tasklist_claim)
                .unwrap_or(false)
            {
                eprintln!(
                    "警告：没能把任务挪到看板列「{}」，状态已是「进行中」，列请手动拖。",
                    cfg.tasklist_claim
                );
            }
            store.update_claim(&claim.claim_id, "running", "")?;
            Ok(claim)
        }
        Err(e) => {
            let _ = store.update_claim(&claim.claim_id, "failed", &e);
            Err(format!("已记账但移动任务状态失败: {e}"))
        }
    }
}

pub fn finish(store: &Store, claim_id: &str, to_stage: &str) -> Result<Claim, String> {
    let mut ctx = Ctx::new(store.clone(), false)?;
    let claims = store.load_claims();
    let claim = claims
        .iter()
        .find(|c| c.claim_id == claim_id)
        .cloned()
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tb status 查看全部"))?;
    if !claim.is_active() {
        return Err(format!(
            "claim {claim_id} 当前状态是 {}，不能重复结束",
            claim.state
        ));
    }
    let stages = ctx.ensure_stages(&claim.project_id)?;
    let stage = ctx
        .find_stage(&stages, to_stage)
        .ok_or(format!("缺少「{to_stage}」阶段"))?
        .clone();
    let cur = ctx
        .task_info(&claim.task_id)
        .map(|t| t.stage_id)
        .unwrap_or_default();
    ctx.move_task_if_changed(&claim.task_id, &cur, &stage)?;
    let new_state = if to_stage == STAGE_CLOSED {
        "closed"
    } else {
        "done"
    };
    // 收尾也挪列：已解决 -> 列4，关闭 -> 列5。失败只警告。
    let cfg = ctx.tb.cfg.clone();
    let target_list = if to_stage == STAGE_CLOSED {
        cfg.tasklist_closed.clone()
    } else {
        cfg.tasklist_done.clone()
    };
    if !ctx
        .move_to_list(&claim.task_id, &claim.project_id, &target_list)
        .unwrap_or(false)
    {
        eprintln!(
            "警告：没能把任务挪到看板列「{target_list}」，状态已是「{to_stage}」，列请手动拖。"
        );
    }
    store.update_claim(claim_id, new_state, "")?;
    let mut c = claim;
    c.state = new_state.to_string();
    Ok(c)
}

// ---- 接单 / 提问 / 继续 / 放弃 / 回收 ----
//
// 这是「任务跟着会话走」的核心状态机。两个维度组合出五种局面：
//
//   状态(tfs)   看板列   含义
//   待处理      列1      队列里，没人接
//   进行中      列2      agent 正在干
//   待回复      列3      agent 已提问，球在用户这边（未完成）
//   已解决/关闭  列4/5    收尾
//   待处理      列1      会话异常断开，已退回队列
//
// 关键约束：
//   1. claim 必须是「列1 + 待处理」双过滤。待回复虽然已独立成状态，
//      但双过滤仍保留：防止有人手动把「待处理」任务拖到列1 之外再被领走。
//   2. waiting 状态永不参与心跳超时——用户可能隔一天才回。
//   3. 只有 running 且心跳超时才判定「会话断开」并回滚。

// 从 URL / 裸 id 里抠出任务 id。
// 支持 https://www.teambition.com/task/<id>、带查询串、带尾斜杠、裸 id。
pub fn parse_task_ref(s: &str) -> String {
    let t = s.trim().trim_end_matches('/');
    if let Some(i) = t.rfind("/task/") {
        let rest = &t[i + 6..];
        // 真实任务 id 是 24 位十六进制；允许 - 和 _ 是为了兼容 mock / 自定义 id。
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !id.is_empty() {
            return id;
        }
    }
    t.to_string()
}

// 评论正文。真实 Teambition 的 content 是 JSON 字符串
// {"comment":"...","renderMode":"markdown"}，要再解一层。
fn comment_text(v: &serde_json::Value) -> String {
    if let Some(s) = v.get("content").and_then(|c| c.as_str()) {
        if let Ok(j) = serde_json::from_str::<serde_json::Value>(s) {
            if let Some(t) = get_str(&j, &["comment", "content", "text"]) {
                return t;
            }
        }
        return s.to_string();
    }
    if let Some(c) = v.get("content") {
        if let Some(t) = get_str(c, &["comment", "content", "text"]) {
            return t;
        }
    }
    get_str(v, &["message", "text", "comment"]).unwrap_or_default()
}

// 比较两个 ISO 时间戳，a 是否晚于 b。解析不了就当不晚（宁可漏读也不误判）。
fn ts_after(a: &str, b: &str) -> bool {
    fn epoch(s: &str) -> Option<i64> {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.timestamp())
    }
    match (epoch(a), epoch(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

pub struct TakeResult {
    pub claim: Claim,
    pub warnings: Vec<String>,
    // 该任务本来就被占着（幂等返回原 claim）
    pub existed: bool,
    // 动作做完后**回读**到的真实看板列名（不是配置里的期望值）。
    // 用来证明「真的挪过去了」，而不是「我们以为挪过去了」。
    pub list_now: String,
}

// 接单：URL / 任务 id -> 进行中 + 列2，写台账。
pub fn take(store: &Store, target: &str, agent: &str) -> Result<TakeResult, String> {
    let task_id = parse_task_ref(target);
    if task_id.is_empty() {
        return Err("没解析出任务 id。用法: tb take https://www.teambition.com/task/<id>".into());
    }
    // 先回收死会话，避免任务被一个已经不存在的会话占着。
    let _ = sweep(store, false);

    let mut ctx = Ctx::new(store.clone(), false)?;
    let project_id = ctx.task_project_id(&task_id)?;
    let stages = ctx.ensure_stages(&project_id)?;
    let doing = ctx
        .find_stage(&stages, STAGE_DOING)
        .ok_or("缺少「进行中」阶段")?
        .clone();
    let detail = ctx.task_info(&task_id)?;
    // 记下接单前它原本在哪个阶段（多半是「待处理」，也可能是别的）。
    let from_stage = stages
        .iter()
        .find(|s| s.id == detail.stage_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| STAGE_TODO.to_string());

    // 已完成/已关闭的任务不该被领。
    if let Some(s) = stages.iter().find(|s| s.id == detail.stage_id) {
        if s.name == STAGE_DONE || s.name == STAGE_CLOSED {
            return Err(format!(
                "任务「{}」当前是「{}」，已完成/已关闭，不能领取。要重做请先在 Teambition 里把它改回「待处理」。",
                detail.title, s.name
            ));
        }
    }

    // 幂等：同一任务已有活动 claim（running 或 waiting），直接返回。
    if let Some(c) = store.find_active_claim(&task_id) {
        let list_now = ctx.task_list_name(&project_id, &task_id);
        return Ok(TakeResult {
            claim: c,
            warnings: vec![],
            existed: true,
            list_now,
        });
    }

    let cfg = ctx.tb.cfg.clone();
    let cur_list_id = ctx.task_list_id(&project_id, &task_id);
    let from_list = ctx.list_name_of(&project_id, &cur_list_id);
    let claim = Claim {
        claim_id: new_id("clm"),
        task_id: task_id.clone(),
        task_name: detail.title.clone(),
        project_id: project_id.clone(),
        stage_from: from_stage,
        stage_to: STAGE_DOING.to_string(),
        agent: agent.to_string(),
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        state: "running".to_string(),
        created_at: chrono::Local::now().to_rfc3339(),
        updated_at: chrono::Local::now().to_rfc3339(),
        note: String::new(),
        heartbeat_at: chrono::Local::now().to_rfc3339(),
        tasklist_from: from_list,
        ask_at: String::new(),
        question: String::new(),
    };
    // 先记账，再动线上。移动失败则标记 failed，不留下「以为领了其实没领」的假象。
    store.append_claim(&claim)?;
    if let Err(e) = ctx.move_task_if_changed(&task_id, &detail.stage_id, &doing) {
        let _ = store.update_claim(&claim.claim_id, "failed", &e);
        return Err(format!("已记账但把任务改成「进行中」失败: {e}"));
    }
    let mut warnings = Vec::new();
    if !ctx
        .move_to_list(&task_id, &project_id, &cfg.tasklist_claim)
        .unwrap_or(false)
    {
        warnings.push(format!(
            "没能把任务挪到看板列「{}」（列名不存在或接口拒绝）。状态已改为「进行中」，列需要你在网页手动拖。",
            cfg.tasklist_claim
        ));
    }
    // 回读真实列，别拿配置当结果。
    let list_now = ctx.task_list_name(&project_id, &task_id);
    Ok(TakeResult {
        claim,
        warnings,
        existed: false,
        list_now,
    })
}

// 开工：没有现成任务时，直接新建一条并立刻进入「进行中 + 列2」。
//
// 和 take 的区别：take 是接用户已经建好的任务（给链接/id），
// start 是用户只在文案里写了「要做什么」，由 agent 自己把任务建出来。
//
// 建任务必须两步：task.create 只能建在 kind=start 的「待处理」里，
// 建完再挪到「进行中」。所以 claim.stage_from 记「待处理」——
// 会话断开时回滚到「待处理 + 列1」，正好回到队列里等下一个 agent。
pub fn start(
    store: &Store,
    project_id: &str,
    title: &str,
    note: &str,
    agent: &str,
) -> Result<TakeResult, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("任务标题不能为空。用法: tb start --project <id> --title \"要做什么\"".into());
    }
    // 先回收死会话，别让过期 claim 占着同名任务。
    let _ = sweep(store, false);

    let mut ctx = Ctx::new(store.clone(), false)?;
    let stages = ctx.ensure_stages(project_id)?;
    let todo = ctx
        .find_stage(&stages, STAGE_TODO)
        .ok_or("缺少「待处理」阶段")?
        .clone();
    let doing = ctx
        .find_stage(&stages, STAGE_DOING)
        .ok_or("缺少「进行中」阶段")?
        .clone();

    // 幂等：同名任务已经有活动 claim，说明这条活已经开过工了，直接复用。
    // 防的是「用户点了几次复制文案 / agent 重试」把同一条活建出好几份。
    if let Some(c) = store
        .active_claims()
        .into_iter()
        .find(|c| c.project_id == project_id && c.task_name == title)
    {
        let list_now = ctx.task_list_name(project_id, &c.task_id);
        return Ok(TakeResult {
            claim: c,
            warnings: vec![],
            existed: true,
            list_now,
        });
    }

    let cfg = ctx.tb.cfg.clone();
    let task_id = ctx.create_task(project_id, &todo, title, note, "")?;
    // 新建的任务落在哪个列由服务端决定（默认列1），这里如实读回来。
    let from_list = ctx.task_list_name(project_id, &task_id);

    let claim = Claim {
        claim_id: new_id("clm"),
        task_id: task_id.clone(),
        task_name: title.to_string(),
        project_id: project_id.to_string(),
        stage_from: STAGE_TODO.to_string(),
        stage_to: STAGE_DOING.to_string(),
        agent: agent.to_string(),
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        state: "running".to_string(),
        created_at: chrono::Local::now().to_rfc3339(),
        updated_at: chrono::Local::now().to_rfc3339(),
        note: "由 tb start 新建".to_string(),
        heartbeat_at: chrono::Local::now().to_rfc3339(),
        tasklist_from: from_list,
        ask_at: String::new(),
        question: String::new(),
    };
    // 先记账再改线上：中途失败也能从台账看出来，不会「以为开工了其实没开」。
    store.append_claim(&claim)?;
    if let Err(e) = ctx.move_task_if_changed(&task_id, &todo.id, &doing) {
        let _ = store.update_claim(&claim.claim_id, "failed", &e);
        return Err(format!("任务已创建（{task_id}）但改成「进行中」失败: {e}"));
    }
    let mut warnings = Vec::new();
    if !ctx
        .move_to_list(&task_id, project_id, &cfg.tasklist_claim)
        .unwrap_or(false)
    {
        warnings.push(format!(
            "任务已建好并设为「进行中」，但没能挪到看板列「{}」（列名不存在或接口拒绝），列需要手动拖。",
            cfg.tasklist_claim
        ));
    }
    Ok(TakeResult {
        claim,
        warnings,
        existed: false,
        list_now: ctx.task_list_name(project_id, &task_id),
    })
}

// 提问：进行中 -> 待处理（仍留在列2），把问题写成任务评论等用户回。
pub fn ask(store: &Store, claim_id: &str, question: &str) -> Result<Claim, String> {
    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tb status 查看全部"))?;
    if !claim.is_active() {
        return Err(format!(
            "claim {claim_id} 当前是 {}，不能提问（任务已经收尾或已被回收）",
            claim.state
        ));
    }
    let q = question.trim();
    if q.is_empty() {
        return Err("问题不能为空。用法: tb ask <claim_id> --q \"要问用户的话\"".into());
    }
    let mut ctx = Ctx::new(store.clone(), false)?;
    let stages = ctx.ensure_stages(&claim.project_id)?;
    let wait = ctx
        .find_stage(&stages, STAGE_WAIT)
        .ok_or("缺少「待回复」阶段")?
        .clone();

    // 问题必须落到任务上——会话可能断，任务评论不会丢。
    let body = format!(
        "**❓ 需要你回复**\n\n{}\n\n---\n> 直接在下面评论回复即可，我会读取后继续执行。\n> 任务状态已改为「{}」，看板列「{}」。回复后自动回到「{}」。",
        q,
        STAGE_WAIT,
        ctx.tb.cfg.tasklist_wait,
        STAGE_DOING
    );
    let (_, asked_at) = ctx.post_comment(&claim.task_id, &body)?;

    let cur = ctx
        .task_info(&claim.task_id)
        .map(|t| t.stage_id)
        .unwrap_or_default();
    ctx.move_task_if_changed(&claim.task_id, &cur, &wait)?;
    // 挪到「待回复」列，让看板上一眼看出球在用户那。
    let _ = ctx.move_to_list(
        &claim.task_id,
        &claim.project_id,
        &ctx.tb.cfg.tasklist_wait.clone(),
    );

    let ts = chrono::Local::now().to_rfc3339();
    store.patch_claim(claim_id, |c| {
        c.state = "waiting".to_string();
        c.stage_from = STAGE_DOING.to_string();
        c.stage_to = STAGE_WAIT.to_string();
        c.ask_at = asked_at.clone();
        c.question = q.to_string();
        c.heartbeat_at = ts.clone();
    })?;
    store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}"))
}

// 继续：读回用户在任务里的回复，待处理 -> 进行中（仍在列2）。
pub fn resume(store: &Store, claim_id: &str) -> Result<(Claim, Vec<CommentInfo>), String> {
    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tb status 查看全部"))?;
    if claim.state == "done" || claim.state == "closed" {
        return Err(format!("claim {claim_id} 已经收尾（{}），不用继续", claim.state));
    }
    if claim.state == "reverted" || claim.state == "released" {
        return Err(format!(
            "claim {claim_id} 已被退回队列（{}），要接着做请用 tb take 重新接单",
            claim.state
        ));
    }
    let mut ctx = Ctx::new(store.clone(), false)?;
    let stages = ctx.ensure_stages(&claim.project_id)?;
    let doing = ctx
        .find_stage(&stages, STAGE_DOING)
        .ok_or("缺少「进行中」阶段")?
        .clone();

    // 只读「提问之后」的评论。ask_at 用的是服务端时间戳，避免本地时钟漂移。
    let comments = ctx.read_comments(&claim.task_id).unwrap_or_default();
    let replies: Vec<CommentInfo> = if claim.ask_at.is_empty() {
        comments
    } else {
        comments
            .into_iter()
            .filter(|c| ts_after(&c.created, &claim.ask_at))
            .collect()
    };

    let cur = ctx
        .task_info(&claim.task_id)
        .map(|t| t.stage_id)
        .unwrap_or_default();
    ctx.move_task_if_changed(&claim.task_id, &cur, &doing)?;
    let _ = ctx.move_to_list(
        &claim.task_id,
        &claim.project_id,
        &ctx.tb.cfg.tasklist_claim.clone(),
    );

    let ts = chrono::Local::now().to_rfc3339();
    store.patch_claim(claim_id, |c| {
        c.state = "running".to_string();
        c.stage_from = STAGE_WAIT.to_string();
        c.stage_to = STAGE_DOING.to_string();
        // 清掉 ask_at，避免下次读到这一轮的旧回复。
        c.ask_at = String::new();
        c.heartbeat_at = ts.clone();
    })?;
    let c = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}"))?;
    Ok((c, replies))
}

// ---- 待回复清单 ----
//
// 远程时最怕的不是「回不去」，是「球在你那而你不知道」。
// inbox 把所有在等你回复的任务按等得最久的排前面，一眼看出卡了几件。
pub fn inbox(store: &Store, only_agent: Option<&str>) -> Vec<Claim> {
    let mut v: Vec<Claim> = store
        .active_claims()
        .into_iter()
        .filter(|c| c.state == "waiting")
        .filter(|c| match only_agent {
            Some(a) => c.agent == a,
            None => true,
        })
        .collect();
    // ask_at 是服务端时间戳，字符串比较等价于时间比较；空的排最后。
    v.sort_by(|a, b| match (a.ask_at.is_empty(), b.ask_at.is_empty()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.ask_at.cmp(&b.ask_at),
    });
    v
}

pub fn task_url(task_id: &str) -> String {
    format!("https://www.teambition.com/task/{task_id}")
}

// 同步等待：agent 挂在会话里等用户回复 ----
//
// 为什么需要它：WorkBuddy 这类会话没有对外 CLI 入口，外部没法把 agent「叫醒」。
// 所以只能让会话自己别结束——agent 在会话里跑一条阻塞命令，一直等到用户在
// Teambition 上评论为止。读到评论就顺手把状态推回「进行中 + 列2」并返回正文，
// 用户不需要再回到会话里补一句「继续」。
//
// 时间约束：Bash 工具单次上限 600 秒，超了会被 auto-background，agent 就拿不到
// 结果了。所以单轮默认 540 秒**主动返回**，由 agent 文案规定「超时再跑一轮」。
pub enum WaitOutcome {
    // 读到回复，任务已推回「进行中 + 列2」
    Replied {
        claim: Claim,
        replies: Vec<CommentInfo>,
        waited_secs: u64,
    },
    // 这一轮没等到，任务仍在「待回复 + 列3」
    Timeout { claim: Claim, waited_secs: u64 },
    // 等待期间任务被别处收尾/退回了，别再等
    Gone {
        claim: Claim,
        state: String,
        waited_secs: u64,
    },
}

pub fn wait(
    store: &Store,
    claim_id: &str,
    timeout_secs: u64,
    interval_secs: u64,
) -> Result<WaitOutcome, String> {
    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tb status 查看全部"))?;
    if !claim.is_active() {
        return Err(format!(
            "claim {claim_id} 当前是 {}，没有可等的回复（任务已收尾或已回收）",
            claim.state
        ));
    }
    if claim.ask_at.is_empty() {
        return Err(format!(
            "任务「{}」还没提问过，没有在等的东西。先 tb ask {claim_id} -q \"问题\" 再 tb wait",
            claim.task_name
        ));
    }

    let mut ctx = Ctx::new(store.clone(), false)?;
    let started = std::time::Instant::now();
    let interval = interval_secs.max(3);
    let timeout = timeout_secs.max(interval);
    let ask_at = claim.ask_at.clone();
    let mut next_report = 60u64;

    loop {
        // 等待期间任务可能被别处 done/close/release 掉，那就别再等了。
        if let Some(c) = store.get_claim(claim_id) {
            if !c.is_active() {
                return Ok(WaitOutcome::Gone {
                    state: c.state.clone(),
                    claim: c,
                    waited_secs: started.elapsed().as_secs(),
                });
            }
        }

        // 只看「提问之后」的评论；ask_at 用服务端时间戳，避免本地时钟漂移。
        let fresh: Vec<CommentInfo> = ctx
            .read_comments(&claim.task_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| ts_after(&c.created, &ask_at))
            .collect();
        if !fresh.is_empty() {
            // 复用 resume：推回 进行中 + 列2，并清掉 ask_at 防止下次读重。
            let (c, replies) = resume(store, claim_id)?;
            return Ok(WaitOutcome::Replied {
                claim: c,
                replies,
                waited_secs: started.elapsed().as_secs(),
            });
        }

        let waited = started.elapsed().as_secs();
        if waited >= timeout {
            let c = store.get_claim(claim_id).unwrap_or(claim);
            return Ok(WaitOutcome::Timeout {
                claim: c,
                waited_secs: waited,
            });
        }
        // 进度走 stderr，不污染 stdout（agent 读的是 stdout 正文）。
        if waited >= next_report {
            eprintln!("… 已等 {waited} 秒（每 {interval} 秒查一次，本轮上限 {timeout} 秒）");
            next_report = waited + 60;
        }
        let step = interval.min(timeout - waited).max(1);
        std::thread::sleep(std::time::Duration::from_secs(step));
    }
}

// 续命：刷新心跳。长构建 / 长测试前后调用，避免被判定会话断开。
pub fn beat(store: &Store, claim_id: &str) -> Result<Claim, String> {    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}"))?;
    if !claim.is_running() {
        // waiting 不用续命（本来就不参与超时），已收尾的也不用。
        return Ok(claim);
    }
    store.beat_claim(claim_id)
}

// 退回队列：待处理 + 列1。agent 主动放弃，或会话断开时由 sweep 调用。
fn revert_to_queue(ctx: &mut Ctx, claim: &Claim, note: &str) -> Result<(), String> {
    let stages = ctx.ensure_stages(&claim.project_id)?;
    let todo = ctx
        .find_stage(&stages, STAGE_TODO)
        .ok_or("缺少「待处理」阶段")?
        .clone();
    let cur = ctx
        .task_info(&claim.task_id)
        .map(|t| t.stage_id)
        .unwrap_or_default();
    ctx.move_task_if_changed(&claim.task_id, &cur, &todo)?;
    let _ = ctx.move_to_list(
        &claim.task_id,
        &claim.project_id,
        &ctx.tb.cfg.tasklist_queue.clone(),
    );
    if !note.is_empty() {
        let _ = ctx.post_comment(&claim.task_id, note);
    }
    Ok(())
}

// 主动放弃：待处理 + 列1。
pub fn release(store: &Store, claim_id: &str, reason: &str) -> Result<Claim, String> {
    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}"))?;
    if !claim.is_active() {
        return Err(format!(
            "claim {claim_id} 当前是 {}，不是活动状态",
            claim.state
        ));
    }
    let mut ctx = Ctx::new(store.clone(), false)?;
    let note = if reason.trim().is_empty() {
        String::new()
    } else {
        format!("**↩️ 任务退回队列**\n\n{reason}")
    };
    revert_to_queue(&mut ctx, &claim, &note)?;
    store.update_claim(claim_id, "released", reason)?;
    store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}"))
}

// 回收死会话：running 且心跳超时 -> 待处理 + 列1。
//
// 只在「本次会话没在操作这条 claim」的时候调用（claim / take / status /
// prompt 里），否则会把 agent 自己刚领的任务误回收。
// force=true 时无视超时（tb sweep --force 手动清）。
pub fn sweep(store: &Store, force: bool) -> Result<Vec<String>, String> {
    let cfg = store.load_config();
    let timeout = cfg.heartbeat_timeout_secs as i64;
    let stale: Vec<Claim> = store
        .active_claims()
        .into_iter()
        .filter(|c| c.is_running())
        .filter(|c| {
            if force {
                return true;
            }
            // 心跳新鲜 = 会话活着。
            if crate::store::age_secs(c.heartbeat()) <= timeout {
                return false;
            }
            // 心跳老了，但它若属于某个 tb serve 会话，而那个会话的心跳文件
            // 仍然新鲜，说明进程还在、只是没调 tb（比如在跑长构建），不能回收。
            if let Some(sa) = store.session_age(&c.agent) {
                if sa <= timeout {
                    return false;
                }
            }
            true
        })
        .collect();
    if stale.is_empty() {
        return Ok(Vec::new());
    }
    let mut ctx = Ctx::new(store.clone(), false)?;
    let mut out = Vec::new();
    for c in stale {
        let secs = crate::store::age_secs(c.heartbeat());
        let ago = human_age(secs);
        let note = format!(
            "**⏱ 会话中断，任务已退回队列**\n\n{ago}没有心跳，判定执行会话已断开（限额 / 网络 / 窗口关闭）。\n任务状态改回「待处理」，挪回看板列「{}」。重新接单用 `tb take`。",
            cfg.tasklist_queue
        );
        match revert_to_queue(&mut ctx, &c, &note) {
            Ok(()) => {
                let _ = store.update_claim(&c.claim_id, "reverted", "心跳超时，会话断开");
                out.push(format!(
                    "已回收 {}（{}）：{ago}无心跳",
                    c.claim_id, c.task_name
                ));
            }
            Err(e) => out.push(format!("回收 {} 失败: {e}", c.claim_id)),
        }
    }
    Ok(out)
}

// ---- skill ----
#[derive(Clone)]
pub struct SkillItem {
    pub task_id: String,
    pub name: String,
    pub content: String,
    pub on: bool,
}

pub fn list_skills(store: &Store, project_id: &str) -> Result<Vec<SkillItem>, String> {
    let mut ctx = Ctx::new(store.clone(), false)?;
    // skill 是「任务类型」(sfc)，不是阶段。按 sfc 筛全项目任务，不限阶段。
    let skill_type_id = ctx.resolve_skill_type_id(project_id)?;
    let tasks = ctx.list_all_tasks(project_id)?;
    let switches = store.load_switches();
    let sw = switches.get(project_id).cloned().unwrap_or_default();
    let mut out: Vec<SkillItem> = tasks
        .into_iter()
        .filter(|t| t.sfc_id == skill_type_id)
        .map(|t| {
            let on = sw.get(&t.title).cloned().unwrap_or(false);
            SkillItem {
                task_id: t.id,
                name: t.title,
                content: t.content,
                on,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn push_skill(
    store: &Store,
    project_id: &str,
    name: &str,
    content: &str,
) -> Result<String, String> {
    let mut ctx = Ctx::new(store.clone(), false)?;
    let skill_type_id = ctx.resolve_skill_type_id(project_id)?;
    // 同名 skill 已存在则更新内容（skill 按任务类型识别，不限阶段）
    let tasks = ctx.list_all_tasks(project_id)?;
    if let Some(exist) = tasks
        .iter()
        .find(|t| t.sfc_id == skill_type_id && t.title == name)
    {
        ctx.update_task_content(&exist.id, name, content)?;
        return Ok(format!("已更新 skill: {name} ({})", exist.id));
    }
    // 新建：Teambition 任务必须有工作流状态。skill 是参考资料，放「已解决」，
    // 让「待处理」保持干净（只留真正待办的任务）。claim 仍按 sfc 跳过 skill 兜底。
    let stages = ctx.ensure_stages(project_id)?;
    let stage = ctx
        .find_stage(&stages, STAGE_DONE)
        .ok_or("缺少「已解决」阶段")?
        .clone();
    let id = ctx.create_task(project_id, &stage, name, content, &skill_type_id)?;
    Ok(format!("已创建 skill: {name} ({id})"))
}

// ---- agent 文案 ----

// 本次任务：用户在文案里直接派下来的活。
//
// 有它 -> agent 走 `tb start` 新建一条「进行中 + 列2」的任务开工；
// 没有 -> agent 走 `tb claim` 去列1 队列领现成的。
//
// body 落盘成文件，文案里只给路径，让 agent 用 `--file` 引用。
// 目的是不让长正文在命令行里二次转义——手抄正文是这类流程最容易出错的地方。
#[derive(Clone, Default)]
pub struct TaskDraft {
    pub title: String,
    pub body: String,
    // 落盘后的绝对路径。空表示没落盘（纯 MCP 场景可以直接传 note）。
    pub body_path: String,
}

impl TaskDraft {
    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.body.trim().is_empty()
    }
}

// 文件名安全化：保留中英文数字和 - _，空白换 -，其余（/\:*?"<>|）丢掉。
fn safe_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().take(40) {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push_str("task");
    }
    out
}

// 把任务正文落盘到 data/tasks/<slug>.md，返回路径。
//
// 首行固定写成 `# <标题>`：这样 agent 用 `tb start --file <路径>` 时
// 标题可以从文件里推断出来，命令行上完全不用传标题，不会因为引号转义出错。
pub fn save_task_draft(store: &Store, title: &str, body: &str) -> Result<String, String> {
    let dir = store.root.join("tasks");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 tasks 目录失败: {e}"))?;
    let path = dir.join(format!("{}.md", safe_name(title)));
    // 正文如果自己已经以同名 `# 标题` 开头，就别再加一行，否则任务正文里标题会出现两遍。
    let mut rest = body.trim();
    if let Some((first, tail)) = rest.split_once('\n') {
        if first.trim().trim_start_matches('#').trim() == title.trim() {
            rest = tail.trim();
        }
    } else if rest.trim_start_matches('#').trim() == title.trim() {
        rest = "";
    }
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", title.trim()));
    if !rest.is_empty() {
        s.push_str(rest);
        s.push('\n');
    }
    std::fs::write(&path, s).map_err(|e| format!("写任务正文失败: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

// UI 上两个「复制 agent 文案」按钮的模式。
//
// 两个按钮的区别只在于「本次任务」草稿要不要带进文案：
// - ThisTask（复制agent文案(本次任务)）：带草稿，agent 用 `tb start` 开工；
// - Queue（复制agent文案(领列1任务)）：一律不带草稿，agent 只能去队列「待处理」领。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyMode {
    ThisTask,
    Queue,
}

// 按按钮模式决定「本次任务」草稿怎么组装。
//
// Queue 模式强制丢弃草稿：用户点了「领列1任务」就是想派 agent 去队列，
// 如果还把界面里残留的草稿带进去，agent 会新建任务而不是去领，行为就跑偏了。
pub fn draft_for_mode(store: &Store, raw: &str, mode: CopyMode) -> Option<TaskDraft> {
    let raw = match mode {
        CopyMode::ThisTask => raw,
        CopyMode::Queue => "",
    };
    if raw.trim().is_empty() {
        return None;
    }
    // 标题 = 第一行非空内容，去掉 `# ` 前缀，最多 60 字。
    let title = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| {
            l.trim()
                .trim_start_matches("# ")
                .trim()
                .chars()
                .take(60)
                .collect::<String>()
        })
        .unwrap_or_else(|| "本次任务".to_string());
    // 正文落盘：文案里只给路径，agent 用 --file 引用，不用手抄长正文。
    let body_path = save_task_draft(store, &title, raw).unwrap_or_default();
    Some(TaskDraft {
        title,
        body: raw.to_string(),
        body_path,
    })
}

// 给 agent 的工作方式说明。两个 prompt 函数共用，避免文案分叉。
//
// draft 有值 = 用户在文案里派了活（走 start 新建）；
// draft 为空 = 让 agent 去队列领（走 claim）。
fn workflow_text(
    exe: &str,
    project_id: &str,
    cfg: &Config,
    draft: Option<&TaskDraft>,
) -> String {
    let t = cfg.heartbeat_timeout_secs / 60;
    let mut s = String::new();
    let has_task = draft.map(|d| !d.is_empty()).unwrap_or(false);
    s.push_str("工作方式（必须按这个闭环走，不要自己直接去改任务状态和看板列）:\n");

    let mut step = 1;
    if has_task {
        let d = draft.unwrap();
        s.push_str(&format!(
            "{step}. 先登记开工——这是第一条命令，不要跳过，也不要去队列领:\n"
        ));
        if d.body_path.is_empty() {
            s.push_str(&format!(
                "   {exe} start --project {project_id} --title \"{}\" --note \"<上面正文>\"\n",
                d.title.replace('"', "'")
            ));
        } else {
            s.push_str(&format!(
                "   {exe} start --project {project_id} --file \"{}\"\n",
                d.body_path
            ));
        }
        s.push_str(&format!(
            "   它会在看板列「{}」新建一条「进行中」任务并打印 claim_id。**不要自己建任务、不要自己改状态。**\n",
            cfg.tasklist_claim
        ));
        s.push_str("   如果它返回「该任务已开工，复用原 claim」，说明已经登记过了，直接用那个 claim_id。\n");
    } else {
        s.push_str(&format!(
            "{step}. 先领任务：`{exe} claim --project {project_id}`，只领看板列「{}」队列里的任务。\n",
            cfg.tasklist_queue
        ));
        s.push_str("   用户若直接给了任务链接，改用 `");
        s.push_str(exe);
        s.push_str(" take <链接>`。\n");
        s.push_str("   **没有可领的任务就向用户说明并停下，不要自己造任务。**\n");
    }
    step += 1;

    s.push_str(&format!(
        "{step}. 按上面已启用的 skill 指引执行。执行中可把进展写入共享记忆目录。\n"
    ));
    s.push_str(&format!(
        "   长操作（构建 / 测试 / 批量请求）**前后各跑一次** `{exe} beat <claim_id>`。\n"
    ));
    step += 1;

    s.push_str(&format!(
        "{step}. 需要用户确认或补充信息：先提问，再**同步等回复**（两步都要做）：\n"
    ));
    s.push_str(&format!(
        "   a) `{exe} ask <claim_id> --question \"问题\"` —— 状态变「{}」+ 看板列「{}」，问题写成任务评论。\n",
        STAGE_WAIT, cfg.tasklist_wait
    ));
    s.push_str(&format!(
        "   b) `{exe} wait <claim_id>` —— 阻塞轮询任务评论。用户在 Teambition 回一句，它就返回\n"
    ));
    s.push_str(&format!(
        "      回复正文并把状态推回「{0}」+ 列「{1}」。用户**不需要**再回到会话里发一句「继续」。\n",
        STAGE_DOING, cfg.tasklist_claim
    ));
    s.push_str("      退出码 0 = 读到回复（直接照回复继续干）；\n");
    s.push_str(&format!(
        "      退出码 2 = 本轮 {0} 秒内没等到（**再跑一次** `{exe} wait <claim_id>` 接着等，\n",
        DEFAULT_WAIT_TIMEOUT_SECS
    ));
    s.push_str("        可以连续等很多轮，直到用户回复或会话结束）；\n");
    s.push_str("      退出码 3 = 任务在别处被收尾了（停止等待，别再 ask）。\n");
    s.push_str("      等的时候不要干别的，也不要重复 ask。\n");
    step += 1;

    s.push_str(&format!(
        "{step}. 干完：`{exe} done <claim_id>`（已解决 + 列「{}」）。作废：`{exe} close <claim_id>`（关闭 + 列「{}」）。\n",
        cfg.tasklist_done, cfg.tasklist_closed
    ));
    step += 1;

    s.push_str(&format!(
        "{step}. 主动放弃：`{exe} release <claim_id> --reason \"原因\"`（退回 待处理 + 列「{}」）。\n",
        cfg.tasklist_queue
    ));
    step += 1;

    s.push_str(&format!(
        "   会话断开（限额 / 网络 / 窗口关闭）超过 {t} 分钟没心跳，任务会被自动退回 待处理 + 列「{}」。\n",
        cfg.tasklist_queue
    ));
    s.push_str(&format!(
        "   「{0}」状态永不超时回收——用户可能隔一天才回，`{exe} wait` 挂多久都安全。\n",
        STAGE_WAIT
    ));
    s.push_str(&format!(
        "{step}. 每条任务必须终结于 ask / done / close 三者之一，不许半路停手不管。\n"
    ));
    s.push_str(&format!(
        "\n会话标识：如果同一目录可能同时跑多个会话（比如两个 WorkBuddy 会话 + 一个 dsh），\n\
         先跑 `{exe} hello --name <会话名>` 给自己命名，否则两个会话共用一个标识，\n\
         会话存活判定会互相顶。不命名也能用，只是多会话时不安全。\n"
    ));
    s
}

// 文案里「本次任务」那一段。放在 header 之后、skill 之前——
// agent 第一眼就该知道这次要干什么，而不是先读一堆 skill 再去找任务。
fn task_section(d: &TaskDraft) -> String {
    let mut s = String::new();
    s.push_str("## 本次任务（用户已指定，不要再去队列领）\n");
    s.push_str(&format!("标题: {}\n", d.title.trim()));
    if !d.body_path.is_empty() {
        s.push_str(&format!("正文文件: {}\n", d.body_path));
    }
    let body = d.body.trim();
    if !body.is_empty() {
        s.push_str("\n正文:\n");
        if body.chars().count() > 4000 {
            let head: String = body.chars().take(4000).collect();
            s.push_str(&head);
            s.push_str("\n\n...（正文过长已截断，完整内容见上面的正文文件）\n");
        } else {
            s.push_str(body);
            s.push('\n');
        }
    }
    s.push('\n');
    s
}

// 文案正文。CLI 和 UI 两条路共用，避免两份文案走偏。
fn prompt_body(
    store: &Store,
    project_id: &str,
    project_name: &str,
    skills: &[SkillItem],
    draft: Option<&TaskDraft>,
) -> String {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "tb".to_string());
    let mem = store.memory_dir(project_id);
    let mut s = String::new();
    s.push_str(&format!("# 项目: {project_name}\n"));
    s.push_str(&format!("项目 ID: {project_id}\n"));
    s.push_str(&format!("共享记忆目录: {}\n", mem.display()));
    s.push_str(&format!("工具路径: {exe}\n\n"));

    if let Some(d) = draft {
        if !d.is_empty() {
            s.push_str(&task_section(d));
        }
    }

    let enabled: Vec<&SkillItem> = skills.iter().filter(|k| k.on).collect();
    if enabled.is_empty() {
        s.push_str("已启用 skill: 无\n\n");
    } else {
        s.push_str(&format!("已启用 skill ({} 个):\n", enabled.len()));
        for k in &enabled {
            s.push_str(&format!("\n--- skill: {} ---\n{}\n", k.name, k.content));
        }
        s.push('\n');
    }
    s.push_str(&workflow_text(
        &exe,
        project_id,
        &store.load_config(),
        draft,
    ));
    s
}

pub fn agent_prompt(
    store: &Store,
    project_id: &str,
    project_name: &str,
    draft: Option<&TaskDraft>,
) -> Result<String, String> {
    // 不吞错误：读不到 skill 要让用户看到原因，而不是默默生成一份没有 skill 的文案
    let skills = list_skills(store, project_id)?;
    Ok(prompt_body(store, project_id, project_name, &skills, draft))
}

// 供 UI 用：从已有 skill 列表直接生成文案，不再请求网络。
pub fn agent_prompt_from(
    store: &Store,
    project_id: &str,
    project_name: &str,
    skills: &[SkillItem],
    draft: Option<&TaskDraft>,
) -> String {
    prompt_body(store, project_id, project_name, skills, draft)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("tbhub-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Store { root }
    }

    // 「复制agent文案(本次任务)」：填了正文就带草稿，标题取首行。
    #[test]
    fn copy_mode_this_task_uses_draft() {
        let store = tmp_store("this");
        let d = draft_for_mode(
            &store,
            "# 验证用的任务标题\n这是正文第二行",
            CopyMode::ThisTask,
        )
        .expect("填了正文就应该有草稿");
        assert_eq!(d.title, "验证用的任务标题");
        assert!(d.body.contains("这是正文第二行"));
        // 正文落盘了，agent 用 --file 引用
        assert!(!d.body_path.is_empty());
        let saved = std::fs::read_to_string(&d.body_path).unwrap();
        assert!(saved.starts_with("# 验证用的任务标题"));
        assert!(saved.contains("这是正文第二行"));
    }

    // 「复制agent文案(领列1任务)」：不管界面里有没有草稿，一律不带。
    // 否则 agent 会新建任务，而不是去队列领，行为跑偏。
    #[test]
    fn copy_mode_queue_drops_draft() {
        let store = tmp_store("queue");
        assert!(
            draft_for_mode(&store, "# 某个残留草稿\n正文", CopyMode::Queue).is_none(),
            "领列1任务 模式必须忽略草稿"
        );
        // 也不该落盘任何文件
        let tasks = store.root.join("tasks");
        let leftovers = std::fs::read_dir(&tasks)
            .map(|rd| rd.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "领列1任务 模式不该写任务正文");
    }

    // 空正文 = 没有草稿，agent 去队列领。
    #[test]
    fn copy_mode_empty_draft_is_none() {
        let store = tmp_store("empty");
        assert!(draft_for_mode(&store, "", CopyMode::ThisTask).is_none());
        assert!(draft_for_mode(&store, "   \n  ", CopyMode::ThisTask).is_none());
    }

    // 两种模式产出的文案必须真的不一样：带草稿的走 start，队列的走 claim。
    #[test]
    fn two_modes_produce_different_prompts() {
        let store = tmp_store("prompts");
        let pid = "p-test";
        let name = "测试项目";
        let skills: Vec<SkillItem> = Vec::new();
        let draft = draft_for_mode(&store, "# 标题一\n正文", CopyMode::ThisTask);
        let p_this = agent_prompt_from(&store, pid, name, &skills, draft.as_ref());
        let p_queue = agent_prompt_from(&store, pid, name, &skills, None);
        assert!(p_this.contains("本次任务"), "本次任务 版应带本次任务 段");
        assert!(p_this.contains("start"), "本次任务 版应让 agent 跑 start");
        assert!(p_queue.contains("claim"), "领列1 版应让 agent 跑 claim");
        assert_ne!(p_this, p_queue);
    }
}
