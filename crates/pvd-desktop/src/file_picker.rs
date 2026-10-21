use crate::i18n::{Language, Message};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    OpenCapture,
    SaveCapture,
    SaveClip,
    SaveRetainedCapture,
    SaveRepx,
    SaveCsv,
    SaveAnalysisCsv,
    RecordingDirectory,
}

impl PickerKind {
    fn extension(self) -> Option<&'static str> {
        match self {
            Self::OpenCapture | Self::RecordingDirectory => None,
            Self::SaveCapture | Self::SaveClip => Some("pvdstream"),
            Self::SaveRetainedCapture => Some("pvdcapture"),
            Self::SaveRepx => Some("repx"),
            Self::SaveCsv | Self::SaveAnalysisCsv => Some("csv"),
        }
    }

    fn title(self, language: Language) -> &'static str {
        match self {
            Self::OpenCapture => language.text("Open physics capture", "打开物理捕获"),
            Self::SaveCapture => language.text("Save captured SDK stream", "保存捕获的 SDK 流"),
            Self::SaveClip => language.text("Save recording clip", "保存录制片段"),
            Self::SaveRetainedCapture => {
                language.text("Save retained project capture", "保存保留范围的项目捕获")
            }
            Self::SaveRepx => language.text("Export RepX", "导出 RepX"),
            Self::SaveCsv => language.text("Export CSV", "导出 CSV"),
            Self::SaveAnalysisCsv => {
                language.text("Export recording analysis CSV", "导出整份录制分析 CSV")
            }
            Self::RecordingDirectory => language.text("Choose recording directory", "选择录制目录"),
        }
    }
}

type Outcome = Result<Option<PathBuf>, Message>;

pub struct PickerResult {
    pub kind: PickerKind,
    pub outcome: Outcome,
}

struct Pending {
    kind: PickerKind,
    receiver: Mutex<mpsc::Receiver<Outcome>>,
}

#[derive(Default)]
pub struct FilePicker {
    pending: Option<Pending>,
}

impl FilePicker {
    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn request(
        &mut self,
        kind: PickerKind,
        initial_path: impl AsRef<Path>,
        language: Language,
        context: egui::Context,
    ) -> Result<(), Message> {
        let initial_path = initial_path.as_ref().to_path_buf();
        self.request_with(kind, context, move || {
            if matches!(
                kind,
                PickerKind::SaveCapture | PickerKind::SaveClip | PickerKind::SaveRetainedCapture
            ) {
                crate::atomic_output::create_parent_directory(&initial_path).map_err(|error| {
                    Message::detail(
                        "Cannot create recording directory",
                        "无法创建录制目录",
                        error,
                    )
                })?;
            }
            native_dialog(kind, &initial_path, language)
        })
    }

    fn request_with(
        &mut self,
        kind: PickerKind,
        context: egui::Context,
        work: impl FnOnce() -> Outcome + Send + 'static,
    ) -> Result<(), Message> {
        if self.is_busy() {
            return Err(Message::new(
                "A file dialog is already open",
                "已有一个文件对话框正在等待选择",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("pvd-file-dialog".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| {
                        Err(Message::new(
                            "The file dialog worker failed",
                            "文件对话框线程失败",
                        ))
                    });
                let _ = sender.send(result);
                context.request_repaint();
            })
            .map_err(|error| {
                Message::detail(
                    "Could not start the file dialog",
                    "无法启动文件对话框",
                    error,
                )
            })?;
        self.pending = Some(Pending {
            kind,
            receiver: Mutex::new(receiver),
        });
        Ok(())
    }

    pub fn poll(&mut self) -> Option<PickerResult> {
        let pending = self.pending.as_mut()?;
        let receiver = pending
            .receiver
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        let outcome = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(Message::new(
                "The file dialog closed without a result",
                "文件对话框关闭且未返回结果",
            )),
        };
        let kind = pending.kind;
        self.pending = None;
        Some(PickerResult { kind, outcome })
    }
}

fn validate_selection(kind: PickerKind, mut path: PathBuf) -> Result<PathBuf, Message> {
    if path.as_os_str().is_empty() {
        return Err(Message::new("The selected path is empty", "所选路径为空"));
    }
    if let Some(extension) = kind.extension() {
        if path.extension().is_none() {
            path.set_extension(extension);
        } else if !path
            .extension()
            .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        {
            return Err(Message::new(
                format!("Choose a .{extension} filename for this operation"),
                format!("此操作需要选择 .{extension} 文件名"),
            ));
        }
    }
    Ok(path)
}

#[cfg(not(target_os = "windows"))]
fn native_dialog(_kind: PickerKind, _initial_path: &Path, _language: Language) -> Outcome {
    Err(Message::new(
        "Native file dialogs are currently available on Windows",
        "原生文件对话框当前仅支持 Windows",
    ))
}

#[cfg(target_os = "windows")]
fn native_dialog(kind: PickerKind, initial_path: &Path, language: Language) -> Outcome {
    windows_dialog::show(kind, initial_path, language)
}

