# 原版 PVD3 文件格式调查

## 通信目录

通信块布局由匹配[参考文件身份](evidence.md)的 EXE/MAP 调用链和三份真实保存文件核对。表中 VA 为首选映像地址，静态与文件证据不能替代原版运行互验。

| 目录 | 布局与含义 | 证据及边界 |
| --- | --- | --- |
| 当前源ID映射 | u32数量，每项`u64 source + u32 local_id` | save `0x1400C3B80–0x1400C3BC5`；destroyInstance `0x1400BFAB0`移除source键。它是当前映射，不是完整代际历史 |
| 具名相机目录 | u32数量，每项`u32 name_StringHandle + u32 local_id` | setCamera `0x1400C40C0–0x1400C43B0`创建/更新相机实例。三份真实表均为空；非空解析与回放有静态证据及构造覆盖，见[相机与显示属性](native-settings.md) |
| Section索引 | 每行`u64 begin_ticks, begin_event, end_ticks, end_event` | serializer `0x1400BCBD0–0x1400BCD60`，addSectionEvent `0x1400BDF20–0x1400BE240`；246个真实帧行与type-8起止事件相符。开放/推断边界未由这些样例证明 |
| 内存事件实例 | 两个u32 local ID，缺失值`0xffffffff` | save `0x1400C3CDA–0x1400C3D0A`；setMemoryEventInstances `0x1400C4530`。baseline为两个缺失值，rich/limits为`[1,3]`；内部实例不一定出现在SDK源ID映射 |
| 内存名称目录 | 数量及`type_string, file_string, u32 line, u32 index` | addMemoryEventEntry `0x1400BDD00–0x1400BDDD0`，save `0x1400C3D31–0x1400C3E03`；三样例分别0、273、224项。这一尾部是最终内存名称目录，不是显示设置 |

Profile目录中的opaque u64仍保持未知；名称目录都只代表保存时内容，不能推导名称生效历史。受限的整份重写与区间裁剪由独立写出入口提供，范围和验证方法见[原版容器保存](native-export.md)。内部字节重建和回放核对不替代原版打开、倒放和再次保存验证。

## 非空起点快照

当前适配器实现 event=0 的非空初始 snapshot：目录为 u32 数量及每项 u32 实例ID、u64 backing位置；实例记录包含类ID、实例ID、body长度，body为32位布局的固定区及按继承属性顺序排列的数组尾部。先创建全部实例，再恢复字段，允许前向引用。未知嵌套动态 datatype 明确拒绝；目录、实例、数组、值展开均有预算和取消检查。

静态依据是 `writeModelInstance 0x1400E8190`、`writeInstanceData 0x1400E83C0` 和 `readInstanceData 0x1400E5B60`。baseline/rich 保存实例表的字段与 type-10 时刻已传输状态逐项比较；真实非空首快照仍需独立输入。

保存的profile/memory/renderer事件属性只恢复为值，不当作新事件执行；起点之前的性能开始、完整存活分配和错误历史顺序无法由快照补出，导入提供诊断。错误数组保留为后续可追加的值，并提供独立的[起点已有错误列表](native-errors.md)，不伪造接收帧或事件编号。自写两页、8事件、3帧用例验证跨页更新/销毁/数组和正反随机回放；它不是原版实际输出。

原版自身保存的非空首快照、中段裁剪和多页文件仍需配对采集。项目导出的受限中段不能替代这些来源证据；摘要与源文件相同的副本不能作为裁剪证据。页数量以实际目录为准，不能由文件名或预计帧数判断。

适配器读取 PVD3 数据库 v1 的有界子集。已有原版保存文件及同次 SDK 流用于核对对象、属性、几何、生命周期、分析、错误和回放。绘制的真实配对覆盖 JointFrames、四类限位、RGB 三角形及同帧多次提交，见[原版绘制](native-debug-draw.md)；这不代表完整格式或双向读写兼容。

局部静态调查使用 objdump/strings、dumpbin 和匹配符号，结果按事实、推断和未验证项区分。符号、虚表和函数体不能单独证明完整私有结构或运行行为。

原版实际保存/重开与静态分析分开验收，配对方法见[原版验证](native-validation.md)。

