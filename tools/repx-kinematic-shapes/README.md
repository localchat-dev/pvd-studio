# 运动学几何的 RepX 验证

使用固定 PhysX 3.4.2 SDK 创建并 attach 真实 Shape，保存 PVD、官方 XML 和独立 getter，再导入项目整帧及每个 Actor 的 RepX，逐项比较。不运行窗口，不修改 SDK，不重新模拟录制来生成展示数据。

场景只有一帧，包含八个 Actor、六个 Shape、两个 Material、一个 TriangleMesh 和一个 HeightField：

| Actor | 状态 / 几何 | Shape |
| --- | --- | --- |
| kin-plane | 运动学 Plane | 独占，simulation + query |
| kin-triangle-exclusive | 运动学 TriangleMesh | 独占，simulation + query |
| kin-heightfield | 运动学 HeightField | 独占，simulation + query |
| query-plane | 非运动学 Plane | 独占，仅 query |
| kin-shared-a / kin-shared-b | 两个运动学 Actor 共用 TriangleMesh | 共享，simulation + query |
| static-shared | Static 共用上述 Shape | 同一 Shape / 资源 |
| zero-flags-box | Static Box，ActorFlags 和 Shape.Flags 均为 0 | 独占，无 simulation/query/visualization |

五个 Actor 处于运动学模式；不设置目标，不验证运动学目标、运动学速度或唤醒计时恢复。三类复杂几何的已验证运动学范围限这些非 trigger 的样例与参数；Plane 的其他非零合法 flags（含 trigger）有 SDK 创建/attach 源码依据及默认测试，但此工具不验证触发回调或所有碰撞排列。TriangleMesh / HeightField trigger 无论 simulation 位如何均拒绝。

## 运行

按 [SDK 生产器说明](../physx-sample/README.md)准备固定 commit `5e42a5f112351a223c19c17bb331e6c55037b8eb`。在仓库根目录执行：

```powershell
./tools/repx-kinematic-shapes/build.ps1
cargo build --locked -p pvd-cli
python tools/repx-kinematic-shapes/verify.py --cli target/debug/pvd-cli.exe --output output/repx-kinematic-shapes-check
$env:PVD_REPX_KINEMATIC_SHAPES_DIRECTORY = (Resolve-Path output/repx-kinematic-shapes-check).Path
cargo test --locked -p pvd-core --test repx_kinematic_shapes -- --ignored --nocapture
```

输出目录必须尚不存在。可用 --producer、--cli 显式指定程序位置；使用其他 Cargo target 目录时须给实际 CLI 路径。结果、输入 hash、getter 比较数及零 flags 对照写到指定目录的 result.json。fixture.json 固定 schema、SDK 编译版本与对象数量；验收不能使用其他捕获替代。

## 真实真值与边界

生产器分别检查 SDK attach 前后的 Shape 数量。getter 输出包括 Actor / Shape flags、位姿、质心、质量 / 惯量、材质参数及模式、几何类型、mesh scale / rotation / flags、顶点与索引、三角材质索引、HF 原始样本 / holes / tessellation / flags / thickness，以及 SDK PxMeshQuery 取得的 HF 三角形。共享 Shape 的 isExclusive=false 和相同 serial ID 在三个 Actor 中逐项核对；TriangleMesh 同时被独占及共享 Shape 引用，资源只写一次。

项目完整帧与八份选中 XML 全部经官方 importer 重读；对象数量、resource ID、共享/独占、getter 和依赖数量一起验证。SDK Reference 的七个非零 Actor 单独核对，zero-flags-box 的官方默认值损失单独记录。曲面资源使用 SDK cooker 重建 portable XML，不能据此声称保留 cooked BVH 或完整模拟内部状态。

零 flags 另有不含 PVD 的 Box probe：SDK getter 的 ActorFlags / Shape.Flags 都为 0；官方省略节点与显式空节点导入后分别为 1 / 11，其余 getter 比较且差异不计普通成功字段。主捕获中另有真实的 zero-flags-box，项目 whole / selected XML 必须明确输出非空文本 0，官方导入 getter 精确为 0。

该零集编码只针对固定 3.4.2 reader：它接受非空 0，并非通用数字 flags 解析；未知名称也会落为零。项目只为已经校验为数值 0 的字段输出该文本，未知输入位和错误类型继续拒绝，不能据此推广其他 SDK 或任意数字转换。

ignored 测试使用同次捕获检查共享 owner 的 OR 约束、切换为非运动学后的 simulation 几何拒绝、tri/HF trigger、缺失/过时代际资源，以及零集 XML 编码。Shape.Actor 仅是 PVD hint，不能替代实际选中 owner。Link 始终使用非运动学限制。运动学帧边界验证见 [运动学参数快照](../repx-kinematics/README.md)。

共用 getter、cooker 与官方 importer 入口位于 ../repx-meshes/mesh_probe.h；网格基准生产器创建六个 Static Actor，与本工具共用 getter、cooker 和 importer。源码与样例使用都不构成 PVD3 真实 RepX XML 的互通验收。