#[cfg(target_os = "windows")]
mod windows_dialog {
    use super::*;
    use std::{
        ffi::{OsStr, OsString},
        os::windows::ffi::{OsStrExt, OsStringExt},
    };
    use windows::{
        Win32::{
            Foundation::ERROR_CANCELLED,
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{
                Common::COMDLG_FILTERSPEC, FOS_DONTADDTORECENT, FOS_FILEMUSTEXIST,
                FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_NOREADONLYRETURN, FOS_PATHMUSTEXIST,
                FOS_PICKFOLDERS, FOS_STRICTFILETYPES, FileOpenDialog, FileSaveDialog, IFileDialog,
                IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
            },
        },
        core::{HRESULT, PCWSTR, PWSTR},
    };

    struct Apartment;

    impl Apartment {
        fn new() -> windows::core::Result<Self> {
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
            }
            Ok(Self)
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }

    struct ShellPath(PWSTR);

    impl Drop for ShellPath {
        fn drop(&mut self) {
            unsafe {
                CoTaskMemFree(Some(self.0.0.cast()));
            }
        }
    }

    fn wide(value: &OsStr) -> Result<Vec<u16>, Message> {
        let mut result: Vec<_> = value.encode_wide().collect();
        if result.contains(&0) {
            return Err(Message::new(
                "A path contains a null character",
                "路径包含空字符",
            ));
        }
        result.push(0);
        Ok(result)
    }

    fn os_error(error: windows::core::Error) -> Message {
        Message::detail(
            "Windows file dialog failed",
            "Windows 文件对话框失败",
            error,
        )
    }

    fn show_result(result: windows::core::Result<()>) -> Result<bool, Message> {
        match result {
            Ok(()) => Ok(true),
            Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => Ok(false),
            Err(error) => Err(os_error(error)),
        }
    }