## 输入身份与工具

参考文件由使用者自行准备，身份见[证据登记](evidence.md)。

| 文件 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `PVD3.exe` | 2206608 | `8a357c845b2622b19ca4c391086de83c9de2e45787d518d972f2490f1c524420` |
| `PVDNext.map` | 5246331 | `be12ee27da57aae194ff4fe14aa86be5d203a76e5b8e65eeb6fcc6339c39c6d5` |
| `PVDNext.pdb` | 3117056 | `9f94a265e38dded40343da5cb957a0b63125065b821fcb2d9db276d98cde301e` |

EXE/PDB 的 RSDS GUID/Age 见[文件身份](evidence.md)：`795b547e-7ade-467a-942d-4c56e41d2824` / `1`。DIA 只读查询再次得到匹配 GUID/Age。PDB 字符串中可见 `PvdFileStreamLoader`、`PvdObjectModelEventStreamMetaData` 等名字；一个 `FileHeader` 命中位于 `_IMAGE_NT_HEADERS64` / `_IMAGE_FILE_HEADER` 的 PE 类型上下文，**不能当作录制容器头证据**。

静态复核需要可用的 objdump/strings 和 dumpbin。可选 DIA 工具通过指定 DLL 的 `DllGetClassObject` 读取 PDB，不需要 COM 注册。匹配 GUID/Age 不保证包含 `PageFileData`、`SnapshotInfo` 等私有类型；PE `FileHeader` 类型也不能作为录制容器头的依据。

以下 VA 是 PE 的首选映像地址，不能直接作为运行时地址。EXE `.text` 为 VA `0x140001000`、文件偏移 `0x400`；`.rdata` 为 VA `0x14010A000`、文件偏移 `0x108600`。函数范围取自 MAP 相邻入口及实际 `ret`，不把相邻函数的内容算入同一函数。

## 已观察：SDK 文件传输器输出原始流

固定官方 SDK 3.4.2 提交为 `5e42a5f112351a223c19c17bb331e6c55037b8eb`。以下文件相对于 `.deps/physx-3.4`：

| 编号 | 来源 | 可支持的结论 |
| --- | --- | --- |
| F01 | `PxShared/src/pvd/src/PxPvdDefaultFileTransport.cpp:40–42,66–76,215–220`；`PxShared/src/filebuf/include/PsFileBuffer.h:60–61,130–144` | 文件名直接传入 `PsFileBuffer`；以 `wb` 打开，输入字节交给 `fwrite`。此路径无按扩展名分流、索引或外层容器封装。 |
| F02 | `PxShared/src/pvd/src/PxPvdImpl.cpp:121–140,232–237`；`PxPvdCommStreamEvents.h:294–347` | 连接后首先发送 28 字节 `StreamInitialization`，之后为 24 字节 event-group 头和事件。文件与 TCP 共用该初始化。 |
| F03 | `PxShared/src/pvd/src/PxPvdCommStreamTypes.h:120–128,155–173`；`PxPvdDefaultSocketTransport.cpp:72–97` | 字段按源端原生字节序逐项写入，TCP 直接发送相同字节层级。 |
| F04 | PhysX 3.4.2 SDK 文件传输器的参考输出 | 以 `.pxd2` 文件名产生的 120 帧输入仍以 communication v1 初始化。名称不能证明它是原版保存数据库。 |

F04 的前 28 字节：

```text
82 E8 E5 31  01 00 00 00  00 CA 9A 3B 00 00 00 00
80 96 98 00 00 00 00 00  01 00 00 00
```

该 SDK 头部的 u32 magic 是 `0x31E5E882`，version 是 `1`；后面是两个 u64 时间换算数值和 u32 flags。完整线协议边界见 [protocol.md](protocol.md)。手册的 `.pxd2` 示例与这个文件方向一致，但 SDK 源码不能单独说明 PVD3 应用保存后的文件布局。

## 已观察：原版按签名分开载入两种数据

