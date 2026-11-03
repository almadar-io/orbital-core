//! The Win32 calls the file host needs that `std` does not expose: a path's change time and a pipe's unread bytes.

use std::fs;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::path::Path;
use windows_sys::Win32::Storage::FileSystem::{
    FileBasicInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;

/// The NTFS change time of `path` (the link itself unless `follow`), in `FILETIME` ticks.
pub(super) fn change_time(path: &Path, follow: bool) -> io::Result<i64> {
    let flags = FILE_FLAG_BACKUP_SEMANTICS | if follow { 0 } else { FILE_FLAG_OPEN_REPARSE_POINT };
    let file = fs::OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES).custom_flags(flags).open(path)?;
    // SAFETY: FILE_BASIC_INFO is plain data; zero is a valid value for every field.
    let mut info: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the call, and the buffer is a FILE_BASIC_INFO of the size passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&mut info as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.ChangeTime)
}

/// How many bytes the pipe holds unread; a closed write end is `BrokenPipe`.
pub(super) fn pipe_available(handle: RawHandle) -> io::Result<u32> {
    let mut available: u32 = 0;
    // SAFETY: the handle is a pipe the caller holds locked; only the count is written.
    let ok = unsafe {
        PeekNamedPipe(handle, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut available, std::ptr::null_mut())
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}
