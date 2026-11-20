// 通用能力（cap）：本地、跨项目的文本能力库。
//
// 和 Teambition 项目 skill 的区别（这两件事以前混在一起，导致「截图」这种
// 跟项目无关的能力被推进了某个项目的云端 skill 里，换个项目就没了）：
//   - 项目 skill：这个项目怎么干活，存 Teambition 云端，按项目 id 取；
//   - 通用能力：这台机器上的 tba 能干什么，存本地 data/caps.toml，所有项目共用。
//
// 每条能力 = 名字 + 一句话说明 + 开关 + 正文（markdown）。
// agent 用 `tba cap list` 看目录、`tba cap <名字>` 读正文——按需读，省 token。
// GUI 的「通用能力 → 能力」页可以直接增删改查，改完立刻落盘，不用联网。
//
// 为什么正文是文本而不是写死在代码里：命令（tba shot / publish）负责
// 「怎么做」的确定性，正文只写「什么时候用、参数、红线」。后者会随使用习惯变，
// 让用户自己改比每次发版改代码划算。

use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CAP_FILE: &str = "caps.toml";

#[derive(Clone, Serialize, Deserialize)]
pub struct Cap {
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default = "default_on")]
    pub on: bool,
    /// 内置条目：正文可改、可删，但删了能用 `tba cap restore` 找回来。
    #[serde(default)]
    pub builtin: bool,
    /// 常驻：开工文案里直接标出来，并把它当红线先讲。
    /// 只给「动手前就必须知道、等踩了坑再去读已经晚了」的约束型能力
    /// （典型是「禁止弹命令行」）。其余能力都是索引 + 按需读正文。
    #[serde(default)]
    pub always: bool,
    #[serde(default)]
    pub body: String,
}

fn default_on() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CapsFile {
    #[serde(default)]
    pub caps: Vec<Cap>,
    /// 已经灌过第几版内置条目。老文件没这个字段 = 第 1 版。
    /// 只在**内置清单本身变了**（新增了一条内置能力）时才升级：
    /// 这样新增的 no-console 能自动出现在老用户的列表里，而用户自己删掉的
    /// 条目不会因为普通读取又长回来。
    #[serde(default = "seed_v1")]
    pub seeded: u32,
}

fn seed_v1() -> u32 {
    1
}

impl Default for CapsFile {
    fn default() -> Self {
        CapsFile {
            caps: Vec::new(),
            seeded: SEED,
        }
    }
}

/// 当前内置清单版本。加/删内置条目、或改内置条目的索引说明时 +1。
///
/// v3：索引说明改成「何时用」句式，并给 no-console 标上常驻。
/// v4：新增 git-create（建 GitHub 仓库）——以前没这条，agent 被叫「建仓库」
///     时不知道 tba 能干，就退化去让用户装 gh 了。
/// v5：新增 dsh-plugin（dsh 插件开发避坑指南内联）和 cap-author
///     （教 agent 怎么给 tba 加通用能力：本地 cap 命令 vs 改 BUILTIN）。
/// v6：新增 context-cost（已知 sessionId 时查 dsh 上下文组成/token 用量，
///     读投影缓存，不打开 UI）。
const SEED: u32 = 6;

// ---- 内置正文 ----
//
// 只写「什么时候用、参数、红线」，实现细节在命令里。改这里等于改默认值，
// 用户已经改过的条目不会被覆盖（load 只在文件不存在时灌一次）。

pub const BODY_SHOT: &str = "\
# 截图

## 一条命令
`tba shot --out .tmp/s.png`

## 参数
- `--out <路径>`：默认 `shot.png`。目录不存在会自动建。
- `--title <标题片段>`：截指定窗口，标题包含匹配，取第一个可见窗口。
- `--hwnd <句柄>`：直接给句柄，优先于 `--title`。
- `--level fast|balanced|high`：压缩级别，**都是无损**。默认 fast（毫秒级，全屏约 0.3–0.8 MB）；嫌大用 high。

## 要点
- 用 BitBlt 抓屏，**不需要把窗口提到前台**，后台窗口也能截。
- **不要自己写 PowerShell 截图脚本**，也不要找第三方工具。就这一条命令。
- 截完用图片查看工具看 `.tmp/s.png`，不要凭想象描述界面。
";

pub const BODY_GIT: &str = "\
# 提交并推送

## 先看状态
`tba git status` —— 分支、脏文件、远端领先/落后、是不是 GitButler 项目。

## 一步提交推送
`tba git main -m \"feat: 改了什么\"`

- GitButler 项目自动走 `but commit` + `but push`；普通仓库走 `git add -A` + `commit` + `push origin HEAD:<分支>`。
- 默认推到 origin/HEAD 指向的分支（通常是 main）。要指定加 `--branch`。
- GitButler 里要直接落到目标分支：加 `--land`。
- 只想看会跑什么：`--dry`。
- 只打 tag：`tba git tag --tag v1.2.3`

## 红线
- 不 force push。push 被拒先 `git pull --rebase`。
- 不提交 `.env`、token、`data/`。
- 提交信息写清「改了什么」，别用「update」。

## 仓库还不存在 / 没连到 GitHub
先 `tba git create`，别自己拼 `git remote add`，也别去装 `gh`。
详见 `tba cap git-create`。

## 这些工具的指定位置
- git：设置页没单独字段，但 `git` 本身挑错版本会出怪问题。config.toml 的 `git_path` 可写死 git 路径（环境变量 TBA_GIT 也可临时覆盖）。
- GitButler CLI：设置页「GitButler」卡可「选择 but.exe 位置」；或用 `tba gitbutler set --path`。
";

pub const BODY_GIT_CREATE: &str = "\
# 创建 / 连接 GitHub 仓库

## 什么时候用
用户说「建个 GitHub 仓库」「把这个项目传到 GitHub」「创建仓库」「更新到 GitHub」，
而当前目录**还没有 GitHub 远端**时，用这一条。

## 一条命令
`tba git create [--name 仓库名] [--private] [--dry]`

会做完：调 GitHub API 建仓库 -> 加 `origin` 远端 -> （没有提交就建初始提交）-> 推送。
**不要装 `gh`**，也不要自己拼 `git remote add` + `git push`，更不要让用户去网页上手动建。

## 参数
- `--name <名>`：缺省用当前目录名；中文/空格会洗成 `-`。
  名字**全是中文**时洗完为空，会报错要求你显式给个英文名。
- `--private`：建私有仓库（默认公开）。
- `--remote <名>`：远端名，默认 `origin`。
- `--dry`：只打印会做什么，不真的建/推。拿不准先跑一次。

## 幂等（可以反复跑）
- `origin` 已指向 GitHub -> **不重复建**，直接推送，等于「更新到 GitHub」。
- `origin` 指向别处（如 GitLab）-> 报错不覆盖，先 `git remote remove origin` 或换 `--remote`。
- 目录还不在 git 仓库里 -> 自动 `git init`。