| 编号 | MAP 行 / VA | 局部函数体观察 |
| --- | --- | --- |
| F05 | 2486 / `0x1400B7CA0`，`PvdCommStreamEventReader::isPvdStream` | 先比较 `0x31E5E882`；不等时交换输入 u32 的四个字节再比较。这个辅助函数认可正反字节序的 SDK magic。 |
| F06 | 12488 / `0x1400C49E0`，`LoaderFactory::getFileType` | 调用 F05；成功返回整数 `1`。否则比较 `0x36101BB0`，相等返回 `2`，其他输入返回 `0`。枚举名称未从 PDB 类型记录恢复。 |
| F07 | 12485 / `0x1400C4870`，`LoaderFactory::createFileStreamLoader` | 读取输入前 4 字节，调用文件类型判别；此函数仅在结果为 `1` 时创建 loader，后续直接调用 `PvdClientMutator::create` (`0x1400BA7A0`) 和 `PvdCommStreamEventReader::create` (`0x1400B5190`)。 |
| F08 | 2396 / `0x1400B0960`，`PvdEngine::load` | 文件首 u32 为 `0x31E5E882` 时走临时 backing-file/loader 路径，循环调用 loader，每次参数 `0x80000`；为 `0x36101BB0` 时直接调用 `PvdClient::load` (`0x1400BB140`)。其余值返回失败路径。 |

F06 的关键指令：

```text
1400c49ea  call  0x1400b7ca0
1400c49f3  mov   eax,0x1          ; isPvdStream 返回真时
1400c49fe  xor   eax,eax
1400c4a00  mov   ecx,0x2
1400c4a05  cmp   ebx,0x36101bb0
1400c4a0b  cmove eax,ecx
```

这确认至少存在两条不同的二进制格式路径。注意 F08 的引擎文件分派只观察到直接比较原生字节序的两个常量，不能因 F05 有字节交换就声称原版顶层打开路径支持所有大端文件。

## 已观察：`.pvd` ZIP 包装入口

匹配的 EXE/MAP 中，文件对话框、拖放和文件参数均调用 `PVDMainWindow::open`。该函数先检查后缀：`pvd` 进入 `OpenZip → GetZipItem → UnzipItem`，直接 `pxd2` 则交给 `PvdEngine::load`。ZIP 分支遍历成员，选择后缀精确为小写 `pxd2` 的文件，解包后再调用同一引擎入口。因此底层的两个内容签名不能排除上层 ZIP 包装。

ZIP reader 的实际指令检查本地头、中央目录和末尾目录签名，并处理 stored/deflate。原版主窗口会解压所有成员；多个匹配时后面的条目覆盖先前选择。这些是静态控制流证据，不能替代原版实际生成包的打开、回放和重存配对。

项目提供唯一录制成员、有限大小和严格结构校验的[录制包读取](pvd-archive.md)，不提取附属文件、不写出 ZIP。内层仍受已支持 SDK stream 或 PVD3 database 的各自边界约束；旧 `.pxd` 没有由此获得格式证据。

## 已观察：Save 入口与容器前缀

| 编号 | MAP 行 / VA | 已核对的调用或字节 |
| --- | --- | --- |
| F09 | 298 / `0x1400262C0`，`PVDMainWindow::onSave` | 引用 VA `0x14016D7C0` 的 `Clip Files (*.pxd2)` 和 `0x14016D7D8` 的 `Save clip` 字符串；打开随机访问输出流后，于 `0x14002646A` 直接调用 `PvdEngine::save`。 |
| F10 | 2401 / `0x1400B1110`，`PvdEngine::save` | 转发到 client 虚表 `+0xA0`。`Client` 虚表 VA `0x140115248` 的该槽位实际内容为 `0x1400BB6C0`。 |
| F11 | 12380 / `0x1400BB6C0`，`Client::save` | 依次写出四个 u32：`0x36101BB0`、`1`、第一块元数据的字节数、第二块元数据的字节数；随后复制两块数据，调用 `zeroPadToPageSize(0x1000)`，再调用对象事件历史的 `save`。 |
| F12 | 12363 / `0x1400BAE60`，`Client::load`；2539 / `0x1400BB140` 为跳转入口 | 顺序读取上述四个 u32，检查第一个 magic，按后两个长度读取两块缓冲。第一块传给 `PvdObjectModelMetaData::create`，第二块传给 `PvdCommStreamMetaData::create`。然后计算下一 4096 字节边界并调用 `PvdObjectModelEventStream::load`。 |
| F13 | 12249 / `0x1400AED40`，`RandomAccessIOStreamImpl::zeroPadToPageSize` | 算式是 `pad = page_size - (position % page_size)`，即位置已对齐时仍补一整页。F12 同样用 `position - (position & 0xFFF) + 0x1000`。不能换成“已对齐则不动”的通常 align-up。 |
| F14 | 13901 / `0x1400EE680`，`PvdObjectModelEventStreamImpl::save` | 调用 `PvdObjectModelEventStreamMetaData::save` (`0x1400EE720`)，再按 4096 字节补齐，并从 backing stream 的保存起点复制后续字节。尚未还原其中所有页、事件和索引字段。 |

