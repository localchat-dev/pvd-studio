# PhysX 3.4.2 粒子系统与流体粒子

`pvd_core::particles::collect(&SceneState)` 从当前捕获快照读取 `physx3::PxParticleSystem` 和 `physx3::PxParticleFluid`，返回系统、有效粒子及诊断。该模块不依赖 Bevy/egui，也不推进粒子模拟。官方 SDK 仅用于独立样例生产和真值读取。

## 证据与字段语义

已验证 SDK 为 PhysX 3.4.2，官方固定提交 `5e42a5f112351a223c19c17bb331e6c55037b8eb`。SDK 的粒子 API 自 3.4 标记 deprecated，但固定 Checked CPU 库仍实际提供并运行；这不意味着 PhysX 4.1 提供相同粒子 API。

关键源码：

- `PhysX_3.4/Source/PhysX/src/PvdMetaDataPvdBinding.cpp` 注册 ParticleBase/System/Fluid 继承及属性，并把 actor 加入 Scene 的 ParticleSystems/ParticleFluids 引用列表。
- `Source/PhysXMetaData/core/include/PvdMetaDataExtensions.h` 声明 Positions、Velocities、RestOffsets、CollisionNormals、Flags 和 Fluid 的 Densities 缓冲属性。
- `Source/PhysX/src/buffering/ScbScenePvdClient.cpp::sendStateDatas/sendArray` 给出每帧真实发流方式：按有效位图从低到高枚举 index，将每个缓冲压缩成连续数组。Positions 第 3 项表示第 3 个有效 index，不表示 index 2。
- `Source/LowLevelParticles/src/PtParticleSystemSimCpu.cpp::getSimParticleDataV` 说明碰撞法线、密度缓冲受读取开关控制。
- `Include/particles/PxParticleReadData.h`、`PxParticleFlag.h`、`PxParticleBaseFlag.h`、`PxParticleFluidReadData.h` 定义 API 稀疏缓冲、标志及密度语义。

| 捕获字段 | 使用方式 |
| --- | --- |
| NbParticles / ValidParticleRange / MaxParticles | 核验有效数量、最大有效 index 加 1 与容量 |
| ValidParticleBitmap | SDK 3.4 发流长度为 `(ValidParticleRange >> 5) + 1` 个 u32；范围为 32 的倍数时也含额外字；检查最高有效位、popcount 和越界非零位 |
| Positions / Velocities | 捕获的世界坐标位置与速度，压缩数组按位图 set bits 对应；不附加 actor 变换 |
| RestOffset / RestOffsets | 全局或逐粒子的碰撞静止间距。不是捕获的球形视觉半径；显示为点或自行选择标记大小时应标明是显示设置 |
| CollisionNormals | SDK 碰撞法线，可选；未启用对应读取缓冲时返回 None |
| Flags | 原始 eVALID、与静态/动态/排水形状碰撞、空间结构溢出位；不推断额外状态 |
| Densities | Fluid 的归一化密度，可选；无邻居可为 0，静止密度对应约 1，不解释为绝对质量密度 |
| RestParticleDistance | Fluid 的静止粒子间距，不代替球半径 |
| Scene | 通过现有引用代际解析，拒绝已销毁或 ID 复用后的陈旧 Scene 引用 |

SDK 在系统清空时只更新计数和范围，可能保留旧数组属性；`NbParticles=0` 且 `ValidParticleRange=0` 时返回空粒子列表，不显示残留数组。稀疏 index 可以删除后复用；公开结果同时保留 actor ID、actor generation 和粒子 index。捕获没有每粒子生命周期代数或剩余寿命字段，不能据此构造假的粒子 generation。若同一个 index 在相邻记录之间被删除并重建，而中间未捕获为空，就不能仅靠两个快照断定其生命周期边界。

## 接口与缺失数据

