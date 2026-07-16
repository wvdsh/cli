//! Open URLs in the user's default browser, with a WSL-aware fallback.
//!
//! Under WSL the [`open`] crate tries `wslview` (from the `wslu` package) and
//! then `xdg-open`/`gio open`. When `wslu` isn't installed those Linux openers
//! report success but never reach the Windows host browser, so nothing happens.
//! On WSL we therefore hand the URL to Windows ourselves.

use std::process::Command;

/// Open `url` in the user's default browser.
pub fn open_url(url: &str) -> std::io::Result<()> {
    if is_wsl() {
        return open_on_windows(url);
    }
    open::that(url)
}

/// Detect a WSL environment by inspecting the kernel release string, which
/// contains `microsoft` (WSL2) or `Microsoft` (WSL1).
fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("microsoft") || s.contains("wsl")
        })
        .unwrap_or(false)
}

/// Try each Windows-side opener in turn, returning as soon as one succeeds.
fn open_on_windows(url: &str) -> std::io::Result<()> {
    // `wslview` (wslu) respects the user's configured default browser when present.
    if run_ok(Command::new("wslview").arg(url)) {
        return Ok(());
    }
    // PowerShell ships with every WSL host. Single-quote the URL so `&` in query
    // strings is treated literally, doubling any embedded single quotes.
    let script = format!("Start-Process '{}'", url.replace('\'', "''"));
    if run_ok(Command::new("powershell.exe").args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &script,
    ])) {
        return Ok(());
    }
    Err(std::io::Error::other(
        "no working browser opener found (tried wslview and powershell.exe)",
    ))
}

/// Run `cmd` to completion, reporting whether it exited successfully.
fn run_ok(cmd: &mut Command) -> bool {
    cmd.status().map(|s| s.success()).unwrap_or(false)
}
