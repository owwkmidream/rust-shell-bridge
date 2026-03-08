[CmdletBinding()]
param(
    [switch]$SkipExplorerRestart
)

$ErrorActionPreference = 'Stop'

$clsid = '{8E61A8FD-0B37-4AEB-9CE0-9D833295673F}'
$handlerName = 'IObitUnlockerShellBridge'
$registryRoot = [Microsoft.Win32.Registry]::CurrentUser
$paths = @(
    "Software\Classes\AllFilesystemObjects\shellex\ContextMenuHandlers\$handlerName",
    "Software\Classes\CLSID\$clsid"
)

foreach ($path in $paths) {
    try {
        $registryRoot.DeleteSubKeyTree($path, $false)
        Write-Host "已删除注册项: HKCU\$path"
    } catch {
        Write-Host "跳过不存在的注册项: HKCU\$path"
    }
}

try {
    $approvedKey = $registryRoot.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved', $true)
    if ($null -ne $approvedKey) {
        $approvedKey.DeleteValue($clsid, $false)
        $approvedKey.Dispose()
        Write-Host "已删除注册项: HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Shell Extensions\\Approved -> $clsid"
    }
} catch {
    Write-Host "跳过不存在的 Approved 项: $clsid"
}

Write-Host ''

