# 碎片化输入的协议测量

`Decoder::feed_inner` 先累积完整事件组，满足组长度后才进入 `Reader::event`。未完整时读取组长度并追加新字节，不反复解析已到达的事件。场景对象多、文件大，不等于单个事件组大。

## 测量方法

[`tools/measure-fragmented-parser.ps1`](../tools/measure-fragmented-parser.ps1) 构建正常 Release example，再把协议源码复制到输出目录，向副本的事件读取、u32 读取和输入追加点插入原子计数。副本由已有 `rustc` 独立编译，生产协议源码不变。带计数版本只用于工作量计数，其耗时不用于性能比较。

正常版本对同一文件按 64、1,024、8,191、65,536 和 1,048,576 字节分块重复运行。feed 计时包含事件分配，不包含文件 I/O、返回后的事件释放、核心状态装配、磁盘历史或显示。缓冲扩容搬移与输入追加计数是不同成本。

## 正确性和资源边界

每种分块都应逐组比较偏移、stream ID、时间戳和完整事件字段，并覆盖初始化、最大组和文件末尾的截断，以及超限长度、非法事件数和未知类型。损坏错误及偏移需与基准一致。

在半组处停止送入并释放 Decoder，只验证调用者放弃解析器。Decoder 不提供 feed 内部的异步取消；加载器在读块之间处理取消。

默认单组上限 64 MiB、单 blob 32 MiB，数组元素与组内事件数各最多一百万。feed 返回的组由调用者持有，大量完整组仍可能产生较大的返回集合。小组样例不能替代接近组上限的内存峰值与取消延迟验证。

## 运行

先按 [CCT 生产器说明](cct-live-history-fixture.md)生成输入，或传入其他真实 SDK 流：

```powershell
./tools/measure-fragmented-parser.ps1 -Capture captures/physx-3.4-cct-large.pvdstream
cargo run --locked --release -p pvd-core --example fragmented_live -- captures/physx-3.4-cct-large.pvdstream output/fragmented-parser/history 2 1024,8191,65536
```

第一条命令输出一致性、截断、损坏和计数结果；加 `-IncludeLive` 可测带磁盘历史的连续模式。临时历史受默认 8 GiB 预算和 512 MiB 剩余空间保护，正常释放后清理会话文件。该基准不执行 TCP 传输或窗口渲染。
