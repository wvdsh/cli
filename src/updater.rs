use anyhow::Result;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const BIN_NAME: &str = "wavedash";

fn repo_owner() -> String {
    std::env::var("WAVEDASH_UPDATE_REPO_OWNER").unwrap_or_else(|_| "wvdsh".to_string())
}

fn repo_name() -> String {
    std::env::var("WAVEDASH_UPDATE_REPO_NAME").unwrap_or_else(|_| "cli".to_string())
}

/// Spawn a background thread that checks for a newer version.
/// Join the handle after the command runs — if an update is available, it prints a notice.
pub fn check_for_update() -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        let Ok(updater) = self_update::backends::github::Update::configure()
            .repo_owner(&repo_owner())
            .repo_name(&repo_name())
            .bin_name(BIN_NAME)
            .current_version(CURRENT_VERSION)
            .build()
        else {
            return;
        };

        let Ok(latest) = updater.get_latest_release() else {
            return;
        };

        if self_update::version::bump_is_greater(CURRENT_VERSION, &latest.version).unwrap_or(false)
        {
            eprintln!();
            if is_homebrew() {
                eprintln!(
                    "Update available: {} → {}. Run: brew upgrade wvdsh/tap/wavedash",
                    CURRENT_VERSION, latest.version
                );
            } else {
                eprintln!(
                    "Update available: {} → {}. Run: wavedash update",
                    CURRENT_VERSION, latest.version
                );
            }
        }
    })
}

/// Download and install the latest version.
pub async fn run_update() -> Result<()> {
    if is_homebrew() {
        anyhow::bail!("Installed via Homebrew. Use: brew upgrade wvdsh/tap/wavedash");
    }

    tokio::task::spawn_blocking(move || {
        let status = self_update::backends::github::Update::configure()
            .repo_owner(&repo_owner())
            .repo_name(&repo_name())
            .bin_name(BIN_NAME)
            .current_version(CURRENT_VERSION)
            .show_download_progress(true)
            .no_confirm(true)
            .build()?
            .update()?;

        println!("✓ Updated to version {}", status.version());
        Ok::<(), anyhow::Error>(())
    })
    .await??;

    Ok(())
}

fn is_homebrew() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s|
            s.contains("/Cellar/") || s.contains("/opt/homebrew/")
        ))
        .unwrap_or(false)
}
