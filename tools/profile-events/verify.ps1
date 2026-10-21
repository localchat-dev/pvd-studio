param(
    [switch]$SkipBuild,
    [switch]$GenerateOnly,
    [string]$OutputDirectory,
    [string]$BuildRoot = (Join-Path $PSScriptRoot '..\..\.deps\profile-events-build')
)

$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$BuildRoot = [IO.Path]::GetFullPath($BuildRoot)
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $projectRoot ('output/profile-events-validation/' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$capture = Join-Path $OutputDirectory 'physx-3.4-profile-events.pvdstream'
$truth = [IO.Path]::ChangeExtension($capture, 'json')
if ((Test-Path -LiteralPath $capture) -or (Test-Path -LiteralPath $truth)) {
    throw 'The output capture or truth file already exists; choose a fresh output directory.'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
Push-Location -LiteralPath $projectRoot
try {
    if (-not $SkipBuild) { & "$PSScriptRoot/build.ps1" -BuildRoot $BuildRoot }
    $producer = Join-Path $BuildRoot 'Debug/pvd-profile-events.exe'
    if (-not (Test-Path -LiteralPath $producer)) { throw "Build the profile producer: $producer" }
    $process = Start-Process -FilePath $producer -ArgumentList ('"{0}" "{1}"' -f $capture, $truth) `
        -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $OutputDirectory 'producer.stdout.txt') `
        -RedirectStandardError (Join-Path $OutputDirectory 'producer.stderr.txt')
    try {
        if (-not $process.WaitForExit(60000)) { throw 'Profile producer exceeded 60 seconds.' }
        if ($process.ExitCode -ne 0) { throw "Profile producer failed with exit code $($process.ExitCode)." }
    } finally {
        if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
    $expected = Get-Content -LiteralPath $truth -Raw | ConvertFrom-Json
    if ($expected.steps.Count -ne 8 -or $expected.spans.Count -ne 12 -or $expected.pending.Count -ne 1) {
        throw 'The producer did not report the required frame/span/pending coverage.'
    }
    if (-not $GenerateOnly) {
        $previous = [Environment]::GetEnvironmentVariable('PVD_PROFILE_EVENTS_CAPTURE', 'Process')
        try {
            [Environment]::SetEnvironmentVariable('PVD_PROFILE_EVENTS_CAPTURE', $capture, 'Process')
            & cargo test --locked -p pvd-core --test profile_curves -- --include-ignored
            if ($LASTEXITCODE -ne 0) { throw 'Independent SDK profile event comparison failed.' }
            & cargo build --locked -p pvd-cli
            if ($LASTEXITCODE -ne 0) { throw 'CLI build failed.' }
            & "$PSScriptRoot/verify-cli.ps1" -Capture $capture
        } finally {
            [Environment]::SetEnvironmentVariable('PVD_PROFILE_EVENTS_CAPTURE', $previous, 'Process')
        }
    }
    Get-FileHash -LiteralPath @($capture, $truth) -Algorithm SHA256 | Format-Table -AutoSize |
        Out-File -LiteralPath (Join-Path $OutputDirectory 'sha256.txt') -Width 300
    Write-Output "Profile event artifacts: $OutputDirectory"
} finally { Pop-Location }
