//! Wayland backend, for wlroots-style compositors such as niri and sway.
//!
//! - Capture: wlr-screencopy into shared memory, with damage, so a still
//!   screen costs nothing.
//! - Pointer: a wlr virtual pointer per monitor, positioned absolutely.
//! - Keys: a uinput keyboard, so they go through the compositor like a real
//!   keyboard's (its layout and its own shortcuts apply).
//! - Text: a Wayland virtual keyboard with a keymap made for the text.
//! - Clipboard: wlr-data-control.
//!
//! One Wayland connection is shared. Capture, input and clipboard each have
//! their own event queue on it, used from their own threads.

mod capture;
pub mod clipboard;
pub mod input;

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::{Connection, EventQueue};

use crate::platform::Monitor;

pub use capture::Capture;

pub type Error = String;

fn connection() -> Result<&'static Connection, Error> {
    static CONN: OnceLock<Result<Connection, Error>> = OnceLock::new();
    CONN.get_or_init(|| {
        Connection::connect_to_env().map_err(|e| format!("cannot connect to the Wayland compositor: {e}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Monitors as capture last saw them, with the output each one is, so input
/// can aim the pointer at the right one. `generation` changes whenever the
/// list does.
struct Layout {
    generation: u64,
    screens: Vec<(Monitor, WlOutput)>,
}

static LAYOUT: RwLock<Layout> = RwLock::new(Layout { generation: 0, screens: Vec::new() });

fn set_layout(screens: Vec<(Monitor, WlOutput)>) {
    let mut layout = LAYOUT.write().unwrap();
    layout.generation += 1;
    layout.screens = screens;
}

/// One-time process setup: check the compositor is reachable and create the
/// virtual keyboard early (a new input device takes a moment to be picked up).
pub fn init() {
    if let Err(e) = connection() {
        eprintln!("{e}. peek needs a Wayland session (WAYLAND_DISPLAY).");
        std::process::exit(1);
    }
    input::init();
}

/// Lines to print at startup about what remote input can and cannot do.
pub fn input_notes() -> Vec<String> {
    input::notes()
}

/// Wayland does not tell other programs which pointer shape is showing.
pub fn cursor_name() -> &'static str {
    "default"
}

/// Nothing on Wayland silently drops our input the way Windows does for
/// administrator windows.
pub fn input_blocked() -> bool {
    false
}

/// Dispatch whatever arrives for `queue` within `timeout`. Returns early as
/// soon as anything was dispatched.
fn dispatch_timeout<S>(queue: &mut EventQueue<S>, state: &mut S, timeout: Duration) -> Result<usize, Error> {
    dispatch_interruptible(queue, state, timeout, None).map(|n| n.unwrap_or(0))
}

/// None means a command interrupted the capture wait.
fn dispatch_interruptible<S>(
    queue: &mut EventQueue<S>,
    state: &mut S,
    timeout: Duration,
    wakeup: Option<&Wakeup>,
) -> Result<Option<usize>, Error> {
    let err = |e: &dyn std::fmt::Display| format!("Wayland connection lost: {e}");
    let n = queue.dispatch_pending(state).map_err(|e| err(&e))?;
    if n > 0 {
        return Ok(Some(n));
    }
    queue.flush().map_err(|e| err(&e))?;
    if let Some(guard) = queue.prepare_read() {
        match wait_readable_or_command(guard.connection_fd(), timeout, wakeup) {
            Ready::Command => return Ok(None),
            Ready::Wayland => match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(err(&e)),
            },
            Ready::Timeout => {}
        }
    }
    queue.dispatch_pending(state).map(Some).map_err(|e| err(&e))
}

/// The engine and its command senders share one non-blocking counter. A
/// command is queued before signalling, and the engine clears the counter
/// before draining commands, so a command cannot be lost between waits.
pub struct Wakeup(OwnedFd);

impl Wakeup {
    pub fn new() -> std::io::Result<Self> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    pub fn signal(&self) {
        let one = 1u64;
        while unsafe { libc::write(self.0.as_raw_fd(), (&raw const one).cast(), size_of::<u64>()) } < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
    }

    pub fn clear(&self) {
        let mut count = 0u64;
        while unsafe { libc::read(self.0.as_raw_fd(), (&raw mut count).cast(), size_of::<u64>()) } < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

enum Ready {
    Wayland,
    Command,
    Timeout,
}

fn wait_readable(fd: BorrowedFd<'_>, timeout: Duration) -> bool {
    matches!(wait_readable_or_command(fd, timeout, None), Ready::Wayland)
}

fn wait_readable_or_command(fd: BorrowedFd<'_>, timeout: Duration, wakeup: Option<&Wakeup>) -> Ready {
    let mut fds = [
        libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: wakeup.map_or(-1, |w| w.0.as_raw_fd()), events: libc::POLLIN, revents: 0 },
    ];
    let deadline = std::time::Instant::now() + timeout;
    let mut left = timeout;
    loop {
        let ms = left.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32;
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        if r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            left = deadline.saturating_duration_since(std::time::Instant::now());
            if !left.is_zero() {
                continue;
            }
        }
        return match fds {
            [_, command] if r > 0 && command.revents != 0 => Ready::Command,
            [wayland, _] if r > 0 && wayland.revents != 0 => Ready::Wayland,
            _ => Ready::Timeout,
        };
    }
}

/// An anonymous shared-memory file of `len` bytes.
fn memfd(name: &str, len: usize) -> std::io::Result<OwnedFd> {
    let name = std::ffi::CString::new(name).unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::ftruncate(fd.as_fd().as_raw_fd(), len as libc::off_t) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd)
}

/// A pipe as (read end, write end), both close-on-exec.
fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn capture_poll_is_interruptible_by_commands() {
        use std::os::fd::AsFd;

        let (read, _write) = super::pipe().unwrap();
        let wakeup = super::Wakeup::new().unwrap();
        wakeup.signal();
        wakeup.signal();
        assert!(matches!(
            super::wait_readable_or_command(read.as_fd(), Duration::ZERO, Some(&wakeup)),
            super::Ready::Command
        ));
        wakeup.clear();
        assert!(matches!(
            super::wait_readable_or_command(read.as_fd(), Duration::ZERO, Some(&wakeup)),
            super::Ready::Timeout
        ));
    }

    /// Clicks, types and sets the clipboard, so it only runs against a
    /// throwaway compositor, such as a nested niri with a terminal focused:
    /// `PEEK_TEST_NESTED=1 WAYLAND_DISPLAY=wayland-2 cargo test -- --ignored nested`
    #[test]
    #[ignore]
    fn nested_input_and_clipboard() {
        if std::env::var_os("PEEK_TEST_NESTED").is_none() {
            eprintln!("skipped: set PEEK_TEST_NESTED=1 and point WAYLAND_DISPLAY at a nested compositor");
            return;
        }
        let cap = super::Capture::new();
        let m = cap.monitors()[0];
        super::input::mouse_move(m.x + m.width as i32 / 2, m.y + m.height as i32 / 2);
        super::input::button(0, true);
        super::input::button(0, false);
        std::thread::sleep(Duration::from_millis(200));
        super::input::text("héllo € 你好\tend\n");
        std::thread::sleep(Duration::from_millis(300));
        assert!(super::input::keymap_restored(), "the seat's keymap is active again after typing");

        let seq = super::clipboard::sequence();
        assert!(super::clipboard::set_text("from peek ✓"));
        std::thread::sleep(Duration::from_millis(300));
        assert_ne!(super::clipboard::sequence(), seq);
        assert_eq!(super::clipboard::get_text().as_deref(), Some("from peek ✓"));
    }
}
