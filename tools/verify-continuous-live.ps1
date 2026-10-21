param(
    [string]$CapturePath = 'output/continuous-live-validation/long.pvdstream',
    [string]$OutputDirectory = 'output/continuous-live-validation/measured',
    [switch]$Release
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
Push-Location $workspace
try {
    $capture = (Resolve-Path -LiteralPath $CapturePath).Path
    $truth = [System.IO.Path]::ChangeExtension($capture, '.json')
    if (-not (Test-Path -LiteralPath $truth -PathType Leaf)) {
        throw "Missing independent SDK pose truth: $truth"
    }
    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
    $destination = (Resolve-Path -LiteralPath $OutputDirectory).Path
    $arguments = @('test', '--locked', '--offline', '-p', 'pvd-core', '--test', 'live_disk', '--no-run', '--message-format=json')
    if ($Release) { $arguments += '--release' }
    $build = & cargo @arguments
    if ($LASTEXITCODE -ne 0) { throw "Core test build failed: $LASTEXITCODE" }
    $artifacts = @($build | ForEach-Object {
        $message = $_ | ConvertFrom-Json
        if ($message.reason -eq 'compiler-artifact' -and $message.target.name -eq 'live_disk' -and $message.executable) {
            $message.executable
        }
    })
    if ($artifacts.Count -ne 1) { throw 'Expected exactly one live_disk test executable.' }
    $executableHash = (Get-FileHash -LiteralPath $artifacts[0] -Algorithm SHA256).Hash.ToLowerInvariant()

    $previousCapture = $env:PVD_LIVE_LONG_CAPTURE
    $previousReport = $env:PVD_LIVE_LONG_REPORT
    $env:PVD_LIVE_LONG_CAPTURE = $capture
    $env:PVD_LIVE_LONG_REPORT = Join-Path $destination 'core-validation.json'
    try {
        $started = [System.Diagnostics.Stopwatch]::StartNew()
        $process = Start-Process -FilePath $artifacts[0] -ArgumentList @(
            'real_long_capture_keeps_current_memory_stable_and_rolls_disk_history', '--ignored', '--nocapture'
        ) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $destination 'test.stdout.log') -RedirectStandardError (Join-Path $destination 'test.stderr.log')
        $samples = [System.Collections.Generic.List[object]]::new()
        $peakWorkingSet = 0L
        $peakPrivate = 0L
        while (-not $process.HasExited) {
            $process.Refresh()
            try {
                $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
                $peakPrivate = [Math]::Max($peakPrivate, $process.PrivateMemorySize64)
                $samples.Add([pscustomobject]@{
                    seconds = $started.Elapsed.TotalSeconds
                    working_set_bytes = $process.WorkingSet64
                    private_bytes = $process.PrivateMemorySize64
                    os_peak_working_set_bytes = $process.PeakWorkingSet64
                })
            } catch [System.InvalidOperationException] {
                if (-not $process.HasExited) { throw }
            }
            Start-Sleep -Milliseconds 250
        }
        $process.WaitForExit()
        $started.Stop()
        $exitCode = $process.ExitCode
        $samples | Export-Csv -LiteralPath (Join-Path $destination 'process-memory.csv') -NoTypeInformation -Encoding utf8
        $processor = Get-CimInstance Win32_Processor | Select-Object -First 1
        $machine = Get-CimInstance Win32_ComputerSystem
        $operatingSystem = Get-CimInstance Win32_OperatingSystem
        $summary = [ordered]@{
            exit_code = $exitCode
            executable = $artifacts[0]
            executable_sha256 = $executableHash
            capture = $capture
            capture_bytes = (Get-Item -LiteralPath $capture).Length
            capture_sha256 = (Get-FileHash -LiteralPath $capture -Algorithm SHA256).Hash.ToLowerInvariant()
            truth_sha256 = (Get-FileHash -LiteralPath $truth -Algorithm SHA256).Hash.ToLowerInvariant()
            elapsed_seconds = $started.Elapsed.TotalSeconds
            peak_working_set_bytes = $peakWorkingSet
            sampled_peak_private_bytes = $peakPrivate
            sample_interval_milliseconds = 250
            samples = $samples.Count
            cpu = $processor.Name
            logical_processors = $machine.NumberOfLogicalProcessors
            installed_ram_bytes = $machine.TotalPhysicalMemory
            operating_system = $operatingSystem.Caption
            operating_system_version = $operatingSystem.Version
            build_profile = $(if ($Release) { 'release' } else { 'debug' })
            conditions = 'Background core integration test; includes SDK truth JSON and display state; excludes Bevy/UI. Other machine workloads are not isolated.'
        }
        $summary | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $destination 'process-summary.json') -Encoding utf8
        $summary | ConvertTo-Json -Depth 5
        if ($exitCode -ne 0) { throw "Continuous live validation failed: $exitCode. See test logs in $destination" }
    } finally {
        $env:PVD_LIVE_LONG_CAPTURE = $previousCapture
        $env:PVD_LIVE_LONG_REPORT = $previousReport
    }
} finally {
    Pop-Location
}
