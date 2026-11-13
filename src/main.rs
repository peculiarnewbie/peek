//! peek: an instant-connect remote desktop host for LAN and Tailscale.
//!
//! The host stays running and idle. Capture only happens while a browser
//! client is connected and its tab is visible, so connecting costs one
//! WebSocket handshake plus one frame.

mod engine;
mod install;
mod pairing;
mod platform;
mod plugins;
mod power;
mod provider;
mod screen;
mod server;
mod vitals;
mod webp;

use std::collections::{BTreeMap, VecDeque};
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
    /// Be a provider (screen, vitals or power) for a core, speaking to it on
    /// stdin and stdout. The core starts these itself
    #[arg(long, hide = true, value_name = "KIND")]
    provider: Option<String>,
    /// Serve a provider-owned page (used by the opt-in screen plugin spike).
    #[arg(long, hide = true, requires = "provider", value_name = "FOLDER")]
    provider_page: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show a one-time code that pairs a browser with this machine. If peek
    /// is not running yet, this starts it here and keeps it running
    Pair {
        /// The HTTPS port Tailscale Serve passes to peek on: 443, 8443 or
        /// 10000. For when another app has 443. Without it, peek keeps the
        /// port it already has, or takes 443
        #[arg(long, value_name = "PORT", value_parser = serve_port)]
        serve_port: Option<u16>,
    },
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
    /// Programs that add to what this machine offers, beside its screen:
    /// each is started with peek and speaks to it on stdin and stdout
    /// (docs/providers.md). Restart peek after a change
    Provider {
        #[command(subcommand)]
        action: ProviderCommand,
    },
    /// Plugins: folders with a peek-plugin.json, installed here from a
    /// folder, a git repository or npm. Also on the Plugins page in a browser
    Plugin {
        #[command(subcommand)]
        action: PluginCommand,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Install a plugin: a folder on this machine, an https address of a git
    /// repository, or an npm package
    Add { source: String },
    /// Update a plugin, keeping its settings and data
    Update { name: String, source: Option<String> },
    /// Remove a plugin, or a command added with `peek provider add`
    Remove {
        name: String,
        /// Also delete the plugin's data
        #[arg(long)]
        purge: bool,
    },
    /// List everything beside the screen, and whether it is on
    List,
    /// Whether a paired browser may install, remove and change plugins (it
    /// can always turn them on and off)
    RemoteInstall {
        #[arg(value_parser = ["on", "off"])]
        on: String,
    },
}

