param(
    [string]$OutputRoot = (Join-Path $PSScriptRoot "../../output/objects-validation/$(Get-Date -Format 'yyyyMMdd-HHmmss-fff')"),
    [string]$BuildRoot = (Join-Path $PSScriptRoot '../../.deps/physx-objects-build'),
    [switch]$SkipBuild,
    [switch]$SkipDesktopTests
)

$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$BuildRoot = [IO.Path]::GetFullPath($BuildRoot)
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1') -BuildRoot $BuildRoot -Target pvd-objects
}
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$capture = Join-Path $OutputRoot 'objects.pvdstream'
$truth = Join-Path $OutputRoot 'objects.objects.json'
if ((Test-Path -LiteralPath $capture) -or (Test-Path -LiteralPath $truth)) {
    throw 'Output capture or truth already exists; choose a fresh output directory.'
}
$producer = Join-Path $BuildRoot 'Release/pvd-objects.exe'
& $producer $capture $truth
if ($LASTEXITCODE -ne 0) { throw 'The SDK object producer failed.' }
$previousCapture = $env:PVD_OBJECTS_CAPTURE
Push-Location $projectRoot
try {
    $env:PVD_OBJECTS_CAPTURE = $capture
    & cargo test --locked -p pvd-core --lib articulation_owner
    if ($LASTEXITCODE -ne 0) { throw 'Articulation parent boundary tests failed.' }
    & cargo test --locked -p pvd-core --test object_hierarchy -- --ignored --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'SDK getter truth comparison failed.' }
    if (-not $SkipDesktopTests) {
        & cargo test --locked -p pvd-desktop tree::tests:: -- --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw 'Desktop tree logic tests failed.' }
        & cargo test --locked -p pvd-desktop tree::tests::aggregate_link_tree_tracks_membership_language_and_generation -- --ignored --exact --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw 'SDK object tree comparison failed.' }
    }
    Get-FileHash -Algorithm SHA256 $capture, $truth | Format-List Path, Hash
} finally {
    $env:PVD_OBJECTS_CAPTURE = $previousCapture
    Pop-Location
}