F11 的两块元数据调用还通过原版虚表槽位复核：

- 对象模型元数据表 `0x140112378 + 0xC8` → `0x1400A0CA0` (`PvdObjectModelMetaDataImpl::write`)。
- 通信元数据表 `0x140115D08 + 0xF0` → `0x1400C3AC0` (`StreamMetaData::save`)。
- 对象历史表 `0x14011AE88 + 0x58` → `0x1400EE680` (`PvdObjectModelEventStreamImpl::save`)。
- 随机访问输出流表 `0x140113F78` 的 `+0x08/+0x30/+0x38` 分别对应 `write`、u64 长度的 `directCopy`、`zeroPadToPageSize`。

针对**这份 x64 EXE 的该保存路径**，下列前缀已与两份真实保存文件一致：

| 文件偏移 | 类型/范围 | 字节含义与证据边界 |
| --- | --- | --- |
| 0 | u32 LE | `0x36101BB0`，字节 `B0 1B 10 36` |
| 4 | u32 LE | 保存器写 `1`。很可能是版本字段；在已查看的 `Client::load` 中未发现对该字段的判定，语义仍需类型记录或样例确认。 |
| 8 | u32 LE | `A`：对象模型元数据字节长度 |
| 12 | u32 LE | `B`：通信元数据字节长度 |
| 16 | A 字节 | 对象模型元数据序列化内容 |
| 16 + A | B 字节 | 通信元数据序列化内容 |
| 16 + A + B | 零填充 | 补到严格大于当前位置的下一 4096 边界 |
| `floor((16+A+B)/4096)*4096+4096` | 后续历史区 | 对象事件流元数据、再次分页及 backing 数据；已实现的范围见下文 |

可以确认“应用保存路径不只是将原始 TCP 字节复制到另一个扩展名”。不能确认 `.pxd` 的所有历史格式、其他 PVD3 版本、不同架构和不同保存选项都使用这个布局。

## 文件名与导出库的边界

EXE ASCII 字符串还包含 `*.pxd3`、`pxd3`、`backingFile`；MAP 将 `pxd3` 放在 `PvdEngine.obj`。这与临时 backing-file 路径是值得继续核对的线索，**不构成公开 `.pxd3` 兼容声明**。打开过滤器包含 `Clip Files (*.pvd *.pxd2)`，对应上文已核对的 ZIP 包装和直接文件入口。它与手册列出的 `.pxd/.pxd2` 不完全相同；`.pvd` 包装不能作为旧 `.pxd` 读取证据。

`dumpbin /exports PVD3.exe` 只列出一个导出：`NvOptimusEnablement`。`dumpbin /linkermember:1 PVDNext.lib` 只列出相应导入描述符、null descriptor/thunk 和 `__imp_NvOptimusEnablement`。所以这份 `.lib` 不是可直接链接的 PVDCore 读写 SDK；当前没有由它提供的、可在控制台无窗口调用的官方转换 API 证据。

## 真实保存文件与验收范围

以下为格式调查使用的参考配对输入，不随仓库分发。它们由固定 PhysX 3.4.2 的 120 帧 baseline/rich 场景经 TCP 代理送入 PVD3，再由原版 Save 保存；采集和约定目录见[原版验证](native-validation.md)。

