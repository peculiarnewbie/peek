//! Screen capture through DXGI Desktop Duplication.
//!
//! Windows hands over a frame only when something on screen changed, so an
//! idle desktop costs nothing, and the copy stays on the GPU until the final
//! read-back.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_MOVE_RECT, IDXGIAdapter1,
    IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_MODE_ROTATION_IDENTITY;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, GetMonitorInfoW,
    HMONITOR, MONITORINFO, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::core::Interface;

use crate::platform::{DamageRect, Monitor, Pixels};

/// How long to wait for a new duplication's first real image before falling
/// back to a plain screenshot. The first frame Windows returns carries no
/// image at all; the real one follows when the compositor next runs, which on
/// a completely still desktop may be never. Two refreshes at 60 Hz: a screen
/// that is changing delivers within that, and a still one is not kept waiting
/// (the screenshot takes about as long again).
const FIRST_IMAGE_WAIT: Duration = Duration::from_millis(35);

/// Keep the Direct3D device while idle, dropping only the duplication (see
/// `--keep-gpu`). Connecting from idle then skips creating the device, for
/// some memory, and on a laptop possibly a discrete GPU kept awake.
static KEEP_DEVICE: AtomicBool = AtomicBool::new(false);

pub fn keep_device(keep: bool) {
    KEEP_DEVICE.store(keep, Ordering::Relaxed);
}

struct Output {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
    handle: HMONITOR,
    monitor: Monitor,
    unrotated: bool,
    /// GDI has already supplied an image since the last capture start.
    seeded: bool,
}

struct Duplication {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// None while idle with the device kept.
    dup: Option<IDXGIOutputDuplication>,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    /// Byte storage aligned for both DXGI metadata structures.
    metadata: Vec<u32>,
    damage: Vec<DamageRect>,
    /// No image has been read from this duplication yet.
    fresh: bool,
    /// How starting this duplication went, logged with its first image.
    startup: Option<Startup>,
}

/// Timings of a capture start, from idle, to see where connecting waits.
struct Startup {
    began: Instant,
    /// Creating the device, and how much process memory it took (None when
    /// it was kept from before).
    device: Option<(Duration, i64)>,
    duplicate: Duration,
}

pub struct Capture {
    outputs: Vec<Output>,
    dups: HashMap<usize, Duplication>,
    /// Time the last successful `acquire` spent getting pixels off the GPU
    /// and through the callback, excluding the wait for the screen to change.
    pub last_cost: Duration,
}

impl Capture {
    pub fn new() -> Self {
        let mut c = Capture { outputs: Vec::new(), dups: HashMap::new(), last_cost: Duration::ZERO };
        c.refresh();
        c
    }

    /// Re-enumerate monitors, dropping every open duplication.
    pub fn refresh(&mut self) {
        self.dups.clear();
        follow_desktop();
        self.outputs = enumerate().unwrap_or_else(|e| {
            crate::log!("could not enumerate monitors: {e}");
            Vec::new()
        });
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        self.outputs.iter().map(|o| o.monitor).collect()
    }

    /// Stop duplicating (the device objects go with it, unless kept). Called
    /// when nobody is watching, so the compositor is left alone while idle.
    pub fn release(&mut self) {
        for out in &mut self.outputs {
            out.seeded = false;
        }
        if KEEP_DEVICE.load(Ordering::Relaxed) {
            for d in self.dups.values_mut() {
                d.dup = None;
                d.staging = None;
            }
        } else {
            self.dups.clear();
        }
    }

