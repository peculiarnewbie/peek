//! `peek install` and `peek uninstall`: copy this executable to a fixed place
//! and start it in the background at logon from there.
//!
//! The copy matters when peek came from a package manager: its own path
//! changes with each update (and with the Node version, for npm), and on
//! Windows a running executable cannot be replaced. The logon task, firewall
//! rule or systemd unit point at the copy instead, and installing again
//! replaces it.
//!
//! On Windows, `peek install --service` installs a service instead of the
//! logon task (see `platform::windows::service`). Its copy goes under
//! Program Files, where only administrators can replace what SYSTEM runs.

use std::path::PathBuf;

/// Checks the flags meant for the background peek, so a typo fails now
/// rather than at the next logon.
pub fn check_service_args(args: &[String]) -> Result<(), String> {
    use clap::Parser;
    let parsed = crate::Args::try_parse_from(std::iter::once("peek".to_string()).chain(args.iter().cloned()))
        .map_err(|e| {
            let first = e.to_string().lines().next().unwrap_or_default().to_string();
            format!("not a valid flag for peek: {}", first.trim_start_matches("error: "))
        })?;
    if parsed.command.is_some() {
        return Err("`peek install` takes flags for peek, not another command".into());
    }
    #[cfg(windows)]
    if parsed.detach || parsed.log.is_some() {
        return Err("--detach and --log are set by `peek install` itself".into());
    }
    Ok(())
}

#[cfg(windows)]
pub use self::windows::{install, uninstall};
#[cfg(windows)]
pub(crate) use self::windows::{installed_exe, service_exe};

