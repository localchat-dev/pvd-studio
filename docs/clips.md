# SDK 帧区间裁剪

`pvd_core::clip::trim_scene_frames_streaming(source, output, first, last, progress)` 按 `frame` / `ApexFrame` End 的全局捕获顺序导出含首尾的区间，输出全局索引和各来源序号重新从零计数。这与工具栏的来源内帧号不同，见[帧来源](frame-sources.md)。桌面与 CLI 使用此流式入口；内存接口 `trim_scene_frames` 保留。输入与输出均为已支持的 SDK 原始 PVD communication stream；扩展名使用 `.pvdstream`，不能声称输出原版 PVD3 保存的 `.pxd/.pxd2` 容器。

## 磁盘文件工作流

当前源码支持完整 SDK 文件的内存和磁盘加载模式：编辑及保存片段定义、重开恢复、流式另存、阶段进度与取消。磁盘加载时边解码边计算完整源身份；导出再次核对源 BLAKE3、大小和完整帧数，文件被替换时拒绝发布。仍在接收的滚动历史需先完成并保存原始录制，停止监听后重开；原版数据库使用独立的[保存与裁剪入口](native-export.md)。

导出分 Inspect 和 Export 两遍完整扫描，包括选定末帧之后的尾部。第一遍验证完整流并建立原始边界与 renderer 重写计划；第二遍按各来源窗口比较所选帧的完整场景状态，并验证两遍源摘要相同。分析字节按原顺序保留，第一遍完整解码验证；不另存两份完整分析历史进行比较。流式入口不累计动作历史、帧目录和检查点，也不受总文件 512 MiB 限制。

录制导出后台任务最多同时存活两个，关闭录制或换源会请求取消，晚到结果不会套用到另一录制。取消可在每 64 KiB 输入及每 512 个事件被检查；系统 I/O、单条解码或文件同步期间不能即时中断。已完成发布的文件不能被取消回滚。临时文件在同目录写入、刷新并同步，成功后无覆盖发布；取消或失败会清理本任务的暂存文件。强制结束进程可能留下暂存文件。

SDK 裁剪回归覆盖真实输入、边界范围、两遍一致性、超过 512 MiB 的流式读取、内存界限、取消、源变化和无覆盖发布。自动化不能替代原生窗口导出验收。

```powershell
cargo test --locked --offline -p pvd-core --test streaming_clips -- --include-ignored
cargo test --locked --offline -p pvd-desktop recording_export::tests
cargo test --locked --offline -p pvd-desktop studio::clip_tests:: -- --ignored
```

## 保留规则

