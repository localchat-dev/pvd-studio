# 运动学刚体的 RepX 参数快照

此工具用固定 PhysX 3.4.2 SDK 核对 PVD 捕获、项目 RepX 参数快照和官方导入结果。运动学 Actor 保留捕获位姿、质心、质量/惯量、flags、Shape 与明确可恢复的参数；不恢复待执行目标、运动学速度、唤醒计时、睡眠或 solver 内部状态。项目 XML 可保留捕获的速度和 WakeCounter 数值，官方 importer 对运动学刚体不应用这些字段，导入 getter 必须为零。

同次运行记录八帧：三次目标、切回普通动态刚体、query-target flag 保留和清零、再次启用运动学，以及不再设置目标的一帧边界审计。保存每帧 simulate 前后的独立 SDK getter、官方 XML、传输字节边界和 PVD 流。独立 flags 探针检查 CCD/speculative CCD、运动学/CCD 的 SDK 过滤，以及运动学 speculative CCD 的合法组合。

## 运行

按 [SDK 生产器说明](../physx-sample/README.md)准备固定 SDK commit `5e42a5f112351a223c19c17bb331e6c55037b8eb`，在仓库根目录执行：

```powershell
.\tools\repx-kinematics\build.ps1
cargo build --locked -p pvd-cli
python tools/repx-kinematics/verify.py --output output/repx-kinematics-check
$env:PVD_REPX_KINEMATIC_DIRECTORY = (Resolve-Path 'output/repx-kinematics-check').Path
cargo test --locked -p pvd-core --test repx_kinematic -- --ignored --nocapture
```

输出目录必须尚不存在；省略 `--output` 时在 `output/repx-kinematic-validation` 下创建时间戳目录。可用 `--cli`、`--producer` 指定程序位置。JSON getter schema 为 2，验收要求八帧及全部 pre/poststep 样本。工具不启动窗口，不修改 SDK。

## 比较规则

- 官方 `PxRigidDynamicRepXSerializer` 通过 metadata 读取参数，没有可恢复的 KinematicTarget。三个 pending 点的 getter 必须报告有效目标；直接官方 XML 和注入同名目标的 XML 均交 importer 重读全部既有 getter，导入目标必须无效、无诊断。
- `RepXMetaDataPropertyVisitor.h` 的 WakeCounter 专用 accessor 跳过运动学对象，`SnXmlVisitorReader.h` 的刚体读取器跳过运动学线/角速度。验收对这三项独立断言精确零；对其他 getter 逐项使用既有容差，不扩大容差或只比较部分姿态。
- 八帧的官方参考、项目整帧及选中 XML 都交官方 importer。项目 XML 的三项运动数值还须等于同帧捕获对应的 SDK 样本。非运动学阶段比较全部 getter。CLI 计数与仅对运动学对象出现的限制提示一起检查。
- PVD 的 KinematicTarget 使用 Body 坐标，没有有效标志。目标不存在或与当前位置相同都不能用来判断状态；捕获的 Body 目标不能当作 Actor GlobalPose。
- 检查类型、有限值、引用代际、flags、几何和资源预算。运动学与 CCD 不能同时开启；CCD 与 speculative CCD 也不能同时开启。此工具只创建 Box。

## 帧边界

每帧的 `frame-N-prestep-*` 和 `frame-N-poststep-*` 都直接来自 SDK getter 与官方 serializer。`frame-N-boundary.json` 保存 step 前后已写传输字节数。`frame-N-truth.json` / `frame-N-reference.xml` 是对应捕获帧的样本：普通阶段为 step 前；Dynamic→Kinematic 阶段 6 要求为 step 后。

ignored 测试独立用原始 Decoder，在每个 Scene frame 的 EndSection 记录 GlobalPose、线/角速度及写入序号，并与项目 state_at 和 SDK 样本比较。阶段 6 必须证明三项写入发生在其 step 前传输边界之后、EndSection 之前。后面再执行真实审计帧 7，避免把阶段 6 的采样结果归因于 EOF。该检查不为 SceneState 添加帧来源特权，也不推断目标有效性。

动态阶段 4–5 的 WakeCounter 在 simulate 前分别设置 0.9 / 0.7，捕获、XML 和导入值与该时点比较；step 后 getter 衰减 1/60。姿态、速度的前后差异和第 4 步后→第 5 步前的连续关系单独断言。PVD 不逐步发布 WakeCounter，不能用当前 getter 代替捕获值。

此工具验证固定 SDK 的参数快照与明确的状态损失，不表示完整运行状态恢复，也不是 PVD3 运动学 RepX XML 的运行时对照或其他 SDK 兼容声明。
