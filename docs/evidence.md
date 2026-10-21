# 参考软件与证据范围

项目以 NVIDIA PhysX Visual Debugger 3（PVD3）为功能参考。静态调查使用下表标识的 Windows x64 文件；文件本身不包含在仓库中。地址、符号和格式结论只适用于匹配版本，其他版本需要独立验证。

## 参考文件身份

| 文件 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `PVD3.exe` | 2206608 | `8a357c845b2622b19ca4c391086de83c9de2e45787d518d972f2490f1c524420` |
| `PVDNext.pdb` | 3117056 | `9f94a265e38dded40343da5cb957a0b63125065b821fcb2d9db276d98cde301e` |
| `PVDNext.map` | 5246331 | `be12ee27da57aae194ff4fe14aa86be5d203a76e5b8e65eeb6fcc6339c39c6d5` |
| `PVD 3 User Manual.pdf` | 1070550 | `4e8e85de15987eb11ecf9a7a74330369283c395dc1db0cc41999cbfd3a0d116b` |

EXE 与 PDB 的 CodeView GUID 均为 `795b547e-7ade-467a-942d-4c56e41d2824`，Age 为 1。这证明标识匹配，不证明 PDB 包含完整源码。EXE 的 FileVersion/ProductVersion 为 `0.0.0.0`，不能用它推断精确发布版本。

## 事实与推断

- 12 页手册覆盖对象、属性、回放、文件裁剪、RepX 和分析界面；逐项对照见[功能清单](manual-checklist.md)。手册更新记录和 PDF 元数据不能替代 EXE 的发布身份。
- 手册的端口示例为 5435，官方 3.4/4.1 集成指南的默认值为 5425；连接双方必须使用同一实际端口。
- MAP 符号用于定位调用链，不能单凭函数名推断实现。保存/读取和历史结构的指令、虚表及真实容器依据见[原版格式](original-file-format.md)与[历史回放](tcp-playback.md)。
- 原版保存数据库、SDK communication stream 和项目捕获是不同格式，扩展名不能证明可互换。
- SDK 源码解释发送布局；真实 SDK 输出与独立 getter 真值验证消费路径；原版保存/重开和配对文件验证原版互通。构造输入不能独立证明兼容。

## SDK 范围

PhysX 3.4.2 固定提交为 `5e42a5f112351a223c19c17bb331e6c55037b8eb`，PhysX 4.1.2 为 `a2c0428acab643e60618c681b501e86f7fd558cc`。生产器和验证命令分别见 [3.4](../tools/physx-sample/README.md) 与 [4.1](../tools/physx41-sample/README.md)。真实数据的地址、计时和分配会随运行变化，应比较同次真值而非要求重录后哈希相同。

各对象、分析和导出范围见[兼容矩阵](compatibility.md)。其他 SDK、APEX/GPU、旧格式、原版写出和完整产品验收仍按[路线图](roadmap.md)分别建立证据。

## 复核

在 PowerShell 中将 `$pvdDirectory` 指向参考软件的安装目录，再执行：

```powershell
Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $pvdDirectory 'PVD3.exe')
(Get-Item -LiteralPath (Join-Path $pvdDirectory 'PVD3.exe')).VersionInfo | Format-List FileVersion,ProductVersion,ProductName,OriginalFilename
rg -n 'onExportSelectedRepX|onExportVisibleRepX|doParseEvents' (Join-Path $pvdDirectory 'PVDNext.map')
```

这些命令只读取文件。静态文件检查、离屏图片和原生窗口操作具有不同证据范围；原版配对方法见[原版验证](native-validation.md)。
