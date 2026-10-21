use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
};

static CANCELLED: AtomicBool = AtomicBool::new(false);

pub struct Cancellation;

impl Cancellation {
    pub fn install() -> io::Result<Self> {
        CANCELLED.store(false, Ordering::Relaxed);
        install_handler()?;
        Ok(Self)
    }

    pub fn flag(&self) -> &AtomicBool {
        &CANCELLED
    }

    pub fn requested(&self) -> bool {
        CANCELLED.load(Ordering::Relaxed)
    }

    pub fn check(&self) -> io::Result<()> {
        if self.requested() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "recording export cancelled",
            ))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub fn request_for_test(&self) {
        CANCELLED.store(true, Ordering::Relaxed);
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn console_handler(event: u32) -> windows::core::BOOL {
    use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
        CANCELLED.store(true, Ordering::Relaxed);
        true.into()
    } else {
        false.into()
    }
}

#[cfg(target_os = "windows")]
fn install_handler() -> io::Result<()> {
    // The callback only touches a static atomic; Windows invokes it on a separate thread.
    unsafe { windows::Win32::System::Console::SetConsoleCtrlHandler(Some(console_handler), true) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))
}

#[cfg(target_os = "windows")]
impl Drop for Cancellation {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Console::SetConsoleCtrlHandler(
                Some(console_handler),
                false,
            );
        }
    }
}

#[cfg(unix)]
fn install_handler() -> io::Result<()> {
    ctrlc::set_handler(|| CANCELLED.store(true, Ordering::Relaxed)).map_err(io::Error::other)
}

#[cfg(not(any(target_os = "windows", unix)))]
fn install_handler() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cooperative export cancellation is unsupported on this platform",
    ))
}
