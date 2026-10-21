# 内存事件历史与当前接收帧

内存分析保留真实 PVD allocation/free 事件，并可恢复任意 `received_frame` 的已观察存活分配。`received_frame` 是包含事件的内存缓冲在录制中被接收到的帧编号，**不是每次分配/释放实际发生的时刻**。SDK 批量刷新和退出时的最后刷新可能与可见场景帧不同。

## 核心接口

接口位于 `pvd_core::analysis`，不依赖 Bevy 或 egui：

| 接口 | 返回内容 |
| --- | --- |
| `MemoryAnalysis::state_at_frame(frame)` | 从事件起点恢复该接收帧及此前全部事件后的 `MemoryState` |
| `MemoryAnalysis::seek(&mut MemoryCursor, frame)` | 在可重用游标上前进、后退或跳转，返回当前 `MemoryState` 引用 |
| `MemoryAnalysis::events_at_frame(frame)` | 恰在该接收帧到达的 allocation/free 事件切片，包括同帧多次缓冲刷新 |
| `MemoryAnalysis::churn_at_frame(frame)` | 本帧分配/释放次数与字节、未知释放数、未释放地址覆盖数及被覆盖字节 |
| `MemoryAnalysis::events()` | 按接收顺序保存的全部事件，只读 |

`MemoryState` 包含以 `(buffer ID, address)` 为键的存活分配、存活字节，以及截至该接收帧的分配/释放/未知释放/地址覆盖计数、总分配字节与已知总释放字节。Allocation 保留其原始 type、file、line、size 和分配事件的接收帧；已释放条目也可从事件列表中检查。

`MemoryEventKind::Allocate { allocation, replaced }` 保存新分配及可能被覆盖的旧分配。`Free { allocation }` 在匹配到已观察分配时保留该分配，否则为 `None`。分配和释放共享同一个 `Arc<Allocation>`，字符串句柄重新定义不会改变历史分配的 type/file。

地址复用与没有先释放的地址覆盖有区别。不同 buffer ID 下的相同地址互不覆盖；未知 free 计入事件数，但不会捏造大小。`MemoryChurn::live_byte_change()` 为分配字节减已知释放字节，再减覆盖掉的旧分配字节。未知 free 不能用于推算完整进程内存变化。

## 恢复、生命周期与资源边界

历史只保留事件增量及共享 Allocation，不为每个帧复制完整分配表。`MemoryCursor` 只维护一份当前表；逆向恢复使用事件保存的旧分配，正向恢复使用对应的新分配。较早的随机跳转可从起点重建。没有为尚未接收到的分配伪造历史。

游标检测其已应用的事件是否仍属于给定历史。切换录制、切换到较早的直播快照或切换分叉历史会安全重建；同一直播历史追加数据不要求清空游标。

事件对象、事件数组余量、所有历史 Allocation、当前存活索引、字符串和刷新采样均计入与性能分析共用的 **128 MiB** 估算预算；它不是进程 RSS 硬上限。free 只释放当前存活索引的预算，历史 Allocation 仍然计费。最多保留 200 万 allocation/free 事件；预算可能先于条数触发。调用者创建的当前游标表是额外的一份视图，其规模受已保留历史限制，不是每帧历史副本。

完整恢复从起点读取事件，因此远距离随机跳转的开销仍与事件数相关。当前没有宣称大型内存历史跳帧的性能结论；可以在有代表性捕获和测量后添加受预算约束的稀疏索引。

接收帧必须单调不减。事件中途截断的缓冲会报错，已出现的高帧事件不会允许后续低帧事件破坏查询排序。

## 可重复验证

默认真实输入由 `tools/physx-features/heightfield.cpp` 通过官方 PhysX 3.4.2 `eALL` 产生，生成方法见 [HeightField 验证](heightfield.md)。

```powershell
cargo test --locked -p pvd-core --lib analysis::tests
cargo test --locked -p pvd-core --test memory_history
cargo test --locked -p pvd-core --test memory_history -- --ignored --nocapture
```

真实测试默认读取 `captures/physx-3.4-heightfield.pvdstream`，也可以通过 `PVD_MEMORY_CAPTURE` 指定包含 `eMEMORY/eALL` 的实际 SDK 捕获。这个测试带 `ignore`，默认测试通过不代表真实 SDK 项已运行。

实际执行结果：

PhysX 4.1.2 内存样例由独立生产器生成，再核对前进、逆序和随机恢复。3.4 的结果不能代替其他 SDK 或平台的验收。

界面应将“当前接收帧的存活分配”“本帧事件”与“全捕获累计统计”分别命名。此结果不是逐事件 CPU 时间、精确物理帧内分配时间或操作系统总内存的测量。