`Collection` 包含 `systems: Vec<ParticleSystem>`、`diagnostics: Vec<Diagnostic { actor, message }>`。系统包含 actor ID/代际、Scene、Kind::System/Fluid、容量/范围、SDK 标志、间距和粒子列表。每个 Particle 包含原 index、position、可选 velocity/rest_offset/collision_normal/density/flags。`enabled()` 对应 SDK eENABLED。

位置、有效位图、计数、系统参数或引用损坏会跳过整个相关系统并给出诊断，其他系统仍可读取。可选缓冲缺失时返回 None 与具体字段诊断，不能补零伪装原始数据。未启用或不适用的 rest offset、collision normal、density 缓冲不使用遗留属性。

禁用系统时 SDK 停止每帧发送状态，结果明确诊断为最后捕获状态。GPU 粒子路径目前拒绝，因为 device-exclusive GPU 访问会让 SDK 跳过 PVD 状态输出，且缺少独立真值。CollisionVelocity 并不在 SDK 3.4 PVD 粒子属性列表中，当前不提供。

默认单次收集最多 1,000,000 个有效粒子、4,096 个系统；`collect_with_limits` 可以进一步收紧这些上限。位图、数组数量、容量、有限数值、逐粒子偏移范围和标志都先核验。畸形系统不会消耗其他有效系统的粒子预算。显示端仍需设置自己的绘制数量和选择预算。

## 独立真实验证

`tools/physx-particles/main.cpp` 链接固定 SDK Checked 库，创建一个 CPU ParticleSystem 和一个 CPU ParticleFluid，容量各 128，启用逐粒子 RestOffset，真实运行 10 帧。独立真值来自 `lockParticleReadData` / `lockParticleFluidReadData` 返回的 SDK 稀疏缓冲；生产器不重写发送端的压缩数组算法。SDK 对象及锁在输出异常时也通过 RAII 释放。

样例包含：

- 跨位图字边界的 index：0、7、31、32、63、95 与 Fluid 的 1、8、30、33、64、96。
- 删除部分 index，后续复用 index 7/8；清空整个系统，再创建 index 31/32/63；最高有效 index 改变。
- 真实静态地面碰撞、逐粒子不同偏移和非零流体密度。
- 独立读取每帧 SDK 世界位置、速度、偏移、flags、collision normals 和 density，再按非顺序 `[9,0,6,1,4,8,2,5,7,3]` 对比捕获。浮点容差为 `1e-6 * max(1, abs(expected))`，数量、ID、index、flags 完全相等。

粒子专项覆盖结构/边界与真实 SDK 真值，比较顺序、逆序、随机、裁剪重开、失效 Scene 代际和清空后的数组状态。

```powershell
.\tools\physx-particles\build.ps1
.\.deps\physx-particles-build\Release\pvd-particles.exe captures/physx-3.4-particles.pvdstream captures/physx-3.4-particles.particles.json
cargo test --locked -p pvd-core --test particles -- --include-ignored --nocapture
cargo clippy --locked -p pvd-core --all-targets -- -D warnings
```

默认输入为 `captures/physx-3.4-particles.pvdstream`，真值为同目录 `.particles.json`，也可通过 `PVD_PARTICLE_CAPTURE` 指定绝对路径。真实 SDK 测试默认 ignored，需先生成配对输入。

该最终记录为 251,686 字节，SHA-256 `13e5de81c4f08d5087fe4d7c91de9bcbf7b669f3cc80b0640ca3bedfa82230f2`。进程地址、性能计时会使重新生成的文件哈希不同，验收以对应 JSON 的实际 SDK 数据为准。

本验证证明 PhysX 3.4.2 CPU 粒子数据解析与回放。它不证明 GPU 粒子、其他 SDK、原版 PVD3 的点大小/颜色/交互，也不证明原版 `.pxd/.pxd2` 保存容器兼容。粒子 RepX 导出和粒子专用曲线/表格尚未纳入本模块。
