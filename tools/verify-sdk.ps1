param(
    [switch]$BuildSdk,
    [switch]$SkipDesktop,
    [switch]$IncludeScale,
    [int]$Port = 15425,
    [string[]]$CargoConfig = @()
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ($Port -lt 1 -or $Port -gt 65535) { throw 'Port must be between 1 and 65535.' }
$cargoOptions = @()
foreach ($value in $CargoConfig) { $cargoOptions += @('--config', $value) }
function Build-Program([string]$Package) {
    $programs = @(& cargo @cargoOptions build --locked -p $Package --message-format=json-render-diagnostics | ForEach-Object {
        $artifactEvent = $_ | ConvertFrom-Json
        if ($artifactEvent.reason -eq 'compiler-artifact' -and $artifactEvent.target.name -eq $Package -and $artifactEvent.target.kind -contains 'bin' -and $artifactEvent.executable) {
            $artifactEvent.executable
        }
    })
    if ($LASTEXITCODE -ne 0) { throw "Build failed: $Package" }
    if ($programs.Count -ne 1 -or -not (Test-Path -LiteralPath $programs[0] -PathType Leaf)) {
        throw "Cargo did not identify one executable for $Package"
    }
    return $programs[0]
}
Push-Location -LiteralPath $projectRoot
try {
    if ($BuildSdk) { & (Join-Path $PSScriptRoot 'physx-sample\build.ps1') }
    $producer = Join-Path $projectRoot '.deps\physx-sample-build\Release\pvd-physx-sample.exe'
    if (-not (Test-Path -LiteralPath $producer)) { throw 'Build the SDK producer first: ./tools/physx-sample/build.ps1' }
    $testPackages = @('-p', 'pvd-protocol', '-p', 'pvd-core', '-p', 'pvd-transport', '-p', 'pvd-cli')
    if (-not $SkipDesktop) { $testPackages += @('-p', 'pvd-desktop') }
    & cargo @cargoOptions test --locked @testPackages -- --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw 'Core tests failed' }
    $cli = Build-Program 'pvd-cli'
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
    $previousMessage = $env:PVD_MESSAGE_CAPTURE
    try {
        $env:PVD_SDK34_CAPTURE = $base
        $env:PVD_SDK34_RICH_CAPTURE = $rich
        $env:PVD_MESSAGE_CAPTURE = $base
        foreach ($case in @(
            'official_sdk_capture_matches_independent_sdk_poses_and_random_replay',
            'rich_geometry_matches_sdk_cooked_resources_and_scaled_axes'
        )) {
            & cargo @cargoOptions test --locked -p pvd-core --test sdk34 $case -- --ignored --exact
            if ($LASTEXITCODE -ne 0) { throw "Independent SDK truth comparison failed: $case" }
        }
        foreach ($case in @(
            'repx::tests::corrupt_real_capture_properties_are_rejected_before_export',
            'property_message_tests::official_sdk_capture_cached_messages_match_uncached_state_and_random_replay'
        )) {
            & cargo @cargoOptions test --locked -p pvd-core --lib $case -- --ignored --exact --test-threads=1
            if ($LASTEXITCODE -ne 0) { throw "Real-capture core regression failed: $case" }
        }
    } finally {
        $env:PVD_SDK34_CAPTURE = $previousCapture
        $env:PVD_SDK34_RICH_CAPTURE = $previousRich
        $env:PVD_MESSAGE_CAPTURE = $previousMessage
    }
    $queryCapture = Join-Path $runDirectory 'queries.pvdstream'
    & $producer --file $queryCapture --query-truth (Join-Path $runDirectory 'queries.query.json') --frames 120 --flags debug --lifecycle
    if ($LASTEXITCODE -ne 0) { throw 'SDK query generation failed' }
    $previousQuery = $env:PVD_QUERY_CAPTURE
    try {
        $env:PVD_QUERY_CAPTURE = $queryCapture
        & cargo @cargoOptions test --locked -p pvd-core --test query recorded_rays_and_hits_match_independent_sdk_results -- --ignored --exact
        if ($LASTEXITCODE -ne 0) { throw 'Independent SDK raycast comparison failed' }
    } finally { $env:PVD_QUERY_CAPTURE = $previousQuery }
    if ($IncludeScale) {
        $scale = Join-Path $runDirectory 'scale.pvdstream'
        & $producer --file $scale --truth (Join-Path $runDirectory 'scale.csv') --frames 120 --flags debug --lifecycle --extra-boxes 500
        if ($LASTEXITCODE -ne 0) { throw 'SDK scale generation failed' }
        $previousScale = $env:PVD_SDK34_SCALE_CAPTURE
        try {
            $env:PVD_SDK34_SCALE_CAPTURE = $scale
            & cargo @cargoOptions test --locked -p pvd-core --test scale many_sdk_actors_match_independent_poses_after_random_seeks -- --ignored --exact
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
        & $producer --host 127.0.0.1 --port $Port --frames 120 --flags all --lifecycle --rich --truth (Join-Path $runDirectory 'tcp.csv') --mesh-truth (Join-Path $runDirectory 'tcp.mesh.json')
        if ($LASTEXITCODE -ne 0) { throw 'SDK TCP production failed' }
        if (-not $listener.WaitForExit(15000)) { throw 'Listener did not finish after SDK disconnect' }
        if ($listener.ExitCode -ne 0) { throw (Get-Content -LiteralPath $listenerErrors -Raw) }
    } finally {
        if (-not $listener.HasExited) { $listener.Kill(); $listener.WaitForExit() }
        $listener.Dispose()
    }
    $captures = @(Get-ChildItem -LiteralPath $captureDirectory -Filter '*.pvdstream')
    if ($captures.Count -ne 1) { throw 'Listener must save exactly one complete capture' }
    $capture = $captures[0]
    Copy-Item -LiteralPath (Join-Path $runDirectory 'tcp.csv') -Destination ([IO.Path]::ChangeExtension($capture.FullName, 'csv'))
    Copy-Item -LiteralPath (Join-Path $runDirectory 'tcp.mesh.json') -Destination ([IO.Path]::ChangeExtension($capture.FullName, 'mesh.json'))
    $previousRich = $env:PVD_SDK34_RICH_CAPTURE
    try {
        $env:PVD_SDK34_RICH_CAPTURE = $capture.FullName
        & cargo @cargoOptions test --locked -p pvd-core --test sdk34 rich_geometry_matches_sdk_cooked_resources_and_scaled_axes -- --ignored --exact
        if ($LASTEXITCODE -ne 0) { throw 'TCP SDK truth comparison failed' }
    } finally { $env:PVD_SDK34_RICH_CAPTURE = $previousRich }
    & $cli verify $capture.FullName | Tee-Object -FilePath (Join-Path $runDirectory 'tcp-verify.txt')
    if ($LASTEXITCODE -ne 0) { throw 'TCP replay verification failed' }
    & $cli profile-csv $capture.FullName (Join-Path $runDirectory 'profile.csv')
    if ($LASTEXITCODE -ne 0) { throw 'Profile export failed' }
    & $cli memory-csv $capture.FullName (Join-Path $runDirectory 'memory.csv')
    if ($LASTEXITCODE -ne 0) { throw 'Memory export failed' }
    if (-not $SkipDesktop) {
        $previousCapture = $env:PVD_SDK34_CAPTURE
        $previousRich = $env:PVD_SDK34_RICH_CAPTURE
        $previousAnalysis = $env:PVD_CLIP_ANALYSIS_CAPTURE
        try {
            $env:PVD_SDK34_CAPTURE = $base
            $env:PVD_SDK34_RICH_CAPTURE = $rich
            $env:PVD_CLIP_ANALYSIS_CAPTURE = $rich
            foreach ($case in @(
                'clip_store::tests::clip_store_real_sdk_reopen_replace_cancel_and_tamper',
                'curve::tests::sdk_curve_values_match_truth_and_never_join_reused_actor_generations',
                'network::tests::real_capture_storage_quotas_keep_latest_scene_after_raw_and_history_stop',
                'network::tests::real_capture_reconnect_and_truncation_keep_sessions_isolated',
                'studio::tests::repeated_open_close_does_not_restore_cancelled_or_previous_state',
                'studio::tests::repeated_disk_seek_matches_real_sdk_capture',
                'studio::tests::disk_file_loader_matches_sdk_replay_and_rejects_incomplete_cache',
                'studio::tests::real_tcp_capture_continues_while_display_is_paused',
                'studio::clip_tests::disk_clip_workflow_persists_ranges_exports_and_rejects_changed_source',
                'studio::clip_tests::disk_analysis_exports_cover_all_recorded_spans_and_memory_samples'
            )) {
                & cargo @cargoOptions test --locked -p pvd-desktop $case -- --ignored --exact --test-threads=1
                if ($LASTEXITCODE -ne 0) { throw "Desktop real-capture regression failed: $case" }
            }
        } finally {
            $env:PVD_SDK34_CAPTURE = $previousCapture
            $env:PVD_SDK34_RICH_CAPTURE = $previousRich
            $env:PVD_CLIP_ANALYSIS_CAPTURE = $previousAnalysis
        }
        $desktop = Build-Program 'pvd-desktop'
        & $desktop --render-check $capture.FullName (Join-Path $runDirectory 'desktop.png')
        if ($LASTEXITCODE -ne 0) { throw 'Offscreen desktop validation failed' }
    }
    Get-FileHash -Algorithm SHA256 -LiteralPath $base, $capture.FullName | Format-List | Out-File -LiteralPath (Join-Path $runDirectory 'sha256.txt')
    Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors | Format-List | Out-File -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory | Format-List | Out-File -Append -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion | Format-List | Out-File -Append -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Write-Output "Verification artifacts: $runDirectory"
} finally { Pop-Location }
