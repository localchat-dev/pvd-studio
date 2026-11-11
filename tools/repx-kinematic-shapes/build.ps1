param([string]$BuildRoot = '.deps/physx-repx-kinematic-shapes-build')
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$sdkRoot = Join-Path $projectRoot '.deps/physx-3.4'
$expected = '5e42a5f112351a223c19c17bb331e6c55037b8eb'
$revision = & git -C $sdkRoot rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $revision.Trim() -ne $expected) { throw 'Expected the pinned official PhysX 3.4 checkout.' }
$changes = & git -C $sdkRoot status --porcelain --untracked-files=no
if ($LASTEXITCODE -ne 0 -or $changes) { throw 'Official SDK tracked files must be unchanged.' }
$buildPath = Join-Path $projectRoot $BuildRoot
& cmake -S $PSScriptRoot -B $buildPath -G 'Visual Studio 18 2026' -A x64 "-DPHYSX34_ROOT=$sdkRoot"
if ($LASTEXITCODE -ne 0) { throw 'RepX kinematic shape producer configure failed.' }
& cmake --build $buildPath --config Debug --target pvd-repx-kinematic-shapes --parallel 2
if ($LASTEXITCODE -ne 0) { throw 'RepX kinematic shape producer build failed.' }
