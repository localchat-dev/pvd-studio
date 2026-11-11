# Windows 打包

入口为 [`tools/package-windows.ps1`](../tools/package-windows.ps1)。推荐带源码的 7z，ZIP 为兼容选项。压缩只影响分发文件，EXE 不加壳。

## 构建

在仓库根目录执行；首次准备依赖可运行 `cargo fetch --locked`。7z 需要已有的 7-Zip，脚本从 PATH 和标准安装目录查找，也可用 `-SevenZipPath` 指定。

```powershell
./tools/package-windows.ps1 -IncludeSource -ArchiveFormat 7z
```

脚本以锁定依赖离线构建桌面和 CLI，使用 `-j 2`，默认构建目录为 `target/`。产物写入仓库根目录下的 `output/dist/pvd-studio-windows-x64-<时间戳>/` 及同名压缩包，实际位置由脚本输出。需要独立构建目录时可传入 `-TargetDirectory output/release-target`，也支持绝对路径。

| 参数 | 用途 |
| --- | --- |
| `-IncludeSource` | 附带源码；省略时仍收集文档、许可证和存在的精选样例 |
| `-ArchiveFormat zip` | 生成 ZIP，也是脚本默认格式 |
| `-SevenZipPath` | 指定已有 `7z.exe` 的路径 |
| `-TargetDirectory` | 构建目录，复制其中 `release/` 下的两项 EXE |
| `-SkipBuild` | 复用已有程序，须先核对同源构建记录 |
| `-BuildStamp yyyyMMdd-HHmmss` | 指定输出标识，已有目录或压缩包不覆盖 |

7z 使用 `-t7z -mx=9 -mmt=2`，ZIP 使用 `-tzip -mm=Deflate -mx=9 -mmt=2`，完成后执行 `7z t`。没有 7-Zip 时，ZIP 使用 PowerShell `Optimal`；选择 7z 则报错。脚本不安装工具或上传产物。

## 内容

运行包包含两项 EXE、诊断启动脚本、说明、项目文档、补丁许可证、文件清单和空历史目录。`samples/` 仅包含脚本列明且实际存在的输入：SDK 样例位于 `captures/`，具名相机位于 `captures/cameras/`，原版配对文件位于 `captures/native/`。准备方法见[SDK 生产器](../tools/physx-sample/README.md)、[相机生产器](../tools/physx-cameras/README.md)和[原版验证](native-validation.md)。

源码包收集 `crates/`、`vendor/`、`test-support/`、`tools/`、公共文档与根配置。编译目录、捕获、日志、符号、缓存、`docs/local/` 和 `docs/CONTINUE-PVD-STUDIO.md` 不进入源码包。过滤规则由脚本统一维护。

`manifest.json` 记录文件相对路径、字节数和 SHA-256，不含清单自身。脚本不冻结编译输入；使用 `-SkipBuild` 不能仅凭文件时间判断源码对应关系。

## 核验

1. 按[验收方法](acceptance-plan.md)完成相关检查，记录构建命令、编译输入身份和两项 EXE 哈希。
2. 解压到独立 `output/` 目录，逐项比较清单与文件，再比较解压 EXE 和构建输出；带源码时核对源码副本及许可证。
3. 使用解压程序执行 CLI 和无窗口渲染检查，并查看图片。输入缺失时按[生产器说明](../tools/physx-sample/README.md)生成，未执行项保持待验。
4. 记录 Git 提交、压缩包 SHA-256、实际结果和限制。发布分发包时将这些信息随下载附件提供，不将个人构建目录写成版本入口。

在 PowerShell 中将 `$packageRoot` 指向解压目录后，可运行：

```powershell
$captureInput = (Resolve-Path 'captures/physx-3.4.pvdstream').Path
& (Join-Path $packageRoot 'pvd-cli.exe') verify $captureInput
if ($LASTEXITCODE -ne 0) { throw 'Packaged CLI verification failed' }
$renderOutput = Join-Path $packageRoot 'render-check.png'
$renderProcess = Start-Process -FilePath (Join-Path $packageRoot 'pvd-desktop.exe') `
    -ArgumentList @('--render-check', "`"$captureInput`"", "`"$renderOutput`"", '0', 'timeline', '1440', '960') `
    -WindowStyle Hidden -Wait -PassThru
if ($renderProcess.ExitCode -ne 0) { throw 'Packaged render check failed' }
```

检查图片与缓存留在解压验证目录。包体大小随输入和构建变化，不作为固定验收阈值。