## 前置条件
需要 GitHub token，二选一：
- 环境变量 `GH_TOKEN` / `GITHUB_TOKEN`（最稳，gh CLI 也认）；
- 或 `git push` 本来就能用（token 在 git 凭据里，tba 会读出来）。
`tba doctor` 会显示当前是哪种。token 只用于本次推送的 HTTP header，不会写进 `.git/config`。

## 之后要更新代码
`tba git main -m \"改了什么\"`（别名 `tba git update`）。GitButler 项目自动走 `but`。

## 红线
- 别让用户装 `gh`：建仓库这一件事 tba 已经能干。
- 别 force push；别提交 `.env`、token、`data/`。
- 建完把 `https://github.com/<owner>/<repo>` 报给用户。
";

pub const BODY_RELEASE: &str = "\
# 发版（GitHub Actions）

## 0. 仓库还没有 release 流程
`tba publish --init`

生成 `.github/workflows/release.yml`：windows x64 + windows arm64（MSVC 交叉编译）
两个 job，有 `installer/*.iss` 就出 setup.exe，没有就出 zip，tag 推上去自动发 Release。
已有同名文件且内容不同时会拒绝，确认覆盖加 `--force`。

## 1. 升版本号
改 `Cargo.toml` 的 `version`，**然后跑一次 `cargo build` 让 Cargo.lock 跟上**，
有 CHANGELOG 就加一条，然后提交。
（CI 用 `--locked`，lock 里还是旧版本号会直接失败：`cannot update the lock file ... --locked`）

## 2. 一条命令发版
`tba publish --tag vX.Y.Z`

打 tag -> 推 tag，就结束。**默认不等 CI**——CI 默认会成功，失败用户会反馈。
没有 `--tag` 时读 Cargo.toml 的 version。
只有用户明确要看 CI 产物时才加 `--watch`（要 GitHub token，且会卡到 CI 跑完）。

## 3. 收尾
告诉用户 tag 已推、CI 在跑；地址：`https://github.com/<owner>/<repo>/releases/tag/vX.Y.Z`

## 红线
- CI 没绿不许说「已发布」。
- 版本号只升不降；已有 tag 默认拒绝重打。
- 打包脚本改了，先本地打一次包再打 tag。

## 编译环境（编 Windows 软件 / DLL 用）
- 编 Windows 原生程序 / DLL 必须 **MSVC** 工具链（link.exe + Windows SDK 的库）。
- 位置在**设置页「编译工具链（MSVC / Windows SDK）」**管理：默认自动探测（vswhere 找 VS + Windows Kits），
  也能手动指定；没装会在卡片和 `tba doctor` 里给安装地址。
- 设定后位置存 `data/toolchain.toml`，`tba doctor` 的 msvc / wsdk 行能看到实际生效值。
- **编译前先查这些位置**（agent 别盲目扫盘）：把 `msvc_bin` 加进 PATH、`msvc_lib` + `winsdk_libs` 加进 LIB，
  再 `cargo build` / `cl.exe`。装了多个 VS/SDK 版本或装在非标准位置时，扫盘必挑错。
";

pub const BODY_GUI: &str = "\
# 写 GUI 的注意事项

## 字体
- **中文用黑体**：SimHei（simhei.ttf）或 Microsoft YaHei（msyh.ttc）。别用默认字体，中文会掉成方框。
- 字体链要有回退：首选 -> 备用中文字体 -> 符号字体（seguisym.ttf）。只装一个字体必然缺字形。
- 字体要**可配置**：设置里列本机已装字体让用户选，选不到就回落链首。

## 别出豆腐块（口）
- 符号（⚠ ✓ ▾ ⚙ emoji）先确认目标字体有字形，没有就换成文字，比如「注意：」「设置」。
- 上线前把所有符号渲染一遍看结果，别只看代码。

## 控件
- 字号可调，默认 18px。数字用可左右拖的 DragValue，**不要滑块**，也不要只能打字的输入框。
- 同一区域按钮尺寸统一，`button` 和 `small_button` 别混用。
- 内容超出窗口要能滚动，底部按钮不能被裁。

## 其它
- 启动声明 DPI 感知，否则 125%/150% 缩放下整个界面糊。
- 打包成 GUI 程序别弹控制台黑窗（Windows 上 `FreeConsole`，别改 subsystem，会吞掉 CLI 输出）。

## 验证
改完必须 `tba shot --title <窗口标题> --out .tmp/ui.png` 截图自己看，不要凭想象说「已修好」。
";

pub const BODY_NO_CONSOLE: &str = "\
# 禁止弹命令行

## 规则
凡是起子进程，**都不许弹控制台窗口**。窗口一闪而过最影响使用，用户看到黑框
就以为程序出问题了。

## 怎么做
- tba 自己起的进程（git / but / cargo / 终端脚本）已经统一加 `CREATE_NO_WINDOW`，
  不用管。
- agent 自己起进程时：
  - 终端脚本：`Start-Process -WindowStyle Hidden` 或 `-WindowStyle Minimized`；
  - cmd：`start /b`，别用 `start`（会开新窗口）；
  - 能不用 shell 就不用，直接调 exe。
- 后台跑长任务用 `run_in_background`，不要开个窗口让用户看着。

## 红线
- 不要为了「看输出」把窗口留在前台；输出重定向到日志文件，需要时读文件。
- 不要用 `cmd /c start \"\" ...` 打开程序，会闪一个黑框。
";

pub const BODY_DSH_PLUGIN: &str = "\
# dsh 插件开发避坑指南

> 基于 `@local/dsh-pwsh-lite` 接入失败案例整理：新建会话报 `agent-preset/invalid: pwsh-lite (@local/dsh-pwsh-lite): never started`，且插件不出现在 dsh 插件列表中。

---

## 1. 案例现象

| 项目 | 内容 |
| --- | --- |
| 插件 | `@local/dsh-pwsh-lite`（内含 `index.js` + `cordis.patch.yml`） |
| 安装方式 | 手工在 `~/.dsh/profiles/desktop/node_modules/@local/` 下创建指向 `D:\\dev\\mcp\\dsh-pwsh-lite` 的 junction |
| 使用位置 | 被 `@local/dsh-tba-preset`、`@local/dsh-chat-preset` 的 preset 组合以行 `{ id: pwsh-lite, name: '@local/dsh-pwsh-lite' }` 引用 |
| 报错 | 新建会话失败：`agent-preset/invalid: pwsh-lite (@local/dsh-pwsh-lite): never started` |
| 附带现象 | dsh 插件管理界面看不到该插件 |

---

## 2. 根因分析

### 2.1 只建了 junction，没有写入 profile 清单

dsh 的 profile 目录结构：

```
~/.dsh/profiles/<profile>/
├── package.json        # 依赖清单（dsh 插件列表的数据源）
├── pnpm-lock.yaml
├── node_modules/
└── cordis.yml
```

