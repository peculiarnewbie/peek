//! Linux: every process from /proc, one small file each.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd, AsRawFd};
use std::os::unix::fs::MetadataExt;
use std::sync::OnceLock;

use crate::{Meta, Policy, Raw, Snapshot};

/// Signals: ask a process to end, or end it.
pub const FORCE: bool = true;
/// Accounts below this are the system's (`UID_MIN` in login.defs).
const UID_MIN: u32 = 1000;

/// Clock ticks a second, which /proc counts processor time in.
pub fn cpu_units() -> f64 {
    static TICKS: OnceLock<f64> = OnceLock::new();
    *TICKS.get_or_init(|| match unsafe { libc::sysconf(libc::_SC_CLK_TCK) } { t if t > 0 => t as f64, _ => 100.0 })
}

fn page_size() -> u64 {
    static SIZE: OnceLock<u64> = OnceLock::new();
    *SIZE.get_or_init(|| match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } { s if s > 0 => s as u64, _ => 4096 })
}

pub fn cores() -> u32 {
    match unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) } { n if n > 0 => n as u32, _ => 1 }
}

/// Peek, which started this.
pub fn parent() -> u32 {
    unsafe { libc::getppid() as u32 }
}

struct Stat {
    comm: String,
    ppid: u32,
    cpu: u64,
    start: u64,
    rss: u64,
}

/// `/proc/<pid>/stat`. The name is in parentheses and may hold anything,
/// parentheses and spaces too, so the fields are counted from the last `)`.
fn parse_stat(s: &str) -> Option<Stat> {
    let (open, close) = (s.find('(')?, s.rfind(')')?);
    let f: Vec<&str> = s.get(close + 1..)?.split_whitespace().collect();
    // f[0] is field 3 (state); field k is f[k - 3].
    let n = |field: usize| f.get(field - 3).and_then(|v| v.parse::<u64>().ok());
    Some(Stat {
        comm: s.get(open + 1..close)?.to_string(),
        ppid: n(4)? as u32,
        cpu: n(14)? + n(15)?,
        start: n(22)?,
        rss: n(24)? * page_size(),
    })
}

fn stat(pid: u32, buf: &mut String) -> Option<Stat> {
    buf.clear();
    fs::File::open(format!("/proc/{pid}/stat")).and_then(|mut f| f.read_to_string(buf)).ok()?;
    parse_stat(buf)
}

pub fn snapshot() -> Snapshot {
    let mut procs = Vec::new();
    let mut buf = String::with_capacity(512);
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        // Gone since the folder was listed, as often happens.
        let Some(st) = stat(pid, &mut buf) else { continue };
        procs.push(Raw { pid, ppid: st.ppid, start: st.start, cpu: st.cpu, mem: st.rss, name: st.comm, session: 0 });
    }
    Snapshot { procs, times: cpu_times(), mem: memory() }
}

fn kernel(pid: u32, ppid: u32) -> bool {
    pid == 2 || ppid == 2
}

pub fn meta(raw: &Raw) -> Meta {
    let uid = fs::metadata(format!("/proc/{}", raw.pid)).map(|m| m.uid()).ok();
    let kernel = kernel(raw.pid, raw.ppid);
    let args = if kernel { Vec::new() } else { fs::read(format!("/proc/{}/cmdline", raw.pid)).unwrap_or_default() };
    let words: Vec<String> = args.split(|b| *b == 0).filter(|w| !w.is_empty()).map(|w| String::from_utf8_lossy(w).into_owned()).collect();
    // The kernel keeps 15 bytes of a name: the program's own name is longer.
    let mut name = raw.name.clone();
    if name.len() >= 15
        && let Some(first) = words.first()
    {
        let base = first.rsplit('/').next().unwrap_or(first);
        if base.starts_with(&name) { name = base.to_string(); }
    }
    let me = unsafe { libc::geteuid() };
    Meta {
        name,
        user: uid.map(user).unwrap_or_default(),
        cmd: words.join(" ").chars().take(400).collect(),
        sys: kernel || uid.is_some_and(|u| u < UID_MIN && u != me),
    }
}

fn user(uid: u32) -> String {
    static NAMES: OnceLock<HashMap<u32, String>> = OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
        passwd.lines().filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            Some((f.nth(1)?.parse().ok()?, name.to_string()))
        }).collect()
    });
    names.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
}

fn cpu_times() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let fields: Vec<u64> = stat.lines().next()?.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    // user nice system idle iowait irq softirq steal
    let total: u64 = fields.iter().take(8).sum();
    let idle = fields.get(3)? + fields.get(4).copied().unwrap_or(0);
    Some((total.saturating_sub(idle), total))
}

fn memory() -> (u64, u64) {
    let info = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let kb = |key: &str| info.lines().find_map(|l| l.strip_prefix(key)).and_then(|r| r.split_whitespace().next()).and_then(|v| v.parse::<u64>().ok());
    match (kb("MemTotal:"), kb("MemAvailable:")) {
        (Some(total), Some(available)) => ((total - available.min(total)) * 1024, total * 1024),
        _ => (0, 0),
    }
}

pub fn end(pid: u32, start: u64, force: bool, policy: &Policy) -> Result<(), String> {
    if pid <= 1 {
        return Err("the machine cannot run without it".into());
    }
    // Held by a pidfd, the process cannot end and its number go to another
    // between the check below and the signal. Kernels before 5.3 have none.
    let fd = match unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_int, 0) } {
        fd if fd >= 0 => Some(unsafe { OwnedFd::from_raw_fd(fd as i32) }),
        _ if errno() == libc::ESRCH => return Err("it has already ended".into()),
        _ => None,
    };
    let st = stat(pid, &mut String::new()).filter(|st| st.start == start).ok_or("it has already ended")?;
    if kernel(pid, st.ppid) {
        return Err("it is part of the kernel".into());
    }
    if !policy.any {
        let uid = fs::metadata(format!("/proc/{pid}")).map(|m| m.uid()).map_err(|_| "it has already ended")?;
        let me = unsafe { libc::geteuid() };
        // Peek as root ends people's processes, not the system's.
        let mine = if me == 0 { uid >= UID_MIN } else { uid == me };
        if !mine {
            return Err("it is the system's or another user's: allow that in the settings of processes".into());
        }
    }
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    let sent = match &fd {
        Some(fd) => unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), signal, std::ptr::null::<libc::siginfo_t>(), 0) },
        None => (unsafe { libc::kill(pid as libc::pid_t, signal) }) as libc::c_long,
    };
    if sent == 0 { return Ok(()); }
    Err(match errno() {
        libc::ESRCH => "it has already ended".into(),
        libc::EPERM => "not allowed to end it: it runs as another user".into(),
        e => std::io::Error::from_raw_os_error(e).to_string(),
    })
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_with_parentheses_and_spaces() {
        let line = "1234 (Web (Content) x) S 1000 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 30 0 98765 4096000 512 18446744073709551615";
        let st = parse_stat(line).unwrap();
        assert_eq!(st.comm, "Web (Content) x");
        assert_eq!((st.ppid, st.cpu, st.start, st.rss), (1000, 300, 98765, 512 * page_size()));
    }

    #[test]
    fn an_ended_child_is_not_ended_twice_and_a_wrong_start_is_refused() {
        let policy = Policy { any: false, keep: [0, 0] };
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let start = stat(pid, &mut String::new()).unwrap().start;
        assert_eq!(end(pid, start + 1, false, &policy).unwrap_err(), "it has already ended");
        end(pid, start, false, &policy).unwrap();
        child.wait().unwrap();
        assert!(end(pid, start, false, &policy).is_err());
    }
}
