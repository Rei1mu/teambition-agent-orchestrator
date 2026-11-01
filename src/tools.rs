use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// MCP 工具的一条记录。
#[derive(Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
}

// 业务角色 -> 工具名。自动识别 + 手工覆盖。
#[derive(Clone, Default)]
pub struct ToolMap {
    pub list_projects: Option<String>,
    // 建空白项目。tb ensure-project 用它给当前目录开一个新项目。
    pub create_project: Option<String>,
    pub get_project: Option<String>,
    pub list_taskflows: Option<String>,
    pub get_taskflow: Option<String>,
    pub create_taskflow: Option<String>,
    pub list_stages: Option<String>,
    pub create_stage: Option<String>,
    pub list_task_types: Option<String>,
    pub create_task_type: Option<String>,
    pub create_task: Option<String>,
    pub update_task: Option<String>,
    pub move_task: Option<String>,
    pub list_tasks: Option<String>,
    pub get_task: Option<String>,
    // 看板列（任务列表）维度：把任务挪到指定列。
    pub move_to_list: Option<String>,
    // 任务动态（含评论）读取。tb reply 靠它读回用户在 Teambition 的回复。
    pub read_activity: Option<String>,
    // 发评论。tb ask 把问题写到任务上。
    pub comment: Option<String>,
}

// 一个角色的识别规则。
// must  : 工具名必须命中其中至少一个词，否则出局
// deny  : 命中任意一个词直接出局
// bonus : 命中加分，分高者胜
// desc  : 描述里命中核心名词的小幅加权，真实工具名不规范时兜底
struct Rule {
    must: &'static [&'static str],
    deny: &'static [&'static str],
    bonus: &'static [(&'static str, i32)],
    desc: &'static [&'static str],
}

fn score(n: &str, d: &str, r: &Rule) -> i32 {
    if r.deny.iter().any(|x| n.contains(x)) {
        return 0;
    }
    // 通用噪声词。真实工具集里同前缀工具靠子功能名区分
    // （project.task-list-query / project.task-count / project.tfs-query ...），
    // 不显式排除就会互相抢分，曾经把 create_task 匹配到上传工具。
    if NOISE.iter().any(|x| n.contains(x)) {
        return 0;
    }
    if !r.must.is_empty() && !r.must.iter().any(|m| n.contains(m)) {
        return 0;
    }
    let mut s = 0;
    for (k, v) in r.bonus {
        if n.contains(k) {
            s += v;
        }
    }
    if s == 0 {
        return 0;
    }
    for k in r.desc {
        if d.contains(k) {
            s += 2;
        }
    }
    s
}

// Teambition 的阶段工具常叫 taskflowstatus_*，名字里带 task，
// 所以阶段类规则不能一刀切排除 "task"，只能用 flow/status 把任务流类的排除掉。

// 真实工具集里的噪声词。项目/任务类工具名经常只是前缀不同
// （project.task-list-query / project.task-count / task.query ...），
// 只靠 must + bonus 会互相抢分，必须把这些子功能名显式排除。
// 注意：sfc（任务类型维度）不在噪声里——它是一条独立角色，靠下面的
// R_LIST_TASK_TYPES / R_CREATE_TASK_TYPE 显式识别，不能一刀切排除。
const NOISE: &[&str] = &[
    "smartgroup", "sprint", "file", "folder", "member", "customfield",
    "objectlink", "priority", "column", "report", "stats", "tag", "template",
    "role", "upload", "token", "deliverable", "count", "progress", "group",
    "org", "node", "trace", "dispatch", "dependency", "activity", "html",
    "comment", "worktime", "program", "plan",
];

const R_LIST_PROJECTS: Rule = Rule {
    must: &["project"],
    deny: &[
        "task", "create", "update", "delete", "remove", "taskflow", "tfs",
        "status", "archive", "restore", "sfc",
    ],
    bonus: &[("list", 10), ("query", 9), ("search", 9), ("all", 8), ("get", 3), ("project", 2)],
    desc: &["项目", "project"],
};

// 建空白项目。真实工具叫 project.create，只接受 name（传 organizationId 会报 not allowed）。
// create-from-template 名字更长、语义也不对，用 template 显式排掉，双保险。
const R_CREATE_PROJECT: Rule = Rule {
    must: &["project"],
    deny: &[
        "task", "taskflow", "tfs", "status", "list", "query", "search",
        "get", "update", "delete", "archive", "restore", "sfc", "template",
        "program", "plan", "file", "member", "tag", "column", "role",
    ],
    bonus: &[("create", 10), ("add", 9)],
    desc: &["项目", "project"],
};

