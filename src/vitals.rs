//! The vitals provider (`peek --provider vitals`): what the machine is
//! doing, as numbers: processor, memory, storage. Needs no display, so it
//! runs beside the core, also in the Windows service's session 0.
//!
//! It says what to show and the client draws it: one line for the machine's
//! card, figures and meters for its page (see `view`). Nothing is read
//! while nobody asks.

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

/// What the machine is doing at one moment.
pub struct Reading {
    /// Processor use over the last second or so, in percent.
    cpu: Option<f64>,
    /// Bytes: (used, total).
    mem: Option<(u64, u64)>,
    /// Name, used bytes, total bytes.
    disks: Vec<(String, u64, u64)>,
    /// Seconds since the machine started.
    up: u64,
}

impl Sampler {
    /// Blocks for a tenth of a second when there is no recent reading to
    /// measure processor use against.
    pub fn reading(&mut self) -> Reading {
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
        Reading { cpu: self.cpu.map(|c| c.clamp(0.0, 100.0)), mem: memory(), disks: disks(), up: uptime() }
    }
}

/// Where a client shows a view.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    /// On the machine's card, in the list of machines: one line.
    Card,
    /// On the machine's own page: figures and meters.
    Page,
}

impl Place {
    fn name(self) -> &'static str {
        match self {
            Place::Card => "card",
            Place::Page => "page",
        }
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 { "– %".into() } else { format!("{} %", (part as f64 * 100.0 / whole as f64).round()) }
}

/// "12.3 of 64.0 GB", with no decimals from 100 GB up.
fn of_gb(used: u64, total: u64) -> String {
    const GB: f64 = (1u64 << 30) as f64;
    let gb = |bytes: u64| {
        let v = bytes as f64 / GB;
        if v < 100.0 { format!("{v:.1}") } else { format!("{v:.0}") }
    };
    format!("{} of {} GB", gb(used), gb(total))
}

