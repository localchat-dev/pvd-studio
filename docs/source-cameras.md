# 捕获中的相机

“显示 → 相机 → 视角”可选择自由视角，或当前帧中捕获的具名相机。选择源端相机后，位置、观察目标和向上向量随当前显示帧变化；实时、暂停、随机定位和正倒放使用同一份显示状态。不会用仍在接收的新数据覆盖暂停画面的相机。

相机选择只在当前录制内保留。切回自由视角恢复原有自由相机；“查看全部”、聚焦和跟随对象会离开源端相机。源端相机下不接受导航输入，仍可拾取对象。倒退到相机首次出现之前，或遇到无效位置/方向，面板明确提示并暂显示自由视角；不会沿用其他帧的源端姿态。

## 数据含义

PhysX 3.4.2/4.1.2 的 `PxPvdSceneClient::updateCamera(name, origin, up, target)` 发送 SDK 协议事件23。事件只有名称及三个向量，没有 Scene ID、投影类型、视角、裁剪面、删除或独立时间戳。不同 stream 的同名相机按接收顺序覆盖；在完成帧边界保存最新值，未更新帧保持前值。完成末帧后的更新不倒灌到该帧。

因此当前选择是录制内的名称目录，不能据此推断相机归属哪个 Scene。视角和近远裁剪沿用查看器设置，不宣称恢复源端的完整投影。SDK 的场景客户端调用也不保证与每个物理帧一一对应。

预设、自定义或自动上方向、两种手性和单位比例同时变换相机与几何，规则见[显示坐标](coordinates.md)。向上向量按方向变换，不按角速度的轴向量规则变换；保留倾斜/滚转。渲染、屏幕投影和拾取使用同一组位置、目标和正交化后的向上方向。无效原始数值仍保存在核心状态中，桌面仅拒绝以其构建视图。

临时历史格式为内部 `PVDHIS03`；它不是原版 `.pxd/.pxd2` 数据库格式。相机数据进入快照、增量、撤销、磁盘分段及资源预算，SDK 流片段导出保留必要的初始相机和后续更新。原版 Camera 目录和实例生命周期也接入此路径，并支持含稳定性能起点组合的[受限帧裁剪](native-export.md#具名相机裁剪)；格式及真实样例边界见[原版相机与显示属性](native-settings.md)。APEX 相机生产路径尚未验证。

## 可重复验证

```powershell
cargo test --locked --offline -p pvd-core --test source_cameras
cargo test --locked --offline -p pvd-desktop captured_camera -- --nocapture
cargo test --locked --offline -p pvd-core --test source_cameras official_sdk_cameras_match_the_producer_in_memory_disk_replay_and_clips -- --ignored --nocapture
```

默认核心测试覆盖大小端、同名跨 stream 更新、首次出现、未更新帧、未完成尾部、NaN/负零位模式、内存与磁盘、跨段连续/随机正倒放、滚动淘汰、合并发布及两种裁剪 API。桌面测试覆盖五种单位尺度下的倾斜相机投影/拾取，以及缺失、非法值和关闭录制后的状态。

真实输入按[相机生产器](../tools/physx-cameras/README.md)生成，测试读取 `captures/cameras/`。预期来自源端调用记录，不能由被测解码器生成；默认通过不代表 ignored 项已执行。

无窗口 GPU 检查支持 `source-camera`、`source-camera-z-left` 与 `source-camera-custom-left`，选择请求帧中第一个有效具名相机；缺少有效相机时失败，不静默退回自由相机。后两者分别设置 +Z 和 `[1,2,-3]` 上轴，均为左手、0.1 米/单位。这些检查不保存用户偏好。

```powershell
cargo run --locked --offline -p pvd-desktop -- --render-check captures/cameras/cameras.pvdstream output/source-camera.png 60 source-camera 1440 960 --language zh-CN
```
