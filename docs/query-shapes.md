# Sweep、Overlap 与批量查询

## 已验证范围

Windows x64 / MSVC，固定的官方 PhysX 3.4.2 源码提交 `5e42a5f112351a223c19c17bb331e6c55037b8eb`：

- 普通 Sweep、Overlap，以及 `PxBatchQuery` 的 Raycast、Sweep、Overlap 真实输出已解析。
- Box、Sphere、Capsule、ConvexMesh 查询几何，位置与旋转、方向、距离、过滤数据与标志、命中 Actor/Shape 引用、位置/法线/距离均与独立 SDK 返回值比较。
- 12 帧共 **420 次查询、312 个命中**：204 sweeps、192 overlaps、24 batch rays；同时包含无命中、双对象 Overlap、旋转几何、负 MTD 穿透距离和 Actor 地址复用。
- Convex 几何顶点逐项比较 SDK 已烹制网格 `getVertices()`，三角化复用已有捕获网格路径。展示不调用物理引擎，不重做查询。
- 逐帧逆向恢复与顺序/检查点状态一致性验证通过。32 位与大端仅通过结构测试；没有宣称这些平台上的 SDK 实测。

## 源码证据

- `PhysX_3.4/Source/PhysX/src/NpPvdSceneQueryCollector.h`：`PvdSweep`、`PvdOverlap`、`PvdSqHit` 原生布局。
- `NpPvdSceneQueryCollector.cpp`：普通与批量数组名称、几何/姿态/过滤数组引用、批量结果收集、几何类型对应的 QueryID。
- `PvdMetaDataPvdBinding.cpp`：查询属性注册、几何对象创建与销毁、无效命中对象清空、每帧查询数据发送。
- `PxQueryReport.h`：Sweep 初始重叠 MTD 允许负距离；Overlap 没有命中位置/法线字段。

这些是官方 SDK 线格式证据，不是原版 PVD3 文件容器或所有 SDK 版本的兼容证明。

## 公共模型与边界

`queries::collect(&SceneState)` 提供 `QueryScene.raycasts`、`sweeps`、`overlaps`。每条记录保留 `scene_id`、数组内 `index`、`batched`、SDK `kind`、`filter_data`、`filter_flags` 和捕获的 `hits`。Sweep/Overlap 的 `QueryGeometry` 包含有代际的对象引用、`Geometry`、位置和旋转。Sweep 起点与方向/距离决定其终点；命中距离单独保留，不将查询距离替换为命中距离。

几何数组和命中对象通过属性绑定代际解析，旧引用不会指向复用地址的新对象。缺失或错误代际的查询几何使该场景的查询可视化失败并产生诊断；其他场景仍可提取。每次提取跨普通、批量和场景共享 100,000 查询、100,000 引用命中、128 MiB 独立凸网格预算。数组范围、数量、有限数值、单位方向/四元数以及几何与 QueryID 的一致性均验证。未知数据不静默拼出几何。

以下信息没有出现在该 SDK 的 `PvdSweep`/`PvdOverlap` 记录内，因此不推断：

- Sweep inflation；显示捕获的基础几何，不能据此声称展示了膨胀壳。
- 回调过滤器代码、缓存参数、每条查询请求的全部 hit flags。
- 批量查询 `queryStatus`，包括结果缓冲区溢出状态。因此“捕获的命中为空”不能自动证明查询成功且完全未命中；独立测试显式检查 SDK 状态为 `eSUCCESS`。
- Overlap 命中位置和法线。只展示捕获的查询体与对象引用；SDK 提供的有效面索引保留。

首条验证样例覆盖单场景、正常完成的批量查询。多场景隔离、损坏范围、跨集合预算、32/64 位与端序有自动化结构测试；多场景独立时间对齐仍属于整体回放任务。

## 重复验证

在项目根目录运行；工具仅使用项目内依赖，不启动桌面窗口：

```powershell
.\tools\physx-features\build.ps1 -Target pvd-query-shapes -BuildRoot .deps/physx-query-shapes-build
.\.deps\physx-query-shapes-build\Release\pvd-query-shapes.exe captures/physx-3.4-query-shapes.pvdstream captures/physx-3.4-query-shapes.json
cargo test --locked -p pvd-core --lib queries::
cargo test --locked -p pvd-core --test query_shapes -- --ignored --nocapture
```

可设置 `PVD_QUERY_SHAPES_CAPTURE` 指向其他捕获；测试读取同名 `.json` 作为独立 SDK 真值。该实测默认被忽略，必须生成数据并显式运行，默认 `cargo test` 不等价于它已通过。

实现与生产器：`crates/pvd-core/src/queries.rs`、`crates/pvd-core/tests/query_shapes.rs`、`tools/physx-features/query_shapes.cpp`。
