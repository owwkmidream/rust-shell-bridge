[CmdletBinding()]
param(
    [string]$DeployDir,
    [string]$TargetDir
)

$ErrorActionPreference = 'Stop'

$scriptDir = if (-not [string]::IsNullOrWhiteSpace($PSScriptRoot)) {
    $PSScriptRoot
} elseif ($MyInvocation.MyCommand.Path) {
    Split-Path -Parent $MyInvocation.MyCommand.Path
} else {
    (Get-Location).Path
}

if ([string]::IsNullOrWhiteSpace($DeployDir)) {
    $DeployDir = Join-Path $scriptDir '..\deploy'
}

if ([string]::IsNullOrWhiteSpace($TargetDir)) {
    $TargetDir = Join-Path $scriptDir '..\target\mixed'
}

$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $scriptDir '..'))
$deployFullPath = [System.IO.Path]::GetFullPath($DeployDir)
$targetFullPath = [System.IO.Path]::GetFullPath($TargetDir)
$configPath = Join-Path $projectRoot 'iobitunlocker_shell_bridge.ini'
$workerManifestPath = Join-Path $projectRoot 'worker-dll\Cargo.toml'

$shellTarget = 'x86_64-pc-windows-msvc'
$helperTarget = 'i686-pc-windows-msvc'
$requiredTargets = @($shellTarget, $helperTarget)

function Invoke-Step {
    param(
        [string]$Description,
        [scriptblock]$Action
    )

    Write-Host "==> $Description"
    & $Action
}

function Assert-RustTargetInstalled {
    param(
        [string[]]$TargetNames
    )

    $installedTargets = @(rustup target list --installed)
    if ($LASTEXITCODE -ne 0) {
        throw '执行 rustup 失败，无法检查已安装 target。'
    }

    $missingTargets = @(
        $TargetNames | Where-Object { $_ -notin $installedTargets }
    )

    if ($missingTargets.Count -gt 0) {
        $missingList = $missingTargets -join ', '
        $command = 'rustup target add ' + ($missingTargets -join ' ')
        throw "缺少 Rust target: $missingList`n请先执行: $command"
    }
}

Invoke-Step -Description '检查 Rust target' -Action {
    Assert-RustTargetInstalled -TargetNames $requiredTargets
}

Invoke-Step -Description '构建 x64 shell extension DLL' -Action {
    cargo build --release --lib --target $shellTarget --target-dir $targetFullPath
    if ($LASTEXITCODE -ne 0) {
        throw '构建 x64 shell extension DLL 失败。'
    }
}

Invoke-Step -Description '构建 x86 helper EXE' -Action {
    cargo build --release --bin iobitunlocker_shell_bridge_helper --target $helperTarget --target-dir $targetFullPath
    if ($LASTEXITCODE -ne 0) {
        throw '构建 x86 helper EXE 失败。'
    }
}

Invoke-Step -Description '构建 x86 worker DLL' -Action {
    cargo build --release --target $helperTarget --target-dir $targetFullPath --manifest-path $workerManifestPath
    if ($LASTEXITCODE -ne 0) {
        throw '构建 x86 worker DLL 失败。'
    }
}

$shellDllPath = Join-Path $targetFullPath "$shellTarget\release\iobitunlocker_shell_bridge.dll"
$helperExePath = Join-Path $targetFullPath "$helperTarget\release\iobitunlocker_shell_bridge_helper.exe"
$workerDllPath = Join-Path $targetFullPath "$helperTarget\release\iobitunlocker_shell_bridge_worker.dll"

if (-not (Test-Path -LiteralPath $shellDllPath)) {
    throw "未找到构建产物: $shellDllPath"
}

if (-not (Test-Path -LiteralPath $helperExePath)) {
    throw "未找到构建产物: $helperExePath"
}

if (-not (Test-Path -LiteralPath $workerDllPath)) {
    throw "未找到构建产物: $workerDllPath"
}

if (-not (Test-Path -LiteralPath $configPath)) {
    throw "未找到配置文件: $configPath"
}

Invoke-Step -Description '整理 deploy 目录' -Action {
    New-Item -Path $deployFullPath -ItemType Directory -Force | Out-Null

    Copy-Item -LiteralPath $shellDllPath -Destination (Join-Path $deployFullPath 'iobitunlocker_shell_bridge.dll') -Force
    Copy-Item -LiteralPath $helperExePath -Destination (Join-Path $deployFullPath 'iobitunlocker_shell_bridge_helper.exe') -Force
    Copy-Item -LiteralPath $workerDllPath -Destination (Join-Path $deployFullPath 'iobitunlocker_shell_bridge_worker.dll') -Force
    Copy-Item -LiteralPath $configPath -Destination (Join-Path $deployFullPath 'iobitunlocker_shell_bridge.ini') -Force
}

Write-Host ''
Write-Host '已生成 deploy 目录:'
Write-Host "  $deployFullPath"
Write-Host ''
Write-Host '部署目录文件:'
Write-Host ("  shell ext dll : {0}" -f (Join-Path $deployFullPath 'iobitunlocker_shell_bridge.dll'))
Write-Host ("  helper exe    : {0}" -f (Join-Path $deployFullPath 'iobitunlocker_shell_bridge_helper.exe'))
Write-Host ("  worker dll    : {0}" -f (Join-Path $deployFullPath 'iobitunlocker_shell_bridge_worker.dll'))
Write-Host ("  config ini    : {0}" -f (Join-Path $deployFullPath 'iobitunlocker_shell_bridge.ini'))
Write-Host ''
Write-Host '后续请手动把以下 IObit 原文件也放到 deploy 同目录:'
Write-Host '  IObitUnlocker.exe'
Write-Host '  IObitUnlocker.dll'
Write-Host '  IObitUnlocker.sys'
