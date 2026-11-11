# PhysX 3.4 真实数据生产器

此控制台程序链接官方 PhysX **3.4.2** SDK，运行真实 CPU 模拟，由 SDK 自己的 PVD 传输器输出数据。它仅作为兼容性测试数据源；pvd-studio 不链接此模拟器，也不通过重新模拟显示录制。

源码固定为 [NVIDIAGameWorks/PhysX-3.4](https://github.com/NVIDIAGameWorks/PhysX-3.4/tree/5e42a5f112351a223c19c17bb331e6c55037b8eb)，提交 `5e42a5f112351a223c19c17bb331e6c55037b8eb`。版本来自 `PhysX_3.4/Include/PxPhysicsVersion.h`，非仓库目录名猜测。

## 构建

从项目根目录在 PowerShell 运行：

```powershell
./tools/physx-sample/build.ps1
```

需要已有 Git、CMake、Visual Studio C++ Build Tools 和 Windows SDK。已验证工具链为 Visual Studio 18 2026 / MSVC 19.50.35730、Windows SDK 10.0.26100.0、CMake 4.3.2。脚本把官方源码放在项目 `.deps/physx-3.4`，构建在 `.deps`；不安装工具，不修改全局配置，也不重置已有 checkout。其他已安装版本可显式传入 `-Generator` / `-Toolset`，尚未验证。

官方 SDK 使用 checked 配置，生产器使用 Release 配置链接该 checked SDK。checked 保留 PVD 与参数验证，Release SDK 本身不启用 PVD。`modern-msvc.props` 只给 SDK 构建增加本地编译选项和兼容 include 路径：将旧编译器时代的警告保留为警告、限制新版本警告级别，并通过 `compat/typeinfo.h` 引入标准 `<typeinfo>`。官方 SDK 源码没有修改。

## 文件流与真值

```powershell
New-Item -ItemType Directory -Path captures -Force | Out-Null
& '.deps/physx-sample-build/Release/pvd-physx-sample.exe' --file captures/physx-3.4.pvdstream --truth captures/physx-3.4.csv --frames 120 --flags debug --lifecycle
& '.deps/physx-sample-build/Release/pvd-physx-sample.exe' --file captures/physx-3.4-all.pvdstream --truth captures/physx-3.4-all.csv --frames 120 --flags all --lifecycle --rich
& '.deps/physx-sample-build/Release/pvd-physx-sample.exe' --file captures/physx-3.4-rich.pvdstream --truth captures/physx-3.4-rich.csv --mesh-truth captures/physx-3.4-rich.mesh.json --frames 120 --flags all --lifecycle --rich
```

基础场景为 Y 向上、重力 `(0,-9.81,0)`、每帧 `1/60 s`、单 CPU worker、一个静态平面与落下的非等边盒子和球。对象名 `PvdStudioGround`、`PvdStudioBox`、`PvdStudioSphere`。在帧数的一半处将盒子改名；`--lifecycle` 在四分之三处销毁并重建球。地址是否复用由 SDK 分配器决定，运行结束打印 `reused_id=0/1`；不能把该开关本身当成 ID 复用证据。

`--rich` 增加胶囊、球形关节，以及 SDK Cooking 生成的非对称凸网格和三角形坡面；两种网格具有非单位缩放，凸网格还有缩放坐标系旋转。`--mesh-truth` 直接从 SDK mesh 对象读取烘焙后的顶点/索引、scale/rotation 与 Actor ID，输出独立 JSON，供验证资源解码。额外启用约束、接触和场景查询发送，每帧执行一次真实 raycast。`--flags all` 同时采集 DEBUG、PROFILE 和 MEMORY；另可用 `profile` 或 `memory` 单独取证。

`--emit-error` 在四分之一处调用 `setMass(-1)`，触发 checked SDK 自己的参数错误回调，并验证 Actor 质量未改变。该受控无效调用用于错误流取证，未通过手工构造 PVD 错误事件替代 SDK 行为。

`--query-truth PATH` 保存独立的 SDK raycast 结果，并在每帧追加动态 Box、Sphere 与 miss 查询。JSON 可与同一录制逐帧核对，位置/法线/flags/Actor/Shape 都来自 SDK 返回值；原有位姿 CSV 格式不变。验证命令和序列化证据见 [查询说明](../../docs/queries.md)。

CSV 直接调用 SDK `getGlobalPose()` 获取，列为 `frame,actor_id,name,x,y,z,qx,qy,qz,qw`，帧号从 0 开始。采样发生在 `simulate()` 之前：PhysX 3.4 的 `ScbScenePvdClient.cpp` 在本次模拟后的动态位姿发送之前结束 PVD 帧，以便与接触数据对齐。因此 CSV 与 PVD 帧表示相同阶段，不能将 post-fetchResults 位姿错配到前一帧。CSV 没有经 pvd-studio 解析器派生，可用于独立校验。

捕获保留 `.pvdstream` 扩展名，文件由官方 SDK 文件传输器写出。SDK 文件流与 PVD3 保存容器分别验证，不能依据扩展名推断原版可打开。每次地址、计时与分配会改变，文件 SHA-256 不要求跨运行一致；应比较语义状态。

SDK 直写 `.pxd2` 的路径也已单独验证：`--file captures/physx-3.4-sdk-file.pxd2`。它使用相同官方文件传输器；已有 PVD3 真实保存容器另行配对验证，两类输入的差异与范围见[原版文件说明](../../docs/original-file-format.md)。

## 多 Actor 规模样例

`--extra-boxes N` 增加网格排布的真实动态刚体，默认 `0`，上限 `10000`。每个刚体使用独立名字，所有新增刚体的位姿也从 SDK 写入同一 CSV；默认 baseline 仍为每帧两个动态刚体。

```powershell
& '.deps/physx-sample-build/Release/pvd-physx-sample.exe' --file captures/physx-3.4-scale.pvdstream --truth captures/physx-3.4-scale.csv --frames 120 --flags debug --lifecycle --extra-boxes 500
cargo test --locked -p pvd-core --test scale -- --ignored --nocapture
```

实测为 120 帧、1513 个当前对象、503 个 Shape；CSV 包含 60240 个独立 SDK 位姿。自动测试核对 16 个固定随机/边界帧的全部 8032 个位姿、名字和 Actor ID 集合，并检查全部帧的顺序与随机回放一致及 SDK 错误为空。该输入用 DEBUG 限制性能/内存事件开销，不能作为 eALL 大规模分析负载的结果。可通过 `PVD_SDK34_SCALE_CAPTURE` 指向另一次同参数生成的录制；同目录须有同名 CSV。

## 持续运动负载

`--continuous-motion` 仅作用于 `--extra-boxes N` 产生的非运动学动态 Box，要求 `N > 0`。它关闭这些 Box 的重力和睡眠阈值，由每步周期性线速度及固定角速度驱动真实 SDK 模拟，位置保持在初始网格附近；原有 Box、Sphere 和 rich 对象的行为不受该开关影响。不传该参数时保留下落、碰撞和自然休眠路径。

`--stats-truth PATH` 可独立使用。在每次 `fetchResults()` 之后直接读取 SDK `PxSimulationStatistics`，写入 `active_dynamic`、`active_kinematic`、`total_dynamic`、`total_static`；另列持续负载 Box 的总数、未休眠数和具有非零线速度的数量。连续模式逐步验证活动动态数量不小于持续 Box 数，且这些 Box 均未休眠并运动，失效或 PVD 连接丢失会以非零退出码报告。CSV 每 60 步刷新，并在正常结束时打印 SDK 活动数的最小值和最大值。默认模式不新增统计采样。

统计 CSV 的 `frame` 是源端循环序号，`elapsed_ms` 从场景初始化完成起计时。统计采样发生在对应步模拟后；既有位姿 CSV 在模拟前取值。两种 CSV 的同号行不能未经阶段对齐直接当作同一 PVD 帧状态，统计数也不能由 `IsSleeping` 或位姿变化替代。

`--duration-seconds N` 按场景初始化完成后的实际单调时钟运行，范围为 `1..3600` 秒；完成的模拟步达到该时长后正常结束。不指定 `--frames` 时，时长模式以 10,000,000 步为安全上限；显式 `--frames` 则作为较小的步数上限，若先耗尽步数，会保存已生成统计并以非零退出码报告未达到时长。输出的 `frames`、`measured_steps` 为实际完成步数，另打印实际 `elapsed_ms`。时长模式没有改变固定 `1/60 s` 的模拟步长，不等于严格 60 Hz；单次 SDK 模拟或发送阻塞仍可能延后退出。改名、`--lifecycle` 和 `--emit-error` 的触发位置仍按步数上限计算，不按墙钟比例计算。

若官方 SDK 已在 `.deps` 构建，可只构建独立生产器，保留原有基线可执行文件：

```powershell
cmake -S tools/physx-sample -B .deps/physx-sample-build -G "Visual Studio 18 2026" -A x64 "-DPHYSX34_ROOT=$((Resolve-Path .deps/physx-3.4).Path)"
cmake --build .deps/physx-sample-build --config Release --parallel 2
```

短时文件验证：

```powershell
$producer = '.deps/physx-sample-build/Release/pvd-physx-sample.exe'
& $producer --file output/continuous.pvdstream --truth output/continuous.csv --stats-truth output/continuous-stats.csv --frames 600 --interval-ms 16 --flags all --extra-boxes 128 --continuous-motion
```

启动待测监听器后，可显式执行至少十分钟的持续发送；该命令本身不启动或控制桌面：

```powershell
& $producer --host 127.0.0.1 --port 15425 --duration-seconds 600 --interval-ms 16 --flags all --extra-boxes 128 --continuous-motion --stats-truth output/continuous-tcp-stats.csv
```

该命令持续到至少 600 秒的一次模拟步结束，Windows 休眠粒度影响实际步数而不会将目标时长按固定步数拉长。调整 `--extra-boxes` 可增加持续活动负载，仍受 10,000 上限约束。长运行通常只需统计 CSV，完整位姿 CSV 会为每个 Actor 每步写一行。`--stats-truth` 自身的 SDK getter 与磁盘写入成本属于生产器成本，比较吞吐时必须保持参数一致。

持续运动专项需用 SDK 统计和独立位姿核对活动 Box 数、未休眠数及实际位移。比较默认模式时保持帧数和配置一致；生产器有效不等于消费者能持续实时处理。

时长参数验证需覆盖正常达到时长、提前耗尽帧数，以及 0 和 3601 等非法值。时长提前耗尽返回失败，正常完成的步数和统计时间应来自本次输出。

## 大量常驻分配与小分析缓冲

混合规模验证可增加 `--extra-static-boxes N`（0..20000，默认0）。这些静态Box位于动态负载之外，保留真实SDK对象与Shape/Geometry初始化成本。与 `--stats-truth PATH` 同用时，额外写出 `PATH.statics.csv`：通过SDK getter记录静态Actor/Shape ID、名称、位姿及半边长。每步同时用Scene Actor API的数量核对SDK静态统计；rich模式除这些Box外还有地面和网格坡面两个静态体。使用 `--resident-allocations 150000 --analysis-batches-per-frame 8 --extra-boxes 7 --continuous-motion --extra-static-boxes 16000 --flags all --rich` 可组合约48,000对象与大量分析数据；这不是用户远端捕获，也不代表异步多Scene。

短文件样例及独立核对：

```powershell
$producer = '.deps/physx-sample-build/Release/pvd-physx-sample.exe'
& $producer --file output/mixed.pvdstream --frames 12 --flags all --rich --extra-boxes 7 --continuous-motion --extra-static-boxes 64 --resident-allocations 150000 --analysis-batches-per-frame 8 --stats-truth output/mixed.stats.csv --analysis-truth output/mixed-analysis.csv
$env:PVD_MIXED_STATIC_CAPTURE = (Resolve-Path output/mixed.pvdstream).Path
cargo test --release --locked --offline -p pvd-core --test live_disk real_sdk_static_and_resident_analysis_fixture_matches_independent_getters -- --ignored
Remove-Item Env:PVD_MIXED_STATIC_CAPTURE
```

`--resident-allocations N` 分配并保留 `N` 个 16 字节内存块，范围 `0..200000`；`--analysis-batches-per-frame N` 在每个模拟步之前产生 `N` 个分析批次，范围 `0..64`。两个参数默认均为 `0`，不改变默认样例。启用任一参数必须使用 `--flags all`，且不能用于 RepX 导入模式。

这是独立测试负载：分配来自固定官方 SDK 的 `shdfnd::getAllocator()` 广播分配器；`PxCreatePhysics` 注册的 PVD allocation listener 经 `PvdMemClient` 与官方 `PxProfileMemoryEventBuffer` 编码后进入 SDK PVD stream。每个批次执行一次真实分配/释放、一对 `PxPvd::zoneStart/zoneEnd`，并调用同一 SDK 的内部 `PsPvd::flush()` 发送小缓冲。它没有手写或替换协议编码，也不表示这些测试分配来自游戏。生产应用不依赖这些内部 SDK 接口。

常驻分配类型名为 `AnalysisResident`，短命分配为 `AnalysisChurn`，测试文件标记为 `analysis-load`，每块均为 16 字节。短固定标记使大量分配的分析元数据成为主要负载，避免重复长路径主导历史序列化大小。常驻块一直保留到模拟循环结束，再通过相同分配器释放并 flush；这批释放位于最后一个已完成场景帧之后，不能当作该场景帧内已发生的释放。

`--analysis-truth PATH` 写入独立源端 CSV：`phase,frame,resident_live,allocation_bytes,allocation_calls,free_calls,profile_pairs`。记录初始化、每步批次结束、最终释放后的真实调用计数；帧为生产器循环编号。`allocation_bytes` 只计算测试常驻块有效载荷，不包括 SDK 其他分配、堆管理及分析器开销。正常结束时所有测试分配与释放计数应相等，profile 对数应等于实际步数乘每步批次数。

建议先生成短录制再比较同一个输入的解码行为：

```powershell
& $producer --file captures/analysis-resident.pvdstream --flags all --frames 180 --resident-allocations 150000 --analysis-batches-per-frame 8 --analysis-truth output/analysis-resident.csv --extra-boxes 7 --continuous-motion --stats-truth output/analysis-resident-stats.csv
```

150,000 个测试块有效载荷为 2,400,000 字节，但源端堆/PVD 追踪及消费者的分配记录、查找表、快照会占用更多内存。这个数量用于尝试跨过消费者的 32 MiB 分析回收触发条件，是否达到必须读取消费者实际计数，不以有效载荷大小推断。大量分配初始化与释放也会产生突发流量；比较持续吞吐时应分开统计初始化、稳定循环和收尾。降低到数千个块可先检查命令、真值及完整性，再逐步增加。

构建需要额外包含固定 SDK 的 foundation/pvd 内部头文件及已有 `compat/typeinfo.h`；不修改 SDK 源码。新增路径的编译、实际分配数量及吞吐结果由对应验证记录单独确认，参数存在不代表验证已经通过。

## RepX 参考与重读

`--export-repx PATH --export-frame 0` 在指定的模拟前帧阶段调用官方 SDK serializer，生成参考 XML。`--import-repx PATH --roundtrip-repx PATH` 使用官方 SDK importer 重建 collection，再序列化供独立属性比较；该模式不启动 PVD 连接和模拟。

Rust 导出器的范围、运行方式和 398 个属性的 SDK 互验见 [RepX 说明](../../docs/repx.md)。

多对象导出使用同次 SDK 位姿 CSV 校验所选 Actor 集合、ID、名称及姿态，再经官方 importer 重读、重导；同时检查重复 ID 去重和无效 ID 不发布部分文件。先按上文生成 rich 捕获，然后执行：

```powershell
cargo build --locked -p pvd-cli
./tools/physx-sample/verify-multiselection.ps1 -Capture captures/physx-3.4-rich.pvdstream -OutputDirectory output/multiselection-check
```

默认验证第 0 帧的 `PvdStudioBox`、`PvdStudioSphere`，CSV 使用捕获同名的 `.csv`。可通过 `-Frame`、`-ActorNames`、`-Truth`、`-CliPath` 和 `-SdkExecutable` 指定其他输入；所选 Actor 必须互相独立，没有额外连通 Actor。输出目录必须不存在，脚本在其中保存导出 XML、SDK 重导 XML、日志、比较结果和哈希，不构建或下载依赖。

多选显示可用同一 rich 捕获进行无窗口检查。`multi-selection` 在第 0 帧选择 Box 和 Sphere，显示所选包围盒及半透明效果，并聚焦两者的合并范围：

```powershell
cargo run --locked -p pvd-desktop -- --render-check captures/physx-3.4-rich.pvdstream output/multiselection-check/scene-zh.png 0 multi-selection 1440 960 --language zh-CN
cargo run --locked -p pvd-desktop -- --render-check captures/physx-3.4-rich.pvdstream output/multiselection-check/scene-en-small.png 0 multi-selection 640 480 --language en
```

检查输出图中的两组选中几何、包围盒、属性面板选择数量和底部 Shift 操作提示。成功退出只表示状态检查和图像输出成功，不代表显示验收；中文、英文及小尺寸布局应分别检查。

## TCP

先启动项目监听器，再运行：

```powershell
& '.deps/physx-sample-build/Release/pvd-physx-sample.exe' --host 127.0.0.1 --port 15425 --frames 120 --flags all --lifecycle --rich --truth captures/physx-3.4-tcp.csv
```

`--interval-ms 16` 可减慢生产速度用于实时观察。端口可配置；示例使用 `15425`，接收端需配置为相同端口。程序不打开窗口，也不操控桌面。

## 限制

- 当前样例只覆盖上述类型，不代表完整 PhysX 3.4、APEX 或其他 SDK 兼容。
- Convex、Joint、分析事件是否正确显示，必须查看项目解析/显示验证结果；产生数据不等于消费者通过。
- 大网格、长录制、跨 SDK、原版文件读写与 RepX 需要独立样例和验证。
- 第三方 SDK 的许可由官方 checkout 保留；`.deps` 中的 SDK 二进制不作为本仓库源码提交。
