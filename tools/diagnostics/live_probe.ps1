param(
    [string]$RuntimeRoot = '',
    [string]$SdkSample = '',
    [string]$OutputDirectory = '',
    [int]$ExtraBoxes = 10000,
    [ValidateRange(0, 20000)][int]$ExtraStaticBoxes = 0,
    [int]$Frames = 180,
    [int]$IntervalMilliseconds = 16,
    [int]$TimeoutSeconds = 60,
    [ValidateSet('network', 'file')][string]$Mode = 'network',
    [switch]$WithoutHistory,
    [switch]$ContinuousMotion,
    [switch]$Profile,
    [ValidateRange(0, 200000)][int]$ResidentAllocations = 0,
    [ValidateRange(0, 64)][int]$AnalysisBatchesPerFrame = 0,
    [ValidateRange(0, 3600)][int]$DurationSeconds = 0,
    [string]$TargetDirectory = ''
)
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if (!$RuntimeRoot) { $RuntimeRoot = $projectRoot }
$RuntimeRoot = (Resolve-Path -LiteralPath $RuntimeRoot).Path
$probeDirectory = Join-Path $projectRoot 'crates/pvd-desktop/src/bin'
$probePath = Join-Path $probeDirectory 'live_probe.rs'
if (Test-Path -LiteralPath $probePath) { throw "Diagnostic target already exists: $probePath" }
$variables = @{
    PVD_PROBE_ROOT = $RuntimeRoot
    PVD_SDK34_SAMPLE = $(if ($SdkSample) { (Resolve-Path -LiteralPath $SdkSample).Path } else { $env:PVD_SDK34_SAMPLE })
    PVD_PROBE_OUTPUT = $(if ($OutputDirectory) { [IO.Path]::GetFullPath($OutputDirectory) } else { $env:PVD_PROBE_OUTPUT })
    PVD_PROBE_BOXES = "$ExtraBoxes"
    PVD_PROBE_STATIC_BOXES = $(if ($ExtraStaticBoxes -gt 0) { "$ExtraStaticBoxes" } else { $null })
    PVD_PROBE_FRAMES = "$Frames"
    PVD_PROBE_INTERVAL = "$IntervalMilliseconds"
    PVD_PROBE_TIMEOUT = "$TimeoutSeconds"
    PVD_PROBE_MODE = $Mode
    PVD_PROBE_NO_HISTORY = $(if ($WithoutHistory) { '1' } else { $null })
    PVD_PROBE_CONTINUOUS = $(if ($ContinuousMotion) { '1' } else { $null })
    PVD_PROBE_PROFILE = $(if ($Profile) { '1' } else { $null })
    PVD_PROBE_DURATION_SECONDS = $(if ($DurationSeconds -gt 0) { "$DurationSeconds" } else { $null })
    PVD_PROBE_RESIDENT_ALLOCATIONS = $(if ($ResidentAllocations -gt 0) { "$ResidentAllocations" } else { $null })
    PVD_PROBE_ANALYSIS_BATCHES = $(if ($AnalysisBatchesPerFrame -gt 0) { "$AnalysisBatchesPerFrame" } else { $null })
}
$saved = @{}
foreach ($name in $variables.Keys) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
New-Item -ItemType Directory -Path $probeDirectory -Force | Out-Null
try {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'live_probe.rs') -Destination $probePath
    foreach ($name in $variables.Keys) { [Environment]::SetEnvironmentVariable($name, $variables[$name], 'Process') }
    $arguments = @('run', '--release', '--locked', '--manifest-path', (Join-Path $projectRoot 'Cargo.toml'), '-p', 'pvd-desktop', '--bin', 'live_probe')
    if ($TargetDirectory) { $arguments += @('--target-dir', $TargetDirectory) }
    & cargo @arguments
    if ($LASTEXITCODE -ne 0) { throw "Diagnostic exited with code $LASTEXITCODE" }
} finally {
    Remove-Item -LiteralPath $probePath -ErrorAction SilentlyContinue
    foreach ($name in $variables.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
}
