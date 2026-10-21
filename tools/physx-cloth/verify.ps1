param(
    [string]$OutputRoot = (Join-Path $PSScriptRoot "../../output/cloth-validation/$(Get-Date -Format 'yyyyMMdd-HHmmss-fff')"),
    [string]$BuildRoot = (Join-Path $PSScriptRoot '../../.deps/physx-cloth-build'),
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$BuildRoot = [IO.Path]::GetFullPath($BuildRoot)
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -BuildRoot $BuildRoot -Target pvd-cloth
}
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$capture = Join-Path $OutputRoot 'cloth.pvdstream'
$truth = Join-Path $OutputRoot 'cloth.cloth.json'
$probe = Join-Path $OutputRoot 'cloth-reused-fabric.pvdstream'
$probeTruth = Join-Path $OutputRoot 'cloth-reused-fabric.cloth.json'
foreach ($path in $capture, $truth, $probe, $probeTruth) {
    if (Test-Path -LiteralPath $path) { throw 'Capture or truth already exists; choose a fresh output directory.' }
}
$producer = Join-Path $BuildRoot 'Release/pvd-cloth.exe'
& $producer $capture $truth
if ($LASTEXITCODE -ne 0) { throw 'The SDK cloth producer failed.' }
& $producer $probe $probeTruth --reuse-fabric-address
if ($LASTEXITCODE -ne 0) { throw 'The SDK Fabric address reuse probe failed.' }
$previousCapture = $env:PVD_CLOTH_CAPTURE
$previousProbe = $env:PVD_CLOTH_REUSE_CAPTURE
Push-Location $projectRoot
try {
    $env:PVD_CLOTH_CAPTURE = $capture
    $env:PVD_CLOTH_REUSE_CAPTURE = $probe
    & cargo test --locked -p pvd-core --lib cloth::tests::
    if ($LASTEXITCODE -ne 0) { throw 'Cloth metadata and resource boundary tests failed.' }
    & cargo test --locked -p pvd-core --test cloth -- --ignored --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'SDK cloth getter truth or stale Fabric diagnostics failed.' }
    Get-FileHash -Algorithm SHA256 $capture, $truth, $probe, $probeTruth | Format-List Path, Hash
} finally {
    $env:PVD_CLOTH_CAPTURE = $previousCapture
    $env:PVD_CLOTH_REUSE_CAPTURE = $previousProbe
    Pop-Location
}
