//! `peek --service`: peek as a Windows service, the way Sunshine does it.
//!
//! A service runs in session 0, which has no screen. So this process is
//! the core (see `provider`): it listens and holds every client's socket.
//! The screen it leaves to a provider, a second peek that it starts in the
//! session at the console, as SYSTEM, which may also capture and control
//! the login screen, the lock screen and UAC prompts. It starts another
//! when the console moves to another session (signing out, switching
//! users), and when the provider exits. The sockets stay open across both.

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, E_FAIL, ERROR_CALL_NOT_IMPLEMENTED, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, HANDLE,
    HANDLE_FLAG_INHERIT, HANDLE_FLAGS, NO_ERROR, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0,
};
use windows::Win32::Security::{
    DuplicateTokenEx, SECURITY_ATTRIBUTES, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS,
    TOKEN_DUPLICATE, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::Pipes::CreatePipe;
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
    OpenProcessToken, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOW, SetEvent,
    TerminateProcess,
    WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use crate::provider::Link;

/// The name the service is registered under (`peek install --service`).
const NAME: PCWSTR = w!("peek");

/// How often to look at the console session even without being told.
const POLL: Duration = Duration::from_secs(2);
/// Waits before starting peek again after it exited soon after starting.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
/// Running at least this long counts as having started fine.
const HEALTHY: Duration = Duration::from_secs(30);
/// The exit code of a process that Windows ended along with its session (a
/// sign-out, a shutdown).
const ENDED_WITH_SESSION: u32 = 0x4001_0004;
/// How long a session whose provider ended that way is left alone, should
/// the console stay on it after all.
const SESSION_ENDING: Duration = Duration::from_secs(10);
/// Size asked for each pipe to a provider: room for the largest frames, so
/// that writing one does not wait on the reader.
const PIPE_BYTES: u32 = 1 << 20;

/// Handles shared with the control handler, which Windows calls on another
/// thread: as integers, since raw handles are not `Sync`.
struct Shared {
    status: AtomicUsize,
    /// Set when the service is to stop (manual reset).
    stop: usize,
    /// Set when the console session may have changed (auto reset).
    wake: usize,
    /// Starts the core in this process, once the service is running, and
    /// returns the link its providers are to be attached to.
    core: fn() -> Arc<Link>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// Hand this process to the service control manager. Returns once the
/// service has stopped.
pub fn run(core: fn() -> Arc<Link>) -> Result<(), String> {
    let event = |manual| unsafe { CreateEventW(None, manual, false, PCWSTR::null()) }.map(|h| h.0 as usize);
    let shared = Shared {
        status: AtomicUsize::new(0),
        stop: event(true).map_err(|e| e.to_string())?,
        wake: event(false).map_err(|e| e.to_string())?,
        core,
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
    let screen = (shared.core)();
    supervise(shared, &screen);
    crate::log!("service: stopped");
    report(SERVICE_STOPPED);
}

/// A job that ends the provider put in it when this service ends, however it
/// ends. One per provider: a job only takes processes of one session.
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

/// A screen provider running in a user session.
struct Child {
    process: HANDLE,
    /// Closing it ends the provider, if it has not ended already.
    job: HANDLE,
    session: u32,
    started: Instant,
}

impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.process);
            let _ = CloseHandle(self.job);
        }
    }
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
            // It must be gone before the next one starts: two would both
            // act on what the clients do.
            WaitForSingleObject(self.process, 5000);
        }
    }
}

/// Whether to start a provider in the session at the console now: not in
/// one that Windows has just ended a provider with (it is being signed out
/// of, and would only end the next one too), until the console has moved
/// on or `SESSION_ENDING` has passed.
fn may_start(console: u32, ending: Option<(u32, Instant)>, now: Instant) -> bool {
    !ending.is_some_and(|(session, until)| session == console && now < until)
}

