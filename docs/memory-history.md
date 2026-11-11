# 内存分析

内存分析保留真实 PVD allocation/free 事件，并可恢复任意 `received_frame` 的已观察存活分配。`received_frame` 是包含事件的内存缓冲在录制中被接收到的帧编号，**不是每次分配/释放实际发生的时刻**。SDK 批量刷新和退出时的最后刷新可能与可见场景帧不同。

## 核心接口

接口位于 `pvd_core::analysis`，不依赖 Bevy 或 egui：

| 接口 | 返回内容 |
| --- | --- |
| `MemoryAnalysis::state_at_frame(frame)` | 从事件起点恢复该接收帧及此前全部事件后的 `MemoryState` |
| `MemoryAnalysis::seek(&mut MemoryCursor, frame)` | 在可重用游标上前进、后退或跳转，返回当前 `MemoryState` 引用 |
| `MemoryAnalysis::events_at_frame(frame)` | 恰在该接收帧到达的 allocation/free 事件切片，包括同帧多次缓冲刷新 |
| `MemoryAnalysis::churn_at_frame(frame)` | 本帧新增且帧末存活的分配数量/字节，以及分配/释放、未知释放和地址覆盖统计 |
| `MemoryAnalysis::events()` | 按接收顺序保存的全部事件，只读 |

`MemoryState` 包含以 `(buffer ID, address)` 为键的存活分配、存活字节，以及截至该接收帧的分配/释放/未知释放/地址覆盖计数、总分配字节与已知总释放字节。Allocation 保留其原始 type、file、line、size 和分配事件的接收帧；已释放条目也可从事件列表中检查。

`MemoryEventKind::Allocate { allocation, replaced }` 保存新分配及可能被覆盖的旧分配。`Free { allocation }` 在匹配到已观察分配时保留该分配，否则为 `None`。分配和释放共享同一个 `Arc<Allocation>`，字符串句柄重新定义不会改变历史分配的 type/file。

地址复用与没有先释放的地址覆盖有区别。不同 buffer ID 下的相同地址互不覆盖；未知 free 计入事件数，但不会捏造大小。`MemoryChurn::live_byte_change()` 为分配字节减已知释放字节，再减覆盖掉的旧分配字节。未知 free 不能用于推算完整进程内存变化。

`surviving_allocations` / `surviving_bytes` 只统计在本刷新帧新分配、处理完本帧全部刷新后仍存活的分配。同帧释放或覆盖会扣除该分配；释放早期帧的分配不影响本项，未来帧的释放也不回改历史结果。计算扫描本帧事件，使用常量额外空间，不复制存活表。

界面将“本帧新增存活（Churn）”与活动量、净变化分别显示。这对应 PVD3 手册描述的本帧分配且未释放口径。刷新帧不能当作精确的物理帧或分配发生时刻。

## 按来源汇总

“按来源”以 `文件名:行号 (类型)` 为分组键，类型为空时省略括号。这与 PhysX Visual Debugger 3 的文件分组规则一致：不同目录中的同名文件、相同行号和类型会合并。悬停可查看完整来源路径；发生合并时显示提示，全部路径仍可在事件明细中检查。

分组列仅统计本刷新帧的新分配：“本帧分配”为收到的大小和次数，“同帧释放”为这些分配在本帧中被释放的大小和次数，“新增存活”为本帧结束后仍存活的部分。地址覆盖会扣除被覆盖分配的存活量，不计为释放；不同内存缓冲中的相同地址互不影响。早期帧分配的释放仍计入明细视图的“本帧活动”，不混入同帧释放列。

PVD3 静态实现按选定 Scene 的前一帧事件与当前事件之间的内存缓冲生成表格，并只匹配该范围内的分配和释放。本项目保留跨帧释放统计，未照搬原版忽略未匹配释放及字节差值窄化的行为。非空真实录制的原版逐行显示值与边界时机仍待独立同源对照。

汇总按分析版本和帧号缓存，每次界面更新最多读取 2,048 个事件，并在约 2 ms 后让出处理；可取消或重试。最多保留 16,384 个来源组、262,144 个本帧存活地址，估算缓存预算为 32 MiB。达到限制时不展示部分合计，事件明细仍可使用。切换来源或帧会释放失效缓存。

## 原版来源总量

原版数据库的“来源总量”按目录索引读取 `MemoryEventTotals.totals`，提供起点、当前帧和录制结束三个位置。它与分配/释放事件表独立；起点及结束值没有帧号或时间戳，当前值仅表示显示状态中保存的属性。零帧录制也可查看起点和结束值。

类型、完整文件路径和行号来自保存的来源目录。总量保留有符号 32 位字节值；存在负值时不提供非负合计。最终目录中的未来来源不显示为零。内存汇总是 PVD3 创建的本地实例，不要求具有 SDK 源指针；目录、类型、实例生命周期或属性不受支持时明确显示不可用。

任意非空原版初始快照都不能证明更早的内存历史完整。快照中的旧事件缓冲只恢复为属性，不作为新分配重播；后续未知释放不补造地址或大小。此时事件页显示已知存活与已知峰值，完整内存 CSV 不可用。来源总量不能反推分配次数、累计量、峰值或旧分配的接收位置。

核心通过 `Recording::native_initial_memory_totals`、`native_memory_totals_at` 和 `native_end_memory_totals` 返回共享只读视图；`memory_history_scope` 标明事件范围。目录及起点/结束数组纳入元数据和历史预算，视图不复制字符串与数组。桌面只格式化可见行，当前属性未变时复用结果；关闭录制及面板缓存失效时释放引用。

