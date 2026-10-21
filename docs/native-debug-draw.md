# PVD3 数据库 v1 的 SDK 调试绘制

## 支持范围与证据

原版保存数据库的 `_debugger_::PvdUserRenderer.events` 接入既有 `SceneState.debug_draw`。只使用捕获的绘制数据，不重新运行物理模拟。既有桌面绘制消费接口无需改变。

下表标识用于格式核对的参考配对输入，文件不随仓库分发。采集方法和测试目录见[原版验证](native-validation.md)。

| 文件 | SHA-256 |
|---|---|
| `rich-paired-native.pxd2` | `7daa115e9da7ee417340775c853017d1c38b1d72e6b416d51318c07855891f48` |
| `rich-paired.pvdstream` | `6f8b90f9fc788f1b7d53551b300610002af28072f28116c642fe794da0860bae` |
| `baseline-paired-native.pxd2` | `d391fb28d7f8a9f319a2885e82b38e049eda2df50c0b1e0261bb286b19ecb67a` |
| `baseline-paired.pvdstream` | `16c8951055d47b534334353148e88774b0b95322d6545e24f45e1240c037d552` |

这些文件来自 SDK 3.4.2 同次 TCP 输出，经本地逐字节代理送入已安装的 PVD3，再由原版保存数据库。它们不是项目自行生成的原版容器。输入来源与保留记录见 [原版文件格式](original-file-format.md)。

已观察到的 rich 容器元数据：

- renderer class 107，实例 5；属性 52，名称 `events`，类型 `physx3::PvdU8` 数组，semantic `render event stream`。
- 120 个完整属性更新，每次 66 字节。每次内容是事件 1（`SetInstanceId`，8 字节整数）和事件 5（`JointFrames`，两个各 28 字节的姿态）。
- 关联对象为容器实例 20。实例 ID 的 SDK 源指针由 communication metadata 的映射读取，不根据数值猜测。
- 第一次更新是 history event 145，收到时已有 0 个完成帧；末次更新是 event 3370，收到时已有 119 个完成帧。
- 同次 raw 的 120 个 renderer 更新也是每帧一个、每个 66 字节。除 `SetInstanceId` 的 8 字节由源指针替换为容器 ID，余下姿态字节逐一相同。

该 rich 样例只有关节坐标系事件。下述独立样例补充 RGB 三角形与四类关节限位；点、线、文本和非空 bulk render 仍没有原版保存文件的配对验收。

## 六帧限位与 RGB 三角形配对

六帧限位专项使用同次 SocketTransport 流、原版 Save 结果和 SDK 几何真值：

| 文件 | 大小 | SHA-256 |
|---|---:|---|
| `joint-limits-live-native.pxd2` | 331,119 B | `132674bfaa4d95a9b32247476ffcb94ee87f070bae98881fdccb2705123fa0c3` |
| `joint-limits-live.pvdstream` | 270,152 B | `1a00cf92f9a085b6954c42d5cfc1f567a36ed3bb815f04f9ff3866e2e931a64e` |
| `joint-limits-live.joints.json` | 382,585 B | `123b2499704220c7dc1d0d596d0d344294866de328f0acac7cd2cef0d2107cf0` |

生产器 `pvd-joint-limits --host … --port … --interval-ms … truth.json` 通过官方 `PxDefaultPvdSocketTransportCreate` 发流；本地代理逐字节留存，再由原版保存。它不是把 SDK 文件流改扩展名或自行构造数据库。此前对旧文件流的 TCP 复送在原版只显示 1 帧、0 objects；文件与成功 TCP 捕获的 28 字节头完全一致，原因仍未确定，不能据此声称 FileTransport 文件可由原版直接打开。

