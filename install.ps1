# 一句话装 tba + 挂 MCP。仓库公开后匿名可用。
#
# 用法（PowerShell，一行）：
#   irm https://raw.githubusercontent.com/Rei1mu/teambition-agent-orchestrator/main/install.ps1 | iex
#
# 做三件事：下载最新 Release 的 setup.exe -> 静默安装 -> 合并写 WorkBuddy 的 mcp.json。
# 不删 data/，不覆盖 mcp.json 里别的 server。
# 固定版本：先设 $env:TBA_VERSION = 'v0.2.31' 再跑上面那行。

$ErrorActionPreference = 'Stop'
$repo = 'Rei1mu/teambition-agent-orchestrator'

# 1. 找最新 Release。公开仓库的 API 匿名可读；也可以设 TBA_VERSION 固定 tag。
$tag = $env:TBA_VERSION
if (-not $tag) {
    $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" `
        -Headers @{ 'User-Agent' = 'tba-install' }
    $tag = $rel.tag_name
}
$ver = $tag.TrimStart('v')
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$asset = "tba-$ver-$arch-setup.exe"
$url = "https://github.com/$repo/releases/download/$tag/$asset"
$setup = Join-Path $env:TEMP $asset

# 2. 下载 + 静默安装。安装页默认把 {app} 写进用户 PATH。
# 输出全用 ASCII：PS 5.1 的 irm 会按 Latin-1 解 UTF-8，中文在这里会变乱码。
Write-Host "Downloading $asset ..."
Invoke-WebRequest -Uri $url -OutFile $setup -UseBasicParsing
Write-Host 'Installing silently ...'
$p = Start-Process -FilePath $setup `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CLOSEAPPLICATIONS' `
    -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "Install failed, exit code $($p.ExitCode)" }

$exe = Join-Path $env:USERPROFILE 'tba\tba.exe'
if (-not (Test-Path $exe)) {
    throw "tba.exe not found at $exe. If you changed the install dir, edit this script and run again."
}

# 3. 合并写 MCP 配置。只动 mcpServers.tba。
$cfg = Join-Path $env:USERPROFILE '.workbuddy-ai\mcp.json'
New-Item -ItemType Directory -Force -Path (Split-Path $cfg) | Out-Null
$json = if (Test-Path $cfg) {
    Get-Content $cfg -Raw | ConvertFrom-Json
} else {
    [pscustomobject]@{}
}
$servers = @{}
if ($json.PSObject.Properties.Name -contains 'mcpServers') {
    foreach ($p in $json.mcpServers.PSObject.Properties) { $servers[$p.Name] = $p.Value }
}
# 写绝对路径而不是裸 tba.exe：装完 PATH 要等新进程才生效，
# agent 宿主进程往往还活着，裸名字会起不来。
$servers['tba'] = [pscustomobject]@{ command = $exe; args = [string[]]@('serve') }
$json | Add-Member -NotePropertyName mcpServers -NotePropertyValue ([pscustomobject]$servers) -Force
$text = $json | ConvertTo-Json -Depth 8
# 不用 Set-Content -Encoding utf8：PS 5.1 会写 BOM，有些 MCP 解析器不接受。
[System.IO.File]::WriteAllText($cfg, $text, (New-Object System.Text.UTF8Encoding($false)))

Write-Host "Done: tba $ver installed at $exe"
Write-Host "MCP config: $cfg"
Write-Host 'Last step: open WorkBuddy connector settings, find tba, click Trust.'
