# Windows 使用与打包

pvd-studio 参考 NVIDIA PhysX Visual Debugger 3（PVD3）的功能与工作流程。当前以 PhysX 3.4.2 为主要验证路径，部分 4.1.2 能力独立验证。格式及对象限制以[兼容矩阵](compatibility.md)为准。

## 启动与连接

1. 将发行包解压到可写目录，运行 `pvd-desktop.exe`。需要 Windows x64 和 Microsoft Visual C++ x64 运行库，不需要 Rust 或 PhysX SDK。
2. 在右上角“设置 → TCP 连接”配置地址和端口，再点击“开始监听”。默认 `0.0.0.0:5425`，也可使用指定 IPv4/IPv6 地址。停止监听后才能修改。
3. 远端 SDK 填写调试机可达的实际 IP 与相同端口；`0.0.0.0` 是绑定地址，不是远端目标。
4. 需要完整原始录制时，先在“设置”勾选“自动保存录制”并选择目录，再开始监听。收到数据后保存独立录制，正常完整结束生成 `.pvdstream`，中断录制保留 `.partial`；零字节连接不生成文件。
5. 在右上角“设置”顶部直接选择“简体中文”或“English”。语言偏好保存在 `%LOCALAPPDATA%\pvd-studio\layout.json`，正常重启后沿用。

应用启动时不自动载入样例。使用「打开录制」选择文件，「路径」输入文件名，也可拖入文件或使用启动参数。帧号从 0 开始，项目捕获保留源会话的帧号。端口冲突或远端无法连接时，检查匹配端口、网卡地址、路由及入站规则。

内网机器若提示缺少 `VCRUNTIME140.dll`，可运行包内 `prerequisites/vc_redist.x64.exe` 安装 x64 运行库后再启动；此安装器仅在打包时显式附带。程序及 NavMesh 导入无需访问互联网。

缺少中文字体的 Windows 环境可使用 `pvd-desktop.exe --language en` 启动英文界面。

顶部“TCP · 等待数据”表示连接已接通但尚未收到数据，悬浮可查看完整连接状态及接收详情。“未收到数据便已断开”表示该连接为零字节，尚未形成 PVD 录制；监听器会继续等待下一条连接。单凭此提示无法判断发送端为何没有发送数据。收到部分协议头后断开仍会报告解码截断。

场景顶部保留一行刚体分类，悬浮可查看 SDK 活动数；时间轴显示历史范围与容量，接收积压和解码统计位于诊断页。界面布局与文案约定见[桌面界面说明](ui.md)。

### 桌面端与 Unity 共用端口

在同一台 Windows 电脑上，用同一 Windows 帐号打开桌面端和 Unity。两端填写相同的监听地址和端口，分别点击「开始监听」。先开的接收 PhysX TCP，后开的界面显示「共享接收」及来源。PhysX 只需建立一条连接，两端独立查看和录制。

「允许中途加入」默认开启，可在持有端口的一端开始监听前关闭。监听端从流开头暂存原始数据，供后开的一端追赶。桌面端和完整录制的 Unity 使用最多 512 MiB 的临时磁盘缓存。仅实时的 Unity 使用最多 128 MiB 的内存缓存。中途加入关闭、缓存不可用或超过容量上限后，后开的一端等待下一次 PhysX 连接。

关闭共享端只会断开自己的接收。持有端口的一端停止监听或退出时，本次 PhysX 连接结束，另一端接管同一个端口。让 PhysX 重新连接即可继续。原版 PVD3 或其它程序占用端口时，界面报告端口冲突。

## SDK 端

PhysX 3.4 使用 `PxCreatePvd` 和 `PxDefaultPvdSocketTransportCreate` 建立连接，按需启用 DEBUG、PROFILE、MEMORY。接触、约束和场景查询还需通过 `PxPvdSceneClient` 分别开启；查看器无法补出未发送的数据。

SDK 应使用包含 PVD 支持的 debug、checked 或 profile 配置。查看器自身的 Release 构建与 SDK 配置独立。原版手册的端口示例是 5435，本项目默认 5425；连接双方使用同一实际端口即可。

文件传输使用 `PxDefaultPvdFileTransportCreate`。SDK 原始流与 PVD3 数据库是不同容器，不能只按 `.pxd2` 扩展名判断。可构建示例见 [PhysX 3.4 生产器](../tools/physx-sample/README.md)，版本范围见兼容矩阵。

## 检查与分析

| 操作 | 入口 |
| --- | --- |
| 区分多个房间 | 左侧对象树上方的[房间列表](rooms.md)，自动编号、切换及查看销毁状态；接收与保存覆盖全部房间 |
| 叠加导航网格 | 工具栏 **NavMesh** 导入服务端 `.bin`；支持透明度、边线、坐标调整与房间绑定，格式范围见 [NavMesh](navmesh.md) |
| 查看对象、属性和引用 | 对象树选择，输入完整 ID 后 Enter 选择；属性链接跳转，数组展开，复制或绘制数值元素 |
| 控制镜头与逐帧 | 场景左下角操作提示；完整按键见[界面说明](ui.md) |
| 回放与返回实时 | 中栏时间轴。选中分析页或收起时间轴时，在顶部操作。暂停和跳帧期间接收继续 |
| 慢放 | 暂停实时，关闭“录制速度”，降低帧率后正放或倒放。左右键逐帧 |
| 性能 | 性能页切换线程时间线/事件曲线，筛选与缩放时间窗，测量区间或绘制帧/阶段 |
| 内存和错误 | 内存页查看存活状态与本帧活动；诊断页查看错误代码和位置 |
| 属性或事件曲线 | 曲线页缩放、标记、跳帧及 CSV 导出 |
| 操作帮助、版本与反馈 | 顶部“帮助”；F1 打开操作说明，反馈页可复制模板 |

