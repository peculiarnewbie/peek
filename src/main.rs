//! peek: an instant-connect remote desktop host for LAN and Tailscale.
//!
//! The host stays running and idle. Capture only happens while a browser
//! client is connected and its tab is visible, so connecting costs one
//! WebSocket handshake plus one frame.

mod engine;
mod install;
mod pairing;
mod platform;
mod provider;
mod screen;
mod server;
mod webp;

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, OnceLock};

use clap::{Parser, Subcommand};

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
/// Where `--log` writes, made absolute so `peek logs` can say where to look.
pub static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// The latest log lines, numbered, for `peek logs`: a host started as a
/// service has no console to read them from.
static RECENT: Mutex<(u64, VecDeque<String>)> = Mutex::new((0, VecDeque::new()));
const RECENT_LINES: usize = 1000;

/// In a provider process: the core, which does the logging for both.
static LOG_TO_CORE: OnceLock<Arc<provider::ToCore>> = OnceLock::new();

pub fn log_line(line: std::fmt::Arguments<'_>) {
    if let Some(core) = LOG_TO_CORE.get() {
        core.log(&line.to_string());
        return;
    }
    let line = format!("{} {line}", chrono::Local::now().format("%H:%M:%S"));
    eprintln!("{line}");
    if let Some(file) = LOG_FILE.get() {
        let _ = writeln!(file.lock().unwrap(), "{line}");
    }
    let mut recent = RECENT.lock().unwrap();
    recent.0 += 1;
    if recent.1.len() == RECENT_LINES {
        recent.1.pop_front();
    }
    recent.1.push_back(line);
}

/// Log lines numbered above `after`, at most the last `limit`, with the
/// number of the newest line (to ask for what follows it next time).
pub fn recent_log(after: u64, limit: usize) -> (u64, Vec<String>) {
    let recent = RECENT.lock().unwrap();
    let (last, lines) = &*recent;
    let new = last.saturating_sub(after).min(lines.len() as u64) as usize;
    (*last, lines.iter().skip(lines.len() - new.min(limit)).cloned().collect())
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log_line(format_args!($($arg)*)) };
}

#[derive(Parser)]
#[command(version, about = "Instant-connect remote desktop host for LAN and Tailscale.")]
struct Args {
    /// Address to listen on
    #[arg(long, default_value = "0.0.0.0", global = true)]
    bind: IpAddr,
    #[arg(long, default_value_t = 9182, global = true)]
    port: u16,
    /// Origin of the hosted client, whose pages may connect with a paired
    /// token ("none" to refuse them)
    #[arg(long, value_name = "ORIGIN", default_value = "https://peek.peculiarnewbie.com")]
    web: String,
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
    /// Keep the GPU device open while nobody is watching, so connecting from
    /// idle is faster. Costs some memory, and on a laptop can keep a discrete
    /// GPU from sleeping
    #[cfg(windows)]
    #[arg(long)]
    keep_gpu: bool,
    /// Run as a Windows service, which starts peek as SYSTEM in the session
    /// at the console, so it also shows the login and lock screens and UAC
    /// prompts. Set it up with `peek install --service`
    #[cfg(windows)]
    #[arg(long)]
    service: bool,
    /// Be the screen for a core (the Windows service starts one in the
    /// session at the console), speaking to it on stdin and stdout
    #[arg(long, hide = true, value_name = "KIND")]
    provider: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show a one-time code that pairs a browser with this machine. If peek
    /// is not running yet, this starts it here and keeps it running
    Pair,
    /// List the browsers paired with this machine
    Clients,
    /// Unpair a browser, by the id `peek clients` shows
    Unpair { id: String },
    /// Show what the running peek has logged recently
    Logs {
        /// Keep showing new lines as they come
        #[arg(short, long)]
        follow: bool,
        /// How many recent lines to show first
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
    },
    /// Start peek in the background at logon: copies it to a fixed place and
    /// registers a logon task and firewall rule (Windows, asks for
    /// administrator) or a systemd user service (Linux). Flags after it are
    /// for that peek, like `peek install --no-token`; on Windows, `--service`
    /// installs a service instead, which also works at the login screen.
    /// Run again after an update
    Install {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "PEEK FLAGS")]
        args: Vec<String>,
    },
    /// Undo `peek install`
    Uninstall,
}

