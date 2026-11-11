//! Windows: the process table, whether a process is the asker's own, and ending a process.

use super::*;
use std::ffi::c_void;
use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, FILETIME, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{EqualSid, GetTokenInformation, TokenUser, PSID, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, TerminateProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

/// A handle closed when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn open(pid: u32, access: u32) -> Option<Owned> {
    let handle = unsafe { OpenProcess(access, 0, pid) };
    (!handle.is_null()).then(|| Owned(handle))
}

/// The `TOKEN_USER` of `process`, in a buffer aligned for it.
fn token_user(process: HANDLE) -> Option<Vec<u64>> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = Owned(token);
    let mut needed = 0u32;
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    let read = unsafe { GetTokenInformation(token.0, TokenUser, buffer.as_mut_ptr().cast(), needed, &mut needed) };
    (read != 0).then_some(buffer)
}

fn sid_of(user: &[u64]) -> PSID {
    unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
}

fn sid_text(sid: PSID) -> Option<String> {
    let mut wide: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 || wide.is_null() {
        return None;
    }
    let len = (0..).take_while(|i| unsafe { *wide.add(*i) } != 0).count();
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, len) });
    unsafe { LocalFree(wide.cast()) };
    Some(text)
}

fn ticks(time: &FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

fn image_path(process: HANDLE) -> String {
    let mut wide = vec![0u16; 32768];
    let mut len = wide.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, wide.as_mut_ptr(), &mut len) } == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&wide[..len as usize])
}

/// The arguments of `process`, split as the C runtime splits a command line; empty when the host refuses them.
fn arguments(process: HANDLE) -> Vec<String> {
    let mut needed = 0u32;
    unsafe { NtQueryInformationProcess(process, ProcessCommandLineInformation, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    let status = unsafe {
        NtQueryInformationProcess(process, ProcessCommandLineInformation, buffer.as_mut_ptr().cast::<c_void>(), needed, &mut needed)
    };
    if status < 0 {
        return Vec::new();
    }
    let line = unsafe { &*buffer.as_ptr().cast::<UNICODE_STRING>() };
    if line.Buffer.is_null() || line.Length == 0 {
        return Vec::new();
    }
    let mut text: Vec<u16> = unsafe { std::slice::from_raw_parts(line.Buffer, usize::from(line.Length) / 2) }.to_vec();
    text.push(0);
    let mut count = 0i32;
    let parts = unsafe { CommandLineToArgvW(text.as_ptr(), &mut count) };
    if parts.is_null() {
        return Vec::new();
    }
    let args = (0..usize::try_from(count).unwrap_or(0))
        .map(|i| {
            let arg = unsafe { *parts.add(i) };
            let len = (0..).take_while(|j| unsafe { *arg.add(*j) } != 0).count();
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(arg, len) })
        })
        .collect();
    unsafe { LocalFree(parts.cast()) };
    args
}

pub(in super::super) fn processes(op: OsOp) -> Result<Vec<Process>, OsError> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io_error(op, &std::io::Error::last_os_error()));
    }
    let snapshot = Owned(snapshot);
    let mine = token_user(unsafe { GetCurrentProcess() });
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut out = Vec::new();
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while more {
        let name_len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
        let mut row = Process {
            pid: entry.th32ProcessID,
            ppid: entry.th32ParentProcessID,
            name: String::from_utf16_lossy(&entry.szExeFile[..name_len]),
            command: String::new(),
            args: Vec::new(),
            user: String::new(),
            own: false,
            rss_bytes: None,
            cpu_seconds: None,
            started_at: None,
        };
        // A process the host will not open for the asker (a system process, another user's) keeps only its listing.
        if let Some(process) = open(entry.th32ProcessID, PROCESS_QUERY_LIMITED_INFORMATION) {
            row.command = image_path(process.0);
            row.args = arguments(process.0);
            if let Some(owner) = token_user(process.0) {
                row.user = sid_text(sid_of(&owner)).unwrap_or_default();
                row.own = mine.as_ref().is_some_and(|me| unsafe { EqualSid(sid_of(&owner), sid_of(me)) } != 0);
            }
            let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) } != 0 {
                row.started_at = Some(filetime_unix_seconds(ticks(&created)));
                row.cpu_seconds = Some(filetime_seconds(ticks(&kernel) + ticks(&user)));
            }
            let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if unsafe { K32GetProcessMemoryInfo(process.0, &mut memory, size) } != 0 {
                row.rss_bytes = Some(memory.WorkingSetSize as f64);
            }
        }
        out.push(row);
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    Ok(out)
}

/// Whether the running process `pid` belongs to the asker; `None` when there is no such process the host will open.
pub(in super::super) fn process_is_own(pid: u32) -> Option<bool> {
    let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let owner = token_user(process.0)?;
    let mine = token_user(unsafe { GetCurrentProcess() })?;
    Some(unsafe { EqualSid(sid_of(&owner), sid_of(&mine)) } != 0)
}

/// End the process `pid`: Windows has no signals, so this is the one way a process is stopped from outside.
pub(in super::super) fn terminate(op: OsOp, pid: u32) -> Result<(), OsError> {
    let process = open(pid, PROCESS_TERMINATE).ok_or_else(|| io_error(op, &std::io::Error::last_os_error()))?;
    if unsafe { TerminateProcess(process.0, 1) } == 0 {
        return Err(io_error(op, &std::io::Error::last_os_error()));
    }
    Ok(())
}
