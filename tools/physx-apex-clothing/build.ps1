param(
    [string]$SdkRoot = (Join-Path $PSScriptRoot '..\..\.deps\physx-3.4'),
    [string]$ApexRoot,
    [string]$BuildRoot = (Join-Path $PSScriptRoot '..\..\.deps\physx-apex-clothing-build'),
    [string]$Generator = 'Visual Studio 18 2026',
    [int]$Jobs = 2
)

$ErrorActionPreference = 'Stop'
$SdkRoot = [IO.Path]::GetFullPath($SdkRoot)
$BuildRoot = [IO.Path]::GetFullPath($BuildRoot)
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
if (-not $ApexRoot) { $ApexRoot = Join-Path $SdkRoot 'APEX_1.4' }
$ApexRoot = [IO.Path]::GetFullPath($ApexRoot)
if ($Jobs -lt 1 -or $Jobs -gt 16) { throw 'Jobs must be between 1 and 16.' }

$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dependencies.json') -Raw | ConvertFrom-Json
$priorLazyFetch = $env:GIT_NO_LAZY_FETCH
try {
    $env:GIT_NO_LAZY_FETCH = '1'
    $actualCommit = & git -C $SdkRoot rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $actualCommit -ne $manifest.commit) {
        throw 'The producer requires the pinned PhysX 3.4.2 checkout.'
    }
    & git -C $SdkRoot diff --quiet HEAD
    if ($LASTEXITCODE -ne 0) { throw 'The SDK has tracked modifications; it was preserved.' }
} finally {
    $env:GIT_NO_LAZY_FETCH = $priorLazyFetch
}

$sha = [Security.Cryptography.SHA256]::Create()
try {
    foreach ($entry in $manifest.files) {
        $path = Join-Path $ApexRoot $entry.path
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Missing pinned APEX dependency: $($entry.path). Set -ApexRoot to the complete verified dependency directory."
        }
        $bytes = [IO.File]::ReadAllBytes($path)
        if ($entry.normalize_line_endings) {
            $content = [Text.Encoding]::UTF8.GetString($bytes).Replace("`r`n", "`n")
            $bytes = [Text.Encoding]::UTF8.GetBytes($content)
        }
        $actualHash = [BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant()
        if ($actualHash -ne $entry.sha256) { throw "APEX dependency identity mismatch: $($entry.path)" }
    }
} finally {
    $sha.Dispose()
}

if (-not (Test-Path -LiteralPath (Join-Path $SdkRoot 'PhysX_3.4/Lib/vc15win64/PhysX3CHECKED_x64.lib'))) {
    throw 'Build the pinned PhysX SDK first with tools/physx-sample/build.ps1.'
}
& cmake -S $PSScriptRoot -B $BuildRoot -G $Generator -A x64 "-DPVD_PROJECT_ROOT=$projectRoot" "-DPHYSX34_ROOT=$SdkRoot" "-DAPEX14_ROOT=$ApexRoot"
if ($LASTEXITCODE -ne 0) { throw 'APEX producer configuration failed.' }
& cmake --build $BuildRoot --config Release --parallel $Jobs
if ($LASTEXITCODE -ne 0) { throw 'APEX producer build failed.' }