    /// Wait up to `timeout` for monitor `index` to change. When it has, `f`
    /// is called with the full current frame and `Ok(true)` is returned.
    /// On `Err` the caller should `refresh()` and try again a little later:
    /// that is what a resolution change or the display going to sleep look
    /// like from here. (Input moving to another desktop, for a UAC prompt or
    /// the lock screen, is `Ok(false)`: the next call starts capturing there.)
    pub fn acquire(
        &mut self,
        index: usize,
        timeout: Duration,
        f: impl FnOnce(Pixels<'_>),
    ) -> windows::core::Result<bool> {
        // No such monitor is a failure, not "nothing changed": enumerating
        // while the display is off (going to sleep, say) finds none, and
        // only a failure makes the caller look again once it is back.
        let Some(out) = self.outputs.get_mut(index) else {
            return Err(windows::core::Error::new(DXGI_ERROR_NOT_FOUND, "no monitor attached"));
        };
        if self.dups.get(&index).is_none_or(|d| d.dup.is_none()) {
            if follow_desktop() {
                out.seeded = false;
            }
            // A mode change while duplication was stopped cannot invalidate it.
            // Check the live bounds before opening it or taking the GDI fallback.
            if monitor_bounds(out.handle) != Some(out.monitor) {
                return Err(windows::core::Error::new(DXGI_ERROR_NOT_FOUND, "monitor bounds changed"));
            }
            // Creating the device takes a couple of hundred milliseconds, so
            // show a screenshot first and let duplication start on the next
            // call. A kept device (`--keep-gpu`) delivers its first image
            // sooner than a screenshot takes, so it goes straight to it.
            if !out.seeded && !self.dups.contains_key(&index) {
                let started = Instant::now();
                if let Ok(data) = screenshot(out.monitor) {
                    out.seeded = true;
                    let (w, h) = (out.monitor.width as usize, out.monitor.height as usize);
                    f(Pixels::bgra(w, h, w * 4, &data));
                    self.last_cost = started.elapsed();
                    return Ok(true);
                }
            }
        }
        match duplicate_and_grab(&mut self.dups, out, index, timeout, f) {
            Ok(Some(cost)) => {
                self.last_cost = cost;
                Ok(true)
            }
            Ok(None) => Ok(false),
            // Input moved to another desktop, which is what ended this
            // duplication: start one there on the next call, keeping the
            // device, rather than fail and wait to be retried.
            Err(_) if follow_desktop() => {
                out.seeded = false;
                if let Some(d) = self.dups.get_mut(&index) {
                    d.dup = None;
                    d.staging = None;
                }
                Ok(false)
            }
            Err(e) => {
                self.dups.remove(&index);
                Err(e)
            }
        }
    }
}

/// Open the device and duplication for `out` as far as they are not yet, and
/// grab a frame. Returns how long reading it took, or `None` if nothing
/// changed.
fn duplicate_and_grab(
    dups: &mut HashMap<usize, Duplication>,
    out: &mut Output,
    index: usize,
    timeout: Duration,
    f: impl FnOnce(Pixels<'_>),
) -> windows::core::Result<Option<Duration>> {
    let d = match dups.entry(index) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(v) => v.insert(open(out)?),
    };
    if d.dup.is_none() {
        let began = Instant::now();
        d.dup = Some(unsafe { out.output.DuplicateOutput(&d.device) }?);
        d.fresh = true;
        d.startup = Some(Startup { began, device: None, duplicate: began.elapsed() });
    }
    unsafe { grab(d, out, timeout, f) }
}

/// Duplication only works on the desktop taking input, which a UAC prompt or
/// the lock screen moves away from the user's own (see `desktop`). Called
/// before starting a duplication, and after one failed, which is how a
/// switch shows up while capturing. Returns whether this thread moved.
fn follow_desktop() -> bool {
    let Some(name) = super::desktop::follow() else { return false };
    crate::log!("capture: following input to the {name} desktop");
    true
}

fn enumerate() -> windows::core::Result<Vec<Output>> {
    let mut outputs = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let mut j = 0;
            while let Ok(output) = adapter.EnumOutputs(j) {
                j += 1;
                let desc = output.GetDesc()?;
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let Some(monitor) = monitor_bounds(desc.Monitor) else { continue };
                outputs.push(Output {
                    adapter: adapter.clone(),
                    output: output.cast()?,
                    handle: desc.Monitor,
                    monitor,
                    unrotated: desc.Rotation == DXGI_MODE_ROTATION_IDENTITY,
                    seeded: false,
                });
            }
        }
    }
    // Primary monitor (the one at the origin) first, then left to right.
    outputs.sort_by_key(|o| ((o.monitor.x, o.monitor.y) != (0, 0), o.monitor.x, o.monitor.y));
    Ok(outputs)
}