#[cfg(target_os = "linux")]
pub use self::linux::{install, uninstall};
#[cfg(target_os = "linux")]
pub(crate) use self::linux::installed_exe;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};

    use super::env_path;

    /// The firewall and logon-task half lives in the PowerShell script that
    /// also serves people running from a source checkout.
    const SCRIPT: &str = include_str!("../scripts/install-autostart.ps1");

    /// %LOCALAPPDATA%\Programs\peek\peek.exe, the per-user programs folder,
    /// for the logon task.
    pub(crate) fn installed_exe() -> Result<PathBuf, String> {
        let base = env_path("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
        Ok(base.join("Programs").join("peek").join("peek.exe"))
    }

    /// %ProgramFiles%\peek\peek.exe, for the service: SYSTEM runs it, so no
    /// user may be able to replace it.
    pub(crate) fn service_exe() -> PathBuf {
        env_path("ProgramFiles").unwrap_or_else(|| PathBuf::from(r"C:\Program Files")).join("peek").join("peek.exe")
    }

    /// Paths the script needs whichever way peek is installed, since
    /// installing one way removes the other.
    fn paths() -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "userExe": installed_exe()?,
            "serviceExe": service_exe(),
            "userConfig": crate::user_config_dir(),
            "machineConfig": crate::machine_config_dir(),
            "callerPid": std::process::id(),
        }))
    }

    /// With `None`, the script keeps the flags of the logon task or service
    /// installed now, and the service stays a service.
    pub fn install(service_args: Option<&[String]>) -> Result<(), String> {
        let reuse = service_args.is_none();
        let service_args = service_args.unwrap_or_default();
        let service = if reuse { service_installed() } else { service_args.iter().any(|a| a == "--service") };
        let peek_args: Vec<&String> = service_args.iter().filter(|a| *a != "--service").collect();
        let source = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut config = paths()?;
        config["source"] = serde_json::json!(source);
        config["user"] = serde_json::json!(current_user()?);
        config["peekArgs"] = serde_json::json!(peek_args);
        config["service"] = serde_json::json!(service);
        config["reuseArgs"] = serde_json::json!(reuse);
        config["remove"] = serde_json::json!(false);
        run_script(&config)?;
        if service {
            println!("peek now runs as a Windows service from {}, as SYSTEM.", service_exe().display());
            println!("It shows the login and lock screens and UAC prompts too, so anyone with its");
            println!("token or a paired browser controls this PC there as well.");
        } else {
            println!("peek now starts at logon from {}, as administrator.", installed_exe()?.display());
        }
        println!("Run `peek install` again after updating peek: it keeps these flags. Flags after it replace them,");
        println!("`peek install --defaults` drops them, and `peek uninstall` removes peek.");
        Ok(())
    }

    /// Whether the peek service is installed; installing the logon task
    /// removes it, so then it is what runs. Any user may query a service.
    fn service_installed() -> bool {
        std::process::Command::new("sc.exe")
            .args(["query", "peek"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// False when nothing was installed: then there is nothing to remove,
    /// and no reason to ask for administrator rights.
    pub fn uninstall() -> Result<bool, String> {
        let task = std::process::Command::new("schtasks.exe")
            .args(["/query", "/tn", "peek"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !task && !service_installed() && !installed_exe()?.exists() && !service_exe().exists() {
            return Ok(false);
        }
        let mut config = paths()?;
        config["remove"] = serde_json::json!(true);
        run_script(&config)?;
        Ok(true)
    }

    /// DOMAIN\user of whoever ran `peek install`. Taken here, not in the
    /// elevated script: with a standard account, Windows asks for an
    /// administrator's password and the script then runs as that other user.
    fn current_user() -> Result<String, String> {
        let user = std::env::var("USERNAME").map_err(|_| "USERNAME is not set")?;
        Ok(match std::env::var("USERDOMAIN") {
            Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
            _ => user,
        })
    }

    /// Runs the script as administrator with its settings in a JSON file
    /// (no quoting through two command lines), asking through UAC unless
    /// this process is elevated already. An elevated window cannot write to
    /// this console, so it writes what it did to a file that is printed here.
    fn run_script(config: &serde_json::Value) -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("peek-install-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let result = (|| {
            let script = dir.join("install-autostart.ps1");
            // A byte-order mark, or Windows PowerShell reads the file as ANSI.
            std::fs::write(&script, format!("\u{feff}{SCRIPT}")).map_err(|e| e.to_string())?;
            let output = dir.join("output.txt");
            let mut config = config.clone();
            config["output"] = serde_json::json!(output);
            let config_path = dir.join("config.json");
            std::fs::write(&config_path, config.to_string()).map_err(|e| e.to_string())?;

            let code = elevate(&script, &config_path)?;
            if let Ok(text) = std::fs::read_to_string(&output) {
                print!("{text}");
            }
            if code != 0 {
                return Err(format!("the install script failed (exit code {code})"));
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn elevate(script: &Path, config: &Path) -> Result<u32, String> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
        use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        use windows::core::{HSTRING, PCWSTR, w};

        let params = format!(
            "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\" -Config \"{}\"",
            script.display(),
            config.display()
        );
        if crate::platform::is_elevated() {
            let status = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(script)
                .arg("-Config")
                .arg(config)
                .status()
                .map_err(|e| format!("could not run PowerShell: {e}"))?;
            return Ok(status.code().unwrap_or(1) as u32);
        }
        println!("Windows will ask for administrator rights, to set up the firewall rule and how peek starts.");
        let params = HSTRING::from(params);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: w!("powershell.exe"),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe {
            ShellExecuteExW(&mut info)
                .map_err(|e| format!("administrator rights were not granted, so nothing was installed ({e})"))?;
            if info.hProcess.is_invalid() {
                return Err("could not start the elevated installer".into());
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
            Ok(code)
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;

    use super::env_path;

    const UNIT: &str = include_str!("../scripts/peek.service");

    fn home() -> Result<PathBuf, String> {
        env_path("HOME").ok_or_else(|| "HOME is not set".to_string())
    }

    /// $XDG_DATA_HOME/peek/peek, or ~/.local/share/peek/peek.
    pub(crate) fn installed_exe() -> Result<PathBuf, String> {
        let data = match env_path("XDG_DATA_HOME") {
            Some(d) => d,
            None => home()?.join(".local").join("share"),
        };
        Ok(data.join("peek").join("peek"))
    }

    /// $XDG_CONFIG_HOME/systemd/user/peek.service, or under ~/.config.
    fn unit_path() -> Result<PathBuf, String> {
        let config = match env_path("XDG_CONFIG_HOME") {
            Some(c) => c,
            None => home()?.join(".config"),
        };
        Ok(config.join("systemd").join("user").join("peek.service"))
    }

    fn systemctl(args: &[&str]) -> Result<(), String> {
        let status = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .status()
            .map_err(|e| format!("could not run systemctl: {e}"))?;
        if !status.success() {
            return Err(format!("`systemctl --user {}` failed", args.join(" ")));
        }
        Ok(())
    }

    /// One argument on a systemd ExecStart line.
    pub(super) fn quote(arg: &str) -> String {
        if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+".contains(c)) {
            arg.to_string()
        } else {
            format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"))
        }
    }

    /// The words of a systemd ExecStart value, undoing `quote`.
    fn unquote(line: &str) -> Vec<String> {
        let (mut words, mut word, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => (quoted, any) = (!quoted, true),
                '\\' if quoted => word.extend(chars.next()),
                '%' => {
                    word.push('%');
                    if chars.clone().next() == Some('%') {
                        chars.next();
                    }
                }
                c if c.is_whitespace() && !quoted => {
                    if any || !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                    any = false;
                }
                c => word.push(c),
            }
        }
        if any || !word.is_empty() {
            words.push(word);
        }
        words
    }

    /// The flags on the ExecStart line of the unit installed now, if any.
    pub(super) fn installed_flags(unit: &str) -> Vec<String> {
        unit.lines()
            .find_map(|l| l.strip_prefix("ExecStart="))
            .map(|exec| unquote(exec).into_iter().skip(1).collect())
            .unwrap_or_default()
    }

    /// With `None`, keeps the flags of the unit installed now.
    pub fn install(service_args: Option<&[String]>) -> Result<(), String> {
        let path = unit_path()?;
        let kept = service_args.is_none();
        let service_args = match service_args {
            Some(flags) => flags.to_vec(),
            None => {
                let flags = installed_flags(&std::fs::read_to_string(&path).unwrap_or_default());
                super::check_service_args(&flags).map_err(|e| {
                    format!(
                        "the flags of the last install ({}) no longer work: {e}.\n\
                         Give new ones after `peek install`, or `peek install --defaults` for none",
                        flags.join(" ")
                    )
                })?;
                flags
            }
        };
        let source = std::env::current_exe().map_err(|e| e.to_string())?;
        let target = installed_exe()?;
        let dir = target.parent().unwrap();
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        if source != target {
            // Copy beside it and rename over it: a running peek keeps its
            // old file, which a plain copy onto it would fail on.
            let staged = dir.join(".peek.new");
            std::fs::copy(&source, &staged).map_err(|e| format!("cannot copy peek to {}: {e}", dir.display()))?;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            std::fs::rename(&staged, &target).map_err(|e| e.to_string())?;
        }

        let exec = std::iter::once(target.to_string_lossy().into_owned())
            .chain(service_args.iter().cloned())
            .map(|a| quote(&a))
            .collect::<Vec<_>>()
            .join(" ");
        let unit: String = UNIT
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| if l.starts_with("ExecStart=") { format!("ExecStart={exec}") } else { l.to_string() })
            .collect::<Vec<_>>()
            .join("\n");
        let unit = format!("# Written by `peek install`; run it again to change, `peek uninstall` to remove.\n{}\n", unit.trim_start());
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&path, unit).map_err(|e| format!("cannot write {}: {e}", path.display()))?;

        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "peek"])?;
        systemctl(&["restart", "peek"])?;
        println!("peek now starts with the graphical session, from {}.", target.display());
        let shown = service_args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
        match (service_args.is_empty(), kept) {
            (true, _) => println!("It runs with no flags."),
            (false, true) => println!("It runs with the flags of the last install: {shown}"),
            (false, false) => println!("It runs with: {shown}"),
        }
        println!("`journalctl --user -u peek` shows its output. Run `peek install` again after updating peek:");
        println!("it keeps these flags. Flags after it replace them, and `peek install --defaults` drops them.");
        Ok(())
    }

    /// False when nothing was installed.
    pub fn uninstall() -> Result<bool, String> {
        let path = unit_path()?;
        let target = installed_exe()?;
        if !path.exists() && !target.exists() {
            return Ok(false);
        }
        if path.exists() {
            let _ = systemctl(&["disable", "--now", "peek"]);
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            let _ = systemctl(&["daemon-reload"]);
        }
        if target.exists() {
            std::fs::remove_file(&target).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_dir(target.parent().unwrap());
        }
        println!("Removed the peek service and {}.", target.display());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::check_service_args;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn accepts_peek_flags() {
        assert!(check_service_args(&[]).is_ok());
        assert!(check_service_args(&args(&["--no-token", "--port", "9000", "--fps", "30"])).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn accepts_service_but_not_detach() {
        assert!(check_service_args(&args(&["--service", "--no-token"])).is_ok());
        assert!(check_service_args(&args(&["--service", "--detach"])).is_err());
    }

    #[test]
    fn rejects_typos_and_commands() {
        assert!(check_service_args(&args(&["--no-tokn"])).is_err());
        assert!(check_service_args(&args(&["pair"])).is_err());
        assert!(check_service_args(&args(&["--fps", "999"])).is_err());
    }

    #[test]
    fn install_captures_trailing_flags() {
        use clap::Parser;
        let parsed = crate::Args::try_parse_from(["peek", "install", "--no-token", "--fps", "30"]).unwrap();
        match parsed.command {
            Some(crate::Command::Install { args, .. }) => assert_eq!(args, ["--no-token", "--fps", "30"]),
            _ => panic!("expected install"),
        }
    }

    /// --port is a global flag: whichever of `install` or the trailing flags
    /// gets it, main.rs hands it on to the background peek.
    #[test]
    fn install_with_port_is_not_lost() {
        use clap::Parser;
        let parsed = crate::Args::try_parse_from(["peek", "install", "--port", "9000", "--no-token"]).unwrap();
        let Some(crate::Command::Install { args, .. }) = parsed.command else { panic!("expected install") };
        assert!(parsed.port == 9000 || args.windows(2).any(|w| w == ["--port", "9000"]), "port lost: {args:?}");
    }

    #[test]
    fn install_defaults_is_its_own_flag() {
        use clap::Parser;
        let parsed = crate::Args::try_parse_from(["peek", "install", "--defaults"]).unwrap();
        let Some(crate::Command::Install { defaults, args }) = parsed.command else { panic!("expected install") };
        assert!(defaults && args.is_empty());
        let parsed = crate::Args::try_parse_from(["peek", "install", "--no-token"]).unwrap();
        let Some(crate::Command::Install { defaults, .. }) = parsed.command else { panic!("expected install") };
        assert!(!defaults);
    }

    /// A reinstall reads the last install's flags back from its unit.
    #[cfg(target_os = "linux")]
    #[test]
    fn unit_flags_survive_a_round_trip() {
        let flags = ["--no-token", "--view-only", "--log", "/tmp/a b/peek.log", "--web", "100%\\\"x"];
        let exec = std::iter::once("/home/u/.local/share/peek/peek")
            .chain(flags)
            .map(super::linux::quote)
            .collect::<Vec<_>>()
            .join(" ");
        let unit = format!("[Unit]\nDescription=peek\n\n[Service]\nExecStart={exec}\nRestart=on-failure\n");
        assert_eq!(super::linux::installed_flags(&unit), flags);
        assert!(super::linux::installed_flags("[Service]\nExecStart=%h/.local/bin/peek\n").is_empty());
        assert!(super::linux::installed_flags("").is_empty());
    }
}
