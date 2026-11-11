# 反复载入与资源释放检查

`--render-lifecycle-check` 在同一进程内使用正式加载、回放和渲染路径，反复执行打开、首尾/中间定位、关闭、取消加载和失败后恢复。它不操作系统窗口，也不改变用户布局。可交替输入 SDK 流和原版容器。

```powershell
cargo build --locked -p pvd-desktop
./tools/verify-lifecycle.ps1 -Capture captures/physx-3.4-rich.pvdstream -SecondCapture captures/native/rich-paired-native.pxd2 -OutputDirectory output/lifecycle-check -Rounds 20
```

输出目录必须尚不存在。每轮检查显示状态与完整恢复一致、渲染对象与场景一致，关闭后 Mesh/Material/Image 资源及实体回到空窗口基线，专用历史目录不残留文件。结束时等待后台工作释放。

`run/lifecycle.json` 保存逐阶段计数和失败原因；`process.csv` 每 500 ms 记录进程工作集、私有内存、句柄、线程和累计 CPU 时间；`manifest.json` 记录程序与输入摘要。资源计数表示引擎持有的资源，不是 GPU 显存字节。分配器或驱动保留的内存不能仅凭单个阈值判为泄漏，应比较多轮关闭后的趋势。

这项检查覆盖同进程的文件生命周期，不替代真实远端 TCP、长期原生窗口操作、GPU 显存测量或 PVD3 同输入性能对照。输入须至少保留两帧，定位范围取实际保留区间；轮数范围为 2–1000，每个阶段最多等待 90 秒。关闭和取消会在该期限内等待资源释放，超时保留现场计数。