/// Where the access token and pairing data live: %APPDATA%\peek on Windows
/// (%ProgramData%\peek for the service), otherwise $XDG_CONFIG_HOME/peek or
/// ~/.config/peek.
fn config_dir() -> PathBuf {
    #[cfg(windows)]
    if platform::is_system() {
        return machine_config_dir();
    }
    user_config_dir()
}

/// %ProgramData%\peek: the service's settings, as it runs as no user.
#[cfg(windows)]
fn machine_config_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from)
        .join("peek")
}

fn user_config_dir() -> PathBuf {
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

/// The hosted client's origin from `--web`, or `None` for "none".
fn web_origin(arg: &str) -> Result<Option<String>, String> {
    let arg = arg.trim().trim_end_matches('/');
    if arg.eq_ignore_ascii_case("none") || arg.is_empty() {
        return Ok(None);
    }
    let rest = arg
        .strip_prefix("https://")
        .or_else(|| arg.strip_prefix("http://").filter(|r| r.starts_with("localhost") || r.starts_with("127.0.0.1")))
        .ok_or("--web must be an https:// origin (or http://localhost for development)")?;
    if rest.is_empty() || rest.contains('/') {
        return Err("--web takes an origin only, like https://peek.example.com".into());
    }
    Ok(Some(arg.to_ascii_lowercase()))
}

/// One request to the peek running on this machine, as its owner: the
/// secret in its config folder proves it. Returns the status and body.
fn local_request(args: &Args, method: &str, path: &str, body: &str) -> Result<(u16, String), String> {
    // The service keeps its secret machine-wide, readable by administrators
    // and whoever installed it; any other peek keeps it in the user's folder.
    #[cfg(windows)]
    let folders = [machine_config_dir(), user_config_dir()];
    #[cfg(not(windows))]
    let folders = [user_config_dir()];
    let secrets: Vec<String> = folders
        .iter()
        .filter_map(|dir| fs::read_to_string(dir.join("local-secret")).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    // The service's folder exists, but this account cannot read it.
    #[cfg(windows)]
    let shut_out = machine_config_dir().is_dir() && fs::read_to_string(machine_config_dir().join("local-secret")).is_err();
    #[cfg(not(windows))]
    let shut_out = false;
    let refused = if shut_out {
        "the running peek refused. If it is the peek service, this account may not use it: \
         try again from an administrator terminal"
    } else {
        "the running peek refused: it was started by another user or with another config folder"
    };
    if secrets.is_empty() {
        return Err(if shut_out { refused } else { "peek has not run on this machine yet (as this user)" }.into());
    }
    for (i, secret) in secrets.iter().enumerate() {
        match local_request_with(args, method, path, body, secret)? {
            (403, _) if i + 1 < secrets.len() => continue,
            (403, _) => return Err(refused.into()),
            reply => return Ok(reply),
        }
    }
    unreachable!("the last secret returns either way")
}

/// Where a peek started with these flags listens, as seen from this machine.
fn local_addr(args: &Args) -> SocketAddr {
    let ip = if args.bind.is_unspecified() { IpAddr::from([127, 0, 0, 1]) } else { args.bind };
    SocketAddr::new(ip, args.port)
}

/// Whether something already listens where this peek would.
fn host_running(args: &Args) -> bool {
    std::net::TcpStream::connect_timeout(&local_addr(args), std::time::Duration::from_secs(1)).is_ok()
}

fn local_request_with(args: &Args, method: &str, path: &str, body: &str, secret: &str) -> Result<(u16, String), String> {
    use std::io::Read;
    let addr = local_addr(args);
    let mut stream = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3))
        .map_err(|e| format!("peek is not running on port {} ({e}). Start it first.", args.port))?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {secret}\r\nContent-Type: text/plain\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|e| e.to_string())?;
    let status = response.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = response.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_string();
    Ok((status, body))
}

/// What Tailscale Serve does with HTTPS on this machine (port 443).
enum Serve {
    /// It forwards to peek.
    Peek,
    /// It forwards somewhere else, which we leave alone.
    Other(String),
    Unset,
}

