# 项目状态

pvd-studio 参考 NVIDIA PhysX Visual Debugger 3（PVD3）的功能与工作流程。主要验证 PhysX 3.4.2，PhysX 4.1.2 为部分支持，已独立核对地形、场景查询和 ReducedCoordinate 捕获中的 Base 关节树。

桌面提供 TCP 接收、滚动历史、正倒/逐帧回放、对象检查、三维显示、性能与内存分析、曲线和录制导出。停靠布局、显示设置、帮助与快捷键说明可保存。

场景与对象树支持 Shift+单击多选，聚焦和显示范围覆盖选择集，属性及跟随使用当前对象；RepX 支持多个 Actor 及其依赖的合并导出。

SDK 文件与 TCP 支持[帧来源选择](frame-sources.md)，回放沿所选 PhysX 或 APEX 来源进行。项目捕获写出 PVDCAP03 并保留来源目录，旧 01/02 仍可读取。APEX 1.4 CPU Clothing 的同步与异步文件、真实 SDK TCP 已核对同次 getter、生命周期、逐源绘制、回放和保存重开；实际画面通过无窗口检查。

PVD3 数据库 v1 支持分页读取、完整线程时间线、受限整份保存和区间裁剪；多 Scene 相机与内存组合、非空起点及再次裁剪有构造验证。[`.pvd` 录制包](pvd-archive.md)支持受限 ZIP32 读取，导出使用内部录制。原版应用互通、更广历史布局与真实多页/非空起点仍需验收，缺失前史保持明确标记。

RepX 支持已验证的刚体、六类官方关节、Articulation、混合 Aggregate、受限 CPU 粒子及运动学参数快照。对象、格式和不可恢复状态的范围集中维护在[兼容矩阵](compatibility.md)和[RepX 说明](repx.md)。

| 文档 | 内容 |
| --- | --- |
| [功能清单](manual-checklist.md) | 各项功能的实现与验收状态 |
| [兼容矩阵](compatibility.md) | SDK、格式、对象和导出范围 |
| [路线图](roadmap.md) | 未完成任务、优先级与完成条件 |
| [验证说明](validation.md) | 检查命令、输入要求与已知问题 |
| [打包规范](packaging.md) | 从源码构建和核验 Windows 分发包 |

开发优先级为原版文件工作流，其次是数据与分析语义对齐、完整交互和规模验收。具体任务以路线图为准。

源码仓库不包含预编译程序或捕获文件。运行方式见[项目说明](../README.md#构建与运行)，测试输入按[SDK 生产器说明](../tools/physx-sample/README.md)生成。