| 样例 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `baseline-paired-native.pxd2` | 342511 | `d391fb28d7f8a9f319a2885e82b38e049eda2df50c0b1e0261bb286b19ecb67a` |
| `baseline-paired.pvdstream` | 409900 | `16c8951055d47b534334353148e88774b0b95322d6545e24f45e1240c037d552` |
| `baseline-paired.csv` | 19326 | `0f97db77014f70ce187facd2a9c062f6898495d221d2d22cdd589401dc221990` |
| `rich-paired-native.pxd2` | 1034096 | `7daa115e9da7ee417340775c853017d1c38b1d72e6b416d51318c07855891f48` |
| `rich-paired.pvdstream` | 939063 | `6f8b90f9fc788f1b7d53551b300610002af28072f28116c642fe794da0860bae` |
| `rich-paired.csv` | 38818 | `aecb412085c59d952b977d78ca791e67d7190db4086bfb3d651bde5708e079d4` |

`crates/pvd-core/tests/native.rs` 的实际结果：6 项集成测试通过，其中 5 项依赖原版真实文件、默认 ignored，需显式 `--include-ignored`；另有 10 项 native/analysis 专项单元测试验证字符串展开、目录冲突、错误布局/顺序、截断、取消和分配预算。baseline 对照 50,880 项语义属性和独立 SDK CSV 的 240 组位姿；rich 对照 70,680 项属性和 480 组位姿。每一帧都按打乱顺序恢复，与同次 TCP 的场景时间戳、对象引用、属性值、场景几何、网格顶点/索引、接触记录、睡眠状态比较，另跑顺序重建与检查点跳转的一致性验证。名称变化、对象销毁和源指针复用包含在这两份生命周期捕获中。opaque 字节及原版内部生成的 `MemoryEventTotals` / `UserCustomedEvent` 不纳入“属性一致”数字；性能/内存有独立的逐事件验证，见下节。

源指针复用会在原版分配新的内部 ID，例如 baseline 的旧 sphere / replacement 是内部 11 / 14。导入保留这些容器 ID；不会将两个生命周期合并回一个源指针。通信元数据中的 local→source 对照仅用于诊断和外部验证。

附加实测：原版 Open 现有 `captures/physx-3.4-sdk-file.pxd2` 显示 `This file is corrupted`，同版本 SDK 的实时 TCP 正常。这是特定文件/打开路径的差异，原因尚未确定，不能因此声称所有 SDK 原始流都能被原版文件打开。

## 历史布局与解析证据

| 字段/事件 | 静态证据 | 真实文件核对 |
| --- | --- | --- |
| 对象元数据 | 原版 `write`，固定 SDK `PxPvdObjectModelMetaData.cpp` 的 writer/read/各 serialize | 完整消费 baseline 103981 B、rich 148990 B，所有字符串、属性、类和消息块边界相符；保存模型中的引用 / StringHandle 是 4 B 内部值，即使 EXE 与 SDK 为 x64。 |
| 历史元数据 | `save 0x1400EE720` / `load 0x1400ED7B0` | u64 总事件数；u32 数量 + 24 B PageFileData；u32 数量 + 24 B SnapshotInfo；u32 数量 + 16 B `{i32 id,u64 position,u32 length}` 实例位置表。 |
| PageFileData | `writeOutPage 0x1400F4BE0` 与 `beginAddEvent 0x1400F0F90` | `{u64 first_event,u64 position,u32 event_count,u32 byte_length}`；position 相对第二次分页后的 backing 起点。 |
| SnapshotInfo | `saveSnapshot 0x1400F3950` 与元数据 `save 0x1400EE720` | `{u64 event,u64 position,u32 byte_length,4 B padding}`；已核对 x64 构建按 24 B 整体保存，尾部填充没有初始化，写出原样保留。真实 baseline/rich 的该值均为 492，不能要求为零。 |
| 页内容 | `writeOutPage` | u32 count + count 个 u32 偏移 + 连续事件字节；首偏移 0，严格递增，末边界取页有效长度。baseline 一页 2602 事件；rich 一页 3457 事件。 |
| 正向应用 | `visitPvdObjectModelEventType<EventDoOperator> 0x1400E8B20` | 1 创建、2 销毁、3 属性、4 属性消息、5 插入、6 删除；3/4 的字节块后还有 u32 count + u32 undo 引用。7 数组元素更新有静态代码实现，当前两真实样例无该类型，不列实测。 |
| 8 用户事件 | `addUserEvent`，实际动态元数据 | 本样例全部为 `_debugger_::SectionEventData`，24 B `{u64 source_stream,u64 ticks,u32 string_handle,u8 begin,3 B padding}`；240 条组成 120 帧。其他用户事件类型明确拒绝。 |
| 9/10 存储辅助 | `EventDoOperator` 中 9 直接返回，10 只消费 i32 ID + u64 位置 | 导入从 event=0 的初始快照顺序重建自己的检查点，不使用原版 undo 来“倒减变换”。非空初始快照的受支持布局可恢复；真实原版非空起点与多页录制仍待配对验证。 |
| 通信实例映射 | `StreamMetaData::save 0x1400C3AC0` → `0x1400BC860` | v1 + 两个 u64 时钟比例 + count 个 26 B 记录；4 B local key、8 B source、4 B local ID、两个 u32 与两个 bool。source→local 映射用于目录身份和帧归属；可选颜色、拾取和顶层标记按[显示属性](native-settings.md)导入。 |

