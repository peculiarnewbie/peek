//! The power provider (`peek --provider power`): putting this machine to
//! sleep, and waking another one on the same network.
//!
//! A machine asleep runs nothing, peek included, so it cannot be asked to
//! wake. Another machine on its network can wake it, with a Wake-on-LAN
//! packet to its network card; a browser cannot send one. So each machine
//! says what it would take to wake it (`info`), the browser keeps that, and
//! when the machine is asleep asks another peek on the same network to send
//! the packet (`wake`).
//!
//! Whether waking works also depends on the firmware, which no program can
//! read. So it is tried once, as a test, and neither half of the test needs
//! the browser to stay open:
//!
//! - The machine that will send the packet is asked first, and counts the
//!   seconds itself (`wake` with `after`).
//! - The machine going to sleep sets a wake timer before it does, where
//!   Windows allows them, so a packet that wakes nothing costs two minutes
//!   and not a walk to the power button. When it is back it knows how the
//!   test went from when it woke: soon after the packets, or only at the
//!   timer. It keeps that itself.
//!
//! Requests, one per channel, each with one answer:
//! - `{"t":"info"}`: what this machine can do; see `info`.
//! - `{"t":"sleep"}`: answers `{"t":"sleeping"}`, then sleeps. With
//!   `"test":true` it is the test above.
//! - `{"t":"wake","mac":"..","addr":"192.168.1.20/24"}`: sends the packet
//!   onto that network, if this machine is on it. Answers `{"t":"wake-sent"}`.
//!   With `"after":15` it answers `{"t":"wake-armed"}` at once and sends the
//!   packet that many seconds later, and three more times after that.
//! - `{"t":"tested","ok":false}`: a wake the browser saw fail. Answers with
//!   `info`.
//!
//! Anything refused answers `{"t":"error","why":".."}`.

use std::fs::File;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::provider::{self, Event};

/// Wake-on-LAN's customary port.
const WOL_PORT: u16 = 9;
/// Long enough for the answer to reach the browser before the network goes.
const SLEEP_AFTER: Duration = Duration::from_secs(1);
const TESTED_FILE: &str = "wake-tested.json";

// The test. The other machine sends its packets 15, 25, 35 and 45 seconds
// after it is asked (the browser's choice of 15, and `WAKE_AGAIN`), in case
// this one is slow to fall asleep. Its own timer is well after the last.
/// When a machine under test wakes itself, if nothing else has.
const TEST_TIMER: Duration = Duration::from_secs(120);
/// Awake again within this of the test starting: a packet woke it. Later:
/// the timer did, or somebody at the machine.
const WOKEN_BY_PACKET_WITHIN: Duration = Duration::from_secs(75);
/// Woken with nobody at it, Windows goes back to sleep within minutes. Long
/// enough to connect and read how the test went.
const STAY_AWAKE: Duration = Duration::from_secs(600);
/// A delayed wake is sent this many more times, this far apart.
const WAKE_AGAIN: (u32, Duration) = (3, Duration::from_secs(10));
/// The longest a wake can be put off.
const MAX_WAKE_AFTER: u64 = 300;

/// What to do once a request's answer is sent.
#[derive(Debug, PartialEq)]
enum Then {
    Nothing,
    Sleep { test: bool },
    /// Send the packet for `mac` from `local` to `broadcast`, `after` from now.
    WakeLater { mac: [u8; 6], local: Ipv4Addr, broadcast: Ipv4Addr, after: Duration },
}

/// Run the provider until the core goes away.
pub fn run(input: File, core: Arc<provider::ToCore>, view_only: bool) {
    provider::serve(input, |event| {
        if let Event::Text(channel, text) = event {
            let (answer, then) = answer(&text, view_only);
            core.text(channel, &answer.to_string());
            match then {
                Then::Nothing => {}
                Then::Sleep { test } => drop(std::thread::spawn(move || sleep(test))),
                Then::WakeLater { mac, local, broadcast, after } => drop(std::thread::spawn(move || {
                    std::thread::sleep(after);
                    for again in 0..=WAKE_AGAIN.0 {
                        if again > 0 {
                            std::thread::sleep(WAKE_AGAIN.1);
                        }
                        match send_wake(mac, local, broadcast) {
                            Ok(()) => crate::log!("power: sent a wake packet to {} on {broadcast}", format_mac(&mac)),
                            Err(e) => crate::log!("power: could not send the wake packet ({e})"),
                        }
                    }
                })),
            }
        }
    });
}

