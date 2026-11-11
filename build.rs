//! Builds the browser client (client/, Solid + Vite) into one HTML file,
//! client/dist/index.html, which server.rs embeds. Needs Bun, or Node's npm.

use std::path::Path;
use std::process::Command;

const INPUTS: &[&str] = &["src", "index.html", "package.json", "bun.lock", "tsconfig.json", "vite.config.ts"];

fn main() {
    for input in INPUTS {
        println!("cargo:rerun-if-changed=client/{input}");
    }
    let client = Path::new("client");
    let tool = package_manager();
    if !client.join("node_modules").exists() {
        let install = if tool == "bun" { vec!["install", "--frozen-lockfile"] } else { vec!["install"] };
        run(&tool, &install, client);
    }
    run(&tool, &["run", "build"], client);
}

/// Bun if it is installed, otherwise npm.
fn package_manager() -> String {
    let works = |cmd: &str| Command::new(cmd).arg("--version").output().is_ok_and(|o| o.status.success());
    if works("bun") {
        "bun".into()
    } else if cfg!(windows) {
        "npm.cmd".into()
    } else {
        "npm".into()
    }
}

fn run(tool: &str, args: &[&str], dir: &Path) {
    let what = format!("{tool} {}", args.join(" "));
    let out = Command::new(tool)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("cannot run `{what}` to build the client (install Bun or Node): {e}"));
    if !out.status.success() {
        panic!(
            "`{what}` failed while building the client:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
