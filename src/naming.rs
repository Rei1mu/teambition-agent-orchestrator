//! 「这个仓库该叫什么项目名」的证据收集。
//!
//! 接入 Teambition 时，过去直接拿目录名当项目名。目录名恰恰是最弱的一条线索：
//! 仓库真正的名字通常写在 git remote、Cargo.toml、package.json 里，而目录名可能叫
//! `code`、`tmp`、甚至工具名本身（TBHub / tba）。
//!
//! 这里把能查到的线索按可信度排好序交出去，由用户拍板——不替用户决定。

use std::path::Path;

/// 一个候选项目名 + 它的来源（来源要给用户看，他才能判断可不可信）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub source: String,
}

/// 扫目录，返回按可信度排好序、去重后的候选名。
///
/// 顺序：git remote > 本目录的构建清单 > 子目录里的项目 > 仓库根目录名 > 当前目录名。
pub fn candidates(dir: &Path) -> Vec<Candidate> {
    let mut raw: Vec<(u8, Candidate)> = Vec::new();

    // 1. git remote：最强的证据。别人 clone 下来看到的就是这个名字。
    if let Some((name, url)) = git_remote(dir) {
        add(&mut raw, 0, &name, format!("git remote origin（{url}）"));
    }

    // 2. 本目录的构建清单：包名通常就是项目名。
    for (file, pri) in MANIFESTS {
        let p = dir.join(file);
        if !p.is_file() {
            continue;
        }
        if let Some(n) = manifest_name(&p) {
            add(&mut raw, *pri, &n, format!("{file} 里的包名"));
        }
    }

    // 3. 子目录里的项目：会话文件夹常常是一个「装着好几个项目」的父目录，
    //    这种情况光看父目录名毫无意义。
    for sub in subprojects(dir) {
        add(&mut raw, 3, &sub.name, sub.source);
    }

    // 4. 仓库根目录名（`git rev-parse --show-toplevel`）：和当前目录不一定同一个。
    if let Some(root) = git_toplevel(dir) {
        if let Some(n) = root.file_name().and_then(|s| s.to_str()) {
            add(&mut raw, 4, n, "git 仓库根目录名".to_string());
        }
    }

    // 5. 当前目录名：最弱，但总比没有强。
    if let Some(n) = dir.file_name().and_then(|s| s.to_str()) {
        add(&mut raw, 5, n, "当前目录名".to_string());
    }

    raw.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<Candidate> = Vec::new();
    for (_, c) in raw {
        if out.iter().any(|x| x.name.eq_ignore_ascii_case(&c.name)) {
            continue;
        }
        out.push(c);
    }
    out
}

// 构建清单 -> 可信度。数字越小越可信。
const MANIFESTS: &[(&str, u8)] = &[
    ("Cargo.toml", 1),
    ("package.json", 1),
    ("pyproject.toml", 1),
    ("go.mod", 2),
    ("composer.json", 2),
    ("pubspec.yaml", 2),
];

fn add(out: &mut Vec<(u8, Candidate)>, pri: u8, name: &str, source: String) {
    let name = clean(name);
    if name.is_empty() {
        return;
    }
    out.push((pri, Candidate { name, source }));
}

/// 去掉引号、路径分隔符、`.git` 后缀，挡掉明显不是名字的东西。
fn clean(name: &str) -> String {
    let mut n = name.trim().trim_matches('"').trim_matches('\'').trim();
    if let Some(stripped) = n.strip_suffix(".git") {
        n = stripped;
    }
    // 远程地址偶尔会带尾斜杠
    let n = n.trim_end_matches('/');
    // 只取最后一段，防止把 `group/repo` 整串当名字
    let n = n.rsplit(['/', '\\']).next().unwrap_or(n);
    let n = n.trim();
    if n.is_empty() || n == "." || n == ".." {
        return String::new();
    }
    n.to_string()
}

fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut c = std::process::Command::new("git");
    c.arg("-C").arg(dir).args(args);
    crate::proc::hide(&mut c);
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn git_remote(dir: &Path) -> Option<(String, String)> {
    // 先 origin，没有再退回第一个 remote——没有 origin 的仓库很常见。
    let url = run_git(dir, &["remote", "get-url", "origin"]).or_else(|| {
        let first = run_git(dir, &["remote"])?
            .lines()
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();
        run_git(dir, &["remote", "get-url", &first])
    })?;
    let name = clean(&url);
    if name.is_empty() {
        None
    } else {
        Some((name, url))
    }
}

