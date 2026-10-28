[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run uninstall.ps1 from an elevated PowerShell.'
}
$setup = Join-Path $PSScriptRoot 'simeji-setup.exe'
if (-not (Test-Path -LiteralPath $setup)) { throw 'Run uninstall.ps1 from the installation directory.' }
& $setup unregister
if ($LASTEXITCODE -ne 0) { throw 'Unregistration failed; the installation was left in place.' }
Remove-ItemProperty -Path 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'SimejiWindows' -ErrorAction SilentlyContinue
Get-Process -Name 'simeji-worker' -ErrorAction SilentlyContinue | Stop-Process -Force
$remaining = 0
foreach ($item in Get-ChildItem -LiteralPath $PSScriptRoot) {
    try { Remove-Item -LiteralPath $item.FullName -Recurse -Force }
    catch { $remaining++ }
}
if ($remaining -eq 0) { Remove-Item -LiteralPath $PSScriptRoot -Force }
Write-Output 'Simeji Windows was unregistered and removed.'
if ($remaining -gt 0) { Write-Output "Restart Windows, then delete $PSScriptRoot; applications still had files open." }
Write-Output 'User data remains in %LOCALAPPDATA%\SimejiWindows.'
