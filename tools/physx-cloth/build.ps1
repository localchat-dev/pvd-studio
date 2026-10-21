param(
    [string]$SdkRoot = (Join-Path $PSScriptRoot '..\..\.deps\physx-3.4'),
    [string]$BuildRoot = (Join-Path $PSScriptRoot '..\..\.deps\physx-cloth-build'),
    [string]$Generator = 'Visual Studio 18 2026',
    [string]$Target,
    [int]$Jobs = 2
)

$ErrorActionPreference = 'Stop'
$SdkRoot = [IO.Path]::GetFullPath($SdkRoot)
$BuildRoot = [IO.Path]::GetFullPath($BuildRoot)
$expectedCommit = '5e42a5f112351a223c19c17bb331e6c55037b8eb'
$actualCommit = & git -C $SdkRoot rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $actualCommit -ne $expectedCommit) { throw 'The feature producers require the pinned PhysX 3.4.2 checkout.' }
& git -C $SdkRoot diff --quiet
if ($LASTEXITCODE -ne 0) { throw 'The SDK checkout has tracked modifications; it was preserved.' }
if (-not (Test-Path -LiteralPath (Join-Path $SdkRoot 'PhysX_3.4/Lib/vc15win64/PhysX3CHECKED_x64.lib'))) {
    throw 'Build the existing SDK first with tools/physx-sample/build.ps1.'
}
if ($Jobs -lt 1 -or $Jobs -gt 16) { throw 'Jobs must be between 1 and 16.' }
& cmake -S $PSScriptRoot -B $BuildRoot -G $Generator -A x64 "-DPHYSX34_ROOT=$SdkRoot"
if ($LASTEXITCODE -ne 0) { throw 'Feature producer configuration failed.' }
$buildArguments = @('--build', $BuildRoot, '--config', 'Release', '--parallel', "$Jobs")
if ($Target) { $buildArguments += @('--target', $Target) }
& cmake @buildArguments
if ($LASTEXITCODE -ne 0) { throw 'Feature producer build failed.' }
