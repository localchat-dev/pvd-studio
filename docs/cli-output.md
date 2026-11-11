# CLI 导出文件的发布与保护

## 错误事件的标准输出

`pvd-cli errors <capture>` 读取 SDK 流或原版 PVD3 保存容器，打印错误数量和每条错误的 `code`、`message`、`file`、`line`。原版容器的 `inspect` 也打印相同的错误记录。字符串使用带引号的转义表示，捕获中的换行和终端控制字符不会成为输出控制指令。

- SDK 错误保留所属事件组的 `timestamp_ticks`，并打印时间戳比率及 `sdk_timestamp_source: event-group`。这不是从帧时间推算出的错误发生时间。
- 原版容器的错误没有已验证的时间戳字段，输出 `timestamp_ticks: unknown`，保留容器里的 `index`、`history_event` 和 `received_frame`。
- `received_frame` 明确表示接收时已完成的场景帧数，即 `completed_frame_count`。它等于总帧数时，位置为 `after-final`；不会截断成最后一帧的索引。没有完成帧时输出 `no-completed-frames`。
- 两种来源都不包含已验证的受影响 Actor 标识，输出 `affected_actor_id: unavailable`。错误容器实例不被当作 Actor。

`errors` prints source error fields to stdout. SDK timestamps are event-group ticks; native database errors retain their saved index and receive order with an unknown timestamp. Receive positions count completed scene frames, including errors received after the final frame. No affected Actor ID is inferred.

`verify` 和 `frame` 分别报告录制层的 `recording_timed_errors`、`recording_untimed_errors` 总数。帧状态里的 `sdk_timed_errors_in_state` 只表示累积至该帧快照的 SDK 有时间戳错误数，不代表该帧发生的错误次数，也不包含录制层未定时错误。未定时错误存在时，输出明确提示它们按接收顺序保留、未分配给某一帧，并指向 `errors` 命令查看详情。

`verify` and `frame` distinguish recording-wide timed/untimed totals from cumulative SDK timed errors present in a frame snapshot. An empty frame error state does not imply that the recording contains no untimed errors.

```powershell
cargo test --locked --offline -p pvd-cli error_tests
cargo test --locked --offline -p pvd-cli error_tests::real_native_and_sdk_error_fields_match_without_fabricated_timing -- --ignored --exact
cargo run --locked --offline -p pvd-cli -- errors captures/native/rich-paired-native.pxd2
cargo run --locked --offline -p pvd-cli -- errors captures/native/rich-paired.pvdstream
```

真实配对测试需要按[原版验证](native-validation.md)准备同次 rich 容器和 TCP 流，再显式执行。默认测试覆盖时间戳、末帧后错误、无完成帧及控制字符转义；构造边界不独立证明原版兼容。

## 帧来源与时间

