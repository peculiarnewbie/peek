//! The vitals provider (`peek --provider vitals`): what the machine is
//! doing, as numbers: processor, memory, storage. Needs no display, so it
//! runs beside the core, also in the Windows service's session 0.
//!
//! Every client's socket is a channel here. Each gets a reading as soon as
//! it opens, then one a second. Nothing is read while no channel is open.

use std::fs::File;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::provider;

/// Processor time so far, summed over every core: (busy, total), in any unit.
#[cfg(windows)]
fn cpu_times() -> Option<(u64, u64)> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetSystemTimes;
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    let n = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    // Kernel time includes idle time.
    let total = n(kernel) + n(user);
    Some((total.saturating_sub(n(idle)), total))
}

/// Physical memory in bytes: (used, total).
#[cfg(windows)]
fn memory() -> Option<(u64, u64)> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok()?;
    Some((m.ullTotalPhys - m.ullAvailPhys, m.ullTotalPhys))
}

/// Fixed drives: (name, used bytes, total bytes).
#[cfg(windows)]
fn disks() -> Vec<(String, u64, u64)> {
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
    use windows::core::HSTRING;
    const DRIVE_FIXED: u32 = 3;
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let name = format!("{}:", (b'A' + i) as char);
            let root = HSTRING::from(format!("{name}\\"));
            if unsafe { GetDriveTypeW(&root) } != DRIVE_FIXED {
                return None;
            }
            let (mut free, mut total) = (0u64, 0u64);
            unsafe { GetDiskFreeSpaceExW(&root, None, Some(&mut total), Some(&mut free)) }.ok()?;
            Some((name, total - free, total))
        })
        .collect()
}

/// Seconds since the machine started.
#[cfg(windows)]
fn uptime() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() / 1000 }
}

// The Linux half is written from the formats of /proc and has not been run.
#[cfg(target_os = "linux")]
fn cpu_times() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let fields: Vec<u64> = stat.lines().next()?.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    // user nice system idle iowait irq softirq steal
    let total: u64 = fields.iter().take(8).sum();
    let idle = fields.get(3)? + fields.get(4).copied().unwrap_or(0);
    Some((total.saturating_sub(idle), total))
}

#[cfg(target_os = "linux")]
fn memory() -> Option<(u64, u64)> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb = |key: &str| {
        info.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let (total, available) = (kb("MemTotal:")?, kb("MemAvailable:")?);
    Some(((total - available.min(total)) * 1024, total * 1024))
}

#[cfg(target_os = "linux")]
fn disks() -> Vec<(String, u64, u64)> {
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c"/".as_ptr(), &mut s) } != 0 {
        return Vec::new();
    }
    let (block, total) = (s.f_frsize as u64, s.f_blocks as u64);
    vec![("/".into(), (total - (s.f_bfree as u64).min(total)) * block, total * block)]
}

#[cfg(target_os = "linux")]
fn uptime() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .map_or(0, |v| v as u64)
}

/// Processor use needs two readings. This keeps the last one.
#[derive(Default)]
pub struct Sampler {
    last: Option<(Instant, u64, u64)>,
    cpu: Option<f64>,
}

impl Sampler {
    /// One `vitals` message. Blocks for a tenth of a second when there is
    /// no recent reading to measure processor use against.
    pub fn snapshot(&mut self) -> String {
        let stale = self.last.is_none_or(|(at, ..)| at.elapsed() > Duration::from_secs(5));
        if stale {
            self.last = cpu_times().map(|(busy, total)| (Instant::now(), busy, total));
            std::thread::sleep(Duration::from_millis(100));
        }
        // Readings closer together than this are too coarse to compare.
        let fresh = self.last.is_none_or(|(at, ..)| at.elapsed() >= Duration::from_millis(50));
        if let (true, Some((_, busy0, total0)), Some((busy, total))) = (fresh, self.last, cpu_times()) {
            if total > total0 {
                self.cpu = Some((busy.saturating_sub(busy0)) as f64 * 100.0 / (total - total0) as f64);
            }
            self.last = Some((Instant::now(), busy, total));
        }
        let mem = memory();
        let disks: Vec<_> = disks()
            .into_iter()
            .map(|(name, used, total)| serde_json::json!({ "name": name, "used": used, "total": total }))
            .collect();
        serde_json::json!({
            "t": "vitals",
            "cpu": self.cpu.map(|c| (c.clamp(0.0, 100.0) * 10.0).round() / 10.0),
            "cores": std::thread::available_parallelism().map_or(0, |n| n.get()),
            "mem": mem.map(|(used, total)| serde_json::json!({ "used": used, "total": total })),
            "disks": disks,
            "up": uptime(),
        })
        .to_string()
    }
}

/// Run the provider until the core goes away.
pub fn run(input: File, core: Arc<provider::ToCore>) {
    #[derive(Default)]
    struct Channels {
        all: Vec<u64>,
        /// Opened since the last reading: they get one straight away.
        new: Vec<u64>,
        gone: bool,
    }
    let state: Arc<(Mutex<Channels>, Condvar)> = Arc::default();
    let sampler = {
        let state = state.clone();
        std::thread::spawn(move || {
            let (lock, wake) = &*state;
            let mut sampler = Sampler::default();
            let mut ticked = Instant::now() - Duration::from_secs(1);
            loop {
                let mut channels = lock.lock().unwrap();
                while channels.all.is_empty() && !channels.gone {
                    channels = wake.wait(channels).unwrap();
                }
                let due = ticked + Duration::from_secs(1);
                while channels.new.is_empty() && !channels.gone && Instant::now() < due {
                    channels = wake.wait_timeout(channels, due - Instant::now()).unwrap().0;
                }
                if channels.gone {
                    return;
                }
                let to = if Instant::now() >= due {
                    ticked = Instant::now();
                    channels.new.clear();
                    channels.all.clone()
                } else {
                    std::mem::take(&mut channels.new)
                };
                drop(channels);
                if to.is_empty() {
                    continue;
                }
                let text = sampler.snapshot();
                for channel in to {
                    core.text(channel, &text);
                }
            }
        })
    };
    provider::serve(input, |event| {
        let (lock, wake) = &*state;
        let mut channels = lock.lock().unwrap();
        match event {
            provider::Event::Open(channel) => {
                channels.all.push(channel);
                channels.new.push(channel);
            }
            provider::Event::Close(channel) => {
                channels.all.retain(|c| *c != channel);
                channels.new.retain(|c| *c != channel);
            }
            provider::Event::Text(..) => return,
        }
        wake.notify_all();
    });
    state.0.lock().unwrap().gone = true;
    state.1.notify_all();
    let _ = sampler.join();
}
