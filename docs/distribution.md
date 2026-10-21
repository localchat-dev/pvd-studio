# Windows 使用与打包

pvd-studio 参考 NVIDIA PhysX Visual Debugger 3（PVD3）的功能与工作流程。当前以 PhysX 3.4.2 为主要验证路径，部分 4.1.2 能力独立验证。格式及对象限制以[兼容矩阵](compatibility.md)为准。

## 启动与连接

1. 将发行包解压到可写目录，运行 `pvd-desktop.exe`。运行查看器不需要 Rust 或 PhysX SDK。
2. 在右上角“设置 → TCP 连接”配置地址和端口，再点击“开始监听”。默认 `0.0.0.0:5425`，也可使用指定 IPv4/IPv6 地址。停止监听后才能修改。
3. 远端 SDK 填写调试机可达的实际 IP 与相同端口；`0.0.0.0` 是绑定地址，不是远端目标。
4. 需要完整原始录制时，先在“设置”勾选“自动保存录制”并选择目录，再开始监听。收到数据后保存独立录制，正常完整结束生成 `.pvdstream`，中断录制保留 `.partial`；零字节连接不生成文件。
5. 在“设置 → 语言”切换简体中文/英文，同一菜单也可重置布局；偏好位于 `%LOCALAPPDATA%\pvd-studio\layout.json`。

应用启动时不自动载入样例。路径栏、文件选择器或启动参数均可打开捕获，帧号从 0 开始；项目捕获保留源会话的帧号。端口冲突或远端无法连接时，检查匹配端口、网卡地址、路由及入站规则。工具不会修改系统防火墙。

顶部“TCP · 等待数据”表示连接已接通但尚未收到数据，悬浮可查看完整连接状态及接收详情。“未收到数据便已断开”表示该连接为零字节，尚未形成 PVD 录制；监听器会继续等待下一条连接。单凭此提示无法判断发送端为何没有发送数据。收到部分协议头后断开仍会报告解码截断。

场景顶部保留一行刚体分类，悬浮可查看 SDK 活动数；时间轴显示历史范围与容量，接收积压和解码统计位于诊断页。界面布局与文案约定见[桌面界面说明](ui.md)。

## SDK 端

PhysX 3.4 使用 `PxCreatePvd` 和 `PxDefaultPvdSocketTransportCreate` 建立连接，按需启用 DEBUG、PROFILE、MEMORY。接触、约束和场景查询还需通过 `PxPvdSceneClient` 分别开启；查看器无法补出未发送的数据。

SDK 应使用包含 PVD 支持的 debug、checked 或 profile 配置。查看器自身的 Release 构建与 SDK 配置独立。原版手册的端口示例是 5435，本项目默认 5425；连接双方使用同一实际端口即可。

文件传输使用 `PxDefaultPvdFileTransportCreate`。SDK 原始流与 PVD3 数据库是不同容器，不能只按 `.pxd2` 扩展名判断。可构建示例见 [PhysX 3.4 生产器](../tools/physx-sample/README.md)，版本范围见兼容矩阵。

## 检查与分析

| 操作 | 入口 |
| --- | --- |
| 查看对象、属性和引用 | 对象树选择；属性链接跳转，数组展开，复制或添加曲线 |
| 控制镜头与逐帧 | 场景左下角操作提示；完整按键见[界面说明](ui.md) |
| 慢放 | 暂停实时，关闭“录制时序”，降低帧率后正放或倒放；左右键逐帧 |
| 性能 | 性能页切换线程时间线/事件曲线，筛选与缩放时间窗，测量区间或绘制帧/阶段 |
| 内存和错误 | 内存页查看存活状态与本帧活动；诊断页查看错误代码和位置 |
| 属性或事件曲线 | 曲线页缩放、标记、跳帧及 CSV 导出 |

