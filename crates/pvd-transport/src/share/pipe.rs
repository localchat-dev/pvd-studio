use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::{
    Foundation::{
        ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
        LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
            PIPE_REJECT_REMOTE_CLIENTS,
        },
        Threading::{
            CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken,
            PROCESS_QUERY_LIMITED_INFORMATION, ResetEvent, WaitForSingleObject,
        },
    },
};

pub(super) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

pub(super) fn disconnected(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        matches!(
            code as u32,
            ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
        )
    })
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

pub(super) fn user_sid() -> io::Result<String> {
    process_user_sid(unsafe { GetCurrentProcess() })
}

fn process_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut size = 0;
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size) };
    if size < size_of::<TOKEN_USER>() as u32 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut sid = null_mut();
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalAllocation(sid.cast());
    let size = (0..)
        .take_while(|&index| unsafe { *sid.add(index) != 0 })
        .count();
    String::from_utf16(unsafe { std::slice::from_raw_parts(sid, size) }).map_err(io::Error::other)
}

pub(super) struct Pipe {
    file: OwnedHandle,
    event: OwnedHandle,
}

impl Pipe {
    fn from_handle(file: HANDLE) -> io::Result<Self> {
        if file == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { OwnedHandle::from_raw_handle(file) };
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file,
            event: unsafe { OwnedHandle::from_raw_handle(event) },
        })
    }

    pub(super) fn listen(name: &[u16], sid: &str) -> io::Result<Self> {
        let access = wide(&format!("D:P(A;;GA;;;{sid})"));
        let mut security = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                access.as_ptr(),
                SDDL_REVISION_1,
                &mut security,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let _allocation = LocalAllocation(security);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security,
            bInheritHandle: 0,
        };
        Self::from_handle(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_REJECT_REMOTE_CLIENTS,
                1,
                (2 * crate::CHUNK_SIZE) as u32,
                4096,
                0,
                &attributes,
            )
        })
    }

    pub(super) fn open(name: &[u16], sid: &str) -> io::Result<Self> {
        let pipe = Self::from_handle(unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        })?;
        let mut server = 0;
        if unsafe { GetNamedPipeServerProcessId(pipe.file.as_raw_handle(), &mut server) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let server = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server) };
        if server.is_null() {
            return Err(io::Error::last_os_error());
        }
        let server = unsafe { OwnedHandle::from_raw_handle(server) };
        if process_user_sid(server.as_raw_handle())? != sid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "shared PVD receiver belongs to a different Windows user",
            ));
        }
        Ok(pipe)
    }

    pub(super) fn accept(&mut self, stopped: &dyn Fn() -> bool) -> io::Result<()> {
        let file = self.file.as_raw_handle();
        match self.operation(stopped, |overlap, _| unsafe {
            ConnectNamedPipe(file, overlap)
        }) {
            Ok(_) => Ok(()),
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn disconnect(&mut self) {
        unsafe { DisconnectNamedPipe(self.file.as_raw_handle()) };
    }

    pub(super) fn read(
        &mut self,
        buffer: &mut [u8],
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<usize> {
        let file = self.file.as_raw_handle();
        self.operation(stopped, |overlap, count| unsafe {
            ReadFile(
                file,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                count,
                overlap,
            )
        })
    }

    pub(super) fn read_exact(
        &mut self,
        mut buffer: &mut [u8],
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        while !buffer.is_empty() {
            let count = self.read(buffer, stopped)?;
            if count == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            buffer = &mut buffer[count..];
        }
        Ok(())
    }

    pub(super) fn write_all(
        &mut self,
        mut buffer: &[u8],
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        let file = self.file.as_raw_handle();
        while !buffer.is_empty() {
            let count = self.operation(stopped, |overlap, count| unsafe {
                WriteFile(file, buffer.as_ptr(), buffer.len() as u32, count, overlap)
            })?;
            if count == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            buffer = &buffer[count..];
        }
        Ok(())
    }

    fn operation(
        &mut self,
        stopped: &dyn Fn() -> bool,
        start: impl FnOnce(*mut OVERLAPPED, *mut u32) -> i32,
    ) -> io::Result<usize> {
        if stopped() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let event = self.event.as_raw_handle();
        if unsafe { ResetEvent(event) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut overlap = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        let mut count = 0;
        if start(&mut overlap, &mut count) != 0 {
            return Ok(count as usize);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(error);
        }
        let file = self.file.as_raw_handle();
        loop {
            let error = match unsafe { WaitForSingleObject(event, crate::POLL.as_millis() as u32) }
            {
                WAIT_OBJECT_0 => {
                    if unsafe { GetOverlappedResult(file, &overlap, &mut count, 0) } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    return Ok(count as usize);
                }
                WAIT_TIMEOUT if !stopped() => continue,
                WAIT_TIMEOUT => io::Error::from(io::ErrorKind::Interrupted),
                _ => io::Error::last_os_error(),
            };
            // The operation can still complete after cancellation. Wait before releasing its buffers.
            unsafe {
                CancelIoEx(file, &overlap);
                GetOverlappedResult(file, &overlap, &mut count, 1);
            }
            return Err(error);
        }
    }
}
