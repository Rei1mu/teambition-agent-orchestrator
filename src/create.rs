//! 项目接入：扫候选名 -> 用户拍板 -> 建/复用项目 -> 补状态 -> 写本地绑定 -> 补 AGENTS.md -> 开工文案。
//!
//! CLI（`tba create`）和窗口「接入项目」页共用这一份，避免两边行为漂移。
//! 以前这条链路只存在于「复制项目初始化文案」那段给 agent 读的文字里；
//! 现在固化成命令，跑完就直接可用。

use crate::ops::{self, Lang};
use crate::store::Store;
use std::path::{Path, PathBuf};

/// 一个候选项目名 + 来源 + Teambition 里的同名情况。
#[derive(Clone, Debug)]
pub struct Row {
    pub name: String,
    pub source: String,
    pub mark: String,
}

/// 扫目录列候选。不建项目、不写任何文件。
/// 联网时会核对 Teambition 是否已有同名；连不上（或离线）标「未核对」，不拦流程。
pub fn scan(store: &Store, dir: &Path, given: Option<&str>) -> Vec<Row> {
    let list = match given.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => vec![crate::naming::Candidate {
            name: n.to_string(),
            source: "命令行 --name".to_string(),
        }],
        None => crate::naming::candidates(dir),
    };

    let existing: Option<Vec<(String, String)>> = if store.load_config().offline() {
        None
    } else {
        ops::Ctx::new(store.clone(), false)
            .and_then(|mut c| c.list_projects())
            .ok()
            .map(|all| {
                all.iter()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect()
            })
    };

    list.into_iter()
        .map(|c| {
            let mark = match &existing {
                None => "未核对（连不上 Teambition / 离线）".to_string(),
                Some(all) => match all.iter().filter(|(_, n)| n == &c.name).count() {
                    0 => "Teambition 里没有，会新建".to_string(),
                    1 => "已有同名，会复用".to_string(),
                    n => format!("有 {n} 个同名，需要 --project 指定"),
                },
            };
            Row {
                name: c.name,
                source: c.source,
                mark,
            }
        })
        .collect()
}

pub struct Created {
    pub project_id: String,
    pub project_name: String,
    pub created: bool,
    pub binding: PathBuf,
    pub entry: Option<PathBuf>,
    pub prompt: String,
    /// 查到的看板列名。None = 没查到（工具缺失 / 接口失败），别当成「一列都没有」误报。
    pub tasklists: Option<Vec<String>>,
}

impl Created {
    /// CLI 和窗口都打印这一份，避免两边说法不一致。
    pub fn report(&self) -> String {
        let mut s = format!(
            "项目就绪: {}\t{}{}\n工作流状态: 7 个已补齐\n本地绑定: {}\n",
            self.project_id,
            self.project_name,
            if self.created { "（新建）" } else { "（复用）" },
            self.binding.display()
        );
        match &self.entry {
            Some(p) => s.push_str(&format!("AGENTS.md 入口: {}\n", p.display())),
            None => s.push_str("AGENTS.md 入口: 已是最新，未改动\n"),
        }
        // 项目链接 + 1~5 五列引导：CLI 和窗口都从这一份出，别让人建完项目找不到入口。
        s.push_str(&ops::tasklist_guidance(
            &self.project_id,
            self.tasklists.as_deref(),
        ));
        s.push_str("\n===== 开工文案（发给 agent，或直接照着跑） =====\n\n");
        s.push_str(&self.prompt);
        s
    }
}

/// 用拍板后的名字建/复用项目，并把 dir 接上。
/// 所有写盘都落在 dir，不是进程 cwd —— 窗口可以选任意目录。
pub fn create(
    store: &Store,
    dir: &Path,
    name: &str,
    project: Option<&str>,
    lang: Lang,
) -> Result<Created, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("项目名不能为空".to_string());
    }
    let mut ctx = ops::Ctx::new(store.clone(), false)?;
    let (p, created) = ctx.ensure_project(Some(name), project, true)?;
    ctx.ensure_stages(&p.id)?;
    for aux in [crate::config::SKILL_STATE, crate::config::MEMORY_STATE] {
        let _ = ctx.ensure_state(&p.id, aux);
    }
    if let Ok(all) = ctx.list_projects() {
        let _ = store.save_projects_cache(
            &all.iter()
                .map(|x| (x.id.clone(), x.name.clone()))
                .collect::<Vec<_>>(),
        );
    }
    // 看板列网页 API 只能查不能建：复用上面这个 ctx 顺手查一次（不额外握手），
    // 把「现在有哪些列 / 还缺哪几列」带进 report()，好让人去网页手动补 1~5。
    let tasklists = ctx
        .task_lists(&p.id)
        .ok()
        .map(|v| v.into_iter().map(|l| l.name).collect::<Vec<_>>());
    let binding = ops::write_project_binding_at(dir, &p.id)?;
    let entry = ops::ensure_entry_in(store, dir);
    // 开工文案是附赠：新项目可能还没有 skill 缓存，生成失败不该把「已建好」变成报错。
    let prompt = ops::agent_prompt(store, &p.id, &p.name, None, lang)
        .unwrap_or_else(|e| format!("（开工文案暂时生成不了：{e}。稍后跑 `tba prompt` 即可。）\n"));
    Ok(Created {
        project_id: p.id,
        project_name: p.name,
        created,
        binding,
        entry,
        prompt,
        tasklists,
    })
}