插件列表（installed 列表）来自 profile `package.json` 的 `dependencies`，且要求对应包在 `node_modules` 中真实可解析。只创建目录链接、不写清单，等于：
- 插件不会出现在插件列表；
- profile 的包解析表中没有它，属于\"野生模块\"；
- 后续任何 `pnpm install` 都会把它清理掉或视而不见。

### 2.2 linked 插件的宿主包导入必须声明 `peerDependencies`（真正的崩溃点）

dsh 的 `@deepseek-ai/*`（dsh-tools、dsh-llm、dsh-jobs、dsh-sandbox、dsh-subprocess、dsh-shell、schemastery 等）都打包在 Harness 的应用包（app.asar）内，磁盘上普通 `node_modules` 找不到。dsh 通过一个**运行时模块解析拦截层**把它们路由到内置安装。

拦截规则对两类插件不同：

- **profile 内安装的插件**（真实目录）：请求 `@deepseek-ai/*` 直接走拦截路由，无需额外声明；
- **linked 插件**（`link:` 依赖 / junction 到 profile 之外）：只有当请求的包名出现在\"最近的 package.json 的 `peerDependencies`\"里时才会被拦截，否则回落到 Node 原生解析。

`@local/dsh-pwsh-lite` 属于第二类，且 package.json 中没有任何 `peerDependencies`，于是：

```text
index.js  import '@deepseek-ai/dsh-tools'
  -> 不在 peerDependencies，不拦截
  -> Node 从 D:\\dev\\mcp\\dsh-pwsh-lite 向上找 node_modules
  -> 找不到（核心包在 app.asar 内）
  -> ERR_MODULE_NOT_FOUND
```

### 2.3 导入失败被包装成 `never started`

preset 挂载后的审计逻辑：entry 导入失败 → fiber 从未创建 → 输出 `<id> (<name>): never started`。原始 `ERR_MODULE_NOT_FOUND` 被 `tree.await()` 的 `allSettled` 吞掉。

结论：**`never started` 基本等同于\"模块导入失败\"**，排查时不要往\"插件被禁用 / 未启用\"方向找。同时 preset 挂载失败在当前进程内是终态，不会自动重试。

### 2.4 交付缺失：没有处理\"默认开启\"

即使导入问题修好，本案例还暴露了另一个问题：插件作者只交付了一个\"能装的包\"，没有说明也没有配置启用方式，结果装完 `pwsh-lite` 仍未默认开启——插件列表里是关闭状态，新建会话里没有对应工具。

dsh 中\"安装\"与\"启用\"分离（详见 3.3），作者如果不主动写 `dsh.profile.bundles` 或在 README 里给出启用步骤，使用者就会以为插件坏了。

---

## 3. 正确做法

### 3.1 安装（写入 profile 清单）

```powershell
cd ~/.dsh/profiles/<profile>
pnpm add link:D:/dev/mcp/dsh-pwsh-lite
```

或使用插件管理器的\"本地目录安装\"。不要手工创建 junction。

### 3.2 package.json 模板

```json
{
  \"name\": \"@local/dsh-pwsh-lite\",
  \"version\": \"1.0.0\",
  \"private\": true,
  \"type\": \"module\",
  \"main\": \"./index.js\",
  \"exports\": {
    \".\": \"./index.js\",
    \"./cordis.patch.yml\": \"./cordis.patch.yml\",
    \"./package.json\": \"./package.json\"
  },
  \"peerDependencies\": {
    \"@deepseek-ai/schemastery\": \"*\",
    \"@deepseek-ai/dsh-tools\": \"*\",
    \"@deepseek-ai/dsh-llm\": \"*\",
    \"@deepseek-ai/dsh-jobs\": \"*\",
    \"@deepseek-ai/dsh-sandbox\": \"*\",
    \"@deepseek-ai/dsh-subprocess\": \"*\",
    \"@deepseek-ai/dsh-shell\": \"*\"
  },
  \"dsh\": { \"bundle\": { \"patch\": \"./cordis.patch.yml\" } }
}
```

要点：

1. **`index.js` 里 import 的每个 `@deepseek-ai/*`，都要在 `peerDependencies` 中出现。**
2. 必须提供可被 `import` 的入口（`main`/`exports`，缺省回落到 `index.js` 也可用），不能只有 `cordis.patch.yml` —— preset 行写的是包名，dsh 会把它当模块导入。
3. `peerDependencies` 用 `\"*\"` 即可，实际的版本由 Harness 内置安装决定。

### 3.3 安装不等于启用

dsh 里「已安装」和「已启用」是两件事：

- **已安装** = 包出现在 profile `package.json` 的 `dependencies`，并能在 `node_modules` 中解析；
- **已启用** = 包名出现在 `dsh.profile.bundles`，启动时才会被组合进根 tree。

只安装、不启用时，插件管理界面会把插件列为「未启用/关闭」，新建的会话里也拿不到它注册的工具。本案例就是这样：包能解析了，但 `pwsh-lite` 仍然没有默认开启。

因此插件作者不能只交付一个\"能装的包\"，要么把推荐启用方式写进 README，要么直接给出需要修改的 `bundles` 配置片段。

### 3.4 是否需要加入 bundles

| 插件用途 | `dependencies` | `dsh.profile.bundles` |
| --- | --- | --- |
| 默认全局启用（装完即可用） | 需要 | 需要 |
| 仅被某个 preset 内部引用 | 需要 | 可选，不加则不默认启用 |

判断标准：

- 希望\"新建会话默认就有这个工具\" → 必须加入 `bundles`；
- 只希望特定 preset 拥有它 → 加进该 preset 的 `plugins` 列表即可，`bundles` 可留空，但要接受\"默认不开启\"。

`@local/dsh-pwsh-lite` 同时被 `tba` / 聊天两个 preset 引用；如果还要求它默认可用，就还需要出现在 `bundles` 中（当前配置即如此）。两种方式并存时，建议确认 preset 的子树是隔离作用域，避免同一个工具名在根 tree 和 preset tree 中被重复注册。

---

## 4. 排查清单

新建或用修改后的插件时，逐项检查：

- [ ] profile `package.json` 的 `dependencies` 中有该包，且 `node_modules` 中可解析
- [ ] `pnpm-lock.yaml` 已同步（`pnpm install --no-frozen-lockfile`）
- [ ] linked 插件的 `peerDependencies` 覆盖了全部 `@deepseek-ai/*` import
- [ ] 插件有 `main`/`exports` 入口
- [ ] preset 专用插件未加入 `dsh.profile.bundles`（如果确实不需要默认启用）
- [ ] 需要默认开启的插件已加入 `dsh.profile.bundles`
- [ ] README 写清了安装方式与启用方式
- [ ] 修改后**完全退出并重新启动 dsh**（解析表在启动时构建，preset 挂载失败是进程内终态）

### 辅助定位

- 插件管理器操作记录：`~/.dsh/profiles/<profile>/.plugin-manager/`
- 安装日志：`~/.dsh/profiles/<profile>/.plugin-manager/logs/*/pnpm.log`
- 不要用裸 `node -e \"import(...)\"` 验证：`@deepseek-ai/*` 的解析拦截只在 dsh 进程内安装，裸 node 必然报找不到，容易误判。

