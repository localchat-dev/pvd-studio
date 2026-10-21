# 项目状态

pvd-studio 参考 NVIDIA PhysX Visual Debugger 3（PVD3）的功能与工作流程。主要验证 PhysX 3.4.2，PhysX 4.1.2 为部分支持。

TCP 接收、滚动历史、正倒回放、对象检查、三维显示、性能与内存分析、SDK 流裁剪和项目捕获保存已实现。原版容器保存、任意区间裁剪及完整交互对齐仍未完成。

| 文档 | 内容 |
| --- | --- |
| [功能清单](manual-checklist.md) | 各项功能的实现与验收状态 |
| [兼容矩阵](compatibility.md) | SDK、格式、对象和导出范围 |
| [路线图](roadmap.md) | 未完成任务、优先级与完成条件 |
| [验证说明](validation.md) | 检查命令、输入要求与已知问题 |
| [打包规范](packaging.md) | 从源码构建和核验 Windows 分发包 |

开发优先级为原版文件工作流，其次是数据与分析语义对齐、完整交互和规模验收。具体任务以路线图为准。

源码仓库不包含预编译程序或捕获文件。运行方式见[项目说明](../README.md#构建与运行)，测试输入按[SDK 生产器说明](../tools/physx-sample/README.md)生成。
