# 精确 CPU 性能事件曲线

本功能把录制中的任意 CPU profile 事件系列加入曲线图页。它使用已经捕获的 start/stop tick，不重执行物理模拟，也不把性能缓冲区的接收帧当成事件发生帧。首条真实验证路径是固定官方 PhysX 3.4.2 SDK（commit `5e42a5f112351a223c19c17bb331e6c55037b8eb`）的 Win64 生产器。

完整 SDK 文件使用流式目录分页、事件与固定阶段采样，并提供 CLI 精确键入口。实时和原版仍受各自分析范围约束，线程时间线也需区分窗口和完整文件，见[完整文件曲线](full-file-profile.md)。

## 身份、参考场景与累计口径

一条系列由完整四元组 `(zone:u64, event:u16, thread:u32, context:u64)` 标识。相同事件名在不同 zone、线程或 context 下是不同系列；显示名不参与去重。元数据被重复定义且发生歧义时，列表显示身份歧义，曲线留空，不能把最后一次名字当成确切历史名字。

参考 Scene 取自当前展示帧的 `scene_id`，并验证为有效 `physx3::PxScene`，不使用对象树选中的其他对象。其对象 ID 和代际固定在曲线中，仅定义相邻 `EndSection("frame")` 的时间窗口。事件的 context 是不透明值，**按 Scene 时间窗口对齐不代表事件归属于该 Scene**，也不强制 `context == scene_id`。真实样例特意使用与 Scene 地址不同的 context，并覆盖同名事件的两个线程与两个 context。

`InclusiveSum` 累加完整落入同一窗口的每个 span 的 `(end-start)`，按流头的时钟比例换算为毫秒。重复、嵌套与重叠的出现次数分别累加，因此结果不是 CPU 占用率、独占耗时或所有阶段的总耗时。每个采样同时保留完整出现次数 `occurrence_count`，空值不能当成零耗时。

| 状态（CSV `status`） | 值与诊断 |
| --- | --- |
| `recorded` | 有确切完整区间，输出包含耗时之和 |
| `no_complete_span` | 此窗口没有完整出现，保留空值 |
| `no_previous_frame` | 参考 Scene 第一帧没有前一个边界 |
| `span_crosses_frame_boundary` | 区间跨越窗口边界，受影响窗口留空，不裁切或推测分摊 |
| `open_span_may_overlap_window` | 未结束的起点可能影响窗口，保留空值 |
| `unmatched_stop_has_unknown_start` | 结束事件没有配对起点，不能可靠划定影响范围 |
| `metadata_ambiguous` | zone/event 元数据身份发生歧义 |
| `non_monotonic_scene_clock` | 参考 Scene 边界不严格递增 |
| `other_scene` | 所请求全局帧不是参考 Scene 的帧 |
| `other_or_missing_generation` | 对象代际不匹配或缺失 |

后两个状态由核心显式帧索引接口保留；桌面与 CLI 的自动采样先筛选参考 Scene 和代际，一般不会主动选择这些帧。原始 spans 不会被聚合结果覆盖。

固定“绘制帧与阶段”的曲线有不同定义，见 [performance-curves.md](performance-curves.md)：其阶段值是匹配场景 context 的**单个完整区间**。已有 `profile-metrics` 录制的 Solver key 含 24 个完整区间及 24 个未结束起点；固定阶段通道保留完整区间值，并在详情说明排除的未结束/未配对数量。通用 InclusiveSum 通道则因可能遗漏累计区间而留空。这两个口径不能互换，已有生产器 JSON 仅独立验证完整区间，不独立证明这些未结束起点的数量。

## 桌面交互与资源边界

性能面板的“事件曲线 / Event curves”切换项展示虚拟化列表。保留窗口和原版已加载目录按每个精确 key 的最小开始 tick 排序（包含未结束起点）；没有开始 tick 的条目排在最后，相同时按 key 排序。完整 SDK 文件分页默认按完整 key 排序，每页 1024 项，筛选仅作用于当前页；Windows 实际文件可选择同样定义的 [首次开始时间排序](profile-catalog-index.md)，通过有界磁盘索引生成全源分页。这是项目明确的排序语义，原版精确规则仍未证实。两种目录不改变线程时间线内部按时间排序及悬停查询。

列表可按名字或显示的精确身份过滤，每行展示身份、完整/未结束/未配对数量，并通过悬停查看全部内容。当前参考 Scene 和代际始终显示；“曲线 / Plot”创建或选中该精确 key 与参考 Scene 代际对应的曲线，并切换到曲线页。没有有效 Scene 时按钮不可用。

曲线页与属性曲线共享最多 16 条曲线、8 个图页；同一曲线的采样由图页共享。彩色图例对应当前图页成员，完整身份在悬停与“详情”中。长标签省略，窄面板纵向滚动；图中仍可缩放、平移、矩形缩放、标记和点击跳帧。详情说明包含耗时语义、空值原因与采样范围。中文与英文共用相同数据路径。

不足 280 像素高的面板使用紧凑控件：性能 CSV 位于导出菜单，参考 Scene 和累计口径从提示查看；目录筛选限定当前页。曲线管理、采样、进度/取消和 CSV 位于图页设置。中英文窄面板需检查标签、事件行与绘图按钮可见性。

通用事件曲线先构建参考 Scene **同一代际的完成帧序列**，再以至多 5,000 个采样保留首尾；`stride` 是这个序列的步长，不是全局帧号差。横轴及导出仍使用全局帧号。该顺序避免多 Scene 交错时全局固定步长恰好跳过整个参考 Scene。属性和固定阶段曲线仍使用全录制序列步长，CSV 明确区分。

