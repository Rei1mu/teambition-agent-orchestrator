//! 通用能力：git / GitButler / 发版。
//!
//! 存在的理由：agent 每次「提交推送」「发版」都要现场摸索一遍——
//! 分支叫什么、远端是哪个、GitButler 工作区要不要用 `but`、CI 跑没跑完、产物叫什么。
//! 这些答案全都写在仓库里，但 agent 得花几十秒去挖，还可能挖错。
//! 固化成命令后，agent 只需要一条命令 + 看退出码。
//!
//! 覆盖：
//!   tba git status          分支 / 脏文件 / 远端领先落后 / 是不是 GitButler 项目
//!   tba git main            提交并推送。GitButler 项目走 but，普通仓库走 git
//!   tba git tag <tag>       打 tag 并推
//!   tba publish             打版本 tag + 推 + (--watch) 等 CI + 列产物
//!   tba publish --init      生成 windows x64 + arm64(MSVC) 的 release workflow
//!
//! 约定：所有函数只在 stdout 打印人能读的一行行文字，错误走 Err。
//! 联网只用于 --watch 查 CI，拿不到 token 就降级成「你自己去 Actions 页看」。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub struct Out {
    pub code: i32,
    pub out: String,
    pub err: String,
}

fn run_in(dir: &Path, prog: &str, args: &[&str]) -> Result<Out, String> {
    let o = Command::new(prog)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("起不来 {prog}: {e}"))?;
    Ok(Out {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).trim().to_string(),
        err: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    })
}

fn git(dir: &Path, args: &[&str]) -> Result<Out, String> {
    run_in(dir, "git", args)
}

fn but(dir: &Path, args: &[&str]) -> Result<Out, String> {
    run_in(dir, but_prog(), args)
}

fn but_prog() -> &'static str {
    if cfg!(windows) {
        "but.exe"
    } else {
        "but"
    }
}

// 仓库根。不在 git 仓库里就直接报错，别让后面的命令在错的目录里跑。
pub fn repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("取当前目录失败: {e}"))?;
    let r = git(&cwd, &["rev-parse", "--show-toplevel"])?;
    if r.code != 0 || r.out.is_empty() {
        return Err(format!(
            "当前目录不在 git 仓库里: {}\n（先 git init，或者 cd 到项目根）",
            cwd.display()
        ));
    }
    Ok(PathBuf::from(r.out))
}

// 是不是 GitButler 项目。判据是 `but status --json` 能跑通——
// 比看 .git/but 目录靠谱：`but setup` 之后目录结构会变，命令不会骗人。
pub fn is_gb_project(dir: &Path) -> bool {
    // 注意：不能只看 code == 0。目录没 setup 时 but 也返回 0，错误在 stdout 的 JSON 里。
    match but(dir, &["status", "--json"]) {
        Ok(o) => matches!(
            crate::gitbutler::project_setup_from(o.code == 0, &o.out, &o.err),
            Some(true)
        ),
        Err(_) => false,
    }
}

fn porcelain(dir: &Path) -> Result<(String, Vec<String>), String> {
    let r = git(dir, &["status", "--porcelain=v1", "-b"])?;
    if r.code != 0 {
        return Err(format!("git status 失败: {}", r.err));
    }
    let mut lines = r.out.lines();
    let head = lines.next().unwrap_or("").to_string();
    let files: Vec<String> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();
    Ok((head, files))
}

// `## main...origin/main [ahead 2, behind 1]` -> ("main", ahead, behind, upstream)
fn parse_head(head: &str) -> (String, i64, i64, String) {
    let body = head.trim_start_matches("## ").trim();
    let (left, rest) = match body.split_once("...") {
        Some((a, b)) => (a, b),
        None => (body, ""),
    };
    let branch = left.split_whitespace().next().unwrap_or("").to_string();
    let mut ahead = 0i64;
    let mut behind = 0i64;
    if let Some(i) = rest.find('[') {
        if let Some(j) = rest[i..].find(']') {
            for part in rest[i + 1..i + j].split(',') {
                let p = part.trim();
                if let Some(n) = p.strip_prefix("ahead ") {
                    ahead = n.trim().parse().unwrap_or(0);
                } else if let Some(n) = p.strip_prefix("behind ") {
                    behind = n.trim().parse().unwrap_or(0);
                }
            }
        }
    }
    let upstream = rest.split_whitespace().next().unwrap_or("").to_string();
    (branch, ahead, behind, upstream)
}

