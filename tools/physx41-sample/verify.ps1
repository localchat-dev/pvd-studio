param(
    [switch]$BuildSdk,
    [int]$Port = 15426,
    [string]$CargoTarget = 'target\sdk41-validation'
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if ($Port -lt 1 -or $Port -gt 65535) { throw 'Port must be between 1 and 65535.' }
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargo = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path -LiteralPath $cargo)) { throw 'An existing Rust/Cargo installation is required.' }

function Invoke-Checked {
    param([string]$Program, [string[]]$ToolArguments)
    & $Program @ToolArguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

Push-Location -LiteralPath $projectRoot
try {
    if ($BuildSdk) { & (Join-Path $PSScriptRoot 'build.ps1') }
    $producer = Join-Path $projectRoot '.deps\physx41-sample-build\Release\pvd-physx41-sample.exe'
    if (-not (Test-Path -LiteralPath $producer)) { throw 'Build the official 4.1 producer first or use -BuildSdk.' }
    Invoke-Checked $cargo @('build', '--locked', '-p', 'pvd-cli', '--target-dir', $CargoTarget)
    $output = Join-Path $projectRoot ('output\sdk41-validation\' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
    New-Item -ItemType Directory -Path $output | Out-Null
    $cli = Join-Path $output 'pvd-cli.exe'
    Copy-Item -LiteralPath (Join-Path $CargoTarget 'debug\pvd-cli.exe') -Destination $cli
    $base = Join-Path $output 'base.pvdstream'
    $rich = Join-Path $output 'rich.pvdstream'
    $query = Join-Path $output 'query.pvdstream'
    Invoke-Checked $producer @('--file', $base, '--truth', (Join-Path $output 'base.csv'), '--frames', '120', '--flags', 'debug', '--lifecycle')
    Invoke-Checked $producer @('--file', $rich, '--truth', (Join-Path $output 'rich.csv'), '--mesh-truth', (Join-Path $output 'rich.mesh.json'), '--frames', '120', '--flags', 'all', '--lifecycle', '--rich')
    Invoke-Checked $producer @('--file', $query, '--query-truth', (Join-Path $output 'query.query.json'), '--frames', '120', '--flags', 'debug', '--lifecycle')
    $oldBase = $env:PVD_SDK34_CAPTURE
    $oldRich = $env:PVD_SDK34_RICH_CAPTURE
    $oldQuery = $env:PVD_QUERY_CAPTURE
    try {
        $env:PVD_SDK34_CAPTURE = $base
        $env:PVD_SDK34_RICH_CAPTURE = $rich
        $env:PVD_QUERY_CAPTURE = $query
        Invoke-Checked $cargo @('test', '--locked', '-p', 'pvd-core', '--test', 'sdk34', '--target-dir', $CargoTarget, '--', '--ignored', '--nocapture')
        Invoke-Checked $cargo @('test', '--locked', '-p', 'pvd-core', '--test', 'query', '--target-dir', $CargoTarget, '--', '--ignored', '--nocapture')
        $tcp = Join-Path $output 'tcp'
        $listenerLog = Join-Path $output 'listener.txt'
        $listenerError = Join-Path $output 'listener-error.txt'
        $listener = Start-Process -FilePath $cli -ArgumentList @('listen', "127.0.0.1:$Port", ('"' + $tcp + '"'), '60') -WindowStyle Hidden -PassThru -RedirectStandardOutput $listenerLog -RedirectStandardError $listenerError
        try {
            $deadline = [DateTime]::UtcNow.AddSeconds(10)
            $ready = $false
            do {
                if ($listener.HasExited) { throw (Get-Content -LiteralPath $listenerError -Raw) }
                if ((Test-Path -LiteralPath $listenerLog) -and (Get-Content -LiteralPath $listenerLog -Raw) -match 'listening:') { $ready = $true; break }
                Start-Sleep -Milliseconds 50
            } while ([DateTime]::UtcNow -lt $deadline)
            if (-not $ready) { throw 'TCP listener startup timed out.' }
            $tcpCsv = Join-Path $output 'tcp.csv'
            $tcpMesh = Join-Path $output 'tcp.mesh.json'
            Invoke-Checked $producer @('--host', '127.0.0.1', '--port', "$Port", '--truth', $tcpCsv, '--mesh-truth', $tcpMesh, '--frames', '120', '--flags', 'all', '--lifecycle', '--rich')
            if (-not $listener.WaitForExit(15000)) { throw 'TCP listener failed to stop after SDK disconnect.' }
            if ($listener.ExitCode -ne 0) { throw (Get-Content -LiteralPath $listenerError -Raw) }
        } finally {
            if (-not $listener.HasExited) { $listener.Kill(); $listener.WaitForExit() }
            $listener.Dispose()
        }
        $captured = @(Get-ChildItem -LiteralPath $tcp -Filter '*.pvdstream')
        if ($captured.Count -ne 1) { throw 'Expected one complete SDK TCP capture.' }
        $tcpCapture = $captured[0].FullName
        Copy-Item -LiteralPath $tcpCsv -Destination ([IO.Path]::ChangeExtension($tcpCapture, 'csv'))
        Copy-Item -LiteralPath $tcpMesh -Destination ([IO.Path]::ChangeExtension($tcpCapture, 'mesh.json'))
        $env:PVD_SDK34_RICH_CAPTURE = $tcpCapture
        Invoke-Checked $cargo @('test', '--locked', '-p', 'pvd-core', '--test', 'sdk34', '--target-dir', $CargoTarget, 'rich_geometry_matches_sdk_cooked_resources_and_scaled_axes', '--', '--ignored', '--nocapture')
        Invoke-Checked $cli @('verify', $tcpCapture)
        Invoke-Checked $cli @('profile-csv', $tcpCapture, (Join-Path $output 'profile.csv'))
        Invoke-Checked $cli @('memory-csv', $tcpCapture, (Join-Path $output 'memory.csv'))
        Get-FileHash -Algorithm SHA256 -LiteralPath $base, $rich, $query, $tcpCapture | Format-List | Out-File -LiteralPath (Join-Path $output 'sha256.txt')
    } finally {
        $env:PVD_SDK34_CAPTURE = $oldBase
        $env:PVD_SDK34_RICH_CAPTURE = $oldRich
        $env:PVD_QUERY_CAPTURE = $oldQuery
    }
    Write-Output "PhysX 4.1.2 verification artifacts: $output"
} finally { Pop-Location }