const R_GET_PROJECT: Rule = Rule {
    must: &["project"],
    // 真实工具集里没有独立的「取项目详情」工具，project.query 收 projectId，
    // 就是事实上的 get。所以 query 只给小分，不让它像 list 那样抢。
    deny: &[
        "task", "list", "search", "all", "create", "update", "delete",
        "taskflow", "tfs", "status", "archive", "restore", "sfc",
    ],
    bonus: &[
        ("get", 10),
        ("detail", 10),
        ("info", 10),
        ("query", 3),
        ("project", 2),
    ],
    desc: &["项目", "project"],
};

const R_LIST_TASKFLOWS: Rule = Rule {
    must: &["taskflow", "flow"],
    deny: &["status", "create", "update", "delete", "node"],
    bonus: &[("list", 10), ("query", 9), ("all", 8), ("get", 3), ("project", 2)],
    desc: &["任务流", "taskflow"],
};

const R_GET_TASKFLOW: Rule = Rule {
    must: &["taskflow", "flow"],
    deny: &[
        "status", "list", "query", "search", "all", "create", "update",
        "delete", "node",
    ],
    bonus: &[("get", 10), ("detail", 10), ("info", 10), ("project", 2)],
    desc: &["任务流", "taskflow"],
};

const R_CREATE_TASKFLOW: Rule = Rule {
    must: &["taskflow", "flow"],
    deny: &["status", "update", "delete", "node"],
    bonus: &[("create", 10), ("add", 9), ("project", 2)],
    desc: &["任务流", "taskflow"],
};

// 阶段 = 工作流状态。真实工具叫 tfs-query，mock 叫 taskflowstatus.query。
const R_LIST_STAGES: Rule = Rule {
    must: &["stage", "taskflowstatus", "status", "tfs"],
    deny: &[
        "create", "add", "update", "move", "delete", "node", "customfield",
        "executor", "validator", "archive", "restore",
    ],
    bonus: &[("list", 10), ("query", 9), ("all", 8), ("get", 3), ("project", 2)],
    desc: &["阶段", "stage"],
};

const R_CREATE_STAGE: Rule = Rule {
    must: &["stage", "taskflowstatus", "status", "tfs"],
    deny: &[
        "update", "move", "delete", "query", "node", "customfield",
        "executor", "validator", "archive", "restore",
    ],
    bonus: &[("create", 10), ("add", 9), ("project", 2)],
    desc: &["阶段", "stage"],
};

// 任务类型 = sfc。真实工具叫 project.sfc-query / project.sfc-create。
// 这是第 4 个维度，和阶段、看板列都无关；skill 就是这里的「skill」类型。
const R_LIST_TASK_TYPES: Rule = Rule {
    must: &["sfc"],
    deny: &[
        "create", "update", "delete", "field", "node", "task", "stage",
        "status", "upsert",
    ],
    bonus: &[("query", 10), ("list", 9), ("get", 3)],
    desc: &["任务类型", "sfc", "type"],
};

const R_CREATE_TASK_TYPE: Rule = Rule {
    must: &["sfc"],
    deny: &[
        "query", "list", "get", "update", "delete", "field", "node", "task",
        "stage", "status", "upsert",
    ],
    bonus: &[("create", 10), ("add", 9)],
    desc: &["任务类型", "sfc", "type"],
};

const R_CREATE_TASK: Rule = Rule {
    must: &["task"],
    deny: &[
        "flow", "status", "update", "move", "delete", "node", "comment",
        "dependency", "trace", "user", "parent", "type", "tags", "members",
        "list", "count", "file", "query", "search", "get", "archive",
        "restore", "stage",
    ],
    bonus: &[("create", 10), ("add", 9), ("project", 2)],
    desc: &["任务", "task"],
};

const R_UPDATE_TASK: Rule = Rule {
    must: &["task"],
    deny: &[
        "flow", "status", "create", "move", "delete", "node", "comment",
        "dependency", "trace", "user", "parent", "type", "tags", "members",
        "list", "count", "file", "query", "search", "get", "archive",
        "restore", "stage",
    ],
    bonus: &[("update", 10), ("edit", 10), ("modify", 10), ("project", 2)],
    desc: &["任务", "task"],
};

