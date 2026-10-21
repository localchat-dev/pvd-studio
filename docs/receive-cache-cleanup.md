# TCP 临时接收目录的异常退出回收

创建接收缓冲时检查同一根目录中已无进程使用的登记会话并回收。默认根目录为当前工作目录的 `pvd-cache/history/receive`，随历史目录配置变化。

每个 `spool-<时间>-<进程>-<序号>` 目录持有独占文件租约与版本化归属标记。队列、读取器和写入器共享目录所有者，断开 TCP 不会提前释放仍在读取的缓冲。每个原始分片另有小型 `.pvdspool` 登记，绑定确切目录与文件名；登记写入内核并关闭后才开始写入分片数据。正常消费时两者一起删除，没有随整个连接不断增长的登记日志。

回收采用非阻塞租约获取，先验证完整直属目录再删除登记文件。活动租约、未标记旧目录、未登记分片、未知文件、错误登记、符号链接、Windows reparse point 及嵌套目录都会使相应目录保留。永久 `.pvdstream` 录制目录不参与此操作。没有递归删除，也不根据 PID 是否存在猜测所有权。

每次扫描最多检查 256 个根目录条目，每个会话接受最多 8,320 个非 owner 条目；超出时保留。上限不保证一次清理所有历史目录。接收配额、`retained_disk_bytes` 和回收报告的 `removed_bytes` 统计 raw 分片逻辑字节，不包含归属标记或文件系统分配开销。

登记不逐片强制刷盘，避免小分片阻塞接收。该路径保证的场景是进程异常退出；系统掉电后的缺失或破损登记会被保守保留，不能据此宣称断电持久化或所有旧缓存都可自动清理。历史数据库目录已有独立租约和回收逻辑，本功能处理 TCP 接收缓冲。

## 验证与重复命令

回归覆盖子进程异常退出后的自动回收、正常停止、活跃读取者、未知文件、旧目录和 Windows junction。真实 SDK 的消费者停滞/排空检查只验证传输与释放，不能替代场景/分析正确性。

在项目根目录执行回归；真实 SDK 项需先构建生产器：

```powershell
cargo test --locked -p pvd-transport
cargo test --locked -p pvd-transport unlimited_spool_retains_more_than_256_mib_and_drains_exactly -- --ignored --nocapture
$env:PVD_SDK34_PRODUCER=(Resolve-Path '.deps/physx-sample-build/Release/pvd-physx-sample.exe').Path
cargo test --locked -p pvd-transport official_sdk_finishes_while_the_decoder_consumer_is_stalled -- --ignored --nocapture
cargo clippy --locked -p pvd-transport --all-targets -- -D warnings
```
