param(
    [Parameter(Mandatory)][string]$PackageDirectory,
    [ValidateSet('update', 'remove', 'reload')][string]$Change = 'update'
)

$ErrorActionPreference = 'Stop'
$editor = Join-Path (Resolve-Path $PackageDirectory) 'Editor'
$sources = @(
    (Join-Path $editor 'NativeViewer.cs')
    (Join-Path $editor 'FrameWorker.cs')
    (Join-Path $PSScriptRoot '../test-support/unity_package_change.cs')
)
Add-Type -Path $sources -CompilerOptions '/nowarn:0649'
[PackageChangeProbe]::Run((Join-Path $editor 'Plugins/x86_64/pvd_unity.dll'), $Change)