旧全目录接口上限 100,000 个精确系列；完整 SDK 文件的分页接口不保留全目录，CLI 每页可请求 1–4096 项，游标绑定源内容。标识顺序每次翻页重新解码完整输入；首次开始时间索引每页完整核验源字节，但不重复协议解码。保留窗口路径的完整事件及帧索引各有 2,000,000 条预算；完整 SDK 文件流式路径不保存全部完成事件，但仍限制帧索引和解析工作集，详见上述专题。超出时明确报错而非静默截断。目录索引与采样运行在后台，支持取消。关闭、切换录制或销毁 Studio 会立即设置目录和采样取消标记，即使性能面板隐藏；保留窗口缓存检查源 Recording 的 Weak 身份，完整文件缓存按来源代际与内容身份绑定，旧任务结果不能成为新源的目录。核心循环轮询取消，排序期间不会同步阻塞 UI。

## CSV

桌面逐曲线 CSV 保留属性/阶段已有字段，并追加精确身份、次数及采样口径，共 22 列：

```text
frame,value,object_id,generation,property,component,stride,captured_frame_count,source,unit,status,association,zone,event,thread,context,occurrence_count,aggregation,reference_scene,reference_scene_generation,stride_basis,reference_frame_count
```

通用事件曲线字段：

- `source=profile_event_inclusive_sum`、`unit=ms`、`aggregation=inclusive_sum`。
- `association=reference_scene_time_window_not_scene_ownership`。
- `zone/event/thread/context` 保留精确 key；zone/context 使用 `0x` 十六进制，event/thread 为十进制。
- `occurrence_count` 即使 `value` 为空也保留；数值为该窗口完整区间数，不填补未结束区间。
- `reference_scene/reference_scene_generation` 是对齐参考对象。旧公共列 `object_id/generation` 在性能曲线中同样指参考场景，不能解释成事件所有者。
- `stride_basis=reference_scene_generation_frames`，`reference_frame_count` 为该参考代际帧总数，`captured_frame_count` 为全录制帧总数。

属性 CSV 的性能身份/次数与参考 Scene 列为空，`aggregation=recorded_value`。固定阶段的精确身份/次数列为空，`aggregation=single_complete_span`；帧结束间隔使用 `interval_difference`。这两类的 `stride_basis=captured_frames`，`reference_frame_count` 为空。

CLI 独立提供 `profile-events` 列表及 `profile-event-csv` 导出。CLI 使用专用 CSV 表头；与桌面共享精确 key、参考 Scene、状态、次数、累计算法及 `stride_basis/reference_frame_count` 含义，不应按桌面的列序解析 CLI 文件。

`profile-key-csv` 直接指定完整 zone/event/thread/context，SDK 输入不先构建全系列目录；其 `series_index` 留空。源文件、参考 Scene 及输出参数见 [完整文件曲线](full-file-profile.md)。

## 可重复验证

```powershell
.\tools\profile-events\build.ps1
.\tools\profile-events\verify.ps1 -SkipBuild
cargo test --locked -p pvd-core --test profile_curves -- --include-ignored
cargo test --locked -p pvd-desktop curve -- --include-ignored
cargo test --locked -p pvd-desktop profile::tests -- --include-ignored
cargo clippy --locked -p pvd-desktop --tests -- -D warnings
.\target\debug\pvd-cli.exe profile-events captures/physx-3.4-profile-events.pvdstream
```

从目录输出选择确切的系列索引、参考 Scene ID 和代际，再执行：

```text
pvd-cli profile-event-csv <capture> <series-index> <scene-id> <scene-generation> <output.csv>
```

SDK 生产器在实际转发 `PxProfilerCallback` 前后读取 Windows QPC，JSON 来自调用与时钟上下界，未从本项目解析输出反向生成。8 帧真实录制包含 6 条自定义精确系列、12 个完整区间及 1 个未结束起点：同线程不同 context、同 context 不同线程、同窗口嵌套重复、跨第 3–4 帧区间、从第 6 帧未结束的起点，以及开始和结束抵达顺序不同的事件。核心测试验证 key/顺序/计时、跨界与未结束的空值。桌面测试对 Sample 主线程/context A 的第 1、4、5 帧分别核对 3、2、2 次出现，数值逐一落在独立 QPC 包含耗时上下界内，并核对 CSV、关闭取消及换源重新采样。

生产器及验证脚本详见 [tools/profile-events/README.md](../tools/profile-events/README.md)。`verify.ps1` 在独立输出目录生成新 stream/JSON 并运行核心与 CLI 验证；桌面真实测试默认使用 `captures/` 的同名固定输入，或用 `PVD_PROFILE_EVENTS_CAPTURE` 指定新输出。捕获和输出需来自同一运行。

合成 egui 输入验证 50,000 条目录只绘制可见行、Plot 精确 key 及禁用状态、元数据歧义筛选；620×280 中英文曲线面板检验长身份不横向溢出且绘图区至少 80px。原有嵌套线程轨道、悬停范围、测量与图表拖拽/输入焦点测试继续覆盖相同路径。这些测试不操作用户的系统输入。

这些证据验证 CPU profile 数据通路，不代表 GPU 事件曲线、CPU 占用率或原版 PVD3 内部图表聚合算法已经兼容。其他 SDK、异步真实多 Scene、其他平台计时器仍需各自样例验证；交错 Scene 与短代际采样边界由核心合成协议测试覆盖。

离屏检查需查看中英文事件列表及曲线，确认控件和数据语义；它不能替代原生输入或 GPU 回放性能验收。