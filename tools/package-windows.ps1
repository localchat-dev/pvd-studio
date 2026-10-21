param(
    [switch]$SkipBuild,
    [switch]$IncludeSource,
    [ValidateSet('zip', '7z')]
    [string]$ArchiveFormat = 'zip',
    [string]$SevenZipPath,
    [string]$TargetDirectory = 'target',
    [ValidatePattern('^\d{8}-\d{6}$')]
    [string]$BuildStamp = (Get-Date -Format 'yyyyMMdd-HHmmss')
)

$ErrorActionPreference = 'Stop'

function Find-SevenZip {
    param([string]$ExecutablePath)
    if ($ExecutablePath) {
        if (-not (Test-Path -LiteralPath $ExecutablePath -PathType Leaf)) {
            throw "7-Zip executable not found: $ExecutablePath"
        }
        return (Resolve-Path -LiteralPath $ExecutablePath).Path
    }
    $command = Get-Command -Name '7z.exe' -CommandType Application -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($directory in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if (-not $directory) { continue }
        $candidate = Join-Path $directory '7-Zip/7z.exe'
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return $candidate }
    }
    return $null
}

function Test-PackagedFile {
    param([string]$RelativePath)
    $parts = $RelativePath -split '[\\/]'
    $normalizedPath = $RelativePath.Replace('\', '/')
    $excludedDirectories = @('target', '.deps', 'output', 'artifacts', 'pvd-cache', '__pycache__', '.vs', '.idea', 'CMakeFiles')
    return (
        @($parts | Where-Object { $_ -in $excludedDirectories }).Count -eq 0 -and
        $RelativePath -notmatch '\.(?:py[cod]|pvdstream|pvdcapture|pxd2?|ovd|pvdspool|partial|log|exe|dll|pdb|obj|o|a|lib|exp|ilk|map|dmp|zip|7z|bak|tmp)$' -and
        $RelativePath -notmatch '\.pvdstudio-clips\.json$' -and
        $parts[-1] -notin @('CMakeCache.txt', 'cmake_install.cmake') -and
        $normalizedPath -notmatch '^docs/local(?:/|$)' -and
        $normalizedPath -ne 'docs/CONTINUE-PVD-STUDIO.md'
    )
}

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$targetPath = if ([System.IO.Path]::IsPathRooted($TargetDirectory)) {
    $TargetDirectory
} else {
    Join-Path $projectRoot $TargetDirectory
}
$targetRoot = [System.IO.Path]::GetFullPath($targetPath)
$releaseDirectory = Join-Path $targetRoot 'release'
$sevenZip = Find-SevenZip $SevenZipPath
if ($ArchiveFormat -eq '7z' -and -not $sevenZip) {
    throw '7z packaging requires 7-Zip. Specify -SevenZipPath or use -ArchiveFormat zip.'
}
$distribution = Join-Path $projectRoot ('output\dist\pvd-studio-windows-x64-' + $BuildStamp)
$archive = "$distribution.$ArchiveFormat"
if ((Test-Path -LiteralPath $distribution) -or (Test-Path -LiteralPath $archive)) {
    throw "Package output already exists: $distribution"
}
Push-Location -LiteralPath $projectRoot
try {
    if (-not $SkipBuild) {
        & cargo build --release --locked --offline --target-dir $targetRoot -p pvd-desktop -p pvd-cli -j 2
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
    $executables = @('pvd-desktop.exe', 'pvd-cli.exe')
    foreach ($name in $executables) {
        if (-not (Test-Path -LiteralPath (Join-Path $releaseDirectory $name))) {
            throw "Missing release executable: $name"
        }
    }
    New-Item -ItemType Directory -Path $distribution | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $distribution 'pvd-cache/history') | Out-Null
    foreach ($name in $executables) {
        Copy-Item -LiteralPath (Join-Path $releaseDirectory $name) -Destination $distribution
    }
    Copy-Item -LiteralPath (Join-Path $projectRoot 'tools\start-with-diagnostics.cmd') -Destination $distribution
    $readme = Get-Content -LiteralPath (Join-Path $projectRoot 'docs\distribution.md') -Raw -Encoding utf8
    $readme = $readme -replace '\]\((?![a-zA-Z][a-zA-Z0-9+.-]*:|/|#)([^)]+)\)', '](docs/$1)'
    Set-Content -LiteralPath (Join-Path $distribution 'README.md') -Value $readme -Encoding utf8
    $documentationRoot = Join-Path $projectRoot 'docs'
    foreach ($file in Get-ChildItem -LiteralPath $documentationRoot -File -Recurse -Force) {
        $relativePath = $file.FullName.Substring($projectRoot.Length + 1)
        if (-not (Test-PackagedFile $relativePath)) { continue }
        $destination = Join-Path $distribution $relativePath
        New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
        Copy-Item -LiteralPath $file.FullName -Destination $destination
    }
    foreach ($dependency in @('bevy_winit', 'egui_dock', 'bevy_gizmo_lines')) {
        $dependencyPatch = Join-Path $projectRoot "vendor\$dependency"
        if (Test-Path -LiteralPath $dependencyPatch) {
            $dependencyNotices = Join-Path $distribution "licenses\$dependency"
            New-Item -ItemType Directory -Path $dependencyNotices -Force | Out-Null
            foreach ($license in Get-ChildItem -LiteralPath $dependencyPatch -Filter 'LICENSE*' -File) {
                Copy-Item -LiteralPath $license.FullName -Destination $dependencyNotices
            }
            Copy-Item -LiteralPath (Join-Path $dependencyPatch 'PVD-STUDIO-PATCH.md') -Destination $dependencyNotices
        }
    }
    $sample = Join-Path $projectRoot 'captures\physx-3.4.pvdstream'
    if (Test-Path -LiteralPath $sample) {
        $samples = Join-Path $distribution 'samples'
        New-Item -ItemType Directory -Path $samples | Out-Null
        Copy-Item -LiteralPath $sample -Destination $samples
    }
    foreach ($extension in @('pvdstream', 'json')) {
        $cctSample = Join-Path $projectRoot "captures\physx-3.4-cct.$extension"
        if (Test-Path -LiteralPath $cctSample) {
            $samples = Join-Path $distribution 'samples'
            New-Item -ItemType Directory -Path $samples -Force | Out-Null
            Copy-Item -LiteralPath $cctSample -Destination $samples
        }
    }
    foreach ($extension in @('pvdstream', 'json')) {
        $originSample = Join-Path $projectRoot "captures\physx-3.4-origin-shift.$extension"
        if (Test-Path -LiteralPath $originSample) {
            $samples = Join-Path $distribution 'samples'
            New-Item -ItemType Directory -Path $samples -Force | Out-Null
            Copy-Item -LiteralPath $originSample -Destination $samples
        }
    }
    $nativeSample = Join-Path $projectRoot 'captures\native\rich-paired-native.pxd2'
    $cameraSamples = Join-Path $projectRoot 'captures\cameras'
    foreach ($name in @('cameras.pvdstream', 'truth.json')) {
        $cameraSample = Join-Path $cameraSamples $name
        if (Test-Path -LiteralPath $cameraSample) {
            $samples = Join-Path $distribution 'samples'
            New-Item -ItemType Directory -Path $samples -Force | Out-Null
            $sampleName = if ($name -eq 'truth.json') { 'physx-3.4-cameras.json' } else { 'physx-3.4-cameras.pvdstream' }
            Copy-Item -LiteralPath $cameraSample -Destination (Join-Path $samples $sampleName)
        }
    }
    if (Test-Path -LiteralPath $nativeSample) {
        $samples = Join-Path $distribution 'samples'
        New-Item -ItemType Directory -Path $samples -Force | Out-Null
        Copy-Item -LiteralPath $nativeSample -Destination (Join-Path $samples 'physx-3.4-pvd3-rich.pxd2')
    }
    $nativeLimits = Join-Path $projectRoot 'captures\native\limits\joint-limits-live-native.pxd2'
    if (Test-Path -LiteralPath $nativeLimits) {
        $samples = Join-Path $distribution 'samples'
        New-Item -ItemType Directory -Path $samples -Force | Out-Null
        Copy-Item -LiteralPath $nativeLimits -Destination (Join-Path $samples 'physx-3.4-pvd3-joint-limits.pxd2')
    }
    if ($IncludeSource) {
        $source = Join-Path $distribution 'source'
        New-Item -ItemType Directory -Path $source | Out-Null
        foreach ($directory in @('crates', 'vendor', 'test-support', 'tools', 'docs')) {
            $sourceDirectory = Join-Path $projectRoot $directory
            foreach ($file in Get-ChildItem -LiteralPath $sourceDirectory -File -Recurse -Force) {
                $relativePath = $file.FullName.Substring($projectRoot.Length + 1)
                if (-not (Test-PackagedFile $relativePath)) { continue }
                $destination = Join-Path $source $relativePath
                New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
                Copy-Item -LiteralPath $file.FullName -Destination $destination
            }
        }
        foreach ($name in @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'README.md', 'AGENTS.md', '.gitignore', '.gitattributes')) {
            Copy-Item -LiteralPath (Join-Path $projectRoot $name) -Destination $source
        }
    }
    if ($IncludeSource) {
        $documentation = Join-Path $distribution 'docs'
        $linkPattern = '(?<open>!?\[[^\]\r\n]*\]\()(?<parents>(?:\.\./)+)(?<target>(?:tools|crates|vendor|test-support)/[^)\r\n]+)\)'
        foreach ($document in Get-ChildItem -LiteralPath $documentation -File -Recurse -Filter '*.md') {
            $contents = [System.IO.File]::ReadAllText($document.FullName)
            $updated = [regex]::Replace($contents, $linkPattern, {
                param($linkMatch)
                $parents = $linkMatch.Groups['parents'].Value
                $target = $linkMatch.Groups['target'].Value
                $original = Join-Path $document.DirectoryName ($parents + $target.Split('#')[0])
                $redirect = $parents + 'source/' + $target
                $sourceTarget = Join-Path $document.DirectoryName $redirect.Split('#')[0]
                if (-not (Test-Path -LiteralPath $original) -and (Test-Path -LiteralPath $sourceTarget)) {
                    return $linkMatch.Groups['open'].Value + $redirect + ')'
                }
                return $linkMatch.Value
            })
            if ($updated -ne $contents) {
                [System.IO.File]::WriteAllText($document.FullName, $updated, [System.Text.UTF8Encoding]::new($false))
            }
        }
    }
    $manifest = @(
        Get-ChildItem -LiteralPath $distribution -File -Recurse | Sort-Object FullName | ForEach-Object {
            $hash = Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256
            [PSCustomObject]@{
                path = $_.FullName.Substring($distribution.Length + 1).Replace('\', '/')
                bytes = $_.Length
                sha256 = $hash.Hash.ToLowerInvariant()
            }
        }
    )
    $manifest | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $distribution 'manifest.json') -Encoding utf8
    if ($sevenZip) {
        $compression = if ($ArchiveFormat -eq 'zip') {
            @('-tzip', '-mm=Deflate', '-mx=9', '-mmt=2')
        } else {
            @('-t7z', '-mx=9', '-mmt=2')
        }
        Push-Location -LiteralPath (Split-Path -Parent $distribution)
        try {
            & $sevenZip a @compression -bso0 -bsp0 $archive (Split-Path -Leaf $distribution)
            if ($LASTEXITCODE -ne 0) { throw 'Archive compression failed' }
            & $sevenZip t -bso0 -bsp0 $archive
            if ($LASTEXITCODE -ne 0) { throw 'Archive integrity check failed' }
        } finally { Pop-Location }
    } else {
        Compress-Archive -LiteralPath $distribution -DestinationPath $archive -CompressionLevel Optimal
    }
    Write-Output "Build folder: $distribution"
    Write-Output "Archive: $archive"
    Get-FileHash -LiteralPath $archive -Algorithm SHA256
} finally { Pop-Location }
