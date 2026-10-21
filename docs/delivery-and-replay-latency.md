# 交付队列唤醒与历史读取取消

## 有界交付队列

`pvd-transport::Output::send` 在交付队列满时等待条件变量，消费者取走事件后通知发送者。发送重试、消费通知和取消使用同一把锁，避免通知先于对应等待。消费者退出也会唤醒发送者，回调在释放交付锁后执行。容量可配置为 1–1024 项，桌面默认 32 项，每块数据最多 64 KiB。

普通监听与磁盘接收使用同一条交付路径。`CaptureControl::request_stop` 设置取消状态、唤醒交付等待并关闭当前 socket。磁盘提交/结束的 `SessionProgress` 条件变量保持独立。accept、socket 读取和桌面状态通知仍有各自的等待，不能把单条路径的唤醒机制外推为整个系统的时延保证。

## 历史读取取消

`Recording::state_at_with_cancel` 和 `analysis_at_with_cancel` 接受取消谓词，以 `io::ErrorKind::Interrupted` 返回。桌面 worker 的请求各自持有取消标记，换目标、返回实时或销毁 worker 时设置。generation 检查阻止旧结果覆盖新请求。

磁盘读取在记录开始、每 64 KiB 读取、反序列化前后、对象/共享资源恢复和分析输入之间检查取消。中断后读取位置失效，下次读取重新定位到明确偏移；长度和校验和检查不省略。内存回放在动作之间检查。

这是协作式取消，不能抢占正在执行的系统读取、单条记录反序列化或单个分析缓冲处理。连续回放使用[回放游标](tcp-playback.md)，首次和随机冷定位仍有基线恢复成本。

## 验证

```powershell
cargo test --locked -p pvd-transport
cargo test --release --locked -p pvd-transport saturated_delivery_wakeup_latency -- --ignored --nocapture
```

时延专项使用容量 1 的饱和交接，起点为消费者取走已排队事件之前，终点为发送线程确认下一事件入队之后，包含调度和确认通道成本。它不测 SDK 吞吐或桌面帧率。

混合负载和持续 TCP 检查见[生产器](../tools/physx-sample/README.md#大量常驻分配与小分析缓冲)与[诊断工具](../tools/README-live-probe.md)。队列采样和源端统计未做时钟对齐，不能据此计算逐帧端到端延迟。
