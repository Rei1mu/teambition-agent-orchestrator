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
#    TB_MSVC_BIN / TB_MSVC_LIB / TB_SDK_LIB / TB_EXTRA_LIB / TB_CARGO
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
$MSVC_BIN = if ($env:TB_MSVC_BIN) { $env:TB_MSVC_BIN } else { "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64" }
$MSVC_LIB = if ($env:TB_MSVC_LIB) { $env:TB_MSVC_LIB } else { "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.44.35207\lib\x64" }
$SDK_LIB  = if ($env:TB_SDK_LIB)  { $env:TB_SDK_LIB }  else { "D:\d\winsdk_libs" }
$EXTRA_LIB = $env:TB_EXTRA_LIB
$CARGO    = if ($env:TB_CARGO) { $env:TB_CARGO } else { "C:\Users\user\.cargo\bin\cargo.exe" }

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
    $argList = @()
    if ($LaunchArgs) { $argList = $LaunchArgs.Split(" ", [System.StringSplitOptions]::RemoveEmptyEntries) }
    if ($Elevate) {
        $p = Start-Process -FilePath $LIVE -ArgumentList $argList -WorkingDirectory $ROOT -Verb RunAs -PassThru -ErrorAction SilentlyContinue
    } else {
        $p = Start-Process -FilePath $LIVE -ArgumentList $argList -WorkingDirectory $ROOT -PassThru -ErrorAction SilentlyContinue
    }
    if (-not $p) { Out-Err "launch" "Start-Process returned nothing" }
    $p.Id
}

function Pid-Running {
    $p = Get-Targets | Select-Object -First 1
    if ($p) { $p.Id } else { $null }
}

switch ($Action) {
    "build" {
        if ($ReleaseSrc) {
            try { Release-Src } catch { Out-Err "release" "$($_.Exception.Message)" }
        }
        $env:PATH = "$MSVC_BIN;$env:PATH"
        $env:LIB = if ($EXTRA_LIB) { "$MSVC_LIB;$SDK_LIB;$EXTRA_LIB" } else { "$MSVC_LIB;$SDK_LIB" }
        & $CARGO +$Triple build --release *>&1 | Out-File -FilePath $LOG -Append -Encoding utf8
        if ($LASTEXITCODE -ne 0) {
            Restore-Src
            Out-Err "build" "cargo exit $LASTEXITCODE (see $LOG)"
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
