//! Following the desktop that takes input.
//!
//! Windows shows the lock screen, the login screen and UAC prompts on a
//! desktop of their own (Winlogon), not on the user's (Default). Capture and
//! injected input only reach the desktop a thread is attached to, so each
//! thread that captures or injects moves itself to whichever one takes input
//! now. Only peek running as SYSTEM (the service) may open Winlogon; any
//! other peek stays where it is.

use std::cell::Cell;

use windows::Win32::Foundation::{GENERIC_ALL, HANDLE};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetThreadDesktop, GetUserObjectInformationW, HDESK,
    OpenInputDesktop, SetThreadDesktop, UOI_NAME,
};
use windows::Win32::System::Threading::GetCurrentThreadId;

thread_local! {
    /// The desktop handle this thread opened and moved to, closed once it
    /// moves on. (The one it started on belongs to the process.)
    static OPENED: Cell<Option<HDESK>> = const { Cell::new(None) };
}

fn name(desktop: HDESK) -> Option<String> {
    let mut buf = [0u16; 128];
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            size_of_val(&buf) as u32,
            None,
        )
        .ok()?;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Move this thread to the desktop taking input, if it is on another one.
/// Returns that desktop's name when it moved.
pub fn follow() -> Option<String> {
    unsafe {
        let input = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(GENERIC_ALL.0)).ok()?;
        let wanted = name(input);
        let current = GetThreadDesktop(GetCurrentThreadId()).ok().and_then(name);
        // SetThreadDesktop refuses a thread that owns windows or hooks;
        // peek's capture and input threads have none.
        if wanted.is_none() || wanted == current || SetThreadDesktop(input).is_err() {
            let _ = CloseDesktop(input);
            return None;
        }
        if let Some(previous) = OPENED.replace(Some(input)) {
            let _ = CloseDesktop(previous);
        }
        wanted
    }
}