`frame` 参数使用全局捕获索引，输出同时报告来源与来源内序号。属性 `csv` 保留原有列，并在末尾追加 `section,source_id,source_frame`。`seconds` / `time_seconds` 以该来源首个可见帧为零点，Legacy 使用录制首个可见帧；隐藏恢复前缀不参与计时。各来源的经过时间不表示已同步的共同时间轴，完整规则见[帧来源](frame-sources.md)。该时间与 CSV 范围的专项验收见[验证说明](validation.md#帧来源与-apex)。

## 文件导出

`trim`、`repx`、`csv`、`profile-csv`、`profile-event-csv` 和 `memory-csv` 使用相同的文件发布流程。命令参数、CSV 列、采样规则和 RepX 内容保持各命令的定义；只有完整写入、缓冲刷新、文件同步和发布均成功后，才打印 `exported_rows` / `file` 等成功结果。

All six file-export commands stage their output in the destination directory, flush buffered bytes, synchronize the file, and publish without replacing an existing destination. Success is reported only after publication.

## 文件边界

- 输出目录必须已经存在。目录先解析为绝对路径，临时文件通过 `create_new` 创建，名称为 `.pvdstudio-cli-output-<pid>-<sequence>.tmp`。
- 写入、生成内容或同步失败时，关闭并删除此次操作创建的临时文件；不扫描、不删除其他临时文件。
- Windows 使用 `MoveFileExW`，标志为 `0`，不允许覆盖且不允许跨卷复制。临时文件与目标文件位于同一目录。其他平台使用不覆盖的硬链接发布，再删除临时路径；这些平台的实际文件系统行为需分别验证。
- 目标预先存在，或其他进程在写入期间创建了同名目标，导出均失败并保留既有内容。源捕获本身、其硬链接别名和已存在的符号链接也属于禁止覆盖的目标。
- CSV 使用流式缓冲写入，不为发布操作另建完整 CSV 内存副本。SDK 的 `profile-csv` / `memory-csv` 使用双遍完整流式读取，原版容器使用有界导入后的完整分析，语义及资源边界见 [分析导出](analysis-export.md)。其他曲线仍按相应核心加载和采样预算处理。`trim` 使用有界双遍读取，取消原总文件 512 MiB 限制，逐帧验证所选场景并检查两遍完整源摘要；检查和导出进度写入 stderr，成功摘要仅在发布后写入 stdout。当前状态及事务预算见 [裁剪](clips.md)。
- `listen` 的实时捕获由 transport 管理，保留 `.partial` 语义；它不属于这些离线导出命令。`profile-events` 等标准输出命令也不创建导出文件。

No command overwrites the input capture or another existing file. A competing writer's completed destination is preserved. This mechanism does not change native PVD3 container support or permit trimming a native container into an SDK stream.

## 中断与持久化范围

`trim`、`profile-csv` 和 `memory-csv` 在 Windows 使用 Ctrl-C / Ctrl-Break 请求协作取消，Unix 接入 Ctrl-C。读取/写入回调及同步后、发布前都会检查；取消返回 `Interrupted`，处理方式与其他失败相同，临时文件会被清理。原版容器的分析导出也使用协作检查，但当前整份原版导入不能在内部随时中止。其他导出命令尚未接此信号处理器。强制结束进程、系统崩溃或断电可能跳过清理，留下 `.tmp` 文件；未完成的写入不会以正式导出文件名发布。文件同步不等于对所有文件系统、设备缓存或目录元数据提供断电恢复保证。清理也依赖操作系统仍允许删除本进程创建的临时文件。

Forced termination can leave a staging file. There is no claim of power-loss durability or automatic recovery of abandoned staging files.

Windows 信号取消需在隔离控制台中验证检查、输出和部分内容已写入时的返回码、发布及暂存清理，不向其他进程广播信号。Unix 入口已实现，但实际平台行为仍需验证。

## 可重复验证

```powershell
cargo test --locked --offline -p pvd-cli atomic_output::tests
cargo clippy --locked --offline -p pvd-cli --all-targets -- -D warnings
```

测试文件仅创建在工作区 `output/cli-output-tests/` 的独立运行子目录中。测试覆盖：

| 条件 | 验证结果 |
| --- | --- |
| 写入部分内容后注入 `StorageFull` | 正式路径不存在，自己的临时文件清理，原捕获和无关文件内容不变；相同目标可重试成功 |
| 数据已刷入文件后同步返回失败 | 不发布正式路径，临时文件清理 |
| 生成内容返回 `Interrupted` | 不留下正式文件或临时文件 |
| 写入期间另一方创建目标 | 返回同名冲突，保留另一方内容，清理自己的临时文件 |
| 中文目标路径、256 KiB 内容、源文件及硬链接别名 | 完整字节一致；所有既有源路径拒绝覆盖，且不执行写入回调 |

写入和同步故障通过受控错误注入验证，不会实际占满磁盘。测试不修改系统设置，也不证明断电恢复能力。
