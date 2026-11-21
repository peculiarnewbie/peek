//! peek: an instant-connect remote desktop host for LAN and Tailscale.
//!
//! The host stays running and idle. Capture only happens while a browser
//! client is connected and its tab is visible, so connecting costs one
//! WebSocket handshake plus one frame.

mod engine;
mod install;
mod update;
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
#[command(
    version = env!("PEEK_VERSION"),
    about = "Instant-connect remote desktop host for LAN and Tailscale.",
    after_long_help = "`peek <command> --help` says more of each command."
)]
struct Args {
    /// Address to listen on
    #[arg(long, default_value = "0.0.0.0", global = true)]
    bind: IpAddr,
    /// Port to listen on
    #[arg(long, default_value_t = 9182, global = true)]
    port: u16,
    /// Origin of a hosted client ("none" to refuse them all)
    ///
    /// Pages from this origin may connect with a paired token. Give it more
    /// than once for several; the first is the one `peek pair` links to.
    #[arg(long, value_name = "ORIGIN", default_values_t = default_web())]
    web: Vec<String>,
    /// Maximum frames per second
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=240))]
    fps: u32,
    /// Show the screen but ignore input
    ///
    /// Mouse, keyboard and clipboard from clients are ignored, and
    /// providers' actions refused.
    #[arg(long)]
    view_only: bool,
    /// Do not synchronise the clipboard in either direction
    #[arg(long)]
    no_clipboard: bool,
    /// Also append log lines to this file
    #[arg(long, value_name = "FILE")]
    log: Option<PathBuf>,
    /// Need no access token; accept Tailscale connections only
    ///
    /// Connections that do not come over Tailscale are refused. The hosted
    /// client still needs a paired browser.
    #[arg(long)]
    no_token: bool,
    /// Log every input event and periodic throughput figures
    #[arg(short, long)]
    verbose: bool,
    /// Start in the background with no console window, then return
    #[cfg(windows)]
    #[arg(long)]
    detach: bool,
    /// Keep the GPU device open while idle, to connect faster
    ///
    /// Kept open while nobody is watching, so connecting from idle is
    /// faster. Costs some memory, and on a laptop can keep a discrete GPU
    /// from sleeping.
    #[cfg(windows)]
    #[arg(long)]
    keep_gpu: bool,
    /// Run as a Windows service (set up by `peek install --service`)
    ///
    /// The service starts peek as SYSTEM in the session at the console, so
    /// it also shows the login and lock screens and UAC prompts.
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
    /// Show a one-time code that pairs a browser with this machine
    ///
    /// If peek is not running yet, this starts it here and keeps it
    /// running. With Tailscale, it sets up Tailscale Serve for the hosted
    /// client first.
    Pair {
        /// The HTTPS port Tailscale Serve passes to peek on: 443, 8443 or 10000
        ///
        /// For when another app has 443. Without it, peek keeps the port it
        /// already has, or takes 443.
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
    /// Start peek in the background at logon
    ///
    /// It copies peek to a fixed place and registers a logon task and
    /// firewall rule (Windows, asks for administrator) or a systemd user
    /// service (Linux). Flags after it are
    /// for that peek, like `peek install --no-token`, and replace those of
    /// the last install; with none, it keeps them. On Windows, `--service`
    /// installs a service instead, which also works at the login screen.
    /// Run again after an update.
    Install {
        /// Drop the flags of the last install and start peek with none
        ///
        /// On Windows, from the logon task.
        #[arg(long)]
        defaults: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "PEEK FLAGS")]
        args: Vec<String>,
    },
    /// Update the installed peek from a release channel. A running peek
    /// updates itself and restarts; with none running, the file is replaced
    Update {
        #[arg(long, value_parser = ["stable", "next"], conflicts_with = "rollback")]
        channel: Option<String>,
        /// Only say what is installed and what the channel has
        #[arg(long, conflicts_with = "rollback")]
        check: bool,
        /// Replace the installed file without asking the running peek to
        /// update and restart (it takes effect when peek next starts)
        #[arg(long)]
        no_restart: bool,
        /// Go back to the version the last update replaced
        #[arg(long)]
        rollback: bool,
        /// Restart the Windows service: run by the service itself to start
        /// again on a new version
        #[arg(long, hide = true)]
        restart_service: bool,
    },
    /// Restart the running peek on this machine
    Restart,
    /// Undo `peek install`
    Uninstall,
    /// Add, remove and list programs started beside peek
    ///
    /// Programs that add to what this machine offers, beside its screen:
    /// each is started with peek and speaks to it on stdin and stdout
    /// (docs/providers.md). A running peek starts and stops them to match.
    Provider {
        #[command(subcommand)]
        action: ProviderCommand,
    },
    /// Install, update, remove and list plugins
    ///
    /// Plugins: folders with a peek-plugin.json, installed here from a
    /// folder, a git repository or npm. Also on the Plugins page in a browser.
    Plugin {
        #[command(subcommand)]
        action: PluginCommand,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Install a plugin from a folder, a git repository or npm
    ///
    /// A folder on this machine, an https address of a git repository, or an
    /// npm package.
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
    /// Whether a paired browser may install, remove and change plugins
    ///
    /// It can always turn them on and off.
    RemoteInstall {
        #[arg(value_parser = ["on", "off"])]
        on: String,
    },
}