fn monitor_bounds(handle: HMONITOR) -> Option<Monitor> {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !unsafe { GetMonitorInfoW(handle, &mut info) }.as_bool() {
        return None;
    }
    let r = info.rcMonitor;
    Some(Monitor {
        x: r.left,
        y: r.top,
        width: (r.right - r.left) as u32,
        height: (r.bottom - r.top) as u32,
    })
}

fn open(out: &Output) -> windows::core::Result<Duplication> {
    unsafe {
        let began = Instant::now();
        let memory = private_bytes();
        let mut device = None;
        let mut context = None;
        // The device has to live on the adapter that drives this output.
        D3D11CreateDevice(
            &out.adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device = device.unwrap();
        let created = (began.elapsed(), private_bytes() - memory);
        let dup = out.output.DuplicateOutput(&device)?;
        let duplicate = began.elapsed() - created.0;
        Ok(Duplication {
            device,
            context: context.unwrap(),
            dup: Some(dup),
            staging: None,
            metadata: Vec::new(),
            damage: Vec::new(),
            fresh: true,
            startup: Some(Startup { began, device: Some(created), duplicate }),
        })
    }
}

/// Memory committed to this process alone, in bytes.
fn private_bytes() -> i64 {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    let size = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut counters).cast(), size) };
    if ok.is_ok() { counters.PrivateUsage as i64 } else { 0 }
}

fn log_startup(s: Startup, first: Duration, screenshot: bool) {
    let ms = |d: Duration| format!("{:.0} ms", d.as_secs_f64() * 1000.0);
    let device = match s.device {
        Some((took, bytes)) => format!("device {} (+{:.1} MB)", ms(took), bytes as f64 / (1 << 20) as f64),
        None => "device kept".into(),
    };
    let how = if screenshot { "screenshot, screen still" } else { "duplication" };
    crate::log!(
        "capture started in {}: {device}, duplication {}, first image {} ({how})",
        ms(s.began.elapsed()),
        ms(s.duplicate),
        ms(first),
    );
}

/// One `AcquireNextFrame`. `None` means nothing new to show: the wait timed
/// out, or only the pointer moved (the client draws its own pointer).
unsafe fn next_image(dup: &IDXGIOutputDuplication, wait: Duration) -> windows::core::Result<Option<(ID3D11Texture2D, DXGI_OUTDUPL_FRAME_INFO)>> {
    unsafe {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match dup.AcquireNextFrame(wait.as_millis() as u32, &mut info, &mut resource) {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => return Err(e),
        }
        // A present time of zero means the desktop image was not updated.
        match resource {
            Some(r) if info.LastPresentTime != 0 => match r.cast() {
                Ok(texture) => Ok(Some((texture, info))),
                Err(e) => {
                    let _ = dup.ReleaseFrame();
                    Err(e)
                }
            },
            _ => {
                dup.ReleaseFrame()?;
                Ok(None)
            }
        }
    }
}

