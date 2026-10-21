param([switch]$SkipDesktop, [switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
Push-Location -LiteralPath $projectRoot
try {
    if (-not $SkipBuild) { & "$PSScriptRoot/build.ps1" }
    $runDirectory = Join-Path $projectRoot ('output/feature-validation/' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
    New-Item -ItemType Directory -Path $runDirectory -Force | Out-Null
    $features = @(
        @{ Name = 'heightfield'; Test = 'heightfield'; Environment = 'PVD_HEIGHTFIELD_CAPTURE'; Extension = 'heightfield.json'; Frame = 40 },
        @{ Name = 'query-shapes'; Test = 'query_shapes'; Environment = 'PVD_QUERY_SHAPES_CAPTURE'; Extension = 'json'; Frame = 5 },
        @{ Name = 'joint-limits'; Test = 'joint_limits'; Environment = 'PVD_JOINT_LIMIT_CAPTURE'; Extension = 'joints.json'; Frame = 2 },
        @{ Name = 'profile-metrics'; Test = 'frame_metrics'; Environment = 'PVD_PROFILE_METRICS_CAPTURE'; Extension = 'json'; Frame = 12; Panel = 'frame-curves' }
    )
    foreach ($feature in $features) {
        $producer = Join-Path $projectRoot ('.deps/physx-features-build/Release/pvd-' + $feature.Name + '.exe')
        if (-not (Test-Path -LiteralPath $producer)) { throw "Build the feature producer: $producer" }
        $capture = Join-Path $runDirectory ($feature.Name + '.pvdstream')
        $truth = [IO.Path]::ChangeExtension($capture, $feature.Extension)
        & $producer $capture $truth
        if ($LASTEXITCODE -ne 0) { throw "SDK generation failed: $($feature.Name)" }
        $previous = [Environment]::GetEnvironmentVariable($feature.Environment, 'Process')
        try {
            [Environment]::SetEnvironmentVariable($feature.Environment, $capture, 'Process')
            & cargo test --locked -p pvd-core --test $feature.Test -- --include-ignored
            if ($LASTEXITCODE -ne 0) { throw "SDK truth comparison failed: $($feature.Name)" }
        } finally { [Environment]::SetEnvironmentVariable($feature.Environment, $previous, 'Process') }
    }
    if (-not $SkipDesktop) {
        & cargo build --locked -p pvd-desktop
        if ($LASTEXITCODE -ne 0) { throw 'Desktop build failed' }
        foreach ($feature in $features) {
            $capture = Join-Path $runDirectory ($feature.Name + '.pvdstream')
            foreach ($language in @('zh-CN', 'en')) {
                $image = Join-Path $runDirectory ($feature.Name + '-' + $language + '.png')
                $panel = if ($feature.Panel) { $feature.Panel } else { 'overview' }
                & ./target/debug/pvd-desktop.exe --render-check $capture $image $feature.Frame $panel 1440 900 --language $language
                if ($LASTEXITCODE -ne 0) { throw "No-window rendering failed: $($feature.Name) / $language" }
            }
        }
    }
    Get-ChildItem -LiteralPath $runDirectory -File | Where-Object { $_.Extension -in @('.pvdstream', '.json') } | Get-FileHash -Algorithm SHA256 | Format-Table -AutoSize | Out-File -LiteralPath (Join-Path $runDirectory 'sha256.txt') -Width 300
    Write-Output "Feature validation artifacts: $runDirectory"
} finally { Pop-Location }