查询记录中原版已把字符串转换为 StringHandle。适配器按已核对的 PvdRaycast / PvdSweep / PvdOverlap 的 32 位布局与元数据指针表恢复字符串，复用现有查询解码器；结果逐帧对同次 raw 查询值核验。不存在重新模拟。

六个 error 元数据属性的 semantic 引用指向字符串表下一 handle，表中缺失；原版 SDK `handleToStr` 对未知 handle 返回空。适配器对这种 semantic 缺失保留数量诊断；类名、属性名和实际属性 StringHandle 缺失则拒绝，不能将其扩展为任意缺失字段可忽略。

## 性能和内存分析

rich 配对文件包含 `_debugger_::PxProfileZone` 实例 2 的 `events`（属性 50）、`_debugger_::PxProfileMemoryEventBuffer` 实例 1 的 `events`（属性 51）。两者均验证为 `physx3::PvdU8` 数组、元素宽度 1，然后在历史事件顺序中送入现有 Analysis 解码器。缓冲边界保留；回放不运行 PhysX。

性能缓冲 240 次更新（120 非空、120 空清除）、共 246,961 B，与同次 TCP 按接收帧逐字节一致。时钟比例和 120 个完成帧 ticks 一致。通信元数据由结构顺序读取：实例映射后是 count×12 B 表、count×8 B 表、section 表，再到 profile zone 表。`SectionListInfo` writer `0x1400BCBD0` 明确写入 section key 和 count×32 B 记录；`ProfileZoneImpl` writer `0x1400BCF70` 明确写入 source key、8 B 字段、名称字符串、local instance ID、事件数组。8 B 字段的业务意义未解释，不作为 source ID。rich 的目录位于通信块偏移 4720–9649，含 1 个 `PhysXSDK` zone 和 140 个 `(名称,u16事件ID,u8启用标记)`，与 raw 定义顺序一致。读取以结构长度和块末校验为准，不搜索字符串猜测偏移。

`addProfileZone 0x1400BDDD0` 会覆盖 zone 的最终名称；`addProfileZoneEvent 0x1400BDE40` 追加事件定义。导入目录只代表**保存时元数据**，不能还原名称生效历史。同一事件 ID 的不同名称会在 Analysis 标为 `metadata_ambiguous`、输出诊断，受影响曲线留空；同 ID 的启用标记冲突明确拒绝。zone 映射重复、缺失或矛盾也拒绝。全部目录存储与辅助映射计入共享元数据预算，名称进入 Analysis 时还计入其 128 MiB 预算。

内存缓冲不是原始流的字节副本。原版 `handleParsedData<AllocationEvent> 0x1400B1780` 调用 `findName 0x1400B6730` 转换类型和文件句柄，再编码分配记录。rich 原始流有 287 条字符串定义，原版保存缓冲中没有这些定义；类型和文件句柄改为容器字符串表句柄。导入按需解析句柄，在分配前收费并按 `(buffer,handle)` 缓存共享字符串，然后复用分配/释放解码器。不能直接把容器 bytes 送入需要流内字符串定义的 SDK 内存入口，也不能用 `MemoryEventTotals` 汇总替代历史。原版缓冲若出现流内字符串定义则明确拒绝。