/// Go to sleep. As a test: with a wake timer set first, and keeping how it
/// went once this machine is awake again.
fn sleep(test: bool) {
    let started = SystemTime::now();
    // The card now: just after waking, its link may not be up yet.
    let card = test.then(target).and_then(Result::ok).map(|t| t.mac);
    let timer = test.then(|| platform::wake_timer(started + TEST_TIMER)).flatten();
    if test {
        crate::log!(
            "power: testing sleep and wake, {}",
            if timer.is_some() { "with a wake timer in case no packet wakes this machine" } else { "with no wake timer" }
        );
    }
    std::thread::sleep(SLEEP_AFTER);
    crate::log!("power: going to sleep, as a client asked");
    // Whether or not the call returns before the machine is asleep, a
    // thread that sleeps a second at a time and finds much more has gone by
    // was stopped with the machine, and it has just woken.
    let asleep = std::thread::spawn(platform::sleep);
    if !test {
        if let Ok(Err(e)) = asleep.join() {
            crate::log!("power: could not sleep ({e})");
        }
        return;
    }
    let mut asleep = Some(asleep);
    let mut last = SystemTime::now();
    let woke = loop {
        std::thread::sleep(Duration::from_secs(1));
        let now = SystemTime::now();
        let since_start = now.duration_since(started).unwrap_or_default();
        if now.duration_since(last).unwrap_or_default() > Duration::from_secs(5) {
            break Some(since_start);
        }
        // Refused: there is nothing to wait for. Otherwise keep the timer
        // set until it is due: a machine still awake then is staying awake.
        if asleep.as_ref().is_some_and(|a| a.is_finished())
            && let Some(Ok(Err(e))) = asleep.take().map(|a| a.join())
        {
            crate::log!("power: could not sleep ({e})");
            break None;
        }
        if since_start > TEST_TIMER {
            break None;
        }
        last = now;
    };
    drop(timer);
    let Some(woke) = woke else {
        crate::log!("power: this machine did not go to sleep, so the test showed nothing");
        return;
    };
    let ok = by_packet(woke);
    crate::log!(
        "power: awake {} s after the test began: {}",
        woke.as_secs(),
        if ok { "a packet woke this machine" } else { "no packet woke it" }
    );
    match card.map(|mac| keep_tested(&mac, ok)) {
        Some(Ok(())) => {}
        Some(Err(why)) => crate::log!("power: could not keep how the test went ({why})"),
        None => crate::log!("power: there was no card to wake this machine through, so the test is not kept"),
    }
    platform::stay_awake(STAY_AWAKE);
}

/// Whether a machine awake again this long after its test began was woken
/// by a packet.
fn by_packet(woke: Duration) -> bool {
    woke < WOKEN_BY_PACKET_WITHIN
}

/// Keep how the last try at waking this machine went, with its card.
fn keep_tested(mac: &[u8; 6], ok: bool) -> Result<(), String> {
    let record = json!({ "mac": format_mac(mac), "ok": ok, "at": now() });
    std::fs::write(crate::config_dir().join(TESTED_FILE), record.to_string()).map_err(|e| e.to_string())
}

fn error(why: impl Into<String>) -> Value {
    json!({ "t": "error", "why": why.into() })
}