CLI 可独立检查来源值，省略位置时读取录制结束：

```powershell
cargo run --locked -p pvd-cli -- memory-totals captures/native/rich-paired-native.pxd2 end
cargo run --locked -p pvd-cli -- memory-totals captures/native/rich-paired-native.pxd2 30
```

`initial` 读取起点；数字指定帧。输出不生成地址、分配次数或虚构接收时间，也不是 `memory-csv` 的累计统计格式。

## 恢复、生命周期与资源边界

历史只保留事件增量及共享 Allocation，不为每个帧复制完整分配表。`MemoryCursor` 只维护一份当前表；逆向恢复使用事件保存的旧分配，正向恢复使用对应的新分配。较早的随机跳转可从起点重建。没有为尚未接收到的分配伪造历史。

游标检测其已应用的事件是否仍属于给定历史。切换录制、切换到较早的直播快照或切换分叉历史会安全重建；同一直播历史追加数据不要求清空游标。

原版磁盘录制保留独立的录制结束分析根。内存页选择“当前帧”时使用显示帧分析，选择“录制结束”时使用该结束根；定位到早期帧不丢失末帧之后送达的分配和释放。当前帧没有事件时仍可切换查看结束状态，完整 CSV 使用同一尾部范围。

事件对象、事件数组余量、所有历史 Allocation、当前存活索引、字符串和刷新采样均计入与性能分析共用的 **128 MiB** 估算预算；它不是进程 RSS 硬上限。free 只释放当前存活索引的预算，历史 Allocation 仍然计费。最多保留 200 万 allocation/free 事件；预算可能先于条数触发。调用者创建的当前游标表是额外的一份视图，其规模受已保留历史限制，不是每帧历史副本。

完整恢复从起点读取事件，因此远距离随机跳转的开销仍与事件数相关。当前没有宣称大型内存历史跳帧的性能结论；可以在有代表性捕获和测量后添加受预算约束的稀疏索引。

接收帧必须单调不减。事件中途截断的缓冲会报错，已出现的高帧事件不会允许后续低帧事件破坏查询排序。

## 可重复验证

默认真实输入由 `tools/physx-features/heightfield.cpp` 通过官方 PhysX 3.4.2 `eALL` 产生，生成方法见 [HeightField 验证](heightfield.md)。

```powershell
cargo test --locked -p pvd-core --lib analysis::tests
cargo test --locked -p pvd-core --test memory_history
cargo test --locked -p pvd-core --test memory_history -- --ignored --nocapture
cargo test --locked -p pvd-desktop memory::tests
```

真实测试默认读取 `captures/physx-3.4-heightfield.pvdstream`，也可以通过 `PVD_MEMORY_CAPTURE` 指定包含 `eMEMORY/eALL` 的实际 SDK 捕获。这个测试带 `ignore`，默认测试通过不代表真实 SDK 项已运行。

原版磁盘的末尾、早期帧定位和完整分析配对入口见[验证说明](validation.md)。构造回归单独覆盖仅在最后完成帧之后送达的分配，不将它移入最后一个场景帧。

来源总量的真实配对入口需要 `captures/native/` 下同次 rich 原版文件与 SDK 流，可用 `PVD_NATIVE_MEMORY_DIRECTORY` 指定输入目录：

```powershell
cargo test --locked -p pvd-core --test native_memory -- --ignored --nocapture
```

该专项将 SDK 存活地址独立按完整类型、路径和行号分组，核对原版各帧及末尾的来源值。非空起点、负值、旧缓冲不重播、生命周期、取消和预算另有构造回归，不替代原版非空起点或原版显示配对。

CLI 与桌面构造专项需要先生成对应容器：

```powershell
$env:PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY = Join-Path (Get-Location) 'output/native-memory'
cargo test --locked -p pvd-core --lib native::memory_totals::tests -- --include-ignored --test-threads=1
cargo test --locked -p pvd-cli -p pvd-desktop constructed_native_memory_totals -- --ignored --test-threads=1
```

生成器写出零帧起点、含帧起点及普通创建三份输入。界面专项检查双语显示、定位后保持所选模式、同帧异步恢复、关闭与隐藏面板缓存释放，以及缺前史时的导出提示。

无窗口界面验证可使用 `--render-check <录制文件> <输出图片> <帧号> memory-groups`；该入口等待当前帧分析及分组完成，分组失败时返回错误。其他渲染参数沿用[验收方法](acceptance-plan.md)。

来源总量对应 `memory-totals`、`memory-totals-initial` 和 `memory-totals-end` 三种离屏面板；没有捕获值时返回失败，零帧的当前位置使用起点。

PhysX 3.4.2 真实样例的 76 个刷新帧、2,172 次分配、1,079 次释放和 506 次地址复用已与独立事件重放核对。Churn 逐帧与完整存活表筛选结果比较；构造输入另行覆盖同帧多次刷新、覆盖和滚动保留。

PhysX 4.1.2 内存样例由独立生产器生成，再核对前进、逆序和随机恢复。3.4 的结果不能代替其他 SDK 或平台的验收。

界面应将“当前接收帧的存活分配”“本帧事件”与“全捕获累计统计”分别命名。此结果不是逐事件 CPU 时间、精确物理帧内分配时间或操作系统总内存的测量。
