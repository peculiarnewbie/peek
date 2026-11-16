mod capture;
mod desktop;
pub mod input;
pub mod service;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY,
    TOKEN_USER, TokenElevation, TokenUser, WinLocalSystemSid,
};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::{HSTRING, w};
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

/// Whether this process runs as administrator (or as SYSTEM, which is more).
pub fn is_elevated() -> bool {
    static ELEVATED: OnceLock<bool> = OnceLock::new();
    *ELEVATED.get_or_init(|| is_system() || process_elevated(unsafe { GetCurrentProcessId() }))
}

/// Whether this process runs as SYSTEM, as the service and the peek it
/// starts do.
pub fn is_system() -> bool {
    static SYSTEM: OnceLock<bool> = OnceLock::new();
    *SYSTEM.get_or_init(|| unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        // TOKEN_USER points into the rest of the buffer, at its SID.
        let mut buf = [0u64; 32];
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), size_of_val(&buf) as u32, &mut len);
        let _ = CloseHandle(token);
        ok.is_ok() && IsWellKnownSid((*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid, WinLocalSystemSid).as_bool()
    })
}

/// Create a folder that only SYSTEM and administrators can open, unless it
/// exists already. For peek's machine-wide settings: created like any other
/// folder under ProgramData, every user could read the access token.
/// (`peek install --service` sets it up beforehand, also letting the user
/// who installed it read it.)
pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    unsafe {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let created = CreateDirectoryW(&HSTRING::from(path), Some(&attributes));
        LocalFree(Some(HLOCAL(descriptor.0)));
        created?;
    }
    Ok(())
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

    /// The most a pasted image may take once decoded, for the bitmap.
    const MAX_DECODED: usize = 192 << 20;

    /// Put an image on the clipboard: as PNG, which keeps transparency and
    /// is what browsers, Office and chat programs read, and as a bitmap for
    /// everything older (.NET's `Clipboard.GetImage`, and so PowerShell's
    /// `Get-Clipboard -Format Image`, reads only that).
    pub fn set_png(png: &[u8]) -> bool {
        // Decoded before the clipboard is opened, so it is held briefly.
        let dib = dib(png);
        let Ok(_open) = clipboard_win::Clipboard::new_attempts(10) else { return false };
        if clipboard_win::raw::empty().is_err() {
            return false;
        }
        let mut set = false;
        if let Some(format) = clipboard_win::raw::register_format("PNG") {
            set |= clipboard_win::raw::set_without_clear(format.get(), png).is_ok();
        }
        if let Some(dib) = dib {
            set |= clipboard_win::raw::set_without_clear(clipboard_win::formats::CF_DIB, &dib).is_ok();
        }
        set
    }

    /// A PNG as a device-independent bitmap (`CF_DIB`): a BITMAPINFOHEADER,
    /// then 32-bit pixels, bottom row first. Few readers honour alpha in a
    /// DIB, so transparent pixels are laid over white.
    pub(super) fn dib(png: &[u8]) -> Option<Vec<u8>> {
        let mut decoder = png::Decoder::new_with_limits(std::io::Cursor::new(png), png::Limits { bytes: MAX_DECODED });
        decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        let (w, h) = (info.width as usize, info.height as usize);
        let channels = match info.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Grayscale => 1,
            png::ColorType::Indexed => return None,
        };
        let size = w.checked_mul(h)?.checked_mul(4)?;
        let mut out = Vec::with_capacity(40 + size);
        out.extend_from_slice(&40u32.to_le_bytes()); // biSize
        out.extend_from_slice(&i32::try_from(w).ok()?.to_le_bytes());
        out.extend_from_slice(&i32::try_from(h).ok()?.to_le_bytes()); // positive: bottom-up
        out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
        out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        out.extend_from_slice(&u32::try_from(size).ok()?.to_le_bytes());
        out.extend_from_slice(&[0; 16]); // resolution, palette
        let over_white = |c: u8, a: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32) + 127) / 255) as u8;
        for row in buf[..info.line_size * h].chunks_exact(info.line_size).rev() {
            for px in row[..w * channels].chunks_exact(channels) {
                let (r, g, b, a) = match channels {
                    4 => (px[0], px[1], px[2], px[3]),
                    3 => (px[0], px[1], px[2], 255),
                    2 => (px[0], px[0], px[0], px[1]),
                    _ => (px[0], px[0], px[0], 255),
                };
                out.extend_from_slice(&[over_white(b, a), over_white(g, a), over_white(r, a), 255]);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_png_becomes_a_bottom_up_bitmap_over_white() {
        // Two rows: red, then green; then blue, then fully transparent.
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 0]).unwrap();
        }
        let dib = super::clipboard::dib(&png).unwrap();
        assert_eq!(&dib[0..4], &40u32.to_le_bytes());
        assert_eq!(&dib[4..8], &2i32.to_le_bytes());
        assert_eq!(&dib[8..12], &2i32.to_le_bytes());
        assert_eq!(&dib[14..16], &32u16.to_le_bytes());
        // The bottom row first, each pixel as BGRX.
        assert_eq!(&dib[40..], &[255, 0, 0, 255, 255, 255, 255, 255, 0, 0, 255, 255, 0, 255, 0, 255]);
        assert!(super::clipboard::dib(b"not a png").is_none());
    }

    #[test]
    fn private_dir_shuts_out_users() {
        let dir = std::env::temp_dir().join(format!("peek-private-{}", std::process::id())).join("peek");
        super::create_private_dir(&dir).unwrap();
        assert!(dir.is_dir());
        super::create_private_dir(&dir).unwrap();
        if !super::is_elevated() {
            assert!(std::fs::read_dir(&dir).is_err(), "a user could list the private folder");
        }
        // As its owner we may still give the folder ordinary rights back,
        // and need to before deleting it.
        let reset = std::process::Command::new("icacls").arg(&dir).args(["/reset", "/Q"]).output().unwrap();
        assert!(reset.status.success());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