// 真实 Teambition 里改任务状态用 task.update-status（按 tfsName/taskflowstatusId）；
// task.move 是「把任务移到另一个项目」，完全不是一回事。mock 用 task.move。
const R_MOVE_TASK: Rule = Rule {
    must: &["task"],
    deny: &[
        "flow", "create", "delete", "node", "comment", "dependency", "trace",
        "user", "parent", "type", "tags", "members", "list", "count", "file",
        "query", "search", "get", "archive", "restore", "stage",
    ],
    bonus: &[
        ("update-status", 20),
        ("move", 11),
        ("transition", 11),
        ("status", 4),
        ("project", 2),
    ],
    desc: &["任务", "task"],
};

const R_LIST_TASKS: Rule = Rule {
    must: &["task"],
    deny: &[
        "flow", "create", "update", "move", "delete", "node", "comment",
        "dependency", "trace", "user", "parent", "type", "tags", "members",
        "count", "file", "archive", "restore", "stage", "status",
        // task-list-* 是「看板列」不是任务：project.task-list-query 名字里
        // 同时有 task 和 query，不排掉会把 list_tasks 抢成看板列查询。
        "task-list", "tasklist",
    ],
    bonus: &[("list", 10), ("query", 9), ("search", 9), ("all", 8), ("project", 2)],
    desc: &["任务", "task"],
};

const R_GET_TASK: Rule = Rule {
    must: &["task"],
    deny: &[
        "flow", "list", "query", "search", "all", "create", "update",
        "move", "delete", "node", "comment", "dependency", "trace", "user",
        "parent", "type", "tags", "members", "count", "file", "archive",
        "restore", "stage", "status",
    ],
    bonus: &[("get", 10), ("detail", 10), ("info", 10), ("project", 2)],
    desc: &["任务", "task"],
};

// 从工具列表里挑一个最像的角色工具。同分取名字短的。
fn pick(tools: &[ToolInfo], r: &Rule) -> Option<String> {
    let mut best: Option<(i32, String)> = None;
    for t in tools {
        let n = t.name.to_lowercase();
        let d = t.description.to_lowercase();
        let s = score(&n, &d, r);
        if s <= 0 {
            continue;
        }
        let replace = match &best {
            None => true,
            Some((bs, bn)) => s > *bs || (s == *bs && t.name.len() < bn.len()),
        };
        if replace {
            best = Some((s, t.name.clone()));
        }
    }
    best.map(|(_, n)| n)
}

// 按名字后缀精确匹配，**绕过 NOISE 噪声词表**。
//
// 为什么需要：comment / activity 这类词在 NOISE 里，是为了防止它们污染
// 任务类规则；但它们本身又是独立角色。与其把它们从 NOISE 里删掉去赌
// 别的规则不误配，不如给这几个「名字唯一」的角色单开一条精确通道。
fn pick_suffix(tools: &[ToolInfo], suffixes: &[&str]) -> Option<String> {
    for suf in suffixes {
        for t in tools {
            if t.name.to_lowercase().ends_with(suf) {
                return Some(t.name.clone());
            }
        }
    }
    None
}

impl ToolMap {
    pub fn resolve(tools: &[ToolInfo], overrides: &HashMap<String, String>) -> ToolMap {
        let mut m = ToolMap::default();
        let get = |role: &str| overrides.get(role).cloned();

        m.list_projects = get("list_projects").or_else(|| pick(tools, &R_LIST_PROJECTS));
        m.create_project = get("create_project").or_else(|| pick(tools, &R_CREATE_PROJECT));
        m.get_project = get("get_project").or_else(|| pick(tools, &R_GET_PROJECT));
        m.list_taskflows = get("list_taskflows").or_else(|| pick(tools, &R_LIST_TASKFLOWS));
        m.get_taskflow = get("get_taskflow").or_else(|| pick(tools, &R_GET_TASKFLOW));
        m.create_taskflow = get("create_taskflow").or_else(|| pick(tools, &R_CREATE_TASKFLOW));
        m.list_stages = get("list_stages").or_else(|| pick(tools, &R_LIST_STAGES));
        m.create_stage = get("create_stage").or_else(|| pick(tools, &R_CREATE_STAGE));
        m.list_task_types = get("list_task_types").or_else(|| pick(tools, &R_LIST_TASK_TYPES));
        m.create_task_type = get("create_task_type").or_else(|| pick(tools, &R_CREATE_TASK_TYPE));
        m.create_task = get("create_task").or_else(|| pick(tools, &R_CREATE_TASK));
        m.update_task = get("update_task").or_else(|| pick(tools, &R_UPDATE_TASK));
        m.move_task = get("move_task").or_else(|| pick(tools, &R_MOVE_TASK));
        m.list_tasks = get("list_tasks").or_else(|| pick(tools, &R_LIST_TASKS));
        m.get_task = get("get_task").or_else(|| pick(tools, &R_GET_TASK));
        // 这三个角色名字唯一，走后缀精确匹配（见 pick_suffix 注释）。
        m.move_to_list = get("move_to_list")
            .or_else(|| pick_suffix(tools, &["task.update-stage", "task.update-list"]));
        m.read_activity = get("read_activity").or_else(|| pick_suffix(tools, &["task.activity"]));
        m.comment = get("comment").or_else(|| pick_suffix(tools, &["task.comment"]));

        m
    }