---

## 5. 错误信息对照表

| 报错 | 含义 | 常见原因 |
| --- | --- | --- |
| `agent-preset/invalid: <id> (<pkg>): never started` | entry 导入失败，fiber 未创建 | 模块解析失败、`peerDependencies` 缺失、包未安装 |
| `ERR_MODULE_NOT_FOUND`（dsh 日志/调试时） | 包无法从插件位置解析 | 未声明 peerDependencies，或插件目录不在 profile 解析层内 |
| 插件列表无该插件 | manifest 未声明 | 只建了 junction，没写 `dependencies` |
| 插件在列表中显示为未启用/关闭 | 未加入启用集合 | 不在 `dsh.profile.bundles`，需手动开启 |

---

## 6. 一句话总结

写 dsh 插件时，\"能 import 到核心包\"不是 Node 的事，而是 dsh 解析拦截的事；**声明依赖 + 声明 peerDependencies + 配置启用（bundles）+ 重启** 四件事缺一不可。
";

pub const BODY_CAP_AUTHOR: &str = "\
# 给 tba 加通用能力（cap）

## 一次性、当前设备：走 cap 命令
- `tba cap add <名字> --summary \"何时用\" --body <文件>`：正文从文件读；`--body -` 从 stdin 读。
- `tba cap list` 看有哪些能力；`tba cap <名字>` 读正文（按需读，省 token）。
- `tba cap edit <名字>` 改正文/说明；`tba cap on/off <名字>` 切开关；
  `tba cap rm <名字>` 删除；内置条目删了可用 `tba cap restore <名字>` 恢复。
- `tba cap path` 看本地文件路径（data/caps.toml）。

## 产品级、所有用户设备：改内置清单
需求是「所有用户/设备升级后自动拥有」时，必须改代码，不是只改本机 caps.toml：
1. 改 `src/caps.rs` 的 `BUILTIN` 数组：每一项是 (名字, summary「何时用」,
   always, 正文常量)。正文写成 `pub const BODY_XXX: &str = \"...\"`，内容随代码分发，
   不能引用本机绝对路径。
2. 把 `SEED` 常量 +1，并在注释里补一行说明这次加了什么。
   `load()` 只在 seeded < SEED 时给老用户补缺失的内置条目并刷新索引层，
   不动用户改过的正文/on 开关。
3. 在 `src/caps.rs` 末尾 tests 模块更新/新增测试：首次加载能拿到新能力；
   老用户（seeded=旧值）升级后自动补上、且不覆盖用户改过的正文和开关。
4. MSVC 环境编译（Git Bash 直接 build 会命中 GNU link.exe 失败）：
   ```powershell
   $env:PATH = \"C:\\Program Files\\Microsoft Visual Studio\\18\\Community\\VC\\Tools\\MSVC\\14.44.35207\\bin\\HostX64\\x64;$env:PATH\"
   $env:LIB  = \"C:\\Program Files\\Microsoft Visual Studio\\18\\Community\\VC\\Tools\\MSVC\\14.44.35207\\lib\\x64;D:\\d\\winsdk_libs\"
   Set-Location D:\\dev\\mcp
   cargo +stable-x86_64-pc-windows-msvc test --offline --lib caps
   cargo +stable-x86_64-pc-windows-msvc build --release --offline
   ```
5. 部署：先关掉运行中的 tba 进程（否则文件占用），再
   `Copy-Item D:\\dev\\mcp\\target\\release\\tba.exe  C:\\Users\\user\\tba\\tba.exe -Force` 和
   `Copy-Item D:\\dev\\mcp\\target\\release\\tbagui.exe C:\\Users\\user\\tba\\tbagui.exe -Force`。
   两个 exe 都要换：tbagui.exe 是双击入口，只换 tba.exe 时启动器自身的改动不会生效。
6. 验证：`tba cap list` 能看到新能力。

## 红线
- 别把能力正文手写进 AGENTS.md 自动块：refresh 会覆盖，而且只在这台设备生效。
- 别只改本机 caps.toml 就以为所有设备都有了：那是当前设备一次性的改法。
- 索引层（summary/always）跟版本走；正文和 on 开关是用户的，升级不许覆盖。
- 同名 builtin=false 的用户自定义条目不会被升级逻辑覆盖；如需清理，让用户自己
  `tba cap rm <名字>`，别在 load() 里抢用户条目。
";

pub const BODY_CONTEXT_COST: &str = "\
# dsh-context 上下文查询

## 什么时候用
已知某个 dsh 会话的 `sessionId`（形如 `session-34ca832b-cf88-4893-8912-c3bad16b3db5`），
要查对话摘要、上下文组成、token 开销，而**不想打开 UI** 时，按本文读投影缓存即可。
路径以 `~/.dsh/` 为数据根（Windows 默认 `C:\\Users\\user\\.dsh`）。

## 1. 数据位置
| 数据 | 路径 | 格式 | 说明 |
| --- | --- | --- | --- |
| 投影缓存（首选） | `~/.dsh/storages/session_projcache/sessions/<sessionId>.json` | JSON | 对话摘要、上下文组成、工具定义、用量都在这里；Context Browser 同源 |
| 会话 transcript | `~/.dsh/sessions/<workspaceSlug>/<sessionId>/session.v4.jsonl.zstd` | zstd + JSONL | 原始事件流；会话存活期可能只有 header，未刷盘 |
| 会话索引 | `~/.dsh/storages/workspace.json` | JSON | sessionId → 工作区路径、归档/置顶状态 |

投影缓存文件名通常是 `session-<uuid>.json`，但历史数据也有裸 uuid 或 `mortis-sum-*` 前缀；
查找时别只按 `session-` 前缀过滤，按 uuid 模糊匹配同目录文件更稳。

## 2. 投影缓存结构
```json
{
  \"version\": 1,
  \"record\": {
    \"identity\": { \"formatVersion\": 4, \"createdAt\": 1795137441677, \"cwd\": \"...\", \"isSeeded\": false },
    \"rows\": { \"<投影名>\": { \"ver\": 2, \"seq\": 47, \"val\": <投影值> } }
  }
}
```
`rows` 每个键是一个投影，`val` 是最新物化状态，`seq` 对应会话事件序号（越大越新）。

