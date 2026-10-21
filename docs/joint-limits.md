# PhysX 3.4.2 关节限位几何

`pvd_core::joint_geometry::tessellate(&debug_draw::Primitive)` 将捕获中的 LinearLimit、AngularLimit、LimitCone、DoubleCone 转成世界坐标线段。输出保留 joint instance ID 和 SDK 的 active 红色 / inactive 灰色。显示长度基准为 1 个捕获世界单位。线性限位箭头的三角面采用边线显示；零长度箭头省略不可见的退化三角形。

## 证据与语义

参考 SDK 为官方 PhysX 3.4.2，固定提交 `5e42a5f112351a223c19c17bb331e6c55037b8eb`：

- `PhysX_3.4/Source/Common/src/CmVisualization.cpp`：四类限位的官方几何帮助函数。LinearLimit 绘制父坐标系 X 方向的箭头与限位圆；AngularLimit 绘制两条半径和弧线；LimitCone 使用 tangent-quarter swing 参数；DoubleCone 使用 `tan(angle)` 的高度。
- `CmRenderOutput.cpp`：DebugArrow、DebugCircle 的线段和三角形输出。
- `Source/PhysX/src/buffering/ScbScenePvdClient.cpp` 的 `PvdConstraintVisualizer`：SDK 将真实 constraint visualize 回调参数原样写入 PVD renderer。
- `Source/PhysXExtensions/src/ExtPrismaticJoint.cpp`、`ExtRevoluteJoint.cpp`、`ExtSphericalJoint.cpp`、`ExtD6Joint.cpp`：真实关节产生对应 renderer primitive 的条件。D6 的一个 swing limited、另一个 swing free 会产生 DoubleCone。

尤其不能把 LimitCone 中的两个参数当成弧度。SDK 传输的参数是半角再次对半后取正切；Rust 按已传输的值生成锥面轮廓，不重新计算关节约束、碰撞或活动状态。

这证明 SDK 3.4.2 调试数据的几何一致性；原版 PVD3 的显示比例、抗锯齿、遮挡和屏幕交互尚未进行像素对照。

## 独立真值验证

生产器 `tools/physx-features/joint_limits.cpp` 直接调用链接的 SDK `Cm::visualize*` 生成八组几何参考，含非原点位置、旋转、正负线性限位、活动和非活动颜色。SDK helper 的输出来自 SDK 动态库，不在生产器中重写公式。

同一生产器创建 Prismatic、Revolute、Spherical、D6 各两个真实关节，启用 limits 和 visualization，使用 SDK PVD 文件传输捕获六个模拟帧，同时读取每帧 `PxScene::getRenderBuffer()` 作为独立几何真值。该模拟只负责测试数据生产；工具接收端仅显示已捕获数据。

真实测试按反向帧序读取捕获、对所有四类限位细分，再与 SDK render buffer 逐条匹配端点和颜色；端点允许 `1e-5` 浮点误差，线段方向和顺序不影响比较，重复线段仍逐一计数。还校验八个关节 ID、四类 primitive 每帧均出现，以及顺序和随机回放一致。

SDK 专项使用八组 helper 参考、八个真实关节和六帧渲染缓冲，逐条比较端点及颜色，覆盖 LinearLimit、AngularLimit、LimitCone 和 DoubleCone。

生产器另外通过官方 `PxPvdSceneClient::drawTriangles` 每帧发送一个具有红、绿、蓝顶点颜色的原始调试三角形；真值保存调用参数，六帧的三个位置与三种颜色逐一匹配。这独立覆盖 Triangle 事件的实际 SDK 发流，不改变限位几何真值。

录制 `captures/physx-3.4-joint-limits.pvdstream` 大小 270,136 字节，SHA-256 `b6e38ea1187211479042f219cc223f3c942ebf4db2ea06cb91e7996a6bc130cd`。进程地址、计时及性能事件影响重新生成的文件哈希，验收依赖语义对照。

```powershell
.\tools\physx-features\build.ps1 -Target pvd-joint-limits -BuildRoot .deps/physx-joint-limits-build
.\.deps\physx-joint-limits-build\Release\pvd-joint-limits.exe captures/physx-3.4-joint-limits.pvdstream captures/physx-3.4-joint-limits.joints.json
cargo test --locked -p pvd-core --test joint_limits -- --include-ignored --nocapture
```

可用 `PVD_JOINT_LIMIT_CAPTURE` 指向另一录制，真值必须同目录同文件主名、扩展名 `.joints.json`。没有真实输入时默认测试仅运行两项边界测试，SDK 测试明确为 ignored。以上命令实际执行三项全部通过；`cargo clippy --locked -p pvd-core --all-targets -- -D warnings` 通过。

原版专项需同次 SocketTransport 流、六帧保存文件和 `.joints.json`，按[原版验证](native-validation.md)准备。先核对 SDK 真值，再检查限位、坐标系、三角形、实例映射和回放，见[原版绘制](native-debug-draw.md)。

生产器每帧及报告完成前在 transport 锁内 flush 后检查 `isConnected(false)`。已知断线返回非零；成功不确认接收端已解析、索引或保存，部分真值不能当作完整捕获。

## 边界与限制

- 单 primitive 最多 128 条线段；执行次数由固定分段数控制，与捕获中的长度无关。显示端还需应用跨 primitive 的总量限制。
- 检查有限坐标、单位四元数、有限限位和合法角度范围；任何非有限输出整体返回错误。
- AngularLimit 要求有序且位于 `[-2π, 2π]`；LimitCone 参数在 `[0, 1]`；DoubleCone 在 `[0, π)` 并拒绝 `cos(angle)` 绝对值小于 `1e-6` 的奇点附近输入。被拒绝的数据应显示诊断。
- PhysX 4.1 及 articulation 限位尚未用本测试独立验证。不能从源码相似或 primitive 名称直接声称支持已验证。
- 当前提供轮廓几何，不提供实心锥体或箭头面材质。

## SDK 衍生几何的许可通知

以下通知适用于依据 `CmVisualization.cpp`、`CmRenderOutput.cpp` 和 `PsMathUtils.h` 实现的几何计算；分发源代码或包含这些计算的二进制时保留此通知。

Copyright (c) 2008-2018 NVIDIA Corporation. All rights reserved.
Copyright (c) 2004-2008 AGEIA Technologies, Inc. All rights reserved.
Copyright (c) 2001-2004 NovodeX AG. All rights reserved.

Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:

- Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.
- Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the documentation and/or other materials provided with the distribution.
- Neither the name of NVIDIA CORPORATION nor the names of its contributors may be used to endorse or promote products derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS ``AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
