param(
    [string]$SdkRoot = (Join-Path $PSScriptRoot '..\..\.deps\physx-3.4'),
    [string]$BuildRoot = (Join-Path $PSScriptRoot '..\..\.deps\physx-features-build'),
    [string]$Generator = 'Visual Studio 18 2026',
    [string]$Toolset = 'v145',
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
if (-not $Target -or $Target -eq 'pvd-cct') {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Character controller SDK build requires an existing vswhere.exe.' }
    $vsInstall = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -property installationPath
    if (-not $vsInstall) { throw 'No existing Visual Studio MSBuild installation was found.' }
    $msbuild = Join-Path $vsInstall 'MSBuild\Current\Bin\MSBuild.exe'
    $solution = Join-Path $SdkRoot 'PhysX_3.4\Source\compiler\vc15win64\PhysX.sln'
    $overrides = Join-Path $PSScriptRoot '..\physx-sample\modern-msvc.props'
    New-Item -ItemType Directory -Path $BuildRoot -Force | Out-Null
    $sdkLog = Join-Path $BuildRoot 'character-sdk-build.log'
    & $msbuild $solution '/t:PhysXCharacterKinematic' '/p:Configuration=checked' '/p:Platform=x64' "/p:PlatformToolset=$Toolset" '/p:WindowsTargetPlatformVersion=10.0' "/p:ForceImportBeforeCppTargets=$overrides" "/m:$Jobs" '/v:minimal' *> $sdkLog
    if ($LASTEXITCODE -ne 0) {
        Get-Content -LiteralPath $sdkLog -Tail 40
        throw "Character controller SDK build failed; full log: $sdkLog"
    }
}
& cmake -S $PSScriptRoot -B $BuildRoot -G $Generator -A x64 "-DPHYSX34_ROOT=$SdkRoot"
if ($LASTEXITCODE -ne 0) { throw 'Feature producer configuration failed.' }
$buildArguments = @('--build', $BuildRoot, '--config', 'Release', '--parallel', "$Jobs")
if ($Target) { $buildArguments += @('--target', $Target) }
& cmake @buildArguments
if ($LASTEXITCODE -ne 0) { throw 'Feature producer build failed.' }
