param(
    [string]$SdkRoot = (Join-Path $PSScriptRoot '..\..\.deps\physx-3.4'),
    [string]$BuildRoot = (Join-Path $PSScriptRoot '..\..\.deps\physx-sample-build'),
    [string]$Toolset = 'v145',
    [string]$Generator = 'Visual Studio 18 2026',
    [int]$Jobs = 2
)

$ErrorActionPreference = 'Stop'
$sdkCommit = '5e42a5f112351a223c19c17bb331e6c55037b8eb'
$SdkRoot = [IO.Path]::GetFullPath($SdkRoot)
$BuildRoot = [IO.Path]::GetFullPath($BuildRoot)

function Invoke-Checked {
    param([string]$Program, [string[]]$ToolArguments)
    & $Program @ToolArguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program failed with exit code $LASTEXITCODE"
    }
}

if ($Jobs -lt 1 -or $Jobs -gt 16) { throw 'Jobs must be between 1 and 16.' }
if (-not (Test-Path -LiteralPath (Join-Path $SdkRoot '.git'))) {
    if (Test-Path -LiteralPath $SdkRoot) { throw "SDK directory exists without Git metadata: $SdkRoot" }
    New-Item -ItemType Directory -Path (Split-Path -Parent $SdkRoot) -Force | Out-Null
    Invoke-Checked git @('clone', '--filter=blob:none', '--no-checkout', 'https://github.com/NVIDIAGameWorks/PhysX-3.4.git', $SdkRoot)
    Invoke-Checked git @('-C', $SdkRoot, 'sparse-checkout', 'set', 'PhysX_3.4/Include', 'PhysX_3.4/Source', 'PhysX_3.4/Snippets', 'PxShared', 'Externals')
    Invoke-Checked git @('-C', $SdkRoot, 'checkout', '--detach', $sdkCommit)
}
$actualCommit = & git -C $SdkRoot rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $actualCommit -ne $sdkCommit) {
    throw "Expected SDK commit $sdkCommit, found $actualCommit; the existing checkout was preserved."
}
& git -C $SdkRoot diff --quiet
if ($LASTEXITCODE -ne 0) { throw 'The SDK has tracked modifications; use a clean pinned checkout.' }

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Visual Studio Build Tools discovery requires an existing vswhere.exe.' }
$vsInstall = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -property installationPath
if (-not $vsInstall) { throw 'No existing Visual Studio MSBuild installation was found.' }
$msbuild = Join-Path $vsInstall 'MSBuild\Current\Bin\MSBuild.exe'
$solution = Join-Path $SdkRoot 'PhysX_3.4\Source\compiler\vc15win64\PhysX.sln'
$overrides = Join-Path $PSScriptRoot 'modern-msvc.props'
New-Item -ItemType Directory -Path $BuildRoot -Force | Out-Null
$sdkLog = Join-Path $BuildRoot 'sdk-build.log'
& $msbuild $solution '/t:PhysX;PhysXExtensions;PhysXCooking' '/p:Configuration=checked' '/p:Platform=x64' "/p:PlatformToolset=$Toolset" '/p:WindowsTargetPlatformVersion=10.0' "/p:ForceImportBeforeCppTargets=$overrides" "/m:$Jobs" '/v:minimal' *> $sdkLog
if ($LASTEXITCODE -ne 0) {
    Get-Content -LiteralPath $sdkLog -Tail 40
    throw "SDK build failed; full log: $sdkLog"
}

Invoke-Checked cmake @('-S', $PSScriptRoot, '-B', $BuildRoot, '-G', $Generator, '-A', 'x64', "-DPHYSX34_ROOT=$SdkRoot")
Invoke-Checked cmake @('--build', $BuildRoot, '--config', 'Release', '--parallel', "$Jobs")
Write-Output (Join-Path $BuildRoot 'Release\pvd-physx-sample.exe')
