#requires -Version 7.0
<#
.SYNOPSIS
Verifies an existing CLI listener through an IPv4 address assigned to this computer.
.EXAMPLE
./tools/verify-lan.ps1 -LocalAddress 192.168.1.20 -Configuration Release
.EXAMPLE
./tools/verify-lan.ps1 -LocalAddress 192.168.1.20 -RunSdkTruthTest
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$LocalAddress,
    [ValidateRange(1, 65535)]
    [int]$Port = 15435,
    [string]$BindAddress = '0.0.0.0',
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',
    [ValidateRange(10, 600)]
    [int]$TimeoutSeconds = 60,
    [switch]$RunSdkTruthTest
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path

function Read-IPv4Address {
    param([string]$Value, [string]$ParameterName)
    $parsed = $null
    if ($Value -notmatch '^\d{1,3}(\.\d{1,3}){3}$' -or
        -not [Net.IPAddress]::TryParse($Value, [ref]$parsed) -or
        $parsed.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork) {
        throw "$ParameterName must be an IPv4 address in dotted-decimal notation."
    }
    return $parsed
}

$localIp = Read-IPv4Address $LocalAddress 'LocalAddress'
$bindIp = Read-IPv4Address $BindAddress 'BindAddress'
if ([Net.IPAddress]::IsLoopback($localIp) -or $localIp.Equals([Net.IPAddress]::Any)) {
    throw 'LocalAddress must be a non-loopback IPv4 address assigned to an active local network interface.'
}
if (-not $bindIp.Equals([Net.IPAddress]::Any) -and -not $bindIp.Equals($localIp)) {
    throw 'BindAddress must be 0.0.0.0 or LocalAddress to accept this test connection.'
}
$interfaces = @([Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces() | Where-Object {
    $_.OperationalStatus -eq [Net.NetworkInformation.OperationalStatus]::Up -and
    $_.NetworkInterfaceType -ne [Net.NetworkInformation.NetworkInterfaceType]::Loopback -and
    @($_.GetIPProperties().UnicastAddresses | Where-Object { $_.Address.Equals($localIp) }).Count -gt 0
})
if ($interfaces.Count -eq 0) { throw "LocalAddress $localIp is not assigned to an active local network interface." }

$cli = Join-Path $projectRoot ('target\' + $Configuration.ToLowerInvariant() + '\pvd-cli.exe')
$producer = Join-Path $projectRoot '.deps\physx-sample-build\Release\pvd-physx-sample.exe'
foreach ($program in @($cli, $producer)) {
    if (-not (Test-Path -LiteralPath $program -PathType Leaf)) { throw "Required existing executable is missing: $program. Build it separately before running this script." }
}
$cargo = $null
if ($RunSdkTruthTest) {
    $cargo = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
}

$outputRoot = [IO.Path]::GetFullPath((Join-Path $projectRoot 'output'))
$runDirectory = [IO.Path]::GetFullPath((Join-Path $outputRoot ('lan-validation\' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))))
if (-not $runDirectory.StartsWith($outputRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Validation output escaped the workspace output directory.'
}
New-Item -ItemType Directory -Path $runDirectory | Out-Null
$ownedProcesses = [Collections.Generic.List[Diagnostics.Process]]::new()
$cleanupErrors = [Collections.Generic.List[string]]::new()
$result = [ordered]@{
    status = 'running'
    scope = 'Same-machine connection to an assigned non-loopback IPv4 address; not a remote-host ingress or firewall test.'
    local_address = $localIp.ToString()
    bind_address = $bindIp.ToString()
    port = $Port
    interfaces = @($interfaces | ForEach-Object { [ordered]@{ name = $_.Name; description = $_.Description; type = $_.NetworkInterfaceType.ToString() } })
    configuration = $Configuration
    cli = $cli
    producer = $producer
    capture = $null
    truth_csv = $null
    sdk_truth_test = if ($RunSdkTruthTest) { 'pending' } else { 'not_requested' }
    started_utc = [DateTime]::UtcNow.ToString('o')
    finished_utc = $null
    error = $null
    cleanup_errors = @()
}

function Start-LoggedProcess {
    param([string]$Program, [string[]]$ToolArguments, [string]$LogName)
    $quoted = foreach ($argument in $ToolArguments) {
        if ($argument.Contains('"')) { throw 'Native argument contains an unsupported quotation mark.' }
        if ($argument -match '\s') { '"' + $argument + '"' } else { $argument }
    }
    $process = Start-Process -FilePath $Program -ArgumentList $quoted -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $runDirectory "$LogName.txt") `
        -RedirectStandardError (Join-Path $runDirectory "$LogName-errors.txt")
    $ownedProcesses.Add($process)
    return $process
}

function Wait-LoggedProcess {
    param([Diagnostics.Process]$Process, [string]$LogName, [int]$Seconds)
    if (-not $Process.WaitForExit($Seconds * 1000)) { throw "$LogName timed out after $Seconds seconds. Logs: $runDirectory" }
    $Process.WaitForExit()
    if ($Process.ExitCode -ne 0) {
        $detail = Get-Content -LiteralPath (Join-Path $runDirectory "$LogName-errors.txt") -Raw
        throw "$LogName failed with exit code $($Process.ExitCode): $detail`nLogs: $runDirectory"
    }
}

$failure = $null
try {
    $occupied = @([Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners() | Where-Object { $_.Port -eq $Port })
    if ($occupied.Count -gt 0) {
        throw "TCP port $Port already has listeners: $($occupied -join ', '). Select a free -Port; this script does not stop existing listeners."
    }
    $captureDirectory = Join-Path $runDirectory 'tcp'
    $listener = Start-LoggedProcess $cli @('listen', "$($bindIp):$Port", $captureDirectory, "$TimeoutSeconds") 'listener'
    $ready = $false
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        if ($listener.HasExited) { Wait-LoggedProcess $listener 'listener' 1; throw 'Listener exited before becoming ready.' }
        $log = Join-Path $runDirectory 'listener.txt'
        if ((Test-Path -LiteralPath $log) -and (Get-Content -LiteralPath $log -Raw) -match ('(?m)^listening: ' + [Regex]::Escape("$($bindIp):$Port") + '\s*$')) {
            $ready = $true
            break
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $ready) { throw "Listener startup timed out after 10 seconds. Logs: $runDirectory" }

    $truthCsv = Join-Path $runDirectory 'tcp.csv'
    $source = Start-LoggedProcess $producer @('--host', $localIp.ToString(), '--port', "$Port", '--frames', '120', '--flags', 'debug', '--lifecycle', '--truth', $truthCsv) 'producer'
    Wait-LoggedProcess $source 'producer' $TimeoutSeconds
    Wait-LoggedProcess $listener 'listener' 15
    $captures = @(Get-ChildItem -LiteralPath $captureDirectory -File -Filter '*.pvdstream')
    if ($captures.Count -ne 1 -or $captures[0].Length -eq 0) { throw "Expected one nonempty completed TCP capture; found $($captures.Count). Logs: $runDirectory" }
    if (-not (Test-Path -LiteralPath $truthCsv -PathType Leaf) -or (Get-Item -LiteralPath $truthCsv).Length -eq 0) { throw 'SDK producer did not write the independent pose CSV.' }
    $capture = $captures[0].FullName
    $matchingTruth = [IO.Path]::ChangeExtension($capture, 'csv')
    Copy-Item -LiteralPath $truthCsv -Destination $matchingTruth
    $result.capture = $capture
    $result.truth_csv = $matchingTruth

    $verification = Start-LoggedProcess $cli @('verify', $capture) 'verify'
    Wait-LoggedProcess $verification 'verify' $TimeoutSeconds
    if ((Get-Content -LiteralPath (Join-Path $runDirectory 'verify.txt') -Raw) -notmatch '(?m)^verified_frames: 120\s*$') {
        throw 'Replay verification did not report the expected 120 frames.'
    }
    if ($RunSdkTruthTest) {
        $previousCapture = $env:PVD_SDK34_CAPTURE
        try {
            $env:PVD_SDK34_CAPTURE = $capture
            $testArguments = @('test', '--locked', '-p', 'pvd-core', '--test', 'sdk34')
            if ($Configuration -eq 'Release') { $testArguments += '--release' }
            $testArguments += @('official_sdk_capture_matches_independent_sdk_poses_and_random_replay', '--', '--ignored', '--nocapture')
            $truthTest = Start-LoggedProcess $cargo $testArguments 'sdk-truth'
            Wait-LoggedProcess $truthTest 'sdk-truth' ([Math]::Max(300, $TimeoutSeconds))
            $result.sdk_truth_test = 'passed'
        } finally { $env:PVD_SDK34_CAPTURE = $previousCapture }
    }
    Get-FileHash -Algorithm SHA256 -LiteralPath $capture, $matchingTruth, $cli, $producer | Select-Object Path, Hash | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $runDirectory 'sha256.json') -Encoding utf8
    $result.status = 'passed'
} catch {
    $failure = $_
    $result.status = 'failed'
    $result.error = $_.Exception.Message
    if ($result.sdk_truth_test -eq 'pending') { $result.sdk_truth_test = 'not_passed' }
} finally {
    foreach ($process in $ownedProcesses) {
        try {
            if (-not $process.HasExited) {
                $process.Kill($true)
                if (-not $process.WaitForExit(5000)) { throw "Owned process $($process.Id) did not stop after termination." }
            }
        } catch { $cleanupErrors.Add($_.Exception.Message) }
        finally { $process.Dispose() }
    }
    $result.cleanup_errors = @($cleanupErrors.ToArray())
    if ($cleanupErrors.Count -gt 0) { $result.status = 'failed' }
    $result.finished_utc = [DateTime]::UtcNow.ToString('o')
    $result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $runDirectory 'result.json') -Encoding utf8
}
if ($failure) { throw "LAN-path verification failed: $($failure.Exception.Message)`nArtifacts: $runDirectory" }
if ($cleanupErrors.Count -gt 0) { throw "Validation process cleanup failed: $($cleanupErrors -join '; '). Artifacts: $runDirectory" }
Write-Output "Verified 120 SDK frames through local interface address $localIp on port $Port."
Write-Output 'Scope: same-machine interface-address path; remote-host connectivity is not verified.'
Write-Output "Artifacts: $runDirectory"