pub fn status() -> Result<String, String> {
    let root = repo_root()?;
    let (head, files) = porcelain(&root)?;
    let (branch, ahead, behind, upstream) = parse_head(&head);
    let mut s = String::new();
    s.push_str(&format!("仓库: {}\n", root.display()));
    s.push_str(&format!(
        "分支: {}\n",
        if branch.is_empty() { "（游离 HEAD）" } else { &branch }
    ));
    s.push_str(&format!(
        "GitButler: {}\n",
        if is_gb_project(&root) { "是（推送用 but）" } else { "否（推送用 git）" }
    ));
    if upstream.is_empty() {
        s.push_str("远端: 没有 upstream\n");
    } else {
        s.push_str(&format!(
            "远端: {upstream}（领先 {ahead} / 落后 {behind}）\n"
        ));
    }
    if files.is_empty() {
        s.push_str("工作区: 干净\n");
    } else {
        s.push_str(&format!("工作区: {} 个改动\n", files.len()));
        for f in files.iter().take(20) {
            s.push_str(&format!("  {f}\n"));
        }
        if files.len() > 20 {
            s.push_str(&format!("  ... 还有 {} 个\n", files.len() - 20));
        }
    }
    Ok(s.trim_end().to_string())
}

// 默认分支名：优先 origin/HEAD 指向的那个，再退回 main / master。
fn default_branch(root: &Path) -> String {
    if let Ok(o) = git(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        if o.code == 0 {
            if let Some(b) = o.out.rsplit('/').next() {
                if !b.is_empty() {
                    return b.to_string();
                }
            }
        }
    }
    for b in ["main", "master"] {
        if let Ok(o) = git(root, &["rev-parse", "--verify", &format!("refs/heads/{b}")]) {
            if o.code == 0 {
                return b.to_string();
            }
        }
    }
    "main".to_string()
}

const DEFAULT_MSG: &str = "chore: 提交改动";