fn supervise(shared: &Shared, screen: &Arc<Link>) {
    let (stop, wake) = (HANDLE(shared.stop as *mut c_void), HANDLE(shared.wake as *mut c_void));
    let mut child: Option<Child> = None;
    // Backing off from a provider that keeps exiting, in the session it does so.
    let mut retry = FIRST_RETRY;
    let mut not_before = Instant::now();
    let mut failing_in = None;
    // A session being signed out of, and until when to leave it alone.
    let mut ending: Option<(u32, Instant)> = None;
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
                crate::log!("service: the screen provider exited in session {} (code {code:#x})", c.session);
                if code == ENDED_WITH_SESSION {
                    ending = Some((c.session, Instant::now() + SESSION_ENDING));
                    retry = FIRST_RETRY;
                } else if c.started.elapsed() < HEALTHY && Some(c.session) == console {
                    not_before = Instant::now() + retry;
                    retry = (retry * 2).min(LAST_RETRY);
                    failing_in = console;
                } else {
                    retry = FIRST_RETRY;
                }
            } else if console.is_some_and(|s| s != c.session) {
                let c = child.take().unwrap();
                crate::log!("service: the console left session {}; stopping the screen provider there", c.session);
                c.stop();
            }
        }
        if let (None, Some(session)) = (&child, console)
            && Instant::now() >= not_before
            && may_start(session, ending, Instant::now())
        {
            ending = None;
            match launch(session, screen) {
                Ok(c) => {
                    crate::log!("service: started the screen provider in session {session}");
                    child = Some(c);
                }
                Err(e) => {
                    crate::log!("service: cannot start the screen provider in session {session}: {e}");
                    not_before = Instant::now() + retry;
                    retry = (retry * 2).min(LAST_RETRY);
                    failing_in = console;
                }
            }
        }

        // Sleep until told to stop, the session changes, the provider exits,
        // or it is time to look again anyway.
        let mut handles = vec![stop, wake];
        handles.extend(child.as_ref().map(|c| c.process));
        let mut wait = POLL;
        if child.is_none() && console.is_some() {
            let now = Instant::now();
            let ready = match ending {
                Some((session, until)) if Some(session) == console => not_before.max(until),
                _ => not_before,
            };
            wait = wait.min(ready.saturating_duration_since(now));
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

/// The two pipes to a provider: its ends, which become its stdin and
/// stdout, and ours.
struct Pipes {
    child_in: HANDLE,
    child_out: HANDLE,
    to: HANDLE,
    from: HANDLE,
}

impl Pipes {
    fn new() -> windows::core::Result<Pipes> {
        let inherit = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let mut pipes =
            Pipes { child_in: HANDLE::default(), child_out: HANDLE::default(), to: HANDLE::default(), from: HANDLE::default() };
        unsafe {
            CreatePipe(&mut pipes.child_in, &mut pipes.to, Some(&inherit), PIPE_BYTES)?;
            CreatePipe(&mut pipes.from, &mut pipes.child_out, Some(&inherit), PIPE_BYTES)?;
            // Only the provider's ends go to the provider.
            SetHandleInformation(pipes.to, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
            SetHandleInformation(pipes.from, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
        }
        Ok(pipes)
    }

    /// Our ends, once the provider has its own: (to it, from it). Our
    /// copies of its ends are closed, or its exit would never be seen.
    fn ours(mut self) -> (File, File) {
        let (to, from) = (std::mem::take(&mut self.to), std::mem::take(&mut self.from));
        unsafe { (File::from_raw_handle(to.0), File::from_raw_handle(from.0)) }
    }
}

impl Drop for Pipes {
    fn drop(&mut self) {
        for handle in [self.child_in, self.child_out, self.to, self.from] {
            if !handle.is_invalid() {
                let _ = unsafe { CloseHandle(handle) };
            }
        }
    }
}

/// Start a screen provider in `session` as SYSTEM, on the session's own
/// desktop, with the flags this service was given (less `--service`), and
/// attach it to `screen`. It is born holding its ends of the pipes, so there
/// is nothing for it to find and nobody else at the other end.
fn launch(session: u32, screen: &Arc<Link>) -> windows::core::Result<Child> {
    let exe = std::env::current_exe().map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;
    let mut args: Vec<OsString> = std::env::args_os().skip(1).filter(|a| a != "--service").collect();
    args.extend(["--provider".into(), "screen".into()]);
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
            let job = job()?;
            let pipes = Pipes::new().inspect_err(|_| drop(CloseHandle(job)))?;
            let startup = STARTUPINFOW {
                cb: size_of::<STARTUPINFOW>() as u32,
                lpDesktop: PWSTR(desktop.as_mut_ptr()),
                dwFlags: STARTF_USESTDHANDLES,
                hStdInput: pipes.child_in,
                hStdOutput: pipes.child_out,
                ..Default::default()
            };
            let mut info = PROCESS_INFORMATION::default();
            let created = CreateProcessAsUserW(
                Some(token),
                &HSTRING::from(exe.as_path()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                // For the pipes: nothing else here is inheritable.
                true,
                CREATE_NO_WINDOW | CREATE_SUSPENDED,
                None,
                PCWSTR::null(),
                &startup,
                &mut info,
            );
            if let Err(e) = created {
                let _ = CloseHandle(job);
                return Err(e);
            }
            if let Err(e) = AssignProcessToJobObject(job, info.hProcess) {
                crate::log!("service: the screen provider will outlive this service if it crashes ({e})");
            }
            let (to, from) = pipes.ours();
            screen.attach(to, from);
            ResumeThread(info.hThread);
            let _ = CloseHandle(info.hThread);
            Ok(Child { process: info.hProcess, job, session, started: Instant::now() })
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
    fn a_session_being_signed_out_of_is_left_alone() {
        let now = Instant::now();
        let later = now + SESSION_ENDING;
        assert!(may_start(2, None, now));
        assert!(!may_start(2, Some((2, later)), now), "its provider was just ended with it");
        assert!(may_start(3, Some((2, later)), now), "the console has moved on");
        assert!(may_start(2, Some((2, later)), later), "the console stayed after all");
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
