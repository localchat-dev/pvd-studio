# Windows 原生文件对话框

桌面的 `file_picker` 模块使用稳定发布版 Microsoft `windows = 0.61.3` 调用 `IFileDialog`。该版本已存在于工作区锁文件并兼容当前 Rust 工具链；新增的 COM/Shell features 只影响桌面 crate，不进入协议与回放核心。

支持打开捕获、选择自动录制目录，以及选择原始 SDK 流、片段、RepX、CSV 的输出路径。对话框只选择路径，不负责加载、导出或写入文件；这些操作继续走应用现有任务和错误处理。

## 线程、取消与错误

`FilePicker::request(kind, initial_path, language, egui_context)` 只启动一个独立 STA 线程，同一时间只允许一个待完成选择。COM 接口的创建、配置、显示、读取和释放全部在该线程完成，之后解除 COM 初始化。应用用 `poll()` 非阻塞读取结果；完成时主动请求 egui 重绘。COM 的线程与对象生命周期要求参考 [Microsoft COM 初始化说明](https://learn.microsoft.com/en-us/windows/win32/learnwin32/initializing-the-com-library)。

结果 `Ok(Some(path))` 为已选路径，`Ok(None)` 为用户取消，`Err(Message)` 为系统、路径或线程错误。`IFileDialog::Show` 的 `ERROR_CANCELLED` 单独识别，其他 HRESULT 保留为诊断，不把失败显示成取消；依据 [Microsoft Common Item Dialog](https://learn.microsoft.com/en-us/windows/win32/shell/common-file-dialog)。

对话框由用户点击对应按钮时触发。它不在启动、录制接收或 `--render-check` 时自动显示。没有用于驱动用户桌面的键鼠操作。当前未提供应用内强制关闭原生对话框接口；用户可在对话框中取消。

## 路径与格式

- 原生接口使用 UTF-16 和 `OsString` 传递路径，正确保留普通中文和非 ASCII 字符；包含 NUL 的输入拒绝处理。
- 打开过滤器列出 `.pvdstream/.pxd/.pxd2/.partial` 和所有文件。过滤器只是文件选择，不意味着所有 `.pxd/.pxd2` 容器都兼容；后续解析器仍负责辨认和诊断。
- 原始流与片段保存只选择 `.pvdstream`；RepX 和 CSV 分别使用 `.repx/.csv`。缺失扩展名时补充目标扩展名，其他扩展名拒绝，避免用原版文件后缀标记自有输出。
- 选择输出时已有文件报错，与应用 `create_new` 的保存语义一致；不会提供覆盖确认后实际拒绝，也不在后台静默覆盖文件。应用写入时仍须使用 `create_new` 处理选择之后发生的路径竞争。
- 对话框标题、过滤器说明与错误上下文使用请求时的应用语言；Windows 系统控件由操作系统语言决定。
- 设置 `FOS_NOCHANGEDIR` 和 `FOS_DONTADDTORECENT`，不改变工作目录，也不把选择加入 Windows 最近文档列表。
- 其他操作系统返回明确的未支持错误，没有宣称跨平台原生对话框已完成。

## 验证边界

自动化验证只编译 Windows COM 路径，并使用注入的非图形后端验证异步请求、忙碌互斥、取消/成功/错误、中文路径与输出扩展名；另外直接验证 HRESULT 取消分类。所有测试都不调用 `Show`，不打开任何真实系统对话框。

```powershell
cargo test --locked -p pvd-desktop file_picker::tests
cargo test --locked -p pvd-desktop file_picker::windows_dialog::tests
cargo clippy --locked -p pvd-desktop --all-targets -- -D warnings
```

原生对话框的前后窗口顺序、中文输入法、无效目录、DPI、选择及取消后恢复需要在目标系统独立验收。