/// The answer to one request, and what to do once it is sent.
fn answer(text: &str, view_only: bool) -> (Value, Then) {
    let Ok(req) = serde_json::from_str::<Value>(text) else {
        return (error("not JSON"), Then::Nothing);
    };
    match req["t"].as_str().unwrap_or("") {
        "info" => (info(), Then::Nothing),
        "sleep" if view_only => (error("this machine is view-only"), Then::Nothing),
        "sleep" => match platform::can_sleep() {
            Some(_) => (json!({ "t": "sleeping" }), Then::Sleep { test: req["test"].as_bool().unwrap_or(false) }),
            None => (error("this machine cannot sleep"), Then::Nothing),
        },
        "wake" => {
            let (Some(mac), Some(addr)) = (req["mac"].as_str().and_then(parse_mac), req["addr"].as_str().and_then(parse_net))
            else {
                return (error("a wake needs mac and addr"), Then::Nothing);
            };
            wake(mac, addr, req["after"].as_u64(), &lan())
        }
        "tested" => {
            let ok = req["ok"].as_bool().unwrap_or(false);
            match target().and_then(|t| keep_tested(&t.mac, ok)) {
                Ok(()) => {
                    crate::log!("power: waking this machine {}", if ok { "worked" } else { "did not work" });
                    (info(), Then::Nothing)
                }
                Err(why) => (error(format!("could not keep it: {why}")), Then::Nothing),
            }
        }
        other => (error(format!("no request is called {other:?}")), Then::Nothing),
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The network card another machine would wake this one through.
pub struct Target {
    mac: [u8; 6],
    /// This machine's address on that network, with its prefix length.
    addr: (Ipv4Addr, u8),
    adapter: String,
    /// Windows lets the card wake the machine.
    armed: bool,
}

/// That card, or why there is none.
fn target() -> Result<Target, String> {
    platform::target()
}

/// What this machine can do, for the browser to keep:
/// - `sleep`: it can be put to sleep (`standby` says how: "s3", or "modern"
///   for Modern Standby, where a sleeping machine may not keep its network
///   card powered).
/// - `wake`: the card to wake it through, or null, with `why`.
/// - `lan`: the networks it is on, so the browser can tell which machines
///   can wake which.
/// - `tested`: how the last try at waking it went, for this card.
/// - `timer`: a test that fails to wake it ends by itself, on a wake timer.
fn info() -> Value {
    let (wake, why, tested) = match target() {
        Ok(t) => {
            let mac = format_mac(&t.mac);
            let tested = std::fs::read_to_string(crate::config_dir().join(TESTED_FILE))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .filter(|r| r["mac"].as_str() == Some(mac.as_str()))
                .map(|r| json!({ "ok": r["ok"], "at": r["at"] }));
            let why = (!t.armed).then(|| format!("Windows does not let {} wake this machine", t.adapter));
            let wake = json!({
                "mac": mac,
                "addr": format!("{}/{}", t.addr.0, t.addr.1),
                "adapter": t.adapter,
                "armed": t.armed,
            });
            (wake, why, tested)
        }
        Err(why) => (Value::Null, Some(why), None),
    };
    json!({
        "t": "power",
        "name": crate::pairing::machine_name(),
        "sleep": platform::can_sleep().is_some(),
        "standby": platform::can_sleep(),
        "wake": wake,
        "why": why,
        "lan": lan().iter().map(|(ip, prefix)| format!("{ip}/{prefix}")).collect::<Vec<_>>(),
        "tested": tested,
        "timer": platform::wake_timers_allowed(),
    })
}

/// This machine's IPv4 networks, other than loopback, link-local and
/// Tailscale's: (its address, prefix length).
fn lan() -> Vec<(Ipv4Addr, u8)> {
    let Ok(addrs) = if_addrs::get_if_addrs() else { return Vec::new() };
    addrs
        .iter()
        .filter_map(|a| match &a.addr {
            if_addrs::IfAddr::V4(v4) => Some((v4.ip, v4.prefixlen)),
            _ => None,
        })
        .filter(|(ip, prefix)| {
            !ip.is_loopback() && !ip.is_link_local() && !crate::server::tailnet((*ip).into()) && (8..=30).contains(prefix)
        })
        .collect()
}

fn mask(prefix: u8) -> u32 {
    if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32)) }
}