图表缺失值保持空缺；接收刷新帧不是事件精确发生时刻。原版手册的配色、手势和图页方式与项目存在差异，见[手册对照](manual-checklist.md#手册对照)。

内网测试 NavMesh 时，先打开对应地图的 PhysX 捕获，再从工具栏 **NavMesh** 导入该地图的导航文件。包内 `samples/navmesh/` 提供两个小型验证文件，可检查导入及显示；它们是构造数据，与包内物理场景没有地图对应关系。此功能离线运行，无需安装 Node、Unity 或 Recast。当前支持小端、32 位引用的 `MSET v1 / DNAV v7`，`TSET` 压缩 TileCache 暂不支持。

## 接收、历史与保存

| 存储 | 默认位置/策略 | 生命周期 |
| --- | --- | --- |
| 接收缓存 | 历史目录下 `receive`，无固定容量配额，受可用空间保护 | 已交付数据逐步回收 |
| 回放历史 | 当前工作目录 `pvd-cache/history`，完整录制加有界热缓存 | 已结束录制保留在临时列表，按清理策略或用户操作回收 |
| 原始录制 | 自动录制默认 `captures/`，监听前启用；手动另存默认 `captures/capture.pvdstream` | 永久 `.pvdstream`，与临时缓存独立 |
| 项目捕获 | 默认保存到 `captures/`，按录制来源和保存时间建议文件名。打开「时间轴 → 保存与导出」保存录制，或在片段页指定范围 | 独立 `.pvdcapture`，可重开和再次裁剪 |
| 完整文件分析索引 | 当前工作目录 `pvd-cache/profile` | 派生缓存，关闭/换源后释放数据，根控制锁可保留 |

接收积压 256 MiB 会告警，正数接收配额、至少 512 MiB 的可用空间保护或写入失败会结束连接。完整临时录制保留起点，磁盘和导航资源不足时明确停止追加；热缓存预算不代表整份录制的磁盘上限。默认清理已结束临时录制的策略与范围见[完整临时录制](temporary-recordings.md)。

暂停或查看历史时接收继续。默认完整录制在回收热缓存前封存数据，可重新读取早期帧；显式滚动模式已淘汰的帧仍无法补回。首次/随机冷定位仍有基线恢复成本。

磁盘历史的“保存录制”保存全部仍可读取的帧，不要求开启自动原始录制，接收中或断开后只有 `.partial` 时均可使用。完整原始数据在“原始流 → 另存原始流”中保存，需要已有完整录制；内存 SDK 文件的保存按钮直接复制 `.pvdstream`。

`captures/` 与历史缓存一样相对当前工作目录解析。保存录制、片段或打开对应保存选择器时，所需目录不存在便自动创建；创建失败会报告错误。手动输入或选择的路径按原值使用，已有输出不覆盖。

## 文件与导出

| 输入 | 可用操作 | 边界 |
| --- | --- | --- |
| 完整 SDK 原始流 | 检查、回放、保存、片段定义/流式裁剪、完整分析 CSV | 分析按实际源数据和预算处理 |
| PVD3 数据库 v1 `.pxd2` | 部分对象/帧/分析/绘制读取和回放；时间轴或片段页的“PVD3 录制”整份保存及受限帧区间裁剪 | 读入使用磁盘历史；写出各限 512 MiB，更广布局及原版应用互验未完成 |
| 项目 `.pvdcapture` | 读取 01/02/03/04，单文件保存、独立重开与再次裁剪 | 无完整原始字节；全源 CSV/目录和 SDK 裁剪拒绝 |

完整 SDK 文件的片段定义保存在旁边的 `.pvdstudio-clips.json`，最多 64 个范围，以完整源摘要绑定；定义不修改录制本体。导出有进度、取消与无覆盖发布。项目捕获不使用 SDK 片段 sidecar。

原版保存使用 `.pxd2`，默认 `captures/capture.pxd2`；CLI 对应 `native-save` 和 `native-trim`，范围见[原版容器保存](native-export.md)。这些命令不将 SDK 流转换为原版容器。

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

原版通信目录中的相机和显示设置已有读取路径，非空相机目录尚缺真实配对；多页、非空起点、含 OriginShift 和更多绘制布局也需独立输入。Cloth 显示捕获的当前粒子和 Fabric 约束，粒子系统以点显示，不生成表面。

核心回放、egui 输入和离屏渲染已有多项验证，但动态 DPI、完整停靠、剪贴板、其他 IME、远端实际负载和长期资源趋势未全部验收。工具栏文字缺失报告在双语及录制相机离屏复核中未复现，仍需对应环境验证。测试范围见[验证说明](validation.md)，剩余工作见[路线图](roadmap.md)。

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

Enable permanent raw recording before listening when a complete `.pvdstream` is required. Receive storage, persistent temporary history, and its bounded hot cache are separate. Pausing does not stop reception. Finished recordings remain available for viewing, saving, and managed cleanup. A `.pvdcapture` preserves the selected available interval and its original frame numbers.

For disk history, **Save capture** saves all available frames as `.pvdcapture`, including while receiving or after an incomplete raw recording. Use **Raw stream → Save raw stream as** for a complete original SDK stream.

Native PVD3 database-v1 import is partial and uses bounded random reads with disk history. Restricted whole-file saving and frame-range export are available under **Clips → PVD3 recording**, with separate 512 MiB input/output limits. Original-application acceptance and broader layouts remain unverified or unsupported. Project captures, SDK streams and native databases are distinct formats; see the linked compatibility matrix.

Choose Simplified Chinese or English from the top menu. For opt-in timing diagnostics, use `start-with-diagnostics.cmd`. Reports stay local. The packaging script builds from cached locked dependencies and writes local output only.
