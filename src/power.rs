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
//! read. So the browser tries it once (sleep, then wake) and tells the
//! machine how that went (`tested`), which it keeps.
//!
//! Requests, one per channel, each with one answer:
//! - `{"t":"info"}`: what this machine can do; see `info`.
//! - `{"t":"sleep"}`: answers `{"t":"sleeping"}`, then sleeps.
//! - `{"t":"wake","mac":"..","addr":"192.168.1.20/24"}`: sends the packet
//!   onto that network, if this machine is on it. Answers `{"t":"wake-sent"}`.
//! - `{"t":"tested","ok":true}`: how the last try went. Answers with `info`.
//!
//! Anything refused answers `{"t":"error","why":".."}`.

use std::fs::File;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::provider::{self, Event};

/// Wake-on-LAN's customary port.
const WOL_PORT: u16 = 9;
/// Long enough for the answer to reach the browser before the network goes.
const SLEEP_AFTER: Duration = Duration::from_secs(1);
const TESTED_FILE: &str = "wake-tested.json";

/// Run the provider until the core goes away.
pub fn run(input: File, core: Arc<provider::ToCore>, view_only: bool) {
    provider::serve(input, |event| {
        if let Event::Text(channel, text) = event {
            let (answer, then_sleep) = answer(&text, view_only);
            core.text(channel, &answer.to_string());
            if then_sleep {
                std::thread::spawn(|| {
                    std::thread::sleep(SLEEP_AFTER);
                    crate::log!("power: going to sleep, as a client asked");
                    if let Err(e) = platform::sleep() {
                        crate::log!("power: could not sleep ({e})");
                    }
                });
            }
        }
    });
}

fn error(why: impl Into<String>) -> Value {
    json!({ "t": "error", "why": why.into() })
}

/// The answer to one request, and whether to sleep once it is sent.
fn answer(text: &str, view_only: bool) -> (Value, bool) {
    let Ok(req) = serde_json::from_str::<Value>(text) else {
        return (error("not JSON"), false);
    };
    match req["t"].as_str().unwrap_or("") {
        "info" => (info(), false),
        "sleep" if view_only => (error("this machine is view-only"), false),
        "sleep" => match platform::can_sleep() {
            Some(_) => (json!({ "t": "sleeping" }), true),
            None => (error("this machine cannot sleep"), false),
        },
        "wake" => {
            let (Some(mac), Some(addr)) = (req["mac"].as_str().and_then(parse_mac), req["addr"].as_str().and_then(parse_net))
            else {
                return (error("a wake needs mac and addr"), false);
            };
            (wake(mac, addr), false)
        }
        "tested" => {
            let ok = req["ok"].as_bool().unwrap_or(false);
            match target() {
                Ok(t) => {
                    let record = json!({ "mac": format_mac(&t.mac), "ok": ok, "at": now() });
                    let path = crate::config_dir().join(TESTED_FILE);
                    if let Err(e) = std::fs::write(&path, record.to_string()) {
                        return (error(format!("could not keep it: {e}")), false);
                    }
                    crate::log!("power: waking this machine {}", if ok { "worked" } else { "did not work" });
                    (info(), false)
                }
                Err(why) => (error(why), false),
            }
        }
        other => (error(format!("no request is called {other:?}")), false),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
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

/// Send the packet for `mac` onto the network `addr` is on. Only onto a
/// network this machine is on itself: nobody else's.
fn wake(mac: [u8; 6], addr: (Ipv4Addr, u8)) -> Value {
    let Some((local, broadcast)) = same_network(addr, &lan()) else {
        return error(format!("{} is not on the network of {}/{}", crate::pairing::machine_name(), addr.0, addr.1));
    };
    let sent = (|| -> std::io::Result<()> {
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
    })();
    match sent {
        Ok(()) => {
            crate::log!("power: sent a wake packet to {} on {broadcast}", format_mac(&mac));
            json!({ "t": "wake-sent", "from": crate::pairing::machine_name() })
        }
        Err(e) => error(format!("could not send the wake packet: {e}")),
    }
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
        let (answer, sleeps) = super::answer(r#"{"t":"sleep"}"#, true);
        assert_eq!(answer["t"], "error");
        assert!(!sleeps);
        let (answer, _) = super::answer(r#"{"t":"wake","mac":"0a:1b:2c:3d:4e:5f"}"#, false);
        assert_eq!(answer["t"], "error");
        let (answer, _) = super::answer("{", false);
        assert_eq!(answer["t"], "error");
    }
}
