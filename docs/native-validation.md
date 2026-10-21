# 原版配对与 Windows 交互验证

PVD3 数据库验证需要原版保存文件、同次 SDK TCP 流和独立 SDK 真值。仓库不分发原版程序、符号或捕获；需要自行准备与[参考身份](evidence.md)匹配的软件和所需 SDK 工具链。

## 配对输入

测试约定把输入放在仓库根目录的 `captures/native/`：

| 场景 | 必需文件 |
| --- | --- |
| baseline | `baseline-paired-native.pxd2`、`baseline-paired.pvdstream`、`baseline-paired.csv` |
| rich | `rich-paired-native.pxd2`、`rich-paired.pvdstream`、`rich-paired.csv`、`rich-paired.mesh.json`、`rich-paired.query.json` |
| 关节限位 | `limits/joint-limits-live-native.pxd2`、`limits/joint-limits-live.pvdstream`、`limits/joint-limits-live.joints.json` |

每组必须来自同一次连接。内部 local ID 和 SDK 指针需要通过通信目录映射比较，不能直接按数值相等判断。带固定输入摘要或数量断言的测试另需匹配的原始样例；目录名称本身不表示任意新捕获都满足该断言。

## 采集方法

先构建 [PhysX 3.4 生产器](../tools/physx-sample/README.md)，启动 PVD3 监听，并确认其实际端口。以下命令使用原版端口 5425，代理端口 15445；已有文件不覆盖。

在第一个终端运行代理：

```powershell
New-Item -ItemType Directory -Path captures/native -Force | Out-Null
python tools/pvd-container/capture_proxy.py captures/native/rich-paired.pvdstream --listen-port 15445 --target-port 5425
```

在第二个终端向代理发送真实 SDK 数据：

```powershell
& ./.deps/physx-sample-build/Release/pvd-physx-sample.exe --host 127.0.0.1 --port 15445 --frames 120 --flags all --lifecycle --rich --emit-error --truth captures/native/rich-paired.csv --mesh-truth captures/native/rich-paired.mesh.json --query-truth captures/native/rich-paired.query.json
```

源端结束后，在 PVD3 中 Save 为 `captures/native/rich-paired-native.pxd2`，再通过 File/Open 重开，核对帧数及场景。baseline 使用 `--flags debug --lifecycle`，省略 rich/error/mesh/query 参数，并保存到表中对应文件。关节限位使用 [features 生产器](../tools/physx-features/README.md)的 SocketTransport 模式，真值与同次流一并保留。

代理只转发并保存字节，不能证明原版已经处理或保存完成。不要用 SDK 文件流更名或项目自写容器替代原版 Save 结果。

## 自动比较

准备输入后显式执行：

```powershell
python tools/pvd-container/inspect_native.py captures/native/rich-paired-native.pxd2
cargo test --locked -p pvd-core --test native --test native_debug_draw -- --include-ignored --nocapture
cargo run --locked -p pvd-cli -- verify captures/native/rich-paired-native.pxd2
```

检查场景帧、属性、引用、代际、资源、绘制、分析和错误的来源语义；具体范围见[格式说明](original-file-format.md)。没有输入时保留 ignored 状态，不能报告为已通过。文件互通验收还需由 PVD3 实际打开、正倒放和重存，项目自读不够。

桌面 CSV 对照专项另需在 `captures/native/reference/` 准备同一 rich 输入的 `native-profile.csv` 和 `native-memory.csv`。用明确的参考版本 CLI 生成并记录其 Git 提交；被测桌面与参考 CLI 使用相同源文件。

## 原生窗口

原生交互需要分别检查对象选择/属性联动、播放和返回实时、菜单连续编辑、文件对话框、剪贴板、停靠、语言、布局保存、最小化恢复、DPI 和 IME。记录程序提交、输入、操作及实际结果。

无窗口渲染和 egui 合成事件用于自动回归，不能替代系统窗口操作。曾验证的个别手势不能扩展为完整交互已通过；未验范围见[验证说明](validation.md)。
