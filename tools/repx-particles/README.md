# Particle RepX 验证

使用固定 PhysX 3.4.2 SDK 生成同次 PVD 捕获、独立 getter 真值和官方 RepX。先验证官方自身往返，再将项目导出的整帧和单个粒子系统导入官方 SDK，逐项比较 getter。生产器覆盖 ParticleSystem、ParticleFluid、32 位 bitmap 边界、多空洞、参数更新、粒子删除及索引复用、对象销毁重建、逐粒子 RestOffset、空系统及重新填充，以及两个粒子类型的 ClientBehaviorFlags 全部 16 种组合与清零。

在仓库根目录运行：

```powershell
./tools/physx-sample/build.ps1
./tools/repx-particles/build.ps1
cargo build --locked -p pvd-cli
python tools/repx-particles/verify.py --output output/repx-particles-validation
```

输出目录必须尚不存在。`--producer-only` 仅执行官方自身往返；`--cli`、`--producer` 可指定程序位置。检查 SDK 错误输出、捕获摘要及比较值数，结果写入指定目录的 `result.json`。无窗口运行，无需 GPU。

十八帧中，前四帧检查官方参考 XML 自身往返；其余非空帧以独立 getter 核对项目导出的稀疏数组，帧 6 核对空系统的项目导出与官方原始 XML 的拒绝。两个粒子类型均在非空帧覆盖客户端通知标志的全部 16 种组合，帧 17 清零。getter JSON 使用 schema 2，包含编译 SDK 版本及每个系统的 `client_flags`；验证入口要求恰有 18 帧。官方 serializer 对稀疏缓冲只写粒子数量对应的数组前缀，importer 却按原始索引读取，因此不将其稀疏参考 XML 送入 importer。`--producer-only` 不验证项目捕获、稀疏导出或全部客户端通知标志的导入往返。

空快照专项覆盖两种粒子类型、有无逐粒子 RestOffset、创建即空、稀疏填充、清空及再填充，并核对默认和扩展读取标志、零 ActorFlags 及全部客户端通知标志：

```powershell
python tools/repx-particles/verify-empty.py --output output/repx-empty-particles-validation
$env:PVD_REPX_EMPTY_PARTICLE_DIRECTORY = (Resolve-Path output/repx-empty-particles-validation).Path
cargo test --locked -p pvd-core --test repx_particle empty_particle_creation_clear_refill_and_read_flags_match_sdk -- --ignored --exact
```

该脚本使用生产器的 `--empty` 模式，生成四帧、每帧八个系统，核对整帧与 32 份选中导出的 SDK getter 和可表示为 `f32` 的浮点位模式。输出目录须不存在，`--cli` 和 `--producer` 可指定程序位置。

有效范围等于容量且为 32 的倍数时，固定 SDK 的 PVD 发送路径会多带一个不属于 bitmap 的 word。导出只采用容量内经过数量和最高索引校验的有效位，并写出规范长度的 bitmap；范围未到容量时，额外 word 的置位仍拒绝。专项将容量设为 64，用独立 getter 核对该边界。

核心负向测试使用同次捕获：

```powershell
$env:PVD_REPX_PARTICLE_DIRECTORY = (Resolve-Path output/repx-particles-validation).Path
cargo test --locked -p pvd-core --test repx_particle -- --ignored
```

导出限 CPU，保留非空快照中连续或稀疏的原始粒子索引。非空快照需要完整位置、速度、有效标志和启用时的逐粒子 RestOffset；不恢复碰撞结果、密度或 solver 历史。稀疏数组按有效 bitmap 展开，未启用槽仅作结构占位，导入器不会读取这些槽或创建对应粒子。粒子标志只能为 `eVALID`，基础标志须保留构造时的碰撞、启用和缓存提示，非空快照的读取标志只允许上述必需缓冲。

空快照要求实际 `NbParticles` 和 `ValidParticleRange` 都为零，容量至少为一。SDK 在空帧不清除旧数组，导出只保存当前系统参数。官方 importer 拒绝扫描范围为零的 XML，因此 XML 使用一个全零 bitmap 槽，粒子数仍为零；带逐粒子 RestOffset 的系统另保留一个未启用的偏移槽，以确定构造类型。官方 getter 的粒子数、有效范围和粒子列表仍为空，不创建占位粒子。空快照可保留合法读取标志的构造默认值及其超集；清除默认标志、未知标志和 GPU 状态仍拒绝。官方 serializer 重存空系统时会再次写出零扫描范围，其原始 XML 仍不能直接被该 importer 重开。

Fluid 的官方属性读取顺序会先设置 GridSize、后设置 RestParticleDistance，网格经过两次量化；仅接受该顺序下数值保持一致的参数。每系统容量最多 100 万，展开后的索引范围计入共享导出预算。ClientBehaviorFlags 只保存 SDK 定义的四位 Actor 参数，不创建 Scene client 或回调。Aggregate 粒子成员、GPU 以及未捕获的状态不在此验证范围。

官方源码依据为 `SnRepXCoreSerializer.cpp` 的 `PxParticleRepXSerializer`、`ScParticleSystemCore.cpp` 的网格量化和默认标志、`PxParticleCreationData.h` 的只读粒子标志约束。SDK 固定版本和构建前提见 [SDK 生产器](../physx-sample/README.md)。
