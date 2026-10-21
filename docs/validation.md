# 验证说明

功能与格式范围见[兼容矩阵](compatibility.md)，未完成项见[路线图](roadmap.md)。验证需要区分构造输入、真实 SDK 输出、原版配对文件和原生窗口操作；一种检查不能替代其他检查。

## 编译与默认测试

在仓库根目录执行：

```powershell
cargo fmt --all --check
cargo test --locked -p pvd-protocol -p pvd-core -p pvd-transport -p pvd-cli
cargo test --locked -p pvd-desktop -- --test-threads=1
cargo clippy --locked -p pvd-desktop --all-targets -- -D warnings
```

依赖已缓存时可加 `--offline`。桌面测试使用单线程，减少并行执行对时序断言的影响。默认测试中的 ignored 项需要额外输入，不能计为已执行或已通过。

## 真实输入

PhysX SDK 由独立生产器构建，详见[PhysX 3.4 生产器](../tools/physx-sample/README.md)。完整检查入口为：

```powershell
./tools/physx-sample/build.ps1
./tools/verify-sdk.ps1
```

PhysX 4.1.2 使用 `tools/physx41-sample/verify.ps1`，不能由 3.4 的结果外推。其他专项的命令与输入要求见[验收方法](acceptance-plan.md)及对应专题。

捕获和独立真值必须来自同一次运行。原版文件专项需要另行准备 PVD3 保存的容器及同次原始流，见[原版验证](native-validation.md)。包含固定摘要、对象数或帧数断言的测试必须使用匹配输入；重新生成的文件不能直接替代。例如 `PVD_SHARED_HISTORY_RAW` 指向固定 970 帧捕获，来源不足时保留忽略状态。

保留历史保存专项可在生成所需 rich/all 输入后执行：

```powershell
$env:PVD_RETAINED_SDK_DIRECTORY = (Resolve-Path 'captures').Path
cargo test --locked -p pvd-desktop studio::retained_capture::tests::sdk::actual_sdk_disk_save_reopen_and_display_match_independent_raw_prefixes -- --ignored --exact --nocapture
```

## 界面与分发包

无窗口渲染使用 `--render-check`，支持指定输入、帧、面板、语言和尺寸，见[验收方法](acceptance-plan.md)。需要查看生成图片；进程成功退出本身不证明显示正确。离屏检查不能替代文件对话框、停靠、IME、动态 DPI 等原生交互验收。

分发包按[打包规范](packaging.md)核对文件清单、许可证、源码副本和程序哈希，并使用解压后的程序执行 CLI 和渲染检查。[界面示例](images/README.md)只展示界面形态。

## 已知问题与未验范围

- `network::tests::live_refresh_publishes_without_a_half_second_gate` 在并行测试中出现过时序失败；隔离及完整单线程测试通过，完整并行测试仍需复核。
- 部分英文和录制相机配置存在顶部工具栏文字或路径绘制缺失，尚未定位。
- 原生保存对话框、完整停靠、动态 DPI、其他 IME、跨机器负载、长期边接收边保存及资源趋势未全部验收。
- 原版多页、非空起点、更多绘制、错误生命周期和正式写出/裁剪的范围分别维护在兼容矩阵中。

验证记录应绑定 Git 提交、输入摘要、参数和实际结果，分别记录失败及未执行项。性能测量需区分核心恢复、模型提交、GPU 完成与原生显示，并注明机器和构建配置。
