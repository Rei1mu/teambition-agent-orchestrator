# 通过 dsh-context 查看上下文

dsh-context（`github.com/bowenliang123/dsh-context`）是 DSH 的上下文洞察插件。它把上下文组成、演进、多 Agent 协作写进 session 缓存，GUI 里的上下文可视化就是它驱动的。用它能在**不发额外请求、不占模型上下文**的前提下，查到工具定义占了多少 token、每个工具调了几次、system / 工具 / 消息各自的占比。

## 1. 数据在哪

```
C:\Users\user\.dsh\storages\session_projcache\sessions\session-<uuid>.json
```

一个文件一个会话，几 KB ~ 几百 KB。判断哪个文件是当前会话：按 mtime 取最近的一个，再核对 `record.rows` 里的 cwd / agentPreset / turn 数。

## 2. 关键字段（都在 `record.rows` 下）

| 字段 | 内容 | 能回答什么 |
|---|---|---|
| `contextHeaders.val.headers[]` | 每次 `request/header` 的工具定义快照 | 工具定义提示词有多长 |
| `headers[].tools[]` | `{name, tokens, plugin?}` —— token 由 `estimateToolSchema()` 估算 | 每个工具占多少 token |
| `headers[].systemTokens` | system prompt token 估算 | system 占比 |
| `contextActivity.val.days.<YYYY-MM-DD>.tools` | `{<name>: {n, last}}` | 某工具当天调用几次 |
| 同层 `cmds` / `toolCalls` | 命令文本、调用总数 | pwsh 里具体跑了什么 |
| `sessionStats.val` | `toolCalls`、`tools.run_code.calls/ms`、`programTally`、`toolArgBlocks` | 累计统计 |
| `contextTimeline` / `turnOutline` / `contextHeaders` / `contextActivity` | 时间线、轮次、上下文头、活动 | 演进与分布 |

**注意**：`contextHeaders` 落盘的是 wire 视图（`name` / `tokens` / `plugin`），**不含工具的 `description` / `schema` 原文**。要看原文得去 DSH 的 session event log 或工具包源码（见第 6 节）。

## 3. 怎么读（别整读大文件）

单个 session json 几百 KB，用 `ConvertFrom-Json` 读一个文件没问题；**不要 glob 后全量读所有文件**。

```powershell
$f = 'C:\Users\user\.dsh\storages\session_projcache\sessions\session-<id>.json'
$j = Get-Content $f -Raw | ConvertFrom-Json

# 最后一个 header 的工具 token 明细，按占用排序
$j.record.rows.contextHeaders.val.headers[-1].tools |
  Sort-Object tokens -Descending | Select-Object name, tokens, plugin

# 工具定义合计
($j.record.rows.contextHeaders.val.headers[-1].tools | Measure-Object tokens -Sum).Sum

# 某天各工具调用次数
$j.record.rows.contextActivity.val.days.'2026-11-20'.tools | Format-List

# 当天总 toolCalls
$j.record.rows.contextActivity.val.days.'2026-11-20'.toolCalls
```

只做定位时用 grep 搜字段名（`contextHeaders` / `contextActivity` / `tools` / `tokens`），命中后再用 read 的 `offset` / `limit` 按块看。

## 4. 名字映射坑

dsh-context 里的工具名可能和 GUI 显示名不同。查数时以 json 里的 `name` 为准，不要按记忆里的工具名搜。工具定义原文要从工具包源码读（见第 5 节）。

## 5. 工具定义原文在哪

- DSH 内置工具在 `D:\dsh\resources\app.asar\dsh\node_modules\@deepseek-ai\dsh-tool-*\src\index.ts`，例如 `dsh-tool-pwsh\src\index.ts`。
- preset `D:\dev\mcp\dsh-tba-preset\cordis.patch.yml` 决定挂哪些工具插件。
- 第三方插件在 `C:\Users\user\.dsh\profiles\desktop\node_modules\<plugin>\`。

## 6. 参考实现（dsh-context 源码）

克隆在 `C:\Users\user\AppData\Local\Temp\dsh-context-inspect`：

- `src/host/headers.ts`：从 `request/header` 事件提取工具定义并算 token。
- `src/host/pricing.ts`：`estimateToolSchema()` / `estimateToolsTotal()`。
- `src/host/activity.ts`、`src/host/foldMetering.ts`：调用次数与耗时统计。
- `src/client/components/browserToolCard.tsx`：`schemaTextOf()` 展示工具 schema 原文。
- `src/client/components/browser.tsx`：工具 token 明细 UI。

## 7. 边界

- session json 里没有工具 schema 原文；要分析提示词文本必须读 DSH 安装包源码。
- `dsh-context-diff` 只压缩 instruction 文件（AGENTS.md / CLAUDE.md）的重复注入，首次加载和 compaction 后仍发全文，不管工具定义。
