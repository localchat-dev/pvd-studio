param(
    [Parameter(Mandatory = $true)][string]$Capture,
    [ValidateRange(1, 100)][int]$Repetitions = 5,
    [string]$ChunkSizes = '64,1024,8191,65536,1048576',
    [switch]$IncludeLive
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$capturePath = (Resolve-Path -LiteralPath $Capture).Path
$runDirectory = Join-Path $projectRoot ('output\fragmented-parser\' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
$instrumentedDirectory = Join-Path $runDirectory 'instrumented'
New-Item -ItemType Directory -Path $instrumentedDirectory -Force | Out-Null
Push-Location -LiteralPath $projectRoot
try {
    & cargo build --locked --offline --release -p pvd-protocol --example fragmented
    if ($LASTEXITCODE -ne 0) { throw 'Protocol benchmark build failed' }
    & target/release/examples/fragmented.exe $capturePath $Repetitions $ChunkSizes verify |
        Out-File -Encoding utf8 -LiteralPath (Join-Path $runDirectory 'protocol.csv')
    if ($LASTEXITCODE -ne 0) { throw 'Protocol benchmark or correctness checks failed' }

    $sourcePath = Join-Path $projectRoot 'crates/pvd-protocol/src/lib.rs'
    $source = Get-Content -Raw -LiteralPath $sourcePath
    $source = $source.Replace('pub struct Decoder {', @'
static PROFILE_EVENTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROFILE_U32_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROFILE_BUFFER_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn take_profile_work() -> (u64, u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (PROFILE_EVENTS.swap(0, Relaxed), PROFILE_U32_READS.swap(0, Relaxed), PROFILE_BUFFER_BYTES.swap(0, Relaxed))
}
pub struct Decoder {
'@)
    $source = $source.Replace('fn event(&mut self) -> Result<Event, Error> {', 'fn event(&mut self) -> Result<Event, Error> { PROFILE_EVENTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);')
    $source = $source.Replace('fn u32(&mut self) -> Result<u32, Error> {', 'fn u32(&mut self) -> Result<u32, Error> { PROFILE_U32_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);')
    $source = $source.Replace('self.buffer.extend_from_slice(&bytes[..take]);', 'PROFILE_BUFFER_BYTES.fetch_add(take as u64, std::sync::atomic::Ordering::Relaxed); self.buffer.extend_from_slice(&bytes[..take]);')
    [IO.File]::WriteAllText((Join-Path $instrumentedDirectory 'lib.rs'), $source)
    Copy-Item -LiteralPath crates/pvd-protocol/src/rewrite.rs -Destination $instrumentedDirectory
    $example = Get-Content -Raw -LiteralPath crates/pvd-protocol/examples/fragmented.rs
    $example = $example.Replace('            decoder.finish().unwrap();', '            decoder.finish().unwrap(); println!("profile_events_u32reads_bufferbytes,{chunk_size},{:?}", pvd_protocol::take_profile_work());')
    [IO.File]::WriteAllText((Join-Path $instrumentedDirectory 'fragmented.rs'), $example)
    $library = Join-Path $instrumentedDirectory 'libpvd_protocol.rlib'
    & rustc --crate-type rlib --crate-name pvd_protocol --edition=2024 -C opt-level=3 (Join-Path $instrumentedDirectory 'lib.rs') -o $library
    if ($LASTEXITCODE -ne 0) { throw 'Instrumented protocol build failed' }
    $benchmark = Join-Path $instrumentedDirectory 'fragmented.exe'
    & rustc --edition=2024 -C opt-level=3 --extern "pvd_protocol=$library" (Join-Path $instrumentedDirectory 'fragmented.rs') -o $benchmark
    if ($LASTEXITCODE -ne 0) { throw 'Instrumented benchmark build failed' }
    & $benchmark $capturePath 1 $ChunkSizes |
        Out-File -Encoding utf8 -LiteralPath (Join-Path $runDirectory 'function-counts.csv')
    if ($LASTEXITCODE -ne 0) { throw 'Instrumented benchmark failed' }

    if ($IncludeLive) {
        & cargo build --locked --offline --release -p pvd-core --example fragmented_live
        if ($LASTEXITCODE -ne 0) { throw 'Live benchmark build failed' }
        & target/release/examples/fragmented_live.exe $capturePath (Join-Path $runDirectory 'history') $Repetitions $ChunkSizes |
            Out-File -Encoding utf8 -LiteralPath (Join-Path $runDirectory 'live.csv')
        if ($LASTEXITCODE -ne 0) { throw 'Live benchmark or state equality failed' }
    }
    $sourceFiles = @($capturePath, $sourcePath, 'crates/pvd-protocol/src/rewrite.rs', 'crates/pvd-protocol/examples/fragmented.rs', 'crates/pvd-core/examples/fragmented_live.rs', 'crates/pvd-core/src/lib.rs', 'crates/pvd-core/src/live.rs')
    $sourceFiles | ForEach-Object { Get-FileHash -Algorithm SHA256 -LiteralPath $_ } |
        ConvertTo-Json | Out-File -Encoding utf8 -LiteralPath (Join-Path $runDirectory 'sha256.json')
    Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors |
        Format-List | Out-File -LiteralPath (Join-Path $runDirectory 'machine.txt')
    & rustc --version | Out-File -Append -LiteralPath (Join-Path $runDirectory 'machine.txt')
    Write-Output "Parser measurement artifacts: $runDirectory"
} finally {
    Pop-Location
}