#[derive(Subcommand)]
enum ProviderCommand {
    /// Add a provider: a name, then the command that starts it, like
    /// `peek provider add jobs -- node C:\jobs\provider.js`
    Add {
        name: String,
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Remove a provider, by name
    Remove { name: String },
    /// List the providers added here
    List,
}

/// What peek has beside the screen: its own providers, then the commands
/// and plugins added here. Each is a process of its own, started now and
/// again if it ends, and stopped when it is turned off.
fn own_providers(args: &Args) -> Vec<(&'static str, provider::Start)> {
    use provider::Start;
    vec![
        (include_str!("apps/vitals.json"), Start::Own { flags: Vec::new(), env: Vec::new() }),
        (include_str!("apps/power.json"), Start::Own { flags: power_flags(args), env: Vec::new() }),
    ]
}

fn plugins(args: &Args) -> Arc<plugins::Plugins> {
    plugins::Plugins::start(plugins::Catalog::new(config_dir(), own_providers(args)), args.view_only)
}

/// The config folder of the peek installed here, for commands that change
/// its settings: the service's if there is one, otherwise this user's.
fn installed_config_dir() -> PathBuf {
    if let Some(dir) = config_override() { return dir; }
    #[cfg(windows)]
    if machine_config_dir().is_dir() {
        return machine_config_dir();
    }
    config_dir()
}

fn plugin_command(args: &Args, action: &PluginCommand) -> Result<(), String> {
    let catalog = plugins::Catalog::new(installed_config_dir(), own_providers(args));
    // A running peek starts and stops providers to match; one that is not
    // running reads it all when it starts.
    let tell = || match local_request(args, "POST", "/api/plugins/reload", "") {
        Ok((204, _)) => println!("The running peek has it now."),
        _ => println!("peek reads this when it starts."),
    };
    let change = |name: &str, action: &dyn Fn() -> Result<(), String>| -> Result<(), String> {
        let on = catalog.list().iter().find(|(n, ..)| n == name).map(|(_, _, _, on)| *on);
        if on == Some(true) {
            catalog.set_on(name, false)?;
            tell();
        }
        let result = action();
        if on == Some(true) && catalog.list().iter().any(|(n, ..)| n == name) {
            let restored = catalog.set_on(name, true);
            tell();
            restored?;
        } else {
            tell();
        }
        result
    };
    match action {
        PluginCommand::List => {
            for (name, kind, runs, on) in catalog.list() {
                println!("{name:<16} {:<4} {kind:<11} {runs}", if on { "on" } else { "off" });
            }
            println!(
                "
A browser {} install, remove and change plugins here (`peek plugin remote-install on|off`).",
                if catalog.remote_install() { "may" } else { "may not" }
            );
            let kept = catalog.kept_data();
            if !kept.is_empty() {
                println!("Kept data: {}. Delete with `peek plugin remove <name> --purge`.", kept.join(", "));
            }
            Ok(())
        }
        PluginCommand::Add { source } => {
            let name = catalog.add(source, |line| println!("{line}"))?;
            println!("Installed {name}.");
            tell();
            Ok(())
        }
        PluginCommand::Update { name, source } => {
            change(name, &|| catalog.update_from(name, source.as_deref(), |line| println!("{line}")).map(|_| ()))?;
            println!("Updated {name}.");
            Ok(())
        }
        PluginCommand::Remove { name, purge } => {
            change(name, &|| catalog.remove(name, *purge))?;
            if !purge && catalog.has_data(name) {
                println!("Data kept at {}. Delete it with `peek plugin remove {name} --purge`.", catalog.data_dir(name).display());
            }
            println!("Removed {name}.");
            Ok(())
        }
        PluginCommand::RemoteInstall { on } => {
            catalog.set_remote_install(on == "on")?;
            println!(
                "{}",
                if on == "on" {
                    "A paired browser may now install, remove and change plugins here."
                } else {
                    "A paired browser may now only turn plugins on and off, and restart them."
                }
            );
            Ok(())
        }
    }
}

fn provider_command(action: &ProviderCommand) -> Result<(), String> {
    let dir = installed_config_dir();
    let path = dir.join(plugins::PROVIDERS_FILE);
    let mut all = plugins::read_commands(&dir);
    let save = |all: &BTreeMap<String, Vec<String>>| {
        fs::create_dir_all(&dir)
            .and_then(|()| fs::write(&path, serde_json::to_string_pretty(all).unwrap_or_default()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    };
    match action {
        ProviderCommand::List => {
            if all.is_empty() {
                println!("No providers are added. `peek provider add <name> -- <command>` adds one.");
            }
            for (name, command) in &all {
                println!("{name:<16}  {}", command.join(" "));
            }
            Ok(())
        }
        ProviderCommand::Add { name, command } => {
            if !plugins::name_ok(name) {
                return Err(format!(
                    "{name:?} cannot name a provider: use up to 32 lowercase letters, digits and dashes, and not {}",
                    plugins::OWN.join(", ")
                ));
            }
            all.insert(name.clone(), command.clone());
            save(&all)?;
            println!("Added {name}. It starts with peek, as the same user: restart peek to start it now.");
            Ok(())
        }
        ProviderCommand::Remove { name } => {
            if all.remove(name).is_none() {
                return Err(format!("no provider is called {name} (see `peek provider list`)"));
            }
            save(&all)?;
            println!("Removed {name}. Restart peek to stop it.");
            Ok(())
        }
    }
}

/// Where the access token and pairing data live: %APPDATA%\peek on Windows
/// (%ProgramData%\peek for the service), otherwise $XDG_CONFIG_HOME/peek or
/// ~/.config/peek.
fn config_dir() -> PathBuf {
    if let Some(dir) = config_override() { return dir; }
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

/// Isolate a development/test host from the installed host and its secrets.
fn config_override() -> Option<PathBuf> {
    std::env::var_os("PEEK_CONFIG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from)
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
    let folders: Vec<PathBuf> = config_override().map_or_else(|| folders.into_iter().collect(), |dir| vec![dir]);
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

/// `--serve-port`: one of the HTTPS ports Tailscale Serve offers.
fn serve_port(arg: &str) -> Result<u16, String> {
    arg.parse()
        .ok()
        .filter(|port| pairing::SERVE_PORTS.contains(port))
        .ok_or_else(|| "Tailscale Serve offers HTTPS on 443, 8443 and 10000 only".into())
}

/// The `tailscale serve` arguments that pass HTTPS `port` to `target`; the
/// bare form for 443, as peek has always run it.
fn serve_args(port: u16, target: &str) -> Vec<String> {
    let mut args = vec!["serve".to_string(), "--bg".into()];
    if port != 443 {
        args.push(format!("--https={port}"));
    }
    args.push(target.into());
    args
}

/// Make Tailscale Serve pass `https://<this machine>.ts.net` to peek, which
/// is how the hosted client reaches it: on HTTPS port `want`, or with none
/// given, wherever it already does, else 443. Tailscale itself asks for
/// whatever is missing (Serve and HTTPS on the tailnet, or permission on
/// Linux), so its output goes straight to the terminal.
fn ensure_serve(args: &Args, tailscale: &str, host: &str, want: Option<u16>) -> Result<(), String> {
    use pairing::Serve;
    let target = pairing::serve_target(args.bind, args.port);
    let status = pairing::serve_status(tailscale)?;
    let port = match want {
        Some(port) => port,
        None if pairing::peek_port(&status, host, &target).is_some() => return Ok(()),
        None => 443,
    };
    match pairing::serve_on(&status, host, port, &target) {
        Serve::Peek => return Ok(()),
        Serve::Other(what) => {
            let free: Vec<String> = pairing::SERVE_PORTS
                .into_iter()
                .filter(|&p| p != port && pairing::serve_on(&status, host, p, &target) == Serve::Unset)
                .map(|p| p.to_string())
                .collect();
            let mut why = format!("Tailscale Serve already uses HTTPS port {port} on this machine (for {what}), so peek leaves it alone.");
            if let Some(first) = free.first() {
                why += &format!(
                    "\nTo use port {} instead: peek pair --serve-port {first}",
                    free.join(if free.len() == 2 { " or " } else { ", " })
                );
            }
            why += &format!("\nTo give port {port} to peek: tailscale {}", serve_args(port, &target).join(" "));
            return Err(why);
        }
        Serve::Unset => {}
    }
    println!(
        "Setting up Tailscale Serve, so browsers can reach peek at https://{} …",
        pairing::serve_host(host, port)
    );
    let ok = std::process::Command::new(tailscale)
        .args(serve_args(port, &target))
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
fn pair(args: &Args, serve_port: Option<u16>) -> Result<bool, String> {
    match pairing::tailscale_cmd().zip(pairing::tailscale_name()) {
        Some((tailscale, host)) => ensure_serve(args, tailscale, &host, serve_port)?,
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
        Command::Pair { .. } => unreachable!("main runs `peek pair` itself"),
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
        Command::Provider { action } => provider_command(action),
        Command::Plugin { action } => plugin_command(args, action),
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
    let screen = provider::Link::new("screen");
    let shared = Arc::new(server::Shared {
        token,
        web,
        pairing,
        screen: server::Screen::Provider(screen.clone()),
        plugins: plugins(&args),
        view_only: args.view_only,
        serve_target: pairing::serve_target(args.bind, args.port),
        next_id: AtomicU64::new(1),
        tickets: Default::default(),
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

/// The flags the power provider needs from the core's.
fn power_flags(args: &Args) -> Vec<String> {
    if args.view_only { vec!["--view-only".into()] } else { Vec::new() }
}

fn main() {
    let args = Args::parse();

    // `peek pair` with no peek running starts one here, then pairs.
    let mut pair_here = false;
    if let Some(command) = &args.command {
        let result = match command {
            Command::Pair { serve_port } => pair(&args, *serve_port).map(|start| pair_here = start),
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

    // A provider for a core, which listens, keeps the settings and logs for
    // both.
    if let Some(kind) = &args.provider {
        if !matches!(kind.as_str(), "screen" | "vitals" | "power") {
            eprintln!("no provider is called {kind}");
            std::process::exit(2);
        }
        let (from_core, core) = provider::connect();
        let _ = LOG_TO_CORE.set(core.clone());
        if let Some(page) = &args.provider_page {
            let page = match fs::canonicalize(page).ok().filter(|p| p.is_dir()) {
                Some(page) => page,
                None => { log!("provider page is not a directory"); std::process::exit(2); }
            };
            core.about(&serde_json::json!({"page":page}).to_string());
        }
        let view_only = args.view_only || std::env::var("PEEK_VIEW_ONLY").is_ok_and(|v| v == "1");
        match kind.as_str() {
            "vitals" => vitals::run(from_core, core),
            "power" => power::run(from_core, core, view_only),
            _ => {
                platform::init();
                #[cfg(windows)]
                platform::keep_device(args.keep_gpu);
                let screen = screen::start(view_only, !args.no_clipboard, args.verbose, args.fps);
                provider::run_screen(screen, from_core, core);
            }
        }
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
        serve_target: pairing::serve_target(args.bind, args.port),
        pairing,
        screen: server::Screen::Here(screen::start(args.view_only, !args.no_clipboard, args.verbose, args.fps)),
        plugins: plugins(&args),
        view_only: args.view_only,
        next_id: AtomicU64::new(1),
        tickets: Default::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_pages_are_opt_in_and_require_a_provider() {
        assert!(Args::try_parse_from(["peek", "--provider-page", "page"]).is_err());
        let args = Args::try_parse_from(["peek", "--provider", "screen", "--provider-page", "page"]).unwrap();
        assert_eq!(args.provider.as_deref(), Some("screen"));
        assert!(Args::try_parse_from(["peek"]).unwrap().provider_page.is_none());
    }

    #[test]
    fn a_provider_added_here_cannot_take_a_name_of_peeks_own() {
        for ok in ["jobs", "build-2", "a"] {
            assert!(plugins::name_ok(ok), "{ok}");
        }
        for bad in ["", "screen", "vitals", "power", "plugins", "Jobs", "my jobs", "a/b", "../x", &"x".repeat(33)] {
            assert!(!plugins::name_ok(bad), "{bad}");
        }
    }
}