/// The local address on the same network as `addr`, and that network's
/// broadcast address.
fn same_network(addr: (Ipv4Addr, u8), lan: &[(Ipv4Addr, u8)]) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let m = mask(addr.1);
    let net = u32::from(addr.0) & m;
    lan.iter()
        .find(|(ip, prefix)| *prefix == addr.1 && u32::from(*ip) & m == net)
        .map(|(ip, _)| (*ip, Ipv4Addr::from(net | !m)))
}

/// Six bytes of 0xFF, then the card's address sixteen times.
fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

/// Send the packet for `mac` onto the network `addr` is on, now or `after`
/// seconds from now. Only onto a network this machine (on `lan`) is on
/// itself: nobody else's.
fn wake(mac: [u8; 6], addr: (Ipv4Addr, u8), after: Option<u64>, lan: &[(Ipv4Addr, u8)]) -> (Value, Then) {
    let from = crate::pairing::machine_name();
    let Some((local, broadcast)) = same_network(addr, lan) else {
        return (error(format!("{from} is not on the network of {}/{}", addr.0, addr.1)), Then::Nothing);
    };
    if let Some(after) = after {
        if after > MAX_WAKE_AFTER {
            return (error(format!("a wake can be put off {MAX_WAKE_AFTER} seconds at most")), Then::Nothing);
        }
        crate::log!("power: will send a wake packet to {} in {after} s", format_mac(&mac));
        let then = Then::WakeLater { mac, local, broadcast, after: Duration::from_secs(after) };
        return (json!({ "t": "wake-armed", "from": from, "after": after }), then);
    }
    match send_wake(mac, local, broadcast) {
        Ok(()) => {
            crate::log!("power: sent a wake packet to {} on {broadcast}", format_mac(&mac));
            (json!({ "t": "wake-sent", "from": from }), Then::Nothing)
        }
        Err(e) => (error(format!("could not send the wake packet: {e}")), Then::Nothing),
    }
}