### 常用投影
| 投影 | 内容 |
| --- | --- |
| `turnOutline` | `{turns:[{turn,seq,prompt,response}],draft}`，按轮次的输入和助手回复（`response` 可能被截断） |
| `title` / `titleInput` | 会话标题及其来源输入 |
| `workingDirectory` | 会话 cwd |
| `agentPreset` | 当前 agent 预设（如 `standard`） |
| `modelSelection` | `lastUsed:{provider,model}` / `pending` |
| `permissions` | `{preset,sandbox,approval}` |
| `contextBreakdown` | `{systems, surfaceTokens, breakdown:{systemTokens, toolsTokens, messageTokens}}` |
| `contextPressure` | `{surfaceTokens, contextWindow, pressureTokens, sampledSurfaceTokens}`，聊天输入框环形图同源 |
| `contextHeaders` | 每次请求头快照：`headers[].tools[]`（工具名与 token 数）等 |
| `contextTimeline` | surface 事件序列，`cat` 取值如 `user` / `inject` / `skill` / `tool`，带 token 数 |
| `contextActivity` | 按天聚合的 tokens、请求数、成本和缓存命中 |
| `tokenUsage` | `totals` 与每次请求的 input/output/cacheRead/cacheWrite |
| `tokenMeasurement` | 官方 token 计量（含 provider 上报 usage） |
| `sessionStats` | 轮次、步骤数、LLM 耗时、工具耗时、TTFT 等 |
| `timeContext` / `turnBoundary` | 时间上下文、轮次边界状态 |
| `todos` / `plan` / `goal` / `inbox` | 任务、计划模式、goal、待处理消息 |
| `subagent` / `subagentCatalog` / `subagentTiming` / `agentTeam` | 子代理与 agent 网络 |
| `userQuestions` | ask_user_question 的进行中/已结束问题 |
| `sessionListMetadata` | 是否空白会话、最后提问时间等 |

### 读取命令（Node 读 JSON）
列出全部投影名：
```powershell
node -e \"const fs=require('fs');const r=JSON.parse(fs.readFileSync(process.argv[1],'utf8')).record.rows;for(const k of Object.keys(r))console.log(k,'seq='+r[k].seq)\" \"<投影缓存路径>\"
```
读对话轮次：
```powershell
node -e \"const fs=require('fs');const r=JSON.parse(fs.readFileSync(process.argv[1],'utf8')).record.rows;console.log(JSON.stringify(r.turnOutline.val,null,2))\" \"<投影缓存路径>\"
```
读上下文组成 + 压力：
```powershell
node -e \"const fs=require('fs');const r=JSON.parse(fs.readFileSync(process.argv[1],'utf8')).record.rows;console.log(JSON.stringify(r.contextBreakdown.val,null,2));console.log(JSON.stringify(r.contextPressure.val,null,2))\" \"<投影缓存路径>\"
```
导出可读摘要（轮次 + 上下文 + 用量 + 模型）：
```powershell
node -e \"const fs=require('fs');const r=JSON.parse(fs.readFileSync(process.argv[1],'utf8')).record.rows;const g=k=>r[k]&&r[k].val;console.log(JSON.stringify({title:g('title'),cwd:g('workingDirectory'),preset:g('agentPreset'),model:g('modelSelection'),turns:g('turnOutline'),context:g('contextBreakdown'),pressure:g('contextPressure'),usage:g('tokenUsage')},null,2))\" \"<投影缓存路径>\"
```

## 3. transcript 与 workspace 索引
transcript 路径 `~/.dsh/sessions/<workspaceSlug>/<sessionId>/session.v4.jsonl.zstd`。
`workspaceSlug` 由工作区路径转义：反斜杠换 `-`、空格换 `~0020`、首尾加 `--`
（如 `D:\\dev\\koishi-mortis` → `--D-dev-koishi-mortis--`）。
zstd 解压用 Node（≥22.15，推荐 24），PowerShell 5 不支持：
```powershell
node -e \"const fs=require('fs'),z=require('zlib');process.stdout.write(z.zstdDecompressSync(fs.readFileSync(process.argv[1])).toString('utf8'))\" \"<transcript 路径>\"
```
完整逐条消息、工具参数和结果原文只能从 transcript 取；会话未刷盘时文件可能只有 session 头。
先用投影缓存确认有没有对话，再解压 transcript 取原文。

`~/.dsh/storages/workspace.json` 用于由 sessionId 反查工作区：
- `global.workspaceIds` / `global.defaultWorkspaceId` / `global.archivedSessionIds` / `global.pinnedSessionIds`
- `tables.workspaces[<workspaceId>]`：`{path, title, sessionIds, createdAt, updatedAt}`
```powershell
node -e \"const fs=require('fs');const w=JSON.parse(fs.readFileSync('C:/Users/user/.dsh/storages/workspace.json','utf8'));for(const [id,v] of Object.entries(w.tables.workspaces)){if(v.sessionIds.includes(process.argv[1]))console.log(v.path,'|',v.title)}\" \"<sessionId>\"
```

## 4. 运行时
投影缓存随会话推进更新（mtime / `seq` 变化），读到的是最近一次物化状态，非读取瞬间。
要权威实时内容用运行中的 dsh Web UI（启动日志 `dsh web: http://127.0.0.1:<port>/?token=...`）；
`contextBreakdown` / `contextPressure` 与 UI 环形图同源，数值一致。

## 5. 六个坑
1. 只看 transcript 会误判空会话（未刷盘）——先查投影缓存。
2. 投影缓存文件名不统一（裸 uuid / `mortis-sum-*` 等），按 uuid 模糊匹配更稳。
3. `turnOutline.response` 是截断摘要，完整回复、thinking、工具细节要 transcript 或 Web UI。
4. zstd 解压不要用 PowerShell 内置能力，统一用 Node 的 `zlib.zstdDecompressSync`。
5. 同会话可能有 `mortis-sum-*` 摘要缓存，不要和主投影缓存混用。
6. `contextWindow` 只是模型声明上限（如 1,000,000），实际压力看 `contextPressure.pressureTokens` / `surfaceTokens`。
";
// (名字, 索引说明, 常驻, 正文)
//
// 索引说明写「**什么时候用**」而不是「是什么」：开工文案只列这一行，
// agent 靠它判断要不要去读正文。写「截图：一条命令出 png」它看不出该在
// 什么场合用；写「看界面/验收 UI」才是一个触发条件。
pub const BUILTIN: &[(&str, &str, bool, &str)] = &[
    // 不放重载：编译 & 重载能力已移除。
    (
        "no-console",
        "绝不弹黑框：所有命令都隐藏窗口调用",
        true,
        BODY_NO_CONSOLE,
    ),
    (
        "shot",
        "看界面/验收 UI：一条命令出 png，后台窗口也能截",
        false,
        BODY_SHOT,
    ),
    (
        "git-main",
        "要提交推送：GitButler 项目自动走 but",
        false,
        BODY_GIT,
    ),
    (
        "git-create",
        "要新建/连接 GitHub 仓库：一条命令建好并推送，别装 gh",
        false,
        BODY_GIT_CREATE,
    ),
    (
        "release",
        "要发版：生成 workflow / 打 tag / 等 CI",
        false,
        BODY_RELEASE,
    ),
    (
        "gui-notes",
        "改 GUI 前先读：字体、黑体、别出豆腐块",
        false,
        BODY_GUI,
    ),
    (
        "dsh-plugin",
        "写/改 dsh 插件前先读：避坑指南（peerDependencies / never started / bundles）",
        false,
        BODY_DSH_PLUGIN,
    ),
    (
        "cap-author",
        "要给 tba 加/改通用能力：走 cap 命令，别手写 AGENTS.md",
        false,
        BODY_CAP_AUTHOR,
    ),
    (
        "context-cost",
        "要查某会话 token 开销/上下文组成：读投影缓存，不用开 UI",
        false,
        BODY_CONTEXT_COST,
    ),
];

