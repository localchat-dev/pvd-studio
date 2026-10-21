# 完整 SDK 文件的性能事件曲线

完整 SDK 文件的精确事件目录和曲线使用流式扫描，读取完整输入。事件身份、参考 Scene、包含耗时和空值遵循[事件曲线](profile-curves.md)。

桌面后台目录扫描绑定打开文件时的 BLAKE3、字节数和帧数；采样再次校验源身份，文件被替换时不会发布不同来源的数据。完整文件目录不随跳帧反复扫描，关闭、切换录制和退出仍通过统一后台任务取消、等待。实时流和原版数据库保留其既有分析路径。固定 simulate/collision/solver 图也已接入完整文件两遍扫描；完整 SDK 文件的线程时间线另有按时间窗查询入口，见 [完整文件线程时间线](full-file-thread-timeline.md)。

桌面事件目录每页 1,024 个精确键，提供首页、上一页和下一页，最多记住 256 个后退游标。默认按 zone/event/thread/context 数值排序，名称筛选只作用于当前页；标识顺序每次翻页完整扫描源文件，不能保证即时返回。Windows 实际文件还可选择 [首次开始时间](profile-catalog-index.md)，首次建立有界目录索引，后续每页完整校验源字节后读索引，不重复协议解码。CLI 的原有全目录仍限 100,000 个键，新 key 分页入口允许每页 1–4,096 项。两种目录都限制 32 MiB 估算返回索引；流式解析器的分析和当前场景另有 128 MiB、512 MiB 预算。曲线最多 5,000 个采样，完成帧索引最多 2,000,000 项、64 MiB 估算索引。CLI 曲线还需要完整磁盘帧目录，默认历史预算 8 GiB、600 帧或 64 MiB 分段；出现淘汰或历史警告会明确失败，不能静默输出部分录制。上述预算不等于进程内存上限。

CLI 保留系列索引入口，并提供完整四元组选择，避免单个目标事件也需要收集十万种无关身份：

```text
pvd-cli profile-events <capture>
pvd-cli profile-events-page <sdk-capture> <limit:1..4096> [cursor]
pvd-cli profile-event-csv <capture> <series-index> <scene-id> <scene-generation> <output.csv>
pvd-cli profile-key-csv <capture> <zone-id> <event-id> <thread-id> <context-id> <scene-id> <scene-generation> <output.csv>
```

直接按键导出的 `series_index` 留空，其余 CSV 字段与系列索引入口相同。曲线输出先写临时文件，源校验和采样成功后无覆盖发布。分页命令在完整校验后写标准输出，`# next_cursor` 绑定源 BLAKE3、字节数和帧数，空游标表示末页。`page_row` 是页内行号，不能作为既有 `profile-event-csv` 的系列索引；可把完整四元组交给 `profile-key-csv`。原版数据库不支持分页命令。

固定阶段保持 `single_complete_span` 定义，不累计多个同阶段区间。缺失、跨边界、重复区间及错误代际仍留空；未结束事件只作为诊断，不加入耗时。相关 zone/event 元数据在文件中被重定义时拒绝采样，避免使用最终名称解释不同历史含义。帧间隔继续从完整帧目录读取。

## 分页与固定阶段验收

目录分页需覆盖多页构造输入和真实 SDK 文件，核对游标、筛选、计数及覆盖范围。取消、源变化和翻页时还需检查 worker 收尾；部分页面通过不能当作全目录验证。

固定阶段采样需以独立 SDK QPC 上下界和完整分析为预期，覆盖保留窗口以外的帧及未结束 Solver 起点。小输入与长输入分别核对；临时历史正常释放后为空，功能核对不作为稳定性能测量。

## 证据与限制

源绑定回归覆盖同长度同内容、同长度不同内容、取消、换源、退出等待和目录身份。验证记录实际构建和输入，不以文件时间推断同源。

真实 SDK 小样例验证 270 条系列与 53 行独立 QPC 边界；CLI 的 6 个自定义系列、12 个完整区间和 48 行 CSV 通过既有 SDK 验证器。原版碰撞曲线有 119 个非空值，与 025600 CLI 字节相同。四个实际 Ctrl+C 场景覆盖目录扫描、磁盘建帧索引、曲线目录和采样，取消后无输出或缓存残留。

600 秒的真实输入为 251,301,039 字节、19,293 帧，SHA256 `6971a3de54de4f1f3f80b7d67ffac8c33a73cbe1a60eb785f50b909551947e84`。默认分段完整保留 33 段、952,654,852 字节历史；完整事件目录因超过十万键明确失败。按一个确切碰撞事件键导出 4,824 行、4,823 个有效值，其中 4,664 个有效值早于当前保留分析起点 18,660。核心另外核对 158 个尾部点与保留窗口算法一致。此长例验证全文件覆盖与清理，小样例提供独立计时真值；不能将尾部对照扩展为全部长录制的独立 SDK 计时证明。

文件和 TCP 专项依赖实际运行环境。环境权限或缺少输入造成的失败应与产品逻辑问题分开记录；测试、Clippy 和格式要求见[验证说明](validation.md)。

重复核心真实输入检查：

```powershell
cargo test --locked -p pvd-core --test profile_curves_streaming sdk_catalog_and_every_exact_series_match_complete_memory_sampling -- --exact --ignored --nocapture
cargo test --locked -p pvd-core --test frame_metrics_streaming complete_fixed_stages_match_independent_sdk_qpc_bounds_after_window_expiry -- --exact --ignored --nocapture
cargo test --locked -p pvd-desktop full_profile -- --nocapture
cargo test --locked -p pvd-desktop profile::refresh::tests::complete_catalog -- --nocapture
```

长文件测试输入由测试环境变量指定，准确参数见候选 `reproduce.ps1` 和 `core-long-file-default-key.log`。不要把默认忽略项当成默认已执行。并行构建期间的导出耗时不作为性能基准。