fn send_wake(mac: [u8; 6], local: Ipv4Addr, broadcast: Ipv4Addr) -> std::io::Result<()> {
    let socket = UdpSocket::bind(SocketAddrV4::new(local, 0))?;
    socket.set_broadcast(true)?;
    let packet = magic_packet(mac);
    // A few times over: it is one datagram, with nothing to say it arrived.
    for _ in 0..3 {
        socket.send_to(&packet, SocketAddrV4::new(broadcast, WOL_PORT))?;
        let _ = socket.send_to(&packet, SocketAddrV4::new(Ipv4Addr::BROADCAST, WOL_PORT));
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let bytes: Vec<u8> = text.split([':', '-']).map(|b| u8::from_str_radix(b, 16)).collect::<Result<_, _>>().ok()?;
    bytes.try_into().ok()
}

fn format_mac(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

fn parse_net(text: &str) -> Option<(Ipv4Addr, u8)> {
    let (ip, prefix) = text.split_once('/')?;
    let prefix: u8 = prefix.parse().ok()?;
    (8..=30).contains(&prefix).then_some(())?;
    Some((ip.parse().ok()?, prefix))
}

#[cfg(windows)]
mod platform {
    use super::Target;
    use std::net::Ipv4Addr;

    /// Whether this machine can sleep, and how: "s3", or "modern".
    pub fn can_sleep() -> Option<&'static str> {
        use windows::Win32::System::Power::{CallNtPowerInformation, SYSTEM_POWER_CAPABILITIES, SystemPowerCapabilities};
        let mut caps = SYSTEM_POWER_CAPABILITIES::default();
        let status = unsafe {
            CallNtPowerInformation(
                SystemPowerCapabilities,
                None,
                0,
                Some(&mut caps as *mut _ as *mut _),
                size_of::<SYSTEM_POWER_CAPABILITIES>() as u32,
            )
        };
        if status.is_err() {
            return None;
        }
        if caps.AoAc {
            Some("modern")
        } else if caps.SystemS3 || caps.SystemS1 || caps.SystemS2 {
            Some("s3")
        } else {
            None
        }
    }

    pub fn sleep() -> Result<(), String> {
        use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
        use windows::Win32::Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
            TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        };
        use windows::Win32::System::Power::SetSuspendState;
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            // Held by users and SYSTEM alike, but off until asked for.
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)
                .map_err(|e| e.to_string())?;
            let mut luid = LUID::default();
            let looked = LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid);
            if looked.is_ok() {
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
                };
                let _ = AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None);
            }
            let _ = CloseHandle(token);
            if SetSuspendState(false, false, false) {
                Ok(())
            } else {
                Err(windows::core::Error::from_thread().to_string())
            }
        }
    }

    /// Whether a timer may wake this machine from sleep: the power plan's
    /// "Allow wake timers", for the power it is on now. "Important wake
    /// timers only" leaves ours out.
    pub fn wake_timers_allowed() -> bool {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::System::Power::{
            GetSystemPowerStatus, PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, SYSTEM_POWER_STATUS,
        };
        use windows::core::GUID;
        const SLEEP: GUID = GUID::from_u128(0x238c9fa8_0aad_41ed_83f4_97be242c8f20);
        const ALLOW_WAKE_TIMERS: GUID = GUID::from_u128(0xbd3b718a_0680_4d9d_8ab2_e1d2b4ac806d);
        unsafe {
            let mut scheme: *mut GUID = std::ptr::null_mut();
            if PowerGetActiveScheme(None, &mut scheme).is_err() || scheme.is_null() {
                return false;
            }
            let mut status = SYSTEM_POWER_STATUS::default();
            let on_battery = GetSystemPowerStatus(&mut status).is_ok() && status.ACLineStatus == 0;
            let mut value = 0u32;
            // Both return 0 for success; the bindings type the two differently.
            let failed = if on_battery {
                PowerReadDCValueIndex(None, Some(scheme), Some(&SLEEP), Some(&ALLOW_WAKE_TIMERS), &mut value)
            } else {
                PowerReadACValueIndex(None, Some(scheme), Some(&SLEEP), Some(&ALLOW_WAKE_TIMERS), &mut value).0
            };
            let _ = LocalFree(Some(HLOCAL(scheme as *mut _)));
            failed == 0 && value == 1
        }
    }

    /// A timer that wakes this machine from sleep, until it is dropped.
    pub struct WakeTimer(windows::Win32::Foundation::HANDLE);

    // The handle is only ever cancelled and closed, from whichever thread.
    unsafe impl Send for WakeTimer {}

    impl Drop for WakeTimer {
        fn drop(&mut self) {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::Threading::CancelWaitableTimer;
            unsafe {
                let _ = CancelWaitableTimer(self.0);
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Have this machine wake itself at `at`, if Windows allows that.
    pub fn wake_timer(at: std::time::SystemTime) -> Option<WakeTimer> {
        use windows::Win32::System::Threading::{CreateWaitableTimerW, SetWaitableTimer};
        use windows::core::PCWSTR;
        if !wake_timers_allowed() {
            return None;
        }
        // An absolute time, as a FILETIME: 100 ns steps since 1601. (A
        // relative one may not count the time spent asleep.)
        let unix = at.duration_since(std::time::UNIX_EPOCH).ok()?;
        let due = (unix.as_secs() as i64 + 11_644_473_600) * 10_000_000 + i64::from(unix.subsec_nanos() / 100);
        unsafe {
            let timer = WakeTimer(CreateWaitableTimerW(None, true, PCWSTR::null()).ok()?);
            match SetWaitableTimer(timer.0, &due, 0, None, None, true) {
                Ok(()) => Some(timer),
                Err(e) => {
                    crate::log!("power: could not set a wake timer ({e})");
                    None
                }
            }
        }
    }

    /// Keep this machine from going back to sleep by itself for a while.
    pub fn stay_awake(how_long: std::time::Duration) {
        use windows::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
        // The request lasts as long as the thread that made it.
        std::thread::spawn(move || unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
            std::thread::sleep(how_long);
            SetThreadExecutionState(ES_CONTINUOUS);
        });
    }

    struct Adapter {
        description: String,
        mac: [u8; 6],
        ethernet: bool,
        wifi: bool,
        addrs: Vec<(Ipv4Addr, u8)>,
        gateway: bool,
    }

    fn adapters() -> Vec<Adapter> {
        use windows::Win32::NetworkManagement::IpHelper::{
            GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
        };
        use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
        use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
        const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
        const IF_TYPE_IEEE80211: u32 = 71;

        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 16 * 1024u32;
        let mut buffer: Vec<u64>;
        loop {
            // u64s, for the alignment the structures need.
            buffer = vec![0u64; (size as usize).div_ceil(8)];
            let first = buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            let status = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(first), &mut size) };
            match status {
                0 => break,
                111 => continue, // ERROR_BUFFER_OVERFLOW: `size` now says how much
                _ => return Vec::new(),
            }
        }
        let mut list = Vec::new();
        let mut next = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while let Some(a) = unsafe { next.as_ref() } {
            next = a.Next;
            if a.OperStatus != IfOperStatusUp || a.PhysicalAddressLength != 6 {
                continue;
            }
            let mut addrs = Vec::new();
            let mut unicast = a.FirstUnicastAddress;
            while let Some(u) = unsafe { unicast.as_ref() } {
                unicast = u.Next;
                if let Some(sa) = unsafe { (u.Address.lpSockaddr as *const SOCKADDR_IN).as_ref() }
                    && sa.sin_family == AF_INET
                {
                    let ip = Ipv4Addr::from(u32::from_be(unsafe { sa.sin_addr.S_un.S_addr }));
                    addrs.push((ip, u.OnLinkPrefixLength));
                }
            }
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&a.PhysicalAddress[..6]);
            list.push(Adapter {
                description: unsafe { a.Description.to_string() }.unwrap_or_default(),
                mac,
                ethernet: a.IfType == IF_TYPE_ETHERNET_CSMACD,
                wifi: a.IfType == IF_TYPE_IEEE80211,
                addrs,
                gateway: !a.FirstGatewayAddress.is_null(),
            });
        }
        list
    }

    /// Software switches and tunnels that look like Ethernet: a packet to
    /// their address wakes nothing.
    fn virtual_card(description: &str) -> bool {
        let d = description.to_ascii_lowercase();
        ["hyper-v", "virtual", "vmware", "virtualbox", "tap-", "wintun", "tailscale", "wireguard", "vpn", "loopback"]
            .iter()
            .any(|v| d.contains(v))
    }

    /// The devices Windows lets wake the machine, by name.
    fn wake_armed() -> Vec<String> {
        let Ok(out) = std::process::Command::new("powercfg").args(["/devicequery", "wake_armed"]).output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
    }

    pub fn target() -> Result<Target, String> {
        let all = adapters();
        let routed = |a: &&Adapter| a.gateway && !a.addrs.is_empty() && !virtual_card(&a.description);
        let Some(card) = all.iter().filter(routed).find(|a| a.ethernet) else {
            return Err(if all.iter().filter(routed).any(|a| a.wifi) {
                "It is on Wi-Fi, which can rarely wake a machine. A network cable can.".into()
            } else {
                "It has no wired network connection to wake it through.".into()
            });
        };
        let armed = wake_armed().iter().any(|d| d.eq_ignore_ascii_case(&card.description));
        Ok(Target { mac: card.mac, addr: card.addrs[0], adapter: card.description.clone(), armed })
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Target;

    pub fn can_sleep() -> Option<&'static str> {
        None
    }

    pub fn sleep() -> Result<(), String> {
        Err("not on this system yet".into())
    }

    pub fn target() -> Result<Target, String> {
        Err("Waking is only set up on Windows so far.".into())
    }

    pub fn wake_timers_allowed() -> bool {
        false
    }

    pub struct WakeTimer;

    pub fn wake_timer(_at: std::time::SystemTime) -> Option<WakeTimer> {
        None
    }

    pub fn stay_awake(_how_long: std::time::Duration) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_magic_packet_is_ff_then_the_address_sixteen_times() {
        let mac = parse_mac("0a:1b:2c:3d:4e:5f").unwrap();
        let p = magic_packet(mac);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert!(p[6..].chunks(6).all(|c| c == mac));
        assert_eq!(format_mac(&mac), "0a:1b:2c:3d:4e:5f");
        assert_eq!(parse_mac("0A-1B-2C-3D-4E-5F"), Some(mac));
        assert_eq!(parse_mac("0a:1b:2c:3d:4e"), None);
        assert_eq!(parse_mac("0a:1b:2c:3d:4e:zz"), None);
    }

    #[test]
    fn a_wake_goes_only_onto_a_network_this_machine_is_on() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let lan = [(ip("10.0.0.5"), 8), (ip("192.168.1.23"), 24)];
        assert_eq!(same_network(parse_net("192.168.1.40/24").unwrap(), &lan), Some((ip("192.168.1.23"), ip("192.168.1.255"))));
        assert_eq!(same_network(parse_net("10.200.3.4/8").unwrap(), &lan), Some((ip("10.0.0.5"), ip("10.255.255.255"))));
        // Another network, and the same addresses with another prefix.
        assert_eq!(same_network(parse_net("192.168.2.40/24").unwrap(), &lan), None);
        assert_eq!(same_network(parse_net("192.168.1.40/16").unwrap(), &lan), None);
        assert_eq!(parse_net("192.168.1.40/32"), None);
        assert_eq!(parse_net("192.168.1.40"), None);
    }

    #[test]
    fn a_view_only_machine_will_not_sleep() {
        for request in [r#"{"t":"sleep"}"#, r#"{"t":"sleep","test":true}"#] {
            let (answer, then) = super::answer(request, true);
            assert_eq!(answer["t"], "error");
            assert_eq!(then, Then::Nothing);
        }
        let (answer, _) = super::answer(r#"{"t":"wake","mac":"0a:1b:2c:3d:4e:5f"}"#, false);
        assert_eq!(answer["t"], "error");
        let (answer, _) = super::answer("{", false);
        assert_eq!(answer["t"], "error");
    }

    #[test]
    fn a_wake_put_off_is_answered_at_once_and_sent_later() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let lan = [(ip("192.168.1.23"), 24)];
        let mac = parse_mac("0a:1b:2c:3d:4e:5f").unwrap();
        let (answer, then) = wake(mac, (ip("192.168.1.40"), 24), Some(15), &lan);
        assert_eq!(answer["t"], "wake-armed");
        assert_eq!(answer["after"], 15);
        assert_eq!(
            then,
            Then::WakeLater { mac, local: ip("192.168.1.23"), broadcast: ip("192.168.1.255"), after: Duration::from_secs(15) }
        );
        // Not onto another network, and not put off for long.
        let (answer, then) = wake(mac, (ip("192.168.2.40"), 24), Some(15), &lan);
        assert_eq!((answer["t"].as_str(), then), (Some("error"), Then::Nothing));
        let (answer, then) = wake(mac, (ip("192.168.1.40"), 24), Some(MAX_WAKE_AFTER + 1), &lan);
        assert_eq!((answer["t"].as_str(), then), (Some("error"), Then::Nothing));
    }

    /// Where the power plan allows wake timers, one can be set (and is
    /// cancelled when dropped, long before it is due).
    #[cfg(windows)]
    #[test]
    fn a_wake_timer_can_be_set_where_they_are_allowed() {
        let timer = platform::wake_timer(SystemTime::now() + Duration::from_secs(3600));
        assert_eq!(timer.is_some(), platform::wake_timers_allowed());
    }

    #[test]
    fn a_test_is_told_by_when_the_machine_woke() {
        // The last packet goes out 45 s after the test began, and the timer
        // is at 120 s: Windows may wake the machine half a minute early.
        assert!(by_packet(Duration::from_secs(20)));
        assert!(by_packet(Duration::from_secs(60)));
        assert!(!by_packet(Duration::from_secs(90)));
        assert!(!by_packet(Duration::from_secs(125)));
        assert!(WOKEN_BY_PACKET_WITHIN < TEST_TIMER - Duration::from_secs(30));
    }
}