pub fn path(store: &Store) -> PathBuf {
    store.root.join(CAP_FILE)
}

fn builtin_caps() -> Vec<Cap> {
    BUILTIN
        .iter()
        .map(|(name, summary, always, body)| Cap {
            name: (*name).to_string(),
            summary: (*summary).to_string(),
            on: true,
            builtin: true,
            always: *always,
            body: (*body).to_string(),
        })
        .collect()
}

// 读全部能力。文件不存在 = 首次使用，灌一份内置模板并落盘。
// 文件存在但是空数组时**不重灌**——那是用户自己删光的，别跟他抢。
pub fn load(store: &Store) -> CapsFile {
    let p = path(store);
    match std::fs::read_to_string(&p) {
        Ok(s) => {
            let mut f: CapsFile = toml::from_str(&s).unwrap_or_default();
            let mut dirty = false;
            // 兼容老文件：摘掉内置的 reload 条目（该能力已移除）。
            // 老文件里那条内置条目留着只会让 agent 读到一个已经不该用的说明，
            // 所以读的时候顺手摘掉（用户自己建的、非内置的同名条目不动）。
            let before = f.caps.len();
            f.caps.retain(|c| !(c.builtin && c.name == "reload"));
            if f.caps.len() != before {
                dirty = true;
            }
            // 内置清单升级：补缺失的条目，并把**索引层**（说明 + 常驻标记）
            // 拉齐到当前版本。
            //
            // 为什么索引层可以覆盖：它是「目录」，决定 agent 会不会去读正文，
            // 跟着版本走才对。用户真正会改的是正文和开关，这两样一律不动。
            // 自定义条目（builtin=false）完全不经手。
            if f.seeded < SEED {
                for (name, summary, always, body) in BUILTIN {
                    match f.caps.iter_mut().find(|c| c.name == *name) {
                        Some(c) => {
                            if c.builtin {
                                c.summary = (*summary).to_string();
                                c.always = *always;
                            }
                        }
                        None => f.caps.push(Cap {
                            name: (*name).to_string(),
                            summary: (*summary).to_string(),
                            on: true,
                            builtin: true,
                            always: *always,
                            body: (*body).to_string(),
                        }),
                    }
                }
                f.seeded = SEED;
                dirty = true;
            }
            if dirty {
                let _ = save(store, &f);
            }
            f
        }
        Err(_) => {
            let f = CapsFile {
                caps: builtin_caps(),
                seeded: SEED,
            };
            let _ = save(store, &f);
            f
        }
    }
}

pub fn save(store: &Store, f: &CapsFile) -> Result<(), String> {
    let p = path(store);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {e}"))?;
    }
    let s = toml::to_string_pretty(f).map_err(|e| format!("序列化失败: {e}"))?;
    // 先写临时文件再改名：写一半崩了不会留下半截 caps.toml。
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, s).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("替换 {} 失败: {e}", p.display()))?;
    Ok(())
}

pub fn list(store: &Store) -> Vec<Cap> {
    load(store).caps
}

pub fn get(store: &Store, name: &str) -> Option<Cap> {
    load(store).caps.into_iter().find(|c| c.name == name)
}

// 只给启用的，给 agent 文案用。
pub fn enabled(store: &Store) -> Vec<Cap> {
    load(store)
        .caps
        .into_iter()
        .filter(|c| c.on)
        .collect()
}

// 某个项目实际能用的：全局开着，且没被这个项目单独关掉。
// 开工文案按这个列，agent 就不会去翻一个跟当前项目无关的能力。
pub fn enabled_for(store: &Store, project_id: &str) -> Vec<Cap> {
    let off = store.load_cap_off(project_id);
    enabled(store)
        .into_iter()
        .filter(|c| !off.iter().any(|n| n == &c.name))
        .collect()
}

// 名字是命令行参数，也是文件名之外的唯一键，限制字符集省得 agent 传奇怪的串。
pub fn check_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("名字不能为空".to_string());
    }
    if n.chars().count() > 40 {
        return Err("名字太长（最多 40 字符）".to_string());
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("名字只能用字母、数字、- 和 _：{n}"));
    }
    Ok(())
}

// 新建或覆盖。同名视为编辑，保留原来的 on 开关和 builtin 标记。
pub fn upsert(store: &Store, mut cap: Cap) -> Result<String, String> {
    check_name(&cap.name)?;
    cap.name = cap.name.trim().to_string();
    let mut f = load(store);
    match f.caps.iter_mut().find(|c| c.name == cap.name) {
        Some(old) => {
            old.summary = cap.summary;
            old.body = cap.body;
            // 开关以传入值为准：GUI 编辑时会带上原值，CLI 的 --off 也能直接生效。
            old.on = cap.on;
            // 内置标记只增不减：用户手改过正文的 shot 仍然是内置条目，还能 restore。
            if cap.builtin {
                old.builtin = true;
            }
            save(store, &f)?;
            Ok(format!("已更新能力: {}", cap.name))
        }
        None => {
            let name = cap.name.clone();
            f.caps.push(cap);
            save(store, &f)?;
            Ok(format!("已新增能力: {name}"))
        }
    }
}

pub fn set_on(store: &Store, name: &str, on: bool) -> Result<String, String> {
    let mut f = load(store);
    let c = f
        .caps
        .iter_mut()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("没有这个能力: {name}（tba cap list 看有哪些）"))?;
    c.on = on;
    save(store, &f)?;
    Ok(format!(
        "{}: {name}",
        if on { "已启用" } else { "已关闭" }
    ))
}

pub fn remove(store: &Store, name: &str) -> Result<String, String> {
    let mut f = load(store);
    let before = f.caps.len();
    f.caps.retain(|c| c.name != name);
    if f.caps.len() == before {
        return Err(format!("没有这个能力: {name}"));
    }
    save(store, &f)?;
    let recover = BUILTIN.iter().any(|(n, ..)| *n == name);
    Ok(if recover {
        format!("已删除能力: {name}（内置条目，`tba cap restore {name}` 可恢复）")
    } else {
        format!("已删除能力: {name}")
    })
}