- 保留区间前的元数据、对象生命周期、属性、引用和错误，使第一帧的状态能够重建。仅按原始 source 和已知名称配对 `frame` / `ApexFrame` 的 Begin/End；被选 End 的 Begin 即使位于前史也保留原位。没有保留 End 的 Begin 被移除，End-only 输入不补造 Begin。同 key 重叠 Begin 无法唯一配对时明确拒绝，不以 stream ID 猜测归属。
- 保留性能与内存数据，包括区间前的字符串表、分配、释放和跨区间的性能事件起点。原始时间戳及事件顺序不变。`received_frame` 是接收关联帧号，区间前的分析事件归到输出第 0 帧，其他事件为原帧号减去 `first`；这不是重新计时后的区间统计。首帧内存分配/释放总数包含初始化与 preroll，跨界 span 可以早于可见区间开始。
- 各来源首个保留帧需要从前一个同源 End 恢复绘制。共享前史只剥离早于所有来源所需窗口的绘制；任一来源没有前驱时从流头保留。被剥离的 `PvdUserRenderer.events` 只保留最后一个 SetInstanceId，来源窗口见[帧来源](frame-sources.md#绘制窗口)。
- 必要绘制窗口及区间内的 renderer payload 逐字节保留，包含完整原始 `events` 属性。协议组只重写受影响的长度和事件数，保留原 source、section 名称、ticks、stream ID 和事件相对顺序。末个选定 End 之后的绘制不移入该帧。
- Begin/Append/EndSetPropertyValue 以 EndSetPropertyValue 的完整提交位置判断。提交早于所需窗口的事务仅在最后一个 Append 中保留 instance 状态，其余 Append 为空；跨越窗口下界的事务完整保留，Begin/End 和 stream ID 不变。

输出按原区间逐帧核对；流式入口先在两侧投影对应来源的绘制窗口，再比较完整 `SceneState`，包含对象全部属性原始字节、引用代际、错误、接触点和调试绘制；任何差异返回错误。不能通过省略 `debug_draw` 或原始 `events` 属性来通过验收。

## 协议证据

PhysX 3.4.2 官方 SDK 固定提交 `5e42a5f112351a223c19c17bb331e6c55037b8eb`：

- `PxShared/src/pvd/src/PxPvdUserRenderTypes.h`、`PxPvdUserRenderImpl.h` 定义 11 种 renderer 事件、字段顺序、数值与数组结构。
- `PxPvdUserRenderer.cpp` 的 `handleEvent`、`setInstanceId` 和 `flushRenderEvents`：事件可因缓冲容量而分批 flush；flush 清空发送缓冲，不自动追加重设 instance 的事件。
- `PhysX_3.4/Source/PhysX/src/buffering/ScbScenePvdClient.cpp` 中 `SceneRendererClient::handleBufferFlush` 将 SDK renderer 缓冲写入 `events` 字节属性。
- `PxShared/src/pvd/src/PxPvdDataStream.cpp` 定义 BeginSetPropertyValue、AppendPropertyValueData、EndSetPropertyValue 的真实编码路径。

这些源码给出可靠事件边界。未知 renderer 事件没有外层单事件长度，裁剪直接拒绝；不能猜测跳过长度。该证据不等于已经观察过原版 PVD3 的片段导出文件或界面行为。

## 验证范围

既有 PhysX 3.4.2 验收记录包括：

| 真实输入 | 场景帧 | 区间数 | 核对内容 |
| --- | ---: | ---: | --- |
| `physx-3.4.pvdstream` | 120 | 5 | 对象生命周期、属性及引用代际，首尾与中段 |
| `physx-3.4-rich.pvdstream` | 120 | 6 | 真实 renderer、接触、网格与关节等全部状态 |
| `physx-3.4-joint-limits.pvdstream` | 6 | 4 | 四类限位、关节坐标系和有顶点颜色的调试三角形 |
| `physx-3.4-heightfield.pvdstream` | 75 | 5 | 高度场资源和对象状态；此输入不含非空 renderer 绘制 |
| `physx-3.4-query-shapes.pvdstream` | 12 | 5 | 查询形状及对象创建销毁 |
| `physx-3.4-error.pvdstream` | 12 | 4 | SDK 实际发出的非法质量错误与原始位置/时间 |
| `physx-3.4-all.pvdstream` | 120 | 70–100 | 性能 zone/span/value、内存逐事件、每帧 live 状态及原始时间戳 |

29 个范围覆盖前六份输入全部可见帧，并包含单帧、首帧、尾帧和中段边界。每个输出都验证顺序播放与检查点回放一致，并按逆序逐帧比较完整状态、原 scene ID、原时间戳和对应内存状态。

以真实 joint-limit payload 构造的结构边界测试还包括：把 renderer 属性分为每片 7 字节的事务、把提交移至起始帧边界之后、移除首个选定帧的 renderer 更新。这些修改仅用于事务与拒绝路径测试，不冒充 SDK 原样发送过相同分块方式。前两项完整比较通过；第三项触发预期的明确拒绝。选定末帧位于未提交事务中的情况也验证为明确拒绝。

默认单元测试同时覆盖大小端、11 种 renderer 事件的每一处截断、未知事件、异常数组长度、文本长度和 instance 跨缓冲保留。`pvd-protocol::rewrite::map_group_events` 的测试覆盖组头保留、截断、异常声明长度及不能可靠分帧的替换。每次替换只允许一个可完整解析的事件，不接受一段包含多个事件的任意字节。

```powershell
cargo test --locked -p pvd-core --lib clip::
cargo test --locked -p pvd-protocol
cargo test --locked -p pvd-core --test clips -- --ignored --nocapture
cargo run --locked -p pvd-cli -- trim captures/physx-3.4-rich.pvdstream 70 100 output/rich-clip-70-100.pvdstream
```

真实输入生成方式分别见 `tools/verify-sdk.ps1`、`docs/joint-limits.md`、`docs/heightfield.md` 和 `docs/query-shapes.md`；测试允许用 `PVD_SDK34_CAPTURE`、`PVD_SDK34_RICH_CAPTURE`、`PVD_JOINT_LIMIT_CAPTURE`、`PVD_HEIGHTFIELD_CAPTURE`、`PVD_QUERY_SHAPES_CAPTURE`、`PVD_CLIP_ERROR_CAPTURE`、`PVD_CLIP_ANALYSIS_CAPTURE` 指向其他相应真实输入。没有输入时保持 ignored；不能把默认测试报告为真实验收已通过。

`apex_clips` 已覆盖双端序、同 raw source 的不同名称、嵌套与未配对边界、分块提交、连续裁剪、取消、资源限制和独立图元真值。真实 APEX 同步/异步前缀另核对原始 payload、事件组元数据、来源端点和完整帧状态；不可表示的中段保持拒绝。命令与输入见[APEX 生产器](../tools/physx-apex-clothing/README.md)。

## 边界与限制

桌面“片段”面板支持最多 64 个命名范围。可以编辑起止帧、在播放头处分割、裁去左右侧、定位、另存录制片段或移除定义。分割采用含首尾范围，播放头属于左片段，右片段从下一帧开始；不能创建空片段。名称最多 80 个 Unicode 字符，分割产生的名称也受此限制。另存录制片段使用面板内明确的输出路径并拒绝覆盖已有文件；移除范围只改变定义，不删除任何录制。

## 片段定义跨会话保存（CLIP-02）

存储自动化和无窗口渲染不能替代原生窗口保存、重开和退出验收。

打开完整录制后，点击“保存片段定义 / Save definitions”，写入原文件旁的 `<原文件名>.pvdstudio-clips.json`。例如 `capture.pvdstream` 对应 `capture.pvdstream.pvdstudio-clips.json`。重新打开同一录制时自动恢复已保存的名称、含首尾帧范围、分割结果和保存时的选中索引；定义编辑具有明确的未保存状态。仅改变选中项不视作未保存编辑。

这是 pvd-studio 自有的 JSON 附属文件，格式标识为 `pvd-studio.clip-definitions`，版本为 1。不向原始 `.pvdstream` 写入片段元数据，不使用 `.pxd/.pxd2` 扩展名，不声称是 NVIDIA PVD3 的保存格式。

- 身份绑定包括整个捕获字节的 BLAKE3、字节数和解析得到的场景帧数。路径相同、长度相同或帧数相同均不能替代内容匹配。换入不同录制后明确拒绝套用定义，同时仍可打开和浏览有效的录制。
- 保存前在后台重新读取并核对完整捕获；同一路径已被替换时拒绝保存。已有 sidecar 的内容摘要也必须匹配当前会话所读版本，防止旧会话覆盖其他会话的修改。
- Sidecar 最多 64 KiB，读取额外 1 字节判定超限；反序列化拒绝未知字段和不支持的版本，检查范围、选中索引、名称长度及控制字符。损坏或不匹配时保留原文件，显示中英双语诊断并禁用定义编辑和覆盖保存。确认来源后可在应用外保留备份并移走错误的 sidecar，再重开录制；应用不会静默重置错误文件。
- 写入使用同目录临时文件、`sync_all`、会话间 `.lock` 和发布步骤。Windows 首次发布使用不带替换标志的 `MoveFileExW`，不要求硬链接支持；更新前核对已读版本，替换完整文件。正常返回或错误会清理本操作的临时文件和锁。进程被强制结束可留下 `.lock` 或临时文件；出现锁诊断时，先确认没有写入进程，再处理遗留文件。不能声称已经验证突然断电的目录持久性。
- 保存和身份检查运行在工作线程，可取消，结果仅应用到原会话。取消前已经完成提交的文件可能仍保留；界面会说明应重开核对，不声称取消能够回滚已提交文件。
- 接收中、监听尚未停止、录制索引未完成、`.partial` 捕获或未保存到本地的实时数据均不能编辑持久化定义。完整实时捕获需结束接收并停止监听；未绑定本地完整文件的接收结果需先保存再重开。
- 有未保存定义或仍在保存时，关闭录制、打开其他录制、开始监听和主窗口关闭按钮均显示可取消的应用内确认。“继续编辑 / Keep editing”保留编辑；“放弃修改并继续 / Discard and continue”才执行原操作。重复主窗口关闭请求不会重复退出；确认期间暂停回放并阻断快捷键和相机手势，已按住的右键飞行需要释放后重新按下才恢复。这不是独立原生多窗口功能。

持久化自动化验收命令：

```powershell
cargo test --locked --offline -p pvd-desktop clip_store::tests:: -- --include-ignored
cargo test --locked --offline -p pvd-desktop clips::tests::
cargo test --locked --offline -p pvd-desktop camera_controller::tests::modal_cancellation
```

`clip_store_real_sdk_reopen_replace_cancel_and_tamper` 使用官方 SDK 3.4 生成的 `physx-3.4.pvdstream` 和 `physx-3.4-rich.pvdstream`（各 120 帧），在临时目录复制后操作：实际解析、保存和三次重开、检查对象状态、取消未完成的打开、同路径换入另一真实捕获、篡改 sidecar 后的拒绝及原录制完整性。默认保持 ignored，只有两份真实输入存在且显式运行时才能计入通过；环境变量 `PVD_SDK34_CAPTURE`、`PVD_SDK34_RICH_CAPTURE` 可覆盖路径。其余存储边界测试使用明确的身份测试字节，不将它们当作有效 SDK 捕获或原版容器证据。

片段存储、关闭保护和 modal 取消恢复由自动化覆盖。系统窗口关闭、焦点、IME 和原生文件选择器仍需独立验收。

## 裁剪格式边界

- 流式入口的瞬时场景及元数据预算为每个验证器 512 MiB，第一遍分析预算为 128 MiB；这些不是整个进程 RSS 上限。单协议组最多 64 MiB。Renderer 临时事务数据总量不超过 64 MiB，待提交 Append 索引总数不超过 1,000,000；重写计划最多 524,288 项，section 边界计划最多 65,536 项。绘制日志还有[独立窗口预算](frame-sources.md#绘制窗口)。数组最多 1,000,000 个元素，文本最多 1 MiB。所有偏移与乘法检查溢出和截断。
- 多来源中段可能需要不同的前驱 End；移除这些 End 后，共享事件前史不能总是表达各自的初始绘制窗口。完整状态比较会拒绝这类区间，不增加、移位或伪造边界。首帧同时要求旧 `events` 原始属性和空绘制窗口时也可能不可表示。零字节 payload 与没有 payload 不同：前者更新原始属性，后者不会。
- 截止边界留下尚未提交的属性事务，或其他不能重建的依赖时，重新解析或完整比较返回错误。流式调用方必须写入暂存文件，仅在返回成功后发布目标文件。
- 这是保留初始化历史的场景帧裁剪，文件不保证按帧数同比变小，也不保证删除区间外全部分析数据。原版保存容器、字节最小化重写、多 SDK 裁剪与原版 PVD3 导出行为尚未独立验收。