独立真实配对测试验证：

- 19,215 个完整性能区间逐条匹配 zone source 映射、event、thread、context、start/end ticks 和 received_frame；全部事件名称、数值事件、未匹配 stop、尚未闭合 start 及 CUDA 缓冲计数也对照 raw。
- 通用性能曲线保留 SDK source context，显式选择容器 Scene 时间窗口；14,360 个有值采样以及所有空值状态与同次 raw 一致。参考 Scene 是时间对齐条件，不代表事件属于该 Scene。
- 135 次内存刷新、4321 次分配和 3228 次释放，共 7549 条记录的地址、大小、类型、文件、行号、替换关系、received_frame 全部一致；逐帧存活记录、字节数、计数和 churn 在顺序、反向、随机访问中一致。
- 最后两个内存刷新在 `received_frame=120`，属于最后完成帧之后的录制结束状态；不会塞入第 119 帧。received_frame 仅是刷新接收位置，没有精确的分配发生 ticks。

`RecordingSource::{SdkStream,Pvd3Database}` 明确来源。容器 Scene 使用 local ID（rich 为 6），性能 context 仍保留 SDK source pointer（对应 Scene source 为 `2306271979056`）；禁止全面重映射 context。固定 simulate/collision/solver 阶段依赖唯一且有效的 Scene→SDK context 映射；缺失或歧义时不可用。帧结束间隔和显式参考 Scene 的通用事件曲线使用各自的时序定义。多 Scene、zone/缓冲身份复用、名称重定义发生时间和非空初始快照仍缺真实验收。

## API、限制与后续

- `Recording::from_bytes` / `from_path` 按内容签名分派，扩展名不决定格式。`native::read_with_progress` 使用字节切片；`native::read_path_with_progress` 使用随机读取和配置的磁盘历史。两者返回 `(Recording, ContainerInfo)`，Info 包含源尺寸、表数量、页数、历史事件数及 local→source 映射。核心不依赖 Bevy/egui。
- 字节切片入口上限 512 MiB；文件入口不整文件载入，以 u64 偏移读取目录、页偏移表及单个事件，单次 I/O 最多 64 KiB。磁盘历史须保留完整导入范围，配额不足或发生淘汰时明确失败。超过 512 MiB 的文件在导入前建立完整摘要和分块索引，随机读取及完成后复核源字节，供后续另存校验载入身份。真实大型原版文件仍待验收；大地址构造和追加 padding 的实际文件只验证规模与 I/O 边界。
- 单块 metadata 64 MiB；解码 metadata 与辅助索引各有 64 MiB 分配预算；数组最多一百万、历史最多两千万事件。当前状态、分析与历史分别遵循各自预算，不构成进程总内存上限。未知事件即使边界可知也明确失败。长度、溢出、页顺序、事件偏移、跨边界引用、截断和取消均有检查。单属性及 packed 查询值在展开前检查预算，字符串句柄展开按实际长度收费。属性 owner 必须属于实例本类或祖先类。
- 核心回调可取消，文件读取、目录扫描、元数据注册及事件恢复均有检查。`native::read_path_with_progress` 报告事件进度，`Recording::from_path_with_progress` 将事件比例映射到源字节范围；该值不是磁盘 I/O 吞吐测量。非空初始 snapshot、原版裁剪容器、跨多个历史页的真实样例仍未验收。
- 对象 `events` 原始缓冲仍保留为 Bytes；性能/内存分析按上节范围重建。`UserCustomedEvent.Events` 的已验证 20 B 错误追加记录导入独立 `UntimedErrorEvent` 列表，保留消息、源码位置、Index、容器实例及接收顺序，不添加伪造时间戳或 Actor 关联。真实 rich 的 1 条错误与配套 raw 字段和接收帧一致；非追加数组编辑明确拒绝。详见 [原版无时间戳错误](native-errors.md)。调试绘制的范围见[原版绘制](native-debug-draw.md)。相关限制由 Recording diagnostics 传递到 CLI/桌面。
- `native::export` 提供受限 v1 整份重写和区间裁剪，布局及事件历史边界见[原版容器保存](native-export.md)。SDK 转原版容器未支持；`clip::trim_scene_frames` 是 SDK 流 API，在入口拒绝 native magic。原版写出使用独立入口及 `.pxd2` 扩展名保护。
- `.pxd` 旧格式、SDK 3.0–3.3 / 4.1 保存差异、其他 PVD3 发行版、原版全部分析事件、APEX/OmniPVD 都未由这些样例证明。下一步需要非空 snapshot、长录制多页，以及多 Scene/跨刷新性能事件、可辨识分配回调和错误位置的针对性配对样例。