// 从内置模板恢复。给名字只恢复那一条，不给则补齐所有缺失的内置条目。
pub fn restore(store: &Store, name: Option<&str>) -> Result<String, String> {
    let mut f = load(store);
    let mut done = Vec::new();
    for (n, summary, always, body) in BUILTIN {
        if let Some(want) = name {
            if want != *n {
                continue;
            }
        }
        match f.caps.iter_mut().find(|c| c.name == *n) {
            Some(c) => {
                // 已存在：正文、说明、常驻标记一起拉回默认；开关留着用户改过的样子。
                c.body = (*body).to_string();
                c.summary = (*summary).to_string();
                c.always = *always;
                c.builtin = true;
                done.push(format!("已重置 {n}"));
            }
            None => {
                f.caps.push(Cap {
                    name: (*n).to_string(),
                    summary: (*summary).to_string(),
                    on: true,
                    builtin: true,
                    always: *always,
                    body: (*body).to_string(),
                });
                done.push(format!("已恢复 {n}"));
            }
        }
    }
    if done.is_empty() {
        return Err(format!(
            "没有这个内置能力: {}（可选 {}）",
            name.unwrap_or(""),
            BUILTIN
                .iter()
                .map(|(n, ..)| *n)
                .collect::<Vec<_>>()
                .join(" / ")
        ));
    }
    save(store, &f)?;
    Ok(done.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("tba-cap-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Store { root }
    }

    // 首次使用自动灌内置模板，且立刻落盘（下次不会重灌、不会覆盖用户改动）。
    #[test]
    fn first_load_seeds_builtins() {
        let store = tmp("seed");
        let caps = list(&store);
        assert_eq!(caps.len(), BUILTIN.len());
        assert!(path(&store).exists());
        assert!(caps.iter().any(|c| c.name == "no-console"));
        assert!(
            !caps.iter().any(|c| c.name == "reload"),
            "reload 能力已移除，不该再出现在能力库里"
        );
        assert!(
            caps.iter().any(|c| c.name == "dsh-plugin"),
            "首次加载就该带 dsh-plugin 避坑指南"
        );
        assert!(
            caps.iter().any(|c| c.name == "cap-author"),
            "首次加载就该带 cap-author 加能力教程"
        );
        assert!(
            caps.iter().any(|c| c.name == "context-cost"),
            "首次加载就该带 context-cost 上下文查询"
        );
        assert!(caps.iter().all(|c| c.on && c.builtin));
    }

    // 用户把能力删光 = 空数组，不能当成「没初始化」再灌回来。
    #[test]
    fn empty_file_is_not_reseeded() {
        let store = tmp("empty");
        // 当前版本写出来的空文件 = 用户主动删光，不能重灌。
        save(
            &store,
            &CapsFile {
                caps: Vec::new(),
                seeded: SEED,
            },
        )
        .unwrap();
        assert!(list(&store).is_empty());
    }

    // 老 caps.toml 没有 always 字段：读进来默认 false，不能因为缺字段就整条丢掉。
    #[test]
    fn missing_always_defaults_to_false() {
        let store = tmp("legacy-always");
        std::fs::write(
            path(&store),
            "seeded = 3\n\n[[caps]]\nname = \"old\"\nsummary = \"老条目\"\non = true\n\
             builtin = false\nbody = \"x\"\n",
        )
        .unwrap();
        let c = get(&store, "old").expect("缺 always 字段也要能读出来");
        assert!(!c.always);
    }

    // 索引层（说明 + 常驻标记）跟版本走；正文和开关是用户的，升级不许碰。
    #[test]
    fn seed_upgrade_refreshes_index_only() {
        let store = tmp("seed-upgrade");
        let mut f = load(&store);
        f.seeded = 1;
        for c in f.caps.iter_mut() {
            if c.name == "shot" {
                c.summary = "我自己写的".into();
                c.body = "我改过的正文".into();
                c.on = false;
            }
            if c.name == "no-console" {
                c.always = false;
            }
        }
        save(&store, &f).unwrap();

        let after = load(&store);
        let shot = after.caps.iter().find(|c| c.name == "shot").unwrap();
        assert_eq!(
            shot.summary, "看界面/验收 UI：一条命令出 png，后台窗口也能截",
            "说明属于索引层，跟着版本走"
        );
        assert_eq!(shot.body, "我改过的正文", "正文是用户的内容，升级不许覆盖");
        assert!(!shot.on, "开关是用户的，升级不许覆盖");
        assert!(
            after
                .caps
                .iter()
                .find(|c| c.name == "no-console")
                .unwrap()
                .always,
            "常驻标记属于索引层，跟着版本走"
        );
    }

    // 增删改查一轮：改名不合法要拒，删了能恢复。
    #[test]
    fn crud_roundtrip() {
        let store = tmp("crud");
        assert!(check_name("bad name").is_err());
        assert!(check_name("").is_err());
        upsert(
            &store,
            Cap {
                name: "my-cap".into(),
                summary: "自用".into(),
                on: true,
                builtin: false,
                always: false,
                body: "# 正文".into(),
            },
        )
        .unwrap();
        let c = get(&store, "my-cap").expect("刚加的能力应该在");
        assert_eq!(c.body, "# 正文");
        assert!(!c.builtin);

        set_on(&store, "my-cap", false).unwrap();
        assert!(!get(&store, "my-cap").unwrap().on);
        assert_eq!(enabled(&store).iter().filter(|c| c.name == "my-cap").count(), 0);

        remove(&store, "my-cap").unwrap();
        assert!(get(&store, "my-cap").is_none());

        // 内置条目删掉后 restore 能找回来，正文是模板原文
        remove(&store, "shot").unwrap();
        assert!(get(&store, "shot").is_none());
        restore(&store, Some("shot")).unwrap();
        assert_eq!(get(&store, "shot").unwrap().body, BODY_SHOT);
    }

    // 老用户（v3）升级到 v4：git-create 要自动出现在列表里，
    // 否则老设备上的 agent 永远看不到这条能力，还会继续叫用户装 gh。
    #[test]
    fn seed_upgrade_adds_git_create_without_touching_user_caps() {
        let store = tmp("seed-git-create");
        let mut f = load(&store);
        // 模拟老配置：没有 git-create，seeded=3，用户改过 git-main 的正文和开关。
        f.caps.retain(|c| c.name != "git-create");
        f.seeded = 3;
        for c in f.caps.iter_mut() {
            if c.name == "git-main" {
                c.body = "我自己写的提交流程".into();
                c.on = false;
            }
        }
        save(&store, &f).unwrap();

        let after = load(&store);
        let created = after
            .caps
            .iter()
            .find(|c| c.name == "git-create")
            .expect("v3 升级后必须补上 git-create");
        assert!(created.on && created.builtin);
        assert_eq!(created.body, BODY_GIT_CREATE);
        assert!(created.summary.contains("GitHub"));

        let main = after.caps.iter().find(|c| c.name == "git-main").unwrap();
        assert_eq!(main.body, "我自己写的提交流程", "升级不许覆盖用户正文");
        assert!(!main.on, "升级不许覆盖用户开关");
        assert_eq!(after.seeded, SEED);
    }

    // 新增的内置能力正文里必须点明「别装 gh」：这正是这次要解决的问题。
    #[test]
    fn git_create_body_forbids_installing_gh() {
        assert!(BODY_GIT_CREATE.contains("gh"));
        assert!(BODY_GIT_CREATE.contains("tba git create"));
        assert!(BODY_GIT_CREATE.contains("GH_TOKEN"));
    }

    // 老用户（seeded=4）升级到 v5：dsh-plugin / cap-author 要自动补上，
    // 且用户改过的正文和开关不能被覆盖。
    #[test]
    fn seed_upgrade_v5_adds_new_caps_without_touching_user_caps() {
        let store = tmp("seed-v5");
        let mut f = load(&store);
        // 模拟老配置：没有两条新能力，seeded=4，用户改过 git-main 的正文和开关。
        f.caps.retain(|c| c.name != "dsh-plugin" && c.name != "cap-author");
        f.seeded = 4;
        for c in f.caps.iter_mut() {
            if c.name == "git-main" {
                c.body = "我自己写的提交流程".into();
                c.on = false;
            }
        }
        save(&store, &f).unwrap();

        let after = load(&store);
        for (name, body) in [
            ("dsh-plugin", BODY_DSH_PLUGIN),
            ("cap-author", BODY_CAP_AUTHOR),
        ] {
            let c = after
                .caps
                .iter()
                .find(|c| c.name == name)
                .expect("v4 升级后必须补上新增能力");
            assert!(c.on && c.builtin, "{name} 应作为内置能力默认启用");
            assert_eq!(c.body, body, "{name} 正文应为模板原文");
            assert!(!c.summary.is_empty(), "{name} 索引说明不能是空的");
        }

        let main = after.caps.iter().find(|c| c.name == "git-main").unwrap();
        assert_eq!(main.body, "我自己写的提交流程", "升级不许覆盖用户正文");
        assert!(!main.on, "升级不许覆盖用户开关");
        assert_eq!(after.seeded, SEED);
    }

    // cap-author 正文要教全两条路：本机 cap 命令 + 改 BUILTIN/SEED 的产品级流程。
    #[test]
    fn cap_author_body_covers_both_paths() {
        assert!(BODY_CAP_AUTHOR.contains("tba cap add"), "本机一次性加能力命令");
        assert!(BODY_CAP_AUTHOR.contains("tba cap list"));
        assert!(BODY_CAP_AUTHOR.contains("tba cap path"));
        assert!(BODY_CAP_AUTHOR.contains("src/caps.rs"), "产品级改内置清单");
        assert!(BODY_CAP_AUTHOR.contains("BUILTIN"));
        assert!(BODY_CAP_AUTHOR.contains("SEED"));
        assert!(BODY_CAP_AUTHOR.contains("AGENTS.md"), "要讲清红线");
        assert!(BODY_CAP_AUTHOR.contains("tbagui.exe"), "部署要两个 exe 都换");
    }

    // dsh-plugin 正文必须真的内联了避坑指南，不能只写一个空壳标题。
    #[test]
    fn dsh_plugin_body_inlines_guide() {
        assert!(BODY_DSH_PLUGIN.contains("never started"), "报错关键词");
        assert!(BODY_DSH_PLUGIN.contains("peerDependencies"), "崩溃根因关键词");
        assert!(BODY_DSH_PLUGIN.contains("ERR_MODULE_NOT_FOUND"));
        assert!(BODY_DSH_PLUGIN.contains("dsh.profile.bundles"), "启用方式");
        assert!(BODY_DSH_PLUGIN.contains("排查清单"));
    }

    // context-cost 正文要能直接指导查上下文：关键路径 + 投影名 + Node 命令。
    #[test]
    fn context_cost_body_has_paths_projections_and_commands() {
        assert!(
            BODY_CONTEXT_COST.contains("~/.dsh/storages/session_projcache/sessions/<sessionId>.json"),
            "首选数据源路径"
        );
        assert!(BODY_CONTEXT_COST.contains("session.v4.jsonl.zstd"), "transcript 路径");
        assert!(BODY_CONTEXT_COST.contains("~/.dsh/storages/workspace.json"), "索引路径");
        for p in [
            "turnOutline",
            "contextBreakdown",
            "contextPressure",
            "contextTimeline",
            "tokenUsage",
            "sessionStats",
        ] {
            assert!(BODY_CONTEXT_COST.contains(p), "缺投影名 {p}");
        }
        assert!(BODY_CONTEXT_COST.contains("node -e"), "Node 读取命令");
        assert!(BODY_CONTEXT_COST.contains("zstdDecompressSync"), "zstd 解压命令");
        assert!(BODY_CONTEXT_COST.contains("六个坑"), "常见坑清单");
        assert!(BODY_CONTEXT_COST.contains("pressureTokens"), "实际压力看 pressureTokens");
    }

    // 老用户（seeded=5）升级到 v6：context-cost 要自动补上，
    // 且用户改过的正文和开关不能被覆盖。
    #[test]
    fn seed_upgrade_v6_adds_context_cost_without_touching_user_caps() {
        let store = tmp("seed-v6");
        let mut f = load(&store);
        // 模拟老配置：没有 context-cost，seeded=5，用户改过 git-main 的正文和开关。
        f.caps.retain(|c| c.name != "context-cost");
        f.seeded = 5;
        for c in f.caps.iter_mut() {
            if c.name == "git-main" {
                c.body = "我自己写的提交流程".into();
                c.on = false;
            }
        }
        save(&store, &f).unwrap();

        let after = load(&store);
        let c = after
            .caps
            .iter()
            .find(|c| c.name == "context-cost")
            .expect("v5 升级后必须补上 context-cost");
        assert!(c.on && c.builtin, "context-cost 应作为内置能力默认启用");
        assert_eq!(c.body, BODY_CONTEXT_COST, "正文应为模板原文");
        assert!(c.summary.contains("上下文"), "索引说明要点明何时用");
        assert!(!c.always, "按需读的能力不该常驻");

        let main = after.caps.iter().find(|c| c.name == "git-main").unwrap();
        assert_eq!(main.body, "我自己写的提交流程", "升级不许覆盖用户正文");
        assert!(!main.on, "升级不许覆盖用户开关");
        assert_eq!(after.seeded, SEED);
    }

    // 编辑同名条目不该产生第二条；开关以传入值为准（GUI 编辑时带原值，不会误开）。
    #[test]
    fn upsert_same_name_updates_in_place() {
        let store = tmp("upsert");
        set_on(&store, "shot", false).unwrap();
        upsert(
            &store,
            Cap {
                name: "shot".into(),
                summary: "改过的说明".into(),
                on: true,
                builtin: false,
                always: false,
                body: "新正文".into(),
            },
        )
        .unwrap();
        let all = list(&store);
        assert_eq!(all.iter().filter(|c| c.name == "shot").count(), 1);
        let c = get(&store, "shot").unwrap();
        assert_eq!(c.body, "新正文");
        assert_eq!(c.summary, "改过的说明");
        assert!(c.on, "开关以传入值为准：显式传 true 就该打开");
        assert!(c.builtin, "内置标记不该被覆盖丢掉");
    }
}
