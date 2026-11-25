# pvd-studio

[English](README.md) | **简体中文**

用于 PhysX 数据捕获、回放和场景检查的 Windows 桌面工具，基于 Rust、Bevy 和 egui，参考 NVIDIA PhysX Visual Debugger 3（PVD3）的功能与工作流程。

主要验证 PhysX 3.4.2，部分支持 PhysX 4.1.2。原版录制格式和功能兼容范围见[兼容矩阵](docs/compatibility.md)。

## 功能

- TCP 接收、原始录制和磁盘滚动历史。
- 实时跟随、暂停、逐帧、随机定位、正放和倒放。
- 对象树、属性、数组与引用检查，以及场景选择联动。
- 刚体、网格、地形、关节、接触和场景查询显示；CPU Cloth 与粒子显示有独立范围限制。
- 线程性能、事件曲线、内存和错误分析，支持 CSV 导出。
- SDK 流裁剪、项目捕获保存与重开、受限的原版容器保存及 PhysX 3.4.2 RepX 导出。

显示状态来自捕获，查看器不重新运行物理模拟。PVD3 数据库 v1 支持部分布局的读取、整份保存与区间裁剪；更广的前史恢复与原版应用互验尚未完成，具体边界见[原版容器保存与裁剪](docs/native-export.md)。

## 构建与运行

需要 Rust 和 Windows MSVC C++ 工具链。Rust 版本由 `rust-toolchain.toml` 指定，依赖由 `Cargo.lock` 固定；运行查看器不需要安装 PhysX SDK。

```powershell
cargo run --locked -p pvd-desktop
cargo run --locked -p pvd-desktop -- --language en
```

程序启动后可打开捕获文件，或点击“开始监听”。默认绑定 `0.0.0.0:5425`；SDK 端使用调试机可达的实际 IP 和相同端口。简体中文、英文及连接设置可在程序中切换。

捕获和 SDK 不随源码提供。测试输入可按 [SDK 生产器说明](tools/physx-sample/README.md)生成；桌面操作见 [Windows 使用说明](docs/distribution.md)。

## Unity 插件打包

插件可在 Windows Unity 编辑器的 Scene 视图中接收和回放 PVD 会话。在配置好 Rust 和 Windows MSVC 工具链的构建机上，从仓库根目录运行以下命令。

```powershell
./tools/package-unity.ps1
```

脚本构建 Windows x64 的 `pvd_unity.dll`，并生成 `output/unity/<时间戳>/com.pvdstudio.viewer-<版本>.tgz`。同一目录下的 `package/` 是展开的包。可通过 `-OutputDirectory` 指定产物目录。

在 Unity 的 Package Manager 中选择 **Add package from tarball**，打开生成的 `.tgz`。使用设备无需安装 Rust。安装、监听和缓存说明见 [Unity 插件](docs/unity.md)。

## 文件与存储

| 数据 | 默认位置与用途 |
| --- | --- |
| 回放历史 | 当前工作目录下的 `pvd-cache/history/`，默认滚动上限 8 GiB，可配置 |
| 接收缓存 | 历史目录下的 `receive/`，与回放历史独立，受可用空间保护 |
| 永久录制 | `captures/`；完整 SDK 原始流需在监听前启用自动录制 |
| 项目捕获 | Unity 和桌面将保留历史保存为一个 `.pvdcapture`。长录制在文件内[自动分段](docs/retained-capture.md#分段导入与导出)，可在桌面连续回放 |

暂停不停止接收和历史写入。临时历史会淘汰旧帧，不能替代永久录制。保存默认位于当前工作目录的 `captures/`，也可指定其他位置；已有文件不覆盖。

SDK communication stream、PVD3 数据库和项目捕获是不同格式，不能通过修改扩展名互换。原版数据库按需读取事件并使用磁盘历史；保存与裁剪有独立上限，详情见兼容矩阵。

## 命令行

生成输入后，可在仓库根目录执行：

```powershell
cargo run --locked -p pvd-cli -- inspect captures/physx-3.4.pvdstream
cargo run --locked -p pvd-cli -- verify captures/physx-3.4.pvdstream
cargo run --locked -p pvd-cli -- listen 0.0.0.0:5425 captures/tcp 60
```

其他查询、裁剪和导出命令见 [Windows 使用说明](docs/distribution.md)。

## 开发

```powershell
cargo fmt --all --check
cargo test --locked -p pvd-protocol -p pvd-core -p pvd-transport -p pvd-cli
cargo test --locked -p pvd-desktop -- --test-threads=1
```

真实 SDK 专项需要额外生成输入并显式执行 ignored 测试，见[验证说明](docs/validation.md)。Windows 分发包使用 `tools/package-windows.ps1`，构建和压缩选项见[打包规范](docs/packaging.md)。

| 文档 | 内容 |
| --- | --- |
| [使用说明](docs/distribution.md) | 连接、录制、回放与分析 |
| [文档索引](docs/README.md) | 设计与实现专题 |
| [架构](docs/architecture.md) | 模块职责、数据路径与资源边界 |
| [功能清单](docs/manual-checklist.md) | 实现与验收状态 |
| [路线图](docs/roadmap.md) | 未完成任务与完成条件 |

仓库保留源码、测试、工具、文档和依赖补丁许可证；构建产物、SDK、捕获及运行缓存由 `.gitignore` 排除。
