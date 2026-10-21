# 普通 Raycast：来源与已验证范围

`pvd_core::queries::collect(&SceneState)` 读取当前帧捕获的 `SceneQueries.Raycasts` 与 `SceneQueries.Hits`，返回射线、命中和诊断。它不在客户端重新执行碰撞查询，不从画面几何推算命中。

## 序列化证据

已检查固定官方 PhysX 3.4.2 源码 commit `5e42a5f112351a223c19c17bb331e6c55037b8eb`，并使用 4.1.2 commit `a2c0428acab643e60618c681b501e86f7fd558cc` 做独立运行验证：

| 源文件 | 证据 |
| --- | --- |
| `PhysX_3.4/Source/PhysX/src/NpPvdSceneQueryCollector.h` | `PvdRaycast`、`PvdReference`、`PvdSqHit` 字段定义；命中由真实 `PxRaycastHit` 复制 |
| 同目录 `NpPvdSceneQueryCollector.cpp` | 普通查询数组名称；kind 的 any/closest/all 选择；命中 baseIndex/count 对共享 Hits 数组的引用 |
| 同目录 `PvdMetaDataPvdBinding.cpp` | 字段注册顺序；`ScopedPropertyValueSender` 分块发送；发送前将无效 Actor/Shape 置空 |
| `PxShared/src/pvd/src/PxPvdObjectModelMetaData.cpp` | 指针类分别使用 4/8 字节；字段按源类型对齐；结构尾按最大对齐补齐 |
| `PxShared/src/pvd/src/PxPvdDataStream.cpp` | `bufferPropertyValue` 将**每条记录**的本体与该记录的 NUL 终止字串依次写入，不是将所有字符串放在整个数组末尾 |
| `PhysX_3.4/Include/PxQueryReport.h` | `PxHitFlag` 决定 position/normal/UV/faceIndex 是否有效；distance 总是计算 |

由以上规则得到的线数据布局如下，全部使用生产端 header 的指针宽度和端序。Rust 程序自身的 `sizeof` 和主机指针宽度不参与判断。

| 记录 | 32 位本体 | 64 位本体 | 关键字段偏移 |
| --- | ---: | ---: | --- |
| `physx3::PvdRaycast` | 64 字节 | 72 字节 | type 0、filterData 4、filterFlags 20、origin 24、direction 36、distance 48；hit-name 指针 52/56，base 56/64，count 60/68 |
| `physx3::PvdSqHit` | 52 字节 | 64 字节 | Shape 0、Actor p；face 2p、flags 2p+4、impact 2p+8、normal 2p+20、distance 2p+32、u 2p+36、v 2p+40；p 为源端指针宽度 |

Raycast 本体后紧跟 hit-array 名字的 UTF-8/NUL 字符串。记录中原始字符串指针仅作为布局字节跳过，绝不解引用。对象引用转为核心 `Value::Ref`，在事件发生时记录代际，避免命中引用被接到复用 ID 的新对象。

## API 与限制

`QueryScene` 包含 `raycasts` 和 `diagnostics`。每条 `Raycast` 提供 scene ID、帧内 index、query kind、过滤数据、origin、direction、最大 distance 与 hits。命中位置、法线、面索引和 UV 是 `Option`，只在对应 SDK flags 声明有效时返回。

Actor/Shape 引用包含原始 ID 与 `Option<generation>`；缺失、被销毁或代际不符时 generation 为空，调用方不能把它跳转到同 ID 的当前对象。SDK 已置空的引用也保持为空；仍然可以显示有效的命中位置。

- 已支持普通 `SceneQueries.Raycasts`；实际 SDK 样例验证的是 closest hit 和 miss。any/all 的 kind 与相同结构已按源代码解码，专门的运行样例尚未覆盖。
- Batched raycast、Sweep、Overlap 暂不做可视化解释；发现非空数据会给出诊断。它们不能用普通 raycast 的结构猜测。
- 每个属性最多 100000 条记录；显示总量最多 100000 条射线及 100000 个被引用命中。重复的命中范围也计入显示预算。
- 精确检查本体长度、逐记录字符串终止、256 字节名字上限、尾部多余字节、命中范围与整数溢出。未知 hit-array 名字不猜测；非有限坐标、非单位方向与越界命中距离通过诊断提示。
- 32 位和大端有结构/边界自动化测试，但没有对应平台的真实 SDK 输出，不能据此宣称已实测兼容。

## 独立 SDK 真值

生产器 `--query-truth PATH` 直接保存 `PxScene::raycast` 返回的 `PxRaycastBuffer`，不经过 pvd-studio。每帧含地面、动态 Box、动态 Sphere 三条命中射线及一条向上的 miss；生命周期模式保留 Sphere 销毁和 ID 复用。原有位姿 CSV 格式不变。

```powershell
.deps/physx-sample-build/Release/pvd-physx-sample.exe --file captures/physx-3.4-query.pvdstream --query-truth captures/physx-3.4-query.query.json --frames 120 --flags debug --lifecycle
cargo test --locked -p pvd-core --test query -- --ignored --nocapture
```

测试默认使用上述文件，也可以设置 `PVD_QUERY_CAPTURE` 指向另一录制，同名 `.query.json` 为独立真值。它逆序读取全部 120 帧，比较全部 480 条 raycast 和 360 个命中的位置、方向、长度、flags、Actor/Shape 及引用代际，然后验证顺序重建与检查点回放一致。

实际通过的输入：

| SDK | 输入 | 结果 |
| --- | --- | --- |
| 3.4.2 checked | `captures/physx-3.4-query.pvdstream` 与 `.query.json` | 480 射线、360 命中全部一致；录制 457660 字节 |
| 4.1.2 checked | `captures/physx-4.1-query.pvdstream` 与 `.query.json` | 同样 480 射线、360 命中全部一致；由 4.1 官方 SDK 独立生成，录制 435142 字节 |

3.4.2 录制 SHA-256：`f2d2705b9bd81a823f1d10b32d35d5276b23fbe7d149975633437d93eaaf81d7`；独立 JSON：`1746ef316c365010f5d64439c05727d36ebc1a3b56ac14baa1f43b8628801fae`。固定文件身份用于复核，每次生成的地址和计时可能不同。

桌面已接入射线、命中点、法线三个开关及诊断；真实查询录制已离屏渲染。原版 PVD3 的查询显示行为与实际鼠标交互仍未人工对照。
