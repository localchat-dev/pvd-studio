# 属性曲线与独立图页

属性曲线从录制历史恢复对象的数值属性；不执行查询或物理模拟。对象 ID、代际、属性和分量共同确定一条曲线。对象不存在、代际不同、数值缺失、分量类型变化或非有限数值均形成空值，绘图不会跨越空值连接。

## 操作与范围

- 属性面板的“曲线”操作向当前图页添加曲线。最多 16 条曲线、8 个图页；同一曲线在多个图页共享采样，各页独立管理可见性、显示区间和标记。
- 曲线管理可添加已有曲线、隐藏、移出当前页或从所有图页删除；图页可命名与删除，至少保留一个图页。录制关闭或换入成功时清除该录制的曲线。
- 拖动平移，滚轮围绕指针缩放两轴；Shift+拖动或“矩形缩放”建立框选区间。“适配全部”恢复自动范围。
- 单击跳至对应录制帧并标记当前曲线的最近实际采样；双击跳至最近实际采样帧。最多保留 32 个标记/图页。标记只显示真实采样值，不生成插值值；刷新后标记重新查找当前采样。
- 多条曲线共用一个值轴。不同数量级或单位宜分到不同图页；暂未提供独立左右轴。图内上沿彩色图例显示分量及曲线编号，空间不足的条目合并为可悬停展开的数量标签；完整对象与分量可悬停查看。悬停图表显示各曲线的采样帧和数值。
- 中英文界面、每条曲线独立 CSV 路径。CSV 带 frame/value/object_id/generation/property/component/stride/captured_frame_count/source/unit/status/association，空值保留为空单元格。文件写入使用 create_new，避免静默覆盖。

## 有界采样与后台任务

单条曲线最多 5000 个采样，固定步长为 `ceil((frame_count - 1) / 4999)`，最少为 1，并保证包含首尾帧。末个采样间隔可能短于标称步长。UI 显示步长、样本数和所依据的录制末帧；大录制会跳过中间帧，不能用该曲线证明不存在短暂尖峰。

仅一个后台工作线程顺序处理曲线队列，逐采样检查取消。采样固定于不可变录制快照；实时接收后可使用“重新采样”更新到新快照。取消或关闭时丢弃接收端并置取消标记，后续录制不会接收旧结果。一个状态恢复期间仍可能有延迟；此处不承诺硬实时取消。

数值图和 CSV 使用 f64，超过 53 位精度的整数可能舍入。原始对象属性仍保存在核心模型，图表不是其无损序列化。

## 可重复验证

```powershell
cargo test --locked -p pvd-desktop curve -- --nocapture
cargo test --locked -p pvd-desktop sdk_curve_values -- --ignored --nocapture
cargo clippy --locked -p pvd-desktop --all-targets -- -D warnings
cargo build --locked -p pvd-desktop
.\target\debug\pvd-desktop.exe --render-check captures/physx-3.4-rich.pvdstream artifacts/desktop-multicurves.png 65 curves 1440 960 --language zh-CN
```

真实 SDK 测试使用 `captures/physx-3.4.pvdstream` 和同名 getter CSV，或通过 `PVD_SDK34_CAPTURE` 指定配对输入，核对值、代际断线、关闭取消与换源隔离。

默认测试覆盖样本预算首尾边界、图页/曲线去重与上限、取消重试、CSV 转义与空值、恒值/极端值坐标、标记断线，以及合成 egui 的矩形缩放、平移、点击、滚轮和文本焦点保护。合成输入只进入测试 egui 上下文，不发送系统输入。`desktop-multicurves.png` 是真实录制位置 x/y/z 三曲线的无窗口 GPU 渲染，已经目视检查；原生 DPI、IME、触控板和用户窗口拖动仍需单独验收。

实现：`crates/pvd-desktop/src/curve.rs`、`curve_panel.rs`，以及 `studio.rs` 中的曲线任务队列。

性能面板可向图页添加帧与阶段曲线，具体时间含义和独立验证见 [性能曲线](performance-curves.md)。
