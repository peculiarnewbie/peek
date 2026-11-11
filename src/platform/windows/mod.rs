mod capture;
pub mod input;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSORINFO, GetCursorInfo, GetForegroundWindow, GetWindowThreadProcessId, IDC_APPSTARTING,
    IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_HELP, IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW,
    IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT, LoadCursorW,
};

pub use capture::{Capture, keep_device};

/// One-time process setup. Must run before anything asks for screen metrics,
/// otherwise Windows reports scaled coordinates instead of physical pixels.
pub fn init() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// CSS name of the pointer shape the host is showing right now. The stream
/// carries no pointer; the client draws its own in this shape.
pub fn cursor_name() -> &'static str {
    static SHAPES: OnceLock<Vec<(usize, &'static str)>> = OnceLock::new();
    let shapes = SHAPES.get_or_init(|| {
        [
            (IDC_ARROW, "default"),
            (IDC_IBEAM, "text"),
            (IDC_WAIT, "wait"),
            (IDC_CROSS, "crosshair"),
            (IDC_SIZENWSE, "nwse-resize"),
            (IDC_SIZENESW, "nesw-resize"),
            (IDC_SIZEWE, "ew-resize"),
            (IDC_SIZENS, "ns-resize"),
            (IDC_SIZEALL, "move"),
            (IDC_NO, "not-allowed"),
            (IDC_HAND, "pointer"),
            (IDC_APPSTARTING, "progress"),
            (IDC_HELP, "help"),
        ]
        .into_iter()
        .filter_map(|(id, name)| {
            unsafe { LoadCursorW(None, id) }.ok().map(|h| (h.0 as usize, name))
        })
        .collect()
    });
    let mut info = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
    if unsafe { GetCursorInfo(&mut info) }.is_err() {
        return "default";
    }
    let handle = info.hCursor.0 as usize;
    shapes.iter().find(|(h, _)| *h == handle).map_or("default", |(_, name)| name)
}

/// Whether a process runs with an elevated (administrator) token. A process
/// we are not even allowed to inspect is treated as more privileged than us.
fn process_elevated(pid: u32) -> bool {
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true;
        };
        let mut token = HANDLE::default();
        let elevated = if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_ok() {
            let mut elevation = TOKEN_ELEVATION::default();
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elevation as *mut _ as *mut _),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            !ok || elevation.TokenIsElevated != 0
        } else {
            true
        };
        let _ = CloseHandle(process);
        elevated
    }
}

/// Whether this process runs as administrator.
fn is_elevated() -> bool {
    static ELEVATED: OnceLock<bool> = OnceLock::new();
    *ELEVATED.get_or_init(|| process_elevated(unsafe { GetCurrentProcessId() }))
}

/// Lines to print at startup about what remote input can and cannot do.
pub fn input_notes() -> Vec<String> {
    if is_elevated() {
        return Vec::new();
    }
    vec!["Not running as administrator: windows that are (Task Manager, installers) will ignore remote input.".into()]
}

/// True while the window in front belongs to an administrator process and we
/// are not one: Windows silently drops input injected by a less privileged
/// process, which from the client looks like a frozen session.
pub fn input_blocked() -> bool {
    if is_elevated() {
        return false;
    }
    static CACHE: Mutex<Option<HashMap<u32, bool>>> = Mutex::new(None);
    let mut pid = 0u32;
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return false;
        }
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return false;
    }
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() > 512 {
        cache.clear(); // process ids get reused; don't trust old answers forever
    }
    *cache.entry(pid).or_insert_with(|| process_elevated(pid))
}

pub mod clipboard {
    /// A number that changes every time the clipboard's content does.
    pub fn sequence() -> u64 {
        clipboard_win::raw::seq_num().map_or(0, |n| n.get() as u64)
    }

    /// The clipboard as text, or `None` when it holds none (or is busy).
    pub fn get_text() -> Option<String> {
        clipboard_win::get_clipboard_string().ok().filter(|s| !s.is_empty())
    }

    pub fn set_text(text: &str) -> bool {
        clipboard_win::set_clipboard_string(text).is_ok()
    }
}
