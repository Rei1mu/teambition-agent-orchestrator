use crate::config::*;
use crate::store::{new_id, Claim, Store};
use crate::tb::Tb;
use crate::tools::*;
use serde_json::json;


// 高层业务操作。CLI / MCP / UI 三处共用。

// 工作流状态的 kind 三态（Teambition）：start=开始 / unset=进行中 / end=结束。
// 只有 kind=end 的任务才算「已完成」(isDone=true)。按 TBHub 的阶段名固定分配：
//   未完成=start（新任务落这里）、进行中=unset、已解决/已完成=end。
// 约束：一个任务流只能有一个 start，若已被别的状态占用，未完成退化为 unset。
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
// 本地数据目录名。绑定（project）和会话标识（session）都放这里。
// 旧版叫 .tba，读到就顺手改名成 .tba——只改目录名，不动内容。
pub const LOCAL_DIR: &str = ".tba";
const LEGACY_DIR: &str = ".tbhub";

// 取 <dir> 下可用的本地数据目录。
// 有 .tba 直接用；只有 .tba 就先改名；改名失败（权限/占用）退回旧的，至少能用。
fn local_dir_at(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let cur = dir.join(LOCAL_DIR);
    if cur.is_dir() {
        return Some(cur);
    }
    let old = dir.join(LEGACY_DIR);
    if old.is_dir() {
        if std::fs::rename(&old, &cur).is_ok() {
            return Some(cur);
        }
        return Some(old);
    }
    None
}

