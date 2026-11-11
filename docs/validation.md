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

## 录制包

[PVD 录制包](pvd-archive.md)的默认回归覆盖 ZIP 目录、解压、CRC、资源上限、取消及三端接入。构造 stored/deflate 包装的真实原版和 SDK 输入另核对回放、CSV、区间导出与整份保存，输出与未包装源逐字节比较；原版生成 ZIP 的互通保持独立验收。

## 界面与分发包

多选默认回归覆盖 Shift 增减、当前对象、数量上限、回放/TCP 中的代际检查，以及批量 RepX 的选择身份和共同依赖；视口拾取与对象树使用同一选择状态。Box 与 Sphere 双 Actor 批量导出已通过官方 SDK 导入/重存，核对 ID、名称、位姿、重复 ID 的 XML 字节一致性及无效第二 ID 不发布，方法见[SDK 生产器](../tools/physx-sample/README.md)。原生组合手势、全对象显示和其他批量组合仍分别验收。

无窗口渲染使用 `--render-check`，支持指定输入、帧、面板、语言和尺寸，见[验收方法](acceptance-plan.md)。需要查看生成图片；进程成功退出本身不证明显示正确。离屏检查不能替代文件对话框、停靠、IME、动态 DPI 等原生交互验收。

[生命周期检查](lifecycle-validation.md)在同进程中交替打开 SDK 流和原版容器，覆盖定位、关闭、取消和错误恢复。40 轮、120 次释放检查中，模型、渲染资源及历史文件均回到空载基线，退出后后台任务为零。此结果不表示长期远端 TCP 或 GPU 显存趋势已经验收。

分发包按[打包规范](packaging.md)核对文件清单、许可证、源码副本和程序哈希，并使用解压后的程序执行 CLI 和渲染检查。[界面示例](images/README.md)只展示界面形态。

## 已知问题与未验范围

- `network::tests::live_refresh_publishes_without_a_half_second_gate` 在并行测试中出现过时序失败；隔离及完整单线程测试通过，完整并行测试仍需复核。
- 工具栏文字或路径缺失报告在双语、录制相机和最小窗口离屏复核中未复现，原生环境仍需对应复现步骤。
- 原生保存对话框、完整停靠、动态 DPI、其他 IME、跨机器负载、长期边接收边保存及资源趋势未全部验收。
- 原版多页、非空起点、更多绘制、错误生命周期及保存/裁剪互验的范围分别维护在兼容矩阵中。

验证记录应绑定 Git 提交、输入摘要、参数和实际结果，分别记录失败及未执行项。性能测量需区分核心恢复、模型提交、GPU 完成与原生显示，并注明机器和构建配置。
