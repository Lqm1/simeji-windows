[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run install.ps1 from an elevated PowerShell of the user who will type with Simeji Windows.'
}
$files = @('simeji_ime.dll', 'simeji-worker.exe', 'simeji-launcher.exe', 'simeji-cloud.exe', 'simeji-setup.exe', 'uninstall.ps1', 'README.md')
foreach ($name in $files + @('Dictionary', 'migrate-user-data.ps1')) {
    if (-not (Test-Path -LiteralPath (Join-Path $PSScriptRoot $name))) { throw 'Run install.ps1 from a package produced by scripts/build.ps1.' }
}
$product = Join-Path $env:ProgramFiles 'SimejiWindows'
$setup = Join-Path $PSScriptRoot 'simeji-setup.exe'
Get-Process -Name 'simeji-worker' -ErrorAction SilentlyContinue | Stop-Process -Force
& (Join-Path $PSScriptRoot 'migrate-user-data.ps1')
New-Item -ItemType Directory -Path $product -Force | Out-Null
foreach ($name in $files) {
    $target = Join-Path $product $name
    if (Test-Path -LiteralPath $target) { & $setup remove $target }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $target
}
$dictionary = Join-Path $product 'Dictionary'
if (Test-Path -LiteralPath $dictionary) { & $setup remove $dictionary }
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'Dictionary') -Destination $dictionary -Recurse
& (Join-Path $product 'simeji-setup.exe') register (Join-Path $product 'simeji_ime.dll')
if ($LASTEXITCODE -ne 0) { throw 'Registration failed. Automatic startup was not enabled.' }
$launcher = Join-Path $product 'simeji-launcher.exe'
New-ItemProperty -Path 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'SimejiWindows' -Value ('"' + $launcher + '"') -PropertyType String -Force | Out-Null
Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'SimejiWindows' -ErrorAction SilentlyContinue
Write-Output "Installed: $product"
Write-Output "Start $launcher from a normal, non-elevated session, or sign in again."
Write-Output 'Select Simeji Windows from the input selector and restart applications that were open.'
& (Join-Path $product 'simeji-cloud.exe') status
Write-Output 'User data: %LOCALAPPDATA%\SimejiWindows'