fn git_toplevel(dir: &Path) -> Option<std::path::PathBuf> {
    run_git(dir, &["rev-parse", "--show-toplevel"]).map(std::path::PathBuf::from)
}

/// 读构建清单里的包名。只做够用的解析，不引 toml/json 依赖。
fn manifest_name(path: &Path) -> Option<String> {
    let file = path.file_name()?.to_str()?;
    let s = std::fs::read_to_string(path).ok()?;
    match file {
        "Cargo.toml" => {
            let mut in_package = false;
            for line in s.lines() {
                let t = line.trim();
                if t.starts_with('[') {
                    in_package = t == "[package]";
                    continue;
                }
                if in_package {
                    if let Some(v) = key_value(t, "name") {
                        return Some(v);
                    }
                }
            }
            None
        }
        "pyproject.toml" => s.lines().find_map(|l| key_value(l.trim(), "name")),
        "package.json" => json_name(&s),
        "composer.json" => json_name(&s),
        "go.mod" => s
            .lines()
            .find_map(|l| l.trim().strip_prefix("module ").map(str::trim))
            .map(|m| m.rsplit('/').next().unwrap_or(m).to_string()),
        "pubspec.yaml" => s.lines().find_map(|l| key_value(l.trim(), "name")),
        _ => None,
    }
}

/// `name = "xxx"` / `name: 'xxx'` 这类。
fn key_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let v = rest.trim();
    let v = v.trim_matches('"').trim_matches('\'').trim();
    if v.is_empty() || v.starts_with('{') || v.starts_with('[') {
        None
    } else {
        Some(v.to_string())
    }
}

/// JSON 里第一个 `"name"` 的值（顶层就是包名；嵌套的 key 很少撞上）。
fn json_name(s: &str) -> Option<String> {
    let i = s.find("\"name\"")?;
    let rest = &s[i + 6..];
    let c = rest.find(':')?;
    let rest = rest[c + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let v = &rest[..end];
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

fn is_project_root(p: &Path) -> Option<&'static str> {
    if p.join(".git").exists() {
        return Some(".git");
    }
    for (f, _) in MANIFESTS {
        if p.join(f).is_file() {
            return Some(f);
        }
    }
    None
}

fn subprojects(dir: &Path) -> Vec<Candidate> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut dirs: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    dirs.sort();
    for p in dirs {
        if !p.is_dir() {
            continue;
        }
        let Some(base) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // 隐藏目录和构建产物永远不是「项目」
        if base.starts_with('.')
            || matches!(base, "target" | "node_modules" | "dist" | "data" | "build")
        {
            continue;
        }
        let Some(why) = is_project_root(&p) else {
            continue;
        };
        let name = if why == ".git" {
            base.to_string()
        } else {
            manifest_name(&p.join(why)).unwrap_or_else(|| base.to_string())
        };
        out.push(Candidate {
            name,
            source: format!("子目录 {base}（{why}）"),
        });
        if out.len() >= 8 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tba-naming-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn manifest_name_beats_directory_name() {
        let d = tmp("manifest");
        // 目录名故意取成工具名：正是用户遇到的那个坑
        let repo = d.join("TBHub");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"teambition-agent\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let c = candidates(&repo);
        assert_eq!(c[0].name, "teambition-agent");
        assert!(c[0].source.contains("Cargo.toml"));
        // 目录名仍然作为最弱候选留在后面
        assert!(c.iter().any(|x| x.name == "TBHub"));
    }

    #[test]
    fn finds_projects_inside_a_parent_folder() {
        let d = tmp("sub");
        let a = d.join("web");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("package.json"), "{\n  \"name\": \"acme-web\"\n}\n").unwrap();
        let b = d.join("api");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("go.mod"), "module github.com/acme/api\n\ngo 1.22\n").unwrap();
        let c = candidates(&d);
        assert!(c.iter().any(|x| x.name == "acme-web"));
        assert!(c.iter().any(|x| x.name == "api"));
        assert!(c.iter().any(|x| x.name == "acme-api" || x.name == "api"));
    }

    #[test]
    fn clean_strips_git_suffix_and_paths() {
        assert_eq!(clean("https://github.com/acme/foo.git"), "foo");
        assert_eq!(clean("git@github.com:acme/bar.git"), "bar");
        assert_eq!(clean("  \"baz\"  "), "baz");
        assert_eq!(clean(""), "");
    }
}