// 提交并推送到目标分支（默认 main）。
//
// 两条路，靠 `but status` 自动判断：
//   GitButler 项目：but commit -> but push（--land 再 but land 落到目标分支）
//   普通仓库：git add -A -> git commit -> git push origin HEAD:<branch>
// 为什么不统一走 git：GitButler 的改动挂在虚拟分支上，裸 git commit 会让工作区
// 在 GitButler 里显示成「游离提交」，用户下次打开 GUI 会看到一堆怪东西。
pub fn main_push(
    message: Option<&str>,
    branch: Option<&str>,
    land: bool,
    dry: bool,
) -> Result<String, String> {
    let root = repo_root()?;
    let msg = message.map(str::trim).filter(|m| !m.is_empty()).unwrap_or(DEFAULT_MSG);
    let gb = is_gb_project(&root);
    let target = branch
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| default_branch(&root));
    let (head, files) = porcelain(&root)?;
    let (cur, _, _, _) = parse_head(&head);

    if dry {
        return Ok(if gb {
            format!(
                "[dry] GitButler 项目，将执行：\n  but commit -m \"{msg}\"\n  but push{}\n改动 {} 个",
                if land { "\n  but land" } else { "" },
                files.len()
            )
        } else {
            format!(
                "[dry] 将执行：\n  git add -A\n  git commit -m \"{msg}\"\n  git push origin HEAD:{target}\n改动 {} 个，当前分支 {cur}",
                files.len()
            )
        });
    }

    let mut log = String::new();
    if gb {
        if !files.is_empty() {
            let r = but(&root, &["commit", "-m", msg])?;
            if r.code != 0 {
                return Err(format!("but commit 失败: {}", tail(&r)));
            }
            log.push_str(&format!("已提交 {} 个改动（GitButler）\n", files.len()));
        } else {
            log.push_str("没有未提交改动，跳过提交\n");
        }
        let r = but(&root, &["push"])?;
        if r.code != 0 {
            return Err(format!(
                "but push 失败: {}\n（GitButler 需要先登录：but login 或在 GUI 里登录）",
                tail(&r)
            ));
        }
        log.push_str("已 but push\n");
        if land {
            let r = but(&root, &["land"])?;
            if r.code != 0 {
                return Err(format!("but land 失败: {}", tail(&r)));
            }
            log.push_str("已 but land 到目标分支\n");
        } else {
            log.push_str("提示: 要直接落到目标分支，加 --land（but land）\n");
        }
        return Ok(log.trim_end().to_string());
    }

    if !files.is_empty() {
        let r = git(&root, &["add", "-A"])?;
        if r.code != 0 {
            return Err(format!("git add 失败: {}", tail(&r)));
        }
        let r = git(&root, &["commit", "-m", msg])?;
        if r.code != 0 {
            return Err(format!("git commit 失败: {}", tail(&r)));
        }
        log.push_str(&format!("已提交 {} 个改动\n", files.len()));
    } else {
        log.push_str("没有未提交改动，跳过提交\n");
    }

    let spec = format!("HEAD:{target}");
    let r = git(&root, &["push", "origin", &spec])?;
    if r.code != 0 {
        return Err(format!(
            "git push 失败: {}\n（先 git pull --rebase 再重试，别 force push）",
            tail(&r)
        ));
    }
    log.push_str(&format!("已推送 {cur} -> origin/{target}\n"));
    if cur != target {
        log.push_str(&format!("提示: 本地分支是 {cur}，远端写的是 {target}\n"));
    }
    Ok(log.trim_end().to_string())
}

// 打 tag 并推送。tag 已存在就报错（不 force），除非明确要覆盖。
pub fn tag_push(name: &str, msg: Option<&str>, no_push: bool, force: bool) -> Result<String, String> {
    let root = repo_root()?;
    let tag = normalize_tag(name);
    let exists = git(&root, &["rev-parse", "--verify", &format!("refs/tags/{tag}")])?;
    if exists.code == 0 {
        if !force {
            return Err(format!(
                "tag {tag} 已存在。要重打请加 --force（会改写远端 tag，谨慎）"
            ));
        }
        let r = git(&root, &["tag", "-d", &tag])?;
        if r.code != 0 {
            return Err(format!("删旧 tag 失败: {}", tail(&r)));
        }
    }
    let m = msg.unwrap_or(&tag).to_string();
    let r = git(&root, &["tag", "-a", &tag, "-m", &m])?;
    if r.code != 0 {
        return Err(format!("打 tag 失败: {}", tail(&r)));
    }
    if no_push {
        return Ok(format!("已打 tag {tag}（--no-push，未推送）"));
    }
    let r = git(&root, &["push", "origin", &tag])?;
    if r.code != 0 {
        let force_hint = if force {
            "\n（要覆盖远端 tag 得显式 force：git push -f origin <tag>）"
        } else {
            ""
        };
        return Err(format!("推 tag 失败: {}{force_hint}", tail(&r)));
    }
    Ok(format!("已打 tag 并推送: {tag}"))
}

fn tag_exists(root: &Path, tag: &str) -> bool {
    matches!(
        git(root, &["rev-parse", "--verify", &format!("refs/tags/{tag}")]),
        Ok(o) if o.code == 0
    )
}

fn normalize_tag(s: &str) -> String {    let t = s.trim();
    if t.starts_with('v') || t.contains('-') || t.contains('/') {
        t.to_string()
    } else {
        format!("v{t}")
    }
}

fn tail(o: &Out) -> String {
    let s = if o.err.is_empty() { &o.out } else { &o.err };
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(6);
    lines[start..].join("\n")
}

