//! Screen capture through DXGI Desktop Duplication.
//!
//! Windows hands over a frame only when something on screen changed, so an
//! idle desktop costs nothing, and the copy stays on the GPU until the final
//! read-back.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1,
    IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
    SRCCOPY, SelectObject,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::core::Interface;

use crate::platform::{Monitor, Pixels};

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
    monitor: Monitor,
}

struct Duplication {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// None while idle with the device kept.
    dup: Option<IDXGIOutputDuplication>,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
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
    /// that is what a resolution change, a UAC prompt or the lock screen
    /// look like from here.
    pub fn acquire(
        &mut self,
        index: usize,
        timeout: Duration,
        f: impl FnOnce(Pixels<'_>),
    ) -> windows::core::Result<bool> {
        // No such monitor is a failure, not "nothing changed": enumerating
        // while the display is off (going to sleep, say) finds none, and
        // only a failure makes the caller look again once it is back.
        let Some(out) = self.outputs.get(index) else {
            return Err(windows::core::Error::new(DXGI_ERROR_NOT_FOUND, "no monitor attached"));
        };
        let monitor = out.monitor;
        let d = match self.dups.entry(index) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(v) => v.insert(open(out)?),
        };
        if d.dup.is_none() {
            let began = Instant::now();
            let dup = unsafe { out.output.DuplicateOutput(&d.device) };
            let dup = match dup {
                Ok(dup) => dup,
                Err(e) => {
                    self.dups.remove(&index);
                    return Err(e);
                }
            };
            d.dup = Some(dup);
            d.fresh = true;
            d.startup = Some(Startup { began, device: None, duplicate: began.elapsed() });
        }
        match unsafe { grab(d, monitor, timeout, f) } {
            Ok(Some(cost)) => {
                self.last_cost = cost;
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => {
                self.dups.remove(&index);
                Err(e)
            }
        }
    }
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
                let r = desc.DesktopCoordinates;
                outputs.push(Output {
                    adapter: adapter.clone(),
                    output: output.cast()?,
                    monitor: Monitor {
                        x: r.left,
                        y: r.top,
                        width: (r.right - r.left) as u32,
                        height: (r.bottom - r.top) as u32,
                    },
                });
            }
        }
    }
    // Primary monitor (the one at the origin) first, then left to right.
    outputs.sort_by_key(|o| ((o.monitor.x, o.monitor.y) != (0, 0), o.monitor.x, o.monitor.y));
    Ok(outputs)
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
unsafe fn next_image(dup: &IDXGIOutputDuplication, wait: Duration) -> windows::core::Result<Option<ID3D11Texture2D>> {
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
            Some(r) if info.LastPresentTime != 0 => Ok(Some(r.cast()?)),
            _ => {
                dup.ReleaseFrame()?;
                Ok(None)
            }
        }
    }
}

/// Returns how long reading the frame took, or `None` if nothing changed.
unsafe fn grab(
    d: &mut Duplication,
    monitor: Monitor,
    timeout: Duration,
    f: impl FnOnce(Pixels<'_>),
) -> windows::core::Result<Option<Duration>> {
    unsafe {
        let dup = d.dup.clone().expect("duplication started before grab");
        let waiting = Instant::now();
        let texture = if d.fresh {
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
        let Some(texture) = texture else {
            if !d.fresh {
                return Ok(None);
            }
            // Still desktop: take the first image the old-fashioned way.
            // Later changes arrive through the duplication as usual (and the
            // engine compares them with this one, so nothing is sent twice).
            let (width, height) = (monitor.width as usize, monitor.height as usize);
            let data = screenshot(monitor)?;
            d.fresh = false;
            f(Pixels { width, height, stride: width * 4, data: &data });
            if let Some(s) = d.startup.take() {
                log_startup(s, waiting.elapsed(), true);
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
        let staging = &d.staging.as_ref().unwrap().0;
        d.context.CopyResource(staging, &texture);
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
        f(Pixels { width, height, stride, data });
        d.context.Unmap(staging, 0);
        d.fresh = false;
        if let Some(s) = d.startup.take() {
            log_startup(s, waiting.elapsed(), false);
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

    /// Needs a real desktop session, so it is not part of the default run:
    /// `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn screenshot_fallback_matches_duplication() {
        crate::platform::init();
        let mut cap = Capture::new();
        let monitor = cap.monitors()[0];
        let mut duplicated = Vec::new();
        let updated = cap
            .acquire(0, Duration::ZERO, |px| {
                for y in 0..px.height {
                    duplicated.extend_from_slice(&px.data[y * px.stride..][..px.width * 4]);
                }
            })
            .unwrap();
        assert!(updated, "a new duplication always yields a first image");
        let shot = screenshot(monitor).unwrap();
        assert_eq!(duplicated.len(), shot.len());
        // The two grabs are milliseconds apart, so allow for a little motion.
        let same = duplicated.chunks(4).zip(shot.chunks(4)).filter(|(a, b)| a[..3] == b[..3]).count();
        let fraction = same as f64 / (shot.len() / 4) as f64;
        assert!(fraction > 0.95, "only {:.1}% of pixels match", fraction * 100.0);
    }
}
