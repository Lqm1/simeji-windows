[CmdletBinding()]
param([string]$LocalAppData = $env:LOCALAPPDATA)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Join-Path $LocalAppData 'SimejiWindows'
$old = Join-Path $root 'UserData'
if (Test-Path -LiteralPath $old) {
    $learning = Join-Path $root 'Learning'
    New-Item -ItemType Directory -Path $learning -Force | Out-Null
    foreach ($file in Get-ChildItem -LiteralPath $old -File) {
        switch -Wildcard ($file.Name) {
            'cloud-enabled.json' {
                $settings = Join-Path $root 'settings.json'
                if (-not (Test-Path -LiteralPath $settings)) {
                    $enabled = (Get-Content -LiteralPath $file.FullName -Raw).Trim() -eq 'true'
                    [IO.File]::WriteAllText($settings, (@{ cloud_conversion = $enabled } | ConvertTo-Json))
                }
                Remove-Item -LiteralPath $file.FullName
            }
            'user-dictionary.json' {
                $target = Join-Path $root $file.Name
                if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $file.FullName }
                else { Move-Item -LiteralPath $file.FullName -Destination $target }
            }
            'cloud-cache.json' { Remove-Item -LiteralPath $file.FullName }
            '*.tmp' { Remove-Item -LiteralPath $file.FullName }
            default {
                $target = Join-Path $learning $file.Name
                if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $file.FullName }
                else { Move-Item -LiteralPath $file.FullName -Destination $target }
            }
        }
    }
    if (-not (Get-ChildItem -LiteralPath $old -Force)) { Remove-Item -LiteralPath $old }
}
$versions = Join-Path $root 'Versions'
if (Test-Path -LiteralPath $versions) {
    Remove-Item -LiteralPath $versions -Recurse -Force -ErrorAction SilentlyContinue
}
