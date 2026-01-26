use anyhow::Result;
use axoupdater::AxoUpdater;
use crate::config;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const BIN_NAME: &str = env!("CARGO_BIN_NAME");
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Serialize, Deserialize)]
struct UpdateCache {
    latest_version: String,
    last_check: String,
    show_notification: bool,
    install_method: String,
    #[serde(default)]
    check_count: u32,
}

impl UpdateCache {
    fn cache_path() -> Result<PathBuf> {
        let cache_dir = config::wvdsh_dir()?;
        fs::create_dir_all(&cache_dir)?;
        Ok(cache_dir.join("update-cache.json"))
    }

    fn load() -> Option<Self> {
        let path = Self::cache_path().ok()?;
        let contents = fs::read_to_string(path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    fn save(&self) -> Result<()> {
        let path = Self::cache_path()?;
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(path, contents)?;
        Ok(())
    }
}

/// Check for updates and show message if available (non-blocking)
pub fn check_for_updates() -> std::thread::JoinHandle<()> {
    // Show cached notification if available
    if let Some(cache) = UpdateCache::load() {
        if cache.show_notification {
            if let (Ok(current), Ok(latest)) = (
                Version::parse(CURRENT_VERSION),
                Version::parse(&cache.latest_version),
            ) {
                if latest > current {
                    println!("\n🎉 Update available → {}", cache.latest_version);
                    // Always check current install method, don't rely on cached value
                    if is_homebrew() {
                        println!("Run: brew upgrade wvdsh/tap/wvdsh");
                    } else {
                        println!("Run: wvdsh update");
                    }
                    println!();
                }
            }
        }
    }

    // Spawn background check for next run
    std::thread::spawn(|| {
        let _ = tokio::runtime::Runtime::new()
            .map(|rt| rt.block_on(async { background_update_check().await }));
    })
}

async fn background_update_check() -> Result<()> {
    // Check Homebrew first (takes priority)
    if is_homebrew() {
        // Get current check count and increment
        let check_count = UpdateCache::load()
            .map(|c| c.check_count)
            .unwrap_or(0) + 1;

        // Pull tap every 3 checks to get fresh version info
        if check_count % 3 == 1 {
            let _ = pull_homebrew_tap().await;
        }

        if let Some((version, count)) = check_homebrew_version(check_count).await? {
            let current = Version::parse(CURRENT_VERSION)?;
            let latest = Version::parse(&version)?;

            let cache = UpdateCache {
                latest_version: version,
                last_check: chrono::Utc::now().to_rfc3339(),
                show_notification: latest > current,
                install_method: "homebrew".to_string(),
                check_count: count,
            };
            cache.save()?;
        }
        return Ok(());
    }

    // Try axoupdater for shell/powershell installs
    let mut updater = AxoUpdater::new_for(BIN_NAME);

    if updater.load_receipt().is_ok() {
        // Shell/powershell install
        if let Some(new_version) = updater.query_new_version().await? {
            let check_count = UpdateCache::load()
                .map(|c| c.check_count)
                .unwrap_or(0) + 1;
            let cache = UpdateCache {
                latest_version: new_version.to_string(),
                last_check: chrono::Utc::now().to_rfc3339(),
                show_notification: true,
                install_method: "shell".to_string(),
                check_count,
            };
            cache.save()?;
        }
    }

    Ok(())
}

async fn pull_homebrew_tap() -> Result<()> {
    // Get the tap repo path
    let repo_output = tokio::process::Command::new("brew")
        .args(["--repo", "wvdsh/homebrew-tap"])
        .output()
        .await?;

    if !repo_output.status.success() {
        return Ok(());
    }

    let repo_path = String::from_utf8_lossy(&repo_output.stdout).trim().to_string();

    // Git pull to refresh
    tokio::process::Command::new("git")
        .args(["-C", &repo_path, "pull", "--quiet"])
        .output()
        .await?;

    Ok(())
}

async fn check_homebrew_version(check_count: u32) -> Result<Option<(String, u32)>> {
    // Use brew info to check version of custom tap formula
    let output = tokio::process::Command::new("brew")
        .args(["info", "--json=v2", "wvdsh/tap/wvdsh"])
        .output()
        .await?;

    if !output.status.success() {
        return Ok(None);
    }

    let response: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    Ok(response["formulae"][0]["versions"]["stable"]
        .as_str()
        .map(|s| (s.to_string(), check_count)))
}

/// Run updater to install latest version
pub async fn run_update() -> Result<()> {

    if is_homebrew() {
        anyhow::bail!("Installed via Homebrew. Use: brew upgrade wvdsh/tap/wvdsh");
    }

    // Mark notification as acknowledged
    if let Some(mut cache) = UpdateCache::load() {
        cache.show_notification = false;
        cache.save()?;
    }

    let mut updater: AxoUpdater = AxoUpdater::new_for(BIN_NAME);
    
    // Load receipt - will fail for homebrew/manual installs
    if let Err(_) = updater.load_receipt() {
        anyhow::bail!("No updates available");
    }
    
    updater.always_update(true);
    
    match updater.run().await? {
        Some(result) => println!("✓ Updated to version {}", result.new_version),
        None => println!("✓ Already on latest version"),
    }
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