#[derive(Subcommand)]
enum ProviderCommand {
    /// Add a provider: a name, then the command that starts it
    ///
    /// Like `peek provider add jobs -- node C:\jobs\provider.js`.
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
        (include_str!("apps/files.json"), Start::Own { flags: Vec::new(), env: Vec::new() }),
        (include_str!("apps/processes.json"), Start::Own { flags: Vec::new(), env: Vec::new() }),
    ]
}

fn plugins(args: &Args) -> Arc<plugins::Plugins> {
    let plugins = plugins::Plugins::start(plugins::Catalog::new(config_dir(), own_providers(args)), args.view_only);
    let running = plugins.clone();
    update::set_before_restart(move || running.stop_all());
    plugins
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

fn provider_command(args: &Args, action: &ProviderCommand) -> Result<(), String> {
    let dir = installed_config_dir();
    let path = dir.join(plugins::PROVIDERS_FILE);
    let mut all = plugins::read_commands(&dir);
    let save = |all: &BTreeMap<String, Vec<String>>| {
        fs::create_dir_all(&dir)
            .and_then(|()| fs::write(&path, serde_json::to_string_pretty(all).unwrap_or_default()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    };
    // As for plugins: a running peek starts and stops providers to match.
    let tell = |quiet: bool| {
        let running = matches!(local_request(args, "POST", "/api/plugins/reload", ""), Ok((204, _)));
        if !quiet {
            println!("{}", if running { "The running peek has it now." } else { "peek reads this when it starts." });
        }
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
            // A new command for a provider that runs: stop the old one first.
            if all.remove(name).is_some() {
                save(&all)?;
                tell(true);
            }
            all.insert(name.clone(), command.clone());
            save(&all)?;
            println!("Added {name}. It runs with peek, as the same user.");
            tell(false);
            Ok(())
        }
        ProviderCommand::Remove { name } => {
            if all.remove(name).is_none() {
                return Err(format!("no provider is called {name} (see `peek provider list`)"));
            }
            save(&all)?;
            println!("Removed {name}.");
            tell(false);
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

/// The access token, made on first start. Only its owner may read it, like
/// the pairing secrets beside it: whoever reads it can control this machine.
fn load_token() -> std::io::Result<String> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join("token");
    // Older peeks wrote it readable by every user.
    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(&path) {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
                eprintln!("warning: {} is readable by other users, and cannot be changed: {e}", path.display());
            }
        }
    }
    pairing::read_or_create(&path, || pairing::random_hex(24))
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

/// The hosted client's sites: released builds, and nightlies (scripts/nightly.mjs).
const STABLE_WEB: &str = "https://peek.peculiarnewbie.com";
const NEXT_WEB: &str = "https://next.peek.peculiarnewbie.com";
/// The client's dev server, tunnelled out while someone works on it
/// (scripts/dev-tunnel.mjs). Only nightlies trust it.
const DEV_WEB: &str = "https://dev.peek.peculiarnewbie.com";

fn default_web() -> Vec<String> {
    web_for(env!("PEEK_CHANNEL"))
}

/// Both sites, this build's channel first: either works with any host, and
/// `peek pair` links to the one that matches it. Nightlies also take the dev
/// server, so a client change can be tried on every machine before it ships.
fn web_for(channel: &str) -> Vec<String> {
    let order: &[&str] = if channel == "next" { &[NEXT_WEB, STABLE_WEB, DEV_WEB] } else { &[STABLE_WEB, NEXT_WEB] };
    order.iter().map(|s| s.to_string()).collect()
}

/// The hosted clients' origins from `--web`, in order, or none at all if
/// any of them is "none".
fn web_origins(args: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for arg in args {
        match web_origin(arg)? {
            None => return Ok(Vec::new()),
            Some(origin) if !out.contains(&origin) => out.push(origin),
            Some(_) => {}
        }
    }
    Ok(out)
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

/// Why this peek cannot listen, in words: most often another peek already
/// does, on the same port.
fn listen_error(args: &Args, e: std::io::Error) -> String {
    let addr = SocketAddr::new(args.bind, args.port);
    if e.kind() != std::io::ErrorKind::AddrInUse {
        return format!("cannot listen on {addr}: {e}");
    }
    let (port, next) = (args.port, args.port.checked_add(1).unwrap_or(1024));
    if is_peek(args) {
        format!("peek is already running on port {port}. To run another, give it a port of its own, like --port {next}.")
    } else {
        format!("another program already uses port {port}. Start peek on another, like --port {next}.")
    }
}

/// Whether what listens where this peek would is a peek: it answers
/// /.well-known/peek with its protocol.
fn is_peek(args: &Args) -> bool {
    use std::io::Read;
    let addr = local_addr(args);
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(1)) else { return false };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    if write!(stream, "GET /.well-known/peek HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut response = String::new();
    let _ = stream.take(64 * 1024).read_to_string(&mut response);
    response.contains("\"protocol\"")
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
    println!(
        "Tailscale Serve now passes https://{} to peek. To undo it: tailscale serve --https={port} off\n",
        pairing::serve_host(host, port)
    );
    Ok(())
}

/// After `peek uninstall`: Tailscale Serve may still pass HTTPS to peek's
/// port, as `peek pair` set it up. Say how to remove that; it is not removed
/// here, as the user may have set it up for more than peek.
fn serve_left(args: &Args) {
    let Some((tailscale, host)) = pairing::tailscale_cmd().zip(pairing::tailscale_name()) else { return };
    let Ok(status) = pairing::serve_status(tailscale) else { return };
    if let Some(port) = pairing::peek_port(&status, &host, &pairing::serve_target(args.bind, args.port)) {
        println!(
            "Tailscale Serve still passes https://{} to peek's port. To remove that too: tailscale serve --https={port} off",
            pairing::serve_host(&host, port)
        );
    }
}

/// `peek pair`. Returns true when no peek is running here yet, so this
/// process should start the host and show the code itself.
fn pair(args: &Args, serve_port: Option<u16>) -> Result<bool, String> {
    // Without Tailscale the hosted client cannot reach this machine, but its
    // own page can still pair a browser on the same network.
    if let Some((tailscale, host)) = pairing::tailscale_cmd().zip(pairing::tailscale_name()) {
        ensure_serve(args, tailscale, &host, serve_port)?;
    } else if serve_port.is_some() {
        return Err("--serve-port sets up Tailscale Serve, and Tailscale is not running here".into());
    }
    if !host_running(args) {
        web_origins(&args.web)?;
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
    // The hosted client's link first, which can add more machines later; the
    // machine's own page works without it, and with `--web none`.
    let (web, own) = (offer["web"].as_str(), offer["own"].as_str());
    let lan: Vec<&str> = offer["lan"].as_array().into_iter().flatten().filter_map(|l| l.as_str()).collect();
    let Some(link) = offer["link"].as_str().or(own).or(lan.first().copied()) else {
        return Err(if offer["tailscale_only"].as_bool() == Some(true) {
            format!("peek could not find {name}'s Tailscale name, and without a token (--no-token) it accepts \
                     Tailscale only. Start Tailscale and try again.")
        } else {
            format!("peek could not find {name}'s Tailscale name, nor an address of it on a private network. \
                     Start Tailscale, or join this machine to the network of the device to pair.")
        });
    };
    if own.is_none() {
        println!("Tailscale is not running here, so this pairs a browser on this network with {name}'s own page.");
        println!("The hosted client reaches machines through Tailscale: start it, then run `peek pair` again for that.\n");
    }
    println!("Pair a browser with {name}:\n\n  {link}\n");
    // Into a pipe or a file, the code would be a block of noise.
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
    if let Some(qr) = qrcode::QrCode::new(link.as_bytes()).ok().filter(|_| terminal) {
        let art = qr
            .render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Light)
            .light_color(qrcode::render::unicode::Dense1x2::Dark)
            .build();
        println!("{art}\n");
    }
    println!("Open the link on that device{}. It works once, within 5 minutes.", if terminal { ", or scan it" } else { "" });
    if own.is_none() && lan.len() > 1 {
        println!("If it does not open, try another of {name}'s addresses:");
        for other in &lan[1..] {
            println!("  {other}");
        }
    }
    if let Some(web) = web.filter(|_| offer["link"].is_string()) {
        println!("Or on {web}, choose Add a machine: {} with code {code}.", offer["host"].as_str().unwrap_or(""));
        if let Some(own) = own {
            println!("Or open {name}'s own page instead: {own}");
        }
    }
    Ok(())
}

/// `peek update`: the running peek updates itself and restarts, which also
/// needs no administrator terminal for the service. With none running (or
/// `--no-restart`), the installed file is replaced here.
fn update_command(args: &Args, channel: Option<&str>, check: bool, no_restart: bool, rollback: bool, restart_service: bool) -> Result<(), String> {
    #[cfg(windows)]
    if restart_service {
        return update::restart_service();
    }
    #[cfg(not(windows))]
    let _ = restart_service;
    if check {
        return update::check(channel);
    }
    // PEEK_UPDATE_TARGET names a test's install: never the running peek's.
    if no_restart || std::env::var_os("PEEK_UPDATE_TARGET").is_some() || !host_running(args) {
        return update::run_here(channel, rollback);
    }
    println!("asking the running peek to {}", if rollback { "go back to its previous version" } else { "update itself" });
    let body = serde_json::json!({ "channel": channel, "rollback": rollback }).to_string();
    let (status, reply) = local_request(args, "POST", "/api/update", &body)?;
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap_or_default();
    match (status, reply["t"].as_str()) {
        (200, Some("current")) => println!("peek {} is already the latest", reply["version"].as_str().unwrap_or("?")),
        (200, Some("updated")) => println!(
            "updated from {} to {}; peek is restarting on it. peek update --rollback goes back",
            reply["from"].as_str().unwrap_or("?"),
            reply["to"].as_str().unwrap_or("?")
        ),
        // A peek from before it could update itself.
        (404, _) => {
            println!("the running peek cannot update itself; replacing the installed file instead");
            return update::run_here(channel, rollback);
        }
        _ => return Err(reply["why"].as_str().map_or_else(|| format!("the running peek answered {status}"), str::to_owned)),
    }
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
        Command::Install { defaults, args: flags } => {
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
            if *defaults && !flags.is_empty() || flags.iter().any(|f| f == "--defaults") {
                return Err("--defaults starts peek with no flags, so it takes no others".into());
            }
            // No flags keeps those of the last install: dropping one silently
            // could be --view-only, which would give remote input back.
            if flags.is_empty() && !defaults {
                return install::install(None);
            }
            install::check_service_args(&flags)?;
            install::install(Some(&flags))
        }
        Command::Uninstall => {
            if !install::uninstall()? {
                println!("peek was not installed here, so there was nothing to remove.");
            }
            serve_left(args);
            Ok(())
        }
        Command::Update { channel, check, no_restart, rollback, restart_service } => {
            update_command(args, channel.as_deref(), *check, *no_restart, *rollback, *restart_service)
        }
        Command::Restart => {
            if !host_running(args) {
                return Err(format!("peek is not running on port {}. Start it first.", args.port));
            }
            let (status, body) = local_request(args, "POST", "/api/restart", "{}")?;
            let reply: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            match (status, reply["t"].as_str()) {
                (200, Some("restarting")) => println!("peek is restarting."),
                (404, _) => return Err("the running peek does not support restart".into()),
                _ => return Err(reply["why"].as_str().map_or_else(|| format!("the running peek answered {status}"), str::to_owned)),
            }
            Ok(())
        }
        Command::Provider { action } => provider_command(args, action),
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
    let web = web_origins(&args.web).unwrap_or_else(|e| fail(e));
    if args.no_token && !(args.bind.is_unspecified() || args.bind.is_loopback() || server::tailnet(args.bind)) {
        fail(format!("--no-token accepts only Tailscale connections; --bind {} is not a Tailscale address", args.bind));
    }
    let token = (!args.no_token).then(|| {
        load_token().unwrap_or_else(|e| fail(format!("cannot store the access token in {}: {e}", config_dir().display())))
    });
    let pairing = pairing::Pairing::load(&config_dir())
        .unwrap_or_else(|e| fail(format!("cannot read or store pairing data in {}: {e}", config_dir().display())));
    // Listening before any provider is started: a second peek on the same
    // port stops here, and a client that connects in between waits.
    let addr = SocketAddr::new(args.bind, args.port);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(format!("cannot start the async runtime: {e}")));
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind(addr))
        .unwrap_or_else(|e| fail(listen_error(&args, e)));
    let screen = provider::Link::new("screen");
    let shared = Arc::new(server::Shared {
        token,
        web,
        pairing,
        screen: server::Screen::Provider(screen.clone()),
        plugins: plugins(&args),
        view_only: args.view_only,
        serve_target: pairing::serve_target(args.bind, args.port),
        listen: SocketAddr::new(args.bind, args.port),
        next_id: AtomicU64::new(1),
        tickets: Default::default(),
    });
    log!("service: listening on {addr}");
    update::started(true, false);
    update::watch();
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

/// For a flag of peek's given after a command (`peek pair --no-token`):
/// where it goes instead, as the command line with it moved.
fn flag_placement(e: &clap::Error) -> Option<String> {
    use clap::CommandFactory;
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    if e.kind() != ErrorKind::UnknownArgument {
        return None;
    }
    let Some(ContextValue::String(given)) = e.get(ContextKind::InvalidArg) else { return None };
    let flag = given.split('=').next().unwrap_or(given);
    let peek = Args::command();
    let arg = peek.get_arguments().find(|a| a.get_long().is_some_and(|l| format!("--{l}") == flag))?;
    let words: Vec<String> = std::env::args().skip(1).collect();
    let at = words.iter().position(|w| peek.find_subcommand(w).is_some())?;
    let from = words.iter().position(|w| w == flag || w.starts_with(&format!("{flag}=")))?;
    // Its value comes along, when given apart from it.
    let till = if words[from] == flag && arg.get_action().takes_values() { from + 2 } else { from + 1 }.min(words.len());
    let mut moved: Vec<String> = words[..at].to_vec();
    moved.extend_from_slice(&words[from..till]);
    moved.extend(words[at..from].iter().chain(&words[till..]).cloned());
    Some(format!(
        "error: {flag} is a flag of peek itself, not of `peek {}`: it goes before the command.\n\n  peek {}",
        words[at],
        moved.join(" ")
    ))
}

fn main() {
    let args = Args::try_parse().unwrap_or_else(|e| match flag_placement(&e) {
        Some(why) => {
            eprintln!("{why}");
            std::process::exit(2);
        }
        None => e.exit(),
    });

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
    let web = match web_origins(&args.web) {
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
        if !matches!(kind.as_str(), "screen" | "vitals" | "power" | "files" | "processes") {
            eprintln!("no provider is called {kind}");
            std::process::exit(2);
        }
        if kind == "files" { peek_files::run(None); return; }
        if kind == "processes" { peek_processes::run(None); return; }
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

    // Started by a peek that updated itself: wait for it to exit, so its
    // port, screen and plugins are free before this one takes them.
    update::wait_for_predecessor();

    // Listen first: a second peek on the same port should say so before it
    // has started anything. And listen before saying so, and before handing
    // out a pairing code.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("start async runtime");
    let addr = SocketAddr::new(args.bind, args.port);
    let listener = match runtime.block_on(tokio::net::TcpListener::bind(addr)) {
        Ok(listener) => listener,
        Err(e) => {
            log!("{}", listen_error(&args, e));
            std::process::exit(1);
        }
    };

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
        listen: SocketAddr::new(args.bind, args.port),
        pairing,
        screen: server::Screen::Here(screen::start(args.view_only, !args.no_clipboard, args.verbose, args.fps)),
        plugins: plugins(&args),
        view_only: args.view_only,
        next_id: AtomicU64::new(1),
        tickets: Default::default(),
    });

    let hosts: Vec<IpAddr> = if args.bind.is_unspecified() {
        let mut hosts = vec![IpAddr::from([127, 0, 0, 1])];
        hosts.extend(
            server::own_addresses()
                .into_iter()
                .filter(|ip| if args.no_token { server::tailnet(*ip) } else { server::allowed(*ip) }),
        );
        hosts
    } else {
        vec![args.bind]
    };
    let fragment = shared.token.as_ref().map_or(String::new(), |t| format!("#{t}"));
    println!("peek is running. Open one of these on the client:");
    for host in &hosts {
        let label = if host.is_loopback() {
            "This machine"
        } else if server::tailnet(*host) {
            "Tailscale"
        } else {
            "Your network"
        };
        println!("  {label:<13} http://{}/{fragment}", SocketAddr::new(*host, args.port));
    }
    if shared.token.is_some() {
        println!("The part after # is the access token; the browser remembers it after the first visit.");
    } else {
        println!("No access token: any device on your tailnet can connect, and nothing else can.");
        if args.bind.is_unspecified() && hosts.iter().all(|h| h.is_loopback()) {
            println!("Tailscale is not up yet; connections are accepted as soon as it is.");
        }
    }
    if let Some(web) = web.first().filter(|_| !pair_here) {
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

    update::started(false, pair_here);
    update::watch();
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
        assert_eq!(Args::try_parse_from(["peek", "--provider", "files"]).unwrap().provider.as_deref(), Some("files"));
    }

    #[test]
    fn hosted_clients_default_to_both_sites_and_can_be_listed_or_refused() {
        let web = |argv: &[&str]| web_origins(&Args::try_parse_from(argv).unwrap().web);
        // A build with no channel set is a stable one.
        assert_eq!(web(&["peek"]).unwrap(), [STABLE_WEB, NEXT_WEB]);
        assert_eq!(web_for("stable"), [STABLE_WEB, NEXT_WEB]);
        assert_eq!(web_for("next"), [NEXT_WEB, STABLE_WEB, DEV_WEB]);
        assert_eq!(web(&["peek", "--web", "https://a.example/", "--web", "http://localhost:5173"]).unwrap(),
            ["https://a.example", "http://localhost:5173"]);
        assert_eq!(web(&["peek", "--web", "https://A.example", "--web", "https://a.example"]).unwrap(), ["https://a.example"]);
        assert!(web(&["peek", "--web", "https://a.example", "--web", "none"]).unwrap().is_empty());
        assert!(web(&["peek", "--web", "http://a.example"]).is_err());
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
