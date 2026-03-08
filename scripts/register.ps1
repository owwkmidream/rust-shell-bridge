[CmdletBinding()]
param(
    [string]$DllPath,
    [switch]$SkipExplorerRestart
)

$ErrorActionPreference = 'Stop'

$scriptDir = if (-not [string]::IsNullOrWhiteSpace($PSScriptRoot)) {
    $PSScriptRoot
} elseif ($MyInvocation.MyCommand.Path) {
    Split-Path -Parent $MyInvocation.MyCommand.Path
} else {
    (Get-Location).Path
}

if ([string]::IsNullOrWhiteSpace($DllPath)) {
    $DllPath = Join-Path $scriptDir '..\deploy\iobitunlocker_shell_bridge.dll'
}

$clsid = '{8E61A8FD-0B37-4AEB-9CE0-9D833295673F}'
$handlerName = 'IObitUnlockerShellBridge'
$defaultDisplayName = 'Iobit Unlocker快捷操作'

function Get-DisplayName {
    param(
        [string]$ConfigPath
    )

    if (-not (Test-Path -LiteralPath $ConfigPath)) {
        return $defaultDisplayName
    }

    foreach ($line in [System.IO.File]::ReadAllLines($ConfigPath, [System.Text.Encoding]::UTF8)) {
        $trimmed = $line.Trim()
        if ([string]::IsNullOrWhiteSpace($trimmed)) { continue }
        if ($trimmed.StartsWith('#') -or $trimmed.StartsWith(';')) { continue }
        $pair = $trimmed.Split('=', 2)
        if ($pair.Length -ne 2) { continue }
        if ($pair[0].Trim() -ne 'root_menu_text') { continue }

        $value = $pair[1].Trim()
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            return $value
        }
    }

    return $defaultDisplayName
}

$dllFullPath = [System.IO.Path]::GetFullPath($DllPath)
if (-not (Test-Path -LiteralPath $dllFullPath)) {
    throw "找不到 DLL：$dllFullPath`n请先执行 .\scripts\deploy.ps1，或显式传入 -DllPath。"
}

$deployDir = Split-Path $dllFullPath -Parent
$helperPath = Join-Path $deployDir 'iobitunlocker_shell_bridge_helper.exe'
$workerDllPath = Join-Path $deployDir 'iobitunlocker_shell_bridge_worker.dll'
$configPath = Join-Path $deployDir 'iobitunlocker_shell_bridge.ini'
$unlockerExePath = Join-Path $deployDir 'IObitUnlocker.exe'
$unlockerDllPath = Join-Path $deployDir 'IObitUnlocker.dll'
$unlockerSysPath = Join-Path $deployDir 'IObitUnlocker.sys'
$displayName = Get-DisplayName -ConfigPath $configPath

$registryRoot = [Microsoft.Win32.Registry]::CurrentUser
$clsidKey = $registryRoot.CreateSubKey("Software\Classes\CLSID\$clsid")
$clsidKey.SetValue('', $displayName, [Microsoft.Win32.RegistryValueKind]::String)

$inprocKey = $registryRoot.CreateSubKey("Software\Classes\CLSID\$clsid\InprocServer32")
$inprocKey.SetValue('', $dllFullPath, [Microsoft.Win32.RegistryValueKind]::String)
$inprocKey.SetValue('ThreadingModel', 'Apartment', [Microsoft.Win32.RegistryValueKind]::String)

$defaultIconKey = $registryRoot.CreateSubKey("Software\Classes\CLSID\$clsid\DefaultIcon")
if (Test-Path -LiteralPath $unlockerExePath) {
    $defaultIconKey.SetValue('', "$unlockerExePath,0", [Microsoft.Win32.RegistryValueKind]::String)
}

$approvedKey = $registryRoot.CreateSubKey('Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved')
$approvedKey.SetValue($clsid, $displayName, [Microsoft.Win32.RegistryValueKind]::String)

$handlerKey = $registryRoot.CreateSubKey("Software\Classes\AllFilesystemObjects\shellex\ContextMenuHandlers\$handlerName")
$handlerKey.SetValue('', $clsid, [Microsoft.Win32.RegistryValueKind]::String)

$handlerKey.Dispose()
$approvedKey.Dispose()
$defaultIconKey.Dispose()
$inprocKey.Dispose()
$clsidKey.Dispose()

Write-Host "已注册右键扩展（当前用户）:"
Write-Host "  DLL     = $dllFullPath"
Write-Host "  CLSID   = $clsid"
Write-Host "  Handler = $handlerName"
Write-Host "  Name    = $displayName"
if (Test-Path -LiteralPath $unlockerExePath) {
    Write-Host "  Icon    = $unlockerExePath,0"
}

Write-Host ''
Write-Host '同目录文件检查:'
Write-Host ("  helper.exe       : {0}" -f (Test-Path -LiteralPath $helperPath))
Write-Host ("  worker.dll       : {0}" -f (Test-Path -LiteralPath $workerDllPath))
Write-Host ("  shell config ini : {0}" -f (Test-Path -LiteralPath $configPath))
Write-Host ("  IObitUnlocker.exe: {0}" -f (Test-Path -LiteralPath $unlockerExePath))
Write-Host ("  IObitUnlocker.dll: {0}" -f (Test-Path -LiteralPath $unlockerDllPath))
Write-Host ("  IObitUnlocker.sys: {0}" -f (Test-Path -LiteralPath $unlockerSysPath))

Write-Host ''