    // 人读摘要。tb tools 命令用。
    pub fn summary(&self) -> Vec<(&'static str, String)> {
        let rows: [(&'static str, &Option<String>); 18] = [
            ("list_projects", &self.list_projects),
            ("create_project", &self.create_project),
            ("get_project", &self.get_project),
            ("list_taskflows", &self.list_taskflows),
            ("get_taskflow", &self.get_taskflow),
            ("create_taskflow", &self.create_taskflow),
            ("list_stages", &self.list_stages),
            ("create_stage", &self.create_stage),
            ("list_task_types", &self.list_task_types),
            ("create_task_type", &self.create_task_type),
            ("create_task", &self.create_task),
            ("update_task", &self.update_task),
            ("move_task", &self.move_task),
            ("list_tasks", &self.list_tasks),
            ("get_task", &self.get_task),
            ("move_to_list", &self.move_to_list),
            ("read_activity", &self.read_activity),
            ("comment", &self.comment),
        ];
        rows.iter()
            .map(|(k, v)| {
                (
                    *k,
                    v.as_ref()
                        .cloned()
                        .unwrap_or_else(|| "(未找到)".to_string()),
                )
            })
            .collect()
    }
}

// 按 inputSchema 的属性名挑参数。逻辑键 -> 可能的属性名。
pub fn build_args(schema: &Value, pairs: &[(&str, Value)]) -> Value {
    let props = schema.get("properties").and_then(|p| p.as_object());
    let mut out = serde_json::Map::new();
    match props {
        Some(props) => {
            for (k, v) in pairs {
                let kl = k.to_lowercase();
                let mut chosen: Option<String> = None;
                for pk in props.keys() {
                    let pkl = pk.to_lowercase();
                    if pkl == kl {
                        chosen = Some(pk.clone());
                        break;
                    }
                    if pkl.contains(&kl) || kl.contains(&pkl) {
                        chosen = Some(pk.clone());
                    }
                }
                if let Some(ck) = chosen {
                    out.insert(ck, v.clone());
                }
            }
        }
        None => {
            for (k, v) in pairs {
                out.insert(k.to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

pub fn get_str(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(k) {
            if let Some(t) = s.as_str() {
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
            if let Some(n) = s.as_i64() {
                return Some(n.to_string());
            }
            if let Some(n) = s.as_u64() {
                return Some(n.to_string());
            }
        }
    }
    None
}

// 从工具返回值里挖出数组。兼容 {tasks:[]} / {result:[]} / {data:{list:[]}} 等。
pub fn extract_list(v: &Value, depth: u8) -> Vec<Value> {
    if let Some(arr) = v.as_array() {
        return arr.clone();
    }
    if depth == 0 {
        return Vec::new();
    }
    if let Some(obj) = v.as_object() {
        let keys = [
            "projects",
            "tasks",
            "items",
            "list",
            "values",
            "records",
            "result",
            "data",
            "stages",
            "taskflows",
        ];
        for k in keys {
            if let Some(sub) = obj.get(k) {
                let got = extract_list(sub, depth - 1);
                if !got.is_empty() {
                    return got;
                }
            }
        }
        // 兜底：任意数组字段
        for (_, sub) in obj.iter() {
            if let Some(arr) = sub.as_array() {
                if !arr.is_empty() {
                    return arr.clone();
                }
            }
        }
    }
    Vec::new()
}

#[derive(Clone)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
}

#[derive(Clone)]
pub struct StageInfo {
    pub id: String,
    pub name: String,
    /// 工作流状态类型：start（起始态，任务只能建在这里）/ end / 空（未知）。
    /// 真实 Teambition 的 tfs 才有这个概念，mock 没有，为空。
    pub kind: String,
}

// 任务列表（看板列）。API 只能查、不能建、不能改名。
#[derive(Clone)]
pub struct ListInfo {
    pub id: String,
    pub name: String,
}

// 任务动态里的一条评论。
#[derive(Clone)]
pub struct CommentInfo {
    pub id: String,
    pub author: String,
    pub created: String,
    pub text: String,
}

#[derive(Clone)]
pub struct TaskInfo {
    pub id: String,
    pub title: String,
    pub content: String,
    /// 工作流状态 id（tfs / TBHub 的阶段）。
    pub stage_id: String,
    /// 任务列表 id（看板列）。真实 Teambition 是任务对象上的 stageId。
    /// 取不到就空串（mock 老数据没有这一维）。
    pub list_id: String,
    /// 任务类型 id（sfc / 任务类型维度）。普通任务用默认 Task 类型。
    /// 真实 Teambition 在任务对象上用 sfcId 字段。
    /// 当前 skill / 记忆都改用工作流状态标记，这个维度暂时不参与判断，但保留解析。
    #[allow(dead_code)]
    pub sfc_id: String,
    pub created: String,
    /// 任务标签名（tagNames）。当前流程不用标签识别，保留解析备用。
    #[allow(dead_code)]
    pub tags: Vec<String>,
}

pub fn parse_project(v: &Value) -> Option<ProjectInfo> {
    let id = get_str(v, &["id", "_id", "projectId", "project_id"])?;
    let name = get_str(v, &["name", "title", "projectName"]).unwrap_or_else(|| id.clone());
    Some(ProjectInfo { id, name })
}

pub fn parse_stage(v: &Value) -> Option<StageInfo> {
    let id = get_str(
        v,
        &["id", "_id", "stageId", "taskflowstatusId", "taskflowstatus_id"],
    )?;
    let name = get_str(v, &["name", "title", "stageName", "value"])?;
    let kind = get_str(v, &["kind", "type"]).unwrap_or_default();
    Some(StageInfo { id, name, kind })
}

pub fn parse_task(v: &Value) -> Option<TaskInfo> {
    let id = get_str(v, &["id", "_id", "taskId", "task_id"])?;
    // Teambition 的 content 字段就是标题，正文在 note。
    // 标题按 title -> name -> content 取；正文把 content 放最后，
    // 优先用真正的备注字段，避免把标题当成正文。
    let title = get_str(v, &["title", "name", "content"]).unwrap_or_default();
    let content = get_str(
        v,
        &["note", "description", "body", "remark", "detail", "content"],
    )
    .unwrap_or_default();
    // 两个维度别混：tfsId 是工作流状态（TBHub 的阶段），stageId 是任务列表（看板列）。
    // 老 mock 只有 stageId 且它是当阶段用的，所以 stageId 只在没有 tfsId 时才
    // 兜底当阶段——那种情况下 list_id 留空，表示「这个数据源没有看板列维度」。
    let tfs = get_str(
        v,
        &["tfsId", "taskflowstatusId", "taskflowstatus_id"],
    );
    let col = get_str(v, &["stageId", "stage_id"]);
    let stage_id = match (&tfs, &col) {
        (Some(t), _) => t.clone(),
        (None, Some(c)) => c.clone(),
        _ => String::new(),
    };
    let list_id = if tfs.is_some() {
        col.unwrap_or_default()
    } else {
        String::new()
    };
    // 任务类型 id（sfc / 任务类型维度）。真实 Teambition 用 sfcId；
    // mock 用 sfc_id / taskType / type。取不到就空串（普通任务）。
    let sfc_id = get_str(
        v,
        &["sfcId", "sfc_id", "taskTypeId", "taskType", "type"],
    )
    .unwrap_or_default();
    let created = get_str(
        v,
        &["created", "createdAt", "createTime", "created_time", "createdDate"],
    )
    .unwrap_or_default();
    let tags = parse_tags(v);
    Some(TaskInfo {
        id,
        title,
        content,
        stage_id,
        list_id,
        sfc_id,
        created,
        tags,
    })
}

// 解析任务标签名。`enrichFields=tag` 时返回 `tagNames` 数组，也兼容逗号串。
// 取不到返回空 vec（= 无标签）。
fn parse_tags(v: &Value) -> Vec<String> {
    for key in ["tagNames", "tag_names", "tags"] {
        if let Some(val) = v.get(key) {
            if let Some(arr) = val.as_array() {
                let mut out = Vec::new();
                for item in arr {
                    if let Some(s) = item.as_str() {
                        out.push(s.to_string());
                    } else if let Some(name) = get_str(item, &["name", "title"]) {
                        out.push(name);
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            } else if let Some(s) = val.as_str() {
                let out: Vec<String> = s
                    .split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect();
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }
    Vec::new()
}
