# PVD 录制包

录制包是包含 `.pxd2` 成员的受限 ZIP 文件。桌面打开和 CLI 读取先检查实际文件签名，再验证目录和录制内容；扩展名或 ZIP 标记本身不表示格式兼容。文件选择器包含 `.pvd`。

内层只允许已支持的 [SDK communication stream](protocol.md) 或 [PVD3 database](original-file-format.md)，仍由各自解析器验证版本、完整结构和资源限制。旧 `.pxd`、递归 ZIP、项目 `.pvdcapture` 和任意其他内容不能作为包内录制读取。

## 打开与导出

打开后使用内层录制进行回放、逐帧、对象查看、曲线和分析。`inspect`、`verify`、`frame`、分析 CSV 和 RepX 等入口沿用内层格式已有范围，不扩展 SDK 或原版容器兼容性。

- SDK 内层可另存完整 `.pvdstream`、导出 [SDK 帧区间](clips.md)，以及使用完整文件分析和时间线。
- 原版数据库内层可按[原版容器保存](native-export.md)的现有范围另存或裁剪 `.pxd2`，并使用对应分析入口。
- 导出使用解出的原始字节和其来源身份，不把外层 ZIP 文件当作 SDK 流或原版数据库。输出是内层录制，当前不写出 ZIP 包。
- SDK 片段定义的 JSON 附属文件需要稳定的原始流路径。请先另存为普通 `.pvdstream` 并重新打开，再保存片段定义；不会猜测包内成员的本地路径，也不会在包内写定义。

## 支持范围

只接受单卷 ZIP32、stored 或 deflate，版本要求不高于 2.0。加密、ZIP64、AES 扩展、特殊文件、自解压前缀、未索引的间隙和目录之外的额外记录均拒绝。普通成员按本地头到中央目录连续索引，范围不能重叠；末尾目录及其注释必须结束于文件末尾。

必须存在唯一的普通文件成员，其名称精确以小写 `.pxd2` 结尾。名称须为有效 UTF-8，最多 259 字节；拒绝控制字符、绝对路径、盘符、反斜杠、空路径段以及 `.` / `..`。按 ASCII 大小写比较重复的成员名也会拒绝。目录可保留为空目录记录，符号链接等特殊成员不支持。

中央目录与本地头的名称、标志、方法、时间及 CRC/长度必须一致。使用 data descriptor 时允许本地 CRC/长度占位为零，末尾描述必须与目录一致。所选录制必须解码到准确声明长度、用尽其压缩数据并通过 CRC32；不能靠修改目录中的大小绕过实际输出限制。

其他普通成员仅检查结构，不解压、不校验其数据 CRC。任何成员名都不会成为磁盘输出路径，不提取包内附属资源。

| 限制 | 上限 |
| --- | ---: |
| 整个压缩输入 | 512 MiB |
| 所选录制解压后大小 | 512 MiB |
| 中央目录 | 4 MiB |
| 目录成员数 | 4096 |
| 单次输入读取 | 64 KiB |

接口可收紧这些上限，不能提高。目录扫描及分块解压检查取消；已解压量一旦超出声明值即停止。取消或失败不会留下解压文件。压缩字节、解压字节和录制模型可能同时驻留，因此这些独立上限不是总内存占用上限。

## 证据与验证

匹配 PhysX Visual Debugger 3 的 EXE/MAP 静态调用链证明：`.pvd` 经 ZIP reader 取出录制后进入既有内容解析器，具体依据见[文件格式调查](original-file-format.md)。原版全量提取和多候选覆盖行为不属于这里的受限读取范围。

默认回归检查构造 ZIP 的正常读取、目录/CRC/边界错误、递归内容、重复成员、预算和取消，以及核心、CLI、桌面消费者是否始终使用内层字节：

```powershell
cargo test --locked -p pvd-core --lib pvd_archive::tests
cargo test --locked -p pvd-core --test pvd_archive_consumers
cargo test --locked -p pvd-cli pvd_archive -- --test-threads=1
cargo test --locked -p pvd-desktop studio::archive::tests -- --test-threads=1
```

真实内层专项使用独立准备的原版数据库和 SDK 流，由测试构造 ZIP，再与未包装输入逐帧比较。这证明包装层保留了内层状态，不证明 ZIP 由原版生成或已经被原版打开。先按[原版配对方法](native-validation.md)准备输入，也可通过环境变量指定其他相应捕获：

```powershell
$env:PVD_ARCHIVE_NATIVE_CAPTURE = (Resolve-Path "captures/native/baseline-paired-native.pxd2").Path
$env:PVD_ARCHIVE_SDK_CAPTURE = (Resolve-Path "captures/native/baseline-paired.pvdstream").Path
cargo test --locked -p pvd-core --test pvd_archive_consumers pvd_archive_wrapped_real_native_and_sdk_match_the_unwrapped_capture -- --ignored --nocapture
```

原版实际生成 `.pvd` 的打开、播放、附属资源和重存互通仍需独立配对；普通 ZIP、自写包或真实内层文件都不能替代这项验收。
