[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'simeji-windows-target' }
if (-not $env:LIBCLANG_PATH) {
    $env:LIBCLANG_PATH = & python -c "import clang, pathlib; print(pathlib.Path(clang.__file__).parent / 'native')"
    if ($LASTEXITCODE -ne 0) { throw 'Install Python libclang first: python -m pip install libclang ninja' }
}
if (-not (Get-Command ninja -ErrorAction SilentlyContinue)) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere) {
        $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($vs) { $env:PATH = (Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja') + ';' + $env:PATH }
    }
}
Push-Location $root
try {
    & cargo build --workspace --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
    $revision = (& git rev-parse --short HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot determine build revision' }
    if (& git status --porcelain) { $revision += '-dirty' }
    if (-not $OutputDirectory) {
        $OutputDirectory = Join-Path $env:TEMP ('SimejiWindowsPackages\' + $revision + '-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss'))
    }
    if (Test-Path -LiteralPath $OutputDirectory) { throw "Output directory already exists: $OutputDirectory" }
    $package = New-Item -ItemType Directory -Path $OutputDirectory
    foreach ($name in @('simeji_ime.dll', 'simeji-worker.exe', 'simeji-launcher.exe', 'simeji-cloud.exe', 'simeji-setup.exe')) {
        Copy-Item -LiteralPath (Join-Path $env:CARGO_TARGET_DIR ('release\' + $name)) -Destination $package.FullName
    }
    Copy-Item -LiteralPath (Join-Path $root 'assets\dictionary') -Destination (Join-Path $package.FullName 'Dictionary') -Recurse
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install.ps1'), (Join-Path $PSScriptRoot 'uninstall.ps1'), (Join-Path $root 'README.md') -Destination $package.FullName
    & (Join-Path $package.FullName 'simeji-setup.exe') verify (Join-Path $package.FullName 'simeji_ime.dll')
    if ($LASTEXITCODE -ne 0) { throw 'Packaged COM DLL verification failed' }
    Write-Output $package.FullName
} finally { Pop-Location }
