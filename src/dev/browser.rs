use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::Handler;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::AsyncWriteExt;

use crate::config;

const CHROME_VERSION: &str = "147.0.7727.56";

fn download_url() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("https://storage.googleapis.com/chrome-for-testing-public/147.0.7727.56/mac-arm64/chrome-mac-arm64.zip"),
        ("macos", "x86_64") => Ok("https://storage.googleapis.com/chrome-for-testing-public/147.0.7727.56/mac-x64/chrome-mac-x64.zip"),
        ("linux", "x86_64") => Ok("https://storage.googleapis.com/chrome-for-testing-public/147.0.7727.56/linux64/chrome-linux64.zip"),
        ("windows", "x86_64") => Ok("https://storage.googleapis.com/chrome-for-testing-public/147.0.7727.56/win64/chrome-win64.zip"),
        (os, arch) => anyhow::bail!("Unsupported platform: {os}/{arch}"),
    }
}

fn chrome_dir() -> Result<PathBuf> {
    Ok(config::wavedash_dir()?.join("chrome"))
}

fn chrome_executable() -> Result<PathBuf> {
    let base = chrome_dir()?;
    let rel = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ("macos", "x86_64") => "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ("linux", _) => "chrome-linux64/chrome",
        ("windows", _) => "chrome-win64/chrome.exe",
        (os, arch) => anyhow::bail!("Unsupported platform: {os}/{arch}"),
    };
    Ok(base.join(rel))
}

pub async fn ensure_chrome_installed() -> Result<PathBuf> {
    let exe = chrome_executable()?;
    if exe.exists() {
        return Ok(exe);
    }

    let dir = chrome_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create Chrome directory at {}", dir.display()))?;

    println!("Downloading Chrome for Testing {CHROME_VERSION}...");

    let url = download_url()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;

    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| "Failed to download Chrome for Testing")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Chrome download failed with status {}",
            response.status()
        );
    }

    let total_size = response.content_length().unwrap_or(0);
    let pb = if std::io::stderr().is_terminal() && total_size > 0 {
        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::with_template("[{bar:40.cyan/blue}] {percent:>3}% {msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb.set_message("Downloading Chrome...");
        pb.enable_steady_tick(Duration::from_millis(100));
        Some(pb)
    } else {
        None
    };

    let zip_path = dir.join("chrome.zip");
    let mut file = tokio::fs::File::create(&zip_path)
        .await
        .with_context(|| "Failed to create temporary zip file")?;

    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| "Error downloading Chrome")?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if let Some(pb) = &pb {
            pb.set_position(downloaded);
        }
    }

    file.flush().await?;
    drop(file);

    if let Some(pb) = &pb {
        pb.finish_with_message("Download complete");
    } else {
        println!("Download complete.");
    }

    println!("Extracting Chrome...");
    extract_zip(&zip_path, &dir)?;

    // Clean up zip
    let _ = std::fs::remove_file(&zip_path);

    // Make executable on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let exe_path = chrome_executable()?;
        if exe_path.exists() {
            let mut perms = std::fs::metadata(&exe_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&exe_path, perms)?;
        }
    }

    let exe = chrome_executable()?;
    if !exe.exists() {
        anyhow::bail!(
            "Chrome executable not found at {} after extraction",
            exe.display()
        );
    }

    println!("Chrome for Testing {CHROME_VERSION} installed.");
    Ok(exe)
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("Failed to open {}", zip_path.display()))?;

    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| "Failed to read Chrome zip archive")?;

    archive
        .extract(dest)
        .with_context(|| format!("Failed to extract Chrome to {}", dest.display()))?;

    Ok(())
}

pub async fn launch_chrome(executable: &Path) -> Result<(Browser, Handler)> {
    let profile_dir = chrome_dir()?.join("profile");
    std::fs::create_dir_all(&profile_dir)?;

    let config = BrowserConfig::builder()
        .chrome_executable(executable)
        .with_head()
        .arg("--disable-features=PrivateNetworkAccessChecks,PrivateNetworkAccessPermissionPrompt")
        .arg("--force-gpu-rasterization")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-client-side-phishing-detection")
        .arg("--disable-default-apps")
        .arg("--disable-extensions")
        .arg("--disable-sync")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build Chrome config: {e}"))?;

    let (browser, handler) = Browser::launch(config)
        .await
        .with_context(|| "Failed to launch Chrome for Testing")?;

    Ok((browser, handler))
}
