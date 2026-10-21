param(
    [switch]$BuildSdk,
    [switch]$SkipDesktop,
    [switch]$IncludeScale,
    [int]$Port = 15425
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location -LiteralPath $projectRoot
try {
    if ($BuildSdk) { & (Join-Path $PSScriptRoot 'physx-sample\build.ps1') }
    $producer = Join-Path $projectRoot '.deps\physx-sample-build\Release\pvd-physx-sample.exe'
    if (-not (Test-Path -LiteralPath $producer)) { throw 'Build the SDK producer first: ./tools/physx-sample/build.ps1' }
    & cargo test --locked -p pvd-protocol -p pvd-core -p pvd-transport -p pvd-cli
    if ($LASTEXITCODE -ne 0) { throw 'Core tests failed' }
    & cargo build --locked -p pvd-cli
    if ($LASTEXITCODE -ne 0) { throw 'CLI build failed' }
    $cli = Join-Path $projectRoot 'target\debug\pvd-cli.exe'
    $runDirectory = Join-Path $projectRoot ('output\validation\' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
    New-Item -ItemType Directory -Path $runDirectory -Force | Out-Null
    $base = Join-Path $runDirectory 'base.pvdstream'
    & $producer --file $base --truth (Join-Path $runDirectory 'base.csv') --frames 120 --flags debug --lifecycle
    if ($LASTEXITCODE -ne 0) { throw 'SDK base generation failed' }
    $rich = Join-Path $runDirectory 'rich.pvdstream'
    & $producer --file $rich --truth (Join-Path $runDirectory 'rich.csv') --mesh-truth (Join-Path $runDirectory 'rich.mesh.json') --frames 120 --flags all --lifecycle --rich
    if ($LASTEXITCODE -ne 0) { throw 'SDK rich generation failed' }
    $previousCapture = $env:PVD_SDK34_CAPTURE
    $previousRich = $env:PVD_SDK34_RICH_CAPTURE
    try {
        $env:PVD_SDK34_CAPTURE = $base
        $env:PVD_SDK34_RICH_CAPTURE = $rich
        & cargo test --locked -p pvd-core --test sdk34 -- --ignored
        if ($LASTEXITCODE -ne 0) { throw 'Independent SDK truth comparison failed' }
        & cargo test --locked -p pvd-core --lib -- --ignored
        if ($LASTEXITCODE -ne 0) { throw 'Real-capture core regression failed' }
    } finally {
        $env:PVD_SDK34_CAPTURE = $previousCapture
        $env:PVD_SDK34_RICH_CAPTURE = $previousRich
    }
    $queryCapture = Join-Path $runDirectory 'queries.pvdstream'
    & $producer --file $queryCapture --query-truth (Join-Path $runDirectory 'queries.query.json') --frames 120 --flags debug --lifecycle
    if ($LASTEXITCODE -ne 0) { throw 'SDK query generation failed' }
    $previousQuery = $env:PVD_QUERY_CAPTURE
    try {
        $env:PVD_QUERY_CAPTURE = $queryCapture
        & cargo test --locked -p pvd-core --test query -- --ignored
        if ($LASTEXITCODE -ne 0) { throw 'Independent SDK raycast comparison failed' }
    } finally { $env:PVD_QUERY_CAPTURE = $previousQuery }
    if ($IncludeScale) {
        $scale = Join-Path $runDirectory 'scale.pvdstream'
        & $producer --file $scale --truth (Join-Path $runDirectory 'scale.csv') --frames 120 --flags debug --lifecycle --extra-boxes 500
        if ($LASTEXITCODE -ne 0) { throw 'SDK scale generation failed' }
        $previousScale = $env:PVD_SDK34_SCALE_CAPTURE
        try {
            $env:PVD_SDK34_SCALE_CAPTURE = $scale
            & cargo test --locked -p pvd-core --test scale -- --ignored
            if ($LASTEXITCODE -ne 0) { throw 'SDK scale truth comparison failed' }
        } finally { $env:PVD_SDK34_SCALE_CAPTURE = $previousScale }
    }
    & $cli verify $base | Tee-Object -FilePath (Join-Path $runDirectory 'base-verify.txt')
    if ($LASTEXITCODE -ne 0) { throw 'Base replay verification failed' }
    & $cli trim $base 70 100 (Join-Path $runDirectory 'trim.pvdstream')
    if ($LASTEXITCODE -ne 0) { throw 'Scene trim verification failed' }
    $sdkFile = Join-Path $runDirectory 'sdk-file.pxd2'
    & $producer --file $sdkFile --frames 120 --flags debug --lifecycle
    if ($LASTEXITCODE -ne 0) { throw 'SDK direct .pxd2 generation failed' }
    & $cli verify $sdkFile | Out-File -LiteralPath (Join-Path $runDirectory 'sdk-file-verify.txt')
    if ($LASTEXITCODE -ne 0) { throw 'SDK direct .pxd2 replay verification failed' }
    $errorCapture = Join-Path $runDirectory 'sdk-error.pvdstream'
    & $producer --file $errorCapture --frames 12 --flags all --emit-error 2> (Join-Path $runDirectory 'sdk-error-source.txt')
    if ($LASTEXITCODE -ne 0) { throw 'Controlled SDK error generation failed' }
    $errorInspection = & $cli inspect $errorCapture
    if ($LASTEXITCODE -ne 0) { throw 'SDK error capture inspection failed' }
    $errorInspection | Out-File -LiteralPath (Join-Path $runDirectory 'sdk-error-inspect.txt')
    if (-not ($errorInspection -match 'sdk_error: 4:')) { throw 'Expected real SDK invalid-parameter diagnostic was not decoded' }
    $captureDirectory = Join-Path $runDirectory 'tcp'
    $listenerLog = Join-Path $runDirectory 'listener.txt'
    $listenerErrors = Join-Path $runDirectory 'listener-errors.txt'
    $listener = Start-Process -FilePath $cli -ArgumentList @('listen', "127.0.0.1:$Port", ('"' + $captureDirectory + '"'), '60') -WindowStyle Hidden -PassThru -RedirectStandardOutput $listenerLog -RedirectStandardError $listenerErrors
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        $listenerReady = $false
        do {
            if ($listener.HasExited) { throw (Get-Content -LiteralPath $listenerErrors -Raw) }
            if ((Test-Path -LiteralPath $listenerLog) -and ((Get-Content -LiteralPath $listenerLog -Raw) -match 'listening:')) { $listenerReady = $true; break }
            Start-Sleep -Milliseconds 50
        } while ([DateTime]::UtcNow -lt $deadline)
        if (-not $listenerReady) { throw 'Listener did not become ready within 10 seconds' }
        & $producer --host 127.0.0.1 --port $Port --frames 120 --flags all --lifecycle --rich --truth (Join-Path $runDirectory 'tcp.csv')
        if ($LASTEXITCODE -ne 0) { throw 'SDK TCP production failed' }
        if (-not $listener.WaitForExit(15000)) { throw 'Listener did not finish after SDK disconnect' }
        if ($listener.ExitCode -ne 0) { throw (Get-Content -LiteralPath $listenerErrors -Raw) }
    } finally {
        if (-not $listener.HasExited) { $listener.Kill(); $listener.WaitForExit() }
        $listener.Dispose()
    }
    $capture = Get-ChildItem -LiteralPath $captureDirectory -Filter '*.pvdstream' | Select-Object -First 1
    if (-not $capture) { throw 'Listener did not save a complete capture' }
    & $cli verify $capture.FullName | Tee-Object -FilePath (Join-Path $runDirectory 'tcp-verify.txt')
    if ($LASTEXITCODE -ne 0) { throw 'TCP replay verification failed' }
    & $cli profile-csv $capture.FullName (Join-Path $runDirectory 'profile.csv')
    if ($LASTEXITCODE -ne 0) { throw 'Profile export failed' }
    & $cli memory-csv $capture.FullName (Join-Path $runDirectory 'memory.csv')
    if ($LASTEXITCODE -ne 0) { throw 'Memory export failed' }
    if (-not $SkipDesktop) {
        $previousCapture = $env:PVD_SDK34_CAPTURE
        try {
            $env:PVD_SDK34_CAPTURE = $base
            & cargo test --locked -p pvd-desktop -- --include-ignored
            if ($LASTEXITCODE -ne 0) { throw 'Desktop state and real-capture regression failed' }
        } finally { $env:PVD_SDK34_CAPTURE = $previousCapture }
        & cargo build --locked -p pvd-desktop
        if ($LASTEXITCODE -ne 0) { throw 'Desktop build failed' }
        & (Join-Path $projectRoot 'target\debug\pvd-desktop.exe') --render-check $capture.FullName (Join-Path $runDirectory 'desktop.png')
        if ($LASTEXITCODE -ne 0) { throw 'Offscreen desktop validation failed' }
    }
    Get-FileHash -Algorithm SHA256 -LiteralPath $base, $capture.FullName | Format-List | Out-File -LiteralPath (Join-Path $runDirectory 'sha256.txt')
    Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors | Format-List | Out-File -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory | Format-List | Out-File -Append -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion | Format-List | Out-File -Append -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Write-Output "Verification artifacts: $runDirectory"
} finally { Pop-Location }