真实六帧各含 19 个图元：1 个 RGB 三角形、8 个 JointFrames、4 个 LinearLimit、2 个 AngularLimit、2 个 LimitCone、2 个 DoubleCone。三角形位置不对称，顶点颜色依次为 `0xffff0000`、`0xff00ff00`、`0xff0000ff`。同次 SDK 真值包含八组官方 `Cm::visualize*` 帮助函数结果，以及每帧 `PxScene::getRenderBuffer()` 的 564 条线段／三角形边线。

自动化验证先用共享测试辅助函数核对 raw 与独立 SDK 真值，再按 communication metadata 将容器 renderer 和图元 instance 映射至源 ID（0 保持 0）。全部图元顺序、位置、四元数、标量限位、active 状态、顶点颜色逐位相等；前进、逆向和固定随机顺序共 18 次恢复均通过。限位细分输出与 SDK 几何比较保留 `1e-5` 端点误差和精确颜色，重复线段逐一计数。

raw 每帧有两次 renderer flush：首次含 RGB triangle 和一个三个数组均为空的 event 11，末次包含关节坐标系与限位。完成帧的绘制状态正确累积两次 flush；`events` 属性只保留末次缓冲区。测试另逐字节比较该末次缓冲区，允许的唯一变换是 `SetInstanceId` 源指针与容器本地 ID 的映射。空 event 11 不能证明非空 bulk 的点／线／三角形兼容。

原版曾成功重开六帧文件。该样例覆盖四类限位和 RGB 三角形，不能推广为所有图元的逐像素对照；窗口验收要求见[原版验证](native-validation.md)。

## 静态格式依据

固定 SDK 源码 `.deps/physx-3.4/PxShared/src/pvd/src/PxPvdUserRenderImpl.h`、`PxPvdUserRenderer.cpp` 和 `PxPvdUserRenderTypes.h` 定义事件码 1–11、各字段顺序、点／线／三角形的 16／32／48 字节元素，以及保留当前 instance 的逐次 flush 行为。

对[匹配身份](evidence.md)的 EXE/MAP 静态检查得到以下转发关系；它们与原版保存/重开具有不同证据范围：

| 函数 | PE 虚拟地址 | 观察 |
|---|---|---|
| `SetInstanceId` | `0x1400B79D0` | 非零源 ID 经对象映射得到有符号 32 位本地 ID，再传给 renderer；无法映射或空值传 0。 |
| `Points` | `0x1400B79B0` | 原始元素地址与指针差除 16 后的数量转发。 |
| `Lines` | `0x1400B7990` | 原始元素地址与指针差除 32 后的数量转发。 |
| `Triangles` | `0x1400B7A30` | 原始元素地址与指针差除 48 后的数量转发。 |
| `DebugRender` | `0x1400B7880` | 三组原始地址／数量转发。 |
| `JointFrames` | `0x1400B7920` | 两个相隔 28 字节的姿态转发。 |
| `Text` | `0x1400B7A20` | 文本结构转发。 |
| `LinearLimit / AngularLimit / LimitCone / DoubleCone` | `0x1400B7960 / 0x1400B7850 / 0x1400B7930 / 0x1400B7900` | 姿态、标量和 active 标志转发。 |

这些静态结果支持复用 SDK renderer 格式解析器；不是对未出现事件的实际原版保存／打开验证。EXE、MAP、PDB 的身份与哈希沿用原版格式文档，安装目录保持只读。

## 生命周期与边界

每个 renderer 维护独立的当前 instance。一次 buffer 中没有 `SetInstanceId` 时沿用该 renderer 的值；失败或取消不会提交部分 decoder 状态。保存 payload 中已经是本地 ID，导入不会再按 SDK 源指针重映射。

同一捕获帧内的多个 flush 累积显示。`EndSection(frame)` 先保存该完成帧，再清除绘制批次；没有新绘制的后续帧保持空白。空 buffer 本身不清除同帧先前的绘制。renderer 销毁会清除批次和 decoder；重建同 ID 的新代际从 instance 0 开始。这些边界由合成历史测试覆盖；真实 rich 样例证明每帧一次 JointFrames 更新，六帧限位样例还证明同帧两次 flush 的累积。绘制帧之后切换为空帧、renderer 销毁／重建的真实原版配对仍待验证。

