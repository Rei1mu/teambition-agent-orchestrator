# dsh-pwsh-lite

A DeepSeek Harness (DSH) plugin that provides the `pwsh-lite` agent tool.

The tool runs a PowerShell command with `pwsh -Command` in a fresh process and returns stdout/stderr. It supports explicit `workdir`, configurable `timeoutMs`, foreground/background execution, timeout promotion to a background job, sandbox escalation, and native Windows paths plus `$env:NAME` expansion.

## Requirements

- DeepSeek Harness with Cordis plugin support
- PowerShell 7 (`pwsh`) on `PATH`
- DSH built-in packages resolved by Harness: `@deepseek-ai/schemastery`, `@deepseek-ai/dsh-tools`, `@deepseek-ai/dsh-llm`, `@deepseek-ai/dsh-jobs`, `@deepseek-ai/dsh-sandbox`, `@deepseek-ai/dsh-subprocess`, `@deepseek-ai/dsh-shell`

All built-in packages above are declared in `peerDependencies` with `*`. Actual versions come from the installed Harness. Do not add them as regular `dependencies`.

## Install

Install the package into the active profile's `package.json` `dependencies`. Do **not** hand-create a junction: that leaves the package out of the profile manifest and DSH will not load it.

Preferred: use the DSH plugin manager's **Plugins > Add plugin** and point it at this local directory. The plugin manager writes the profile manifest and lockfile for you.

Alternative from the profile directory:

```powershell
cd ~/.dsh/profiles/<profile>
pnpm add link:D:/dev/mcp/dsh-pwsh-lite
```

For a clone at another path, replace `D:/dev/mcp/dsh-pwsh-lite` with the absolute path to this repository.

The installed plugin must remain resolvable from the profile's `node_modules`. After installation, check that the profile `package.json` lists `@local/dsh-pwsh-lite` under `dependencies` and that `node_modules/@local/dsh-pwsh-lite` resolves.

## Enable

Installation and activation are separate in DSH. Being listed in `dependencies` only means the package is installed; the plugin still needs to be enabled.

There are two supported ways to make `pwsh-lite` available:

1. **Preset reference**: add the plugin to the chosen preset's `plugins` list:

   ```yaml
   - id: pwsh-lite
     name: '@local/dsh-pwsh-lite'
   ```

   This makes the tool available only in sessions using that preset.

2. **Default activation**: add the plugin name to `dsh.profile.bundles` in the profile configuration. This mounts the root plugin in sessions and enables the tool by default. Do both only if you understand preset isolation; otherwise duplicate registration may occur.

If you use only a preset reference and expect default availability, you will see the plugin listed as disabled/off in the plugin UI and new sessions will not have the tool. Add it to `dsh.profile.bundles` if you want it enabled by default.

After any install or enablement change, **completely exit and restart DSH**. DSH builds its module resolution table at startup, and a failed preset mount is terminal for that process.

## Package entry points

- `main` / `.` export: `./index.js`
- Cordis patch export: `./cordis.patch.yml`
- package manifest export: `./package.json`
- DSH bundle patch: `dsh.bundle.patch = ./cordis.patch.yml`

`cordis.patch.yml` inserts row `pwsh-lite` from `./index.js`.

## Config

From `index.js`:

- `enableRunInBackground` (boolean, default `true`): allow `run_in_background` jobs and timeout promotion.
- `promoteOnTimeout` (boolean, default `true`): when background jobs are available, move a timed-out foreground command to a background job. Has no effect when `enableRunInBackground` is `false`.

## Troubleshooting

- `agent-preset/invalid: pwsh-lite (@local/dsh-pwsh-lite): never started` usually means the entry module failed to import, not that the plugin was disabled. Check that the package is in profile `dependencies`, resolvable from profile `node_modules`, `main`/`exports` point at `index.js`, and every `@deepseek-ai/*` import is present in `peerDependencies`.
- A raw `ERR_MODULE_NOT_FOUND` for `@deepseek-ai/*` from outside DSH is expected: DSH installs its resolution interception only inside the app process. Do not verify with a bare `node -e "import(...)"`.
- Plugin appears in the UI but is off/disabled: it is installed but not enabled. Add it to `dsh.profile.bundles` or to a preset `plugins` list, then restart DSH.
- Plugin not listed at all: the profile `package.json` was not updated, or the package cannot be resolved from the profile `node_modules`. Reinstall through the plugin manager or `pnpm add link:...`.

Plugin manager logs can be found under `~/.dsh/profiles/<profile>/.plugin-manager/`.

## License

MIT. Copyright 2026 Rei1mu.
