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
& $setup remove $PSScriptRoot
if ($LASTEXITCODE -ne 0) { throw "Could not remove $PSScriptRoot." }
Write-Output 'Simeji Windows was unregistered and removed.'
Write-Output 'User data remains in %LOCALAPPDATA%\SimejiWindows.'
