# 完整历史线程时间线

完整 SDK 文件和原版数据库 v1 的“性能 → 线程时间线”可查询早于当前显示分析窗口的 CPU 区间。原版查询要求完整保留的磁盘历史；实时捕获继续使用当前分析窗口。

SDK 查询按[完整文件性能扫描](full-file-profile.md)独立读取性能数据，内存字典缺失不阻断时间线；协议或性能数据本身损坏仍报错。

## 操作与数据含义

首次打开面板会扫描完整源，建立按数值线程 ID 排序的目录和完整时间范围。桌面每页 512 个线程、最多保留 256 个返回游标；核心每页上限 4096。时间窗默认放在当前帧附近，宽度至多 100 ms；在“时间窗”设置中输入范围、选择线程和可选 Context 后，点击“载入时间窗”。Context 接受十进制或 `0x` 十六进制，留空表示不限。

载入后可筛选名称、缩放、平移、测量和点击事件跳到其刷新帧。滚动只操作已经载入的结果，不触发源文件扫描；换到另一段时间需要再次载入。修改参数时明确显示与已载入结果的区别，开始新请求会清除旧结果，失败后仍可缩小范围重试。UI 一次发起一个当前查询；取消后的任务仍由统一后台注册表追踪到资源释放，关闭、换源和退出均发送取消。

Shift+滚轮水平平移，Ctrl+滚轮缩放。右键单个事件条可绘制该精确事件/线程/Context 的逐帧耗时曲线，计算口径见[事件曲线](profile-curves.md)；合并显示的密集条不能任意代表其中一个事件。

毫秒相对完整源首个完成事件的起点，以整数 tick 相减后换算。悬停信息保留原始起止 ticks、zone/event/thread/context 和接收刷新帧。窗口是闭区间，返回相交事件的完整原始端点，包括跨越整个窗口的长事件和零时长事件。刷新帧不等于精确 SDK 帧边界；局部轨道表达重叠关系，不推断调用栈，也不保证两个独立查询的轨道编号相同。

pending start、unmatched stop 和旧 CUDA 缓冲计数来自全源，不能解释为当前窗内发生数。只有 pending/unmatched 的输入没有完成事件时间范围。CUDA 缓冲未转换为 GPU 时间线，数值 profile value 也不当作 CPU span。

## 来源与资源边界

SDK 目录页游标和时间窗绑定完整输入的 BLAKE3、字节数及帧数，桌面还核对打开时的来源身份；同大小的不同内容也会拒绝。Windows 桌面的完整 SDK 文件默认首次载入时间窗会建立可复用的 CPU 磁盘索引，后续窗口不再重复解码协议；每次仍完整读取源文件核对 BLAKE3，再检查索引页。“时间窗…”设置可选择“直接扫描（不建索引）”，索引错误行也提供同范围直接扫描入口：每次完整解码源，不创建或等待索引，仍核验身份和窗口上限。线程目录仍单独扫描，没有实际路径的纯内存输入与非 Windows 路径仍流式解码。进度与取消覆盖这些步骤，同步文件读取调用本身不能立即中断。目录、生命周期和限额见 [CPU 时间线索引](profile-disk-index.md)。

原版数据库使用导入后持久历史的独立身份：录制末尾不可变分析根的进程内版本、帧数和时钟比例。它不是文件摘要，也不要求保留原始字节。显示跳帧不改变此身份；其他录制的游标会被拒绝。查询逐帧读取新增送达区间，并包含末帧后的分析尾部；按事件实际 ticks 筛选，晚送达不会遗漏或重复。每次查询扫描完整历史，不建立 SDK 字节索引；帧根逐个释放，历史淘汰或警告会明确报错。大文件仍需单独规模验收。

一次窗口最多 200,000 个 span、32 MiB 索引计量值，超限报错并要求缩窗或过滤，没有静默截断的成功结果。Vec 容量、增长时旧新缓冲共存、所选名称和树索引估算都参与计量；BTreeMap 节点和分配器开销并非精确堆字节，32 MiB 不包括桌面派生的 Analysis/Layout，也不是进程 RSS 上限。共享协议扫描器仍有自己的状态、配对和分析预算。

名称采用完整源尾部目录。所选 zone/event 曾发生实际重定义时拒绝该查询；相同定义重复发送允许，不相关的歧义不影响结果，缺失名称保留数字身份。返回结果构建独立 Analysis/Layout，不污染录制的当前分析或回放状态。

## 已有验证

- 原版 rich 容器与同次 SDK 流核对 3 个线程、19,215 个完整区间；12 个窗口共 57,458 条返回记录逐一核对起止 tick、身份、接收帧、名称和配对统计。构造持久历史另覆盖晚送达、末帧尾部、零帧输入、游标隔离、取消与资源预算。大于 512 MiB 的真实原版输入未验。
- 核心 v2 在两份官方 PhysX 3.4 输入上核对 3,612/1,223 个完整区间和独立 QPC 边界，覆盖闭区间、长事件、元数据、来源身份、取消及资源限制。真实 600 秒、19,293 帧输入的早/中/晚三个指定 Context 窗口各返回一个完整 span，并覆盖窄点窗、4 MiB 处取消和过宽窗口拒绝；不从三个窗口推断全量逐事件独立真值。
- 桌面候选对两份真实 SDK 输入的六个窗口逐字段匹配完整内存解码结果；保留窗口被清理后仍可查询早期事件。实际 reader 的关闭/退出/换源取消、同大小源变化、超限清空及缩窗重试通过。
- 应用字体下中英 620/400×180 面板测试中，事件轨道至少有 14 像素实际可见交集。正式 Release 的四张实际 GPU 图覆盖两份 SDK 输入的中英窗口，逐图核对可见事件轨道与控件；构建身份和全部十图验收见验证报告。原生鼠标操作仍待验，不能用纯 egui 或静态截图代替。

线程时间线与桌面查询应使用同一源身份核对。存在并行编译或其他负载时，功能检查耗时不作为稳定性能基准。

```powershell
$env:PVD_TIMELINE_ROOT = (Get-Location).Path
$env:PVD_PROFILE_STREAM_ROOT = (Get-Location).Path
cargo test --locked -p pvd-core --test profile_timeline sdk_timeline_windows_match_complete_spans_and_independent_qpc_bounds -- --exact --ignored
cargo test --locked -p pvd-desktop profile::timeline::tests::sdk_thread_windows_match_the_in_memory_decoder_and_leave_recording_untouched -- --exact --ignored
cargo test --locked -p pvd-core --test native_timeline -- --ignored --nocapture
cargo test --locked -p pvd-desktop profile::timeline::native_tests -- --ignored --test-threads=1
```

原版专项要求 `captures/native/rich-paired-native.pxd2` 与 `rich-paired.pvdstream` 来自同次捕获；核心可用 `PVD_NATIVE_FIXTURES` 指定目录，桌面可用 `PVD_NATIVE_RICH_CAPTURE` 指定容器。
