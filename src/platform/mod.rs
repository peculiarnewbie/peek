//! Everything OS-specific lives behind this module. A backend provides:
//!
//! - `init()`: one-time process setup.
//! - `Capture`: `new()`, `monitors()`, `acquire()`, `release()`.
//! - `input`: `mouse_move`, `button`, `wheel`, `key`, `text`.
//! - `clipboard`: `sequence`, `get_text`, `set_text`, `get_png`, `set_png`.
//! - `cursor_name()`: CSS cursor name for the pointer shape currently shown.
//! - `input_blocked()`: true while the OS is refusing our injected input.
//! - `input_notes()`: lines for the console about what remote input cannot do.

/// A monitor's position and size on the desktop, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Damage in top-down screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl DamageRect {
    pub fn right(self) -> usize {
        self.x.saturating_add(self.w)
    }

    pub fn bottom(self) -> usize {
        self.y.saturating_add(self.h)
    }
}

/// Pixels handed to the engine for one captured frame. Rows are `stride`
/// bytes apart and hold `width` BGRA pixels (the fourth byte is ignored).
pub struct Pixels<'a> {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub data: &'a [u8],
    /// Regions changed since the previous delivered frame. None or empty
    /// means the whole frame should be checked.
    pub damage: Option<&'a [DamageRect]>,
    pub y_invert: bool,
    pub swap_rb: bool,
}

impl<'a> Pixels<'a> {
    pub fn bgra(width: usize, height: usize, stride: usize, data: &'a [u8]) -> Self {
        Self { width, height, stride, data, damage: None, y_invert: false, swap_rb: false }
    }
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

#[cfg(not(any(windows, target_os = "linux")))]
compile_error!("peek has Windows and Linux (Wayland) backends only");
