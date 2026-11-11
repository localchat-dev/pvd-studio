# 原版数据库 v1 无时间戳错误列表

范围为 PVD3 数据库 v1，真实配对依据是 PhysX 3.4.2 rich 场景。其他布局与错误列表生命周期需独立验证。

## 数据模型与语义

`Recording::untimed_error_events() -> &[UntimedErrorEvent]` 返回按容器历史追加顺序保存的记录。字段为：

| 字段 | 含义 |
| --- | --- |
| `code`、`message`、`file`、`line` | 原版错误码、文本与源码位置 |
| `index` | 原版记录的 `Index`，原样保留；不是帧号，不用于猜测时间 |
| `history_event` | 容器中该追加操作的历史事件序号 |
| `received_frame` | 遇到记录时已完成的帧数量；是接收位置，不能当作精确发生帧或某个 Scene 所属关系 |
| `container_instance`、`container_generation` | 保存错误数组的 `UserCustomedEvent` 容器身份及代际；不是受影响 Actor 的 ID |

此类型没有时间戳字段。现有 SDK 原始流的 `ErrorEvent.timestamp` 和 `Recording::error_events()` 保持不变；原始流保存的是事件组时间戳，原版容器不能恢复该数值。两类错误不会通过填入 `0`、帧 ticks 或当前时间混合。

列表保留全部已接收记录，与当前选中的回放帧独立。`received_frame == recording.frame_count()` 表示记录位于最后完成帧之后，不能强行关联最后一帧。界面可展示“时间未知”和接收位置；如提供查看邻近帧操作，须说明这是按历史顺序定位。原始 `Events` 属性也继续保留，按正常检查点/增量回放恢复。

`Recording::initial_untimed_errors() -> &[InitialUntimedError]` 单独提供起点快照中已有的错误。记录保留内容、原始 Index 和容器身份，没有时间戳、接收帧或历史事件号。容器内维持数组顺序及重复记录，不推导跨容器的发生顺序。读取只在文件起点快照恢复时收集，不从第一帧、后续 Create 或当前显示状态反推；零帧文件也可展示。

桌面“起点已有错误”提供详情和复制，不提供帧跳转。CLI `errors` 以 `native_initial_error` 类别输出，`receive_position` 为 `initial-snapshot`。原始 SDK 流不产生这类记录，原版容器不使用项目捕获格式保存。

## 已观察的格式与解析边界

rich 文件的 `_debugger_::UserCustomedEvent` 为 class 104、实例 29；属性 49 `Events` 是 `_debugger_::PvdErrorMessage` 数组。元素 class 103 的 packed 32 位布局为 20 B，字符串句柄位于偏移 4 和 8：

| 偏移 | 类型 | 字段 |
| ---: | --- | --- |
| 0 | u32 | Code |
| 4 | u32 StringHandle | Message |
| 8 | u32 StringHandle | File |
| 12 | u32 | Line |
| 16 | u32 | Index |

适配器按名称、数组种类、元素类、20 B 尺寸、无基类、字符串位置表，以及全部五个字段的名称/类型/偏移验证布局；没有把样例 class/property 数字写死。属性 owner 仍须匹配目标实例或祖先类。

目前支持历史 kind 5 向该数组末尾追加单条 20 B 记录。数组替换、插入中间位置、删除、更新和未知字段布局明确拒绝，因为这些行为尚无真实样例验证其错误列表语义。普通对象或其他属性仍走其原有解析路径；不会因为字段名类似 `Events` 就把它解释为错误。

匹配符号及局部静态证据：`sendErrorMessage 0x1400C3E20` 创建/取得 `UserCustomedEvent`，调用 `pushBackArrayElementIf<PvdErrorMessage> 0x1400BBEA0`。动态元数据只有上述五个字段，没有错误时间戳。完整文件身份与容器结构见 [原版格式调查](original-file-format.md)。这不是完整源代码恢复或其他 PVD3 版本的证明。

字符串先借用容器表查找，核算消息、文件及列表存储所需字节后才复制，计入共享 512 MiB 历史预算；起点记录与后续发生记录合计最多一百万条，原始属性数组还受已有值/历史预算约束。缺失句柄、短/长记录、未知布局、非追加编辑均产生明确错误。初始数组逐元素检查取消，历史追加沿用逐事件回调。

## 已验证与仍待验证

格式参考使用 rich 原版容器及同次逐字节 TCP 流，见[原版验证](native-validation.md)。已配对的错误为：

- code `4`，消息 `PxRigidDynamic::setMass: mass must be non-negative!`，源码 `NpRigidBodyTemplate.h:248`；完整源码路径逐字符与 raw 对照。
- `Index=0`、`history_event=981`、`received_frame=30`、容器实例 `29` / 代际 `0`。
- 消息、文件、行号、错误码、列表顺序和接收帧与同次 raw 一致。原版错误列表不产生 SDK `ErrorEvent`；SDK 流不产生 `UntimedErrorEvent`。
- 在回放帧 0、29、30、119、30、29、30 检查原始属性存在/消失边界，并验证顺序重建与检查点回放一致。

构造测试覆盖源 Index 与帧位置分离、末帧后记录、起点多容器/重复记录、销毁后列表保持、零帧、取消、预算和无效布局。中段导出按源历史序号及起点存活容器独立核对已有记录，再比较区间内追加；帧间收到的错误不能仅按接收帧归类，末帧后的错误不能移入选定区间。这些测试不代替原版非空快照或裁剪样例的互通证明。

CLI 配对输出分别确认原版时间未知、SDK 保留事件组 ticks。跳转只能定位接收顺序附近的帧，不能据此添加错误时间戳；完整交互见[原版验证](native-validation.md)。

仍需真实样例：多条错误、首帧前/末帧后错误、多 Scene 交错、重复 Index、多个错误容器及其销毁/复用、原版数组编辑、非空初始快照和其他格式版本。当前只有 1 条真实错误，不能宣称完整错误事件格式兼容。

可重复命令：

```powershell
cargo test --locked -p pvd-core --lib untimed -- --nocapture
cargo test --locked -p pvd-core --lib native::errors
cargo test --locked -p pvd-core --lib native::export::middle::tests::error_tests
cargo test --locked -p pvd-core --test native original_rich_untimed_error -- --include-ignored --nocapture
cargo test --locked -p pvd-core --test native -- --include-ignored --nocapture
```
