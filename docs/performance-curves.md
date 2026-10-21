# 帧与阶段性能曲线

本地 `PVD 3 User Manual.pdf` p7 描述 Plot frame graph，包含 frame、collision、solver；p10 将事件图的 Y 轴定义为事件持续时间。这些说明不规定原版图表内部的帧归属算法。本实现按固定官方 PhysX 3.4.2 源码与真实时钟证据解释捕获，不宣称已经复原原版内部算法。

## 数据口径

| 曲线 | 数据 | 不代表什么 |
| --- | --- | --- |
| 帧结束间隔 | 相同场景相邻 `EndSection("frame")` 时钟差，第一帧为空 | 模拟 dt、CPU 忙碌时间或纯引擎耗时 |
| 模拟阶段经过时间 | 单个完整 `Basic.simulate` span 的 end-start | 所有线程累计 CPU 时间 |
| 碰撞阶段经过时间 | 单个完整 `Basic.collision` span 的 end-start | 逐个 BroadPhase/NarrowPhase 子事件之和 |
| 求解阶段经过时间 | 单个完整 `Basic.rigidBodySolver` span 的 end-start | 子任务或嵌套事件累加值 |

性能面板“绘制帧与阶段”使用当前展示帧所属的场景 ID 与代际，在空图页或新图页创建四条曲线。图页和每条曲线 CSV 支持与属性曲线相同的有界后台采样、缩放、平移、框选、标记与跳帧操作。

完整 SDK 文件使用双遍流式阶段采样。第一遍收集元数据，第二遍配对完整区间；两遍内容一致，身份重定义报错。帧索引最多 2,000,000 项、采样最多 5,000 点，索引预算 64 MiB，支持取消，见[完整文件曲线](full-file-profile.md)。

只匹配 `PhysXSDK` 区域中的确切事件名，以及 `context == scene_id`、保留线程 ID `99999789` 的完整阶段区间。SDK 将该 ID 用于跨线程阶段；界面将其标为跨线程分组，不作为真实操作系统线程计数。

以事件结束时钟寻找同场景的首个不早于该时钟的 frame EndSection。阶段起点不得早于前一个同场景 EndSection；同一个帧区间只有一个该阶段完整事件时才输出值。缺失、重复、跨界、场景时钟非严格递增、其他场景、其他或缺失代际分别保留为空，并在悬停诊断及 CSV `status` 中说明。没有把 payload 刷新帧当成阶段发生帧，也没有修改原始 profile spans。

不同场景只根据自己的 EndSection 序列计算，其他场景的全局帧位置为空；尚未用异步多场景真实样例独立验收。场景地址复用通过代际筛选隔离，帧间隔也检查相邻场景帧的代际。

## 固定源码证据

官方 3.4.2 commit `5e42a5f112351a223c19c17bb331e6c55037b8eb`：

- `PxShared/src/pvd/src/PxPvdDataStream.cpp:755–766`：BeginSection/EndSection 使用 `Time::getCurrentCounterValue()`。
- `PxShared/src/pvd/src/PxProfileEventBuffer.h:161–188`：profile start/stop 使用相同的 `shdfnd::Time::getCurrentCounterValue()`。
- `PxShared/src/pvd/src/PxPvdCommStreamEvents.h:313–314`：初始化头保存时钟纳秒比例；显示毫秒时再除以 `1e6`。
- `PhysX_3.4/Source/PhysX/src/NpSceneQueries.h:174` 的 `getContextId()` 返回 `this` 地址，`NpScene.cpp:98` 将其传入 ScScene，`ScScene.cpp:556` 保存该值。
- `PxShared/src/pvd/src/PxPvdImpl.cpp:331–378`：detached start/stop 使用 `CrossThreadId=99999789`。
- `NpScene.cpp:1965,2341,2466` 标记 simulate；`ScScene.cpp:2083,2480` 标记 collision；`ScScene.cpp:2714,2860,3743` 标记 rigidBodySolver。

真实 SDK 样例进一步核对 `context` 与 SDK `PxScene*` 地址，避免仅根据源码函数名推断映射。

## 独立 QPC 验证

`tools/physx-features/profile_metrics.cpp` 包装官方 `PxProfilerCallback`，继续转发原 PVD callback，并在每次 start/stop 转发前后分别调用 Windows QueryPerformanceCounter。JSON 直接记录这些独立上下界、SDK 场景地址、调用帧号及 QPC 频率；没有从捕获解析结果反向生成真值。

```powershell
.\tools\physx-features\build.ps1 -Target pvd-profile-metrics -BuildRoot .deps/physx-profile-metrics-build
.\.deps\physx-profile-metrics-build\Release\pvd-profile-metrics.exe captures/physx-3.4-profile-metrics.pvdstream captures/physx-3.4-profile-metrics.json
cargo test --locked -p pvd-core frame_metrics:: -- --nocapture
cargo test --locked -p pvd-core --test frame_metrics -- --ignored --nocapture
cargo clippy --locked -p pvd-core -p pvd-desktop --all-targets -- -D warnings
cargo build --locked -p pvd-desktop
.\target\debug\pvd-desktop.exe --render-check captures/physx-3.4-profile-metrics.pvdstream artifacts/desktop-frame-curves.png 12 frame-curves 1440 960 --language zh-CN
```

`PVD_PROFILE_METRICS_CAPTURE` 可指定配对 stream/JSON。用 SDK QPC 上下界检查 Scene 帧末、阶段 start/stop、时钟比例、context 和代际；指针与计时随运行变化。

默认核心测试还覆盖刷新帧与事件帧不同、同帧嵌套/重复阶段不累加、跨帧阶段留空、缺失阶段、其他场景、第一帧、非单调时钟及取消。真值生产器有计时与锁的额外开销，所以这些数据只用于正确性验证，不能作为 PhysX 性能基准。

## 与任意事件累计曲线的区别

固定 SDK 的 Solver 可能同时存在完整区间和额外未配对起点。只含完整区间的 QPC 真值不能用作额外起点的独立证据；SDK 源码中的多个 start 和单个 stop 也不能替代运行时核对。

固定阶段曲线的定义仍然是**单个完整区间的经过时间**：已有72个完整区间的QPC对照成立，但不能提升为所有起点对应的阶段总耗时。[任意事件曲线](profile-curves.md) 的定义是窗口内同一精确键的全部完整区间之和；由于未结束事件可能影响总量，该通用接口对solver首帧返回 `no_previous_frame`，后23帧返回 `open_span_may_overlap_window` 空值。Simulate和Collision在通用接口各有23个可用窗口；两个入口不同的口径在界面和CSV中分别标识，不补造缺失时长。

PhysX 4.1 的 raw spans 能进入同一接口，但当前证据中的 `Basic.rigidBodySolver` 有未配对事件；曲线会保留缺失，不补造数值。独立回调验证仅完成 Windows x64 PhysX 3.4.2；没有据此声明其他 SDK 或 GPU 阶段兼容。

实现：`pvd-core/src/frame_metrics.rs` 独立于 Bevy/egui，桌面入口及显示位于 `profile.rs`、`curve.rs`、`curve_panel.rs`。
