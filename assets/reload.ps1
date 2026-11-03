# ============================================================================
#  reload.ps1 - generic reload capability (built into TBHub)
#
#  Contract: exit code is the only truth. 0 = ok, 1 = fail.
#    stdout: OK <pid> / OK / RUNNING <pid> / STOPPED
#    stderr: ERR <step>:<msg>   (one line)
#    cargo output goes to <root>\target\reload.log, never back to the caller.
#
#  Actions (-Action):
#    build    compile + kill old + copy-verify + launch
#    restart  kill old + launch (no compile)
#    kill     kill old instance only
#    status   print RUNNING <pid> or STOPPED
#
#  Generic:
#    -Root <dir>       target project root (default: script directory)
#    -Name <base>      exe base name (default: [[bin]] name / default-run /
#                      [package] name from Cargo.toml)
#    -Live <path>      live copy (default <root>\<Name>_test.exe)
#    -LaunchArgs       args passed to the launched exe (space separated)
#    -Mode <copy|rename>
#                      copy   live copy mode: artifact is never the running file
#                      rename run the artifact directly: rename the running exe
#                             aside, build, kill old, launch fresh artifact
#    -SelfPid <pid>    caller pid, never killed (the caller may be the artifact)
#    -ReleaseSrc       rename the running cargo artifact aside before build
#    -Elevate          kill/launch via UAC elevation (off by default)
#    -StatusFile       also write the result line to this file
#
#  Toolchain paths can be overridden by env vars:
#    TBA_MSVC_BIN / TBA_MSVC_LIB / TBA_SDK_LIB / TBA_EXTRA_LIB / TBA_CARGO
#
#  NOTE: keep this file pure ASCII. PowerShell 5.1 decodes BOM-less files as
#  ANSI, and a trailing multi-byte char can swallow the following newline.
# ============================================================================
param(
    [string]$Action = "build",
    [string]$Root = "",
    [string]$Name = "",
    [string]$Live = "",
    [string]$LaunchArgs = "",
    [string]$Triple = "stable-x86_64-pc-windows-msvc",
    [string]$Mode = "copy",
    [string]$Compile = "",
    [int]$SelfPid = 0,
    [string]$StatusFile = "",
    [switch]$ReleaseSrc,
    [switch]$Elevate
)

function Write-Status([string]$text) {
    # status queries must not overwrite the last real result
    if ($StatusFile -and $Action -ne "status") {
        try { Set-Content -Path $StatusFile -Value $text -Encoding UTF8 -ErrorAction Stop } catch {}
    }
}
function Out-Ok([string]$msg) {
    Write-Status $msg
    [Console]::Out.WriteLine($msg)
    exit 0
}
function Out-Err([string]$step, [string]$msg) {
    $line = "ERR ${step}:$msg"
    Write-Status $line
    [Console]::Error.WriteLine($line)
    exit 1
}

Write-Status "START $Action"

# ---- locate project root ----
if ($Root) {
    if (-not (Test-Path $Root)) { Out-Err "root" "no such dir: $Root" }
    $ROOT = (Resolve-Path $Root).Path
} else {
    $ROOT = Split-Path -Parent $MyInvocation.MyCommand.Definition
}
$TARGET = Join-Path $ROOT "target"
if (-not (Test-Path $TARGET)) { New-Item -ItemType Directory -Force -Path $TARGET | Out-Null }
$LOG = Join-Path $TARGET "reload.log"

# ---- toolchain ----
$MSVC_BIN = if ($env:TBA_MSVC_BIN) { $env:TBA_MSVC_BIN } else { "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64" }
$MSVC_LIB = if ($env:TBA_MSVC_LIB) { $env:TBA_MSVC_LIB } else { "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.44.35207\lib\x64" }
$SDK_LIB  = if ($env:TBA_SDK_LIB)  { $env:TBA_SDK_LIB }  else { "D:\d\winsdk_libs" }
$EXTRA_LIB = $env:TBA_EXTRA_LIB
$CARGO    = if ($env:TBA_CARGO) { $env:TBA_CARGO } else { "C:\Users\user\.cargo\bin\cargo.exe" }

