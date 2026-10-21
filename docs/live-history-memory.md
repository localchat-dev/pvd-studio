# 实时状态与历史内存

当前对象状态、回放历史和原始录制分开管理。内存预算用于限制核心持有的数据，不等同于进程 RSS；提高预算不能替代持续负载下的吞吐验证。连续接收策略见[持续实时](continuous-live.md)，磁盘历史见[共享历史](shared-history.md)。

## 共享和去重

- 动作日志使用共享块，已发布快照共享完成块，增长中的尾块写入时复制。
- 检查点共享未变化对象与引用绑定，缓存受历史预算限制。
- 只有属性值和引用代际均未变化时才省略重复更新；保留浮点位模式、对象 ID 复用和重新绑定语义。
- 显示快照允许合并中间更新，协议事件和保留历史不能静默丢弃。

解码失败后保留最后完成帧，界面显示“仅接收原始数据 · 解码已停止”，释放失败的解码器。原始接收与可选永久录制继续受各自的容量及磁盘约束；重连建立独立会话。

## 原版对照

匹配[证据登记](evidence.md)所列 EXE/MAP 的静态调用链中，`PvdListener::initializePvdServer` 创建随机访问读写文件，并将其传入 `PvdClientMutator::create` 和对象历史工厂。原版 TCP 历史使用磁盘文件，不能将其理解为仅在点击 Save 时才写盘。

此调用链不能证明原版页缓存容量、磁盘总量、淘汰策略或磁盘满行为。文件页表、快照和实例目录见[原版格式](original-file-format.md)，不能据此推断任意负载下无限录制或固定内存占用。

## 验证

先按 [CCT 说明](cct-live-history-fixture.md)生成真实 SDK 捕获及独立真值，再执行：

```powershell
$env:PVD_CCT_CAPTURE = (Resolve-Path 'captures/physx-3.4-cct-large.pvdstream').Path
cargo test --locked -p pvd-core --test sdk_cct -- --ignored
$env:PVD_LARGE_CAPTURE = $env:PVD_CCT_CAPTURE
cargo test --locked -p pvd-desktop network::tests::real_large_capture_keeps_live_snapshots_and_raw_capture_complete -- --ignored --nocapture
Remove-Item Env:PVD_CCT_CAPTURE, Env:PVD_LARGE_CAPTURE
```

核心回归覆盖共享快照不可变性、淘汰后的顺序/随机一致性、重复值、NaN/负零、引用代际和预算失败原子性。网络测试覆盖接收、解码、快照与原始保存，不创建原生窗口；跨机器和长期资源验收仍按[路线图](roadmap.md)执行。
