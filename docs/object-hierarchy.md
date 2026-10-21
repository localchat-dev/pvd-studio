# Aggregate 与 Articulation：PhysX 3.4.2 实测

验证使用 Windows x64、固定官方 PhysX 3.4.2 checked SDK、PVD ALL。生产器只在独立进程中模拟，查看器从流恢复状态。

## 已实现与证据

| 项目 | 实现和验证范围 |
| --- | --- |
| Aggregate 成员 | `PxAggregate.Actors` 和 `Articulations` 的引用增删，实时流里的成员集合与 SDK `getActors` / `getAggregate` 真值一致 |
| 层级 | Scene → Aggregate → Articulation → root Link → 分支 Link；`InboundJoint` 展示为对应 Link 的子对象；共享对象保留共享引用标识 |
| Link 场景映射 | 从 `GlobalPose` / Shape `LocalPose` 和 Geometry 创建展示对象；Box、Capsule、Sphere 的世界姿态与 `PxShapeExt::getGlobalPose` 一致 |
| 状态 | Link 的 active/sleep 状态来自所属 `PxArticulation.IsSleeping`；不把缺少 Link 局部 IsSleeping 当作永久活动 |
| 引用生命周期 | `SceneState::articulation_for_link(id)` 沿 Parent 解析，逐跳使用捕获时的 generation；上限 64 Link，拒绝失效引用、错误类型和循环 |
| 回放 | 210 帧顺序重建与检查点跳转一致，全部帧反向检查，跨创建/销毁边界随机跳帧一致 |
| 树交互逻辑 | Aggregate 优先于 Scene 的重复 Actor 引用；双语标识、折叠状态和 generation 更新通过无窗口测试 |

树深度上限为 128，足以容纳官方 64-Link 上限与 Scene、Aggregate、Articulation、Shape、Geometry 层级；仍保留 100,000 行总限制。这些是树数据逻辑测试，不代表原生窗口鼠标交互或原版像素对照已经验收。

## 独立真值生产器

`tools/physx-objects/objects.cpp` 创建一个 capacity=16、selfCollision=false 的 Aggregate，内含 1 static Actor、1 dynamic Actor 和 4 个分支 Articulation Link。所有 Shape 均有实际 SDK 几何，Link Shape 含非零局部平移与旋转；关节的 ParentPose、ChildPose 也直接经 SDK getter 输出。

源端关闭重力，初始给 Root Link 一个冲量，让其先运动后自然睡眠。动态体插入后调用 `setWakeCounter(0.4f)`，以显式的 SDK 属性通知记录唤醒状态。第 60 帧从 Aggregate 移出动态体，第 90 帧移出 Scene 后重新加入 Aggregate；第 120 帧移出 Articulation，第 150 帧移出 Scene 后重新加入。SDK 复用相同地址，本项目必须得到新 generation 并重新绑定父子、Shape 和 Joint 引用。

`objects.objects.json` 由 SDK 的 `getActors/getLinks/getChildren/getArticulation/getInboundJoint/getGlobalPose/getLinearVelocity/getAngularVelocity` 和形状、关节 getter 输出，不由 pvd-studio 导出，也不复用其层级或姿态实现。

核心显式运行的测试核对 1,260 个 Shape 的世界姿态、几何尺寸及状态，1,050 组刚体速度，每帧 Aggregate 成员、Articulation 根、Link 子集、Parent/InboundJoint/Joint.Link 引用及其代际，并检查 SDK 无错误。普通测试另覆盖 64-Link 边界、65-Link 超限、循环、Parent 类型错误、父地址复用与重绑定。

## 真实帧边界

PhysX 3.4.2 的 `ScbScenePvdClient::frameEnd` 在动态 Actor/Link 位姿更新之前发送 EndFrame，故帧内展示位姿对应本次 simulate 前的 SDK getter 结果。Articulation 的整体属性还由 `Scb::Scene::processSimUpdates` 在同步后、EndFrame 前发送，因此其 IsSleeping 对应本次 fetchResults 后状态。生产器分别保存 step 前位姿/速度和 step 后 `articulation_sleeping_at_end`；测试按事件真实阶段对照。

这在入睡帧可出现“该帧 Link 位姿来自前一步，但 Articulation 已标睡眠”的记录状态。本项目保留 SDK 事件顺序，不重排捕获内容来消除这一差异。动态体插入时的初始 PVD 元数据也可能早于插入完成后的自动唤醒；本样例通过明确的 SDK wake-counter 属性调用验证最终通知，不能外推为已覆盖所有自动唤醒通知路径。

固定提交的源证据：

- `PhysX_3.4/Source/PhysX/src/PvdMetaDataPvdBinding.cpp`：PxArticulationLink 元数据、addChild/createInstance、ArticulationLinkUpdateOp、changeAggregateSubActors。
- `PhysX_3.4/Source/PhysX/src/buffering/ScbScene.cpp`：processSimUpdates 与 Articulation 同步。
- `PhysX_3.4/Source/PhysX/src/buffering/ScbScenePvdClient.cpp`：Articulation sendAllProperties 与 frameEnd 顺序。
- `PhysX_3.4/Include/PxArticulation.h`、`PxArticulationLink.h`、`PxAggregate.h`：公开 SDK getter、64-Link 上限与成员移出后的 Scene 行为。

## 可重复验证

在项目根目录运行：

```powershell
./tools/physx-objects/verify.ps1
```

脚本只构建后台 SDK 生产器、写入新输出目录、执行 Rust 核心测试和树数据测试；不启动 pvd-studio 窗口，不操作原版。默认输出到 `output/objects-validation/<timestamp>`，已有捕获不会覆盖。`-SkipBuild` 复用已有生产器；`-SkipDesktopTests` 可单独检查核心。测试通过 `PVD_OBJECTS_CAPTURE` 选择文件，并从同 stem 的 `.objects.json` 读取真值。真实 SDK 测试为 ignored，必须显式运行，不能当作默认 cargo test 已覆盖。

保留样例：

| 文件 | 字节 | SHA-256 |
| --- | ---: | --- |
| `captures/physx-3.4-objects.pvdstream` | 3968936 | `d3a18d34a26688e75cc98b9dfeaede704025a34f4590bf28464c59817e6ec4fc` |
| `captures/physx-3.4-objects.objects.json` | 997336 | `333f32d369f60529c97c464bbc7be175953b30427ca22fa3ac2659df879cf6cb` |

指针、时间戳与分配事件随运行变化，因此再生成的 SHA 不必一致；测试按同次运行的 SDK 真值核对。

## 尚未验收

PhysX 4.1 Articulation/ReducedCoordinate、其他 SDK 版本、64-Link 真实 SDK 场景、更多 Link 几何类别组合、Articulation RepX、原版 PVD3 打开/保存或原生交互对照均不属于这条通过声明。Aggregate/PxArticulationJoint 的属性检查来自录制元数据；当前测试没有声明完整 Joint 分析或约束可视化行为。
