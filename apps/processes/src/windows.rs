//! Windows: every process in one call (NtQuerySystemInformation), as Task
//! Manager reads them, so nothing has to be opened to list them.
//!
//! Written from the documented layouts; checked to compile here, not yet run.

use std::cell::RefCell;

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS};
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, E_INVALIDARG, FILETIME, HANDLE};
use windows::Win32::Security::{GetTokenInformation, IsWellKnownSid, TOKEN_QUERY, TOKEN_USER, TokenUser, WinLocalServiceSid, WinLocalSystemSid, WinNetworkServiceSid};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetProcessTimes, GetSystemTimes, IsProcessCritical, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
};
use windows::core::BOOL;

use crate::{Meta, Policy, Raw, Snapshot};

/// TerminateProcess is the only way: there is no asking first.
pub const FORCE: bool = false;
const PROCESS_LIST: SYSTEM_INFORMATION_CLASS = SYSTEM_INFORMATION_CLASS(5);
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const SYSTEM_MSG: &str = "it is the system's or a service's: allow that in the settings of processes";

/// 100 ns units a second.
pub fn cpu_units() -> f64 {
    1e7
}

pub fn cores() -> u32 {
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }.max(1)
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum: u16,
    buffer: *const u16,
}

/// The start of SYSTEM_PROCESS_INFORMATION, with what winternl.h calls
/// reserved named. Entries follow one another in the buffer.
#[repr(C)]
struct ProcessInfo {
    next: u32,
    threads: u32,
    private_working_set: i64,
    hard_faults: u32,
    threads_high: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    pid: usize,
    parent: usize,
    handles: u32,
    session: u32,
}

// The documented offsets, on 64-bit Windows.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::offset_of!(ProcessInfo, create_time) == 0x20);
    assert!(std::mem::offset_of!(ProcessInfo, image_name) == 0x38);
    assert!(std::mem::offset_of!(ProcessInfo, pid) == 0x50);
    assert!(std::mem::offset_of!(ProcessInfo, parent) == 0x58);
    assert!(std::mem::offset_of!(ProcessInfo, session) == 0x64);
};

thread_local! {
    /// Kept between readings: a few hundred processes take some 300 KB.
    static BUFFER: RefCell<Vec<u64>> = RefCell::new(vec![0; 1 << 15]);
}

fn processes() -> Vec<Raw> {
    BUFFER.with_borrow_mut(|buf| {
        for _ in 0..4 {
            let mut need = 0u32;
            let status = unsafe { NtQuerySystemInformation(PROCESS_LIST, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut need) };
            if status.0 == STATUS_INFO_LENGTH_MISMATCH {
                // More processes than room: with some to spare, as more may start.
                buf.resize(need as usize / 8 + 8192, 0);
                continue;
            }
            if status.0 < 0 { return Vec::new(); }
            return walk(buf);
        }
        Vec::new()
    })
}

fn walk(buf: &[u64]) -> Vec<Raw> {
    let (base, len) = (buf.as_ptr().cast::<u8>(), buf.len() * 8);
    let mut out = Vec::with_capacity(400);
    let mut at = 0usize;
    while at + size_of::<ProcessInfo>() <= len {
        let p = unsafe { std::ptr::read_unaligned(base.add(at).cast::<ProcessInfo>()) };
        let pid = p.pid as u32;
        // 0 is the processor's idle time, not a process.
        if pid != 0 {
            let name = if p.image_name.buffer.is_null() || p.image_name.length == 0 {
                if pid == 4 { "System".to_string() } else { format!("pid {pid}") }
            } else {
                // The name is in the same buffer, after the entries.
                String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p.image_name.buffer, p.image_name.length as usize / 2) })
            };
            out.push(Raw {
                pid,
                ppid: p.parent as u32,
                start: p.create_time as u64,
                cpu: (p.user_time + p.kernel_time).max(0) as u64,
                mem: p.private_working_set.max(0) as u64,
                name,
                session: p.session,
            });
        }
        if p.next == 0 { break; }
        at += p.next as usize;
    }
    out
}

pub fn snapshot() -> Snapshot {
    Snapshot { procs: processes(), times: cpu_times(), mem: memory() }
}

pub fn meta(raw: &Raw) -> Meta {
    // Who each runs as would need each opened; session 0 is the system's.
    Meta { name: raw.name.clone(), user: String::new(), cmd: String::new(), sys: raw.session == 0 }
}

/// Peek, which started this.
pub fn parent() -> u32 {
    let me = std::process::id();
    processes().iter().find(|p| p.pid == me).map_or(0, |p| p.ppid)
}

fn filetime(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

fn cpu_times() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    // Kernel time includes idle time.
    let total = filetime(kernel) + filetime(user);
    Some((total.saturating_sub(filetime(idle)), total))
}

fn memory() -> (u64, u64) {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    match unsafe { GlobalMemoryStatusEx(&mut m) } {
        Ok(()) => (m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys),
        Err(_) => (0, 0),
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn refused(e: &windows::core::Error) -> String {
    match e.code() {
        E_ACCESSDENIED => "not allowed to end it".into(),
        // OpenProcess's answer for a number no process has.
        E_INVALIDARG => "it has already ended".into(),
        _ => e.message(),
    }
}

/// Runs as LocalSystem, LocalService or NetworkService, or cannot be told.
fn service_account(process: HANDLE) -> bool {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.is_err() {
        return true;
    }
    let token = Handle(token);
    let mut buf = [0u64; 64];
    let mut len = 0u32;
    if unsafe { GetTokenInformation(token.0, TokenUser, Some(buf.as_mut_ptr().cast()), (buf.len() * 8) as u32, &mut len) }.is_err() {
        return true;
    }
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    [WinLocalSystemSid, WinLocalServiceSid, WinNetworkServiceSid].into_iter().any(|kind| unsafe { IsWellKnownSid(user.User.Sid, kind) }.as_bool())
}

pub fn end(pid: u32, start: u64, _force: bool, policy: &Policy) -> Result<(), String> {
    if pid == 0 || pid == 4 {
        return Err("Windows cannot run without it".into());
    }
    // While this handle is open the number cannot go to another process, so
    // the start time below is checked of the one that is ended.
    let process = Handle(unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(|e| refused(&e))?);
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }.map_err(|e| refused(&e))?;
    if filetime(created) != start {
        return Err("it has already ended".into());
    }
    // Ending one of these stops the machine with a blue screen.
    let mut critical = BOOL(0);
    if unsafe { IsProcessCritical(process.0, &mut critical) }.is_err() || critical.as_bool() {
        return Err("Windows cannot run without it".into());
    }
    if !policy.any {
        let mut session = 0u32;
        if unsafe { ProcessIdToSessionId(pid, &mut session) }.is_err() || session == 0 || service_account(process.0) {
            return Err(SYSTEM_MSG.into());
        }
    }
    unsafe { TerminateProcess(process.0, 1) }.map_err(|e| refused(&e))
}