/// Read actual pixels for the destinations of moves, as well as dirty
/// rectangles, from the new texture. The source of a move needs no copy.
fn read_damage(
    d: &mut Duplication, dup: &IDXGIOutputDuplication, size: u32, width: u32, height: u32,
) -> Option<()> {
    const MAX_DAMAGE_RECTS: usize = 64;
    let clamp = |r: RECT| {
        let x = r.left.clamp(0, width as i32) as usize;
        let y = r.top.clamp(0, height as i32) as usize;
        let w = (r.right.clamp(0, width as i32) as usize).saturating_sub(x);
        let h = (r.bottom.clamp(0, height as i32) as usize).saturating_sub(y);
        (w != 0 && h != 0).then_some(DamageRect { x, y, w, h })
    };
    d.metadata.resize((size as usize).div_ceil(size_of::<u32>()), 0);
    let ptr = d.metadata.as_mut_ptr();
    let mut used = 0;
    // Both C structs are made of 32-bit integers. Reuse the aligned buffer
    // once the move destinations have been copied out.
    const { assert!(align_of::<DXGI_OUTDUPL_MOVE_RECT>() <= align_of::<u32>()) };
    const { assert!(align_of::<RECT>() <= align_of::<u32>()) };
    unsafe {
        dup.GetFrameMoveRects(size, ptr.cast(), &mut used).ok()?;
        let moves = metadata_slice::<DXGI_OUTDUPL_MOVE_RECT>(&d.metadata, size, used)?;
        if moves.len() > MAX_DAMAGE_RECTS {
            return None;
        }
        let move_count = moves.len();
        d.damage.extend(moves.iter().filter_map(|m| clamp(m.DestinationRect)));

        dup.GetFrameDirtyRects(size, ptr.cast(), &mut used).ok()?;
        let dirty = metadata_slice::<RECT>(&d.metadata, size, used)?;
        if dirty.len() + move_count > MAX_DAMAGE_RECTS {
            return None;
        }
        d.damage.extend(dirty.iter().filter_map(|&r| clamp(r)));
    }
    let area: u64 = d.damage.iter().map(|r| r.w as u64 * r.h as u64).sum();
    let screen_area = u64::from(width) * u64::from(height);
    (!d.damage.is_empty() && area < screen_area.div_ceil(2)).then_some(())
}

/// The initialized prefix returned by the matching DXGI metadata call.
unsafe fn metadata_slice<T>(buffer: &[u32], size: u32, used: u32) -> Option<&[T]> {
    let len = used as usize;
    (used <= size && len.is_multiple_of(size_of::<T>()))
        .then(|| unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast(), len / size_of::<T>()) })
}

/// Returns how long reading the frame took, or `None` if nothing changed.
unsafe fn grab(
    d: &mut Duplication,
    out: &mut Output,
    timeout: Duration,
    f: impl FnOnce(Pixels<'_>),
) -> windows::core::Result<Option<Duration>> {
    unsafe {
        let dup = d.dup.clone().expect("duplication started before grab");
        let waiting = Instant::now();
        let first_image = d.fresh && !out.seeded;
        let texture = if first_image {
            let deadline = waiting + FIRST_IMAGE_WAIT;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match next_image(&dup, left)? {
                    Some(texture) => break Some(texture),
                    None if left.is_zero() => break None,
                    None => {}
                }
            }
        } else {
            next_image(&dup, timeout)?
        };
        let started = Instant::now();
        let Some((texture, info)) = texture else {
            if !first_image {
                return Ok(None);
            }
            // Still desktop: take the first image the old-fashioned way.
            // Later changes arrive through the duplication as usual (and the
            // engine compares them with this one, so nothing is sent twice).
            let (width, height) = (out.monitor.width as usize, out.monitor.height as usize);
            let data = screenshot(out.monitor)?;
            out.seeded = true;
            f(Pixels::bgra(width, height, width * 4, &data));
            if let Some(s) = d.startup.take() {
                log_startup(s, started - waiting, true);
            }
            return Ok(Some(started.elapsed()));
        };

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let reusable = matches!(&d.staging, Some((_, w, h)) if *w == desc.Width && *h == desc.Height);
        if !reusable {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut staging = None;
            d.device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            d.staging = Some((staging.unwrap(), desc.Width, desc.Height));
        }
        d.damage.clear();
        let partial = reusable && !d.fresh && out.unrotated && info.TotalMetadataBufferSize > 0
            && read_damage(d, &dup, info.TotalMetadataBufferSize, desc.Width, desc.Height).is_some();
        let staging = &d.staging.as_ref().unwrap().0;
        if partial {
            for r in &d.damage {
                let src = D3D11_BOX {
                    left: r.x as u32, top: r.y as u32, front: 0,
                    right: r.right() as u32, bottom: r.bottom() as u32, back: 1,
                };
                d.context.CopySubresourceRegion(staging, 0, src.left, src.top, 0, &texture, 0, Some(&src));
            }
        } else {
            d.context.CopyResource(staging, &texture);
        }
        drop(texture);
        dup.ReleaseFrame()?;

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        d.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let (width, height) = (desc.Width as usize, desc.Height as usize);
        let stride = mapped.RowPitch as usize;
        let data = std::slice::from_raw_parts(
            mapped.pData as *const u8,
            stride * (height - 1) + width * 4,
        );
        f(Pixels { damage: partial.then_some(d.damage.as_slice()), ..Pixels::bgra(width, height, stride, data) });
        d.context.Unmap(staging, 0);
        d.fresh = false;
        if let Some(s) = d.startup.take() {
            log_startup(s, started - waiting, false);
        }
        Ok(Some(started.elapsed()))
    }
}