fn duration(seconds: u64) -> String {
    let (m, h, d) = (seconds / 60, seconds / 3600, seconds / 86400);
    if d > 0 {
        format!("{d}d {}h", h % 24)
    } else if h > 0 {
        format!("{h}h {}m", m % 60)
    } else if m > 0 {
        format!("{m}m {}s", seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// A reading as something to show: the client draws it as described, and
/// knows nothing of processors or storage (docs/providers.md).
fn view(reading: &Reading, place: Place) -> String {
    let cpu = reading.cpu.map_or("– %".to_string(), |c| format!("{} %", c.round()));
    let mem = reading.mem.map_or("– %".to_string(), |(used, total)| percent(used, total));
    let (used, total) = reading.disks.iter().fold((0, 0), |(u, t), d| (u + d.1, t + d.2));
    let storage = percent(used, total);
    let view = match place {
        Place::Card => serde_json::json!({
            "items": [{
                "kind": "text",
                "tone": "hint",
                "text": format!("Processor {cpu} · Memory {mem} · Storage {storage}"),
            }],
        }),
        Place::Page => {
            let meter = |label: &str, used: u64, total: u64| {
                serde_json::json!({
                    "label": label,
                    "fraction": if total == 0 { 0.0 } else { used as f64 / total as f64 },
                    "text": of_gb(used, total),
                })
            };
            let meters: Vec<_> = reading
                .mem
                .iter()
                .map(|(used, total)| meter("Memory", *used, *total))
                .chain(reading.disks.iter().map(|(name, used, total)| meter(name, *used, *total)))
                .collect();
            serde_json::json!({
                "title": "This machine",
                "aside": format!("up {}", duration(reading.up)),
                "items": [
                    { "kind": "tiles", "tiles": [
                        { "label": "Processor", "value": cpu },
                        { "label": "Memory", "value": mem },
                        { "label": "Storage", "value": storage },
                    ] },
                    { "kind": "meters", "meters": meters },
                ],
            })
        }
    };
    serde_json::json!({ "t": "view", "place": place.name(), "view": view }).to_string()
}

/// How a channel that watches the page is opened again by the provider
/// after this one.
const WATCHING: &str = r#"{"watch":"page"}"#;

/// Run the provider until the core goes away.
///
/// A client asks for a view (`{"t":"view","place":"card"}`) and gets one.
/// Asked for the page's, it gets one a second from then on, for as long as
/// its channel is open. Nothing is read while nobody asks.
pub fn run(input: File, core: Arc<provider::ToCore>) {
    #[derive(Default)]
    struct Channels {
        /// Watching the page.
        watching: Vec<u64>,
        /// Asked since the last reading: they get a view straight away.
        asked: Vec<(u64, Place)>,
        gone: bool,
    }
    core.about(r#"{"places":["card","page"]}"#);
    let state: Arc<(Mutex<Channels>, Condvar)> = Arc::default();
    let sampler = {
        let state = state.clone();
        let core = core.clone();
        std::thread::spawn(move || {
            let (lock, wake) = &*state;
            let mut sampler = Sampler::default();
            let mut ticked = Instant::now() - Duration::from_secs(1);
            loop {
                let mut channels = lock.lock().unwrap();
                while channels.watching.is_empty() && channels.asked.is_empty() && !channels.gone {
                    channels = wake.wait(channels).unwrap();
                }
                let due = ticked + Duration::from_secs(1);
                while channels.asked.is_empty() && !channels.gone && Instant::now() < due {
                    channels = wake.wait_timeout(channels, due - Instant::now()).unwrap().0;
                }
                if channels.gone {
                    return;
                }
                let mut to = std::mem::take(&mut channels.asked);
                if Instant::now() >= due && !channels.watching.is_empty() {
                    ticked = Instant::now();
                    // Everyone watching, once each.
                    to.retain(|(_, place)| *place != Place::Page);
                    to.extend(channels.watching.iter().map(|c| (*c, Place::Page)));
                }
                drop(channels);
                if to.is_empty() {
                    continue;
                }
                let reading = sampler.reading();
                let (card, page) = (view(&reading, Place::Card), view(&reading, Place::Page));
                for (channel, place) in to {
                    core.text(channel, if place == Place::Card { &card } else { &page });
                }
            }
        })
    };
    provider::serve(input, |event| {
        let (lock, wake) = &*state;
        let mut channels = lock.lock().unwrap();
        let watch = |channels: &mut Channels, channel: u64| {
            if !channels.watching.contains(&channel) {
                channels.watching.push(channel);
            }
            channels.asked.push((channel, Place::Page));
        };
        match event {
            provider::Event::Open(channel, how) => {
                if how.contains(r#""watch""#) {
                    watch(&mut channels, channel);
                }
            }
            provider::Event::Close(channel) => {
                channels.watching.retain(|c| *c != channel);
                channels.asked.retain(|(c, _)| *c != channel);
            }
            provider::Event::Text(channel, text) => {
                let ask = serde_json::from_str::<serde_json::Value>(&text).unwrap_or_default();
                match (ask["t"].as_str(), ask["place"].as_str()) {
                    (Some("view"), Some("card")) => channels.asked.push((channel, Place::Card)),
                    (Some("view"), Some("page")) => {
                        watch(&mut channels, channel);
                        core.state(channel, WATCHING);
                    }
                    _ => {
                        core.text(channel, r#"{"t":"error","why":"vitals only shows: ask for a view of card or page"}"#);
                        return;
                    }
                }
            }
        }
        wake.notify_all();
    });
    state.0.lock().unwrap().gone = true;
    state.1.notify_all();
    let _ = sampler.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_is_described_for_the_card_and_for_the_page() {
        const GB: u64 = 1 << 30;
        let reading = Reading {
            cpu: Some(12.4),
            mem: Some((16 * GB, 64 * GB)),
            disks: vec![("C:".into(), 300 * GB, 500 * GB), ("D:".into(), 0, 0)],
            up: 3 * 3600 + 12 * 60,
        };
        let card: serde_json::Value = serde_json::from_str(&view(&reading, Place::Card)).unwrap();
        assert_eq!(card["place"], "card");
        assert_eq!(card["view"]["items"][0]["text"], "Processor 12 % · Memory 25 % · Storage 60 %");

        let page: serde_json::Value = serde_json::from_str(&view(&reading, Place::Page)).unwrap();
        assert_eq!(page["view"]["aside"], "up 3h 12m");
        assert_eq!(page["view"]["items"][0]["tiles"][1]["value"], "25 %");
        let meters = &page["view"]["items"][1]["meters"];
        assert_eq!(meters[0]["text"], "16.0 of 64.0 GB");
        assert_eq!(meters[1]["label"], "C:");
        assert_eq!(meters[1]["text"], "300 of 500 GB");
        assert_eq!(meters[1]["fraction"], 0.6);
        // An empty drive, and a machine that cannot say: no division by nothing.
        assert_eq!(meters[2]["fraction"], 0.0);
        let unknown = Reading { cpu: None, mem: None, disks: Vec::new(), up: 59 };
        let card: serde_json::Value = serde_json::from_str(&view(&unknown, Place::Card)).unwrap();
        assert_eq!(card["view"]["items"][0]["text"], "Processor – % · Memory – % · Storage – %");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(2 * 86400 + 5 * 3600), "2d 5h");
    }
}
