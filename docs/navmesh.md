# NavMesh 叠加调试

桌面可将本地 Detour NavMesh 导入为独立静态图层，与 PhysX 捕获共用三维视口和相机。用途是检查导航表面与碰撞几何的位置、坡度及覆盖范围。导航数据不写入捕获，也不参与 PhysX 回放或模拟。

## 输入范围

导入按文件内容识别格式；`.bin` 只作为文件选择提示。

| 项目 | 当前范围 |
| --- | --- |
| 容器 | `MSET` NavMeshSet，版本 1，小端 |
| Tile 数据 | `DNAV`，版本 7 |
| 引用与多边形 | 32 位 Tile/Poly 引用，每多边形最多 6 个顶点 |
| 导航表面 | 普通多边形的 detail triangles，包含额外 detail vertices |
| 边线 | 普通多边形的各条边，同一 Tile 内去重；Tile 间共享边可能重复 |
| Off-mesh connection | 读取并跳过其表面，不把两点连接画成三角形 |
| TileCache / `TSET` | 当前拒绝；压缩层需要独立解压和重建流程 |
| 其他布局 | 大端、64 位引用、自定义版本或布局不作兼容推断 |

导入上限为 128 MiB、65,536 个 Tile、100 万个多边形、200 万个顶点、200 万个三角形及 300 万条边。预算分别检查，不由文件大小推断解码后的内存用量。

Unity 或服务端来源本身不能确定导出格式。实际结构体定义、引用宽度和对齐仍需以导出文件核对；此功能仅显示静态导航表面，不包含业务侧高度或区域修改，也不提供寻路、Crowd 或动态障碍重建。

## 使用

打开 PhysX 捕获后，从工具栏的 **NavMesh** 入口打开叠加窗口。窗口每次打开时居中，打开后可拖动和调整尺寸。在“导入目标”选择物理场景，使用“浏览…”或输入路径，再执行导入或替换。每个物理场景分别保留一份 NavMesh；替换和卸载只影响所选目标。也可以在启动时指定：

```powershell
./target/debug/pvd-desktop.exe captures/sample.pvdstream --navmesh path/to/map.bin
```

图层提供显示开关、半透明表面、多边形边线、透明度、聚焦和卸载。导入在后台完成，可取消；新文件读取或解析失败时保留原图层。损坏数据、截断、无效索引、非有限坐标及超出预算的输入均拒绝发布。

“与物理场景对齐”提供 XYZ 位移、XYZ 角度、正数统一缩放、镜像 X 和重置。顺序为镜像 X、缩放、依次绕 X/Y/Z 旋转，再平移到物理源坐标，最后使用与 PhysX 相同的上轴、手性和单位转换。默认勾选“镜像 X”，可手动取消；“重置对齐”也恢复勾选，导入或替换文件保留当前对齐设置。调整只影响显示，不修改源文件，也不自动推断坐标比例。

切换物理场景时，导入目标自动跟随当前显示场景；也可从下拉列表或已导入图层中选择另一个目标进行管理。每个目标独立保留文件、显示开关、透明度和对齐参数，初次导入默认镜像 X。导入期间切换场景不会改变该任务的目标，失败或取消不会替换已有数据。

场景身份包含会话和 Scene 代际，防止复用的 Scene ID 接收旧绑定。单场景视图显示对应图层及全局图层；“全部场景”显示当前捕获帧中实际存在的 Scene 代际对应图层。全局图层是独立目标，在各个场景显示，需要自行确认地图一致。回放换帧复用导航缓存。

最多保留 64 个图层，导航几何的源数组、渲染数组及 GPU 副本按 512 MiB 估算总预算限制；这是图层预算，不是整个进程的内存上限，导入任务另受单文件与解码上限约束。换捕获会释放旧会话的场景图层并取消其未完成导入，全局图层保留。图层和设置不随捕获保存或重启恢复。

## 验证

测试输入分为两类：固定版本 `@recast-navigation/core` 0.43.1 实际生成/导出的数据，以及手工构造的多 Tile/detail/off-mesh 数据。后者的生成入口为：

```powershell
python tools/navmesh/create-fixture.py
```

默认写出 `output/navmesh-validation/sample.bin` 和 `sample.expected.json`。它包含两个相邻 Tile、坡面、额外 detail 顶点及一条 off-mesh connection；期望表面为 8 个三角形，普通多边形边线为 8 条。此构造文件已通过 `@recast-navigation/core` 0.43.1 `importNavMesh` 读取，并将 `getNavMeshPositionsAndIndices` 返回的全部坐标与独立预期逐项核对。

```powershell
cargo test --locked --offline -p pvd-core --lib navmesh
cargo test --locked --offline -p pvd-desktop navmesh -- --test-threads=1
./target/debug/pvd-desktop.exe --render-check captures/sample.pvdstream output/navmesh-validation/overlay.png 0 navmesh --navmesh output/navmesh-validation/sample.bin
```

多场景离屏专项需要一份在目标帧含两个物理场景的捕获。按[双 Scene 生产器说明](../tools/physx-multiscene/README.md)生成 `output/multiscene-check/capture.pvdstream` 后执行；输出目录须为新目录：

```powershell
./target/debug/pvd-desktop.exe --render-check output/multiscene-check/capture.pvdstream output/navmesh-validation/scenes-zh 99 navmesh-scenes 1440 960 --language zh-CN --navmesh crates/pvd-core/src/navmesh/fixtures/recast-0.43.1.bin --navmesh-second crates/pvd-core/src/navmesh/fixtures/synthetic-detail.bin
./target/debug/pvd-desktop.exe --render-check output/multiscene-check/capture.pvdstream output/navmesh-validation/scenes-en 99 navmesh-scenes 1024 720 --language en --navmesh crates/pvd-core/src/navmesh/fixtures/recast-0.43.1.bin --navmesh-second crates/pvd-core/src/navmesh/fixtures/synthetic-detail.bin
```

专项通过实际异步文件导入分别向两个场景加载 1 Tile / 6 三角形和 2 Tile / 8 三角形的导航数据，设置不同的透明度、位移与镜像，切换两个场景再切到全部场景。每个输出目录包含六张窗口/纯几何图片及 `navmesh-scenes.json`；应检查 `failure` 为 `null`，各场景仅显示各自图层，全部场景显示两个图层，切换后各自参数不变。`--navmesh-second` 仅供此专项使用，日常多场景导入通过窗口操作。

核心与桌面 NavMesh 专项已通过，覆盖独立场景设置、替换失败与取消、异步任务目标固定、会话/代际隔离、图层和几何预算、渲染缓存复用与释放，以及双语小窗口布局和重新打开时居中。桌面构建、格式与 Clippy 检查通过；默认回归与权限限制见[验证说明](validation.md#编译与默认测试)。

真实 SDK 双 Scene 捕获与上述两份导航输入已完成中文 1440×960、英文 1024×720 无窗口检查。状态报告通过，窗口与几何图片已查看；单场景/全部场景分别显示 1/2 个独立图层，导入统计、透明度和居中布局符合预期。该组合验证显示与场景隔离，不是同地图的导航与碰撞配对。

上述文件是验证材料，不是游戏地图。真实 Unity/服务端导出文件及同地图 PhysX 捕获尚需配对检查：核对 Tile 数、导航表面、单位/手性、物理场景切换和原点。构造测试、上游交叉读取或离屏图片都不能单独替代该验收；原生文件对话框、取消手势和持续回放资源趋势仍需独立验收。
