param(
    [Parameter(Mandatory = $true)][string]$Capture,
    [string]$CliPath = (Join-Path $PSScriptRoot '../../target/debug/pvd-cli.exe'),
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$Capture = [IO.Path]::GetFullPath($Capture)
$CliPath = [IO.Path]::GetFullPath($CliPath)
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path ([IO.Path]::GetDirectoryName($Capture)) ('cli-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw 'CLI verification requires a fresh output directory.' }
if (-not (Test-Path -LiteralPath $CliPath)) { throw 'Build the debug CLI with cargo build --locked -p pvd-cli.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$truth = Get-Content -LiteralPath ([IO.Path]::ChangeExtension($Capture, 'json')) -Raw | ConvertFrom-Json
$culture = [Globalization.CultureInfo]::InvariantCulture

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Id-Value([string]$Value) {
    if ($Value.StartsWith('0x')) { return [Convert]::ToUInt64($Value.Substring(2), 16) }
    return [UInt64]::Parse($Value, $culture)
}

function Matches-Key($Item, $Entry) {
    return $Item.event -eq $Entry.event_name -and
        [UInt64]$Item.context -eq (Id-Value $Entry.context_id) -and
        [UInt32]$Item.thread -eq [UInt32]$Entry.thread_id
}

$catalogPath = Join-Path $OutputDirectory 'catalog.csv'
& $CliPath profile-events $Capture | Set-Content -LiteralPath $catalogPath -Encoding utf8
if ($LASTEXITCODE -ne 0) { throw 'CLI profile catalog failed.' }
$catalog = Import-Csv -LiteralPath $catalogPath
$custom = @($catalog | Where-Object event_name -Like 'PvdStudio.Profile.*')
Assert-True ($custom.Count -eq 6) 'Expected six exact custom profile keys.'

$snapshotPath = Join-Path $OutputDirectory 'frame-0.txt'
& $CliPath frame $Capture 0 | Set-Content -LiteralPath $snapshotPath -Encoding utf8
if ($LASTEXITCODE -ne 0) { throw 'CLI frame snapshot failed.' }
$scenes = @(Get-Content -LiteralPath $snapshotPath | ForEach-Object {
    if ($_ -match '^0x([0-9a-f]+) generation=(\d+) physx3::PxScene$') {
        if ([Convert]::ToUInt64($Matches[1], 16) -eq [UInt64]$truth.scene) {
            [PSCustomObject]@{ Id = [UInt64]$truth.scene; Generation = [UInt32]$Matches[2] }
        }
    }
})
Assert-True ($scenes.Count -eq 1) 'The real Scene must exist in the first CLI snapshot.'
$scene = $scenes[0]
Assert-True ($scene.Generation -eq 0) 'This fresh SDK sample unexpectedly reused its Scene identity.'

$spansPath = Join-Path $OutputDirectory 'spans.csv'
& $CliPath profile-csv $Capture $spansPath | Set-Content -LiteralPath (Join-Path $OutputDirectory 'spans-export.txt') -Encoding utf8
if ($LASTEXITCODE -ne 0) { throw 'CLI raw span export failed.' }
$rawSpans = @(Import-Csv -LiteralPath $spansPath | Where-Object event -Like 'PvdStudio.Profile.*')
Assert-True ($rawSpans.Count -eq $truth.spans.Count) 'The raw span export has an unexpected custom span count.'
$matched = [Collections.Generic.HashSet[int]]::new()
foreach ($expected in $truth.spans) {
    $matches = @(for ($index = 0; $index -lt $rawSpans.Count; $index++) {
        $actual = $rawSpans[$index]
        if ($actual.zone -eq $truth.zone_name -and $actual.event -eq $expected.event -and
            [UInt64]$actual.context -eq [UInt64]$expected.context -and [UInt32]$actual.thread -eq [UInt32]$expected.thread -and
            [UInt64]$actual.start_ticks -ge [UInt64]$expected.start_lower -and [UInt64]$actual.start_ticks -le [UInt64]$expected.start_upper -and
            [UInt64]$actual.end_ticks -ge [UInt64]$expected.end_lower -and [UInt64]$actual.end_ticks -le [UInt64]$expected.end_upper) { $index }
    })
    Assert-True ($matches.Count -eq 1) 'Each truth span must match one raw SDK span inside independent QPC bounds.'
    Assert-True ($matched.Add($matches[0])) 'Two truth spans unexpectedly matched the same raw SDK span.'
}

$reports = @()
foreach ($entry in $custom) {
    $expectedSpans = @($truth.spans | Where-Object { Matches-Key $_ $entry })
    $expectedStarts = @($truth.calls | Where-Object { $_.kind -eq 'start' -and (Matches-Key $_ $entry) })
    $expectedPending = @($truth.pending | Where-Object { Matches-Key $_ $entry })
    Assert-True ($entry.zone_name -eq $truth.zone_name -and $entry.metadata_ambiguous -eq 'false') 'Custom event metadata differs from the SDK zone.'
    Assert-True ([int]$entry.complete_spans -eq $expectedSpans.Count -and [int]$entry.open_spans -eq $expectedPending.Count -and [int]$entry.unmatched_stops -eq 0) 'Exact-key catalog counts differ from independent truth.'
    $first = $expectedStarts[0]
    Assert-True ([UInt64]$entry.first_start_tick -ge [UInt64]$first.lower -and [UInt64]$entry.first_start_tick -le [UInt64]$first.upper) 'Catalog ordering timestamp is outside the first SDK start bounds.'
    $csvPath = Join-Path $OutputDirectory ('series-' + $entry.series_index + '.csv')
    & $CliPath profile-event-csv $Capture $entry.series_index $scene.Id $scene.Generation $csvPath |
        Set-Content -LiteralPath (Join-Path $OutputDirectory ('series-' + $entry.series_index + '-export.txt')) -Encoding utf8
    if ($LASTEXITCODE -ne 0) { throw 'CLI event curve export failed.' }
    $rows = @(Import-Csv -LiteralPath $csvPath)
    Assert-True ($rows.Count -eq 8) 'The curve must contain eight scene frame rows.'
    for ($frame = 0; $frame -lt $rows.Count; $frame++) {
        $row = $rows[$frame]
        Assert-True ([int]$row.frame -eq $frame -and $row.zone_id -eq $entry.zone_id -and $row.event_id -eq $entry.event_id -and
            $row.thread_id -eq $entry.thread_id -and $row.context_id -eq $entry.context_id -and $row.event_name -eq $entry.event_name -and
            (Id-Value $row.reference_scene) -eq $scene.Id -and [UInt32]$row.reference_scene_generation -eq $scene.Generation) 'Curve row identity or scene mapping changed.'
        Assert-True ($row.stride_basis -eq 'reference_scene_generation_frames' -and [int]$row.reference_frame_count -eq 8 -and
            [int]$row.stride -eq 1 -and [int]$row.captured_frame_count -eq 8) 'Curve sampling metadata must identify all eight reference Scene generation frames.'
        $spans = @($expectedSpans | Where-Object { $_.start_frame -eq $frame -and $_.end_frame -eq $frame })
        $expectedStatus = if ($frame -eq 0) { 'no_previous_frame' }
            elseif ($entry.event_name -eq 'PvdStudio.Profile.CrossFrame' -and $frame -in @(3, 4)) { 'span_crosses_frame_boundary' }
            elseif ($entry.event_name -eq 'PvdStudio.Profile.Pending' -and $frame -ge 6) { 'open_span_may_overlap_window' }
            elseif ($spans.Count -eq 0) { 'no_complete_span' }
            else { 'recorded' }
        Assert-True ($row.status -eq $expectedStatus) "Unexpected frame $frame status for $($entry.event_name): $($row.status)."
        Assert-True ([int]$row.occurrence_count -eq $spans.Count) 'Curve occurrence count mixed keys or frames.'
        if ($expectedStatus -ne 'recorded') {
            Assert-True ($row.value_ms -eq '') 'Missing or ambiguous measurements must be empty, not zero.'
            continue
        }
        $lower = [decimal]0
        $upper = [decimal]0
        foreach ($span in $spans) {
            $lower += [Math]::Max([decimal]0, ([decimal]$span.end_lower - [decimal]$span.start_upper))
            $upper += [decimal]$span.end_upper - [decimal]$span.start_lower
        }
        $scale = 1000.0 / [double]$truth.counter_frequency
        $actualMs = [double]::Parse($row.value_ms, $culture)
        Assert-True ($actualMs -ge [double]$lower * $scale - 1e-9 -and $actualMs -le [double]$upper * $scale + 1e-9) 'Summed inclusive duration is outside independent QPC bounds.'
    }
    $reports += [PSCustomObject]@{ series_index = [int]$entry.series_index; event = $entry.event_name; thread = [UInt32]$entry.thread_id; context = $entry.context_id; counts = @($rows | ForEach-Object { [int]$_.occurrence_count }); csv = $csvPath }
}

$sample = @($custom | Where-Object { $_.event_name -eq 'PvdStudio.Profile.Sample' -and [UInt32]$_.thread_id -eq [UInt32]$truth.main_thread -and (Id-Value $_.context_id) -eq [UInt64]$truth.context_a })[0]
$second = @($custom | Where-Object event_name -EQ 'PvdStudio.Profile.Second')[0]
Assert-True ([int]$sample.series_index -lt [int]$second.series_index) 'Catalog must order by first start, not completion order.'
Assert-True ($rawSpans[0].event -eq 'PvdStudio.Profile.Second') 'The fixture must complete Second before its first Sample span.'
$sampleReport = @($reports | Where-Object series_index -EQ ([int]$sample.series_index))[0]
Assert-True (($sampleReport.counts -join ',') -eq '0,3,0,0,2,2,0,0') 'Repeated event frame counts differ from the designed SDK workload.'

function Assert-ExportRejected([UInt32]$Generation, [string]$OutputPath, [string]$Label) {
    $arguments = 'profile-event-csv "{0}" {1} {2} {3} "{4}"' -f $Capture, $sample.series_index, $scene.Id, $Generation, $OutputPath
    $process = Start-Process -FilePath $CliPath -ArgumentList $arguments -WorkingDirectory $OutputDirectory -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $OutputDirectory ($Label + '.stdout.txt')) `
        -RedirectStandardError (Join-Path $OutputDirectory ($Label + '.stderr.txt'))
    try {
        if (-not $process.WaitForExit(60000)) { throw "CLI rejection check exceeded 60 seconds: $Label." }
        Assert-True ($process.ExitCode -ne 0) "CLI export unexpectedly accepted $Label."
    } finally {
        if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
}

$invalidGenerationPath = Join-Path $OutputDirectory 'invalid-generation.csv'
Assert-ExportRejected -Generation 1 -OutputPath $invalidGenerationPath -Label 'invalid-generation'
Assert-True (-not (Test-Path -LiteralPath $invalidGenerationPath)) 'An invalid reference Scene generation must not create a CSV.'
$originalHash = (Get-FileHash -LiteralPath $sampleReport.csv -Algorithm SHA256).Hash
Assert-ExportRejected -Generation $scene.Generation -OutputPath $sampleReport.csv -Label 'existing-output'
Assert-True ((Get-FileHash -LiteralPath $sampleReport.csv -Algorithm SHA256).Hash -eq $originalHash) 'An existing CSV must not be overwritten or truncated.'
[PSCustomObject]@{ capture = $Capture; scene = $scene.Id; scene_generation = $scene.Generation; compared_spans = $matched.Count; compared_series = $reports.Count; frame_rows = $reports.Count * 8; stride_basis = 'reference_scene_generation_frames'; reference_frame_count = 8; stride = 1; captured_frame_count = 8; invalid_generation_rejected_without_output = $true; existing_output_preserved = $true; series = $reports } |
    ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'result.json') -Encoding utf8
Write-Output "CLI verified 6 exact keys, 12 independent QPC spans, 48 curve rows, reference-frame sampling, diagnostic gaps and output rejection: $OutputDirectory"