# ---- exe base name: [[bin]] name > default-run > [package] name > dir name ----
if (-not $Name) {
    $toml = Join-Path $ROOT "Cargo.toml"
    if (Test-Path $toml) {
        $pkgName = ""; $binName = ""; $defRun = ""; $inBin = $false
        foreach ($l in Get-Content $toml) {
            $t = $l.Trim()
            if ($t.StartsWith("[")) {
                $inBin = ($t -eq "[[bin]]")
                continue
            }
            $eq = $t.IndexOf("=")
            if ($eq -lt 0) { continue }
            $k = $t.Substring(0, $eq).Trim()
            $v = $t.Substring($eq + 1).Trim().Trim('"').Trim("'")
            if ($k -eq "name") {
                if ($inBin -and -not $binName) { $binName = $v }
                elseif (-not $inBin -and -not $pkgName) { $pkgName = $v }
            } elseif ($k -eq "default-run" -and -not $defRun) {
                $defRun = $v
            }
        }
        if ($binName) { $Name = $binName }
        elseif ($defRun) { $Name = $defRun }
        else { $Name = $pkgName }
    }
}
if (-not $Name) { $Name = Split-Path $ROOT -Leaf }

$SRC = Join-Path $TARGET "release\$Name.exe"
if (-not $Live) {
    # keep the live copy next to the artifact so data/ stays in one place
    $LIVE = Join-Path $TARGET "release\${Name}_test.exe"
} elseif (-not [System.IO.Path]::IsPathRooted($Live)) {
    $LIVE = Join-Path $ROOT $Live
} else {
    $LIVE = $Live
}

# ---- mode ----
#   copy    keep a live copy; the artifact is never the running file
#   rename  rename the running artifact aside, then run the fresh artifact
#           directly (no copy). Windows keeps the old process alive under its
#           old image name, which is what makes this work.
$NEED_COPY = $true
if ($Mode -eq "rename") {
    $NEED_COPY = $false
    $LIVE = $SRC
}
$LIVE_LEAF = Split-Path $LIVE -Leaf
$LIVE_BASE = [System.IO.Path]::GetFileNameWithoutExtension($LIVE)