帧状态仍由已有历史 action／检查点恢复。正向、逆向与随机跳转不会对绘制做逆向几何运算。多个异步 Scene 仍使用现有合并事件顺序时间轴；独立场景的 debug draw 对齐尚未验收。帧尾之后的未成帧数据不能当成最后一个已完成帧的绘制。

严格识别的 renderer 属性必须是 `PvdU8` 数组且元素大小为 1。属性数组插入／移除／修改操作明确拒绝；完整 buffer 按 renderer 事件格式解析。未知事件码没有独立事件长度可供跳过，故停止导入；不会用剩余字节猜边界。原始属性 payload 同时保留。

## 资源和取消

- 解码前进行无分配格式扫描，检查完整缓冲区总图元数量不超过 1,000,000；组合数组不能分别满足上限后绕过总限制。
- native 预扫同时检查图元和文本的保留内存估算不超过 64 MiB 或剩余 history 预算。图元 Vec 一次按精确数量预留；文本考虑非法 UTF-8 最坏三倍替换，转换后压紧容量以符合历史记账。
- 单条文本最多 1 MiB；数组长度、截断、无 NUL 文本、无效布尔值和未知事件均拒绝。
- 扫描与构造阶段按输入推进约每 4 KiB 检查取消，返回 `Interrupted`；外层 history 上下文保留该错误类型。
- history action 继续使用既有总预算并计入批次复制；原始文件、metadata 与历史索引的限制不变。
- SDK raw 路径复用预扫以加强格式与总图元限制，仍保留既有最多一百万图元边界；它不采用 native 的额外单 buffer 64 MiB 预算，既有 history 总预算在构造 action 时检查。raw 与 native 的限制不能混称相同。

## 重复验证

```powershell
cargo test --locked -p pvd-core --lib
cargo test --locked -p pvd-core --test native --test native_debug_draw -- --include-ignored
cargo clippy --locked -p pvd-core --all-targets -- -D warnings
```

真实新测试核对 120 个 raw buffer 的顺序、长度、码、实例映射与全部姿态字节；按前进、后退和随机顺序共 360 次恢复 native 与 raw 帧状态，验证每帧只有一条 JointFrames、不会累积上一帧。baseline 的两个真实输入均逐帧保持空绘制。真实容器还验证 buffer 内取消。

六帧限位样例的专项命令与实际结果：

```powershell
cargo test --locked -p pvd-core --test native_debug_draw --test joint_limits -- --include-ignored --nocapture
cargo clippy --locked -p pvd-core --test native_debug_draw --test joint_limits -- -D warnings
```

`tests/support/joint_limits.rs` 同时服务于 SDK 和 native 测试，读取同次 `.joints.json`，不能用两个导入器的相同结果替代独立真值。原版专项默认 ignored，需显式执行。

合成测试核对同帧累积、空 buffer、空帧、跨 flush instance、renderer 销毁和代际、点／线／三角形颜色、所有既有事件长度、限位 bool、文本、非法 UTF-8、总数量、低预算、截断、未知事件、错误 metadata 与不支持编辑。合成测试不能独立证明原版兼容。实际 GPU 与原生交互范围以 [验证说明](validation.md) 和 [原生交互](native-validation.md) 为准。

## 下一份真实样例

需要由 SDK producer 显式发送不对称、多颜色的点、线、文本与非空 bulk render，并包含绘制之后没有新绘制的完成帧、未重复 SetInstanceId 的后续 flush、instance 0、renderer 销毁／重建。原版保存文件必须与同次逐字节 TCP 配对，SDK 端记录调用顺序和参数。该样例用于补齐剩余事件及特殊清除生命周期的真实原版证据；不需要重新模拟回放，也不应拿合成容器替代。
