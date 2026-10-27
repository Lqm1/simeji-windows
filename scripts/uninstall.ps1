[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run from an elevated PowerShell using the same Windows account.' }
$setup = Join-Path $PSScriptRoot 'simeji-setup.exe'
if (-not (Test-Path -LiteralPath $setup)) { throw 'Run uninstall.ps1 from the installed version directory.' }
& $setup unregister
if ($LASTEXITCODE -ne 0) { throw 'TSF unregister failed; startup and user data were retained.' }
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Remove-ItemProperty -Path $run -Name 'SimejiWindows' -ErrorAction SilentlyContinue
$root = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'SimejiWindows\Versions')) + [IO.Path]::DirectorySeparatorChar
Get-Process -Name 'simeji-worker' -ErrorAction SilentlyContinue | ForEach-Object {
    if ($_.Path -and [IO.Path]::GetFullPath($_.Path).StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { Stop-Process -Id $_.Id }
}
Write-Output 'IME registration and automatic startup removed. Restart applications to unload the DLL.'
Write-Output 'Installed files and user dictionaries remain in %LOCALAPPDATA%\SimejiWindows.'
