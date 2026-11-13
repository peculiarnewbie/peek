//! Builds the browser client (client/, Solid + Vite) into one HTML file,
//! client/dist/index.html, which server.rs embeds. Needs Bun, or Node's npm.

use std::path::Path;
use std::process::Command;

const INPUTS: &[&str] = &["src", "public", "index.html", "package.json", "bun.lock", "tsconfig.json", "vite.config.ts"];

fn main() {
    // Which release channel this build belongs to, and its full version: a
    // nightly says which one it is (scripts/nightly.mjs sets both).
    println!("cargo:rerun-if-env-changed=PEEK_CHANNEL");
    println!("cargo:rerun-if-env-changed=PEEK_BUILD");
    let channel = std::env::var("PEEK_CHANNEL").unwrap_or_else(|_| "stable".into());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let version = match std::env::var("PEEK_BUILD") {
        Ok(build) if !build.is_empty() => format!("{version}-{build}"),
        _ => version,
    };
    println!("cargo:rustc-env=PEEK_CHANNEL={channel}");
    println!("cargo:rustc-env=PEEK_VERSION={version}");

    for input in INPUTS {
        println!("cargo:rerun-if-changed=client/{input}");
    }
    let client = Path::new("client");
    let tool = package_manager();
    // Every time, not only when node_modules is missing: a pull can add a
    // package. With nothing to do this takes a moment.
    let install = if tool == "bun" { vec!["install", "--frozen-lockfile"] } else { vec!["install"] };
    run(&tool, &install, client);
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