fn serve_state(tailscale: &str, host: &str, target: &str) -> Result<Serve, String> {
    let out = std::process::Command::new(tailscale)
        .args(["serve", "status", "--json"])
        .output()
        .map_err(|e| format!("could not run tailscale: {e}"))?;
    if !out.status.success() {
        return Err(format!("`tailscale serve status` failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let web = &status["Web"][format!("{host}:443")]["Handlers"];
    if let Some(handlers) = web.as_object() {
        let proxy = |h: &serde_json::Value| h["Proxy"].as_str().unwrap_or("").trim_end_matches('/').to_string();
        return Ok(match handlers.get("/").map(proxy) {
            Some(p) if p == target || p == target.replace("127.0.0.1", "localhost") => Serve::Peek,
            Some(p) if !p.is_empty() => Serve::Other(p),
            _ => Serve::Other(format!("{} path(s)", handlers.len())),
        });
    }
    if !status["TCP"]["443"].is_null() {
        return Ok(Serve::Other("a TCP forward".into()));
    }
    Ok(Serve::Unset)
}

/// Make Tailscale Serve pass `https://<this machine>.ts.net` to peek, which
/// is how the hosted client reaches it. Tailscale itself asks for whatever
/// is missing (Serve and HTTPS on the tailnet, or permission on Linux), so
/// its output goes straight to the terminal.
fn ensure_serve(args: &Args, tailscale: &str, host: &str) -> Result<(), String> {
    let ip = if args.bind.is_unspecified() || args.bind.is_loopback() { IpAddr::from([127, 0, 0, 1]) } else { args.bind };
    let target = format!("http://{}", SocketAddr::new(ip, args.port));
    match serve_state(tailscale, host, &target)? {
        Serve::Peek => return Ok(()),
        Serve::Other(what) => {
            return Err(format!(
                "Tailscale Serve already uses HTTPS on this machine (for {what}), so peek leaves it alone. \
                 To give it to peek: tailscale serve --bg {target}"
            ))
        }
        Serve::Unset => {}
    }
    println!("Setting up Tailscale Serve, so browsers can reach peek at https://{host} …");
    let ok = std::process::Command::new(tailscale)
        .args(["serve", "--bg", &target])
        .status()
        .map_err(|e| format!("could not run tailscale: {e}"))?
        .success();
    if !ok {
        let mut why = "`tailscale serve` failed (see above).".to_string();
        if cfg!(target_os = "linux") {
            why += "\nIf it was refused, allow this user to change Tailscale once, then run `peek pair` again:\n  \
                    sudo tailscale set --operator=$USER";
        }
        return Err(why);
    }
    println!();
    Ok(())
}

/// `peek pair`. Returns true when no peek is running here yet, so this
/// process should start the host and show the code itself.
fn pair(args: &Args) -> Result<bool, String> {
    match pairing::tailscale_cmd().zip(pairing::tailscale_name()) {
        Some((tailscale, host)) => ensure_serve(args, tailscale, &host)?,
        None => {
            return Err("Tailscale is not running here, and the hosted client reaches machines through it. \
                        Start Tailscale and try again."
                .into())
        }
    }
    if !host_running(args) {
        if web_origin(&args.web)?.is_none() {
            return Err("with --web none there is no hosted client to pair".into());
        }
        #[cfg(windows)]
        if args.detach || args.service {
            return Err("`peek pair` shows its code in this terminal, so it cannot start peek with --detach or --service. \
                        Start peek that way first, then run `peek pair`."
                .into());
        }
        return Ok(true);
    }
    let (_, body) = local_request(args, "POST", "/api/pair-code", "")?;
    let offer: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    print_offer(&offer)?;
    Ok(false)
}

/// Print a pairing code and its link and QR code, as the host made them.
fn print_offer(offer: &serde_json::Value) -> Result<(), String> {
    if let Some(e) = offer["error"].as_str() {
        return Err(e.to_string());
    }
    let (code, name) = (offer["code"].as_str().unwrap_or(""), offer["name"].as_str().unwrap_or("this machine"));
    let Some(web) = offer["web"].as_str() else {
        return Err("this peek was started with --web none, so there is no hosted client to pair".into());
    };
    let Some(link) = offer["link"].as_str() else {
        return Err(format!("peek could not find {name}'s Tailscale name. Is Tailscale up?"));
    };
    println!("Pair a browser with {name}:\n\n  {link}\n");
    if let Ok(qr) = qrcode::QrCode::new(link.as_bytes()) {
        let art = qr
            .render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Light)
            .light_color(qrcode::render::unicode::Dense1x2::Dark)
            .build();
        println!("{art}\n");
    }
    println!("Open the link on that device, or scan it. It works once, within 5 minutes.");
    println!("Or on {web}, choose Add a machine: {} with code {code}.", offer["host"].as_str().unwrap_or(""));
    Ok(())
}

fn run_command(args: &Args, command: &Command) -> Result<(), String> {
    match command {
        Command::Pair => unreachable!("main runs `peek pair` itself"),
        Command::Clients => {
            let (_, body) = local_request(args, "GET", "/api/clients", "")?;
            let clients: Vec<serde_json::Value> = serde_json::from_str(&body).map_err(|e| e.to_string())?;
            if clients.is_empty() {
                println!("No browsers are paired. `peek pair` pairs one.");
            }
            let when = |v: &serde_json::Value| {
                chrono::DateTime::from_timestamp(v.as_i64().unwrap_or(0), 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default()
            };
            for c in &clients {
                println!(
                    "{}  {:<24}  paired {}  last seen {}",
                    c["id"].as_str().unwrap_or(""),
                    c["label"].as_str().unwrap_or(""),
                    when(&c["created"]),
                    when(&c["last_seen"])
                );
            }
            Ok(())
        }
        Command::Logs { follow, lines } => {
            let mut after = 0;
            let mut limit = *lines;
            loop {
                let (status, body) = match local_request(args, "GET", &format!("/api/logs?after={after}&limit={limit}"), "") {
                    Ok(reply) => reply,
                    Err(e) if *follow && after > 0 => return Err(format!("peek stopped ({e})")),
                    Err(e) => return Err(e),
                };
                if status == 404 {
                    return Err("the running peek is too old for `peek logs`. Update and restart it.".into());
                }
                let reply: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
                for line in reply["lines"].as_array().into_iter().flatten() {
                    println!("{}", line.as_str().unwrap_or(""));
                }
                if after == 0 && !*follow {
                    if let Some(file) = reply["file"].as_str() {
                        eprintln!("(the full log is in {file})");
                    }
                }
                if !*follow {
                    return Ok(());
                }
                let last = reply["last"].as_u64().unwrap_or(after);
                // peek restarted: its numbering began again.
                after = if last < after { 0 } else { last };
                limit = RECENT_LINES;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        Command::Unpair { id } => match local_request(args, "POST", "/api/clients/revoke", id)?.0 {
            204 => {
                println!("Unpaired {id}. It can no longer connect; a session it has open stays until it disconnects.");
                Ok(())
            }
            _ => Err(format!("no paired browser has the id {id} (see `peek clients`)")),
        },
        Command::Install { args: flags } => {
            // --bind and --port are global flags, so clap takes them even
            // after `install`; hand them on to the background peek too.
            let given = |name: &str| flags.iter().any(|f| f == name || f.starts_with(&format!("{name}=")));
            let (add_bind, add_port) = (!args.bind.is_unspecified() && !given("--bind"), args.port != 9182 && !given("--port"));
            let mut flags = flags.clone();
            // So is --service, when it comes before `install`.
            #[cfg(windows)]
            if args.service && !given("--service") {
                flags.push("--service".into());
            }
            if add_bind {
                flags.extend(["--bind".into(), args.bind.to_string()]);
            }
            if add_port {
                flags.extend(["--port".into(), args.port.to_string()]);
            }
            install::check_service_args(&flags)?;
            install::install(&flags)
        }
        Command::Uninstall => install::uninstall(),
    }
}

/// The core, in the Windows service: it listens where there is no display,
/// and returns the link the service attaches a screen provider to, one for
/// each session that comes to the console.
#[cfg(windows)]
fn core_in_service() -> Arc<provider::Link> {
    fn fail(what: String) -> ! {
        log!("{what}");
        std::process::exit(1);
    }
    let args = Args::parse();
    let web = web_origin(&args.web).unwrap_or_else(|e| fail(e));
    if args.no_token && !(args.bind.is_unspecified() || args.bind.is_loopback() || server::tailnet(args.bind)) {
        fail(format!("--no-token accepts only Tailscale connections; --bind {} is not a Tailscale address", args.bind));
    }
    let token = (!args.no_token).then(|| {
        load_token().unwrap_or_else(|e| fail(format!("cannot store the access token in {}: {e}", config_dir().display())))
    });
    let pairing = pairing::Pairing::load(&config_dir())
        .unwrap_or_else(|e| fail(format!("cannot read or store pairing data in {}: {e}", config_dir().display())));
    let screen = provider::Link::new();
    let shared = Arc::new(server::Shared {
        token,
        web,
        pairing,
        screen: server::Screen::Provider(screen.clone()),
        next_id: AtomicU64::new(1),
    });
    let addr = SocketAddr::new(args.bind, args.port);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(format!("cannot start the async runtime: {e}")));
    // Listening before the first provider is started: a client that
    // connects in between waits for it.
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind(addr))
        .unwrap_or_else(|e| fail(format!("cannot listen on {addr}: {e}")));
    log!("service: listening on {addr}");
    std::thread::Builder::new()
        .name("core".into())
        .spawn(move || {
            if let Err(e) = runtime.block_on(server::serve(shared, listener)) {
                fail(format!("cannot serve on {addr}: {e}"));
            }
        })
        .unwrap_or_else(|e| fail(format!("cannot start the server thread: {e}")));
    screen
}

fn main() {
    let args = Args::parse();

    // `peek pair` with no peek running starts one here, then pairs.
    let mut pair_here = false;
    if let Some(command) = &args.command {
        let result = match command {
            Command::Pair => pair(&args).map(|start| pair_here = start),
            command => run_command(&args, command),
        };
        if let Err(e) = result {
            eprintln!("{e}");
            std::process::exit(1);
        }
        if !pair_here {
            return;
        }
    }
    let web = match web_origin(&args.web) {
        Ok(web) => web,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    #[cfg(windows)]
    if args.detach && args.service {
        eprintln!("--detach and --service do not go together: Windows starts the service itself");
        std::process::exit(2);
    }
    #[cfg(windows)]
    if args.detach {
        if let Err(e) = detach() {
            eprintln!("could not start in the background: {e}");
            std::process::exit(1);
        }
        return;
    }

    // SYSTEM's settings go in a folder of their own, which must not be
    // readable by every user (as a new folder under ProgramData would be).
    #[cfg(windows)]
    if platform::is_system()
        && let Err(e) = platform::create_private_dir(&config_dir())
    {
        eprintln!("cannot create {}: {e}", config_dir().display());
        std::process::exit(1);
    }

    // The screen for a core, which listens, keeps the settings and logs for
    // both: this process only has the display.
    if let Some(kind) = &args.provider {
        if kind != "screen" {
            eprintln!("no provider is called {kind}");
            std::process::exit(2);
        }
        let (from_core, core) = provider::connect();
        let _ = LOG_TO_CORE.set(core.clone());
        platform::init();
        #[cfg(windows)]
        platform::keep_device(args.keep_gpu);
        let screen = screen::start(args.view_only, !args.no_clipboard, args.verbose, args.fps);
        provider::run_screen(screen, from_core, core);
        return;
    }

    if let Some(path) = &args.log {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => {
                let _ = LOG_FILE.set(Mutex::new(file));
                let _ = LOG_PATH.set(std::path::absolute(path).unwrap_or_else(|_| path.clone()));
            }
            Err(e) => eprintln!("cannot write log file {}: {e}", path.display()),
        }
    }

    #[cfg(windows)]
    if args.service {
        if let Err(e) = platform::service::run(core_in_service) {
            log!("{e}");
            std::process::exit(1);
        }
        return;
    }

    platform::init();
    #[cfg(windows)]
    platform::keep_device(args.keep_gpu);
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

    let pairing = match pairing::Pairing::load(&config_dir()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot read or store pairing data in {}: {e}", config_dir().display());
            std::process::exit(1);
        }
    };

    let shared = Arc::new(server::Shared {
        token,
        web: web.clone(),
        pairing,
        screen: server::Screen::Here(screen::start(args.view_only, !args.no_clipboard, args.verbose, args.fps)),
        next_id: AtomicU64::new(1),
    });

    // Listen before saying so, and before handing out a pairing code.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("start async runtime");
    let addr = SocketAddr::new(args.bind, args.port);
    let listener = match runtime.block_on(tokio::net::TcpListener::bind(addr)) {
        Ok(listener) => listener,
        Err(e) => {
            log!("cannot listen on {addr}: {e}");
            std::process::exit(1);
        }
    };

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
    if let Some(web) = web.as_ref().filter(|_| !pair_here) {
        println!("To use it from {web}, pair the browser: run `peek pair`.");
    }
    if !args.view_only {
        for note in platform::input_notes() {
            println!("{note}");
        }
    }
    if pair_here {
        println!();
        if let Err(e) = print_offer(&server::pairing_offer(&shared)) {
            eprintln!("{e}");
        }
        println!("\npeek keeps running here; Ctrl+C stops it. For another code later, run `peek pair` in another terminal.");
        println!("To start peek at logon instead, run `peek install`.");
    }

    if let Err(e) = runtime.block_on(server::serve(shared, listener)) {
        log!("cannot listen on {addr}: {e}");
        std::process::exit(1);
    }
}