// ---- 发版 ----

// 从 Cargo.toml 取版本号（只认第一个 version = "x.y.z"）。
fn cargo_version(root: &Path) -> Option<String> {
    let s = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    for line in s.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("version") {
            let v = v.trim_start().strip_prefix('=')?.trim();
            let v = v.trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

// origin 的 owner/repo。https 和 ssh 两种写法都认。
pub fn repo_slug(root: &Path) -> Option<String> {
    let o = git(root, &["remote", "get-url", "origin"]).ok()?;
    if o.code != 0 {
        return None;
    }
    let url = o.out.trim();
    let path = if let Some(rest) = url.strip_prefix("git@") {
        // git@github.com:owner/repo.git
        rest.split_once(':').map(|(_, p)| p.to_string())?
    } else {
        let i = url.find("github.com")?;
        url[i + "github.com".len()..].trim_start_matches('/').to_string()
    };
    let path = path.trim_end_matches(".git");
    let mut it = path.split('/');
    let owner = it.next()?;
    let name = it.next()?;
    if owner.is_empty() || name.is_empty() {
        None
    } else {
        Some(format!("{owner}/{name}"))
    }
}

fn github_token() -> Option<String> {
    for k in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    // 退回 git 凭据：用户 git push 能用，说明 token 在凭据管理器里。
    let mut c = Command::new("git")
        .args(["credential", "fill"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        let mut si = c.stdin.take()?;
        si.write_all(b"protocol=https\nhost=github.com\n\n").ok()?;
    }
    let out = c.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.strip_prefix("password="))
        .map(|s| s.trim().to_string())
}

fn api_get(url: &str, token: Option<&str>) -> Result<serde_json::Value, String> {
    let mut req = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("tba-cli")
        .build()
        .map_err(|e| format!("建 HTTP 客户端失败: {e}"))?
        .get(url)
        .header("Accept", "application/vnd.github+json");
    if let Some(t) = token {
        req = req.header("Authorization", format!("token {t}"));
    }
    let resp = req.send().map_err(|e| format!("请求失败: {e}"))?;
    let code = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    if code == 404 {
        return Err(format!("404：仓库或资源不存在，或 token 没权限（{url}）"));
    }
    if code >= 400 {
        return Err(format!("GitHub API {code}: {}", body.chars().take(300).collect::<String>()));
    }
    serde_json::from_str(&body).map_err(|e| format!("解析 JSON 失败: {e}"))
}

// 发版主流程。tag 缺省取 Cargo.toml 的版本号。
pub fn release(
    tag: Option<&str>,
    watch: bool,
    repo: Option<&str>,
    no_push: bool,
    timeout_secs: u64,
) -> Result<String, String> {
    let root = repo_root()?;
    let tag = match tag.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => normalize_tag(t),
        None => match cargo_version(&root) {
            Some(v) => format!("v{v}"),
            None => {
                return Err("没给 --tag，Cargo.toml 里也没读到 version".to_string());
            }
        },
    };
    // 刚推完 tag 时，CI 的 run 还没创建出来，第一次轮询会撞上「同名 tag 的上一次运行」
    // （比如重发同一个 tag），然后拿旧结论骗人。所以记下推送时刻，只认这之后的 run。
    let mut pushed_at: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut log = if tag_exists(&root, &tag) && watch && !no_push {
        // 打 tag 已经做过了（比如上次 --watch 等到超时），这时不该再报「tag 已存在」，
        // 直接接着看 CI——超时提示里让用户重跑的就是这条命令。
        // 这种情况下不加时间过滤：API 按创建时间倒序，第一条就是最新的。
        format!("tag {tag} 已存在，跳过打 tag，直接看 CI\n")
    } else {
        pushed_at = Some(chrono::Utc::now());
        let mut l = tag_push(&tag, None, no_push, false)?;
        l.push('\n');
        l
    };
    let slug = repo
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .or_else(|| repo_slug(&root));
    match &slug {
        Some(s) => log.push_str(&format!("Release 页: https://github.com/{s}/releases/tag/{tag}\n")),
        None => log.push_str("没解析出 origin，自己开仓库的 Releases 页看\n"),
    }
    if no_push || !watch {
        return Ok(log.trim_end().to_string());
    }
    let Some(slug) = slug else {
        return Ok(log.trim_end().to_string());
    };
    // 查 CI 失败不该让整条命令失败：tag 已经推上去了，agent 只需要知道「去 Actions 页看」。
    match watch_ci(&slug, &tag, timeout_secs, pushed_at) {
        Ok(s) => log.push_str(&s),
        Err(e) => log.push_str(&format!(
            "查 CI 失败: {e}\n（tag 已推成功，自己去 Actions 页看）"
        )),
    }
    Ok(log.trim_end().to_string())
}

// 轮询 CI 直到跑完 / 超时。
//
// 超时返回 Err 里的提示而不是死等：agent 的 shell 工具上限 600 秒，
// 卡在这里会连会话一起被砍。返回 exit 2 的语义由调用方给。
fn watch_ci(
    slug: &str,
    tag: &str,
    timeout_secs: u64,
    pushed_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<String, String> {
    let token = github_token();
    if token.is_none() {
        return Ok(
            "没有 GitHub token（环境变量 GH_TOKEN 或 git 凭据），看不到 CI 状态。\n\
             自己打开 Actions 页看，绿了再告诉用户下载地址。"
                .to_string(),
        );
    }
    let token = token.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs.max(30));
    let mut run_id: Option<u64> = None;
    let mut s = String::new();
    // 只在状态变化时输出：轮询 20 次打 20 行一样的「等 CI 触发」纯属噪音。
    let mut last_line = String::new();
    loop {
        if run_id.is_none() {
            let url = format!(
                "https://api.github.com/repos/{slug}/actions/runs?per_page=20&event=push"
            );
            let v = api_get(&url, Some(&token))?;
            let found = v["workflow_runs"]
                .as_array()
                .and_then(|a| {
                    a.iter().find(|r| {
                        if r["head_branch"].as_str() != Some(tag)
                            || r["event"].as_str() != Some("push")
                        {
                            return false;
                        }
                        // 只认本次推送之后创建的 run（留 90 秒时钟偏差余量）。
                        match (&pushed_at, r["created_at"].as_str()) {
                            (Some(t), Some(c)) => chrono::DateTime::parse_from_rfc3339(c)
                                .map(|d| d.with_timezone(&chrono::Utc) + chrono::Duration::seconds(90) >= *t)
                                .unwrap_or(true),
                            _ => true,
                        }
                    })
                })
                .cloned();
            if let Some(r) = found {
                run_id = r["id"].as_u64();
                s.push_str(&format!(
                    "CI 已触发: {}\n",
                    r["html_url"].as_str().unwrap_or("")
                ));
            } else if last_line != "waiting" {
                s.push_str("等 CI 触发...\n");
                last_line = "waiting".to_string();
            }
        }
        if let Some(id) = run_id {
            let v = api_get(
                &format!("https://api.github.com/repos/{slug}/actions/runs/{id}"),
                Some(&token),
            )?;
            let status = v["status"].as_str().unwrap_or("");
            let concl = v["conclusion"].as_str().unwrap_or("");
            if status == "completed" {
                let jobs = api_get(
                    &format!("https://api.github.com/repos/{slug}/actions/runs/{id}/jobs"),
                    Some(&token),
                )?;
                s.push_str(&format!("CI 结果: {concl}\n"));
                for j in jobs["jobs"].as_array().cloned().unwrap_or_default() {
                    s.push_str(&format!(
                        "  {} -> {}\n",
                        j["name"].as_str().unwrap_or("?"),
                        j["conclusion"].as_str().unwrap_or("?")
                    ));
                }
                if concl != "success" {
                    s.push_str("CI 没绿，别对外说「已发布」。看上面的 job 名去 Actions 页翻日志。\n");
                    return Ok(s.trim_end().to_string());
                }
                // 绿了才列产物
                match api_get(
                    &format!("https://api.github.com/repos/{slug}/releases/tags/{tag}"),
                    Some(&token),
                ) {
                    Ok(rel) => {
                        s.push_str("Release 产物:\n");
                        for a in rel["assets"].as_array().cloned().unwrap_or_default() {
                            let size = a["size"].as_u64().unwrap_or(0) as f64 / 1048576.0;
                            s.push_str(&format!(
                                "  {}  {:.2} MB\n",
                                a["name"].as_str().unwrap_or("?"),
                                size
                            ));
                        }
                    }
                    Err(e) => s.push_str(&format!("列产物失败: {e}\n")),
                }
                return Ok(s.trim_end().to_string());
            }
            let line = format!("CI 进行中: {status}");
            if last_line != line {
                s.push_str(&line);
                s.push('\n');
                last_line = line;
            }
        }
        if std::time::Instant::now() >= deadline {
            s.push_str(&format!(
                "等了 {} 秒还没跑完。别死等，稍后重跑 `tba publish --tag {tag} --watch` 看结果。",
                timeout_secs
            ));
            return Ok(s.trim_end().to_string());
        }
        std::thread::sleep(Duration::from_secs(15));
    }
}

// ---- 生成 release workflow ----
//
// 为什么内置：windows x64 + windows arm64(MSVC 交叉编译) + Inno Setup 打包 +
// 上传 Release，这套流程 agent 每次都要从零写，写完还得调 3 轮才绿。
pub fn workflow_yaml(exe: &str, iss: Option<&str>) -> String {
    let build_pack = match iss {
        Some(iss) => format!(
            r#"      - name: 装 Inno Setup
        run: choco install innosetup --no-progress -y

      - name: 打 setup.exe
        shell: pwsh
        run: |
          iscc "/DAppVersion=${{{{ steps.ver.outputs.version }}}}" `
               "/DTargetArch=${{{{ matrix.arch }}}}" `
               "/DInnoArch=${{{{ matrix.inno }}}}" `
               "/DSrcExe=..\target\${{{{ matrix.target }}}}\release\{exe}.exe" `
               "{iss}"

      - uses: actions/upload-artifact@v4
        with:
          name: setup-${{{{ matrix.arch }}}}
          path: dist/*.exe
          if-no-files-found: error
"#
        ),
        None => format!(
            r#"      - name: 打包 zip
        shell: pwsh
        run: |
          New-Item -ItemType Directory -Force dist | Out-Null
          Compress-Archive -Path "target/${{{{ matrix.target }}}}/release/{exe}.exe" `
            -DestinationPath "dist/{exe}-${{{{ steps.ver.outputs.version }}}}-${{{{ matrix.arch }}}}.zip"

      - uses: actions/upload-artifact@v4
        with:
          name: bin-${{{{ matrix.arch }}}}
          path: dist/*
          if-no-files-found: error
"#
        ),
    };
    let files = if iss.is_some() {
        "dist/*.exe"
    } else {
        "dist/*.zip"
    };
    format!(
        r#"name: release

# 打 v* tag 自动构建；手动触发只出 artifact，不发 Release。
on:
  push:
    tags: ["v*"]
  workflow_dispatch:

permissions:
  contents: write

jobs:
  build:
    name: build ${{{{ matrix.arch }}}}
    runs-on: windows-latest
    strategy:
      fail-fast: false
      matrix:
        include:
          # x64 用 runner 本机工具链；arm64 在 x64 runner 上交叉编译（VS 自带 ARM64 工具）。
          # 不用 windows-11-arm runner：私有仓库拿不到，公开仓库也常排队。
          - target: x86_64-pc-windows-msvc
            arch: x64
            inno: x64compatible
          - target: aarch64-pc-windows-msvc
            arch: arm64
            inno: arm64
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{{{ matrix.target }}}}

      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{{{ matrix.target }}}}

      - name: 取版本号
        id: ver
        shell: pwsh
        run: |
          if ($env:GITHUB_REF_TYPE -eq 'tag') {{
            $v = $env:GITHUB_REF_NAME -replace '^v', ''
          }} else {{
            $m = Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
            $v = $m.Matches[0].Groups[1].Value
          }}
          "version=$v" >> $env:GITHUB_OUTPUT

      - name: 编译
        run: cargo build --release --locked --target ${{{{ matrix.target }}}}

{build_pack}
  release:
    needs: build
    if: startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true

      - uses: softprops/action-gh-release@v2
        with:
          files: {files}
          fail_on_unmatched_files: true
          generate_release_notes: true
"#
    )
}

// 找 installer/*.iss，找不到就返回 None（走 zip 分支）。
fn find_iss(root: &Path) -> Option<String> {
    let dir = root.join("installer");
    let entries = std::fs::read_dir(&dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().map(|x| x.eq_ignore_ascii_case("iss")).unwrap_or(false) {
            return Some(format!(
                "installer\\{}",
                p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            ));
        }
    }
    None
}

pub fn init_workflow(exe: Option<&str>, force: bool) -> Result<String, String> {
    let root = repo_root()?;
    let exe = exe
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::capability::guess_name(&root.display().to_string()));
    let iss = find_iss(&root);
    let yml = workflow_yaml(&exe, iss.as_deref());
    let dir = root.join(".github").join("workflows");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败: {e}"))?;
    let path = dir.join("release.yml");
    if let Ok(old) = std::fs::read_to_string(&path) {
        if old.trim() != yml.trim() && !force {
            return Err(format!(
                "{} 已存在且内容不同。确认要覆盖加 --force\n（现有 {} 字节，新生成 {} 字节）",
                path.display(),
                old.len(),
                yml.len()
            ));
        }
    }
    std::fs::write(&path, &yml).map_err(|e| format!("写 workflow 失败: {e}"))?;
    Ok(format!(
        "已生成 {}\n  可执行文件: {exe}.exe\n  打包方式: {}\n  矩阵: x86_64-pc-windows-msvc + aarch64-pc-windows-msvc",
        path.display(),
        match iss {
            Some(i) => format!("Inno Setup（{i}）"),
            None => "zip（没找到 installer/*.iss）".to_string(),
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_head_reads_ahead_behind() {
        let (b, a, be, up) = parse_head("## main...origin/main [ahead 2, behind 1]");
        assert_eq!(b, "main");
        assert_eq!(a, 2);
        assert_eq!(be, 1);
        assert_eq!(up, "origin/main");
    }

    #[test]
    fn parse_head_without_upstream() {
        let (b, a, be, up) = parse_head("## feature/x");
        assert_eq!(b, "feature/x");
        assert_eq!((a, be), (0, 0));
        assert!(up.is_empty());
    }

    #[test]
    fn normalize_tag_adds_v_prefix() {
        assert_eq!(normalize_tag("0.2.0"), "v0.2.0");
        assert_eq!(normalize_tag("v0.2.0"), "v0.2.0");
        assert_eq!(normalize_tag("v0.2.0-rc1"), "v0.2.0-rc1");
    }

    // workflow 里 `${{ }}` 是 GitHub 表达式，Rust 的 format! 要写成 `{{{{ }}}}`，
    // 漏一个就会生成 `${{ matrix.arch }` 这种坏 YAML。这条测试专盯这个。
    #[test]
    fn workflow_yaml_keeps_github_expressions() {
        let y = workflow_yaml("tba", Some("installer\\tba.iss"));
        assert!(y.contains("${{ matrix.arch }}"), "矩阵表达式被吃掉了");
        assert!(y.contains("${{ matrix.target }}"));
        assert!(y.contains("${{ steps.ver.outputs.version }}"));
        assert!(y.contains("installer\\tba.iss"));
        assert!(!y.contains("{{{{"), "format 转义没还原");
        let z = workflow_yaml("app", None);
        assert!(z.contains("Compress-Archive"));
        assert!(!z.contains("iscc"));
    }
}
