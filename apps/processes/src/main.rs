use std::path::PathBuf;

fn main() { peek_processes::run(Some(page_dir())); }

/// The page beside this program: `page/` in the plugin's folder, which is
/// the folder above `bin/`, and the folder peek starts it in.
fn page_dir() -> PathBuf {
    let beside = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.parent()?.join("page")));
    let page = beside.filter(|p| p.is_dir()).unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join("page"));
    std::fs::canonicalize(&page).unwrap_or(page)
}
