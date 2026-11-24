# 验证说明

功能与格式范围见[兼容矩阵](compatibility.md)，未完成项见[路线图](roadmap.md)。验证需要区分构造输入、真实 SDK 输出、原版配对文件和原生窗口操作；一种检查不能替代其他检查。

## 编译与默认测试

在仓库根目录执行：

```powershell
cargo fmt --all --check
cargo test --locked -p pvd-protocol -p pvd-core -p pvd-transport -p pvd-cli
cargo test --locked -p pvd-desktop -- --test-threads=1
cargo clippy --locked -p pvd-desktop --all-targets -- -D warnings
```

依赖已缓存时可加 `--offline`。桌面测试使用单线程，减少并行执行对时序断言的影响。默认测试中的 ignored 项需要额外输入，不能计为已执行或已通过。

Windows 默认回归已通过，范围包含本地 TCP、硬链接、目录联接及捕获发布后的读取校验。受限沙箱可能以 Windows 错误 5 拒绝文件系统测试；遇到权限失败，应在具备相应权限的环境复验，并分别记录受限环境结果，不能仅跳过失败项就报告全套通过。逐次执行的数量、耗时和日志保留在被忽略的生成目录中。

CLI 的取消测试使用进程共享的取消标志，并行执行时可能相互影响；遇到取消相关失败时需保留并行结果，并使用 `--test-threads=1` 串行复验四个核心 crate。串行通过不表示并行路径已经通过。

## 完整临时录制

退出回归包含封存写入、读回校验、等待发布锁和发布后回收的取消；还覆盖积压排空与程序退出的区别、界面隐藏资源的后台释放、两秒清理宽限不放弃永久导出、清理队列满时遗留文件恢复，以及超过一次扫描容量的接收分段分批回收。

真实 SDK TCP 专项使用 13,020 帧流触发第 12,000 帧封存，核对取消后后台任务停止、不再提交帧且不报告解码失败。五种隐藏子进程退出场景均通过，退出时后台任务和临时文件均为零。这些固定输入不提供任意磁盘状态或大场景的耗时保证；原生窗口及 GPU 退出交互未在该探针中执行。

真实退出测试需要显式提供输入：`PVD_EXIT_SEAL_CAPTURE` 指向足以触发封存的 SDK 流；`PVD_EXIT_PROBE_CAPTURE` 指向超过 1 MiB 的 SDK 流，`PVD_EXIT_PROBE_OUTPUT` 指定新的结果目录。对应 ignored 测试为 `network::tests::shutdown::real_tcp_exit_interrupts_an_in_flight_history_seal` 和 `studio::shutdown_tests::real_tcp_process_exit_releases_history_and_receive_files`。后者使用隐藏子进程与正式关闭系统，覆盖空监听、TCP 积压、历史定位、关闭后待打开文件及加载中退出，启用正式后台清理；它不代替原生窗口/GPU/DPI 交互验收。

[完整临时录制](temporary-recordings.md)的核心回归覆盖低热预算下多次封存、首帧保留、跨冷块正倒回放、旧快照、读写并发迁移、分析失败时保留场景历史、磁盘保留空间和导航容量边界。项目捕获的发布检查另覆盖写入索引可容纳但实际打开预算不足，以及读取校验期间取消。

桌面回归覆盖断开与重连、查看旧录制时继续接收、关闭和切换文件前排空接收缓冲、最新超预算录制保护、导出期间保护和实际文件回收。可聚焦执行：

```powershell
cargo test --locked -p pvd-core --lib live::persistent
cargo test --locked -p pvd-core --lib live::capture::tests::admission_tests
cargo test --locked -p pvd-core --test persistent_history
cargo test --locked -p pvd-transport finish_tests -- --test-threads=1
cargo test --locked -p pvd-desktop temporary_recordings -- --test-threads=1
```

真实 SDK 基线输入使用 `captures/physx-3.4.pvdstream`，多场景输入通过环境变量指定；显式执行两个 ignored 检查，不能把它们算作默认测试通过：

```powershell
$env:PVD_PERSISTENT_ROOM_CAPTURE = (Resolve-Path 'captures/multiscene.pvdstream').Path
cargo test --locked -p pvd-core --test persistent_history -- --include-ignored --nocapture --test-threads=1
```

