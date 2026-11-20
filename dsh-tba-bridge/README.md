# dsh-tba-bridge

让 [tba](https://github.com/Rei1mu/teambition-agent-orchestrator) 的 detached 监听在收到任意回复时主动唤醒 DSH 会话，无需等待后台 job 退出。

## 用途

DSH 原本只有「后台 job 退出」一条 agent 唤醒通道。tba 的监听收到非「回 1」的回复不会退出（要一直等到收尾），于是 DSH 不会唤醒。本插件用「文件握手」绕开：

1. tba 监听收到任意回复 → 写 `data/wakes/<sessionId>.json`，内容 `{ sessionId, text, ts }`。
2. 本插件（跑在 DSH 进程内，持有 `agents` 服务）每 2 秒轮询该目录。
3. 读到文件 → `agents.get(sessionId).steer(msg)` 插队注入对应会话、立刻唤醒 → 删文件。

目标会话不在本进程（子代理干完被回收）时回退注入当前活跃主会话，消息带原会话 id 前缀；一个活跃会话都没有才保留文件，等会话回来再注入。

## 唤醒目录

- 默认：用户目录下的 `tba/data/wakes`（即 `~/.tba` 的同级 `tba\data\wakes`）。
- 覆盖：设环境变量 `TBA_DATA_DIR` 指向 tba 的数据根目录（即包含 `wakes/` 的 `data/` 目录）。

## 安装（DSH profile bundle）

本仓库是一个 [DSH](https://github.com/deepseek-ai/DeepSeek-Harness) host 插件 bundle。在 DSH 的 profile 里以 link 方式安装：

```
git clone https://github.com/Rei1mu/dsh-tba-bridge.git
```

在 profile 的 bundle 配置里加入（路径按实际 clone 位置调整）：

```json
{
  "name": "dsh-tba-bridge",
  "source": "link:/absolute/path/to/dsh-tba-bridge"
}
```

`cordis.patch.yml` 会挂载 `dsh-tba-bridge` 插件；运行期不产生配置参数。`catalog/dsh-plugin.json` 是插件目录元数据。

## 抽风循环截断

子代理（`session.header.origin === 'subagent'`，`delegationDepth > 0` 兜底）卡在同一个工具调用上反复重试，就是「抽风」。插件在 `tools/post-execute` 上数重复，攒够了直接 `agent.cancel()` 截断。

### 机制

1. `ctx.on('tools/post-execute', ...)`：waterfall，**永远先 `await next()` 放行**再统计，不阻断工具结果。被拒绝（denied）的调用也流经这里——模型死磕一个被拒的调用正是要断的循环。
2. 链 key = `JSON.stringify([exec.name, canonicalize(exec.arguments)])`，`canonicalize` 做深 key 排序，参数书写顺序不同视为同一次调用。
3. 同一个 key 连续出现 `REPEAT_THRESHOLD = 3` 次 → 记一次「抽风」（命名沿用用户口径）。
4. 同一个子代理累计 `TRIP_LIMIT = 2` 次抽风 → `truncate()`：
   ```js
   agent.cancel({ kind: 'hook', reason: 'tba-bridge/loop-truncated: <工具名> 连续重复 N 次，第 2 次抽风' },
                { keepInbox: true })
   ```
   并在 `wakes/_bridge-status.json` 落一条 `{ phase: 'loop-truncated', agentId, tool, count, trip }`，同时 `logger.warn` 一行。
5. 截断后清掉该 agent 的 chains/trips。

### 重置与豁免

- `agent/pre-step`：消息里有 `source.kind === 'user'`（用户插话）即清零链与计数。人已介入，旧账不再累积。
- `agent/disposed`：显式清除 WeakMap 条目（WeakMap 本身也会随 Agent 回收）。
- **只管子代理**。主代理（root）重复调用常常是有意的多轮排查，`root` 重复任意多次绝不截断。

### 可调

`index.js` 顶部常量：

| 常量 | 默认 | 含义 |
| --- | --- | --- |
| `REPEAT_THRESHOLD` | `3` | 同一调用连续重复几次算一次抽风 |
| `TRIP_LIMIT` | `2` | 累计几次抽风后截断（用户口径「两三次」，取 2 早点断） |

想更早断：`TRIP_LIMIT = 1`。想更宽容：调大任一常量。

### 已知限制

- `agent.cancel()` 是**协作式**的：只置状态、让 driver 在下一个检查点停下。正在跑的工具若不看 signal 会跑完才停；idle 时 cancel 是 no-op。
- `cancel` 传 `{ keepInbox: true }`：默认 `false` 会 `inbox.clear()` 丢掉还没跑的用户消息。
- 与 `dsh-repeat-tool-reminder` 是**两套独立计数**，互不知道对方的存在；同时装会各自提醒/截断。本插件只做截断，不注入提醒文本。
- 只识别「完全相同的 name + arguments」。参数每次微调（时间戳、随机数、路径递增）的循环不会被抓到。
- 只统计子代理。主代理抽风不处理。

## 验证

```
node --check index.js
```

启动后插件会在 `wakes/` 目录写 `_bridge-status.json`，记录启动、扫描、注入等诊断阶段。

## License

MIT（见 [LICENSE](LICENSE)）。
