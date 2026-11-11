# 运动学目标采样验证

此工具使用固定 PhysX 3.4.2 SDK，分别记录 getter、原始 PVD 事件顺序、已完成帧和实时显示更新，核对休眠观察与待执行目标是否来自同一采样阶段。RepX 保存可导入的 Actor 参数快照，不恢复运动学目标、速度、WakeCounter 或睡眠内部状态；不从目标字段缺失或目标等于当前位置推断有效性。

先按 [SDK 生产器说明](../physx-sample/README.md)准备 commit `5e42a5f112351a223c19c17bb331e6c55037b8eb` 及其 checked 库，在仓库根目录执行：

```powershell
$projectRoot = (Resolve-Path .).Path
$sdkRoot = Join-Path $projectRoot '.deps/physx-3.4'
$buildDirectory = Join-Path $projectRoot '.deps/physx-repx-sleeping-build'
cmake -S tools/repx-sleeping -B $buildDirectory -G 'Visual Studio 18 2026' -A x64 "-DPVD_STUDIO_ROOT=$projectRoot" "-DPHYSX34_ROOT=$sdkRoot"
cmake --build $buildDirectory --config Debug --target pvd-repx-sleeping

$evidenceRoot = 'output/repx-sleeping-check'
New-Item -ItemType Directory -Path "$evidenceRoot/normal", "$evidenceRoot/initial-buffered" -ErrorAction Stop
$producer = Join-Path $buildDirectory 'Debug/pvd-repx-sleeping.exe'
& $producer "$evidenceRoot/normal/capture.pvdstream" "$evidenceRoot/normal"
& $producer "$evidenceRoot/initial-buffered/capture.pvdstream" "$evidenceRoot/initial-buffered" --initial-buffered

$env:PVD_REPX_SLEEPING_DIRECTORY = (Resolve-Path $evidenceRoot).Path
$env:PVD_REPX_SLEEPING_REPORT_DIRECTORY = Join-Path $env:PVD_REPX_SLEEPING_DIRECTORY 'reports'
cargo test --locked -p pvd-core --test repx_sleeping sdk_sleeping_kinematic_raw_order_and_committed_display -- --ignored --exact --nocapture
cargo test --locked -p pvd-core --test repx_sleeping sdk_initial_buffered_target_and_zero_frame_state -- --ignored --exact --nocapture
```

输出目录须为新目录。捕获、`oracle.json`、各封闭采样的官方参考 XML 和可选报告保存在指定目录；同名文件不覆盖。`PVD_REPX_SLEEPING_REPORT_DIRECTORY` 可省略。ignored 测试必须使用同次生成的捕获与 getter，不能混用其他输入。

两种模式各完成 10 帧，只创建一个运动学刚体和一个 Box Shape，覆盖直接设置目标、模拟中设置目标、目标执行、再次休眠、禁用模拟和移出/重入 Scene。每次等待休眠最多空跑四步，确认阶段必须由 SDK getter 报告已休眠。`--initial-buffered` 额外覆盖第一个完成帧之前设置目标的状态。运动学与非运动学的官方导入 getter 对照见[运动学工具](../repx-kinematics/README.md)。

报告区分 SDK 此刻状态、原始尾部、内存零帧初值和显示中的已完成帧。测试检查目标与休眠写入顺序、确认休眠时的零速度/零 WakeCounter、零帧混合观察，以及没有新完成帧时不发布显示更新。历史显示与 SDK 此刻状态不同本身不表示显示错误。

参考 XML 不包含可恢复的运动学目标；保存了 XML 不等于待执行目标已被恢复。参数快照可从已提交帧或具备必要字段的零帧状态导出，但不声称当前待执行目标或运动状态已恢复。测试保留无新帧时显示不更新的断言，并分别记录已提交显示和零帧参数快照结果；混合观察不能成为目标有效性判断。此工具不建立 PVD3 原版或其他 SDK 的兼容声明。