function Test-Elevated {
    try {
        $id = [Security.Principal.WindowsIdentity]::GetCurrent()
        (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    } catch { $false }
}

# ---- release the cargo artifact name ----
# Windows allows renaming a running exe (the image keeps running under the old
# name), so when the caller IS the artifact we move it aside and the linker can
# write a fresh file.
$script:SRC_OLD = ""
function Release-Src {
    if (-not (Test-Path $SRC)) { return }
    $script:SRC_OLD = "$SRC.old"
    if (Test-Path $script:SRC_OLD) { Remove-Item $script:SRC_OLD -Force -ErrorAction SilentlyContinue }
    if (Test-Path $script:SRC_OLD) { $script:SRC_OLD = "$SRC.old$(Get-Random)" }
    Rename-Item -Path $SRC -NewName (Split-Path $script:SRC_OLD -Leaf) -Force -ErrorAction Stop
}
function Restore-Src {
    if ($script:SRC_OLD -and (Test-Path $script:SRC_OLD) -and -not (Test-Path $SRC)) {
        Rename-Item -Path $script:SRC_OLD -NewName (Split-Path $SRC -Leaf) -Force -ErrorAction SilentlyContinue
        $script:SRC_OLD = ""
    }
}
function Cleanup-SrcOld {
    # cannot delete while the caller still runs; next run cleans it up
    if ($script:SRC_OLD -and (Test-Path $script:SRC_OLD)) {
        Remove-Item $script:SRC_OLD -Force -ErrorAction SilentlyContinue
    }
}

# ---- kill old: pid based, never the caller (it may be the artifact itself) ----
# Both the live-copy name and the artifact name are considered, so switching
# modes cannot leave a stale instance behind.
function Get-Targets {
    $seen = @{}
    # artifact name, live copy name and (rename mode) the same artifact again
    $names = @($Name, $LIVE_BASE, "${Name}_test")
    foreach ($n in ($names | Select-Object -Unique)) {
        foreach ($p in @(Get-Process -Name $n -ErrorAction SilentlyContinue)) {
            if ($p.Id -ne $SelfPid -and -not $seen.ContainsKey($p.Id)) {
                $seen[$p.Id] = $p
            }
        }
    }
    return $seen.Values
}
function Kill-Old {
    $targets = @(Get-Targets)
    if ($targets.Count -eq 0) { return }
    if ($Elevate -and -not (Test-Elevated)) {
        foreach ($t in $targets) {
            Start-Process -FilePath "taskkill.exe" -ArgumentList "/F", "/PID", $t.Id `
                -Verb RunAs -WindowStyle Hidden -ErrorAction SilentlyContinue | Out-Null
        }
    } else {
        foreach ($t in $targets) {
            try { Stop-Process -Id $t.Id -Force -ErrorAction SilentlyContinue } catch {}
        }
    }
    for ($i = 0; $i -lt 20; $i++) {
        Start-Sleep -Milliseconds 250
        if (@(Get-Targets).Count -eq 0) { break }
    }
}

# ---- copy + size verify (3 tries). Throws so the caller can restore SRC ----
function Copy-Verify {
    if (-not (Test-Path $SRC)) { throw "no build artifact: $SRC" }
    $sz = (Get-Item $SRC).Length
    for ($i = 1; $i -le 3; $i++) {
        Copy-Item $SRC $LIVE -Force -ErrorAction SilentlyContinue
        $live = if (Test-Path $LIVE) { (Get-Item $LIVE).Length } else { 0 }
        if ($live -eq $sz) { return }
        Start-Sleep -Seconds 1
    }
    throw "size mismatch src=$sz (old instance may still hold $LIVE_LEAF)"
}

# ---- launch ----
function Launch {
    if (-not (Test-Path -LiteralPath $LIVE)) { Out-Err "launch" "no exe: $LIVE" }
    $argList = @()
    if ($LaunchArgs) { $argList = $LaunchArgs.Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries) }
    # -WindowStyle Hidden 同时解决两件事：
    #   1) 控制台程序不弹黑框（控制台窗口建了但隐藏）；
    #   2) 走 ShellExecute，子进程不继承调用方的 stdout 管道 —— 否则
    #      `tba reload build` 会一直等管道 EOF，直到被启动的程序退出（踩过）。
    # GUI 程序的主窗口不受影响（实测 egui 窗口照常显示）。
    # 注意：-ArgumentList 传空数组会报「参数为 Null、为空」，所以空的时候整个不传。
    $sp = @{ FilePath = $LIVE; WorkingDirectory = $ROOT; PassThru = $true; ErrorAction = "SilentlyContinue"; WindowStyle = "Hidden" }
    if ($argList.Count -gt 0) { $sp.ArgumentList = $argList }
    if ($Elevate) { $sp.Verb = "RunAs" }
    $p = Start-Process @sp
    if (-not $p) { Out-Err "launch" "Start-Process returned nothing" }
    $p.Id
}

function Pid-Running {
    $p = Get-Targets | Select-Object -First 1
    if ($p) { $p.Id } else { $null }
}

switch ($Action) {
    "build" {
        # 编译必须在项目根跑：以前靠调用方的 cwd 是项目根，现在参数来自
        # 项目设置（可能是从窗口/别的目录起的），cwd 靠不住。
        if (-not $ROOT -or -not (Test-Path -LiteralPath $ROOT)) {
            Out-Err "root" "no such dir: $ROOT"
        }
        Set-Location -LiteralPath $ROOT
        if ($ReleaseSrc) {
            try { Release-Src } catch { Out-Err "release" "$($_.Exception.Message)" }
        }
        $env:PATH = "$MSVC_BIN;$env:PATH"
        $env:LIB = if ($EXTRA_LIB) { "$MSVC_LIB;$SDK_LIB;$EXTRA_LIB" } else { "$MSVC_LIB;$SDK_LIB" }
        if ($Compile) {
            # 编译指令来自项目设置（Teambition 的 setting 任务）：zig / go / npm 各不相同。
            # 用 cmd /c 跑整串，才能带参数和管道；输出照样进日志，不回灌调用方。
            Add-Content -Path $LOG -Value "[compile] $Compile" -Encoding utf8
            & cmd /c $Compile *>&1 | Out-File -FilePath $LOG -Append -Encoding utf8
            if ($LASTEXITCODE -ne 0) {
                Restore-Src
                Out-Err "compile" "exit $LASTEXITCODE (see $LOG)"
            }
        } else {
            & $CARGO +$Triple build --release *>&1 | Out-File -FilePath $LOG -Append -Encoding utf8
            if ($LASTEXITCODE -ne 0) {
                Restore-Src
                Out-Err "build" "cargo exit $LASTEXITCODE (see $LOG)"
            }
        }
        Kill-Old
        if ($NEED_COPY) {
            try {
                Copy-Verify
            } catch {
                Restore-Src
                Out-Err "copy" "$($_.Exception.Message)"
            }
        }
        $newPid = Launch
        Cleanup-SrcOld
        Out-Ok "OK $newPid"
    }
    "restart" {
        if (-not (Test-Path $LIVE)) { Out-Err "launch" "no $LIVE (run build first)" }
        Kill-Old
        $newPid = Launch
        Out-Ok "OK $newPid"
    }
    "kill" {
        Kill-Old
        Out-Ok "OK"
    }
    "status" {
        $running = Pid-Running
        if ($running) { Out-Ok "RUNNING $running" }
        Out-Ok "STOPPED"
    }
    default { Out-Err "usage" "unknown action: $Action (build|restart|kill|status)" }
}