// 解析顺序：TBA_AGENT 环境变量 > 向上找 .tba/session 文件 > "cli"。
// 用文件而不是环境变量作主通道，是因为 agent 每次跑命令都是新 shell，
// 环境变量不跨调用保留，而文件在。
pub fn current_agent() -> String {
    if let Ok(v) = std::env::var("TBA_AGENT") {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        if let Some(local) = local_dir_at(&d) {
            let p = local.join("session");
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Some(t) = s
                    .lines()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                {
                    return t.to_string();
                }
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
    let d = dir.join(LOCAL_DIR);
    std::fs::create_dir_all(&d).map_err(|e| format!("创建 {LOCAL_DIR} 目录失败: {e}"))?;
    let path = d.join("session");
    std::fs::write(&path, format!("{tag}\n")).map_err(|e| format!("写会话标识失败: {e}"))?;
    Ok((tag, path))
}

// 本地项目绑定：<目录>/.tba/project 存 project id。
//
// 和 .tba/session 用同一套「向上找」规则：在仓库子目录跑命令也能认出
// 父目录绑定的项目，不用每层都写一份。
pub fn read_project_binding() -> Option<String> {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        if let Some(local) = local_dir_at(&d) {
            let p = local.join("project");
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Some(t) = s
                    .lines()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                {
                    return Some(t.to_string());
                }
            }
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

// 把 project id 记到当前目录的 .tba/project。返回写入的文件路径。
pub fn write_project_binding(project_id: &str) -> Result<std::path::PathBuf, String> {
    let dir = std::env::current_dir().map_err(|e| format!("拿不到当前目录: {e}"))?;
    let d = dir.join(LOCAL_DIR);
    std::fs::create_dir_all(&d).map_err(|e| format!("创建 {LOCAL_DIR} 目录失败: {e}"))?;
    let path = d.join("project");
    std::fs::write(&path, format!("{project_id}\n"))
        .map_err(|e| format!("写项目绑定失败: {e}"))?;
    Ok(path)
}

// 当前目录名，给「没指定项目名」时兜底用。
pub fn cwd_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|d| {
            d.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "tb-project".to_string())
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
                    // 请求参数名是 pageToken；nextPageToken 只是响应字段名。
                    obj.insert("pageToken".to_string(), json!(t));
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

    // 建一个空白项目。
    //
    // project.create 只接受 name；传 organizationId 会被拒（not allowed）。
    // 返回体形状不稳定，所以优先从返回体取 id，取不到就重新列一遍按名字找。
    pub fn create_project(&mut self, name: &str) -> Result<ProjectInfo, String> {
        let tool = self.need("create_project", &self.map.create_project)?;
        let res = self.tb.call(&tool, json!({"name": name}))?;
        let inner = res.get("result").cloned().unwrap_or(serde_json::Value::Null);
        let id = get_str(&res, &["id", "_id", "projectId"])
            .or_else(|| get_str(&inner, &["id", "_id", "projectId"]));
        if let Some(id) = id {
            let pname = get_str(&res, &["name", "title"])
                .or_else(|| get_str(&inner, &["name", "title"]))
                .unwrap_or_else(|| name.to_string());
            return Ok(ProjectInfo { id, name: pname });
        }
        let list = self.list_projects()?;
        list.into_iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("建项目「{name}」后查不到，请去网页确认。原始返回: {res}"))
    }

    // 确保项目存在（幂等）。解析顺序：显式 id > 显式名字 > 本地 .tba/project 绑定。
    // 返回 (项目, 是否新建)。allow_create=false 时找不到就报错，绝不静默新建。
    pub fn ensure_project(
        &mut self,
        name: Option<&str>,
        project_id: Option<&str>,
        allow_create: bool,
    ) -> Result<(ProjectInfo, bool), String> {
        let list = self.list_projects()?;
        if let Some(pid) = project_id.map(str::trim).filter(|s| !s.is_empty()) {
            // 显式 id 是用户给的：列表里查不到也照用（列表接口可能漏项），
            // 后续 ensure_stages / list_skills 会真正校验它。
            return match list.iter().find(|p| p.id == pid) {
                Some(p) => Ok(((*p).clone(), false)),
                None => {
                    eprintln!("提示: 项目列表里没找到 id「{pid}」，按给定 id 继续");
                    Ok((
                        ProjectInfo {
                            id: pid.to_string(),
                            name: pid.to_string(),
                        },
                        false,
                    ))
                }
            };
        }
        if let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) {
            let hits: Vec<&ProjectInfo> = list.iter().filter(|p| p.name == name).collect();
            return match hits.len() {
                1 => Ok(((*hits[0]).clone(), false)),
                0 if allow_create => Ok((self.create_project(name)?, true)),
                0 => Err(format!("没有名为「{name}」的项目。去掉 --project 并加 --name 来新建")),
                n => Err(format!(
                    "有 {n} 个项目都叫「{name}」。用 --project <id> 指定具体哪个"
                )),
            };
        }
        if let Some(bound) = read_project_binding() {
            return match list.iter().find(|p| p.id == bound) {
                Some(p) => Ok(((*p).clone(), false)),
                None => Err(format!(
                    "本地绑定指向的项目「{bound}」已不存在（可能被删了）。用 --name 或 --project 重新指定"
                )),
            };
        }
        Err("没指定项目。用 --project <id> 指定已有项目，或 --name <名字> 新建".to_string())
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

    // 补齐 5 个阶段（工作流状态 / tfs）。缺哪个建哪个。返回最终阶段列表。
    // skill / mem 两个用途状态不在 ALL_STAGES 里，由 ensure_state 单独补。
    //
    // kind 三态（实测 + 官方文档）：start(开始) / unset(进行中) / end(结束)。
    // 按阶段名固定分配：未完成=start、进行中/待回复=unset、已解决/已完成=end。
    // 约束：start 整个任务流只能有一个（建第二个报 ShouldOnlyOneStartStatus），
    // 所以若 start 已被别的状态占用，未完成退化为 unset。
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

    // 解析项目绑定的某个「任务类型」id（sfc）。skill / 项目记忆 共用。
    //
    // 任务类型不是阶段，而是第 4 维（sfc）。每个项目按名字解析同名类型：
    //   1) 先 sfc-query 按 projectId 拉全部类型，**精确等值**匹配 name；
    //   2) 查不到就用 sfc-create 建一个（带 projectId + taskflowId，项目绑定）。
    // 注意必须精确等值匹配：曾经误建过「项目skill」类型，包含 "skill"
    // 但不是同一个，不能用 contains 匹配。
    #[allow(dead_code)] // 保留：任务类型/标签是 Teambition 的原生维度，当前流程不用但封装还有价值
    pub fn resolve_task_type_id(
        &mut self,
        project_id: &str,
        name: &str,
    ) -> Result<String, String> {
        // 1) 先查。只传 projectId：sfc-query 按项目过滤即可，
        //    且不能传 taskflowId——build_args 的模糊匹配会把它错配到 id 属性上。
        if let Some(t) = &self.map.list_task_types {
            if let Ok(res) = self.tb.call_smart(t, &[("projectId", json!(project_id))]) {
                for item in extract_list(&res, 3) {
                    let n = get_str(&item, &["name", "title", "sfcName"]).unwrap_or_default();
                    if n == name {
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
            .ok_or(format!(
                "MCP 工具集里找不到「任务类型」(sfc) 查询/创建工具，无法初始化 {name} 类型"
            ))?;
        let res = self.tb.call(
            &t,
            json!({"projectId": project_id, "taskflowId": tf, "name": name}),
        )?;
        // 返回体是 {"code":200,"result":{"id":"...","name":"..."}}——
        // id 埋在 result 里，直接取顶层会取不到（skill 因为先查后建，一直没暴露这个坑）。
        get_str(&res, &["id", "_id", "sfcId"])
            .or_else(|| {
                res.get("result")
                    .and_then(|r| get_str(r, &["id", "_id", "sfcId"]))
            })
            .ok_or_else(|| format!("创建 {name} 任务类型失败，无法解析 id: {res}"))
    }

    // 查一个「工作流状态」（tfs）。找不到返回 None。
    //
    // skill / mem 都是这类状态：不是阶段、不参与生命周期，只是用途标记。
    // 所以不能走 ensure_stages（那会按 5 个阶段名去建），要单独按名字查/建。
    pub fn find_state(&mut self, project_id: &str, name: &str) -> Result<Option<StageInfo>, String> {
        Ok(self
            .list_stages(project_id)?
            .into_iter()
            .find(|s| s.name == name))
    }

    // 查一个工作流状态，没有就建（kind=unset：既不是 start 也不是 end）。
    // 幂等：已存在直接返回。
    pub fn ensure_state(&mut self, project_id: &str, name: &str) -> Result<StageInfo, String> {
        if let Some(s) = self.find_state(project_id, name)? {
            return Ok(s);
        }
        let tf = self.taskflow_id(project_id)?;
        self.create_stage(project_id, &tf, name, "unset")?;
        self.find_state(project_id, name)?
            .ok_or_else(|| format!("建了状态「{name}」但查不到，请去 Teambition 网页确认"))
    }

    // 建一条「普通 Task + 指定状态」的任务，正文写进备注。
    //
    // Teambition 只允许把任务**建在 start 状态**（否则报
    // "taskflowstatus should be start kind"），所以统一走「先建在未完成 → 再挪到目标状态」。
    // 标题 = content，正文 = note。
    pub fn create_marked_task(
        &mut self,
        project_id: &str,
        title: &str,
        note: &str,
        state: &StageInfo,
    ) -> Result<String, String> {
        let stages = self.ensure_stages(project_id)?;
        let start = self
            .find_stage(&stages, STAGE_TODO)
            .ok_or_else(|| format!("缺少「{STAGE_TODO}」阶段（建任务必须先落在 start 状态）"))?
            .clone();
        let id = self.create_task(project_id, &start, title, note, "")?;
        self.move_task_if_changed(&id, "", state)?;
        Ok(id)
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
        // enrichFields=tag：让返回带 tagNames，claim 才能按「项目记忆」标签跳过记忆分片。
        let precise: [serde_json::Value; 2] = [
            json!({"projectId": project_id, "enrichFields": "tag",
                   "tql": format!("taskflowstatusId = '{sid}' AND isArchived = false")}),
            json!({"projectId": project_id, "enrichFields": "tag",
                   "tql": format!("tfsId = '{sid}' AND isArchived = false")}),
        ];
        // 后三条是粗筛：只有筛出东西才算数，空就继续换。
        let loose: [serde_json::Value; 3] = [
            json!({"projectId": project_id, "enrichFields": "tag",
                   "tql": format!("stageId = '{sid}' AND isArchived = false")}),
            json!({"projectId": project_id, "enrichFields": "tag", "tql": format!("isArchived = false")}),
            json!({"projectId": project_id, "enrichFields": "tag"}),
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

    // 按看板列查任务（不按工作流状态）。
    //
    // 用于「列1 里有什么」这种判断：用户可能把任意状态的任务拖回列1，
    // 按状态查会漏掉它们。
    //
    // 只能全量拉再本地筛：TQL 里 `stageId = '<列id>'` 实测返回 0 条却报成功
    // （跟按 tfsId 查不是一回事），没法靠它过滤。任务多的项目会慢一点，
    // 但正确性优先——按列查错会让「拖回列1 重接」整个失效。
    pub fn list_tasks_in_list(
        &mut self,
        project_id: &str,
        list_id: &str,
    ) -> Result<Vec<TaskInfo>, String> {
        let tool = self.need("list_tasks", &self.map.list_tasks)?;
        let base = json!({
            "projectId": project_id,
            "enrichFields": "tag",
            "tql": "isArchived = false"
        });
        let list = self.list_tasks_page(&tool, base)?;
        let mut out: Vec<TaskInfo> = list
            .into_iter()
            .filter(|t| t.list_id == list_id)
            .collect();
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
        Ok(out)
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
                    // 请求参数名是 pageToken；nextPageToken 只是响应字段名。
                    obj.insert("pageToken".to_string(), json!(t));
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
    #[allow(dead_code)] // 保留：任务类型/标签是 Teambition 的原生维度，当前流程不用但封装还有价值
    pub fn list_all_tasks(&mut self, project_id: &str) -> Result<Vec<TaskInfo>, String> {
        let tool = self.need("list_tasks", &self.map.list_tasks)?;
        // enrichFields=tag：让返回带 tagNames，记忆分片靠「项目记忆」标签识别。
        let candidates: [serde_json::Value; 2] = [
            json!({"projectId": project_id, "tql": "isArchived = false", "enrichFields": "tag"}),
            json!({"projectId": project_id, "enrichFields": "tag"}),
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

    // 给任务打标签（按名称，服务端关联到已有标签；项目里没有会自动建）。
    // 记忆分片靠「项目记忆」标签被识别，所以建完分片必须打上。
    #[allow(dead_code)] // 保留：任务类型/标签是 Teambition 的原生维度，当前流程不用但封装还有价值
    pub fn add_task_tag(&mut self, task_id: &str, tag: &str) -> Result<(), String> {
        let tool = self
            .find_tool("teambition.task.update-tags")
            .ok_or("MCP 工具集里找不到打标签工具（task.update-tags）")?;
        self.tb.call(&tool, json!({"taskId": task_id, "add": tag}))?;
        Ok(())
    }

    pub fn update_task_content(
        &mut self,
        task_id: &str,
        title: &str,
        content: &str,
    ) -> Result<(), String> {
        let tool = self.need("update_task", &self.map.update_task)?;
        // **标题和备注分两次调用，绝不混在一个请求里**。
        //
        // 实测（2026-10-27，三轮验证）：只传 `{"note": ...}` → 备注更新成功、标题不变；
        // 只传 `{"content": ...}` → 标题更新。混传的结果历史上出现过「标题被写成备注」，
        // 所以这里分开调，各管一个字段，行为确定。
        //
        // 备注(note) 是内置字段：不占自定义字段额度、网页任务详情直接可见可编辑，
        // 项目记忆和 skill 的正文都存在这里。
        if !title.trim().is_empty() {
            self.tb
                .call(&tool, json!({"taskId": task_id, "content": title}))?;
        }
        if !content.trim().is_empty() {
            self.tb
                .call(&tool, json!({"taskId": task_id, "note": content}))?;
        }
        Ok(())
    }

    // 往任务备注（note）后面追加一段。返回写入后的完整备注。
    //
    // 为什么是追加而不是覆盖：备注里可能已经有用户写的需求原文（tba start 写进去的），
    // 覆盖会把它毁掉。所以先读回、拼上、再整段写回。
    //
    // 为什么 agent 写备注而不是评论：
    //   1. 一个 agent 一个任务，不存在多个 agent 同时写同一条备注的覆盖问题。
    //   2. 评论留给用户。agent 不写评论，wait 读评论时就只会读到用户的话，
    //      不用再靠「内容是不是纯数字」去猜这条是谁发的。
    //   3. 备注在网页任务详情第一屏可见，结论不会被评论淹没。
    pub fn append_task_note(&mut self, task_id: &str, md: &str) -> Result<String, String> {
        let text = md.trim();
        if text.is_empty() {
            return Err("要写进备注的内容是空的".into());
        }
        let old = read_task_notes(self, &[task_id.to_string()])?
            .into_iter()
            .next()
            .map(|(_, n)| n)
            .unwrap_or_default();
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M");
        let merged = if old.trim().is_empty() {
            format!("#### {stamp}\n\n{text}")
        } else {
            format!("{}\n\n---\n\n#### {stamp}\n\n{text}", old.trim_end())
        };
        self.update_task_content(task_id, "", &merged)?;
        Ok(merged)
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

// 「队列里没活」的唯一错误前缀。tb go 靠它把「没活」和「真出错」分开：
// 没活要退出码 0 + EMPTY，真出错要报错。改文案必须改这里，否则分支会错。
pub const NO_TASK_PREFIX: &str = "「未完成」里没有可领的任务。";

// 「等待中断，先处理」的错误前缀。go 靠它把这种错误转成 BLOCKED（退出码 5）。
pub const BLOCKED_PREFIX: &str = "BLOCKED: ";

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
        .ok_or("缺少「未完成」阶段")?;
    // 不需要再按 sfc / 标签剔 skill 和记忆：它们现在是**独立工作流状态**
    // （skill / mem），根本不会出现在列1 里。
    let cfg = ctx.tb.cfg.clone();
    let queue_id = ctx
        .task_lists(project_id)
        .unwrap_or_default()
        .into_iter()
        .find(|l| l.name == cfg.tasklist_queue)
        .map(|l| l.id);
    // 列1 是「重新分配」的信号，所以**按列查，不按状态查**。
    // 用户可能把「进行中」「待回复」「已解决」的任务拖回列1 要求重做，
    // 按状态（未完成）查会把这些任务全漏掉。
    // 数据源没有列这一维时退回按状态查，保证降级场景还能用。
    let candidates: Vec<TaskInfo> = match &queue_id {
        Some(qid) => ctx.list_tasks_in_list(project_id, qid)?,
        None => ctx.list_tasks(project_id, todo)?,
    };
    let doing = ctx
        .find_stage(&stages, STAGE_DOING)
        .ok_or("缺少「进行中」阶段")?
        .clone();

    // 等待中断（ask 被丢后台 / 进程被清理）要拦住，但**列1 的除外**：
    // 任务被拖回列1 就是用户明确说「重来」，那条等待已经作废，不该再拦。
    // 这个检查必须放在知道「列1 有谁」之后，所以从函数开头挪到了这里。
    let queue_ids: Vec<String> = candidates.iter().map(|t| t.id.clone()).collect();
    let orphans: Vec<Claim> = orphan_waits(store)
        .into_iter()
        .filter(|c| !queue_ids.contains(&c.task_id))
        .collect();
    if !orphans.is_empty() {
        return Err(format!("{BLOCKED_PREFIX}{}", orphan_block_msg(&orphans)));
    }

    // ---- 临界区：读台账 -> 选任务 -> 写台账 ----
    // 锁外已经做完全部网络查询，这里只碰本地文件，持锁时间很短。
    let lock = store.lock_claims(30)?;
    let mut tasks = candidates;
    // 正在被别的会话干的任务跳过：running 且心跳新鲜 = 那个会话还活着。
    //
    // 注意这里**不能**把所有有活动 claim 的都跳过。tba 改不了看板列
    // （MCP 没有移列工具，move_to_list 映射为空），所以任务领走之后往往还留在列1。
    // 如果按「有活动 claim 就跳过」，用户拖回列1 也永远领不到；
    // 如果全不跳过，agent 自己下次开工就会把手上正在干的任务重新领一遍。
    // 分界线就是心跳：活着的 running 让开，其余（waiting / 死掉的 running）都接管。
    let heartbeat_limit = cfg.heartbeat_timeout_secs as i64;
    tasks.retain(|t| match store.find_active_claim(&t.id) {
        Some(c) => {
            !(c.is_running() && crate::store::age_secs(c.heartbeat()) <= heartbeat_limit)
        }
        None => true,
    });
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
            "{NO_TASK_PREFIX}注意 claim 只领看板列「{}」里的任务——\
             列「{}」是 agent 的地盘（进行中 / 待回复），不会被重复领。\
             要把某条任务交给 agent，把它拖到列「{}」，或直接用 `tba take <任务链接>`。",
            cfg.tasklist_queue, cfg.tasklist_claim, cfg.tasklist_queue
        )
    })?;

    // 列1 是用户的最高优先级信号。任务出现在列1，说明用户要重接——
    // 台账里哪怕还挂着 running / waiting，也一律作废，重新记账。
    // 旧记录标 reverted 而不是删：留痕能看出「这条被用户拖回列1 重来过」。
    if let Some(old) = store.find_active_claim(&picked.id) {
        let _ = store.update_claim(
            &old.claim_id,
            "reverted",
            "任务被拖回列1，用户要求重接",
        );
    }

    // 已经按队列列过滤过了，所以来源列就是队列列本身，不必再发一次查询。
    let from_list = if picked.list_id.is_empty() {
        String::new()
    } else {
        cfg.tasklist_queue.clone()
    };

    // 任务可能是任意状态被拖回列1 的，stage_from 要如实记下它当时的状态，
    // 不能一律写「未完成」——回滚/排查时那会骗人。
    let from_stage_name = stages
        .iter()
        .find(|s| s.id == picked.stage_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| STAGE_TODO.to_string());

    let claim = Claim {
        claim_id: new_id("clm"),
        task_id: picked.id.clone(),
        task_name: picked.title.clone(),
        project_id: project_id.to_string(),
        stage_from: from_stage_name,
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
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tba status 查看全部"))?;
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

// ---- 外部收尾判定 ----
//
// 规则（用户 2026-10-25 定）：会话只有两种终点——
//   ① 完成：agent 跑 tb done/close，或用户在网页把状态改成 已解决/已完成、把任务拖到列4/列5；
//   ② 被迫结束：会话断开（心跳超时/进程被杀）→ 退回 未完成 + 列1。
// 除这两种外，会话一直 running/waiting，不自动结束。
//
// 关键：被①（用户手动收尾）完成的任务，绝不能又被②回收回列1。所以回收前必须先判定。

// 纯判定：给定任务当前的工作流状态名 + 看板列名，是否已被「外部」完成。
// 返回 Some("closed") / Some("done") / None。列名（数字 1..5）优先于状态名——
// 用户可能只拖了列没改状态。可单测，不碰网络。
pub fn external_done_by(
    stage_name: &str,
    list_name: &str,
    cfg: &crate::config::Config,
) -> Option<&'static str> {
    if list_name == cfg.tasklist_closed {
        return Some("closed");
    }
    if list_name == cfg.tasklist_done {
        return Some("done");
    }
    if stage_name == STAGE_CLOSED {
        return Some("closed");
    }
    if stage_name == STAGE_DONE {
        return Some("done");
    }
    None
}

// 网络版：读任务当前的列名 + 状态名，套 external_done_by。查不到返回 None（宁可漏判也不误判）。
fn external_done_state(ctx: &mut Ctx, project_id: &str, task_id: &str) -> Option<&'static str> {
    let cfg = ctx.tb.cfg.clone();
    let info = ctx.task_info(task_id).ok()?;
    let list_name = if info.list_id.is_empty() {
        String::new()
    } else {
        ctx.list_name_of(project_id, &info.list_id)
    };
    let stage_name = if info.stage_id.is_empty() {
        String::new()
    } else {
        ctx.ensure_stages(project_id)
            .ok()
            .and_then(|st| st.into_iter().find(|s| s.id == info.stage_id))
            .map(|s| s.name)
            .unwrap_or_default()
    };
    external_done_by(&stage_name, &list_name, &cfg)
}

// ---- 接单 / 提问 / 继续 / 放弃 / 回收 ----
//
// 这是「任务跟着会话走」的核心状态机。两个维度组合出五种局面：
//
//   状态(tfs)   看板列   含义
//   未完成      列1      队列里，没人接
//   进行中      列2      agent 正在干
//   待回复      列3      agent 已提问，球在用户这边（未完成）
//   已解决/已完成  列4/5    收尾
//   未完成      列1      会话异常断开，已退回队列
//
// 关键约束：
//   1. claim 必须是「列1 + 未完成」双过滤。待回复虽然已独立成状态，
//      但双过滤仍保留：防止有人手动把「未完成」任务拖到列1 之外再被领走。
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
        return Err("没解析出任务 id。用法: tba take https://www.teambition.com/task/<id>".into());
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
    // 记下接单前它原本在哪个阶段（多半是「未完成」，也可能是别的）。
    let from_stage = stages
        .iter()
        .find(|s| s.id == detail.stage_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| STAGE_TODO.to_string());

    // 已完成/已关闭的任务不该被领。
    if let Some(s) = stages.iter().find(|s| s.id == detail.stage_id) {
        if s.name == STAGE_DONE || s.name == STAGE_CLOSED {
            return Err(format!(
                "任务「{}」当前是「{}」，已完成/已关闭，不能领取。要重做请先在 Teambition 里把它改回「未完成」。",
                detail.title, s.name
            ));
        }
    }

    // 幂等：同一任务已有活动 claim（running 或 waiting），直接返回。
    // 例外：任务已经被拖回队列列（列1）——那是用户明确要求重接，旧 claim 作废。
    if let Some(c) = store.find_active_claim(&task_id) {
        let list_now = ctx.task_list_name(&project_id, &task_id);
        let in_queue = !list_now.is_empty() && list_now == ctx.tb.cfg.tasklist_queue;
        if !in_queue {
            return Ok(TakeResult {
                claim: c,
                warnings: vec![],
                existed: true,
                list_now,
            });
        }
        let _ = store.update_claim(&c.claim_id, "reverted", "任务被拖回列1，用户要求重接");
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
// 建任务必须两步：task.create 只能建在 kind=start 的「未完成」里，
// 建完再挪到「进行中」。所以 claim.stage_from 记「未完成」——
// 会话断开时回滚到「未完成 + 列1」，正好回到队列里等下一个 agent。
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
        .ok_or("缺少「未完成」阶段")?
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

// 「做完了，请确认」的标准问题。`ask --done` 用它，免得 agent 每次自己编数字选项、
// 编漏一个（最常见是忘了 0 收尾）。
pub const DONE_QUESTION: &str = "已完成，请确认：\n\n\
0 = 收尾（转「已解决」+ 列「已解决」）\n\
1 = 先写项目记忆再收尾（回「进行中」，写完再问一次）\n\
2 = 还有问题，继续改（回「进行中」）\n\n\
直接回复数字即可。会话会一直挂着等你，不会自己结束。";

// 提问：进行中 -> 待回复（列3），把问题写进任务备注等用户评论回复。
pub fn ask(store: &Store, claim_id: &str, question: &str) -> Result<Claim, String> {
    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tba status 查看全部"))?;
    if !claim.is_active() {
        return Err(format!(
            "claim {claim_id} 当前是 {}，不能提问（任务已经收尾或已被回收）",
            claim.state
        ));
    }
    let q = question.trim();
    if q.is_empty() {
        return Err("问题不能为空。用法: tba ask <claim_id> --q \"要问用户的话\"".into());
    }
    let mut ctx = Ctx::new(store.clone(), false)?;
    let stages = ctx.ensure_stages(&claim.project_id)?;
    let wait = ctx
        .find_stage(&stages, STAGE_WAIT)
        .ok_or("缺少「待回复」阶段")?
        .clone();

    // 问题必须落到任务上——会话可能断，任务备注不会丢。
    // 写备注而不是评论：评论是用户的通道，agent 不往里写。
    let body = format!(
        "**❓ 需要你回复**\n\n{}\n\n---\n> 问题已写进本任务备注。请在**评论**或**邮件**里回复数字。\n> 会话一直挂着等你，不会自己结束。\n> 任务状态已改为「{}」，看板列「{}」。回复后自动回到「{}」。",
        q,
        STAGE_WAIT,
        ctx.tb.cfg.tasklist_wait,
        STAGE_DOING
    );
    ctx.append_task_note(&claim.task_id, &body)?;
    // 邮件要带**完整备注**：用户在邮箱里就能看到 agent 干了什么、结论是什么，
    // 不用再跳回 Teambition 网页翻。备注读失败不影响提问，退回只发问题。
    let full_note = read_task_notes(&mut ctx, &[claim.task_id.clone()])
        .ok()
        .and_then(|v| v.into_iter().next())
        .map(|(_, n)| n)
        .unwrap_or_default();
    // 时间戳基准：wait 用它过滤「提问之后的评论」。
    // 以前取评论的 createTime，现在 agent 不写评论了，直接取本地时间。
    let asked_at = chrono::Local::now().to_rfc3339();

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
    // 邮件通知放在台账更新之后：即使发信挂了，任务状态也已经是「待回复」。
    // 失败要写进备注——agent 可能把这条命令丢后台，stderr 没人看，
    // 只有落在任务上你才看得见「邮件没发出去」。
    if let Err(e) = notify_by_mail(&ctx.tb.cfg, &claim, q, &full_note) {
        eprintln!("⚠ 邮件没发出去（问题已写进备注，不影响回数字）: {e}");
        let _ = ctx.append_task_note(
            &claim.task_id,
            &format!(
                "**⚠ 邮件通知没发出去**\n\n`{e}`\n\n> 直接在评论里回数字即可，不用等邮件。"
            ),
        );
    }
    store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}"))
}

// 把一段文字追加到**任务备注**（Teambition 的 note 字段）。
//
// 为什么单独做命令：会话记录、思考链这类东西必须跟着任务走，
// 不能只留在 agent 的聊天窗口里（换个会话就没了）。
//
// **agent 只写备注，不写评论。** 评论是用户的输入通道：
//   - 用户手动评论 = 用户的话（wait 读这里）
//   - agent 写备注 = agent 的话（网页第一屏可见）
// 两边各占一个通道，不用再靠内容格式去猜某条是谁发的。
pub fn append_note(store: &Store, claim_id: &str, text: &str) -> Result<String, String> {
    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tba status 查看全部"))?;
    let text = text.trim();
    if text.is_empty() {
        return Err("要写进备注的内容是空的（用 --text 或 --file 给内容）".into());
    }
    let mut ctx = Ctx::new(store.clone(), false)?;
    ctx.append_task_note(&claim.task_id, text)?;
    Ok("已追加到任务备注".to_string())
}

// 到「待回复」时给客户发邮件。
//
// 主题里带 `[TB <claim_id>]` 是收信侧的匹配标记：客户直接回复（保留主题）时，
// tb wait 就能从信箱里认出这是哪条任务的回信。
//
// 返回 Err 而不是静默吞掉：发信失败必须让调用方写进任务备注。
// 以前只 eprintln! 一行，agent 把 ask 丢后台时那行字谁也看不见，
// 用户就会一直纳闷「怎么没收到邮件」。
// 重发某条任务的通知邮件。
//
// 两个用途：
//   ① 之前发信失败过（smtp_from 填错 / 网络断了 / 邮箱满了），补一封；
//   ② 你想在邮箱里再要一份完整上下文，不想翻网页。
//
// 只发信，不动任务状态——它本来就在「待回复」，重发一次不该把它再挪一遍。
pub fn resend_mail(store: &Store, claim_id: &str) -> Result<String, String> {
    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tba status 查看全部"))?;
    let cfg = store.load_config();
    if !cfg.mail_on_ask {
        return Err("mail_on_ask 是关的，不会发通知信。要发就先把开关打开".into());
    }
    let mut ctx = Ctx::new(store.clone(), false)?;
    let note = read_task_notes(&mut ctx, &[claim.task_id.clone()])
        .ok()
        .and_then(|v| v.into_iter().next())
        .map(|(_, n)| n)
        .unwrap_or_default();
    let question = if claim.question.trim().is_empty() {
        DONE_QUESTION
    } else {
        &claim.question
    };
    notify_by_mail(&cfg, &claim, question, &note)?;
    Ok(cfg.mail_to.clone())
}

// 邮件正文。带上**完整备注**——用户在外面收邮件时，光看到一个「已完成，请确认」
// 是没法判断的，得能看到 agent 到底改了什么、结论是什么。
//
// 备注为空（读失败或还没写过）就退回只发问题，别发一封空信。
fn mail_body(claim: &Claim, question: &str, note: &str) -> String {
    let content = if note.trim().is_empty() { question } else { note };
    format!(
        "任务「{name}」需要你回复。\n\n\
         ======== 任务备注（完整） ========\n\n\
         {content}\n\n\
         ================================\n\n\
         链接：https://www.teambition.com/task/{task}\n\n\
         直接回复本邮件即可（请保留标题里的 [TB {cid}]，否则认不出是哪条任务）。",
        name = claim.task_name,
        content = content.trim(),
        task = claim.task_id,
        cid = claim.claim_id,
    )
}

fn notify_by_mail(
    cfg: &Config,
    claim: &Claim,
    question: &str,
    note: &str,
) -> Result<(), String> {
    // 用户主动关的，不算失败。
    if !cfg.mail_on_ask {
        return Ok(());
    }
    if !crate::mail::can_send(cfg) {
        return Err("邮件没配全：缺 SMTP 主机或收件人".to_string());
    }
    let subject = format!("[TB {}] 需要你回复：{}", claim.claim_id, claim.task_name);
    let body = mail_body(claim, question, note);
    match crate::mail::send(cfg, &subject, &body) {
        Ok(()) => {
            eprintln!("已发邮件通知: {}", cfg.mail_to);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

// 继续：读回用户在任务里的回复，未完成 -> 进行中（仍在列2）。
pub fn resume(store: &Store, claim_id: &str) -> Result<(Claim, Vec<CommentInfo>), String> {
    let claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tba status 查看全部"))?;
    if claim.state == "done" || claim.state == "closed" {
        return Err(format!("claim {claim_id} 已经收尾（{}），不用继续", claim.state));
    }
    if claim.state == "reverted" || claim.state == "released" {
        return Err(format!(
            "claim {claim_id} 已被退回队列（{}），要接着做请用 tba take 重新接单",
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
// ---- 待回复的数字协议（用户 2026-10-26 定）----
//
// 用户在看板/评论里回复数字，tb 直接按数字动作，**不让 agent 解释再执行**：
//   0 = 问题已解决 → 「已解决」+ 列4，收尾
//   1 = 已解决，但要先把这次的经验写进项目记忆 → 列2，agent 写完再进待回复
//   2 = 问题未解决 → 列2，agent 自查后重新给选项
//   3+ = agent 在问题里自定义的选项 → 列2
//   非数字 = 按原文理解 → 列2
//
// **只有 0 收尾，其余一律回列2。**
//
// 为什么 tb 执行而不是 agent 执行：这是**流程**不是**判断**。让 agent 解释数字
// 再跑 done，中间有几十种出错方式（理解错、忘改列、先汇报再改导致状态错乱）。
// 而用户回复 0 就是明确授权，tb 只是执行用户的指令，不违反「收尾权在用户」。
#[derive(Clone, Debug, PartialEq)]
pub enum ReplyAction {
    // 0：收尾到「已解决」+ 列4
    Closed,
    // 1：已解决但先写项目记忆，写完再进待回复
    Summarize,
    // 2：未解决，自查后重新给选项
    Recheck,
    // 3+：agent 自定义的选项
    Custom(u32),
    // 非数字或有歧义，交给 agent 读原文
    Free,
}

impl ReplyAction {
    pub fn tag(&self) -> String {
        match self {
            ReplyAction::Closed => "closed".to_string(),
            ReplyAction::Summarize => "summarize".to_string(),
            ReplyAction::Recheck => "recheck".to_string(),
            ReplyAction::Custom(n) => format!("custom n={n}"),
            ReplyAction::Free => "free".to_string(),
        }
    }

    // 对应的数字。Free 没有数字（用户没回数字）。
    pub fn num(&self) -> Option<u32> {
        match self {
            ReplyAction::Closed => Some(0),
            ReplyAction::Summarize => Some(1),
            ReplyAction::Recheck => Some(2),
            ReplyAction::Custom(n) => Some(*n),
            ReplyAction::Free => None,
        }
    }
}

// 解析用户回复。返回 (动作, 去掉数字前缀后的正文)。
//
// 防歧义的规则（宁可当文本，也不猜错——猜错就是收错任务）：
//   * 首字符必须是 ASCII 数字，否则当文本（"好的，0" 不认）；
//   * 数字后面必须紧跟「结尾 / 非 ASCII 字母数字」（"0可以了" 认，"0abc" 不认）；
//   * 去掉前缀后正文里还有数字 → 当文本（"1 和 2 都做了" 有歧义，不猜）。
pub fn parse_reply(text: &str) -> (ReplyAction, String) {
    let t = text.trim();
    if t.is_empty() {
        return (ReplyAction::Free, String::new());
    }
    let first = match t.chars().next() {
        Some(c) => c,
        None => return (ReplyAction::Free, String::new()),
    };
    if !first.is_ascii_digit() {
        return (ReplyAction::Free, t.to_string());
    }
    // 连续数字的结束位置
    let mut end = 0usize;
    for (i, c) in t.char_indices() {
        if c.is_ascii_digit() {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    let num_str = &t[..end];
    let rest = &t[end..];
    // 数字后面紧跟 ASCII 字母数字 → 不是选项（如 "0abc" / "123x"）
    if let Some(c) = rest.chars().next() {
        if c.is_ascii_alphanumeric() {
            return (ReplyAction::Free, t.to_string());
        }
    }
    let n: u32 = match num_str.parse() {
        Ok(n) => n,
        Err(_) => return (ReplyAction::Free, t.to_string()),
    };
    let body = rest
        .trim_start_matches(|c: char| {
            c.is_whitespace() || ".,。，、:：;；)）]】".contains(c)
        })
        .trim();
    // 正文里再出现数字 → 有歧义，不猜
    if body.chars().any(|c| c.is_ascii_digit()) {
        return (ReplyAction::Free, t.to_string());
    }
    let act = match n {
        0 => ReplyAction::Closed,
        1 => ReplyAction::Summarize,
        2 => ReplyAction::Recheck,
        _ => ReplyAction::Custom(n),
    };
    (act, body.to_string())
}

pub enum WaitOutcome {
    // 读到回复。action=Closed 时任务已收尾；其余已推回「进行中 + 列2」
    Replied {
        claim: Claim,
        replies: Vec<CommentInfo>,
        action: ReplyAction,
        // 去掉数字前缀后的正文（有附言时给 agent 看）
        body: String,
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
    let mut claim = store
        .get_claim(claim_id)
        .ok_or_else(|| format!("找不到 claim: {claim_id}。用 tba status 查看全部"))?;
    if !claim.is_active() {
        // 已收尾/已回收：按「别处收尾」返回，agent 据此停止，不再报错。
        return Ok(WaitOutcome::Gone {
            state: claim.state.clone(),
            claim,
            waited_secs: 0,
        });
    }

    // 自愈：没 ask 过就跑 wait，说明 agent 忘了问。
    // 这时候用户既不知道要回什么，也没收到邮件。tb 替它补一次 ask：
    // 问题写进备注 + 状态转「待回复」+ 发邮件，然后按正常流程等回复。
    // 关掉 wait_auto_ask 就退回旧行为：只等外部收尾，不读评论。
    if claim.ask_at.is_empty() && store.load_config().wait_auto_ask {
        match ask(store, claim_id, DONE_QUESTION) {
            Ok(c) => {
                eprintln!(
                    "wait: 这个任务没 ask 过（agent 忘了问），已自动补一次：\
                     问题写进备注、状态转「{STAGE_WAIT}」、邮件已发。"
                );
                claim = c;
            }
            Err(e) => {
                eprintln!("wait: 自动补 ask 失败（{e}）。按「进行中」继续等外部收尾。");
            }
        }
    }

    // ask_at 为空 = 进行中态等用户手动收尾（改状态/拖列4-5），不读评论。
    // ask_at 非空 = 提问后等回复。
    let has_ask = !claim.ask_at.is_empty();

    let mut ctx = Ctx::new(store.clone(), false)?;
    let started = std::time::Instant::now();
    let interval = interval_secs.max(3);
    let timeout = timeout_secs.max(interval);
    let ask_at = claim.ask_at.clone();
    let mut next_report = 60u64;
    // 邮件轮询节流：IMAP 一次往返比读评论贵，别跟评论一个频率。
    // 下限 5 秒，防止配置填 0/1 把邮箱服务器打成拒绝服务。
    let mail_poll = ctx.tb.cfg.mail_poll_secs.max(crate::config::MIN_MAIL_POLL_SECS);
    let mut last_mail = std::time::Instant::now() - std::time::Duration::from_secs(mail_poll);

    loop {
        // 每轮刷新「我在等」的标记。进程被杀就停更，别的命令据此判断
        // 「任务在等，但没人接了」——agent 把 ask 丢后台就是这么被发现的。
        store.wait_touch(claim_id);

        // 等待期间任务可能被别处 done/close/release 掉，那就别再等了。
        if let Some(c) = store.get_claim(claim_id) {
            if !c.is_active() {
                store.wait_drop(claim_id);
                return Ok(WaitOutcome::Gone {
                    state: c.state.clone(),
                    claim: c,
                    waited_secs: started.elapsed().as_secs(),
                });
            }
        }

        // 用户在网页手动收尾（改状态为 已解决/已完成，或拖到列4/列5）→ 任务已完成，
        // 标记本地 claim 收尾并停止等待，别再傻等评论。
        if let Some(st) = external_done_state(&mut ctx, &claim.project_id, &claim.task_id) {
            let _ = store.update_claim(claim_id, st, "用户手动收尾（改状态/拖到列4-5）");
            let c = store.get_claim(claim_id).unwrap_or_else(|| claim.clone());
            store.wait_drop(claim_id);
            return Ok(WaitOutcome::Gone {
                state: st.to_string(),
                claim: c,
                waited_secs: started.elapsed().as_secs(),
            });
        }

        // 邮件回信：配了 IMAP 就顺便轮询信箱，把回信**写成任务评论**，
        // 后面完全复用「读评论 → resume」这条老路，不搞第二套状态机。
        if has_ask && crate::mail::can_receive(&ctx.tb.cfg) && last_mail.elapsed().as_secs() >= mail_poll
        {
            last_mail = std::time::Instant::now();
            let marker = format!("[TB {claim_id}]");
            match crate::mail::fetch_replies(&ctx.tb.cfg, &marker) {
                Ok(msgs) => {
                    for m in msgs {
                        let body = format!("（邮件回复）\n\n{m}");
                        if let Err(e) = ctx.post_comment(&claim.task_id, &body) {
                            eprintln!("邮件回信写入任务评论失败: {e}");
                        }
                    }
                }
                Err(e) => eprintln!("IMAP 收信失败（评论轮询照常）: {e}"),
            }
        }

        // 提问后等回复：只看「提问之后」的评论（ask_at 用服务端时间戳）。
        // 进行中态（未提问）只等外部收尾，不读评论。
        if has_ask {
            let fresh: Vec<CommentInfo> = ctx
                .read_comments(&claim.task_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|c| ts_after(&c.created, &ask_at))
                .collect();
            if !fresh.is_empty() {
                // 按**最后一条**回复判动作：用户可能先打错再改口，以最后一条为准。
                let last = fresh
                    .last()
                    .map(|c| c.text.clone())
                    .unwrap_or_default();
                let (action, body) = parse_reply(&last);
                // 复用 resume：推回 进行中 + 列2，并清掉 ask_at 防止下次读重。
                let (c, replies) = resume(store, claim_id)?;
                // 回复 0 = 用户明确授权收尾 → tb 直接执行。
                // （agent 自己跑 tb done 依然禁止，这里执行的是**用户**的指令。）
                if action == ReplyAction::Closed {
                    let c2 = finish(store, claim_id, STAGE_DONE)?;
                    store.wait_drop(claim_id);
                    return Ok(WaitOutcome::Replied {
                        claim: c2,
                        replies,
                        action,
                        body,
                        waited_secs: started.elapsed().as_secs(),
                    });
                }
                store.wait_drop(claim_id);
                return Ok(WaitOutcome::Replied {
                    claim: c,
                    replies,
                    action,
                    body,
                    waited_secs: started.elapsed().as_secs(),
                });
            }
        }

        // 边等边续命：running 的 claim 靠这里保活，避免被 sweep 误判会话断开。
        let _ = beat(store, claim_id);

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

// 退回队列：未完成 + 列1。agent 主动放弃，或会话断开时由 sweep 调用。
fn revert_to_queue(ctx: &mut Ctx, claim: &Claim, note: &str) -> Result<(), String> {
    let stages = ctx.ensure_stages(&claim.project_id)?;
    let todo = ctx
        .find_stage(&stages, STAGE_TODO)
        .ok_or("缺少「未完成」阶段")?
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
    // 写备注而不是评论：评论是用户的通道，agent 一个字都不往那儿写。
    if !note.is_empty() {
        let _ = ctx.append_task_note(&claim.task_id, note);
    }
    Ok(())
}

// 主动放弃：未完成 + 列1。
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

// 回收死会话：running 且心跳超时 -> 未完成 + 列1。
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
        // 用户可能已手动收尾（网页改状态为 已解决/已完成，或拖到列4/列5）：
        // 那就标记完成，绝不回收回列1。会话被迫结束才回列1，手动完成不算。
        if let Some(st) = external_done_state(&mut ctx, &c.project_id, &c.task_id) {
            let _ = store.update_claim(&c.claim_id, st, "用户手动收尾（改状态/拖到列4-5）");
            out.push(format!(
                "已确认外部收尾 {}（{}）：{st}，不回收",
                c.claim_id, c.task_name
            ));
            continue;
        }
        let secs = crate::store::age_secs(c.heartbeat());
        let ago = human_age(secs);
        let note = format!(
            "**⏱ 会话中断，任务已退回队列**\n\n{ago}没有心跳，判定执行会话已断开（限额 / 网络 / 窗口关闭）。\n任务状态改回「未完成」。看板列请手动拖回「{}」（tba 改不了列）。重新接单用 `tba take`。",
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

// ---- 项目记忆（sfc == "项目记忆" 的任务，它的备注就是记忆正文）----
//
// 为什么用任务备注：备注在 Teambition 云端，换机器、换 agent 都能读到，
// 人在网页上直接可看可改。代价是备注**全量覆盖写**，所以写之前必须先读回再拼。
//
// 唯一性策略（用户 2026-10-26 定）：Teambition 没有「每类型只有一个任务」的约束，
// task.create 也没有幂等键，所以**不追求「绝不重复」，只做三层兜底**：
//   ① 查的时候按 sfc 筛出全部候选，取「备注最长」的那个（内容最多 = 真正在用的）；
//   ② 建的时候用本地文件锁包住「查→建」，挡住同机并发；
//   ③ 发现多条不自动删，只告警列 id，让人手动归档。

// ---- 项目记忆：分片 + 富文本字段 ----
//
// 为什么分片：Teambition 的 RTF 自定义字段单值上限 **9999 字符**（实测写 12000
// 会被服务端截断到 9999），一份记忆装不下。所以按「节」拆成多个任务：
//   项目记忆 1 · 红线 / 2 · 决策 / 3 · 踩坑
// 读取时按序号拼接，写入时只碰对应那一片，天然避开并发覆盖。
//
// 为什么不用任务备注：`task.update` 的 `note` 参数实测会落到 `content`（标题）上，
// 备注在建完任务之后就改不动了。为什么不用 `customfield-rtf-update`：它把内容
// 截断到 100 字符。写入必须走 `customfield-update` + jsonValue。详见 tb记忆设计.md。

// ---- 项目记忆：一条记忆 = 一条状态为 mem 的普通任务，正文在备注 ----
//
// 为什么是「一条记忆一条任务」而不是固定几个分片：备注是内置字段、单条能装
// 5000+ 字，一条记忆本来就只该是几十到几百字的结论，用不着分片。而
// 「每加一条记忆就加一条任务」让写入变成纯 append（不读旧内容、不改旧任务），
// 两个 agent 同时写也不会互相覆盖。
// 代价是任务数会增长，所以读取时按节分组拼成 markdown，--status 负责告警条数。
#[derive(Clone)]
pub struct MemoryEntry {
    pub task_id: String,
    // 节名（红线/决策/踩坑），从标题解析
    pub section: String,
    // 备注正文
    pub text: String,
    pub created: String,
}

#[derive(Clone)]
pub struct ProjectMemory {
    pub entries: Vec<MemoryEntry>,
    // 按节分组的完整 markdown
    pub text: String,
}

// 记忆任务标题：`{节名} · {正文首行摘要}`
//
// 标题是节名的唯一来源（没有别的字段可挂），所以必须**以节名开头**。
// 摘要只是为了在网页看板上一眼看出这条是什么。
pub fn memory_title(section: &str, text: &str) -> String {
    let first = text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let brief: String = first.chars().take(MEMORY_TITLE_BRIEF).collect();
    let brief = brief.trim_end_matches(['.', '。', ',', '，', ';', '；', ':', '：']);
    if brief.is_empty() {
        section.to_string()
    } else {
        format!("{section}{MEMORY_TITLE_SEP}{brief}")
    }
}

// 从标题解析节名。认不出返回 None——宁可当成「不是记忆任务」，
// 也不要把用户的普通任务误当记忆读进来。
//
// 只认「标题就是节名」或「标题以 节名+分隔符 开头」，这样用户手动改摘要
// （分隔符后面那段）不会让识别失效。
pub fn parse_memory_title(title: &str) -> Option<String> {
    let t = title.trim();
    for sec in MEMORY_SECTIONS {
        if t == sec {
            return Some(sec.to_string());
        }
        if let Some(rest) = t.strip_prefix(sec) {
            if rest.starts_with(MEMORY_TITLE_SEP)
                || rest.starts_with('：')
                || rest.starts_with(':')
            {
                return Some(sec.to_string());
            }
        }
    }
    None
}

// 批量读任务的备注：**一批一次请求**，别一个任务一次请求。
//
// 必须走 `teambition.task.query` + fields（taskId 支持逗号分隔）：
// `project.task-query`（列表）不保证返回 note，拿它当正文会退化成标题。
pub(crate) fn read_task_notes(
    ctx: &mut Ctx,
    ids: &[String],
) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let tool = ctx.need("get_task", &ctx.map.get_task)?;
    for chunk in ids.chunks(30) {
        let res = ctx.tb.call(
            &tool,
            json!({
                "taskId": chunk.join(","),
                "fields": ["taskId", "content", "note"]
            }),
        )?;
        for item in extract_list(&res, 3) {
            let id = get_str(&item, &["taskId", "id", "_id"]).unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let note = get_str(&item, &["note", "description", "body", "remark", "detail"])
                .unwrap_or_default();
            out.push((id, note));
        }
    }
    Ok(out)
}

// 列出一个项目的全部记忆条目（状态 = mem 的任务），按创建时间排序。
//
// 只认「标题能解析出节名」的任务：状态 mem 下的其它任务（用户手建的）
// 不当记忆读，避免把无关内容混进记忆。
pub fn list_memory_entries(store: &Store, project_id: &str) -> Result<Vec<MemoryEntry>, String> {
    let mut ctx = Ctx::new(store.clone(), false)?;
    let state_name = store.load_tb_memory(project_id).state().to_string();
    let state = match ctx.find_state(project_id, &state_name)? {
        Some(s) => s,
        // 状态还没建 = 一条记忆都没有。不报错：--status 要能正常显示。
        None => return Ok(Vec::new()),
    };
    let tasks = ctx.list_tasks(project_id, &state)?;
    let mut cands: Vec<(String, String, String)> = Vec::new(); // (id, title, created)
    for t in tasks {
        if parse_memory_title(&t.title).is_none() {
            continue;
        }
        cands.push((t.id, t.title, t.created));
    }
    if cands.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = cands.iter().map(|(id, _, _)| id.clone()).collect();
    let notes = read_task_notes(&mut ctx, &ids)?;
    let mut out: Vec<MemoryEntry> = Vec::new();
    for (id, title, created) in cands {
        let section = match parse_memory_title(&title) {
            Some(s) => s,
            None => continue,
        };
        let text = notes
            .iter()
            .find(|(tid, _)| tid == &id)
            .map(|(_, n)| n.clone())
            .unwrap_or_default();
        out.push(MemoryEntry {
            task_id: id,
            section,
            text,
            created,
        });
    }
    // 按创建时间升序 = 记忆的写入顺序。created 是 ISO 字符串，字典序即时间序。
    out.sort_by(|a, b| a.created.cmp(&b.created));
    Ok(out)
}

// 确保记忆状态存在（幂等）。返回 `名字 (id)`。
//
// 状态由 tb 自动建（taskflow-update 的 tfsJson upsert），新项目开记忆不用去网页。
pub fn ensure_memory_state(store: &Store, project_id: &str) -> Result<String, String> {
    let mut ctx = Ctx::new(store.clone(), false)?;
    let name = store.load_tb_memory(project_id).state().to_string();
    let st = ctx.ensure_state(project_id, &name)?;
    Ok(format!("{} ({})", st.name, st.id))
}

// 读项目记忆。开关关着就报错，**不静默返回空**——
// 静默返回空会让 agent 以为「项目没有记忆」，比报错更危险。
pub fn read_project_memory(store: &Store, project_id: &str) -> Result<ProjectMemory, String> {
    let m = store.load_tb_memory(project_id);
    if !m.enabled {
        return Err(format!(
            "本项目未启用 tba 共享记忆。用 tba memory --on 打开（或 GUI 里的开关）。\n\
             开关文件: {}",
            store
                .project_dir(project_id)
                .join("tb-memory.toml")
                .display()
        ));
    }
    let entries = list_memory_entries(store, project_id)?;
    if entries.is_empty() {
        return Err(format!(
            "本项目还没有记忆。用 tba memory --append \"...\" --section 踩坑 加一条。\n\
             （记忆 = 状态为「{}」的普通任务，正文在备注里；tba memory --init 会把状态建出来）",
            m.state()
        ));
    }
    // 按固定节序输出：红线 → 决策 → 踩坑。每条一行，多行正文缩进续行，
    // 既省 token 又能让 agent 一眼扫完。
    let mut text = String::new();
    for sec in MEMORY_SECTIONS {
        let list: Vec<&MemoryEntry> = entries.iter().filter(|e| e.section == *sec).collect();
        if list.is_empty() {
            continue;
        }
        text.push_str(&format!("## {sec}\n"));
        for e in list {
            let body = e.text.trim();
            if body.is_empty() {
                continue;
            }
            let mut lines = body.lines();
            if let Some(first) = lines.next() {
                text.push_str(&format!("- {}\n", first.trim()));
                for l in lines {
                    if l.trim().is_empty() {
                        continue;
                    }
                    text.push_str(&format!("  {}\n", l.trim_end()));
                }
            }
        }
        text.push('\n');
    }
    Ok(ProjectMemory { entries, text })
}

// 节名校验 + 红线/决策的写权限检查（append 和 overwrite 共用）。
//
// 红线/决策是价值判断，只有用户能改：agent 写这两节必须带 force，
// 否则拒绝——让 agent 自己定规矩，等于没有规矩。
fn check_memory_section<'a>(section: &'a str, force: bool) -> Result<&'a str, String> {
    let sec = if section.is_empty() {
        MEMORY_SECTION_PITFALLS
    } else {
        section
    };
    if !MEMORY_SECTIONS.contains(&sec) {
        return Err(format!(
            "未知分节「{sec}」。可用分节: {}",
            MEMORY_SECTIONS.join(" / ")
        ));
    }
    let guarded = sec == MEMORY_SECTION_RULES || sec == MEMORY_SECTION_DECISIONS;
    if guarded && !force {
        return Err(format!(
            "「{sec}」是价值判断，只有用户能改。确实要写请加 --force。"
        ));
    }
    Ok(sec)
}

// 加一条记忆：**新建一条状态为 mem 的普通任务**，正文写进备注。
//
// 纯 append：不读旧内容、不改旧任务，所以两个 agent 同时写也不会互相覆盖。
// 这就是「每次加记忆都加一条任务」——写入永远不冲突。
pub fn append_memory(
    store: &Store,
    project_id: &str,
    section: &str,
    text: &str,
    force: bool,
) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("要写入的内容是空的".into());
    }
    let sec = check_memory_section(section, force)?;
    let m = store.load_tb_memory(project_id);
    if !m.enabled {
        return Err("本项目未启用 tba 共享记忆。先 tba memory --on".into());
    }
    // 超硬上限必须拒绝，不能静默截断——截断后 agent 以为写进去了，
    // 实际后半段丢了，这是最难查的一类 bug。
    let n = text.chars().count();
    if n > MEMORY_HARD_LIMIT_CHARS {
        return Err(format!(
            "这条记忆 {n} 字，超过单条上限 {MEMORY_HARD_LIMIT_CHARS}。\n\
             一条记忆该是结论不是长文——拆成几条分别 --append。"
        ));
    }
    let mut ctx = Ctx::new(store.clone(), false)?;
    let state = ctx.ensure_state(project_id, m.state())?;
    let title = memory_title(sec, text);
    let id = ctx.create_marked_task(project_id, &title, text, &state)?;
    // 数一下该节现在多少条，超上限就在返回值里告警（不阻断）。
    let count = list_memory_entries(store, project_id)?
        .iter()
        .filter(|e| e.section == sec)
        .count();
    let mut msg = format!(
        "已加一条「{sec}」记忆（任务 {}，{n} 字）。该节现在 {count} 条。",
        &id[..id.len().min(12)]
    );
    msg.push_str("\n判定标准：下一个 agent 不知道这条，会不会做错事？");
    if n > MEMORY_SOFT_LIMIT_CHARS {
        msg.push_str(&format!(
            "\n⚠️ 这条 {n} 字，超过软上限 {MEMORY_SOFT_LIMIT_CHARS}，该拆短了。"
        ));
    }
    if count > MEMORY_SECTION_LIMIT {
        msg.push_str(&format!(
            "\n⚠️「{sec}」已 {count} 条，超过上限 {MEMORY_SECTION_LIMIT}，该合并淘汰了。"
        ));
    }
    Ok(msg)
}

// 覆盖某一节：把这节的旧任务**归档**（进回收站、可恢复），再写一条新的。
//
// 为什么归档而不是删：Teambition 删任务不可逆，归档还能捞回来。
pub fn overwrite_memory(
    store: &Store,
    project_id: &str,
    section: &str,
    text: &str,
    force: bool,
) -> Result<String, String> {
    if !force {
        return Err("整节覆盖会把这一节的旧记忆全部归档，必须加 --force".into());
    }
    let sec = check_memory_section(section, true)?;
    let old: Vec<String> = list_memory_entries(store, project_id)?
        .into_iter()
        .filter(|e| e.section == sec)
        .map(|e| e.task_id)
        .collect();
    let mut archived = 0usize;
    if !old.is_empty() {
        let mut ctx = Ctx::new(store.clone(), false)?;
        if let Some(tool) = ctx.find_tool("teambition.task.archive") {
            for id in &old {
                ctx.tb.call(&tool, json!({ "taskId": id }))?;
                archived += 1;
            }
        } else {
            return Err(
                "MCP 工具集里找不到归档工具（task.archive），覆盖会留下旧条目，已中止".into(),
            );
        }
    }
    let msg = append_memory(store, project_id, sec, text, true)?;
    Ok(format!(
        "已覆盖「{sec}」：归档旧任务 {archived} 条，写入新的一条。\n{msg}"
    ))
}

// 项目记忆状态摘要（给 tb memory --status 和 GUI 用）。
pub fn project_memory_status(store: &Store, project_id: &str) -> Result<String, String> {
    let saved = store.load_tb_memory(project_id);
    let mut s = String::new();
    s.push_str(&format!(
        "开关: {}\n记忆状态: {}\n",
        if saved.enabled { "开" } else { "关" },
        saved.state()
    ));
    // 状态检查放在开关判断之前：开关关着也要能看出「状态还没建」，
    // 否则用户开了开关才发现跑不起来。
    let mut ctx = Ctx::new(store.clone(), false)?;
    let state = ctx.find_state(project_id, saved.state())?;
    match &state {
        Some(st) => s.push_str(&format!("状态 id: {}\n", &st.id[..st.id.len().min(12)])),
        None => {
            s.push_str("状态: ❌ 还没建（tba memory --init 会自动建）\n");
            return Ok(s);
        }
    }
    if !saved.enabled {
        s.push_str("（关着的时候 tba memory 不读云端记忆）\n");
        return Ok(s);
    }
    drop(ctx);
    let entries = list_memory_entries(store, project_id)?;
    if entries.is_empty() {
        s.push_str("记忆: 还没有。tba memory --append \"...\" 加一条。\n");
        return Ok(s);
    }
    s.push_str("分节:\n");
    let mut total = 0usize;
    for sec in MEMORY_SECTIONS {
        let list: Vec<&MemoryEntry> = entries.iter().filter(|e| e.section == *sec).collect();
        let chars: usize = list.iter().map(|e| e.text.chars().count()).sum();
        total += chars;
        let warn = if list.len() > MEMORY_SECTION_LIMIT {
            format!(" ⚠️ 超过 {} 条，该合并了", MEMORY_SECTION_LIMIT)
        } else {
            String::new()
        };
        s.push_str(&format!("  {sec}: {} 条 / {chars} 字{warn}\n", list.len()));
    }
    s.push_str(&format!(
        "合计 {} 条 / {total} 字（单条上限 {MEMORY_HARD_LIMIT_CHARS}）\n",
        entries.len()
    ));
    Ok(s)
}

// 把旧的本地记忆目录（data/projects/<id>/memory/）迁成「踩坑」记忆。
//
// 一个本地文件 = 一条记忆任务，正文前标一句来源。
// **不删本地**——迁移是单向的，本地留作备份。真要清理由用户自己动手。
pub fn import_local_memory(store: &Store, project_id: &str) -> Result<String, String> {
    let files = store.read_memory(project_id);
    if files.is_empty() {
        return Ok(format!(
            "本地记忆目录是空的，没什么可迁的。目录: {}",
            store.memory_dir(project_id).display()
        ));
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d");
    let mut added = 0usize;
    let mut chars = 0usize;
    for (name, content) in &files {
        let body = content.trim();
        if body.is_empty() {
            continue;
        }
        chars += body.chars().count();
        let text = format!("迁移自 {name}（{stamp}）：\n{body}");
        append_memory(store, project_id, MEMORY_SECTION_PITFALLS, &text, false)?;
        added += 1;
    }
    Ok(format!(
        "已把 {added} 个本地文件（{chars} 字）迁成 {added} 条「{MEMORY_SECTION_PITFALLS}」记忆。\n\
         本地文件保留未删。"
    ))
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
    // skill = 状态为 skill 的普通任务（不是任务类型、不限看板列）。
    let state = match ctx.find_state(project_id, SKILL_STATE)? {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    let tasks = ctx.list_tasks(project_id, &state)?;
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    // 正文在备注里，批量读一次（列表接口不保证带 note）。
    let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    let notes = read_task_notes(&mut ctx, &ids)?;
    let switches = store.load_switches();
    let sw = switches.get(project_id).cloned().unwrap_or_default();
    let mut out: Vec<SkillItem> = tasks
        .into_iter()
        .map(|t| {
            let on = sw.get(&t.title).cloned().unwrap_or(false);
            let content = notes
                .iter()
                .find(|(id, _)| id == &t.id)
                .map(|(_, n)| n.clone())
                .unwrap_or_default();
            SkillItem {
                task_id: t.id,
                name: t.title,
                content,
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
    let state = ctx.ensure_state(project_id, SKILL_STATE)?;
    // 同名 skill 已存在则更新：标题 = 名字，正文 = 备注（两次调用，各管一个字段）。
    let tasks = ctx.list_tasks(project_id, &state)?;
    if let Some(exist) = tasks.iter().find(|t| t.title == name) {
        ctx.update_task_content(&exist.id, name, content)?;
        return Ok(format!("已更新 skill: {name} ({})", exist.id));
    }
    // 新建：普通 Task + 状态 skill，正文写备注。
    // 不占「未完成」，所以 claim 永远领不到它（claim 只查未完成）。
    let id = ctx.create_marked_task(project_id, name, content, &state)?;
    Ok(format!("已创建 skill: {name} ({id})"))
}

// ---- 内置能力模板 ----
//
// 正文统一放在 caps 模块：那是**本地、跨项目**的通用能力库，GUI 里能增删改查，
// 换项目也还在。这里保留的只是「把某条能力推成项目 skill」的老入口——项目 skill
// 存在 Teambition 云端、按项目隔离，只在该项目确实要把某条流程钉在云端时才用。
pub const SKILL_SHOT: &str = crate::caps::BODY_SHOT;
pub const SKILL_GIT: &str = crate::caps::BODY_GIT;
pub const SKILL_RELEASE_TBA: &str = crate::caps::BODY_RELEASE;
pub const SKILL_GUI: &str = crate::caps::BODY_GUI;

// (名字, 一句话说明, 正文)。说明给 `tba skill-tpl` 用，agent 先看目录再决定推哪个。
pub const BUILTIN_SKILLS: &[(&str, &str, &str)] = &[
    ("shot", "截图：一条命令出 png，后台窗口也能截", SKILL_SHOT),
    ("git-main", "提交并推送（GitButler 项目自动走 but）", SKILL_GIT),
    (
        "release",
        "发版：生成 workflow / 打 tag / 等 CI / 列产物",
        SKILL_RELEASE_TBA,
    ),
    (
        "gui-notes",
        "写 GUI 的注意事项：黑体、字体可选、别出豆腐块",
        SKILL_GUI,
    ),
    // 老名字，保留给已经推过 release-push 的项目（同名覆盖成新版内容）
    ("release-push", "老名字，等同 release", SKILL_RELEASE_TBA),
];

// 把内置模板推成项目 skill。同名会覆盖旧正文（push_skill 的行为）。
pub fn init_builtin_skills(
    store: &Store,
    project_id: &str,
    only: Option<&str>,
) -> Result<String, String> {
    let mut out: Vec<String> = Vec::new();
    for (name, _, body) in BUILTIN_SKILLS {
        match only {
            Some(o) => {
                if o != *name {
                    continue;
                }
            }
            // 老名字不进默认批量推送：新项目里出现两条同内容的 skill 纯属噪音，
            // 只有点名要它（--name release-push）时才推。
            None => {
                if *name == "release-push" {
                    continue;
                }
            }
        }
        out.push(push_skill(store, project_id, name, body)?);
    }
    if out.is_empty() {
        return Err(format!(
            "没有这个内置 skill: {}（可选 {}）",
            only.unwrap_or(""),
            BUILTIN_SKILLS
                .iter()
                .map(|(n, _, _)| *n)
                .collect::<Vec<_>>()
                .join(" / ")
        ));
    }
    Ok(out.join("\n"))
}

// 删除 skill：归档那条任务。Teambition 没有硬删除，归档就是不显示。
pub fn archive_skill(store: &Store, project_id: &str, name: &str) -> Result<String, String> {
    let mut ctx = Ctx::new(store.clone(), false)?;
    let Some(state) = ctx.find_state(project_id, SKILL_STATE)? else {
        return Err("项目还没有 skill 状态".to_string());
    };
    let tasks = ctx.list_tasks(project_id, &state)?;
    let Some(t) = tasks.iter().find(|t| t.title == name) else {
        return Err(format!("没找到 skill: {name}"));
    };
    let tool = ctx
        .find_tool("teambition.task.archive")
        .ok_or("MCP 工具集里找不到归档工具（task.archive）")?;
    ctx.tb.call(&tool, json!({ "taskId": t.id }))?;
    Ok(format!("已删除 skill: {name}"))
}

// ---- 导出：把项目 skill / mem 拉成本地两个 md ----
//
// 目的：让人在本地直接看到「agent 读到的 skill / mem 到底长什么样」，
// 而不用去网页一条条点任务。每次调用都重新从 Teambition 拉一遍，
// 所以「下载」和「刷新」是同一个动作。
//
//   data/projects/<id>/skillmem/skill.md  所有 skill 正文合并（标出启用状态）
//   data/projects/<id>/skillmem/mem.md    按节分组的记忆（= tb memory 的输出）
pub struct SkillMemExport {
    pub dir: std::path::PathBuf,
    pub skill_file: std::path::PathBuf,
    pub mem_file: std::path::PathBuf,
    pub skill_count: usize,
    pub skill_on: usize,
    pub mem_count: usize,
    pub mem_chars: usize,
}

pub fn export_skill_mem(
    store: &Store,
    project_id: &str,
    project_name: &str,
) -> Result<SkillMemExport, String> {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let skills = list_skills(store, project_id)?;
    let skill_on = skills.iter().filter(|s| s.on).count();

    // 文件结构：**正文在前、说明在后**。
    // agent 其实不读这个文件——它跑 `tba skill <名字>` / `tba memory` 读云端，
    // stdout 就是这里的正文部分。把项目名/时间放头部会让人误以为 agent 也看到那些。
    let mut s = String::new();
    if skills.is_empty() {
        s.push_str("（本项目还没有 skill。用 `tba skill-push` 加一条。）\n\n");
    }
    for it in &skills {
        // 段头是 tb 加的边界行（agent 读单个 skill 时没有它），
        // 但没有它多个 skill 的正文会连成一片，只能留着。
        s.push_str(&format!(
            "## {}{}\n\n",
            it.name,
            if it.on { "" } else { "（未启用）" }
        ));
        let body = it.content.trim();
        if body.is_empty() {
            s.push_str("（正文为空）\n\n");
        } else {
            s.push_str(body);
            s.push_str("\n\n");
        }
    }
    s.push_str(&format!(
        "---\n\n\
         # 说明（agent 读不到这段）\n\n\
         - 项目: {project_name} `{project_id}`\n\
         - 拉取时间: {stamp}\n\
         - 共 {} 个 skill（启用 {} 个）\n\
         - agent 执行 `tba skill <名字> --project <id>`，stdout 就是该名字下面那段正文；\
         上面每段的 `## 名字` 是 tb 加的分隔行，agent 读不到\n\
         - 开工文案里只列**已启用**的名字，正文按需读\n",
        skills.len(),
        skill_on
    ));

    // 记忆：开关关着 / 一条没有时，read_project_memory 会报错并说清原因，
    // 这里把原因写进文件而不是让整次导出失败——「看不了 skill」也得能看。
    let (mem_count, mem_chars, mem_body) = match read_project_memory(store, project_id) {
        Ok(pm) => (pm.entries.len(), pm.text.chars().count(), pm.text),
        Err(e) => (
            0usize,
            0usize,
            format!("（没读到记忆：agent 跑 tba memory 会直接报错，原因如下）\n\n{e}\n"),
        ),
    };
    let mut m = mem_body.trim_end().to_string();
    m.push_str(&format!(
        "\n\n---\n\n\
         # 说明（agent 读不到这段）\n\n\
         - 项目: {project_name} `{project_id}`\n\
         - 拉取时间: {stamp}\n\
         - 共 {mem_count} 条 / {mem_chars} 字\n\
         - agent 执行 `tba memory --project <id>`，stdout 就是上面「---」之前的全部内容\
         （按节分组，一条一行，多行正文缩进续行）\n"
    ));

    let dir = store.skillmem_dir(project_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录 {} 失败: {e}", dir.display()))?;
    let skill_file = dir.join("skill.md");
    let mem_file = dir.join("mem.md");
    write_text_atomic(&skill_file, &s)?;
    write_text_atomic(&mem_file, &m)?;
    Ok(SkillMemExport {
        dir,
        skill_file,
        mem_file,
        skill_count: skills.len(),
        skill_on,
        mem_count,
        mem_chars,
    })
}

// 先写临时文件再 rename：导出文件可能正被编辑器/agent 读着，
// 直接覆盖会让对方读到写了一半的内容。
fn write_text_atomic(path: &std::path::Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("重命名到 {} 失败: {e}", path.display())
    })
}

// 在系统文件管理器里打开目录（不存在就先建）。
pub fn open_folder(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("建目录失败: {e}"))?;
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg(path);
        crate::proc::hide(&mut c);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("打开 {} 失败: {e}", path.display()))
}

// ---- agent 文案 ----

// 本次任务：用户在文案里直接派下来的活。
//
// 有它 -> agent 走 `tba start` 新建一条「进行中 + 列2」的任务开工；
// 没有 -> agent 走 `tba claim` 去列1 队列领现成的。
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
// 首行固定写成 `# <标题>`：这样 agent 用 `tba start --file <路径>` 时
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
// - ThisTask（复制agent文案(本次任务)）：带草稿，agent 用 `tba start` 开工；
// - Queue（复制agent文案(领列1任务)）：一律不带草稿，agent 只能去队列「未完成」领。
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

// 文案语言。
//
// **中文版是准本**：内容逐条对齐需求，改动先改中文。
// **英文版据中文压缩改写**：同义内容中文 ~880 token、英文 ~540（cl100k_base），
// 英文版只为省 token，不许比中文版多出规则、也不许少掉任何不变量。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Lang {
    #[default]
    Zh,
    En,
}

impl Lang {
    // CLI 用 --lang 收字符串，这里做一次宽松解析。
    pub fn parse(s: &str) -> Option<Lang> {
        match s.trim().to_ascii_lowercase().as_str() {
            "zh" | "cn" | "zh-cn" | "chinese" | "中文" => Some(Lang::Zh),
            "en" | "eng" | "english" | "英文" => Some(Lang::En),
            _ => None,
        }
    }

    // UI 按钮/状态提示里的短标签。
    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "英文",
        }
    }
}

// 中文版工作流程（准本）。
//
// 与英文版一一对应：中文是完整表达，英文并句压缩。
// 改规则先改这里，再同步英文版——单测会两种语言都跑一遍不变量。
//
// **写得短是硬指标**：文案每次开工都要整段贴给 agent，越长越贵。
// 所以 exit 码只讲一次、收尾规则不重复陈述、能省的括号解释全省。
fn workflow_text_zh(
    exe: &str,
    project_id: &str,
    cfg: &Config,
    draft: Option<&TaskDraft>,
    claimed: Option<&Claim>,
    memory_on: bool,
) -> String {
    let t = cfg.heartbeat_timeout_secs / 60;
    let has_task = draft.map(|d| !d.is_empty()).unwrap_or(false);
    let mut s = String::new();
    s.push_str("## 工作流程（必守；不许自己改任务状态或看板列）\n");

    // 已领分支（tb go 专用）：任务已经落账，agent 再 claim 会白跑一轮。
    if let Some(c) = claimed {
        s.push_str(&format!(
            "1. 任务已领：claim_id `{}`，任务「{}」（状态「{}」→「{}」）。别重复 claim。\n",
            c.claim_id, c.task_name, STAGE_TODO, STAGE_DOING
        ));
    } else if has_task {
        let d = draft.unwrap();
        if d.body_path.is_empty() {
            s.push_str(&format!(
                "1. 先登记（别去队列领）：`{exe} start --project {project_id} --title \"{}\" --note \"<上面的正文>\"`，",
                d.title.replace('"', "'")
            ));
        } else {
            s.push_str(&format!(
                "1. 先登记（别去队列领）：`{exe} start --project {project_id} --file \"{}\"`，",
                d.body_path
            ));
        }
        s.push_str(&format!(
            "它在列「{0}」新建状态「{1}」的任务并打印 claim_id；返回 \"already started, reusing claim\" 就复用，别自建任务。\n",
            cfg.tasklist_claim, STAGE_DOING
        ));
    } else {
        s.push_str(&format!(
            "1. 领任务：`{exe} claim --project {project_id}`（列「{0}」里最早那条）。用户给了链接用 `{exe} take <链接>`。没得领就告诉用户并停，不许自编任务。\n",
            cfg.tasklist_queue
        ));
    }

    // 第 2 步：开关开着才加「开工先读项目记忆」——关着时那句命令会直接报错。
    // 输出通道：agent 只写**备注**，不写评论。评论是用户的地盘。
    if memory_on {
        s.push_str(&format!(
            "2. 开工先读项目记忆：`{exe} memory --project {project_id}`。干活；过程和结论写进**任务备注**（`{exe} note <claim_id> --text \"...\"`，往后追加）。**不写评论**（那是用户的通道）。长操作前后各跑一次 `{exe} beat <claim_id>`。\n"
        ));
    } else {
        s.push_str(&format!(
            "2. 干活；过程和结论写进**任务备注**（`{exe} note <claim_id> --text \"...\"`，往后追加）。**不写评论**（那是用户的通道）。长操作前后各跑一次 `{exe} beat <claim_id>`。\n"
        ));
    }

    // 第 3 步：数字协议由 tb 执行（agent 不解释、不跑 done），只有 0 收尾。
    // ask 默认就地阻塞等回复，所以这里不再有单独的 wait 一步。
    s.push_str(&format!(
        "3. 要用户输入：`{exe} ask <claim_id> -q \"问题（写清数字含义）\"`（问题进备注，转「{0}」+列「{1}」，发邮件，**问完就地阻塞等回复，别结束会话**）。回复数字 tb 自动处理：0=已解决→列{3}收尾；1=先写项目记忆→列{2}，写完再ask；2=未解决→列{2}自查再给选项；3+=自定义→列{2}；非数字=按原文。只有0收尾，其余回列{2}。只等别重复问。选项用纯文本编号，禁用选择框/弹窗。\n",
        STAGE_WAIT, cfg.tasklist_wait, cfg.tasklist_claim, cfg.tasklist_done
    ));

    // 第 4 步：做完必须交回用户。ask 自带等待，所以是两步不是三步。
    // 「禁止 run_in_background」要写死：agent 把 ask 丢后台 = 会话结束了但没人接回复，
    // 任务卡在「待回复」，用户永远等不到下一步。
    if memory_on {
        s.push_str(&format!(
            "4. 做完必须交回用户，两步不许省：①`{exe} note <claim_id> --text \"改了什么/怎么验\"`；②`{exe} ask <claim_id> --done`（0=收尾/1=先写记忆/2=还有问题；转「{0}」+列「{1}」+发邮件，**前台阻塞等回复，禁止 run_in_background，绝不结束会话**）。ask exit 0=回复 / exit 2=超时，改用 `{exe} wait <claim_id>`（别重跑 ask，会重发信）/ exit 3=用户已收尾（「{2}」「{3}」或列「{4}」「{5}」），停。**不许停在进行中**。有坑就 `{exe} memory --project {project_id} --append \"...\"`。不许自己跑 `{exe} done`/`{exe} close` —— 只有用户能收尾（明确要求才 `{exe} done <claim_id>` / `{exe} close <claim_id>`）。放弃用 `{exe} release <claim_id>`。无心跳 {t} 分钟 → 自动退回「{6}」+列「{7}」；「{0}」永不超时。多会话先 `{exe} hello --name <名字>`。\n",
            STAGE_WAIT, cfg.tasklist_wait, STAGE_DONE, STAGE_CLOSED, cfg.tasklist_done, cfg.tasklist_closed, STAGE_TODO, cfg.tasklist_queue
        ));
    } else {
        s.push_str(&format!(
            "4. 做完必须交回用户，两步不许省：①`{exe} note <claim_id> --text \"改了什么/怎么验\"`；②`{exe} ask <claim_id> --done`（0=收尾/1=先写记忆/2=还有问题；转「{0}」+列「{1}」+发邮件，**前台阻塞等回复，禁止 run_in_background，绝不结束会话**）。ask exit 0=回复 / exit 2=超时，改用 `{exe} wait <claim_id>`（别重跑 ask，会重发信）/ exit 3=用户已收尾（「{2}」「{3}」或列「{4}」「{5}」），停。**不许停在进行中**。不许自己跑 `{exe} done`/`{exe} close` —— 只有用户能收尾（明确要求才 `{exe} done <claim_id>` / `{exe} close <claim_id>`）。放弃用 `{exe} release <claim_id>`。无心跳 {t} 分钟 → 自动退回「{6}」+列「{7}」；「{0}」永不超时。多会话先 `{exe} hello --name <名字>`。\n",
            STAGE_WAIT, cfg.tasklist_wait, STAGE_DONE, STAGE_CLOSED, cfg.tasklist_done, cfg.tasklist_closed, STAGE_TODO, cfg.tasklist_queue
        ));
    }
    s
}

// 给 agent 的工作方式说明。两个 prompt 函数共用，避免文案分叉。
//
// draft 有值 = 用户在文案里派了活（走 start 新建）；
// draft 为空 = 让 agent 去队列领（走 claim）。
//
// **中文准本在 workflow_text_zh**；这里只剩英文版（省 token 用）。
// 英文版据中文压缩，规则一条不多一条不少——单测两种语言都跑。
// 阶段名（未完成/进行中/...）两种语言都保持中文——那是 Teambition 上的真实名字，改了 agent 对不上。
// 关键不变量一条不许丢（有单测守着）：只经 tb 改状态/列、别自建任务、
// 方案1 = agent 永不自动收尾（done/close 只能用户显式触发）、做完停在进行中边等边 beat、
// 等用户手动收尾；长操作前后 beat、待回复永不超时、心跳超时退回队列、退出码 0/2/3。
fn workflow_text(
    exe: &str,
    project_id: &str,
    cfg: &Config,
    draft: Option<&TaskDraft>,
    claimed: Option<&Claim>,
    lang: Lang,
    memory_on: bool,
) -> String {
    if lang == Lang::Zh {
        return workflow_text_zh(exe, project_id, cfg, draft, claimed, memory_on);
    }
    let t = cfg.heartbeat_timeout_secs / 60;
    let mut s = String::new();
    let has_task = draft.map(|d| !d.is_empty()).unwrap_or(false);
    s.push_str("## Workflow (mandatory; never edit task state or board columns yourself)\n");

    // 已领分支（tb go 专用）：任务已经落账，别再 claim 一次。
    if let Some(c) = claimed {
        s.push_str(&format!(
            "1. Already claimed: claim_id `{}`, task \"{}\" (status {} -> {}). Do NOT claim again.\n",
            c.claim_id, c.task_name, STAGE_TODO, STAGE_DOING
        ));
    } else if has_task {
        let d = draft.unwrap();
        if d.body_path.is_empty() {
            s.push_str(&format!(
                "1. Register first (do NOT claim from the queue): `{exe} start --project {project_id} --title \"{}\" --note \"<body above>\"`, ",
                d.title.replace('"', "'")
            ));
        } else {
            s.push_str(&format!(
                "1. Register first (do NOT claim from the queue): `{exe} start --project {project_id} --file \"{}\"`, ",
                d.body_path
            ));
        }
        s.push_str(&format!(
            "it creates a {0} task in column \"{1}\" and prints claim_id; on \"already started, reusing claim\" reuse it, never create extra tasks.\n",
            STAGE_DOING, cfg.tasklist_claim
        ));
    } else {
        s.push_str(&format!(
            "1. Claim: `{exe} claim --project {project_id}` (oldest task in status {0} + column \"{1}\"). Link given -> `{exe} take <link>`. Nothing to claim -> tell the user and stop; never invent tasks.\n",
            STAGE_TODO, cfg.tasklist_queue
        ));
    }

    if memory_on {
        s.push_str(&format!(
            "2. Read memory first: `{exe} memory --project {project_id}`. Work; findings -> **task note** (`{exe} note <claim_id> --text \"...\"`). NEVER post comments - that's the user's channel. `{exe} beat <claim_id>` before/after long ops.\n"
        ));
    } else {
        s.push_str(&format!(
            "2. Work; findings -> **task note** (`{exe} note <claim_id> --text \"...\"`). NEVER post comments - that's the user's channel. `{exe} beat <claim_id>` before/after long ops.\n"
        ));
    }

    s.push_str(&format!(
        "3. Need user input: `{exe} ask <claim_id> -q \"question (state each number's meaning)\"` (-> {0} + column \"{1}\", mail sent; BLOCKS for the reply in place). tb handles numbers: 0=solved -> col {3}, closed; 1=solved but write project memory first -> col {2}, then ask again; 2=not solved -> col {2}, recheck, offer options; 3+=your own -> col {2}; non-number=read as text. ONLY 0 closes; others return to col {2}. Plain numbered options; NEVER use a picker or modal dialog.\n",
        STAGE_WAIT, cfg.tasklist_wait, cfg.tasklist_claim, cfg.tasklist_done
    ));

    let handoff = format!(
        "Hand back in 2 steps: (a) `{exe} note <claim_id> --text \"what changed / how to verify\"`; (b) `{exe} ask <claim_id> --done` (0=close/1=memory then close/2=still broken; -> {0} + column \"{1}\", mail sent; BLOCKS in foreground - NEVER run_in_background, never end session). NEVER stop in {2} - the user's reply would go unread. ",
        STAGE_WAIT, cfg.tasklist_wait, STAGE_DOING
    );

    let close_rule = format!(
        "Do NOT run `{exe} done`/`{exe} close` - only the user closes. exit 0=reply / exit 2=timeout, rerun with `{exe} wait <claim_id>` (not ask - resends mail), don't end the session / exit 3=user already closed it ({0}/{1} or column \"{2}\"/\"{3}\"), stop. Explicit order: `{exe} done <claim_id>` / `{exe} close <claim_id>`. Give up: `{exe} release <claim_id>`. No heartbeat {t} min -> auto-return to {4} + column \"{5}\"; {6} never times out. Multi-session: `{exe} hello --name <name>`.\n",
        STAGE_DONE, STAGE_CLOSED, cfg.tasklist_done, cfg.tasklist_closed, STAGE_TODO, cfg.tasklist_queue, STAGE_WAIT
    );
    if memory_on {
        s.push_str(&format!(
            "4. {handoff}Ask yourself: any pitfall the next agent would hit? If yes: `{exe} memory --project {project_id} --append \"...\"`. {close_rule}"
        ));
    } else {
        s.push_str(&format!("4. {handoff}{close_rule}"));
    }
    s
}

// 文案里「本次任务」那一段。放在 header 之后、skill 之前——
// agent 第一眼就该知道这次要干什么，而不是先读一堆 skill 再去找任务。
// 结构标签用英文（省 token）；标题/正文是用户自己的内容，原样保留。
fn task_section(d: &TaskDraft, lang: Lang) -> String {
    let zh = lang == Lang::Zh;
    let mut s = String::new();
    s.push_str(if zh {
        "## 本次任务（用户派的活 —— 不要去队列领）\n"
    } else {
        "## This task (assigned by the user - do NOT claim from the queue)\n"
    });
    s.push_str(&format!(
        "{}: {}\n",
        if zh { "标题" } else { "Title" },
        d.title.trim()
    ));
    if !d.body_path.is_empty() {
        s.push_str(&format!(
            "{}: {}\n",
            if zh { "正文文件" } else { "Body file" },
            d.body_path
        ));
    }
    let body = d.body.trim();
    if !body.is_empty() {
        s.push_str(if zh { "\n正文:\n" } else { "\nBody:\n" });
        if body.chars().count() > 4000 {
            let head: String = body.chars().take(4000).collect();
            s.push_str(&head);
            s.push_str(if zh {
                "\n\n...(正文过长已截断，完整内容见上面的正文文件)\n"
            } else {
                "\n\n...(body truncated, full text in the body file above)\n"
            });
        } else {
            s.push_str(body);
            s.push('\n');
        }
    }
    s.push('\n');
    s
}

// 文案正文。CLI 和 UI 两条路共用，避免两份文案走偏。
//
// **skill 不内联全文**：那曾是文案 token 的最大头（skill 越多越大越贵，
// 且每次发文案都要全量重发）。改成只列名字 + 一条按需读取命令，
// agent 用到哪个才 `tba skill <名字>` 读哪个，读哪个付哪个。
fn prompt_body(
    store: &Store,
    project_id: &str,
    project_name: &str,
    skills: &[SkillItem],
    draft: Option<&TaskDraft>,
    claimed: Option<&Claim>,
    lang: Lang,
) -> String {
    let zh = lang == Lang::Zh;
    let cfg = store.load_config();
    // 文案里路径要出现十几次，所以允许配一个短路径（如目录联接 D:\tb\tba.exe）。
    // 没配就用真实 exe 路径，行为跟以前一致。
    let exe = if cfg.tool_path.trim().is_empty() {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "tba".to_string())
    } else {
        cfg.tool_path.trim().to_string()
    };
    let mem = store.memory_dir(project_id);
    // 共享记忆开关：开着就把「本地目录」换成「读云端记忆」的命令。
    // 开着还显示本地目录会误导 agent 去读一份已经不用的旧文件。
    let memory_on = store.load_tb_memory(project_id).enabled;
    let mut s = String::new();
    s.push_str(&format!(
        "# {}: {project_name}\n",
        if zh { "项目" } else { "Project" }
    ));
    s.push_str(&format!(
        "{}: {project_id}\n",
        if zh { "项目 ID" } else { "Project ID" }
    ));
    if memory_on {
        s.push_str(&format!(
            "{}: `{exe} memory --project {project_id}`\n",
            if zh {
                "项目记忆（云端，存状态为 mem 的任务备注）"
            } else {
                "Project memory (cloud, note of mem-state tasks)"
            }
        ));
    } else {
        s.push_str(&format!(
            "{}: {}\n",
            if zh { "共享记忆目录" } else { "Shared memory dir" },
            mem.display()
        ));
    }
    s.push_str(&format!(
        "{}: {exe}\n\n",
        if zh { "工具路径" } else { "Tool path" }
    ));

    if let Some(d) = draft {
        if !d.is_empty() {
            s.push_str(&task_section(d, lang));
        }
    }

    let enabled: Vec<&SkillItem> = skills.iter().filter(|k| k.on).collect();
    if enabled.is_empty() {
        s.push_str(if zh {
            "已启用 skill: 无\n\n"
        } else {
            "Enabled skills: none\n\n"
        });
    } else {
        let names: Vec<&str> = enabled.iter().map(|k| k.name.as_str()).collect();
        s.push_str(&format!(
            "{} ({}): {}\n",
            if zh { "已启用 skill" } else { "Enabled skills" },
            enabled.len(),
            names.join(", ")
        ));
        s.push_str(&format!(
            "{} `{exe} skill <name> --project {project_id}`\n\n",
            if zh {
                "按需读一个（不要全读）："
            } else {
                "Read one on demand (do NOT read all):"
            }
        ));
    }
    // 通用能力：本地、跨项目，和项目 skill 是两套东西（skill 存在云端、按项目隔离）。
    // 压成一行：只列名字，正文不内联——agent 用到哪个才 `cap <名字>` 读哪个。
    // 项目级开关：这个项目取消勾选的能力不进文案（agent 搜不到就不会用）。
    let caps = crate::caps::enabled_for(store, project_id);
    if !caps.is_empty() {
        let names: Vec<&str> = caps.iter().map(|c| c.name.as_str()).collect();
        s.push_str(&format!(
            "{}{}（`{exe} cap <name>`）\n\n",
            if zh { "通用能力: " } else { "Capabilities: " },
            names.join(", ")
        ));
    }
    // 编译 & 重载：参数存 Teambition 的 setting 任务，每个项目一份。
    // 编译指令空着就明说「去探测再写回来」——否则 agent 会拿默认 cargo 硬编，
    // 遇到 zig / go 项目就白跑一轮。
    if let Some(st) = crate::setting::ProjectSetting::load_cached(store, project_id) {
        if st.on {
            if st.compile.trim().is_empty() {
                s.push_str(&format!(
                    "{}: {} `{exe} setting guess` → `{exe} setting set --compile \"...\"`（{}）\n\n",
                    if zh { "编译 & 重载" } else { "Build & reload" },
                    if zh { "编译指令未填，先探测项目类型：" } else { "compile not set, detect first:" },
                    if zh {
                        "存到 Teambition 的 setting 任务，再 `reload build`"
                    } else {
                        "saved to the setting task, then `reload build`"
                    }
                ));
            } else {
                s.push_str(&format!(
                    "{}: `{exe} reload build`（{}: {}）\n\n",
                    if zh { "编译 & 重载" } else { "Build & reload" },
                    if zh { "编译指令" } else { "compile" },
                    st.compile
                ));
            }
        }
    }
    s.push_str(&workflow_text(
        &exe, project_id, &cfg, draft, claimed, lang, memory_on,
    ));
    s
}

// 「复制项目初始化文案」的内容：给一个**还没有** Teambition 项目的仓库用。
//
// 和开工文案是两件事：这份只管「项目存不存在 / 建没建 / 阶段齐不齐」，
// 不涉及任务队列，所以不带任务、不带 skill、不带记忆——那要等项目建好之后才有。
//
// 项目名默认不写死（传 None）：让 agent 用它所在目录名当项目名，
// 因为只有 agent 知道它跑在哪个仓库。GUI 顶栏按钮就是这个用法——
// 那会儿本地可能一个项目都没有，谈不上「选哪个项目」。
pub fn init_prompt(store: &Store, project_name: Option<&str>, lang: Lang) -> String {
    let cfg = store.load_config();
    // 跟开工文案同一套路径策略：配了短命令就用短的（现在默认裸 `tba`）。
    let exe = if cfg.tool_path.trim().is_empty() {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "tba".to_string())
    } else {
        cfg.tool_path.trim().to_string()
    };
    // 有名字就固定用它；没有就写裸命令，并交代「默认取目录名、不对就 --name」。
    let given = project_name.map(str::trim).filter(|s| !s.is_empty());
    let zh = lang == Lang::Zh;
    let mut s = String::new();
    if zh {
        s.push_str("# TBHub 项目接入\n这个仓库还没接 Teambition。先判断项目建没建，没有就建（幂等，有就复用）：\n\n");
        match given {
            Some(n) => s.push_str(&format!("1. 跑 `{exe} ensure-project --name \"{n}\"`\n")),
            None => {
                s.push_str(&format!("1. 在仓库根目录跑 `{exe} ensure-project`\n"));
                s.push_str(
                    "   不带 --name 时项目名取当前目录名；目录名不是想要的就加 `--name \"<名字>\"`。\n",
                );
            }
        }
        s.push_str(
            "   有同名项目就复用，没有就新建；两种都会补齐 7 个工作流状态\
             （未完成/进行中/待回复/已解决/已完成 + skill/mem）。\n",
        );
        s.push_str("   成功最后一行是 `项目就绪: <id>  <名字>（新建/复用）`。\n");
        s.push_str(&format!(
            "2. 用那个 id 继续：`{exe} prompt --project <id>`，照输出开工。\n\n"
        ));
        s.push_str(&format!(
            "之后在本目录直接跑 `{exe} prompt`（不带 --project）即可，id 已写进 .tba/project。\n\
             出错就把 stdout 原样报给用户：不要改 data/config.toml，不要自己拼 Teambition 接口。\n"
        ));
    } else {
        s.push_str(
            "# TBHub project onboarding\nThis repo is not linked to Teambition yet. Check whether the project exists; create it if not (idempotent):\n\n",
        );
        match given {
            Some(n) => s.push_str(&format!("1. Run `{exe} ensure-project --name \"{n}\"`\n")),
            None => {
                s.push_str(&format!("1. In the repo root run `{exe} ensure-project`\n"));
                s.push_str(
                    "   Without --name the project name is the current directory name; add `--name \"<name>\"` if that is not what you want.\n",
                );
            }
        }
        s.push_str(
            "   Reuses the project with that name, creates it otherwise; either way it fills in the \
             7 workflow states (未完成/进行中/待回复/已解决/已完成 + skill/mem).\n",
        );
        s.push_str("   Last line is `项目就绪: <id>  <name> (new/reused)`.\n");
        s.push_str(&format!(
            "2. Continue with that id: `{exe} prompt --project <id>` and follow the output.\n\n"
        ));
        s.push_str(&format!(
            "Afterwards just run `{exe} prompt` in this dir (no --project); the id is in .tba/project.\n\
             On error report stdout as-is: do not edit data/config.toml, do not call Teambition APIs by hand.\n"
        ));
    }
    s
}

// ---- tb go：一句话开工 ----
//
// 用户只发一句话（`tba g`），剩下的判定全在这里做，用**退出码**交给 agent 分支：
//   3 = 当前目录没绑定项目 -> 停，不领任务，不建项目
//   4 = 绑定指向的项目在 Teambition 里查不到 -> agent 问用户要不要初始化
//   0 + EMPTY      = 项目在，队列空 -> 停
//   0 + claim_id   = 已领到任务，直接干
//
// 判定顺序是刻意的：本地绑定 -> 线上项目 -> 才领任务。
// 少一步就可能「在错的目录里把别人的任务领走」。
pub enum GoOutcome {
    /// 当前目录（含父目录）没有 .tba/project
    NoBinding { cwd: String },
    /// 找不到可用项目。id 非空 = 绑定失效（多半被删了）；id 空 = 按目录名没匹配到。
    /// name 是建议的初始化名字（取当前目录名）。
    NoProject { id: String, name: String },
    /// 项目在，但「未完成」队列空
    Empty { project_name: String },
    /// 领到了。text 里已经带 claim_id，不再单独回传 Claim。
    Claimed {
        text: String,
        entry: Option<std::path::PathBuf>,
    },
    /// 手上有「在等回复但没人接」的任务，先处理掉再来领新的。
    Blocked { msg: String },
}

// 「任务在等回复，但没人接了」——agent 把 ask 丢进后台、会话结束被清理，
// 或者进程被限额/网络掐掉，都会留下这个状态。
//
// 判定：状态是 waiting，但等待心跳停了。
//   - 有心跳文件：超过 ORPHAN_WAIT_SECS 没刷新
//   - 没心跳文件：旧版 ask 写的 claim（那时还不写心跳），用 ask_at 给双倍宽限
//
// 为什么不一并回收：任务状态本身是对的（确实在等你回复）。回收会把它打回队列，
// 等于把你还没回答的问题扔掉。所以只标记 + 拦住新任务，让人决定怎么处理。
pub const ORPHAN_WAIT_SECS: i64 = 60;

pub fn orphan_waits(store: &Store) -> Vec<Claim> {
    store
        .active_claims()
        .into_iter()
        .filter(|c| c.state == "waiting")
        .filter(|c| match store.wait_age(&c.claim_id) {
            Some(a) => a > ORPHAN_WAIT_SECS,
            None => crate::store::age_secs(&c.ask_at) > ORPHAN_WAIT_SECS * 2,
        })
        .collect()
}

// 给 agent 看的阻断文案：说清是什么、怎么办。
pub fn orphan_block_msg(claims: &[Claim]) -> String {
    let mut s = String::from(
        "有任务在等你回复，但接手的会话已经断了（ask 被丢后台 / 进程被清理）：\n",
    );
    for c in claims {
        let ago = human_age(crate::store::age_secs(&c.ask_at));
        s.push_str(&format!("  • {} [{}] 已等 {}\n", c.task_name, c.claim_id, ago));
    }
    s.push_str(
        "\n先处理掉再领新任务：\n  \
         接回来继续等  tba wait <claim_id>\n  \
         退回队列      tba release <claim_id> --reason \"会话断了\"\n",
    );
    s
}

// 工作空间入口文件的标记。有这个标记就认为已经写过，不再重复追加。
const ENTRY_MARK: &str = "<!-- tba:entry";
// 编码纪律那一节的标记。单独一个标记是为了让老项目的 AGENTS.md
// 在下次 tba g 时能补上这一节，而不是被「已有入口」挡住。
const RULES_MARK: &str = "<!-- tba:rules";

// 当前目录是不是 git 仓库（向上找 .git；worktree 里 .git 是文件，用 exists 一并覆盖）。
// 用来区分「项目目录」和「随手打开的目录」——后者不该去猜 Teambition 项目。
fn is_git_repo() -> bool {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return true;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    false
}

// 入口文件里该写哪条命令：配了短路径就用短的，否则用裸 `tba`。
fn entry_cmd(store: &Store) -> String {
    let cfg = store.load_config();
    let t = cfg.tool_path.trim();
    if t.is_empty() {
        "tba".to_string()
    } else {
        t.to_string()
    }
}

// 真实可执行路径。config 里的 tool_path 往往只是 `tba`，而 agent 的 shell
// 不一定继承了 PATH——没有路径它就会全盘搜 exe，实测白烧一分多钟。
// 所以凡是给 agent 看的文案，都写死绝对路径，不指望 PATH。
fn exe_path_or(cmd: &str) -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| cmd.to_string())
}

// 在当前目录补一个 AGENTS.md 入口。两个块各自检测标记，缺哪个补哪个——
// 这样已经生成过入口的老项目，下次 tba g 也能自动补上后加的编码纪律。
// 失败不报错：入口文件只是省几个字，没有它 `tba g` 一样能跑。
fn ensure_entry_file(store: &Store) -> Option<std::path::PathBuf> {
    let dir = std::env::current_dir().ok()?;
    ensure_entry_at(&dir, &entry_cmd(store))
}

// 实际干活的那一半，单独抽出来是为了能测（不碰全局 cwd）。
// 删掉一个 tba 块（从标记行到下一个 `## ` 标题之前）。
// 找不到标记就原样返回。找不到下一个标题就删到文件尾——那说明块在最后。
fn strip_tba_block(s: &str, mark: &str) -> String {
    let Some(start) = s.find(mark) else {
        return s.to_string();
    };
    let after = start + mark.len();
    // 先跳过本块的标题行，再从它之后找下一个 `## ` 作边界。
    let body = s[after..]
        .find("\n## ")
        .map(|i| after + i + 1)
        .unwrap_or(after);
    let end = s[body..].find("\n## ").map(|i| body + i).unwrap_or(s.len());
    format!("{}{}", &s[..start], &s[end..])
}

fn ensure_entry_at(dir: &std::path::Path, cmd: &str) -> Option<std::path::PathBuf> {
    let path = dir.join("AGENTS.md");

    // 真实可执行路径。config 里的 tool_path 往往只是 `tba`，而 agent 的 shell
    // 不一定继承了 PATH——没有路径它就会全盘搜 exe，实测白烧一分多钟。
    // 所以入口文件里写死绝对路径，不指望 PATH。
    let exe = exe_path_or(cmd);

    let entry = format!(
        "{ENTRY_MARK} 由 tba 自动生成，删掉不影响功能 -->\n\
         ## 任务队列入口\n\n\
         **工具**：`{exe}`（项目根的 `tba.cmd` 等价）。别搜 exe，别读 `.tba/`。\n\n\
         **开工**：用户说「开工」→ 跑 `{exe} g`。退出码：\
         0+claim_id=干活；0+EMPTY=队列空；3=没绑定项目；4=项目查不到，问用户。\
         别自建任务。\n\n\
         **流转**：进行中·列2 → `ask` 后 待回复·列3 → 用户收尾 已解决·列4。\n\n\
         **交回**：做完两步，不许省：\n\
         ① `{exe} note <claim_id> --text \"改了什么/怎么验\"`\n\
         ② `{exe} ask <claim_id> --done`\n\n\
         `ask` 发邮件后**就地阻塞**等你回数字（0=收尾 1=写记忆 2=有问题）。\
         退出码：0=收到回复；2=超时，改用 `{exe} wait <claim_id>`（别重跑 ask，会重发信）；\
         3=已收尾。\n\
         **禁止 run_in_background，绝不结束会话**——断了没人接你回复。\
         只有用户能 `done`/`close`，放弃用 `{exe} release`。\n\n\
         **通用能力**（别自己拼脚本）：`doctor` 环境自检 / `git main -m` 提交推送 / \
         `publish` 发版 / `shot` 截图。先 `{exe} --help`。\n\
         **编译 & 重载**：`{exe} setting` 看本项目的编译指令和 exe 名（存 Teambition 的 \
         setting 任务），再 `{exe} reload build`。\n"
    );

    // 四条编码纪律。来自 Karpathy 对 LLM 编码陷阱的总结，专治三种毛病：
    // 默默做错误假设、把 100 行堆成 1000 行、顺手改与任务无关的代码。
    //
    // 为什么放 AGENTS.md 而不是塞进 prompt：这四条是**通用编码习惯**，跟 tba 的
    // 任务流程无关，而且每条都要解释才管用。塞进 prompt 每次会话固定多 1500 字，
    // 而 AGENTS.md 是 agent 进项目就会读的约定文件，成本几乎为零。
    let rules = format!(
        "{RULES_MARK} 由 tba 自动生成，删掉不影响功能 -->\n\
         ## 编码纪律\n\n\
         四条，动手前过一遍。\n\n\
         1. **先想再写。** 不假设、不藏困惑。不确定就问，别猜着往下做；有歧义就摆出几种解释；\
         有更简单的路就直说；想不明白就停下问。\n\
         2. **最简实现。** 不加没要的功能、不做一次性抽象、不留「以后可能用得上」的开关、\
         不为不可能发生的场景写错误处理。200 行能写成 50 行就重写。\
         检验：资深工程师会觉得这太复杂吗？\n\
         3. **只改必须改的。** 不顺手「改进」旁边的代码/注释/格式，不重构没坏的东西，跟随现有风格。\
         发现无关的死代码只说一句，别删。检验：每行改动都能直接追溯到用户的要求。\n\
         4. **先定验证标准。** 把「加个校验」变成「为非法输入写测试再让它过」；\
         把「修 bug」变成「写个复现的测试再让它过」。多步任务先列「步骤 → 怎么验证」。\n\n\
         例外：错别字、一行小改这类琐碎活，别上完整流程。\
         这四条是为了减少非琐碎工作的代价，不是拖慢简单任务。\n"
    );

    let mut old = std::fs::read_to_string(&path).unwrap_or_default();

    // 已经是新版就不用动：入口段只有一份、带绝对路径、带「交回」「流转」「通用能力」。
    // 这三句当版本号用——老入口缺哪句，就整段替换成新版（不会重复追加）。
    let entry_ok = old.matches(ENTRY_MARK).count() == 1
        && old.contains(&exe)
        && old.contains("**交回**")
        && old.contains("**流转**")
        && old.contains("**通用能力**");
    let rules_ok = old.matches(RULES_MARK).count() == 1;

    // 转发脚本：PATH 没生效时 agent 也能直接跑 `tba.cmd g`，不用找 exe。
    // 已存在就不动——用户可能自己改过指向。
    let shim = dir.join("tba.cmd");
    if !shim.exists() {
        let _ = std::fs::write(&shim, format!("@echo off\r\n\"{exe}\" %*\r\n"));
    }

    if entry_ok && rules_ok {
        return None;
    }

    // 清掉所有旧的 tba 块，再整体重写两块。
    //
    // 边界必须是「下一个 `## ` 标题」，不能是「下一个 `<!-- tba:`」。
    // 入口段常常就在文件第一行，后面紧跟用户自己写的章节（这个项目是什么 /
    // 编译与部署 / 红线），按 `<!-- tba:` 找边界会把它们整段吞掉——
    // 实测把一份 100 多行的 AGENTS.md 删到只剩两个块。
    old = strip_tba_block(&old, ENTRY_MARK);
    old = strip_tba_block(&old, RULES_MARK);
    // 删块会留下连续空行，收一下。
    while old.contains("\n\n\n") {
        old = old.replace("\n\n\n", "\n\n");
    }

    // 绝不覆盖用户自己的内容：只在末尾追加。
    let mut s = old.trim_end().to_string();
    for block in [&rules, &entry] {
        if !s.is_empty() {
            s.push_str("\n\n");
        }
        s.push_str(block);
    }
    std::fs::write(&path, s).ok()?;
    Some(path)
}

// go() 输出头：claim 元信息 + 交回硬提示。
// 提示放最前面是唯一能被看见的位置——实测 agent 读完任务正文就直接开干，
// 干完就结束回合，完全不翻后面的流程文案。命令里的 claim_id 直接填好，
// 省得它自己去拼。抽成函数只为能测：这段文案丢了没人会发现。
fn claim_header(c: &Claim, exe: &str) -> String {
    format!(
        "claim_id: {}\ntask_id:  {}\ntask_name: {}\n状态:      {} -> {}\n\n\
         ⚠ 做完必须交回，两步不许省：\n  \
         {exe} note {} --text \"改了什么/怎么验\"\n  \
         {exe} ask {} --done\n\
         ask 会阻塞等你回数字，禁止 run_in_background，绝不结束会话。只有用户能收尾。\n",
        c.claim_id, c.task_id, c.task_name, c.stage_from, c.stage_to, c.claim_id, c.claim_id
    )
}

pub fn go(
    store: &Store,
    project: Option<&str>,
    lang: Lang,
    write_entry: bool,
) -> Result<GoOutcome, String> {
    // 等待中断的检查不在这里做，挪进了 claim：那边才知道「列1 有谁」，
    // 才能把用户拖回列1 的任务排除掉（拖回列1 = 用户要求重来，那条等待作废）。
    let explicit = project.map(str::trim).filter(|s| !s.is_empty());
    let bound = read_project_binding();
    let here = cwd_name();

    // 1) 先判断「这是不是一个项目目录」。
    //    既没绑定、又不在 git 仓库里 -> 多半是随手开的目录（桌面、用户根目录）。
    //    这种情况必须停：去猜项目就会把任务领到错的地方。
    if explicit.is_none() && bound.is_none() && !is_git_repo() {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".to_string());
        return Ok(GoOutcome::NoBinding { cwd });
    }

    // 2) 项目 id：显式 > 本地绑定 > 按目录名去线上找同名项目。
    //    第三档是为换设备准备的：新机器没有 .tba/project，但 Teambition 上项目还在。
    //    网络失败要报错，不能当成「项目不存在」——那会让 agent 去重建一个本来好好的项目。
    let mut ctx = Ctx::new(store.clone(), false)?;
    let list = ctx.list_projects()?;
    let mut adopted = false;
    let pid = if let Some(p) = explicit {
        p.to_string()
    } else if let Some(b) = bound {
        b
    } else {
        let hits: Vec<&ProjectInfo> = list.iter().filter(|p| p.name == here).collect();
        match hits.len() {
            1 => {
                adopted = true;
                hits[0].id.clone()
            }
            // 0 = 没这个项目；多个 = 名字撞了，得让用户指定。
            // 这里也补入口文件：已经确认是项目目录，只是项目还没建。
            // 只等「领到任务」才补就太晚了——项目没建时用户永远进不了两字开工的状态。
            _ => {
                if write_entry {
                    ensure_entry_file(store);
                }
                return Ok(GoOutcome::NoProject {
                    id: String::new(),
                    name: here,
                });
            }
        }
    };

    let proj = match list.iter().find(|p| p.id == pid).cloned() {
        Some(p) => p,
        // 显式给了 id：项目列表可能漏项，照用；真不存在 claim 会报错。
        None if explicit.is_some() => ProjectInfo {
            id: pid.clone(),
            name: pid.clone(),
        },
        None => {
            return Ok(GoOutcome::NoProject {
                id: pid,
                name: here,
            })
        }
    };
    // 按目录名认回来的项目，顺手把绑定补上，下次不用再查。
    if adopted {
        let _ = write_project_binding(&proj.id);
    }

    // 3) 领活。队列空是正常结果，不是错误。
    //    领之前先回收死会话，别让一个已经不存在的会话占着任务。
    for m in sweep(store, false).unwrap_or_default() {
        eprintln!("{m}");
    }
    // 多会话共目录时必须能区分，否则会互相续命（僵尸任务回收不掉）。
    // 没登记过就按目录名补一个；同目录要开两个会话仍得显式 `tba hello --name`。
    if current_agent() == "cli" {
        let _ = set_session(Some(&cwd_name()));
    }
    let agent = current_agent();
    let c = match claim(store, &proj.id, None, &agent) {
        Ok(c) => c,
        Err(e) if e.starts_with(BLOCKED_PREFIX) => {
            return Ok(GoOutcome::Blocked {
                msg: e.trim_start_matches(BLOCKED_PREFIX).to_string(),
            })
        }
        Err(e) if e.starts_with(NO_TASK_PREFIX) => {
            // 队列空也要补入口文件：队列空的时候用户最可能去翻 AGENTS.md
            // 找「工具在哪」，正是该有 tba.cmd 的时候。
            if write_entry {
                ensure_entry_file(store);
            }
            return Ok(GoOutcome::Empty {
                project_name: proj.name,
            });
        }
        Err(e) => return Err(e),
    };

    // 4) 任务正文 + 流程文案。正文放前面：先知道干什么，再读怎么干。
    let body = ctx
        .task_info(&c.task_id)
        .map(|t| t.content)
        .unwrap_or_default();
    let skills = list_skills(store, &proj.id).unwrap_or_default();
    let rules = prompt_body(store, &proj.id, &proj.name, &skills, None, Some(&c), lang);

    let mut text = claim_header(&c, &exe_path_or(&entry_cmd(store)));
    if !body.trim().is_empty() {
        text.push_str("\n---- 任务正文 ----\n");
        text.push_str(body.trim());
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&rules);

    let entry = if write_entry {
        ensure_entry_file(store)
    } else {
        None
    };
    Ok(GoOutcome::Claimed { text, entry })
}

pub fn agent_prompt(
    store: &Store,
    project_id: &str,
    project_name: &str,
    draft: Option<&TaskDraft>,
    lang: Lang,
) -> Result<String, String> {
    // 不吞错误：读不到 skill 要让用户看到原因，而不是默默生成一份没有 skill 的文案
    let skills = list_skills(store, project_id)?;
    Ok(prompt_body(
        store, project_id, project_name, &skills, draft, None, lang,
    ))
}

// 供 UI 用：从已有 skill 列表直接生成文案，不再请求网络。
pub fn agent_prompt_from(
    store: &Store,
    project_id: &str,
    project_name: &str,
    skills: &[SkillItem],
    draft: Option<&TaskDraft>,
    lang: Lang,
) -> String {
    prompt_body(store, project_id, project_name, skills, draft, None, lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 外部收尾判定：用户网页手动改状态、或拖到列4/列5，都算「已完成」。
    // 列名用数字（1..5），列优先于状态名（可能只拖了列没改状态）。
    #[test]
    fn external_done_by_detects_manual_completion() {
        let cfg = crate::config::Config::default(); // tasklist_done="4" closed="5"
        // 只拖列、状态还是「进行中」也算完成
        assert_eq!(external_done_by("进行中", "4", &cfg), Some("done"));
        assert_eq!(external_done_by("进行中", "5", &cfg), Some("closed"));
        // 只改状态、列还在 2 也算完成
        assert_eq!(external_done_by("已解决", "2", &cfg), Some("done"));
        assert_eq!(external_done_by("已完成", "2", &cfg), Some("closed"));
        // 列和状态冲突时列优先：拖到列5=关闭
        assert_eq!(external_done_by("已解决", "5", &cfg), Some("closed"));
        // 没收尾的都不算
        assert_eq!(external_done_by("未完成", "1", &cfg), None);
        assert_eq!(external_done_by("进行中", "2", &cfg), None);
        assert_eq!(external_done_by("待回复", "3", &cfg), None);
        // 空列名（数据源没有看板列维度）时退化成只看状态
        assert_eq!(external_done_by("已解决", "", &cfg), Some("done"));
        assert_eq!(external_done_by("进行中", "", &cfg), None);
    }

    fn tmp_store(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("tba-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Store { root }
    }

    // 粗估 token（cl100k 量级）：CJK 约 1 字 1 token，其余约 4 字符 1 token。
    // 用来断言「英文版真的更省」，别拿字符数比——中文一字符信息量大，字符数天然更少。
    fn est_tokens(s: &str) -> usize {
        let mut cjk = 0usize;
        let mut other = 0usize;
        for c in s.chars() {
            if ('\u{3000}'..='\u{9fff}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c) {
                cjk += 1;
            } else {
                other += 1;
            }
        }
        cjk + other / 4
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
        let p_this = agent_prompt_from(&store, pid, name, &skills, draft.as_ref(), Lang::Zh);
        let p_queue = agent_prompt_from(&store, pid, name, &skills, None, Lang::Zh);
        assert!(p_this.contains("本次任务"), "ThisTask 版应带本次任务段");
        assert!(p_this.contains("start"), "ThisTask 版应让 agent 跑 start");
        assert!(p_queue.contains("claim"), "Queue 版应让 agent 跑 claim");
        assert_ne!(p_this, p_queue);
    }

    // 两种语言的关键不变量一条都不许丢。
    // 数字回复协议：只有 0 收尾，其余都回列2。
    // 解析必须防歧义——宁可当文本让 agent 读原文，也不猜错（猜错就是收错任务）。
    #[test]
    fn reply_numbers_map_to_actions() {
        // 干净的数字
        assert_eq!(parse_reply("0").0, ReplyAction::Closed);
        assert_eq!(parse_reply("1").0, ReplyAction::Summarize);
        assert_eq!(parse_reply("2").0, ReplyAction::Recheck);
        assert_eq!(parse_reply("3").0, ReplyAction::Custom(3));
        assert_eq!(parse_reply("9").0, ReplyAction::Custom(9));

        // 数字后面跟标点 / 空白 / 中文：认
        assert_eq!(parse_reply("0.").0, ReplyAction::Closed);
        assert_eq!(parse_reply("0 可以了").0, ReplyAction::Closed);
        assert_eq!(parse_reply("0可以了").0, ReplyAction::Closed);
        assert_eq!(parse_reply("  1  ").0, ReplyAction::Summarize);

        // 附言要带出来，但去掉数字前缀
        assert_eq!(parse_reply("0 可以了").1, "可以了");
        assert_eq!(parse_reply("2. 还是报错").1, "还是报错");

        // 不认：数字不在开头 / 后面跟字母 / 正文还有数字（有歧义）/ 空
        assert_eq!(parse_reply("好的，0").0, ReplyAction::Free);
        assert_eq!(parse_reply("0abc").0, ReplyAction::Free);
        assert_eq!(parse_reply("1 和 2 都做了").0, ReplyAction::Free);
        assert_eq!(parse_reply("把那个函数也改一下").0, ReplyAction::Free);
        assert_eq!(parse_reply("").0, ReplyAction::Free);

        // 数字↔动作的对应写进了文案，不能错位
        assert_eq!(ReplyAction::Closed.num(), Some(0));
        assert_eq!(ReplyAction::Summarize.num(), Some(1));
        assert_eq!(ReplyAction::Recheck.num(), Some(2));
        assert_eq!(ReplyAction::Free.num(), None);
    }

    // 记忆标题的生成与解析必须互逆：写错了会把记忆读丢（或者把普通任务读成记忆）。
    #[test]
    fn memory_title_roundtrip() {
        for sec in MEMORY_SECTIONS {
            let t = memory_title(sec, "看板列用数字 1..5\n第二行不进标题");
            assert!(t.starts_with(sec), "标题必须以节名开头: {t}");
            assert!(t.contains("看板列用数字"), "标题要带摘要: {t}");
            assert!(!t.contains("第二行"), "摘要只取首行: {t}");
            assert_eq!(parse_memory_title(&t).as_deref(), Some(sec));
        }
        // 只有节名、没有摘要也认（用户手建的、或空正文）
        assert_eq!(
            parse_memory_title(MEMORY_SECTION_RULES).as_deref(),
            Some(MEMORY_SECTION_RULES)
        );
        // 用户手动改过摘要：只要节名+分隔符在开头就还认得
        assert_eq!(
            parse_memory_title("踩坑 · 我自己改的摘要").as_deref(),
            Some(MEMORY_SECTION_PITFALLS)
        );
        assert_eq!(
            parse_memory_title("决策：为什么用备注").as_deref(),
            Some(MEMORY_SECTION_DECISIONS)
        );
        // 不是记忆的标题一律不认（宁可漏读，也不能把普通任务当记忆）
        assert_eq!(parse_memory_title("通用能力方案"), None);
        assert_eq!(parse_memory_title("skill"), None);
        assert_eq!(parse_memory_title("踩坑记录"), None);
        assert_eq!(parse_memory_title(""), None);
    }

    // 一条记忆超长时标题要截断，不能把整篇正文塞进标题。
    #[test]
    fn memory_title_is_brief() {
        let long = "很长的结论".repeat(50);
        let t = memory_title(MEMORY_SECTION_PITFALLS, &long);
        // 节名 + 分隔符 + 最多 MEMORY_TITLE_BRIEF 字
        let brief = t
            .strip_prefix(MEMORY_SECTION_PITFALLS)
            .and_then(|r| r.strip_prefix(MEMORY_TITLE_SEP))
            .expect("应该有摘要");
        assert!(brief.chars().count() <= MEMORY_TITLE_BRIEF);
        assert_eq!(parse_memory_title(&t).as_deref(), Some(MEMORY_SECTION_PITFALLS));
    }

    // 开关只影响「读记忆提示」和「项目记忆行」，别的规则一条不能动。
    #[test]
    fn memory_switch_only_toggles_memory_hint() {
        let store = tmp_store("wfmem");
        let cfg = store.load_config();
        for lang in [Lang::Zh, Lang::En] {
            let on = workflow_text("tba", "p-1", &cfg, None, None, lang, true);
            let off = workflow_text("tba", "p-1", &cfg, None, None, lang, false);
            let tag = lang.label();
            assert!(on.contains("memory --project"), "{tag}版开关开时缺读记忆命令");
            assert!(
                !off.contains("memory --project"),
                "{tag}版开关关时不该出现记忆命令"
            );
            for code in ["exit 0", "exit 2", "exit 3"] {
                assert!(on.contains(code), "{tag}版开关开时缺 {code}");
                assert!(off.contains(code), "{tag}版开关关时缺 {code}");
            }
            // 开关开时多一句读记忆，必然更长；但不能因此失控
            assert!(on.chars().count() > off.chars().count(), "{tag}版开关没起作用");
        }
    }

    // 这些是「agent 跑偏」的防线：改 workflow_text(_zh) 时先跑这个测试。
    #[test]
    fn workflow_keeps_key_invariants() {
        let store = tmp_store("wf");
        let cfg = store.load_config();
        for lang in [Lang::Zh, Lang::En] {
            let wf = workflow_text("tba", "p-1", &cfg, None, None, lang, false);
            let tag = lang.label();
            // 闭环命令一个不能少（命令本身两种语言完全一致）
            for cmd in [
                "claim --project",
                "take <",
                "beat <claim_id>",
                "note <claim_id>",
                "ask <claim_id>",
                "wait <claim_id>",
                "done <claim_id>",
                "close <claim_id>",
                "release <claim_id>",
            ] {
                assert!(wf.contains(cmd), "{tag}版缺命令: {cmd}");
            }
            // 五个阶段名一个都不能漏（曾经漏了「未完成」）
            for st in ALL_STAGES {
                assert!(wf.contains(st), "{tag}版缺阶段名: {st}");
            }
            // 退出码语义
            for code in ["exit 0", "exit 2", "exit 3"] {
                assert!(wf.contains(code), "{tag}版缺 {code} 语义");
            }
            assert!(wf.contains("hello --name"), "{tag}版缺多会话命名提示");

            // 行为规则：两种语言用各自的说法表达同一批规则。
            let rules: [&[&str]; 7] = if lang == Lang::Zh {
                [
                    &["不许自己改任务状态或看板列"],
                    &["不许自编任务"],
                    &["不许自己跑 `tba done`", "只有用户能收尾"],
                    &["纯文本编号", "禁用选择框"],
                    &["长操作前后各跑一次"],
                    &["永不超时"],
                    &["自动退回", "用户已收尾"],
                ]
            } else {
                [
                    &["never edit task state or board columns yourself"],
                    &["never invent tasks"],
                    &["Do NOT run `tba done`", "only the user closes"],
                    &["numbered", "NEVER use a picker"],
                    &["before/after long ops"],
                    &["never times out"],
                    &["auto-return", "user already closed it"],
                ]
            };
            for group in rules {
                for piece in group {
                    assert!(wf.contains(piece), "{tag}版缺规则片段: {piece}");
                }
            }
        }
        // 中文版是准本，不该混进英文速查表；英文版也不该整段中文（阶段名除外）。
        assert!(
            !workflow_text("tba", "p-1", &cfg, None, None, Lang::En, false).contains("不许"),
            "英文版不该出现中文规则句"
        );
    }

    // 带草稿时走 start 登记，且不许去队列领——两种语言都要守住。
    #[test]
    fn draft_branch_registers_instead_of_claiming() {
        let store = tmp_store("wfdraft");
        let cfg = store.load_config();
        let d = draft_for_mode(&store, "# 草稿标题\n正文", CopyMode::ThisTask);
        for lang in [Lang::Zh, Lang::En] {
            let wf = workflow_text("tba", "p-1", &cfg, d.as_ref(), None, lang, false);
            assert!(wf.contains("start --project"), "{}版该走 start", lang.label());
            assert!(
                !wf.contains("claim --project"),
                "{}版带了草稿就不该让 agent 去队列领",
                lang.label()
            );
        }
    }

    // skill 只列名字，不内联全文——文案 token 的最大头必须按需读取。
    #[test]
    fn prompt_lists_skill_names_not_full_content() {
        let store = tmp_store("skillname");
        let pid = "p-1";
        let big = "部署流程要点：".to_string() + &"很长的部署内容。".repeat(200);
        let skills = vec![SkillItem {
            task_id: "t-1".into(),
            name: "部署流程".into(),
            content: big.clone(),
            on: true,
        }];
        let p = agent_prompt_from(&store, pid, "项目一", &skills, None, Lang::Zh);
        assert!(p.contains("部署流程"), "skill 名字必须出现在文案里");
        assert!(
            p.contains("skill <name> --project"),
            "必须给出按需读取命令 tb skill <名字>"
        );
        assert!(!p.contains(&big), "skill 全文不许内联进文案");
        assert!(!p.contains("很长的部署内容"), "skill 正文片段也不该进文案");
    }

    // 体积回归：skill 再大，文案恒定成本也不能跟着涨。
    #[test]
    fn prompt_with_huge_skill_stays_small() {
        let store = tmp_store("skillsize");
        let pid = "p-1";
        let mut skills: Vec<SkillItem> = Vec::new();
        for i in 0..20 {
            skills.push(SkillItem {
                task_id: format!("t-{i}"),
                name: format!("skill-{i}"),
                content: "x".repeat(5000),
                on: true,
            });
        }
        // 20 个 5000 字 skill 全内联会是 10 万+ 字符；只列名字应在 2 千字符上下。
        // 两种语言都量一遍：中文准本也不能超，英文必须更省 token。
        let zh = agent_prompt_from(&store, pid, "项目一", &skills, None, Lang::Zh);
        let en = agent_prompt_from(&store, pid, "项目一", &skills, None, Lang::En);

        // 实测（20 个 5000 字 skill）：中文 1756 字符 ≈710 token，英文 2385 字符 ≈615 token。
        // 字符数英文反而更多，但 token 更少——中文字在 BPE 里几乎 1 字 1 token。
        // 预算按实测留一点余量：涨过就说明文案又变长了，要主动确认（用户明确要求「短」）。
        // v0.2.4 加了一行通用能力名单（约 116 字符，含 exe 路径），预算跟着抬一点；
        // 再涨就说明文案又变长了，要主动确认。
        for (tag, p, budget) in [("中文", &zh, 2200usize), ("英文", &en, 3000usize)] {
            assert!(
                p.chars().count() < budget,
                "{tag}版文案体积失控: {} 字符（预算 {budget}）",
                p.chars().count()
            );
        }
        assert!(
            est_tokens(&en) < est_tokens(&zh),
            "英文版应比中文版省 token: en≈{} zh≈{}",
            est_tokens(&en),
            est_tokens(&zh)
        );
    }

    // AGENTS.md 入口：两个块各自补，用户自己的内容一个字不动。
    // 重点是「老项目也能补上后加的编码纪律」——不能被「已有入口」挡住。
    #[test]
    fn entry_file_adds_rules_without_touching_user_content() {
        let store = tmp_store("entry");
        let dir = store.root.clone();

        // 1) 空目录：两个块都写进去，四条原则一条不少。
        let p = ensure_entry_at(&dir, "tba").expect("空目录该生成 AGENTS.md");
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains(ENTRY_MARK), "缺任务队列入口块");
        assert!(s.contains(RULES_MARK), "缺编码纪律块");
        for rule in ["先想再写", "最简实现", "只改必须改的", "先定验证标准"] {
            assert!(s.contains(rule), "缺编码纪律: {rule}");
        }

        // 2) 再跑一次：两块都在，不该再写。
        assert!(
            ensure_entry_at(&dir, "tba").is_none(),
            "两块都在时不该再写文件"
        );

        // 3) 老项目只有入口块：只补纪律块，入口块不重复。
        std::fs::write(
            &p,
            format!("# 用户自己的说明\n\n{ENTRY_MARK} 旧版入口 -->\n## 任务队列入口\n\n旧内容\n"),
        )
        .unwrap();
        assert!(
            ensure_entry_at(&dir, "tba").is_some(),
            "老项目该补上纪律块"
        );
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("# 用户自己的说明"), "用户内容被动了");
        assert!(s.contains(RULES_MARK), "老项目没补上纪律块");
        assert_eq!(s.matches(ENTRY_MARK).count(), 1, "入口块被重复写了");
        assert_eq!(s.matches(RULES_MARK).count(), 1, "纪律块被重复写了");
    }

    // go() 输出头里的「交回」提示丢了，agent 就会干完直接结束回合——
    // 实测发生过两次，用户只能自己去翻看板。这段文案必须有单测兜着。
    #[test]
    fn go_header_tells_agent_to_hand_back_first() {
        let c = fake_waiting("clm-x", "2026-01-01T00:00:00Z");
        let h = claim_header(&c, "tba");

        // 两条命令都得把 claim_id 填好，别让 agent 自己拼。
        assert!(h.contains("tba note clm-x --text"), "缺 note 命令: {h}");
        assert!(h.contains("tba ask clm-x --done"), "缺 ask 命令: {h}");
        assert!(h.contains("绝不结束会话"), "缺不许断会话的硬话: {h}");

        // 提示必须在 claim_id 之后、紧跟其后——不能沉到正文后面去。
        let head = h.find("claim_id:").expect("缺 claim_id 行");
        let warn = h.find("⚠ 做完必须交回").expect("缺交回提示");
        assert!(warn > head, "交回提示必须在 claim_id 之后");
        assert!(warn - head < 200, "交回提示离 claim_id 太远，容易被跳过");
    }

    // 造一条台账记录，只关心跟孤儿判定相关的字段。
    fn fake_waiting(claim_id: &str, ask_at: &str) -> Claim {
        Claim {
            claim_id: claim_id.to_string(),
            task_id: format!("task-{claim_id}"),
            task_name: "测试任务".into(),
            project_id: "p-1".into(),
            stage_from: STAGE_DOING.into(),
            stage_to: STAGE_WAIT.into(),
            agent: "s-test".into(),
            cwd: ".".into(),
            state: "waiting".into(),
            created_at: ask_at.to_string(),
            updated_at: ask_at.to_string(),
            note: String::new(),
            heartbeat_at: ask_at.to_string(),
            tasklist_from: "2".into(),
            ask_at: ask_at.to_string(),
            question: "测试问题".into(),
        }
    }

    // 「任务在等，但没人接了」的判定：只看等待心跳，别误伤刚 ask 的。
    #[test]
    fn orphan_wait_detects_stopped_heartbeat() {
        let store = tmp_store("orphan");
        let now = chrono::Local::now().to_rfc3339();
        let old = (chrono::Local::now() - chrono::Duration::seconds(600)).to_rfc3339();

        // fresh：刚 ask，宽限期内，不该算孤儿
        // stale：ask 很久且从没写过心跳（旧版遗留），算孤儿
        // dead：写过心跳但停更了（进程被杀），算孤儿
        store.append_claim(&fake_waiting("clm-fresh", &now)).unwrap();
        store.append_claim(&fake_waiting("clm-stale", &old)).unwrap();
        store.append_claim(&fake_waiting("clm-dead", &old)).unwrap();

        let d = store.waits_dir();
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("clm-dead.wait"), format!("1\n{old}")).unwrap();

        let got: Vec<String> = orphan_waits(&store)
            .into_iter()
            .map(|c| c.claim_id)
            .collect();
        assert!(!got.contains(&"clm-fresh".to_string()), "刚 ask 的不该算孤儿");
        assert!(got.contains(&"clm-stale".to_string()), "旧版遗留该算孤儿");
        assert!(got.contains(&"clm-dead".to_string()), "心跳停更该算孤儿");

        // 刷新一次心跳，dead 就不再是孤儿。
        store.wait_touch("clm-dead");
        let got: Vec<String> = orphan_waits(&store)
            .into_iter()
            .map(|c| c.claim_id)
            .collect();
        assert!(!got.contains(&"clm-dead".to_string()), "刷新后不该再算孤儿");

        // 正常结束要删文件，删完就该是「从没等过」——用 ask_at 兜底仍然判孤儿。
        store.wait_drop("clm-dead");
        assert!(store.wait_age("clm-dead").is_none(), "wait_drop 没删掉文件");
    }

    // 邮件正文要带完整备注：用户在外面收信，光看到「已完成，请确认」判断不了
    // agent 到底干了什么。
    #[test]
    fn mail_body_carries_full_note() {
        let c = fake_waiting("clm-1", "2026-10-31T15:00:00+08:00");
        let note = "#### 2026-10-31 15:20\n\n改了三处：\n1. 改了 A\n2. 改了 B";
        let body = mail_body(&c, "已完成，请确认", note);
        assert!(body.contains("任务备注（完整）"), "缺备注分区标题");
        assert!(body.contains("改了三处"), "备注正文没进邮件");
        assert!(body.contains("改了 B"), "备注后半段被截断了");
        assert!(body.contains(&c.task_id), "缺任务链接");
        assert!(body.contains(&c.claim_id), "缺回信标记");

        // 备注为空 → 退回只发问题，不发一封空信。
        let body = mail_body(&c, "已完成，请确认", "");
        assert!(body.contains("已完成，请确认"), "备注为空时该退回问题");
    }
}