    pub(super) fn show(kind: PickerKind, initial_path: &Path, language: Language) -> Outcome {
        wide(initial_path.as_os_str())?;
        // Every COM object stays in this worker's apartment and is released before it uninitializes.
        let _apartment = Apartment::new().map_err(os_error)?;
        let dialog: IFileDialog = unsafe {
            CoCreateInstance(
                if kind.extension().is_some() {
                    &FileSaveDialog
                } else {
                    &FileOpenDialog
                },
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(os_error)?
        };
        let title = wide(OsStr::new(kind.title(language)))?;
        let mut options =
            FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
        if kind == PickerKind::RecordingDirectory {
            options |= FOS_PICKFOLDERS;
        } else if kind == PickerKind::OpenCapture {
            options |= FOS_FILEMUSTEXIST;
        } else {
            options |= FOS_STRICTFILETYPES | FOS_NOREADONLYRETURN;
        }
        unsafe {
            dialog.SetTitle(PCWSTR(title.as_ptr())).map_err(os_error)?;
            dialog.SetOptions(options).map_err(os_error)?;
        }

        let filters: Vec<(&str, &str)> = match kind {
            PickerKind::OpenCapture => vec![
                (
                    language.text("Physics capture files", "物理捕获文件"),
                    "*.pvdstream;*.pxd;*.pxd2;*.pvdcapture;*.partial",
                ),
                (language.text("All files", "所有文件"), "*.*"),
            ],
            PickerKind::SaveRetainedCapture => vec![(
                language.text("Project capture", "本项目捕获"),
                "*.pvdcapture",
            )],
            PickerKind::SaveCapture | PickerKind::SaveClip => vec![(
                language.text("Captured SDK stream", "捕获的 SDK 流"),
                "*.pvdstream",
            )],
            PickerKind::SaveRepx => vec![("PhysX RepX", "*.repx")],
            PickerKind::SaveCsv | PickerKind::SaveAnalysisCsv => vec![("CSV", "*.csv")],
            PickerKind::RecordingDirectory => Vec::new(),
        };
        let filters = filters
            .into_iter()
            .map(|(label, pattern)| Ok((wide(OsStr::new(label))?, wide(OsStr::new(pattern))?)))
            .collect::<Result<Vec<_>, Message>>()?;
        let specifications: Vec<_> = filters
            .iter()
            .map(|(label, pattern)| COMDLG_FILTERSPEC {
                pszName: PCWSTR(label.as_ptr()),
                pszSpec: PCWSTR(pattern.as_ptr()),
            })
            .collect();
        if !specifications.is_empty() {
            unsafe {
                dialog.SetFileTypes(&specifications).map_err(os_error)?;
            }
        }
        let extension = kind
            .extension()
            .map(|value| wide(OsStr::new(value)))
            .transpose()?;
        if let Some(extension) = &extension {
            unsafe {
                dialog
                    .SetDefaultExtension(PCWSTR(extension.as_ptr()))
                    .map_err(os_error)?;
            }
        }
        if !initial_path.as_os_str().is_empty() {
            let absolute = std::path::absolute(initial_path).map_err(|error| {
                Message::detail("Invalid initial dialog path", "对话框初始路径无效", error)
            })?;
            let directory = if absolute.is_dir() {
                Some(absolute.as_path())
            } else {
                absolute.parent()
            };
            if let Some(directory) = directory.filter(|directory| directory.is_dir()) {
                let directory = wide(directory.as_os_str())?;
                let folder: IShellItem = unsafe {
                    SHCreateItemFromParsingName(PCWSTR(directory.as_ptr()), None)
                        .map_err(os_error)?
                };
                unsafe {
                    dialog.SetFolder(&folder).map_err(os_error)?;
                }
            }
            if kind != PickerKind::RecordingDirectory
                && !absolute.is_dir()
                && let Some(file_name) = absolute.file_name()
            {
                let file_name = wide(file_name)?;
                unsafe {
                    dialog
                        .SetFileName(PCWSTR(file_name.as_ptr()))
                        .map_err(os_error)?;
                }
            }
        }
        if !show_result(unsafe { dialog.Show(None) })? {
            return Ok(None);
        }
        let item = unsafe { dialog.GetResult().map_err(os_error)? };
        let text = ShellPath(unsafe { item.GetDisplayName(SIGDN_FILESYSPATH).map_err(os_error)? });
        if text.0.is_null() {
            return Err(Message::new(
                "Windows returned no filesystem path",
                "Windows 未返回文件系统路径",
            ));
        }
        let path = PathBuf::from(OsString::from_wide(unsafe { text.0.as_wide() }));
        let path = validate_selection(kind, path)?;
        if kind.extension().is_some() && path.exists() {
            return Err(Message::new(
                "The output already exists; choose a new filename",
                "输出文件已存在，请选择新的文件名",
            ));
        }
        Ok(Some(path))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cancellation_and_system_failure_are_distinct_without_opening_a_dialog() {
            assert!(show_result(Ok(())).unwrap());
            assert!(
                !show_result(Err(windows::core::Error::from_hresult(
                    HRESULT::from_win32(ERROR_CANCELLED.0)
                )))
                .unwrap()
            );
            let error = show_result(Err(windows::core::Error::from_hresult(HRESULT(
                0x80004005u32 as i32,
            ))))
            .unwrap_err();
            assert!(
                error
                    .text(Language::English)
                    .contains("Windows file dialog failed")
            );
            assert!(
                error
                    .text(Language::Chinese)
                    .contains("Windows 文件对话框失败")
            );
            assert!(wide(OsStr::new("bad\0path")).is_err());
            assert_eq!(*wide(OsStr::new("录制")).unwrap().last().unwrap(), 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for_result(picker: &mut FilePicker) -> PickerResult {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = picker.poll() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "file picker worker did not finish"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn picker_does_not_block_or_start_a_second_dialog_and_preserves_cancellation() {
        let (release, wait) = mpsc::channel();
        let mut picker = FilePicker::default();
        picker
            .request_with(
                PickerKind::OpenCapture,
                egui::Context::default(),
                move || {
                    wait.recv().unwrap();
                    Ok(None)
                },
            )
            .unwrap();
        assert!(picker.is_busy());
        assert!(picker.poll().is_none());
        assert!(
            picker
                .request_with(PickerKind::SaveCsv, egui::Context::default(), || panic!(
                    "second request must not run"
                ))
                .is_err()
        );
        release.send(()).unwrap();
        let result = wait_for_result(&mut picker);
        assert_eq!(result.kind, PickerKind::OpenCapture);
        assert!(result.outcome.unwrap().is_none());
        assert!(!picker.is_busy());
        assert!(picker.poll().is_none());
        picker
            .request_with(PickerKind::SaveCsv, egui::Context::default(), || {
                Ok(Some(PathBuf::from("录制.csv")))
            })
            .unwrap();
        assert_eq!(
            wait_for_result(&mut picker).outcome.unwrap().unwrap(),
            PathBuf::from("录制.csv")
        );
    }

    #[test]
    fn picker_errors_preserve_both_languages_and_output_formats() {
        let mut picker = FilePicker::default();
        picker
            .request_with(
                PickerKind::RecordingDirectory,
                egui::Context::default(),
                || Err(Message::new("denied", "拒绝")),
            )
            .unwrap();
        let error = wait_for_result(&mut picker).outcome.unwrap_err();
        assert_eq!(error.text(Language::English), "denied");
        assert_eq!(error.text(Language::Chinese), "拒绝");
        for (kind, extension) in [
            (PickerKind::SaveCapture, "pvdstream"),
            (PickerKind::SaveClip, "pvdstream"),
            (PickerKind::SaveRetainedCapture, "pvdcapture"),
            (PickerKind::SaveRepx, "repx"),
            (PickerKind::SaveCsv, "csv"),
            (PickerKind::SaveAnalysisCsv, "csv"),
        ] {
            assert_eq!(
                validate_selection(kind, PathBuf::from("录制"))
                    .unwrap()
                    .extension()
                    .unwrap(),
                extension
            );
            assert!(validate_selection(kind, PathBuf::from("recording.pxd2")).is_err());
        }
        assert!(validate_selection(PickerKind::SaveCsv, PathBuf::from("DATA.CSV")).is_ok());
        assert!(
            validate_selection(PickerKind::OpenCapture, PathBuf::from("original.pxd2")).is_ok()
        );
        assert!(validate_selection(PickerKind::OpenCapture, PathBuf::new()).is_err());
    }
}
