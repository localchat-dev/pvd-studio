# 交错多 Scene 的 SDK 验证

[双 Scene 生产器](../tools/physx-multiscene/README.md)使用固定 PhysX 3.4.2 checked SDK、真实文件传输和 ALL 标志。两个 Scene 共用四线程 dispatcher，分别以 1/60 和 1/30 秒模拟，交替 fetch 次序，输出 144 个合并帧边界，A/B 分别为 96/48 个。

SDK 在动态位姿更新前发送 EndSection。独立 JSON 记录每个边界之前已发布的两 Scene 位姿、fetch 前后 QPC 范围和转发到官方 profiler callback 的阶段边界。不读取尚未 fetch 的 Actor；同一合并快照不代表同一物理时刻。

## 核对范围

- 帧边界的 Scene、局部步号、SDK 位姿、名称和引用。
- 完整载入、碎片输入、分段磁盘历史与正倒游标的一致性。
- 同 Scene 的帧间隔和阶段曲线；其他 Scene 点保留 `OtherScene` 与空值。
- 完整 simulate/collision/solver 区间与独立 QPC 范围。
- SDK 的额外未闭合 Solver 起点，保留嵌套语义，不能伪造结束时间。
- 游标和磁盘历史释放后的文件清理。

两个 simulate 区间有时间重叠，不证明 CPU 核心同时执行，也不构成性能结果。生成和显式 ignored 测试的命令见生产器说明；使用其他输入时可设置 `PVD_MULTISCENE_CAPTURE`。

原版多 Scene 容器、跨 Scene debug draw、Scene 销毁/复用、Actor 转移、跨机器时钟和独立物理时间轴重采样仍需验证。回放按合并流顺序推进，绘制在全局帧边界重置，保守的多 Scene 诊断保持。
