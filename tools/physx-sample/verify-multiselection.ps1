param(
    [Parameter(Mandatory)][string]$Capture,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Truth,
    [string]$CliPath = (Join-Path $PSScriptRoot '../../target/debug/pvd-cli.exe'),
    [string]$SdkExecutable = (Join-Path $PSScriptRoot '../../.deps/physx-sample-build/Release/pvd-physx-sample.exe'),
    [int]$Frame = 0,
    [string[]]$ActorNames = @('PvdStudioBox', 'PvdStudioSphere')
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($Frame -lt 0) { throw 'Frame must be nonnegative.' }
if ($ActorNames.Count -lt 2 -or $ActorNames.Count -gt 4095 -or @($ActorNames | Select-Object -Unique).Count -ne $ActorNames.Count) {
    throw 'Select 2 to 4095 distinct Actor names; one input slot is needed for duplicate and invalid-ID checks.'
}
$Capture = [IO.Path]::GetFullPath($Capture)
$CliPath = [IO.Path]::GetFullPath($CliPath)
$SdkExecutable = [IO.Path]::GetFullPath($SdkExecutable)
if (-not $Truth) { $Truth = [IO.Path]::ChangeExtension($Capture, '.csv') }
$Truth = [IO.Path]::GetFullPath($Truth)
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
foreach ($path in @($Capture, $Truth, $CliPath, $SdkExecutable)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Input not found: $path" }
}
if (Test-Path -LiteralPath $OutputDirectory) { throw 'OutputDirectory must be new.' }
$expectedActors = @(Import-Csv -LiteralPath $Truth | Where-Object {
    $_.frame -eq [string]$Frame -and $_.name -in $ActorNames
})
if ($expectedActors.Count -ne $ActorNames.Count -or @($expectedActors.actor_id | Select-Object -Unique).Count -ne $ActorNames.Count) {
    throw 'SDK truth must contain one distinct Actor per selected name at the requested frame.'
}
foreach ($name in $ActorNames) {
    if (@($expectedActors | Where-Object { $_.name -eq $name }).Count -ne 1) { throw "Ambiguous SDK Actor name: $name" }
}
$ids = @($expectedActors | ForEach-Object { [string]$_.actor_id })
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$xml = Join-Path $OutputDirectory 'selected.xml'
& $CliPath repx $Capture $Frame $xml @ids | Set-Content -LiteralPath (Join-Path $OutputDirectory 'export.txt')
if ($LASTEXITCODE -ne 0) { throw 'Batch export failed.' }
$roundtrip = Join-Path $OutputDirectory 'selected-sdk.xml'
& $SdkExecutable --import-repx $xml --roundtrip-repx $roundtrip | Set-Content -LiteralPath (Join-Path $OutputDirectory 'import.txt')
if ($LASTEXITCODE -ne 0) { throw 'Official SDK import failed.' }
foreach ($path in @($xml, $roundtrip)) {
    [xml]$document = Get-Content -LiteralPath $path -Raw
    $actors = @($document.SelectNodes('/*/PxRigidDynamic | /*/PxRigidStatic'))
    if ($actors.Count -ne $expectedActors.Count) { throw "Unexpected exported Actor set: $path" }
    foreach ($expected in $expectedActors) {
        $actor = @($actors | Where-Object {
            $_.SelectSingleNode('Id').InnerText -eq $expected.actor_id -and $_.SelectSingleNode('Name').InnerText -eq $expected.name
        })
        if ($actor.Count -ne 1) { throw "Missing or duplicated Actor $($expected.name): $path" }
        $pose = $actor[0].SelectSingleNode('GlobalPose').InnerText
        $values = @($pose -split '\s+' | Where-Object { $_ } | ForEach-Object {
            [double]::Parse($_, [Globalization.CultureInfo]::InvariantCulture)
        })
        $expectedPose = @($expected.qx, $expected.qy, $expected.qz, $expected.qw, $expected.x, $expected.y, $expected.z)
        if ($values.Count -ne 7) { throw "Invalid pose: $path" }
        for ($index = 0; $index -lt 7; $index++) {
            $target = [double]::Parse($expectedPose[$index], [Globalization.CultureInfo]::InvariantCulture)
            if ([Math]::Abs($values[$index] - $target) -gt 0.0001) { throw "SDK pose mismatch: $path" }
        }
    }
}
$duplicate = Join-Path $OutputDirectory 'duplicates.xml'
& $CliPath repx $Capture $Frame $duplicate @ids $ids[0] | Set-Content -LiteralPath (Join-Path $OutputDirectory 'duplicates.txt')
if ($LASTEXITCODE -ne 0 -or (Get-FileHash -LiteralPath $duplicate).Hash -ne (Get-FileHash -LiteralPath $xml).Hash) {
    throw 'Repeated selection changed the exported collection.'
}
$rejected = Join-Path $OutputDirectory 'invalid.xml'
& $CliPath repx $Capture $Frame $rejected @ids '18446744073709551615' 2>&1 | Out-File -LiteralPath (Join-Path $OutputDirectory 'invalid.txt')
if ($LASTEXITCODE -eq 0 -or (Test-Path -LiteralPath $rejected)) { throw 'Invalid selection published a partial result.' }
@{
    capture = $Capture
    truth = $Truth
    frame = $Frame
    selected = $ids
    expected_names = $ActorNames
    sdk_import = $true
    independent_pose_comparison = $true
    duplicates_identical = $true
    invalid_rejected_without_output = $true
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'result.json')
Get-FileHash -LiteralPath $Capture, $Truth, $CliPath, $SdkExecutable, $xml, $roundtrip |
    Out-File -LiteralPath (Join-Path $OutputDirectory 'sha256.txt') -Width 300
Write-Output "Batch selection verified: $OutputDirectory"
