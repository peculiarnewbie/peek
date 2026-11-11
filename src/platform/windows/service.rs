//! `peek --service`: peek as a Windows service, the way Sunshine does it.
//!
//! A service runs in session 0, which has no screen. So this process only
//! supervises: it starts peek proper in the session at the console, as
//! SYSTEM, which may also capture and control the login screen, the lock
//! screen and UAC prompts. It starts peek again when the console moves to
//! another session (signing out, switching users), and when peek exits.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, E_FAIL, ERROR_CALL_NOT_IMPLEMENTED, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, HANDLE, NO_ERROR,
    WAIT_FAILED, WAIT_OBJECT_0,
};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TOKEN_DUPLICATE, TokenPrimary,
    TokenSessionId,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP,
    SERVICE_RUNNING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateEventW, CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess,
    OpenProcessToken, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, SetEvent, TerminateProcess,
    WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

/// The name the service is registered under (`peek install --service`).
const NAME: PCWSTR = w!("peek");

/// How often to look at the console session even without being told.
const POLL: Duration = Duration::from_secs(2);
/// Waits before starting peek again after it exited soon after starting.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
/// Running at least this long counts as having started fine.
const HEALTHY: Duration = Duration::from_secs(30);

/// Handles shared with the control handler, which Windows calls on another
/// thread: as integers, since raw handles are not `Sync`.
struct Shared {
    status: AtomicUsize,
    /// Set when the service is to stop (manual reset).
    stop: usize,
    /// Set when the console session may have changed (auto reset).
    wake: usize,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// Hand this process to the service control manager. Returns once the
/// service has stopped.
pub fn run() -> Result<(), String> {
    let event = |manual| unsafe { CreateEventW(None, manual, false, PCWSTR::null()) }.map(|h| h.0 as usize);
    let shared = Shared {
        status: AtomicUsize::new(0),
        stop: event(true).map_err(|e| e.to_string())?,
        wake: event(false).map_err(|e| e.to_string())?,
    };
    let _ = SHARED.set(shared);
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR(NAME.0 as *mut u16), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }.map_err(|e| {
        if e.code() == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT.to_hresult() {
            "--service is how Windows starts peek as a service. To set that up: peek install --service".to_string()
        } else {
            format!("cannot start the service: {e}")
        }
    })
}

fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let Some(shared) = SHARED.get() else { return };
    let handle = SERVICE_STATUS_HANDLE(shared.status.load(Ordering::Acquire) as *mut c_void);
    if handle.is_invalid() {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
        } else {
            0
        },
        dwWaitHint: if state == SERVICE_STOP_PENDING { 10_000 } else { 0 },
        ..Default::default()
    };
    let _ = unsafe { SetServiceStatus(handle, &status) };
}

unsafe extern "system" fn control(control: u32, _event: u32, _data: *mut c_void, _context: *mut c_void) -> u32 {
    let Some(shared) = SHARED.get() else { return NO_ERROR.0 };
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING);
            let _ = unsafe { SetEvent(HANDLE(shared.stop as *mut c_void)) };
            NO_ERROR.0
        }
        SERVICE_CONTROL_SESSIONCHANGE => {
            let _ = unsafe { SetEvent(HANDLE(shared.wake as *mut c_void)) };
            NO_ERROR.0
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR.0,
        _ => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let Some(shared) = SHARED.get() else { return };
    let Ok(handle) = (unsafe { RegisterServiceCtrlHandlerExW(NAME, Some(control), None) }) else { return };
    shared.status.store(handle.0 as usize, Ordering::Release);
    report(SERVICE_RUNNING);
    crate::log!("service: started");
    match job() {
        Ok(job) => {
            supervise(shared, job);
            // Closing the job ends anything still in it.
            let _ = unsafe { CloseHandle(job) };
        }
        Err(e) => crate::log!("service: cannot create a job object for peek: {e}"),
    }
    crate::log!("service: stopped");
    report(SERVICE_STOPPED);
}

/// A job that ends peek when this service ends, however it ends.
fn job() -> windows::core::Result<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null())?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )?;
        Ok(job)
    }
}

/// peek running in a user session.
struct Child {
    process: HANDLE,
    session: u32,
    started: Instant,
}

