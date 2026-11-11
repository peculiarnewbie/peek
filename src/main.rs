//! peek: an instant-connect remote desktop host for LAN and Tailscale.
//!
//! The host stays running and idle. Capture only happens while a browser
//! client is connected and its tab is visible, so connecting costs one
//! WebSocket handshake plus one frame.

mod engine;
mod platform;
mod server;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use clap::Parser;

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();

pub fn log_line(line: std::fmt::Arguments<'_>) {
    let line = format!("{} {line}", chrono::Local::now().format("%H:%M:%S"));
    eprintln!("{line}");
    if let Some(file) = LOG_FILE.get() {
        let _ = writeln!(file.lock().unwrap(), "{line}");
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log_line(format_args!($($arg)*)) };
}

#[derive(Parser)]
#[command(version, about = "Instant-connect remote desktop host for LAN and Tailscale.")]
struct Args {
    /// Address to listen on
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,
    #[arg(long, default_value_t = 9182)]
    port: u16,
    /// Maximum frames per second
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=240))]
    fps: u32,
    /// Show the screen but ignore mouse, keyboard and clipboard from clients
    #[arg(long)]
    view_only: bool,
    /// Do not synchronise the clipboard in either direction
    #[arg(long)]
    no_clipboard: bool,
    /// Also append log lines to this file
    #[arg(long, value_name = "FILE")]
    log: Option<PathBuf>,
    /// Need no access token; accept connections only over Tailscale instead
    #[arg(long)]
    no_token: bool,
    /// Log every input event and periodic throughput figures
    #[arg(short, long)]
    verbose: bool,
    /// Start in the background with no console window, then return
    #[cfg(windows)]
    #[arg(long)]
    detach: bool,
}

/// Where the access token lives: %APPDATA%\peek on Windows, otherwise
/// $XDG_CONFIG_HOME/peek or ~/.config/peek.
fn config_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("peek")
}

fn load_token() -> std::io::Result<String> {
    let path = config_dir().join("token");
    if let Ok(existing) = fs::read_to_string(&path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return Ok(existing.to_string());
        }
    }
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    fs::create_dir_all(config_dir())?;
    fs::write(&path, format!("{token}\n"))?;
    Ok(token)
}

/// Re-run this executable hidden and return, for use from a logon task.
#[cfg(windows)]
fn detach() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    Command::new(std::env::current_exe()?)
        .args(std::env::args_os().skip(1).filter(|a| a != "--detach"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
}

fn main() {
    let args = Args::parse();

    #[cfg(windows)]
    if args.detach {
        if let Err(e) = detach() {
            eprintln!("could not start in the background: {e}");
            std::process::exit(1);
        }
        return;
    }

    if let Some(path) = &args.log {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => drop(LOG_FILE.set(Mutex::new(file))),
            Err(e) => eprintln!("cannot write log file {}: {e}", path.display()),
        }
    }

    platform::init();
    if args.no_token && !(args.bind.is_unspecified() || args.bind.is_loopback() || server::tailnet(args.bind)) {
        eprintln!("--no-token accepts only Tailscale connections; --bind {} is not a Tailscale address", args.bind);
        std::process::exit(2);
    }
    let token = if args.no_token {
        None
    } else {
        match load_token() {
            Ok(token) => Some(token),
            Err(e) => {
                eprintln!("cannot store the access token in {}: {e}", config_dir().display());
                std::process::exit(1);
            }
        }
    };

    let (engine_tx, engine_rx) = std::sync::mpsc::channel();
    let shared = Arc::new(server::Shared {
        token,
        view_only: args.view_only,
        clipboard: !args.no_clipboard,
        verbose: args.verbose,
        engine: engine_tx,
        monitors: RwLock::new(Vec::new()),
        peers: Mutex::new(HashMap::new()),
        clip_last: Mutex::new(String::new()),
        next_id: AtomicU64::new(1),
    });

    {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || engine::run(engine_rx, shared, args.fps))
            .expect("spawn engine thread");
    }
    if shared.clipboard {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || server::watch_clipboard(shared))
            .expect("spawn clipboard thread");
    }

    let hosts: Vec<IpAddr> = if args.bind.is_unspecified() {
        let mut hosts = vec![IpAddr::from([127, 0, 0, 1])];
        if let Ok(addrs) = if_addrs::get_if_addrs() {
            hosts.extend(
                addrs.iter().map(|a| a.ip()).filter(|ip| {
                    ip.is_ipv4()
                        && !ip.is_loopback()
                        && if args.no_token { server::tailnet(*ip) } else { server::allowed(*ip) }
                }),
            );
        }
        hosts
    } else {
        vec![args.bind]
    };
    let fragment = shared.token.as_ref().map_or(String::new(), |t| format!("#{t}"));
    println!("peek is running. Open one of these on the client:");
    for host in &hosts {
        match host {
            IpAddr::V6(v6) => println!("  http://[{v6}]:{}/{fragment}", args.port),
            v4 => println!("  http://{v4}:{}/{fragment}", args.port),
        }
    }
    if shared.token.is_some() {
        println!("The part after # is the access token; the browser remembers it after the first visit.");
    } else {
        println!("No access token: any device on your tailnet can connect, and nothing else can.");
        if args.bind.is_unspecified() && hosts.iter().all(|h| h.is_loopback()) {
            println!("Tailscale is not up yet; connections are accepted as soon as it is.");
        }
    }
    if !args.view_only && !platform::is_elevated() {
        println!(
            "Not running as administrator: windows that are (Task Manager, installers) will ignore remote input."
        );
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("start async runtime");
    let addr = SocketAddr::new(args.bind, args.port);
    if let Err(e) = runtime.block_on(server::serve(shared, addr)) {
        log!("cannot listen on {addr}: {e}");
        std::process::exit(1);
    }
}
