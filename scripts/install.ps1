[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'TSF registration requires an elevated PowerShell for this setup. Use the same Windows account.'
}
$required = @('simeji-worker.exe','simeji-cloud.exe','simeji-setup.exe','simeji-launcher.exe','simeji_ime.dll','dict')
foreach ($name in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $PSScriptRoot $name))) { throw 'Run install.ps1 from a package produced by scripts/build.ps1.' }
}
$root = Join-Path $env:LOCALAPPDATA 'SimejiWindows'
$version = Join-Path $root ('Versions\' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss'))
if (Test-Path -LiteralPath $version) { throw "Version directory exists: $version" }
New-Item -ItemType Directory -Path $version -Force | Out-Null
foreach ($name in $required + @('uninstall.ps1','README.md')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $version -Recurse
}
& (Join-Path $version 'simeji-setup.exe') register (Join-Path $version 'simeji_ime.dll')
if ($LASTEXITCODE -ne 0) { throw 'TSF registration failed. Startup was not enabled; retain this package for diagnosis.' }
$launcher = Join-Path $version 'simeji-launcher.exe'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
New-Item -Path $run -Force | Out-Null
New-ItemProperty -Path $run -Name 'SimejiWindows' -Value ('"' + $launcher + '"') -PropertyType String -Force | Out-Null
Write-Output "Installed: $version"
Write-Output "Start $launcher from a normal, non-elevated session (or sign in again)."
Write-Output 'Select Simeji Windows from the Windows input selector. Restart target applications before testing.'
Write-Output 'Cloud is disabled by default. User data: %LOCALAPPDATA%\SimejiWindows\UserData'