4 项完整历史集成测试包括 120 帧 SDK 基线和 144 帧多场景文件；12 项内部持久历史测试和 2 项保存读取检查包含在上述核心单元回归中。3 项停止接收测试包含真实 TCP、积压排空与结束事件顺序，均通过。桌面的 11 项临时录制回归包含实际磁盘保存/重开/回收和真实 TCP 关闭后保留全部 100 帧；独立的 PhysX 3.4 多场景生命周期专项检查零帧销毁、无后续帧的销毁通知、销毁场景回放、同连接其他场景继续录制和断开，需按输入要求显式运行。

这些专项故意缩小分段和热预算以频繁跨块，并与相同 SDK 字节的完整内存解码结果逐帧对照；不是长期空间效率或原版性能对比。原生交互、长时录制和被其他进程锁定后的磁盘清理重试需要另行验收。

封存频率的回归另覆盖分析写入器固定预留、大于一半热磁盘预算的静态场景，以及无法靠封存释放的高元数据预留。真实 SDK 的 120 帧规模输入、6000 帧长输入及 13020 帧持续运动输入已核对；默认配置下前两者没有封存，后者封存一次并保留第 0 帧。首、中、末帧的完整状态、来源目录和正倒游标均与独立内存解码一致。测试方法见[实时吞吐](realtime-throughput.md#分层测量)。

单次封存仍同步写入和校验，因此频率修复不能视为异步历史写入完成，也不能保证封存期间时间轴持续刷新。长段封存耗时需要单独记录，不能用整段平均吞吐掩盖停顿。

## NavMesh 叠加

格式、操作和可重复命令见 [NavMesh](navmesh.md#验证)。默认专项使用固定版本上游实际生成的数据及手工多 Tile/detail/off-mesh 输入，分别检查几何提取和损坏/预算边界。`tools/navmesh/create-fixture.py` 的构造输入已由 `@recast-navigation/core` 0.43.1 交叉读取，全部表面坐标与预期相符；这不代表真实游戏导出的 ABI 与坐标已经验收。

核心与桌面 NavMesh 专项已通过，覆盖项集中记录在 [NavMesh 验证](navmesh.md#验证)。双语多场景离屏专项通过实际异步导入检查两个独立图层及设置，在两个单场景视图与全部场景间切换，并输出窗口、纯几何图片和状态报告；已检查居中布局、导入统计及可见范围。该专项不依赖 TCP 监听。

真实配对需提供同地图导航 `.bin` 与 PhysX 捕获，核对 Tile/多边形计数、detail 高度、坐标单位/手性、物理场景切换及 Scene ID 复用。原生文件对话框、取消操作和持续回放资源趋势另行记录，不用构造输入或离屏图替代。

## 真实输入

PhysX SDK 由独立生产器构建，详见[PhysX 3.4 生产器](../tools/physx-sample/README.md)。完整检查入口为：

```powershell
./tools/physx-sample/build.ps1
./tools/verify-sdk.ps1
```

该入口生成独立的 baseline、rich、查询和 TCP 数据，只运行与这些输入匹配的专项；原版容器、长跑及其他生产器的测试使用各自入口。默认包含桌面状态与离屏渲染，`-SkipDesktop` 可省略；`-IncludeScale` 增加规模场景。`-CargoConfig` 可显式记录 Cargo 配置，性能结果须保留实际构建参数。TCP 的位姿与网格真值来自同一次连接，不借用文件生产器的结果。

PhysX 4.1.2 的基础场景与普通射线使用 `tools/physx41-sample/verify.ps1`，地形、复杂查询和 ReducedCoordinate 的 Base 树使用 `tools/physx41-sample/verify-features.py`，命令见[4.1 生产器](../tools/physx41-sample/README.md)。文件和独立 TCP 捕获分别绑定同次 SDK 真值；关节根属性和 Link 状态按实际消息边界核对。结果不能由 3.4 外推。其他专项的命令与输入要求见[验收方法](acceptance-plan.md)及对应专题。

[同名性能区专项](../tools/physx-multizone/README.md)由真实 SDK 生成两个同名 Zone，核对跨帧配对、同键嵌套和未结束起点。直接 API 调用和 SDK 回调构成独立真值，不替代原版多 Zone 容器配对。

捕获和独立真值必须来自同一次运行。原版文件专项需要另行准备 PVD3 保存的容器及同次原始流，见[原版验证](native-validation.md)。包含固定摘要、对象数或帧数断言的测试必须使用匹配输入；重新生成的文件不能直接替代。例如 `PVD_SHARED_HISTORY_RAW` 指向固定 970 帧捕获，来源不足时保留忽略状态。

保留历史保存专项可在生成所需 rich/all 输入后执行：

```powershell
$env:PVD_RETAINED_SDK_DIRECTORY = (Resolve-Path 'captures').Path
cargo test --locked -p pvd-desktop studio::retained_capture::tests::sdk::actual_sdk_disk_save_reopen_and_display_match_independent_raw_prefixes -- --ignored --exact --nocapture
```

原版流式整份保存、多 Scene 裁剪、源身份和取消专项见[原版容器保存](native-export.md)。超过 512 MiB 的构造输入另覆盖实际文件与桌面后台打开/另存/关闭；该大小验证不代表同规模对象或事件负载。磁盘导入及完整 CSV 配对使用同次 rich 原版文件与 SDK 流：

```powershell
cargo test --locked -p pvd-core --test native original_rich_disk_history_preserves_all_frames_analysis_and_complete_csv -- --ignored --exact --nocapture
```

OriginShift 原版保存的构造专项不需要额外捕获输入，覆盖原始浮点位模式、辅助行、多页、非空初始快照整份保存、受限刚体前缀和中段，以及不支持范围的拒绝：

```powershell
cargo test --locked -p pvd-core --lib native::export::origin
cargo test --locked -p pvd-core --lib native::export::middle::origin_history
```

多 Scene 专项另用独立常量核对交错帧边界、各 Scene 平移标记、基线与原始浮点位模式、辅助行、磁盘正倒/随机恢复，并检查来源歧义、成员错配、预算和取消。该专项不表示包含 type 11 的真实原版容器或原版应用打开已经验收。

数组撤销专项覆盖单一完整属性来源、连续元素来源、索引移动、跨起点恢复、完整赋值及原版回溯插入/更新边界；使用手写数组值核对中段重开结果，并检查输出引用的顺序与目标。设置 `PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY` 可留存构造容器及 OriginShift 整份保存输入；默认不写文件。

消息专项覆盖所属类与继承、来源类型与目标宽度、严格按位别名、无匹配成员、重复/重叠成员、标量消息与数组历史的独立撤销规则，以及前缀/中段对数组目标和未知转换的拒绝。期望值由显式常量给出，扫描覆盖预算和取消；构造结果不代替原版消息互通验收。

[相机裁剪专项](native-settings.md#验证边界)覆盖多 Scene 前缀、中段、派生非空起点连续裁剪、目录生命周期、原始基线和撤销，以及内存与跨分段磁盘正倒放。稳定单区/多区性能起点组合另核对真实 Start、相机基线与各自撤销引用，以及共存索引的预算。OriginShift、未知布局和缺失性能起点等组合明确拒绝；构造容器不替代原版非空相机目录互验。

多 Scene 内存裁剪专项 `native::export::memory_middle::multi_scene_tests` 使用独立事件、来源总量和逐帧分配常量，核对前缀、中段、再次裁剪及磁盘正倒/随机恢复；帧间内存刷新仍拒绝。`camera_memory_multi_scene` 的五项默认测试另覆盖相机与共享内存组合，以显式常量核对空/非空起点、目录、销毁、撤销目标、来源总量及重复裁剪。构造输入不代替真实原版组合文件与原版重开。

[初始错误专项](native-errors.md)区分快照已有记录与区间内真实追加，覆盖多容器、重复 Index、销毁、帧间和末帧后边界、零帧、取消及共享预算。CLI 与桌面将起点记录独立展示，不提供虚构的接收位置。真实非空原版快照及原版打开仍需配对验证。

[内存来源总量](memory-history.md#可重复验证)使用同次 rich 原版文件与 SDK 存活分配逐来源核对。零帧起点、部分事件范围及 CLI/桌面缓存由独立构造专项覆盖；目录总量不作为完整前史的证据。

OriginShift 对象及空间数据分别由[独立生产器](../tools/physx-origin-shift/README.md)生成。空间专项覆盖两 Scene 的普通/批量查询、接触、SDK 渲染缓冲及显式 Scene ID/无 Scene 绘制；不代替原版 type 11 或原版绘制消费验证。

原版桌面分析专项还要求同源 CLI 对照 CSV。先构建 CLI，并在目标文件不存在时生成：

```powershell
cargo build --locked -p pvd-cli
New-Item -ItemType Directory -Path captures/native/reference -Force
./target/debug/pvd-cli.exe profile-csv captures/native/rich-paired-native.pxd2 captures/native/reference/native-profile.csv
./target/debug/pvd-cli.exe memory-csv captures/native/rich-paired-native.pxd2 captures/native/reference/native-memory.csv
cargo test --locked -p pvd-desktop native -- --include-ignored --test-threads=1
```

此项核对桌面任务与 CLI 的完整发布结果；逐事件数值正确性仍由原版与同次 SDK 配对专项验证。

原版完整线程时间窗的同源配对、显示帧切换和取消专项见[线程时间线](full-file-thread-timeline.md)。[RepX 官方导入器互验](repx.md)覆盖关节、Aggregate、Articulation 及受限 CPU 粒子，包含完整帧、选中依赖、更新与销毁重建。[受限 eERROR 专项](../tools/repx-articulations/README.md#eerror-交集互验)另核对原始消息边界、目标位模式，以及官方写出精度与读取限制。真实输入专项需要同次捕获与独立真值，默认忽略；不能用默认测试数量代替实际互验。

[运动学参数快照专项](../tools/repx-kinematics/README.md)使用同次八帧捕获、SDK getter、官方 XML 和传输边界核对导出；[休眠采样专项](../tools/repx-sleeping/README.md)分别覆盖普通与首帧前设置目标的两种模式。真实 ignored 测试核对原始尾部、零帧初值和已提交显示，不从混合观察推断目标有效性。运动学 Plane、TriangleMesh、HeightField、非运动学 query-only Plane、共享身份及零 flags 使用[几何专项](../tools/repx-kinematic-shapes/README.md)核对同次捕获与官方 importer getter。参数快照的恢复限制与验收状态集中见 [RepX](repx.md)。

## 帧来源与 APEX

默认回归已覆盖帧源、逐源绘制窗口、PVDCAP03 与 APEX SDK 裁剪，包含来源序号/全局索引映射、各来源时间零点与 CSV 来源列、滚动缺口、实时切换、每源绘制下界、分块属性提交、预算和取消，以及停滞来源、绘制日志回收与准备失败后的游标回滚；捕获专项还检查旧版重存，以及重新计算外层校验和后仍拒绝错误的来源目录。可单独执行：

```powershell
cargo test --locked -p pvd-core --lib frame_source
cargo test --locked -p pvd-core --lib source_debug
cargo test --locked -p pvd-core --lib live::capture::tests::source_tests
cargo test --locked -p pvd-core --test apex_clips
cargo test --locked -p pvd-desktop frame_sources -- --test-threads=1
```

[APEX 1.4 Clothing 生产器](../tools/physx-apex-clothing/README.md)分别生成同步和异步 DEBUG 捕获、getter 真值及原始事件目录，并提供 ignored 测试命令。两种文件捕获已覆盖两个来源的完整边界、共享 Asset、Actor 生命周期、局部/世界坐标、正倒/随机回放、分块解码和项目捕获重开；实际画面另经无窗口渲染检查。SDK 裁剪核对原始 payload 与端点；不能只以两次使用同一核心解析器得到相同结果代替独立真值。

真实 SDK SocketTransport 的同步与异步模式另通过受控回环检查：在接收期间切换来源、暂停、逐帧、正倒放及返回实时，核对断开后的显示、原始流保存、PVDCAP03 重开和历史释放。两个来源各有 36 个完整边界，显示状态对照同次原始事件与 getter；这项检查不代表持续吞吐或跨机器验收。

同步与异步滚动专项已通过：同一连接每来源 360 个完成边界，历史配额为 4 MiB、每 4 帧分段；已检查旧帧实际淘汰、累计解码绘制量超过 64 MiB，以及保留窗口正倒/随机回放、保存重开和关闭清理。接收字节、原始 renderer 字节、解码图元估算及逐步采样的历史峰值分别记录，不能以循环次数推定发生回收。命令与输入要求见[生产器的滚动专项](../tools/physx-apex-clothing/README.md#rolling-history)。同份输入另通过 997 字节分块核心复验及独立 renderer 全部顶点核对。两种模式各接收 55,437,506 字节，累计解码图元估算均为 80,262,720 字节；历史采样结果如下。受控步进不代表持续吞吐、跨机器或长时间负载。

| 模式 | 历史采样峰值（字节） | 最终保留全局帧 |
| --- | ---: | --- |
| 同步 | 4,062,657 | 692–719，共 28 帧 |
| 异步 | 4,059,841 | 688–719，共 32 帧 |

APEX ALL 输入可能缺少内存字符串定义；桌面保留场景和分析失败诊断，完整 SDK 性能目录、曲线、线程窗口及索引独立扫描，内存和完整分析 CSV 仍严格拒绝。独立原始性能 oracle 及核心专项见[性能验证](../tools/physx-apex-clothing/README.md#verify-profile-analysis)。一份缺字典 ALL 输入已逐字段核对 138 个事件键、6,264 个完整区间和双来源共 72 个完成边界；流式与索引结果使用同一独立真值。桌面另通过实际后台目录的两种排序、线程时间窗、事件曲线、失败不发布与资源清理。使用同一输入复验：

```powershell
$env:PVD_APEX_ALL_CAPTURE = $env:PVD_APEX_PROFILE_CAPTURE
cargo test --locked -p pvd-desktop studio::analysis_file_tests::profile::apex_all_profile_workers_remain_available_after_memory_dictionary_failure -- --ignored --exact --nocapture --test-threads=1
```

性能查询成功不表示全部 ALL 分析完整，不补造内存字典。异步末尾绘制没有后续 End 时保留为未提交尾部。来源与编号规则见[帧来源](frame-sources.md)，原版容器互通仍按独立专项验收。

## 多房间

[房间列表与显示范围](rooms.md)的构造回归、PhysX 3.4.2 文件/受控 TCP 和四组双语无窗口检查已通过。对应任务为 `ROOM-01`；这些结果不扩大其他 SDK、原版格式互通或原生交互的验收范围。

构造检查覆盖编号与代际、共享依赖、房间切换、零帧和末帧后销毁、销毁后的倒放、历史边界及元数据保存/截取；Legacy 另核对共享帧轴、生命周期边界和切换失败保留显示。定向入口：

```powershell
cargo test --locked -p pvd-core --lib rooms
cargo test --locked -p pvd-core --lib frame_sources
cargo test --locked -p pvd-desktop room -- --test-threads=1
```

[多 Scene 生产器](../tools/physx-multiscene/README.md#room-lifecycle)的 `--lifecycle` 模式提供同次 SDK Scene 身份、字节边界、存活列表和位姿真值。按生产器说明生成输入后运行：

```powershell
$env:PVD_ROOM_LIFECYCLE_CAPTURE = (Resolve-Path output/room-validation/file/lifecycle.pvdstream).Path
cargo test --locked -p pvd-core --test room_lifecycle -- --ignored --nocapture --test-threads=1
$env:PVD_ROOMS_PRODUCER = (Resolve-Path .deps/physx-multiscene-build/Debug/pvd-physx-multiscene.exe).Path
cargo test --locked -p pvd-desktop studio::rooms::tests::tcp -- --ignored --nocapture --test-threads=1
```

真实双 Scene 输入已核对显示范围与对象树。四 Scene 生命周期文件专项按独立字节边界核对零帧销毁、末帧后销毁和未确认销毁的断开；八个完成边界经过完整、前缀、中段、尾段保存/重开及再次裁剪，逐帧正倒恢复与源状态和来源目录一致。房间目录的期望位置来自同次 SDK 检查点，较早区间不带入后续创建或销毁，释放后检查缓存及输出清理。

受控 TCP 专项覆盖无后续物理帧时的生命周期发布、房间切换、正倒放、返回实时、断开和原始流镜像一致性。实际生命周期样例未发生 Scene 地址复用；该边界由构造输入另验，不能将分配器未复用地址的结果作为真实复用验证。

无窗口渲染使用 `rooms` 和 `room-destroyed` 面板，沿用[验收方法](acceptance-plan.md#桌面与-gpu)的参数；实际图片和输出的房间身份、选择、显示范围均已核对：

| 场景 | 中文尺寸 | 英文尺寸 | 核对内容 |
| --- | --- | --- | --- |
| 双 Scene，帧 99 | 1440×960 | 1024×720 | 房间 2，仅显示其 2 个 Actor |
| 四 Scene 生命周期，帧 3 | 1024×720 | 1440×960 | 已销毁房间 2，仅显示其 1 个 Actor；列表保留零帧销毁房间 3 和未确认销毁房间 4 |

跨机器持续吞吐、长时间频繁创建销毁、全部绘制类别及原生输入仍需独立检查。

## 录制包

[PVD 录制包](pvd-archive.md)的默认回归覆盖 ZIP 目录、解压、CRC、资源上限、取消及三端接入。构造 stored/deflate 包装的真实原版和 SDK 输入另核对回放、CSV、区间导出与整份保存，输出与未包装源逐字节比较；原版生成 ZIP 的互通保持独立验收。

## 界面与分发包

[坐标与网格](coordinates.md)的无限参考网格按视图射线与地面求交，数值检查与 GPU 图片分别核对世界单位、地面交点、近远裁剪和恒定资源。固定捕获帧的相机序列覆盖大范围平移、远距离缩放、转向、地平线、地下、向上观察、倾斜录制相机及共面深度；缩小时允许显示更粗的五倍网格。自动输入与无窗口图片不代替原生鼠标操作验收。

多选默认回归覆盖 Shift 增减、当前对象、数量上限、回放/TCP 中的代际检查，以及批量 RepX 的选择身份和共同依赖；视口拾取与对象树使用同一选择状态。Box 与 Sphere 双 Actor 批量导出已通过官方 SDK 导入/重存，核对 ID、名称、位姿、重复 ID 的 XML 字节一致性及无效第二 ID 不发布，方法见[SDK 生产器](../tools/physx-sample/README.md)。原生组合手势、全对象显示和其他批量组合仍分别验收。

无窗口渲染使用 `--render-check`，支持指定输入、帧、面板、语言和尺寸，见[验收方法](acceptance-plan.md)。需要查看生成图片；进程成功退出本身不证明显示正确。离屏检查不能替代文件对话框、停靠、IME、动态 DPI 等原生交互验收。

[生命周期检查](lifecycle-validation.md)在同进程中交替打开 SDK 流和原版容器，覆盖定位、关闭、取消和错误恢复。40 轮、120 次释放检查中，模型、渲染资源及历史文件均回到空载基线，退出后后台任务为零。此结果不表示长期远端 TCP 或 GPU 显存趋势已经验收。

分发包按[打包规范](packaging.md)核对文件清单、许可证、源码副本和程序哈希，并使用解压后的程序执行 CLI 和渲染检查。[界面示例](images/README.md)只展示界面形态。

## 已知问题与未验范围

- `network::tests::live_refresh_publishes_without_a_half_second_gate` 在并行测试中出现过时序失败；隔离及完整单线程测试通过，完整并行测试仍需复核。
- pvd-cli `atomic_output::tests::cancellation_after_the_writer_returns_prevents_publication` 在并行测试中因进程级取消状态竞争失败；隔离及单线程运行通过，完整并行路径仍需复核。
- 工具栏文字或路径缺失报告在双语、录制相机和最小窗口离屏复核中未复现，原生环境仍需对应复现步骤。
- 原生保存对话框、完整停靠、动态 DPI、其他 IME、跨机器负载、长期边接收边保存及资源趋势未全部验收。
- 原版多页、非空起点、更多绘制、错误生命周期及保存/裁剪互验的范围分别维护在兼容矩阵中。

验证记录应绑定 Git 提交、输入摘要、参数和实际结果，分别记录失败及未执行项。性能测量需区分核心恢复、模型提交、GPU 完成与原生显示，并注明机器和构建配置。

Unity 编辑器插件的安装包、TCP 最新帧接收、完整长录制、跨块回放及 Scene 视图现场验证见 [Unity 插件](unity.md#验证)。最新帧探针检查零磁盘缓存和重新打开视图时的帧恢复。实际地图的主线程帧耗时用独立的编辑器对照测量。

单文件分段回归可运行 `cargo test --locked -p pvd-core portable`、`cargo test --locked -p pvd-core packed` 和 `cargo test --locked -p pvd-desktop desktop_opens_packed_range_and_resaves_to_one_compatible_capture`。跨端验证用插件的 `--portable --output` 探针生成录制，只复制导出的 `.pvdcapture` 到新目录，执行 `pvd-cli verify <复制后的文件>`，再用桌面 Studio 定位到内部段边界两侧。
