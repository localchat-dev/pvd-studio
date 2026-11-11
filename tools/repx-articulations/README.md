# RepX Articulation 互验

独立 PhysX 3.4.2 生产器创建分叉 Articulation、Link、InboundJoint、跨 Articulation 的 FixedJoint、Static/Dynamic 刚体和混合 Aggregate。四帧覆盖参数更新、移出/移回 Aggregate、销毁与重建，以及客户端通知标志的全部 16 个组合和清零。

## 运行

先按 [SDK 生产器说明](../physx-sample/README.md)准备固定 SDK commit `5e42a5f112351a223c19c17bb331e6c55037b8eb`。在仓库根目录执行：

```powershell
.\tools\repx-articulations\build.ps1
cargo build --locked -p pvd-cli
python tools/repx-articulations/verify.py --output output/repx-articulations-check
$env:PVD_REPX_ARTICULATION_DIRECTORY = (Resolve-Path 'output/repx-articulations-check').Path
cargo test --locked -p pvd-core --test repx_articulation -- --ignored
```

输出目录必须尚不存在；省略 `--output` 时在 `output/repx-articulation-validation` 下创建时间戳目录。目录包含同次捕获、官方参考 XML、getter 真值、导出/导入结果和 `result.json`。`--producer-only` 仅验证 SDK 参考 XML 的官方导入，不验证 Rust。

互验分别检查整帧，以及每个 Articulation、Link 和普通 Actor 的选择闭包。通过官方 importer 重建后，比较树关系、质量/惯量、位姿/速度、关节参数、组成员和容量；比较对象均来自 SDK getter，不能以 Rust 输出作为自身真值。

当前四帧样例的官方参考 XML 与项目整帧导出各核对 1,981 个 getter 值；43 份选中导出核对 16,525 个值。实际运行结果保存在输出目录的 `result.json`。

## 边界

仅针对 PhysX 3.4.2。每棵树最多 64 Link，父子关系和 InboundJoint 引用必须完整且代次有效；Aggregate 的容量按全部 Link 数量计算。选中任一 Link 会包含整棵树，并继续扩展 Aggregate 成员及相连的扩展关节。

Static、Dynamic 和 Link 的 `ClientBehaviorFlags` 保留 SDK 定义的 trigger、contact、constraint break 和 scene query 四个位，以官方枚举名写入 XML；缺失字段、错误类型和未知位拒绝。这里只恢复 Actor 参数，不创建 Scene client 或回调。

InboundJoint 支持 `eTARGET` 与受限 `eERROR`。`eERROR` 仅接受有限原始 f32、w 为正/负零，且原 f32 转 f64 所得长度与 1 的差严格小于 `9e-5`；不归一化或变号。SDK XML importer 在设置 DriveType 前读取 TargetOrientation，这个内侧范围同时满足默认 `eTARGET` 的单位检查；一般 `eERROR` 旋转向量仍拒绝。Link 支持 CCD、CCD friction、pose integration preview、speculative CCD、CCD max contact impulse 标志，生产器覆盖组合、清零及后续更新；拒绝 kinematic、未知位和互斥的 CCD/speculative CCD。InboundJoint 在 SDK XML 中内嵌于 Link，不保留独立序列化 ID；Articulation、Link 与外部扩展关节保留 ID。

这些互验不代表原版 PVD3 全部 RepX 入口行为已验收，也不恢复求解器内部状态。完整导出范围见 [RepX 文档](../../docs/repx.md)。

## eERROR 交集互验

独立的 18 帧模式使用同一 InboundJoint，覆盖 `eTARGET → eERROR → eTARGET`、非轴向单位值、非精确单位值、负向与负零，以及项目 `9e-5` 内侧范围和 SDK `1e-4` 检查的内外控制。它与默认四帧样例分别生成输入，不能混用。准备好上述程序后运行：

```powershell
python tools/repx-articulations/verify-error-drive.py --output output/repx-articulations-error-check
$env:PVD_REPX_ERROR_DRIVE_DIRECTORY = (Resolve-Path 'output/repx-articulations-error-check').Path
$env:PVD_REPX_ERROR_DRIVE_REPORT = Join-Path $env:PVD_REPX_ERROR_DRIVE_DIRECTORY 'raw-frame-report.json'
cargo test --locked -p pvd-core --test repx_articulation_error_drive error_drive_inner_subset_matches_requested_sdk_bits_and_committed_messages -- --ignored --exact --nocapture
```

`--producer` 与 `--cli` 可指定实际程序，`--output` 必须尚不存在；省略输出时使用 `output/repx-error-drive-validation` 下的时间戳目录。生产器的 `--error-drive capture.pvdstream output-directory` 写同次流、getter、独立请求位模式、SDK `isUnit` 和 step 前后字节边界；`--import-drive input.xml truth.json roundtrip.xml` 记录导入及资源清理期间的 SDK 诊断。

Python 与 Rust 各自保留 18 组显式请求位常量，先检查 setter、getter 和捕获未改写目标。9 帧符合项目范围，整帧与 Link 选中导出各 9 份，官方导入后逐分量 f32 位模式必须相同；其他 getter 沿用默认互验容差。其余 9 帧必须拒绝且不生成文件。Rust 另在原始 Scene `EndSection` 绑定目标与模式，按非顺序帧恢复核对；报告文件使用独占创建。`eERROR` 的 q 与 −q 是相反误差，不能按旋转四元数符号等价比较。

SDK 参考 writer 的六位 `%g` 可能量化临界值。结果单独保存原参考导入与诊断，并以仅提高 TargetOrientation 十进制精度的 reader-probe 核对官方 reader；不替换目标、DriveType 或其他字段。4 帧通过 SDK 单位检查但超出项目内侧范围，仍作为项目拒绝控制。非单位 `eERROR` 在源 SDK 合法，fresh importer 的默认 `eTARGET` 会拒绝其目标；这些诊断与默认值差异单独记录。

范围限定于固定 Windows PhysX 3.4.2 的已验证 reader 与正常浮点运算模型；不代表所有 `eERROR`、浮点模式或其他 SDK 兼容。
