#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
模拟 Teambition 业务 MCP 服务，用于 tbhub 端到端测试。

只实现 tbhub 需要的几个工具，工具名故意起成需要自动识别的样子
(project_list / taskflow_list / taskflowstatus_list / task_list ... )，
用来验证 ToolMap 的关键词自动识别是否可靠。

用法:
    python tools/mock_tb_mcp.py [端口]       # 默认 8791
然后在 data/config.toml 里把 mcp_url 指到 http://127.0.0.1:8791/mcp
"""
import json
import sys
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8791

# ---------------- 模拟数据 ----------------

PROJECTS = [
    {"id": "p-alpha", "name": "演示项目-Alpha", "taskflowId": "tf-alpha"},
    {"id": "p-beta", "name": "演示项目-Beta", "taskflowId": "tf-beta"},
]

# taskflowId -> stages（工作流状态 = TBHub 的 4 个阶段）
# kind 三态：start=开始 / unset=进行中 / end=结束（只有 end 算完成）
STAGES = {
    "tf-alpha": [
        # 故意只给两个阶段，让 tb init 去补剩下三个
        {"id": "st-alpha-todo", "name": "未完成", "kind": "start"},
        {"id": "st-alpha-doing", "name": "进行中", "kind": "unset"},
    ],
    "tf-beta": [
        {"id": "st-beta-todo", "name": "未完成", "kind": "start"},
        {"id": "st-beta-doing", "name": "进行中", "kind": "unset"},
        # 待回复 = 球在用户那，但没完成，kind 必须是 unset
        {"id": "st-beta-wait", "name": "待回复", "kind": "unset"},
        {"id": "st-beta-done", "name": "已解决", "kind": "end"},
        {"id": "st-beta-closed", "name": "已完成", "kind": "end"},
    ],
}

# projectId -> 任务列表(看板列)。真实 API 只能查，不能建、不能改名。
# 列名用中文，和 config 的默认值一致（1 未完成 / 2 进行中 / 3 待回复 / 4 已解决 / 5 已完成）
TASKLISTS = {
    "p-alpha": [
        {"id": "lst-alpha-1", "name": "未完成"},
        {"id": "lst-alpha-2", "name": "进行中"},
        {"id": "lst-alpha-3", "name": "待回复"},
        {"id": "lst-alpha-4", "name": "已解决"},
        {"id": "lst-alpha-5", "name": "已完成"},
    ],
    "p-beta": [
        {"id": "lst-beta-1", "name": "未完成"},
        {"id": "lst-beta-2", "name": "进行中"},
        {"id": "lst-beta-3", "name": "待回复"},
        {"id": "lst-beta-4", "name": "已解决"},
        {"id": "lst-beta-5", "name": "已完成"},
    ],
}

# 任务对象的两个维度别混：
#   tfsId   = 工作流状态（TBHub 的阶段）
#   stageId = 任务列表（看板列）

# projectId -> 任务类型(sfc) 列表。skill 是「任务类型」维度，不是阶段。
SFCS = {
    "p-alpha": [
        {"id": "sfc-alpha-task", "name": "Task", "type": "default"},
    ],
    "p-beta": [
        {"id": "sfc-beta-task", "name": "Task", "type": "default"},
        {"id": "sfc-beta-skill", "name": "skill", "type": "normal"},
    ],
}

# taskId -> task
#   tfsId   = 工作流状态（阶段）
#   stageId = 任务列表（看板列）
TASKS = {
    "t-alpha-1": {
        "id": "t-alpha-1",
        "content": "修复登录页在窄屏下溢出",
        "note": "在 375px 宽度下顶部导航换行错乱，需要加断点。",
        "tfsId": "st-alpha-todo",
        "stageId": "lst-alpha-1",
        "created": "2026-10-20T09:00:00+08:00",
        "isDone": False,
        "isArchived": False,
    },
    "t-alpha-2": {
        "id": "t-alpha-2",
        "content": "给导出功能补单元测试",
        "note": "覆盖 CSV 和 XLSX 两条路径。",
        "tfsId": "st-alpha-todo",
        "stageId": "lst-alpha-1",
        "created": "2026-10-21T09:00:00+08:00",
        "isDone": False,
        "isArchived": False,
    },
    "t-alpha-3": {
        "id": "t-alpha-3",
        "content": "把 README 的安装章节改写",
        "note": "补充 Windows 下缺 Windows SDK 的说明。",
        "tfsId": "st-alpha-doing",
        "stageId": "lst-alpha-2",
        "created": "2026-10-19T09:00:00+08:00",
        "isDone": False,
        "isArchived": False,
    },
    "t-alpha-trash": {
        "id": "t-alpha-trash",
        "content": "已废弃的旧任务(在回收站)",
        "note": "不应被查到。",
        "tfsId": "st-alpha-todo",
        "stageId": "lst-alpha-1",
        "created": "2026-10-01T09:00:00+08:00",
        "isDone": False,
        "isArchived": True,
    },
    "t-beta-1": {
        "id": "t-beta-1",
        "content": "skill: 代码评审规范",
        "note": "# 代码评审规范\n\n1. 先看错误处理\n2. 再看边界条件\n3. 最后看命名",
        "tfsId": "st-beta-done",
        "stageId": "lst-beta-3",
        "sfcId": "sfc-beta-skill",
        "created": "2026-10-18T09:00:00+08:00",
        "isDone": True,
        "isArchived": False,
    },
    "t-beta-2": {
        "id": "t-beta-2",
        "content": "skill: 提交信息模板",
        "note": "# 提交信息模板\n\ntype(scope): subject",
        "tfsId": "st-beta-done",
        "stageId": "lst-beta-3",
        "sfcId": "sfc-beta-skill",
        "created": "2026-10-18T10:00:00+08:00",
        "isDone": True,
        "isArchived": False,
    },
    "t-beta-3": {
        "id": "t-beta-3",
        "content": "升级依赖到最新稳定版",
        "note": "注意 reqwest 的破坏性变更。",
        "tfsId": "st-beta-todo",
        "stageId": "lst-beta-1",
        "created": "2026-10-22T09:00:00+08:00",
        "isDone": False,
        "isArchived": False,
    },
    # 故意放在列2但状态是未完成：模拟「agent 已提问，等用户回复」。
    # tb claim 必须跳过它，否则会把正在等人的任务又领走。
    "t-beta-wait": {
        "id": "t-beta-wait",
        "content": "等你回复的任务",
        "note": "在列2但未完成，不该被 claim 领走。",
        "tfsId": "st-beta-todo",
        "stageId": "lst-beta-2",
        "created": "2026-10-17T09:00:00+08:00",
        "isDone": False,
        "isArchived": False,
    },
}

# taskId -> 评论列表
COMMENTS = {}

# 给每条任务补 projectId（按 id 前缀推断，t-alpha-1 -> p-alpha）
for _tid, _t in TASKS.items():
    _t.setdefault("projectId", "p-" + _tid.split("-")[1])


def text_result(obj):
    """MCP tools/call 的返回。tbhub 会优先尝试把 text 解析成 JSON。"""
    if isinstance(obj, str):
        payload = obj
    else:
        payload = json.dumps(obj, ensure_ascii=False)
    return {"content": [{"type": "text", "text": payload}]}


def stage_by_name(taskflow_id, name):
    for s in STAGES.get(taskflow_id, []):
        if s["name"] == name:
            return s
    return None


def stage_kind(tfs_id):
    for sts in STAGES.values():
        for s in sts:
            if s["id"] == tfs_id:
                return s.get("kind") or "unset"
    return "unset"


def list_by_name(pid, name):
    for l in TASKLISTS.get(pid, []):
        if l["name"] == name:
            return l
    return None


def now_iso():
    import datetime

    return (
        datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def apply_status(t, tfs_id):
    """改工作流状态，并按 kind 同步 isDone（只有 end 算完成）。"""
    t["tfsId"] = tfs_id
    t["isDone"] = stage_kind(tfs_id) == "end"
    return t


def handle_tool(name, args):
    a = args or {}

    if name == "teambition.project.query":
        # 复现真实服务端的参数校验
        if not (a.get("projectId") or a.get("tql") or a.get("my")):
            return {
                "isError": True,
                "content": [{
                    "type": "text",
                    "text": json.dumps({
                        "businessCode": "INVALID_ARGUMENT",
                        "category": "argument_validation",
                        "message": "projectId, tql or my=true is required",
                        "nextAction": "correct_arguments",
                        "retryable": False,
                    }, ensure_ascii=False),
                }],
            }
        pid = a.get("projectId")
        if pid:
            ps = [p for p in PROJECTS if p["id"] == pid]
        else:
            ps = list(PROJECTS)
        # 分页
        size = int(a.get("pageSize") or 100)
        tok = a.get("pageToken")
        start = int(tok) if tok else 0
        page = ps[start:start + size]
        nxt = str(start + size) if start + size < len(ps) else None
        out = {"projects": page}
        if nxt:
            out["pageToken"] = nxt
        return text_result(out)

    if name == "teambition.project.get":
        pid = a.get("projectId") or a.get("project_id") or a.get("id")
        for p in PROJECTS:
            if p["id"] == pid:
                return text_result(p)
        return {"isError": True, "content": [{"type": "text", "text": "项目不存在"}]}

    if name == "teambition.taskflow.query":
        pid = a.get("projectId") or a.get("project_id")
        for p in PROJECTS:
            if p["id"] == pid:
                return text_result(
                    {"taskflows": [{"id": p["taskflowId"], "name": "默认任务流"}]}
                )
        return text_result({"taskflows": []})

    if name == "teambition.taskflow.get":
        tf = a.get("taskflowId") or a.get("taskflow_id")
        if tf in STAGES:
            return text_result(
                {"id": tf, "name": "默认任务流", "taskflowstatuses": STAGES[tf]}
            )
        return {"isError": True, "content": [{"type": "text", "text": "任务流不存在"}]}

    if name == "teambition.taskflowstatus.query":
        tf = a.get("taskflowId") or a.get("taskflow_id")
        return text_result({"taskflowstatuses": STAGES.get(tf, [])})

    if name == "teambition.taskflowstatus.create":
        tf = a.get("taskflowId") or a.get("taskflow_id")
        nm = a.get("name") or a.get("title")
        kd = a.get("kind") or "end"
        lst = STAGES.setdefault(tf, [])
        if not any(s["name"] == nm for s in lst):
            lst.append({"id": "st-%s" % uuid.uuid4().hex[:8], "name": nm, "kind": kd})
        return text_result({"id": lst[-1]["id"], "name": nm})

    if name == "teambition.sfc.query":
        pid = a.get("projectId") or a.get("project_id")
        return text_result({"sfcs": SFCS.get(pid, [])})

    if name == "teambition.sfc.create":
        pid = a.get("projectId") or a.get("project_id")
        nm = a.get("name") or a.get("title")
        lst = SFCS.setdefault(pid, [])
        exist = next((s for s in lst if s["name"] == nm), None)
        if exist:
            return text_result(exist)
        item = {"id": "sfc-%s" % uuid.uuid4().hex[:8], "name": nm, "type": "normal"}
        lst.append(item)
        return text_result(item)

    if name == "teambition.task.query":
        # 真实服务端：查询走 TQL，stageId/taskflowstatusId/projectId 是 TQL 字段
        tql = a.get("tql") or ""
        pid = a.get("projectId")
        if not tql and not pid:
            return {
                "isError": True,
                "content": [{
                    "type": "text",
                    "text": json.dumps({
                        "businessCode": "INVALID_ARGUMENT",
                        "category": "argument_validation",
                        "message": "tql or projectId is required",
                        "retryable": False,
                    }, ensure_ascii=False),
                }],
            }
        import re as _re
        out = list(TASKS.values())
        # 工作流状态（阶段）：taskflowstatusId / tfsId
        m = _re.search(r"taskflowstatusId\s*=\s*'([^']+)'", tql) or _re.search(
            r"tfsId\s*=\s*'([^']+)'", tql
        )
        if m:
            out = [t for t in out if t.get("tfsId") == m.group(1)]
        # 任务列表（看板列）：stageId
        m = _re.search(r"stageId\s*=\s*'([^']+)'", tql)
        if m:
            out = [t for t in out if t.get("stageId") == m.group(1)]
        # 按任务 id 精查：真实服务端接受 id / _id，不接受 taskId
        # 前面的否定环视避免把 stageId / taskflowstatusId 误当成 id
        m = _re.search(r"(?<![A-Za-z_])_?id\s*=\s*'([^']+)'", tql)
        if m:
            out = [t for t in out if t.get("id") == m.group(1)]
        if pid:
            out = [t for t in out if t.get("projectId") == pid]
        if "isArchived = false" in tql:
            out = [t for t in out if not t.get("isArchived")]
        elif "isArchived = true" in tql:
            out = [t for t in out if t.get("isArchived")]
        size = int(a.get("pageSize") or 100)
        tok = a.get("nextPageToken") or a.get("pageToken")
        start = int(tok) if tok else 0
        page = out[start:start + size]
        nxt = str(start + size) if start + size < len(out) else None
        res = {"tasks": page}
        if nxt:
            res["nextPageToken"] = nxt
        return text_result(res)

    if name == "teambition.task.get":
        tid = a.get("taskId") or a.get("task_id") or a.get("id")
        if tid in TASKS:
            return text_result(TASKS[tid])
        return {"isError": True, "content": [{"type": "text", "text": "任务不存在"}]}

    if name == "teambition.task.create":
        pid = a.get("projectId")
        # 状态：taskflowstatusId / tfsName；没给就落 kind=start 的那个状态
        tfs = a.get("taskflowstatusId")
        if not tfs and a.get("tfsName"):
            for p in PROJECTS:
                if p["id"] == pid:
                    st = stage_by_name(p["taskflowId"], a["tfsName"])
                    tfs = st["id"] if st else None
        if not tfs:
            for p in PROJECTS:
                if p["id"] == pid:
                    for s in STAGES.get(p["taskflowId"], []):
                        if s.get("kind") == "start":
                            tfs = s["id"]
                            break
        title = a.get("content") or a.get("title") or a.get("name") or "未命名"
        body = a.get("note") or a.get("description") or ""
        sfc = a.get("sfcId") or a.get("sfc_id") or ""
        tid = "t-%s" % uuid.uuid4().hex[:8]
        # 新建任务落默认列（列1），跟真实服务端一致
        default_list = list_by_name(pid, "1")
        TASKS[tid] = {
            "id": tid,
            "content": title,
            "note": body,
            "projectId": pid,
            "tfsId": tfs,
            "stageId": default_list["id"] if default_list else "",
            "sfcId": sfc,
            "created": "2026-10-23T12:00:00+08:00",
            "isDone": stage_kind(tfs) == "end",
            "isArchived": False,
        }
        return text_result({"id": tid, "content": title})

    if name == "teambition.task.update":
        tid = a.get("taskId") or a.get("task_id") or a.get("id")
        if tid not in TASKS:
            return {"isError": True, "content": [{"type": "text", "text": "任务不存在"}]}
        t = TASKS[tid]
        if a.get("content"):
            t["content"] = a["content"]
        elif a.get("title"):
            t["content"] = a["title"]
        if a.get("note"):
            t["note"] = a["note"]
        if a.get("taskflowstatusId"):
            apply_status(t, a["taskflowstatusId"])
        if a.get("stageId"):
            t["stageId"] = a["stageId"]
        return text_result({"id": tid, "content": t["content"]})

    if name == "teambition.task.update-status":
        tid = a.get("taskId") or a.get("id")
        if tid not in TASKS:
            return {"isError": True, "content": [{"type": "text", "text": "任务不存在"}]}
        t = TASKS[tid]
        tfs = a.get("taskflowstatusId")
        if not tfs and a.get("tfsName"):
            pid = t.get("projectId")
            for p in PROJECTS:
                if p["id"] == pid:
                    st = stage_by_name(p["taskflowId"], a["tfsName"])
                    tfs = st["id"] if st else None
        if not tfs:
            return {
                "isError": True,
                "content": [{"type": "text", "text": "需要 taskflowstatusId 或 tfsName"}],
            }
        apply_status(t, tfs)
        return text_result({"id": tid, "tfsId": tfs, "isDone": t["isDone"]})

    if name == "teambition.task.move":
        tid = a.get("taskId") or a.get("task_id") or a.get("id")
        if tid not in TASKS:
            return {"isError": True, "content": [{"type": "text", "text": "任务不存在"}]}
        t = TASKS[tid]
        tfs = a.get("taskflowstatusId")
        if not tfs and a.get("tfsName"):
            pid = t.get("projectId")
            for p in PROJECTS:
                if p["id"] == pid:
                    st = stage_by_name(p["taskflowId"], a["tfsName"])
                    tfs = st["id"] if st else None
        if tfs:
            apply_status(t, tfs)
        # 只有明确是任务列表 id 时才改列
        sid = a.get("stageId")
        if sid and any(l["id"] == sid for ls in TASKLISTS.values() for l in ls):
            t["stageId"] = sid
        return text_result({"id": tid, "tfsId": t.get("tfsId"), "stageId": t.get("stageId")})

    if name == "teambition.project.task-list-query":
        pid = a.get("projectId")
        return text_result({"result": TASKLISTS.get(pid, [])})

    if name == "teambition.task.update-stage":
        tid = a.get("taskId") or a.get("id")
        if tid not in TASKS:
            return {"isError": True, "content": [{"type": "text", "text": "任务不存在"}]}
        t = TASKS[tid]
        pid = a.get("projectId") or t.get("projectId")
        lid = a.get("stageId")
        if not lid and a.get("stageName"):
            l = list_by_name(pid, a["stageName"])
            if not l:
                return {
                    "isError": True,
                    "content": [{"type": "text", "text": "任务列表不存在: %s" % a["stageName"]}],
                }
            lid = l["id"]
        if not lid:
            return {
                "isError": True,
                "content": [{"type": "text", "text": "需要 stageId 或 stageName"}],
            }
        if not any(l["id"] == lid for ls in TASKLISTS.values() for l in ls):
            return {"isError": True, "content": [{"type": "text", "text": "任务列表 id 无效"}]}
        t["stageId"] = lid
        return text_result({"id": tid, "stageId": lid})

    if name == "teambition.task.update-list":
        tid = a.get("taskId") or a.get("id")
        if tid not in TASKS:
            return {"isError": True, "content": [{"type": "text", "text": "任务不存在"}]}
        t = TASKS[tid]
        pid = t.get("projectId")
        lid = a.get("taskListId")
        if a.get("default") and not lid:
            l = list_by_name(pid, "1")
            lid = l["id"] if l else None
        if not lid:
            return {
                "isError": True,
                "content": [{"type": "text", "text": "需要 taskListId 或 default"}],
            }
        t["stageId"] = lid
        return text_result({"id": tid, "stageId": lid})

    if name == "teambition.task.comment":
        tid = a.get("taskId") or a.get("id")
        if tid not in TASKS:
            return {"isError": True, "content": [{"type": "text", "text": "任务不存在"}]}
        cid = "cm-%s" % uuid.uuid4().hex[:8]
        ts = now_iso()
        item = {
            "id": cid,
            "action": "comment",
            "creatorId": "u-me",
            "creatorName": "我",
            "createTime": ts,
            "content": json.dumps(
                {"comment": a.get("content") or "", "renderMode": a.get("renderMode") or ""},
                ensure_ascii=False,
            ),
        }
        COMMENTS.setdefault(tid, []).append(item)
        return text_result(
            {"id": cid, "createTime": ts, "boundToObjectId": tid, "content": {"comment": a.get("content")}}
        )

    if name == "teambition.task.activity":
        tid = a.get("taskId") or a.get("id")
        if tid not in TASKS:
            return {"isError": True, "content": [{"type": "text", "text": "任务不存在"}]}
        want = a.get("actions")
        out = list(COMMENTS.get(tid, []))
        # actions 为空字符串 = 查全部；给了值就只留对应类型
        if want:
            kinds = {x.strip() for x in want.split(",") if x.strip()}
            out = [c for c in out if c["action"] in kinds]
        return text_result({"result": out})

    return {"isError": True, "content": [{"type": "text", "text": "未知工具: %s" % name}]}


TOOLS = [
    {
        "name": "teambition.project.query",
        "description": "列出企业下的所有项目",
        "inputSchema": {
            "type": "object",
            "properties": {"enterpriseId": {"type": "string", "description": "企业 ID"}},
        },
    },
    {
        "name": "teambition.project.get",
        "description": "获取项目详情",
        "inputSchema": {
            "type": "object",
            "properties": {"projectId": {"type": "string", "description": "项目 ID"}},
            "required": ["projectId"],
        },
    },
    {
        "name": "teambition.taskflow.query",
        "description": "列出项目下的任务流",
        "inputSchema": {
            "type": "object",
            "properties": {"projectId": {"type": "string", "description": "项目 ID"}},
            "required": ["projectId"],
        },
    },
    {
        "name": "teambition.taskflow.get",
        "description": "获取任务流详情",
        "inputSchema": {
            "type": "object",
            "properties": {"taskflowId": {"type": "string", "description": "任务流 ID"}},
            "required": ["taskflowId"],
        },
    },
    {
        "name": "teambition.taskflowstatus.query",
        "description": "列出任务流下的所有阶段",
        "inputSchema": {
            "type": "object",
            "properties": {"taskflowId": {"type": "string", "description": "任务流 ID"}},
            "required": ["taskflowId"],
        },
    },
    {
        "name": "teambition.taskflowstatus.create",
        "description": "在任务流下创建一个阶段",
        "inputSchema": {
            "type": "object",
            "properties": {
                "taskflowId": {"type": "string", "description": "任务流 ID"},
                "projectId": {"type": "string", "description": "项目 ID"},
                "name": {"type": "string", "description": "阶段名称"},
                "title": {"type": "string", "description": "阶段标题"},
                "kind": {"type": "string", "description": "状态类型: start/unset/end"},
            },
            "required": ["taskflowId", "name"],
        },
    },
    {
        "name": "teambition.sfc.query",
        "description": "列出项目的任务类型(sfc)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "projectId": {"type": "string", "description": "项目 ID"},
                "taskflowId": {"type": "string", "description": "任务流 ID"},
            },
        },
    },
    {
        "name": "teambition.sfc.create",
        "description": "在项目下创建任务类型(sfc)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "projectId": {"type": "string", "description": "项目 ID"},
                "taskflowId": {"type": "string", "description": "任务流 ID"},
                "name": {"type": "string", "description": "任务类型名称"},
            },
            "required": ["projectId", "name"],
        },
    },
    {
        "name": "teambition.task.query",
        "description": "列出阶段下的任务",
        "inputSchema": {
            "type": "object",
            "properties": {
                "projectId": {"type": "string", "description": "项目 ID"},
                "stageId": {"type": "string", "description": "阶段 ID"},
                "taskflowstatusId": {"type": "string", "description": "阶段 ID"},
                "tql": {"type": "string", "description": "TQL 查询表达式"},
                "pageSize": {"type": "integer", "description": "每页数量"},
                "nextPageToken": {"type": "string", "description": "分页标记"},
            },
        },
    },
    {
        "name": "teambition.task.get",
        "description": "获取任务详情",
        "inputSchema": {
            "type": "object",
            "properties": {"taskId": {"type": "string", "description": "任务 ID"}},
            "required": ["taskId"],
        },
    },
    {
        "name": "teambition.task.create",
        "description": "在阶段下创建任务",
        "inputSchema": {
            "type": "object",
            "properties": {
                "projectId": {"type": "string", "description": "项目 ID"},
                "stageId": {"type": "string", "description": "阶段 ID"},
                "taskflowstatusId": {"type": "string", "description": "阶段 ID"},
                "tfsName": {"type": "string", "description": "阶段名称"},
                "sfcId": {"type": "string", "description": "任务类型 ID"},
                "title": {"type": "string", "description": "任务标题"},
                "content": {"type": "string", "description": "任务内容"},
                "note": {"type": "string", "description": "备注"},
            },
            "required": ["stageId", "title"],
        },
    },
    {
        "name": "teambition.task.update",
        "description": "更新任务",
        "inputSchema": {
            "type": "object",
            "properties": {
                "taskId": {"type": "string", "description": "任务 ID"},
                "id": {"type": "string", "description": "任务 ID"},
                "title": {"type": "string", "description": "任务标题"},
                "content": {"type": "string", "description": "任务内容"},
                "note": {"type": "string", "description": "备注"},
                "stageId": {"type": "string", "description": "阶段 ID"},
                "taskflowstatusId": {"type": "string", "description": "阶段 ID"},
            },
            "required": ["taskId"],
        },
    },
    {
        "name": "teambition.task.move",
        "description": "把任务移动到另一个阶段",
        "inputSchema": {
            "type": "object",
            "properties": {
                "taskId": {"type": "string", "description": "任务 ID"},
                "id": {"type": "string", "description": "任务 ID"},
                "stageId": {"type": "string", "description": "阶段 ID"},
                "taskflowstatusId": {"type": "string", "description": "阶段 ID"},
                "tfsName": {"type": "string", "description": "阶段名称"},
            },
            "required": ["taskId", "stageId"],
        },
    },
    {
        "name": "teambition.task.update-status",
        "description": "更新任务的工作流状态",
        "inputSchema": {
            "type": "object",
            "properties": {
                "taskId": {"type": "string", "description": "任务 ID"},
                "taskflowstatusId": {"type": "string", "description": "状态 ID"},
                "tfsName": {"type": "string", "description": "状态名称"},
            },
            "required": ["taskId"],
        },
    },
    {
        "name": "teambition.project.task-list-query",
        "description": "查询项目下的任务列表(看板列)",
        "inputSchema": {
            "type": "object",
            "properties": {
                "projectId": {"type": "string", "description": "项目 ID"},
                "name": {"type": "string", "description": "列名"},
            },
            "required": ["projectId"],
        },
    },
    {
        "name": "teambition.task.update-stage",
        "description": "按名称或 ID 将任务调整到项目内指定任务列表",
        "inputSchema": {
            "type": "object",
            "properties": {
                "taskId": {"type": "string", "description": "任务 ID"},
                "projectId": {"type": "string", "description": "项目 ID"},
                "stageId": {"type": "string", "description": "目标任务列表 ID"},
                "stageName": {"type": "string", "description": "目标任务列表名称"},
            },
            "required": ["taskId"],
        },
    },
    {
        "name": "teambition.task.update-list",
        "description": "按列表 ID 或默认列表移动任务",
        "inputSchema": {
            "type": "object",
            "properties": {
                "taskId": {"type": "string", "description": "任务 ID"},
                "taskListId": {"type": "string", "description": "目标任务列表 ID"},
                "default": {"type": "boolean", "description": "移到默认列表"},
            },
            "required": ["taskId"],
        },
    },
    {
        "name": "teambition.task.comment",
        "description": "为任务发表评论",
        "inputSchema": {
            "type": "object",
            "properties": {
                "taskId": {"type": "string", "description": "任务 ID"},
                "content": {"type": "string", "description": "评论内容"},
                "renderMode": {"type": "string", "description": "渲染模式"},
            },
            "required": ["taskId", "content"],
        },
    },
    {
        "name": "teambition.task.activity",
        "description": "获取任务动态列表（评论、状态变更等）",
        "inputSchema": {
            "type": "object",
            "properties": {
                "taskId": {"type": "string", "description": "任务 ID"},
                "actions": {"type": "string", "description": "过滤动态类型，默认只返回评论"},
            },
            "required": ["taskId"],
        },
    },
]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("[mock] %s\n" % (fmt % args))

    def _send(self, obj, status=200):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("mcp-session-id", "mock-session-1")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            req = json.loads(raw.decode("utf-8"))
        except Exception as e:
            self._send(
                {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": str(e)}},
                400,
            )
            return

        # 鉴权：tbhub 会带 bearer token，随便校验一下非空
        auth = self.headers.get("Authorization") or ""
        if not auth.startswith("Bearer ") or len(auth) < 10:
            self._send(
                {
                    "jsonrpc": "2.0",
                    "id": req.get("id"),
                    "error": {"code": -32001, "message": "missing token"},
                },
                401,
            )
            return

        method = req.get("method")
        rid = req.get("id")

        # 通知没有 id，不回包
        if rid is None:
            self._send({"jsonrpc": "2.0"} if False else {"ok": True})
            return

        if method == "initialize":
            self._send(
                {
                    "jsonrpc": "2.0",
                    "id": rid,
                    "result": {
                        "protocolVersion": "2025-03-26",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "mock-teambition", "version": "0.0.1"},
                    },
                }
            )
            return

        if method == "tools/list":
            self._send({"jsonrpc": "2.0", "id": rid, "result": {"tools": TOOLS}})
            return

        if method == "tools/call":
            params = req.get("params") or {}
            name = params.get("name")
            args = params.get("arguments") or {}
            sys.stderr.write("[mock] call %s %s\n" % (name, json.dumps(args, ensure_ascii=False)))
            self._send({"jsonrpc": "2.0", "id": rid, "result": handle_tool(name, args)})
            return

        self._send(
            {
                "jsonrpc": "2.0",
                "id": rid,
                "error": {"code": -32601, "message": "method not found: %s" % method},
            }
        )


if __name__ == "__main__":
    # 必须多线程：单线程版开了 keep-alive 就卡住，不再接受新连接
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    srv.daemon_threads = True
    sys.stderr.write("mock teambition mcp listening on http://127.0.0.1:%d/mcp\n" % PORT)
    srv.serve_forever()