impl Child {
    /// Its exit code, once it has exited.
    fn exited(&self) -> Option<u32> {
        unsafe {
            if WaitForSingleObject(self.process, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0;
            let _ = GetExitCodeProcess(self.process, &mut code);
            Some(code)
        }
    }

    fn stop(self) {
        unsafe {
            let _ = TerminateProcess(self.process, 0);
            // It must be gone before the next one can listen on its port.
            WaitForSingleObject(self.process, 5000);
            let _ = CloseHandle(self.process);
        }
    }
}

fn supervise(shared: &Shared, job: HANDLE) {
    let (stop, wake) = (HANDLE(shared.stop as *mut c_void), HANDLE(shared.wake as *mut c_void));
    let mut child: Option<Child> = None;
    // Backing off from a peek that keeps exiting, in the session it does so.
    let mut retry = FIRST_RETRY;
    let mut not_before = Instant::now();
    let mut failing_in = None;
    loop {
        // 0 is the services' own session and u32::MAX means none is at the
        // console (between sessions): wait for a real one.
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        let console = (console != 0 && console != u32::MAX).then_some(console);
        if failing_in.is_some() && failing_in != console {
            (retry, not_before, failing_in) = (FIRST_RETRY, Instant::now(), None);
        }
        if let Some(c) = &child {
            if let Some(code) = c.exited() {
                let c = child.take().unwrap();
                crate::log!("service: peek exited in session {} (code {code:#x})", c.session);
                if c.started.elapsed() < HEALTHY && Some(c.session) == console {
                    not_before = Instant::now() + retry;
                    retry = (retry * 2).min(LAST_RETRY);
                    failing_in = console;
                } else {
                    retry = FIRST_RETRY;
                }
                let _ = unsafe { CloseHandle(c.process) };
            } else if console.is_some_and(|s| s != c.session) {
                let c = child.take().unwrap();
                crate::log!("service: the console left session {}; stopping peek there", c.session);
                c.stop();
            }
        }
        if let (None, Some(session)) = (&child, console)
            && Instant::now() >= not_before
        {
            match launch(session, job) {
                Ok(c) => {
                    crate::log!("service: started peek in session {session}");
                    child = Some(c);
                }
                Err(e) => {
                    crate::log!("service: cannot start peek in session {session}: {e}");
                    not_before = Instant::now() + retry;
                    retry = (retry * 2).min(LAST_RETRY);
                    failing_in = console;
                }
            }
        }

        // Sleep until told to stop, the session changes, peek exits, or it is
        // time to look again anyway.
        let mut handles = vec![stop, wake];
        handles.extend(child.as_ref().map(|c| c.process));
        let mut wait = POLL;
        if child.is_none() && console.is_some() {
            wait = wait.min(not_before.saturating_duration_since(Instant::now()));
        }
        match unsafe { WaitForMultipleObjects(&handles, false, wait.as_millis() as u32) } {
            WAIT_OBJECT_0 => break,
            WAIT_FAILED => std::thread::sleep(POLL),
            _ => {}
        }
    }
    if let Some(c) = child {
        c.stop();
    }
}

/// Start peek in `session` as SYSTEM, on the session's own desktop, with the
/// flags this service was given (less `--service`).
fn launch(session: u32, job: HANDLE) -> windows::core::Result<Child> {
    let exe = std::env::current_exe().map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;
    let args: Vec<OsString> = std::env::args_os().skip(1).filter(|a| a != "--service").collect();
    let mut command = command_line(exe.as_os_str(), &args);
    command.push(0);
    let mut desktop: Vec<u16> = "winsta0\\default\0".encode_utf16().collect();
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE, &mut own)?;
        let mut token = HANDLE::default();
        let duplicated = DuplicateTokenEx(own, TOKEN_ALL_ACCESS, None, SecurityImpersonation, TokenPrimary, &mut token);
        let _ = CloseHandle(own);
        duplicated?;
        let result = (|| {
            SetTokenInformation(token, TokenSessionId, (&raw const session).cast(), size_of::<u32>() as u32)?;
            let startup = STARTUPINFOW {
                cb: size_of::<STARTUPINFOW>() as u32,
                lpDesktop: PWSTR(desktop.as_mut_ptr()),
                ..Default::default()
            };
            let mut info = PROCESS_INFORMATION::default();
            CreateProcessAsUserW(
                Some(token),
                &HSTRING::from(exe.as_path()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW | CREATE_SUSPENDED,
                None,
                PCWSTR::null(),
                &startup,
                &mut info,
            )?;
            if let Err(e) = AssignProcessToJobObject(job, info.hProcess) {
                crate::log!("service: peek will outlive this service if it crashes ({e})");
            }
            ResumeThread(info.hThread);
            let _ = CloseHandle(info.hThread);
            Ok(Child { process: info.hProcess, session, started: Instant::now() })
        })();
        let _ = CloseHandle(token);
        result
    }
}

/// A Windows command line, quoted so that the C runtime's rules (which Rust
/// follows too) split it back into exactly `program` and `args`.
fn command_line(program: &OsStr, args: &[OsString]) -> Vec<u16> {
    // The program name is split on quotes alone, and a path has none.
    let mut line: Vec<u16> = std::iter::once('"' as u16).chain(program.encode_wide()).chain(['"' as u16]).collect();
    for arg in args {
        line.push(' ' as u16);
        let arg: Vec<u16> = arg.encode_wide().collect();
        // Space, tab, line feed, vertical tab, quote.
        let special = |c: &u16| matches!(c, 0x20 | 0x09 | 0x0a | 0x0b | 0x22);
        if !arg.is_empty() && !arg.iter().any(special) {
            line.extend(arg);
            continue;
        }
        line.push('"' as u16);
        let mut backslashes = 0;
        for c in arg {
            if c == '\\' as u16 {
                backslashes += 1;
                continue;
            }
            // Backslashes are literal unless a quote follows them.
            let doubled = if c == '"' as u16 { backslashes * 2 + 1 } else { backslashes };
            line.extend(std::iter::repeat_n('\\' as u16, doubled));
            line.push(c);
            backslashes = 0;
        }
        line.extend(std::iter::repeat_n('\\' as u16, backslashes * 2));
        line.push('"' as u16);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split a command line the way Windows does for the started program.
    fn split(line: &[u16]) -> Vec<String> {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        let mut line = line.to_vec();
        line.push(0);
        let mut count = 0;
        unsafe {
            let argv = CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut count);
            assert!(!argv.is_null());
            let args = (0..count as usize).map(|i| (*argv.add(i)).to_string().unwrap()).collect();
            LocalFree(Some(HLOCAL(argv.cast())));
            args
        }
    }

    #[test]
    fn command_line_round_trips() {
        let args = [
            "--port",
            "9000",
            "--log",
            r"C:\ProgramData\peek\peek.log",
            "",
            "two words",
            r"ends with a backslash\",
            r#"say "hi""#,
            r#"back\"slash"#,
            r"\\server\share\",
        ];
        let os: Vec<OsString> = args.iter().map(OsString::from).collect();
        let program = r"C:\Program Files\peek\peek.exe";
        let line = command_line(OsStr::new(program), &os);
        let mut expected = vec![program.to_string()];
        expected.extend(args.iter().map(|s| s.to_string()));
        assert_eq!(split(&line), expected);
    }
}
