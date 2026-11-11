//! Everything OS-specific lives behind this module. A backend provides:
//!
//! - `init()`: one-time process setup.
//! - `Capture`: `new()`, `monitors()`, `acquire()`, `release()`.
//! - `input`: `mouse_move`, `button`, `wheel`, `key`, `text`.
//! - `clipboard`: `sequence`, `get_text`, `set_text`.
//! - `cursor_name()`: CSS cursor name for the pointer shape currently shown.
//! - `input_blocked()`: true while the OS is refusing our injected input.
//! - `is_elevated()`: whether this process can control privileged windows.

/// A monitor's position and size on the desktop, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Pixels handed to the engine for one captured frame. Rows are `stride`
/// bytes apart and hold `width` BGRA pixels (the fourth byte is ignored).
pub struct Pixels<'a> {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub data: &'a [u8],
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(not(windows))]
compile_error!("peek only has a Windows backend so far");