可重复验证：

```powershell
python tools/pvd-container/inspect_native.py captures/native/baseline-paired-native.pxd2
cargo test --locked -p pvd-core --test native -- --include-ignored --nocapture
cargo run --locked -p pvd-cli -- verify captures/native/rich-paired-native.pxd2
cargo clippy --locked -p pvd-core --all-targets -- -D warnings
```

`inspect_native.py` 仅为布局调查工具，非产品导入器；可选 `--metadata-json` 使用 create-new 方式写入工作区分析文本。DIA 工具需要已有 Visual Studio DIA SDK。将 `$diaSdkDirectory` 和 `$pvdDirectory` 分别设置为对应目录的绝对路径；在开发者终端中可从 `VSINSTALLDIR` 推导 DIA SDK。

```powershell
cmake -S tools/pvd-container -B .deps/pvd-container-build -A x64 "-DDIA_SDK=$diaSdkDirectory"
cmake --build .deps/pvd-container-build --config Release --parallel 2
& ./.deps/pvd-container-build/Release/pvd-symbol-types.exe (Join-Path $diaSdkDirectory 'bin/amd64/msdia140.dll') (Join-Path $pvdDirectory 'PVDNext.pdb') '*PageFileData*' '*SnapshotInfo*' '_IMAGE_NT_HEADERS64'
```

## 可重复的只读命令

在已设置 `$pvdDirectory` 且相关工具位于 PATH 的终端中执行；只读取安装文件。地址与参考 EXE 哈希绑定。

```powershell
$pvdDir = $pvdDirectory
$pvdExe = Join-Path $pvdDir 'PVD3.exe'
$pvdMap = Join-Path $pvdDir 'PVDNext.map'
Get-FileHash -Algorithm SHA256 -LiteralPath $pvdExe, $pvdMap
objdump --version
objdump -h $pvdExe
rg -n 'isPvdStream|getFileType|createFileStreamLoader|\?save@Client|\?load@Client|zeroPadToPageSize' $pvdMap

objdump -d -M intel --start-address=0x1400B7CA0 --stop-address=0x1400B7CDE $pvdExe
objdump -d -M intel --start-address=0x1400C49E0 --stop-address=0x1400C4A14 $pvdExe
objdump -d -M intel --start-address=0x1400C4870 --stop-address=0x1400C49CF $pvdExe
objdump -d -M intel --start-address=0x1400B0960 --stop-address=0x1400B0D6C $pvdExe
objdump -d -M intel --start-address=0x1400262C0 --stop-address=0x140026536 $pvdExe
objdump -d -M intel --start-address=0x1400B1110 --stop-address=0x1400B111E $pvdExe
objdump -d -M intel --start-address=0x1400BAE60 --stop-address=0x1400BB131 $pvdExe
objdump -d -M intel --start-address=0x1400BB6C0 --stop-address=0x1400BB895 $pvdExe
objdump -d -M intel --start-address=0x1400AED40 --stop-address=0x1400AEDAB $pvdExe
objdump -d -M intel --start-address=0x1400EE680 --stop-address=0x1400EE714 $pvdExe
strings -a -t x -n 5 $pvdExe | Select-String -Pattern '\.pxd|backingFile|Clip Files'

dumpbin /exports $pvdExe
dumpbin /linkermember:1 (Join-Path $pvdDir 'PVDNext.lib')
```

这些静态命令不会验证原版能打开任何具体文件，也没有证明所有虚函数调用的动态对象类型。上述关键保存调用额外核对了匹配虚表与加载链；未检查的调用、类型记录和文件区块仍保持未知。
