//! Screen capture through wlr-screencopy.
//!
//! After the first image of a monitor, every request asks for a copy "with
//! damage": the compositor holds it until the screen next changes, so a
//! still desktop costs nothing. A request that times out stays queued and
//! is picked up by the next `acquire`.

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::{self, Transform, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{Format, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self, Flags, ZwlrScreencopyFrameV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use super::Error;
use crate::platform::{Monitor, Pixels};

/// How long to wait for a monitor's first image. It is copied straight
/// away, without waiting for a change, so this only covers a busy compositor.
const FIRST_IMAGE_WAIT: Duration = Duration::from_secs(1);

#[derive(Default)]
struct OutputInfo {
    global: u32,
    output: Option<WlOutput>,
    name: String,
    position: (i32, i32),
    /// Current mode, in physical pixels, before the output's rotation.
    mode: (i32, i32),
    transform: Option<Transform>,
    /// The first `done` has arrived.
    described: bool,
}

impl OutputInfo {
    fn size(&self) -> (u32, u32) {
        let (w, h) = (self.mode.0.max(0) as u32, self.mode.1.max(0) as u32);
        match self.transform {
            Some(
                Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270,
            ) => (h, w),
            _ => (w, h),
        }
    }
}

#[derive(Default)]
struct FrameEvents {
    /// Shared-memory format, width, height, stride.
    buffer: Option<(WEnum<Format>, u32, u32, u32)>,
    buffer_done: bool,
    y_invert: bool,
    ready: bool,
    failed: bool,
}

#[derive(Default)]
struct State {
    /// Every global the compositor announced: name, interface, version.
    globals: Vec<(u32, String, u32)>,
    outputs: Vec<OutputInfo>,
    /// Outputs came, went or changed mode since the last `refresh`.
    changed: bool,
    /// Events for frames in flight, by frame id.
    frames: HashMap<u64, FrameEvents>,
}

struct Pending {
    frame: ZwlrScreencopyFrameV1,
    id: u64,
    copied: bool,
}

/// A shared-memory buffer the compositor copies into.
struct ShmBuffer {
    _pool: WlShmPool,
    buffer: WlBuffer,
    _fd: OwnedFd,
    map: *mut u8,
    len: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: Format,
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self._pool.destroy();
        unsafe { libc::munmap(self.map.cast(), self.len) };
    }
}

pub struct Capture {
    queue: Option<EventQueue<State>>,
    state: State,
    registry: Option<WlRegistry>,
    shm: Option<WlShm>,
    /// Monitors in the order the engine numbers them, with their outputs.
    order: Vec<(Monitor, usize)>,
    /// One screencopy manager per monitor being captured: the compositor
    /// tracks damage per manager, and destroying it ends the "screencast".
    managers: HashMap<usize, ZwlrScreencopyManagerV1>,
    pending: HashMap<usize, Pending>,
    buffers: HashMap<usize, ShmBuffer>,
    /// Monitors an image has been read from since they were opened.
    seen: std::collections::HashSet<usize>,
    next_frame: u64,
    error: Option<Error>,
    /// Time the last successful `acquire` spent copying pixels and running
    /// the callback, excluding the wait for the screen to change.
    pub last_cost: Duration,
}

impl Capture {
    pub fn new() -> Self {
        let mut c = Capture {
            queue: None,
            state: State::default(),
            registry: None,
            shm: None,
            order: Vec::new(),
            managers: HashMap::new(),
            pending: HashMap::new(),
            buffers: HashMap::new(),
            seen: Default::default(),
            next_frame: 0,
            error: None,
            last_cost: Duration::ZERO,
        };
        match super::connection() {
            Ok(conn) => {
                let queue = conn.new_event_queue();
                c.registry = Some(conn.display().get_registry(&queue.handle(), ()));
                c.queue = Some(queue);
                c.refresh();
            }
            Err(e) => c.error = Some(e),
        }
        c
    }

    fn qh(&self) -> QueueHandle<State> {
        self.queue.as_ref().unwrap().handle()
    }

    fn roundtrip(&mut self) -> Result<(), Error> {
        let queue = self.queue.as_mut().ok_or_else(|| self.error.clone().unwrap_or_default())?;
        queue.roundtrip(&mut self.state).map(|_| ()).map_err(|e| format!("Wayland connection lost: {e}"))
    }

    /// Re-enumerate monitors, dropping every capture in progress.
    pub fn refresh(&mut self) {
        self.release();
        if self.queue.is_none() {
            return;
        }
        for o in self.state.outputs.drain(..) {
            if let Some(output) = o.output {
                if output.version() >= 3 {
                    output.release();
                }
            }
        }
        let result = (|| {
            self.roundtrip()?; // the registry's globals
            let qh = self.qh();
            let registry = self.registry.as_ref().unwrap();
            if self.shm.is_none() {
                if let Some(&(name, _, _)) = self.state.globals.iter().find(|g| g.1 == "wl_shm") {
                    self.shm = Some(registry.bind(name, 1, &qh, ()));
                }
            }
            for &(name, ref interface, version) in &self.state.globals {
                if interface == "wl_output" {
                    let index = self.state.outputs.len();
                    let output = registry.bind(name, version.min(4), &qh, index);
                    self.state.outputs.push(OutputInfo { global: name, output: Some(output), ..Default::default() });
                }
            }
            self.roundtrip() // the outputs' details
        })();
        if let Err(e) = result {
            crate::log!("could not enumerate monitors: {e}");
        }
        self.state.changed = false;

        let mut outputs: Vec<usize> = (0..self.state.outputs.len())
            .filter(|&i| {
                let (w, h) = self.state.outputs[i].size();
                w > 0 && h > 0
            })
            .collect();
        // Left to right, then top to bottom, as the compositor arranges them.
        outputs.sort_by_key(|&i| {
            let (x, y) = self.state.outputs[i].position;
            (x, y)
        });
        // Positions are only used to tell monitors apart when injecting
        // input, so they are laid side by side in physical pixels (logical
        // positions would overlap where monitors use different scales).
        let mut x = 0;
        self.order = outputs
            .into_iter()
            .map(|i| {
                let (width, height) = self.state.outputs[i].size();
                let m = Monitor { x, y: 0, width, height };
                x += width as i32;
                (m, i)
            })
            .collect();
        super::set_layout(
            self.order
                .iter()
                .filter_map(|&(m, i)| Some((m, self.state.outputs[i].output.clone()?)))
                .collect(),
        );
        if self.error.is_none() && self.order.is_empty() {
            crate::log!(
                "no monitors to capture{}",
                if self.state.globals.iter().any(|g| g.1 == "zwlr_screencopy_manager_v1") {
                    ""
                } else {
                    " (this compositor does not offer wlr-screencopy)"
                }
            );
        }
        for &(m, i) in &self.order {
            let name = &self.state.outputs[i].name;
            crate::log!("output {name}: {}x{}", m.width, m.height);
        }
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        self.order.iter().map(|(m, _)| *m).collect()
    }

    /// Stop capturing. Called when nobody is watching, so the compositor
    /// stops treating us as a screencast while idle.
    pub fn release(&mut self) {
        for (_, p) in self.pending.drain() {
            p.frame.destroy();
            self.state.frames.remove(&p.id);
        }
        for (_, m) in self.managers.drain() {
            m.destroy();
        }
        self.buffers.clear();
        self.seen.clear();
        if let Some(q) = &self.queue {
            let _ = q.flush();
        }
    }

    /// Wait up to `timeout` for monitor `index` to change. When it has, `f`
    /// is called with the full current frame and `Ok(true)` is returned.
    /// On `Err` the caller should `refresh()` and try again a little later.
    pub fn acquire(&mut self, index: usize, timeout: Duration, f: impl FnOnce(Pixels<'_>)) -> Result<bool, Error> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if self.state.changed {
            return Err("monitors changed".into());
        }
        let Some(&(monitor, output_index)) = self.order.get(index) else { return Ok(false) };
        let fresh = !self.seen.contains(&index);
        let deadline = Instant::now() + if fresh { FIRST_IMAGE_WAIT.max(timeout) } else { timeout };

        if !self.pending.contains_key(&index) {
            let manager = match self.managers.get(&index) {
                Some(m) => m.clone(),
                None => {
                    let &(name, _, version) = self
                        .state
                        .globals
                        .iter()
                        .find(|g| g.1 == "zwlr_screencopy_manager_v1")
                        .ok_or("this compositor does not offer wlr-screencopy")?;
                    let m: ZwlrScreencopyManagerV1 =
                        self.registry.as_ref().unwrap().bind(name, version.min(3), &self.qh(), ());
                    self.managers.insert(index, m.clone());
                    m
                }
            };
            let output = self.state.outputs[output_index].output.clone().ok_or("monitor is gone")?;
            let id = self.next_frame;
            self.next_frame += 1;
            self.state.frames.insert(id, FrameEvents::default());
            let frame = manager.capture_output(0, &output, &self.qh(), id);
            self.pending.insert(index, Pending { frame, id, copied: false });
        }

        loop {
            let p = self.pending.get_mut(&index).unwrap();
            let ev = self.state.frames.get(&p.id).unwrap();
            if ev.failed {
                self.drop_pending(index);
                return Err("the compositor could not copy the screen".into());
            }
            if ev.ready {
                break;
            }
            let described = if p.frame.version() >= 3 { ev.buffer_done } else { ev.buffer.is_some() };
            if !p.copied && described {
                let Some((format, width, height, stride)) = ev.buffer else {
                    self.drop_pending(index);
                    return Err("the compositor offers no shared-memory format".into());
                };
                let format = match format {
                    WEnum::Value(f @ (Format::Xrgb8888 | Format::Argb8888 | Format::Xbgr8888 | Format::Abgr8888)) => f,
                    other => {
                        self.drop_pending(index);
                        return Err(format!("unsupported pixel format {other:?}"));
                    }
                };
                if (width, height) != (monitor.width, monitor.height) {
                    self.drop_pending(index);
                    self.state.changed = true;
                    return Err("monitor size changed".into());
                }
                let buffer = self.buffer(index, width, height, stride, format)?.clone();
                let p = self.pending.get_mut(&index).unwrap();
                if fresh {
                    p.frame.copy(&buffer);
                } else {
                    p.frame.copy_with_damage(&buffer);
                }
                p.copied = true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let _ = self.queue.as_ref().unwrap().flush();
                return Ok(false);
            }
            super::dispatch_timeout(self.queue.as_mut().unwrap(), &mut self.state, left)?;
        }

        let started = Instant::now();
        let p = self.pending.remove(&index).unwrap();
        let ev = self.state.frames.remove(&p.id).unwrap();
        p.frame.destroy();
        let b = &self.buffers[&index];
        let (width, height, stride) = (b.width as usize, b.height as usize, b.stride as usize);
        let data = unsafe { std::slice::from_raw_parts(b.map, b.len) };
        let needs_copy = ev.y_invert || matches!(b.format, Format::Xbgr8888 | Format::Abgr8888);
        if needs_copy {
            let swap = matches!(b.format, Format::Xbgr8888 | Format::Abgr8888);
            let mut out = vec![0u8; width * height * 4];
            for y in 0..height {
                let from = if ev.y_invert { height - 1 - y } else { y };
                let src = &data[from * stride..][..width * 4];
                let dst = &mut out[y * width * 4..][..width * 4];
                dst.copy_from_slice(src);
                if swap {
                    for px in dst.chunks_exact_mut(4) {
                        px.swap(0, 2);
                    }
                }
            }
            f(Pixels { width, height, stride: width * 4, data: &out });
        } else {
            f(Pixels { width, height, stride, data });
        }
        self.seen.insert(index);
        self.last_cost = started.elapsed();
        Ok(true)
    }

    fn drop_pending(&mut self, index: usize) {
        if let Some(p) = self.pending.remove(&index) {
            p.frame.destroy();
            self.state.frames.remove(&p.id);
        }
    }

    /// The buffer for monitor `index`, reused while its shape stays the same.
    fn buffer(&mut self, index: usize, width: u32, height: u32, stride: u32, format: Format) -> Result<&WlBuffer, Error> {
        let reusable = matches!(self.buffers.get(&index),
            Some(b) if (b.width, b.height, b.stride, b.format) == (width, height, stride, format));
        if !reusable {
            self.buffers.remove(&index);
            let shm = self.shm.as_ref().ok_or("the compositor offers no shared memory")?;
            let len = stride as usize * height as usize;
            let fd = super::memfd("peek-capture", len).map_err(|e| format!("cannot allocate a capture buffer: {e}"))?;
            let map = unsafe {
                libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, std::os::fd::AsRawFd::as_raw_fd(&fd), 0)
            };
            if map == libc::MAP_FAILED {
                return Err(format!("cannot map a capture buffer: {}", std::io::Error::last_os_error()));
            }
            let qh = self.qh();
            let pool = shm.create_pool(fd.as_fd(), len as i32, &qh, ());
            let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, &qh, ());
            self.buffers.insert(
                index,
                ShmBuffer { _pool: pool, buffer, _fd: fd, map: map.cast(), len, width, height, stride, format },
            );
        }
        Ok(&self.buffers[&index].buffer)
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(state: &mut Self, _: &WlRegistry, event: wl_registry::Event, _: &(), _: &wayland_client::Connection, _: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, version } => {
                // (`refresh` clears this after its own enumeration.)
                state.changed |= interface == "wl_output";
                state.globals.push((name, interface, version));
            }
            wl_registry::Event::GlobalRemove { name } => {
                if state.outputs.iter().any(|o| o.global == name) {
                    state.changed = true;
                }
                state.globals.retain(|g| g.0 != name);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, usize> for State {
    fn event(state: &mut Self, _: &WlOutput, event: wl_output::Event, &index: &usize, _: &wayland_client::Connection, _: &QueueHandle<Self>) {
        let Some(o) = state.outputs.get_mut(index) else { return };
        match event {
            wl_output::Event::Geometry { x, y, transform, .. } => {
                o.position = (x, y);
                o.transform = transform.into_result().ok();
            }
            wl_output::Event::Mode { flags, width, height, .. } => {
                if flags.into_result().is_ok_and(|f| f.contains(wl_output::Mode::Current)) {
                    o.mode = (width, height);
                }
            }
            wl_output::Event::Name { name } => o.name = name,
            // A change after the first description: re-enumerate.
            wl_output::Event::Done => {
                state.changed |= o.described;
                o.described = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, u64> for State {
    fn event(state: &mut Self, _: &ZwlrScreencopyFrameV1, event: zwlr_screencopy_frame_v1::Event, id: &u64, _: &wayland_client::Connection, _: &QueueHandle<Self>) {
        let Some(f) = state.frames.get_mut(id) else { return };
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                f.buffer = Some((format, width, height, stride));
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => f.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Flags { flags } => {
                f.y_invert = flags.into_result().is_ok_and(|fl| fl.contains(Flags::YInvert));
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => f.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => f.failed = true,
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &wayland_client::Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlShm, WlShmPool, WlBuffer, ZwlrScreencopyManagerV1);

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a running Wayland session: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn captures_a_first_image_and_then_waits_for_damage() {
        let mut cap = Capture::new();
        let monitor = cap.monitors()[0];
        let mut size = (0, 0);
        assert!(cap.acquire(0, Duration::ZERO, |px| size = (px.width, px.height)).unwrap());
        assert_eq!(size, (monitor.width as usize, monitor.height as usize));
        // The next copy waits for damage; it must not fail either way.
        cap.acquire(0, Duration::from_millis(50), |_| {}).unwrap();
        cap.release();
    }
}


