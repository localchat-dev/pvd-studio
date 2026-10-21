param(
    [Parameter(Mandatory = $true)][string]$Capture,
    [string]$BaselineCli,
    [ValidateRange(1, 1000)][int]$Seeks = 32,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$capturePath = (Resolve-Path -LiteralPath $Capture).Path
$runDirectory = Join-Path $projectRoot ('output\live-history\' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
New-Item -ItemType Directory -Path $runDirectory | Out-Null

function Measure-HistoryBuild([string]$Executable, [string]$Name) {
    $executablePath = (Resolve-Path -LiteralPath $Executable).Path
    $log = Join-Path $runDirectory "$Name.log"
    $errors = Join-Path $runDirectory "$Name-errors.log"
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $process = Start-Process -FilePath $executablePath -ArgumentList @('bench', ('"' + $capturePath + '"'), "$Seeks") -WindowStyle Hidden -PassThru -RedirectStandardOutput $log -RedirectStandardError $errors
    $peakWorkingSet = 0L
    try {
        while (-not $process.WaitForExit(50)) {
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
        }
        $watch.Stop()
        $measurement = [ordered]@{
            executable = $executablePath
            executable_sha256 = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash
            exit_code = $process.ExitCode
            wall_seconds = $watch.Elapsed.TotalSeconds
            sampled_peak_working_set_bytes = $peakWorkingSet
            sampling_interval_ms = 50
            scope = 'One CLI indexing and random-seek process; not desktop rendering or live snapshots'
            output = $log
            errors = $errors
        }
        $measurement | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $runDirectory "$Name.json")
        return $measurement
    } finally { $process.Dispose() }
}

Push-Location -LiteralPath $projectRoot
try {
    if (-not $SkipBuild) {
        & cargo build --release --locked -p pvd-cli -j 2
        if ($LASTEXITCODE -ne 0) { throw 'Release CLI build failed' }
    }
    $results = [ordered]@{
        capture = $capturePath
        capture_bytes = (Get-Item -LiteralPath $capturePath).Length
        capture_sha256 = (Get-FileHash -LiteralPath $capturePath -Algorithm SHA256).Hash
        random_seeks = $Seeks
    }
    if ($BaselineCli) { $results.baseline = Measure-HistoryBuild $BaselineCli 'baseline' }
    $results.current = Measure-HistoryBuild (Join-Path $projectRoot 'target\release\pvd-cli.exe') 'current'
    $results | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $runDirectory 'comparison.json')
    Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $runDirectory 'cpu.json')
    Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $runDirectory 'memory.json')
    Get-Content -LiteralPath (Join-Path $runDirectory 'comparison.json')
    if ($results.current.exit_code -ne 0) { throw "Current build failed; see $runDirectory" }
    Write-Output "History measurements: $runDirectory"
} finally { Pop-Location }