图表缺失值保持空缺；接收刷新帧不是事件精确发生时刻。原版手册的配色、手势和图页方式与项目存在差异，见[手册对照](manual-checklist.md#手册对照)。

## 接收、历史与保存

| 存储 | 默认位置/策略 | 生命周期 |
| --- | --- | --- |
| 接收缓存 | 历史目录下 `receive`，无固定容量配额，受可用空间保护 | 已交付数据逐步回收 |
| 回放历史 | 当前工作目录 `pvd-cache/history`，可配置，默认滚动 8 GiB | 会话释放后清理自有临时数据 |
| 原始录制 | 自动录制默认 `captures/`，监听前启用；手动另存默认 `captures/capture.pvdstream` | 永久 `.pvdstream`，与临时缓存独立 |
| 项目捕获 | 默认 `captures/capture.pvdcapture`；时间轴“保存录制”，或“片段 → 保存保留历史”指定范围 | 独立 `.pvdcapture`，可重开和再次裁剪 |
| 完整文件分析索引 | 当前工作目录 `pvd-cache/profile` | 派生缓存，关闭/换源后释放数据，根控制锁可保留 |

接收积压 256 MiB 会告警，正数接收配额、至少 512 MiB 的可用空间保护或写入失败会结束连接。历史容量不保证固定时长；帧、元数据和当前场景预算也可能提前限制保留范围。

暂停或查看历史时接收继续。暂停帧被淘汰后可保留画面，但无法重新加载该帧；使用恢复入口或返回实时。临时历史不能补回已淘汰的完整原始流。首次/随机冷定位仍有基线恢复成本。

磁盘历史的“保存录制”保存全部仍可读取的帧，不要求开启自动原始录制，接收中或断开后只有 `.partial` 时均可使用。完整原始数据在“原始流 → 另存原始流”中保存，需要已有完整录制；内存 SDK 文件的保存按钮直接复制 `.pvdstream`。

`captures/` 与历史缓存一样相对当前工作目录解析。保存录制、片段或打开对应保存选择器时，所需目录不存在便自动创建；创建失败会报告错误。手动输入或选择的路径按原值使用，已有输出不覆盖。

## 文件与导出

| 输入 | 可用操作 | 边界 |
| --- | --- | --- |
| 完整 SDK 原始流 | 检查、回放、保存、片段定义/流式裁剪、完整分析 CSV | 分析按实际源数据和预算处理 |
| PVD3 数据库 v1 `.pxd2` | 部分对象/帧/分析/绘制读取和回放 | 超过 512 MiB 拒绝；原版保存和裁剪未接正式入口 |
| 项目 `.pvdcapture` | 读取 01/02，保存 02，独立重开与再次裁剪 | 无完整原始字节；全源 CSV/目录和 SDK 裁剪拒绝 |

完整 SDK 文件的片段定义保存在旁边的 `.pvdstudio-clips.json`，最多 64 个范围，以完整源摘要绑定；定义不修改录制本体。导出有进度、取消与无覆盖发布。项目捕获不使用 SDK 片段 sidecar。

RepX 支持已验证的 PhysX 3.4.2 刚体、基础/网格/地形几何、共享 Shape/资源和整帧六类官方关节。连接关节的单 Actor、特殊对象、运动学目标及其他依赖有限制，详见 [RepX](repx.md)。

可选 `samples/` 仅在打包时对应生成材料存在才附带。SDK 原始样例与原版实际保存容器分别标识；它们不构成完整 SDK 或原版格式支持声明。

## CLI 与诊断

```powershell
.\pvd-cli.exe listen 0.0.0.0:5425 captures 60
.\pvd-cli.exe inspect samples\physx-3.4.pvdstream
.\pvd-cli.exe verify samples\physx-3.4.pvdstream
```

接收命令的 `60` 是超时秒数，桌面监听没有这个限制，CLI 输出为英文。示例文件需要由发行包提供或通过 SDK 生产器生成。

发生接收积压或显示延迟时，可用 `start-with-diagnostics.cmd` 启动，等价于 `pvd-desktop.exe --live-profile`，复现后在“诊断”复制 JSON。详细计时默认关闭且有开销；报告不自动上传。

Rust panic 尽力写入 `%LOCALAPPDATA%\pvd-studio\diagnostics\panic-*.log`；驱动崩溃或系统强制终止不保证有日志。窗口与停靠补丁的来源和许可证包含在发行包 `licenses/`。

## 已知限制

原版通信目录中的相机和显示设置未导入；真实多页、非空起点、含 OriginShift 和更多绘制布局仍需配对。Cloth 只显示捕获的当前粒子和 Fabric 约束，粒子系统以点显示，不生成表面。

核心回放、egui 输入和离屏渲染已有多项验证，但动态 DPI、完整停靠、剪贴板、其他 IME、远端实际负载和长期资源趋势未全部验收。部分英文和录制相机离屏配置存在顶部工具栏绘制缺失，原因尚未定位。测试范围见[验证说明](validation.md)，剩余工作见[路线图](roadmap.md)。

## 从源码打包

```powershell
cargo fetch --locked
./tools/package-windows.ps1 -IncludeSource -ArchiveFormat 7z
```

脚本使用已缓存的锁定依赖离线构建桌面和 CLI，生成 `output/dist/pvd-studio-windows-x64-<时间戳>/` 与压缩包。默认 ZIP；`-ArchiveFormat 7z` 生成更小的 7z，内容相同，不改变程序或运行时性能。它不会上传或发布。

完整操作、ZIP 备选、同源构建和解压核验见[打包规范](packaging.md)，功能概况与后续工作见[项目状态](current-status.md)。

需要已有的 7-Zip 或支持该格式的解压工具。源码包与运行包的内容、过滤规则和校验步骤由打包规范统一定义。

## English

Extract to a writable directory and run `pvd-desktop.exe`. The viewer does not require Rust or the PhysX SDK. Set **Bind** and **Port**, then choose **Listen**. Defaults are `0.0.0.0:5425`; the remote SDK must use the debugger's reachable IP and the same port.

Enable permanent raw recording before listening when a complete `.pvdstream` is required. Receive storage and the rolling 8 GiB history are separate temporary stores. Pausing does not stop reception. A `.pvdcapture` preserves only the selected retained interval and its original frame numbers.

For disk history, **Save capture** saves all available frames as `.pvdcapture`, including while receiving or after an incomplete raw recording. Use **Raw stream → Save raw stream as** for a complete original SDK stream.

Native PVD3 database-v1 import is partial and rejects files above 512 MiB. Native-format Save/Trim is not a production feature. Project captures are distinct from SDK streams and native databases. Full compatibility and interaction limits are listed in the linked matrices.

Choose Simplified Chinese or English from the top menu. For opt-in timing diagnostics, use `start-with-diagnostics.cmd`. Reports stay local. The packaging script builds from cached locked dependencies and writes local output only.
