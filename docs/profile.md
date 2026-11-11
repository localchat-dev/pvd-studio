# CPU 性能时间线

完整 SDK 文件和原版数据库磁盘历史提供全源时间窗查询，可载入早于当前显示分析窗口的事件；具体来源校验、资源上限与交互见[完整历史线程时间线](full-file-thread-timeline.md)。以下保留窗口路径用于实时接收及未持久化的分析。

性能面板直接使用 SDK 捕获的 CPU start/stop 区间，不执行物理模拟。窗口时间单位为毫秒，相对于捕获中的第一个完整性能区间；转换使用流头中的时钟比例。

## 功能

- 按线程 ID 分组，将嵌套和相互交叠的事件放在不同轨道；相邻、不重叠的事件复用轨道。SDK 保留 ID `99999789` 显示为“SDK 跨线程阶段”，不是操作系统线程。轨道排布表达时间重叠，不推断调用栈或父子任务关系。
- 事件色按 zone/event 稳定选择；悬停查看事件、持续时间、线程、上下文和 payload 刷新帧，点击跳到该刷新帧。右键单个事件条可绘制其精确 zone/event/thread/context 的耗时曲线，按参考 Scene 时间窗累计；密集合并条需放大后选择。刷新帧归属不等于精确的 SDK 帧边界。
- 可按线程、zone 或事件名称筛选；可显示全部录制、当前刷新帧，或输入毫秒窗口。
- 时间轴拖动平移、滚轮缩放；轨道拖动平移、Ctrl+滚轮缩放，Shift+滚轮水平平移，普通滚轮滚动线程列表。
- “测量区间”模式或 Shift+拖动建立起止标记，显示起点、终点与 Δ 毫秒；清除按钮移除测量。文本编辑获得焦点时禁用图表手势。
- 中英文界面；布局调整保留现有停靠流程。
- “绘制帧与阶段”将当前场景的帧结束间隔、模拟、碰撞与求解阶段经过时间添加到曲线面板，见 [性能曲线](performance-curves.md)。

## 大量事件处理

按不可变录制实例缓存排序与轨道分配；筛选仅重建匹配索引。线程/轨道列表使用虚拟行，各轨道通过二分搜索裁掉窗口外区间。小于像素的密集事件按屏幕列合并成灰色条，每轨道最多 4096 列；缩放后可逐条查看。捕获的事件仍保留在核心记录中，合并只影响显示。

布局首次生成或接收到新的实时快照时需对区间排序；这里没有宣称任意规模下无停顿。区间持续时间不是 CPU 使用率，同线程嵌套持续时间不能直接相加为忙碌时间。GPU 数据未在该面板中解析或显示。

## 验证

```powershell
cargo test --locked -p pvd-desktop profile:: -- --nocapture
cargo clippy --locked -p pvd-desktop --all-targets -- -D warnings
cargo build --locked -p pvd-desktop
.\target\debug\pvd-desktop.exe --render-check captures/physx-3.4-query-shapes.pvdstream artifacts/desktop-profile-threads-zh.png 6 performance 1440 960 --language zh-CN
.\target\debug\pvd-desktop.exe --render-check captures/physx-3.4-query-shapes.pvdstream artifacts/desktop-profile-threads-en.png 6 performance 1440 960 --language en
.\target\debug\pvd-desktop.exe --render-check captures/physx-3.4-query-shapes.pvdstream artifacts/desktop-profile-threads-1024.png 6 performance 1024 720 --language zh-CN
```

自动化测试覆盖嵌套与部分交叠、轨道复用、零时长事件、窗口二分裁剪、筛选后的原轨道保留、非有限/空/逆序窗口，以及合成 egui 指针事件的区间测量；合成事件仅送入测试中的 egui 上下文，不发送操作系统输入。

离屏检查覆盖中英文 1440×960 和中文 1024×720，确认线程分组、时间轴、缩放和标签可读。图片不能替代原生触控板、DPI、输入法和拖拽。

实现：`crates/pvd-desktop/src/profile.rs`。
