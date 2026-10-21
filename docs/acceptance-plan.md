# 验收方法

功能状态使用“已实现”“部分支持”“待验证”“未实现”。只有明确记录数据来源、命令、实际结果和限制，才标记相应范围的验证通过。实现、合成测试、真实 SDK、原版互通和原生交互是不同证据。

## 基线检查

从仓库根目录执行：

```powershell
cargo fmt --all --check
cargo test --locked -p pvd-protocol -p pvd-core -p pvd-transport -p pvd-cli
cargo test --locked -p pvd-desktop -- --test-threads=1
cargo clippy --locked -p pvd-protocol -p pvd-core -p pvd-transport -p pvd-cli --all-targets -- -D warnings
cargo clippy --locked -p pvd-desktop --all-targets -- -D warnings
```

workspace 默认成员是 CLI，单独执行 `cargo test` 不覆盖全部核心与桌面目标。锁定依赖已经缓存时可以添加 `--offline`。上面的命令是复现入口，不表示每次文档整理均重新执行了全量测试；实际运行见[验证说明](validation.md)。

桌面基线使用单测试线程。完整并行门中的实时刷新时序用例曾失败，隔离复核与串行门通过；未调整判定条件，也不将串行结果表述为并行门已通过。

核心检查包括截断/损坏/异常长度、未知事件、资源边界与取消；对象创建/销毁/ID 复用和引用；同帧顺序/反向/随机一致性；TCP 分片/重连隔离、输出原子发布和缓存回收。

需要覆盖持久撤销读取计量与资源写入故障时，显式启用对应测试特性：

```powershell
cargo test --locked -p pvd-core --lib --features resource-test-write-faults,replay-test-read-counts
```

## 真实 SDK

真实输入不纳入源码仓库。需要 Git、CMake、MSVC C++ Build Tools 和 Windows SDK 等已有工具，SDK 与构建留在 `.deps/`，生产器只作为验证数据源。

```powershell
./tools/physx-sample/build.ps1
./tools/verify-sdk.ps1
./tools/physx41-sample/verify.ps1
```

首条路径是固定提交的 PhysX 3.4.2；4.1.2 独立记录，不能互相继承结果。输入由 SDK 自己的 transport 输出，位姿/几何/查询/时间由独立 getter 或回调采集。详细参数见 [3.4 生产器](../tools/physx-sample/README.md)和 [4.1 验证](../tools/physx41-sample/README.md)。

按专题生成和执行：

| 范围 | 入口 |
| --- | --- |
| 地形、查询、关节限位 | `tools/physx-features/verify.ps1` |
| Aggregate / Articulation | `tools/physx-objects/verify.ps1` |
| CPU Cloth / 粒子 | `tools/physx-cloth/verify.ps1`、`tools/physx-particles/` |
| 性能曲线与任意事件 | `tools/profile-events/verify.ps1` 与对应测试环境变量 |
| 多 Scene | [生产器与测试](../tools/physx-multiscene/README.md) |
| 捕获相机 | [生产器](../tools/physx-cameras/README.md)与[回放说明](source-cameras.md) |
| OriginShift | [生成、真值及显式测试](../tools/physx-origin-shift/README.md) |
| RepX | [导出与 SDK 重读](repx.md)、`tools/repx-joints/`、`tools/repx-shared-shapes/` |

忽略测试必须实际生成所需输入、设置环境变量并显式运行，不能将编译成功或默认忽略计为通过。大型/长时间专项只按实际输入和参数声明规模。

## 原版容器配对

保留原版安装文件和符号；不上传二进制、符号或捕获。格式证据、文件哈希和局部函数依据可以作为文字记录，原始材料留在未纳入版本管理的目录。

1. 使用真实 SDK TCP 生产器连接 PVD3，同时逐字节保存同一次原始流，并生成 SDK 独立真值。
2. 由 PVD3 保存容器；记录输入/输出大小、哈希、帧数、版本和源端开关。
3. 对照对象、属性、引用、几何、绘制和分析。容器内部 ID 与 SDK 源指针通过已确认映射比较，不能直接混同。
4. 验证顺序、反向、随机和边界帧；加入截断、冲突目录、取消和预算反例。
5. 对自写文件，额外要求原版打开、倒放、重存和语义核对。自己写自己读、扩展名相同或重建原文件字节均不足以证明完整互通。

已有 `native`、`native_debug_draw` 和 `native_frame_metrics` 集成目标包含显式专项。每个忽略原因、测试函数和专题规定具体输入位置及依赖，不直接将所有 `--ignored` 测试视为可在无样例环境运行。真实多页、非空起点和含 OriginShift 的原版文件仍需独立补齐。

## 桌面与 GPU

生成样例后使用无窗口渲染：

```powershell
cargo run --locked -p pvd-desktop -- --render-check captures/physx-3.4-rich.pvdstream output/scene-check.png 65 scene --language zh-CN
```

检查实际输出图，核对选择、属性、帧号、几何和布局；连续显示探针还应验证每个目标的接受、GPU 完成和 UI/Scene 一致性。退出后检查所属缓存清理，不能只看进程退出码。

离屏尺寸变化和 egui 输入测试不替代 Windows 文件选择器、剪贴板、IME、DPI、停靠或原版交互。原生操作保持必要范围，不修改系统配置；待验路径记录在[功能清单](manual-checklist.md)和[路线图](roadmap.md)。

## 持续接收与性能

- 同时接收与正倒放，确认解码持续提交且显示目标、保留区间和分析来源一致。
- 记录积压、历史物理占用、可用空间和退出清理，并用同次原始流抽样核对状态。
- 明确区分源端帧率、协议解析、核心 seek、模型提交、GPU 完成和原生窗口帧率。
- 对比需固定源码、程序、机器、输入、设置及保留区间；冷定位与热相邻步进分开。
- 记录失败和取消，不放宽断言来凑通过；资源预算不是进程/GPU 总内存保证。

每次结果应包含源码/提交或文件清单身份、工具链、输入来源/哈希、参数、命令、通过/失败/忽略数量，以及未测范围。多个重叠测试组不相加为唯一通过总数。
