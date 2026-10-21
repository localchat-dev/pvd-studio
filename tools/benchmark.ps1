param(
    [Parameter(Mandatory = $true)][string]$Capture,
    [ValidateRange(1, 100000)][int]$Seeks = 1000
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$capturePath = (Resolve-Path -LiteralPath $Capture).Path
Push-Location -LiteralPath $projectRoot
try {
    & cargo build --locked --release -p pvd-cli
    if ($LASTEXITCODE -ne 0) { throw 'Release CLI build failed' }
    $runDirectory = Join-Path $projectRoot ('output\benchmark\' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
    New-Item -ItemType Directory -Path $runDirectory -Force | Out-Null
    $log = Join-Path $runDirectory 'measurements.txt'
    $errors = Join-Path $runDirectory 'errors.txt'
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $process = Start-Process -FilePath (Join-Path $projectRoot 'target\release\pvd-cli.exe') -ArgumentList @('bench', ('"' + $capturePath + '"'), $Seeks) -WindowStyle Hidden -PassThru -RedirectStandardOutput $log -RedirectStandardError $errors
    $peakWorkingSet = 0L
    try {
        while (-not $process.WaitForExit(20)) {
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
        }
        $watch.Stop()
        if ($process.ExitCode -ne 0) { throw (Get-Content -LiteralPath $errors -Raw) }
        @("wall_seconds: $($watch.Elapsed.TotalSeconds)", "observed_peak_working_set_bytes: $peakWorkingSet", 'memory_sampling_ms: 20', 'scope: one CLI process; renderer and native idle CPU are excluded') | Add-Content -LiteralPath $log
    } finally { $process.Dispose() }
    Get-FileHash -Algorithm SHA256 -LiteralPath $capturePath | Format-List | Out-File -LiteralPath (Join-Path $runDirectory 'input-sha256.txt')
    Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors | Format-List | Out-File -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory | Format-List | Out-File -Append -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Get-Content -LiteralPath $log
    Write-Output "Benchmark artifacts: $runDirectory"
} finally { Pop-Location }
