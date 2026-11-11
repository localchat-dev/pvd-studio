# 多性能区验证

生产器使用固定 PhysX 3.4.2 SDK 的两个 `PxProfileZone`，通过 SDK 回调和 `PvdDataStream` 写出真实通信流。两个区使用相同名称、事件号、线程和上下文，用区实例身份区分数据。8 帧包含跨帧、嵌套、单侧先结束及一个未结束事件。

独立真值记录直接 API 调用、明确配对、帧位置、QPC 上下界及 SDK 回调原字节。测试先独立解码回调并核对调用，再检查导入、8 个前缀的完成事件和未结束起点，以及反向对象恢复。此路径不生成原版数据库，也不证明原版多区容器互通。

在仓库根目录执行，需要已有 Windows C++ 工具链、CMake 和固定 SDK：

```powershell
./tools/physx-sample/build.ps1
cmake -S tools/physx-multizone -B .deps/physx-multizone-build -A x64 "-DPHYSX34_ROOT=$PWD/.deps/physx-3.4" "-DPVD_STUDIO_ROOT=$PWD"
cmake --build .deps/physx-multizone-build --config Debug --parallel 2
$output = Join-Path $PWD ('output/sdk-multizone/' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
New-Item -ItemType Directory -Path $output | Out-Null
$env:PVD_MULTIZONE_SDK_CAPTURE = Join-Path $output 'multizone.pvdstream'
$env:PVD_MULTIZONE_SDK_TRUTH = Join-Path $output 'multizone.json'
./.deps/physx-multizone-build/Debug/pvd-physx-multizone.exe $env:PVD_MULTIZONE_SDK_CAPTURE $env:PVD_MULTIZONE_SDK_TRUTH
cargo test --locked -p pvd-core --test profile_multizone_sdk -- --ignored --nocapture
```

生产器拒绝覆盖既有捕获和真值。测试默认忽略，必须实际生成输入并显式运行；SDK 文件与原版容器的验收状态分别记录。
