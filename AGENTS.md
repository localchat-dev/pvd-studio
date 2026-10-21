# pvd-studio 项目约定

- 开发前阅读 `docs/current-status.md`、`docs/roadmap.md` 和 `docs/validation.md`，确认功能边界、待办和验证要求。
- Windows 发行按 `docs/packaging.md` 执行，推荐带源码的高压 7z，ZIP 为兼容选项；构建目录可通过 `-TargetDirectory` 指定，确认程序与源码对应后才使用 `-SkipBuild`。
- Rust、Bevy、bevy_egui/egui、Cargo workspace，Windows 优先。PhysX 3.4 是首条验证路径；其他 SDK 的证据独立记录。直接依赖使用稳定发布版并维护 Cargo.lock。
- 保留原版安装文件；不上传二进制、符号或捕获。不安装全局工具、不修改全局配置、不发布外部服务，除非用户明确授权具体操作。
- 桌面操作限定于 pvd-studio 与必要的 PVD3 验证；由主任务统一控制窗口与输入，子任务仅做后台开发和测试。不操作无关应用，不修改系统配置。批量布局验证优先 `--render-check` 无窗口渲染。
- 协议、历史模型与回放不依赖 Bevy/egui。展示只来自捕获，不重新模拟。SDK 生产器只属于独立验证工具。
- 样例、自写格式、源码共用、文档提及都不能自动成为原版 `.pxd/.pxd2` 或完整 SDK 兼容声明。事实、推断、未验证项分别记录。
- 文档描述用途、操作、设计与边界；提交信息描述最终行为和必要验证，不写对话或开发纪要。相同事实集中维护，避免重复命令、状态和清单。公开说明不引用个人目录或未提交的实验记录；生成文件的路径须由命令或配置明确给出。
- 核心测试：`cargo test --locked -p pvd-protocol -p pvd-core -p pvd-transport -p pvd-cli`。完整真实 SDK 路径见 `tools/verify-sdk.ps1`；被忽略的 SDK 测试需要生成真实输入并显式运行，不能报告为默认已通过。
- 清晰命名与结构优先。注释只解释必要的设计原因、业务约束、安全不变量或兼容性要求，不复述代码、不记录修改历史。
- 阶段结束检查实际修改内容、Git diff、格式、错误处理、资源边界和新增注释。更新功能/兼容矩阵的验收状态，并保留可重复命令及具体限制。
- 功能概况维护在 `docs/current-status.md`，任务及完成条件维护在 `docs/roadmap.md`，验证方法与已知问题维护在 `docs/validation.md`，打包流程维护在 `docs/packaging.md`。实验日志、产物哈希及会话记录保留在被忽略的生成目录中。