/// A plain GDI screenshot of one monitor as top-down BGRA.
fn screenshot(m: Monitor) -> windows::core::Result<Vec<u8>> {
    let (w, h) = (m.width as i32, m.height as i32);
    let mut pixels = vec![0u8; m.width as usize * m.height as usize * 4];
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let previous = SelectObject(memory, bitmap.into());
        let copied = BitBlt(memory, 0, 0, w, h, Some(screen), m.x, m.y, SRCCOPY | CAPTUREBLT);
        SelectObject(memory, previous);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative: rows run top to bottom
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let rows = if copied.is_ok() {
            GetDIBits(memory, bitmap, 0, h as u32, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS)
        } else {
            0
        };
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        copied?;
        if rows == 0 {
            return Err(windows::core::Error::from_thread());
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate a resolution change while there is no active duplication.
    #[test]
    #[ignore = "needs a Windows desktop session"]
    fn stale_monitor_bounds_are_rejected_on_capture_start() {
        crate::platform::init();
        let mut cap = Capture::new();
        let monitor = cap.monitors()[0];
        cap.outputs[0].monitor.width += 1;

        let err = cap.acquire(0, Duration::ZERO, |_| panic!("captured with stale bounds")).unwrap_err();
        assert_eq!(err.code(), DXGI_ERROR_NOT_FOUND);
        cap.refresh();
        assert_eq!(cap.monitors()[0], monitor);
    }

    /// Needs a real desktop session, so it is not part of the default run:
    /// `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn screenshot_fallback_matches_duplication() {
        crate::platform::init();
        let mut cap = Capture::new();
        let monitor = cap.monitors()[0];
        let mut duplicated = Vec::new();
        let updated = duplicate_and_grab(&mut cap.dups, &mut cap.outputs[0], 0, Duration::ZERO, |px| {
                for y in 0..px.height {
                    duplicated.extend_from_slice(&px.data[y * px.stride..][..px.width * 4]);
                }
            })
            .unwrap();
        assert!(updated.is_some(), "a new duplication always yields a first image");
        let shot = screenshot(monitor).unwrap();
        assert_eq!(duplicated.len(), shot.len());
        // The two grabs are milliseconds apart, so allow for a little motion.
        let same = duplicated.chunks(4).zip(shot.chunks(4)).filter(|(a, b)| a[..3] == b[..3]).count();
        let fraction = same as f64 / (shot.len() / 4) as f64;
        assert!(fraction > 0.95, "only {:.1}% of pixels match", fraction * 100.0);
    }
}
