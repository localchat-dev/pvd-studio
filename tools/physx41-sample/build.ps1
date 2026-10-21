param([int]$Jobs = 8)
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$sdkRoot = Join-Path $projectRoot '.deps/physx-4.1'
$buildRoot = Join-Path $projectRoot '.deps/physx-build'
$sampleBuild = Join-Path $projectRoot '.deps/physx41-sample-build'
$commit = 'a2c0428acab643e60618c681b501e86f7fd558cc'
function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
if (!(Test-Path -LiteralPath (Join-Path $sdkRoot '.git'))) {
    Invoke-Checked 'git' @('clone', '--depth', '1', '--branch', '4.1', 'https://github.com/NVIDIAGameWorks/PhysX.git', $sdkRoot)
}
$actualCommit = (& git -C $sdkRoot rev-parse HEAD).Trim()
if ($actualCommit -ne $commit) { throw "Expected official SDK commit $commit, found $actualCommit at $sdkRoot" }
$vswherePath = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (!(Test-Path -LiteralPath $vswherePath)) { throw 'Existing Visual Studio C++ build tools were not found.' }
$version = (& $vswherePath -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationVersion).Trim()
$majorVersion = [int]($version.Split('.')[0])
$generator = switch ($majorVersion) { 18 { 'Visual Studio 18 2026' } 17 { 'Visual Studio 17 2022' } 16 { 'Visual Studio 16 2019' } default { throw "Unvalidated Visual Studio version $version" } }
Invoke-Checked 'cmake' @('-S', (Join-Path $sdkRoot 'physx/compiler/public'), '-B', $buildRoot, '-G', $generator, '-A', 'x64',
    '-DTARGET_BUILD_PLATFORM=windows', "-DPHYSX_ROOT_DIR=$sdkRoot/physx", "-DCMAKEMODULES_PATH=$sdkRoot/externals/cmakemodules",
    "-DPXSHARED_PATH=$sdkRoot/pxshared", '-DCMAKEMODULES_VERSION=1.27', "-DPX_OUTPUT_LIB_DIR=$buildRoot", "-DPX_OUTPUT_BIN_DIR=$buildRoot",
    '-DNV_USE_STATIC_WINCRT=ON', '-DPX_GENERATE_STATIC_LIBRARIES=ON', '-DPX_BUILDSNIPPETS=OFF', '-DPX_BUILDPUBLICSAMPLES=OFF', '-DCMAKE_POLICY_VERSION_MINIMUM=3.5')
Invoke-Checked 'cmake' @('--build', $buildRoot, '--config', 'checked', '--target', 'PhysX', 'PhysXExtensions', 'PhysXCooking', 'PhysXPvdSDK', '--parallel', "$Jobs")
$libraries = @(Get-ChildItem -LiteralPath $buildRoot -Filter 'PhysX_static.lib' -Recurse | Where-Object { $_.Directory.Name -eq 'checked' })
if ($libraries.Count -ne 1) { throw 'Could not identify a unique checked static SDK library directory.' }
Invoke-Checked 'cmake' @('-S', $PSScriptRoot, '-B', $sampleBuild, '-G', $generator, '-A', 'x64', "-DPHYSX41_ROOT=$sdkRoot", "-DPHYSX41_LIB=$($libraries[0].Directory.FullName)")
Invoke-Checked 'cmake' @('--build', $sampleBuild, '--config', 'Release', '--parallel', "$Jobs")
Write-Output (Join-Path $sampleBuild 'Release/pvd-physx41-sample.exe')
