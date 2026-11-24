param(
    [string]$TargetDirectory = 'target',
    [string]$OutputDirectory = ('output/unity/' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Push-Location $repo
try {
    $target = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($TargetDirectory)
    $package = Join-Path $OutputDirectory 'package'
    $version = (Get-Content -Raw 'unity/com.pvdstudio.viewer/package.json' | ConvertFrom-Json).version
    $archive = Join-Path $OutputDirectory "com.pvdstudio.viewer-$version.tgz"
    if (Test-Path -LiteralPath $package) { throw "Package already exists: $package" }
    if (Test-Path -LiteralPath $archive) { throw "Archive already exists: $archive" }
    $triple = 'x86_64-pc-windows-msvc'
    cargo build --locked --release --target $triple --target-dir $target -p pvd-unity
    if ($LASTEXITCODE -ne 0) { throw 'pvd-unity build failed' }
    $dll = Join-Path $target "$triple/release/pvd_unity.dll"
    if (!(Test-Path -LiteralPath $dll -PathType Leaf)) { throw "Missing DLL: $dll" }
    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
    Copy-Item -Recurse -LiteralPath 'unity/com.pvdstudio.viewer' -Destination $package
    Copy-Item -LiteralPath $dll -Destination (Join-Path $package 'Editor/Plugins/x86_64/pvd_unity.dll')
    tar.exe -czf $archive -C $OutputDirectory package
    if ($LASTEXITCODE -ne 0) { throw 'Unity package archive failed' }
    Write-Output (Resolve-Path $archive).Path
    Write-Output (Resolve-Path (Join-Path $package 'package.json')).Path
} finally {
    Pop-Location
}
