param(
    [Parameter(Mandatory)][string]$Capture,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$SecondCapture,
    [ValidateRange(2, 1000)][int]$Rounds = 20,
    [string]$Desktop = 'target/debug/pvd-desktop.exe'
)
$ErrorActionPreference = 'Stop'
$capturePath = (Resolve-Path -LiteralPath $Capture).Path
$desktopPath = (Resolve-Path -LiteralPath $Desktop).Path
$outputPath = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $outputPath) { throw 'OutputDirectory must not exist.' }
$inputs = @($capturePath)
if ($SecondCapture) { $inputs += (Resolve-Path -LiteralPath $SecondCapture).Path }
New-Item -ItemType Directory -Path $outputPath | Out-Null
$start = [Diagnostics.ProcessStartInfo]::new($desktopPath)
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
foreach ($argument in @('--render-lifecycle-check', $capturePath, (Join-Path $outputPath 'run'), "$Rounds")) {
    $start.ArgumentList.Add($argument)
}
if ($SecondCapture) { $start.ArgumentList.Add($inputs[1]) }
$process = [Diagnostics.Process]::new()
$process.StartInfo = $start
$rows = [Collections.Generic.List[object]]::new()
$timer = [Diagnostics.Stopwatch]::StartNew()
$started = $false
try {
    if (-not $process.Start()) { throw 'Could not start lifecycle check.' }
    $started = $true
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    while (-not $process.WaitForExit(500)) {
        try {
            $process.Refresh()
            $rows.Add([pscustomobject]@{
                elapsed_ms = $timer.ElapsedMilliseconds
                working_set_bytes = $process.WorkingSet64
                private_bytes = $process.PrivateMemorySize64
                handles = $process.HandleCount
                threads = $process.Threads.Count
                cpu_ms = $process.TotalProcessorTime.TotalMilliseconds
            })
        }
        catch {
            if ($process.HasExited) { break }
            throw
        }
        if ($timer.Elapsed.TotalSeconds -gt (180 + 360 * $Rounds)) {
            throw 'Lifecycle check exceeded its total time limit.'
        }
    }
    $process.WaitForExit()
    $manifest = @{
        exit_code = $process.ExitCode
        elapsed_ms = $timer.ElapsedMilliseconds
        desktop = @{ path = $desktopPath; sha256 = (Get-FileHash -LiteralPath $desktopPath -Algorithm SHA256).Hash }
        inputs = @($inputs | ForEach-Object { @{ path = $_; sha256 = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash } })
        scope = 'Process counters sampled every 500 ms; retained allocator memory and driver caches are not classified as leaks by a single threshold.'
    }
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $outputPath 'manifest.json') -Encoding utf8
    if ($process.ExitCode -ne 0) { throw "Lifecycle check failed; see $outputPath" }
    Get-Content -LiteralPath (Join-Path $outputPath 'run/lifecycle.json')
}
finally {
    if ($started) {
        if (-not $process.HasExited) { $process.Kill($true) }
        $process.WaitForExit()
        [IO.File]::WriteAllText((Join-Path $outputPath 'stdout.log'), $stdout.GetAwaiter().GetResult())
        [IO.File]::WriteAllText((Join-Path $outputPath 'stderr.log'), $stderr.GetAwaiter().GetResult())
        $rows | Export-Csv -LiteralPath (Join-Path $outputPath 'process.csv') -NoTypeInformation
    }
    $process.Dispose()
}
