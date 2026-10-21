# 文档索引

项目参考 NVIDIA PhysX Visual Debugger 3（PVD3）的功能与工作流程。当前能力以源码、明确的 SDK 范围及对应验证共同确定。

## 当前状态与开发入口

| 文档 | 用途 |
| --- | --- |
| [项目说明](../README.md) | 用途、构建、运行、缓存及常用命令 |
| [项目状态](current-status.md) | 功能概况与开发优先级 |
| [架构与边界](architecture.md) | 模块职责、线程、历史与数据来源 |
| [兼容矩阵](compatibility.md) | SDK、文件、对象和导出的当前范围 |
| [逐项功能清单](manual-checklist.md) | 以稳定编号追踪实现与验收 |
| [实现路线图](roadmap.md) | 剩余工作、优先级、依赖和完成条件 |
| [验收方法](acceptance-plan.md) | 默认检查、真实 SDK、原版配对和产品验收 |
| [验证说明](validation.md) | 检查命令、输入要求和已知问题 |
| [Windows 使用说明](distribution.md) | 桌面、CLI 和源码打包 |
| [Windows 打包规范](packaging.md) | 7z/ZIP 命令、可配置构建目录与解压核验 |

## 实现专题

| 范围 | 文档 |
| --- | --- |
| 协议与对象 | [协议](protocol.md)、[对象层级](object-hierarchy.md)、[坐标](coordinates.md)、[多 Scene](multiscene-sdk.md) |
| 接收与历史 | [持续接收](continuous-live.md)、[接收策略](receive-backpressure.md)、[共享历史](shared-history.md)、[回放](tcp-playback.md)、[保留捕获](retained-capture.md) |
| 显示 | [查询形状](query-shapes.md)、[地形](heightfield.md)、[关节限位](joint-limits.md)、[Cloth](cloth.md)、[粒子](particles.md)、[相机](source-cameras.md) |
| 分析 | [性能](profile.md)、[内存](memory-history.md)、[曲线](curves.md)、[完整文件分析](full-file-profile.md)、[线程时间线](full-file-thread-timeline.md)、[CSV](analysis-export.md) |
| 交换格式 | [原版容器](original-file-format.md)、[原版绘制](native-debug-draw.md)、[原版错误](native-errors.md)、[OriginShift](origin-shift.md)、[SDK 片段](clips.md)、[RepX](repx.md) |
| 桌面与资源 | [界面](ui.md)、[相机操作](camera-controls.md)、[显示帧一致性](display-frame-consistency.md)、[退出收尾](background-shutdown.md)、[接收清理](receive-cache-cleanup.md) |

专题说明设计、数据来源和限制；当前支持范围以兼容矩阵和功能清单为准。

## 验证材料与版本管理

文档中的相对路径以仓库根目录为起点。`output/`、`captures/`、`.deps/`、`target/`、`pvd-cache/` 和 `artifacts/` 存放生成物或外部材料，不随源码提交。捕获需要按对应生产器说明生成，原版容器需要另外准备。

界面示例位于 [images](images/README.md)。只保留经过选择的展示图；它们说明界面形态，不构成原生交互、稳定帧率或完整格式兼容证据。

设计、待办与验证分别维护在架构、路线图和验证说明中。实验日志及原始产物保留在生成目录，不作为公共文档的依赖。
