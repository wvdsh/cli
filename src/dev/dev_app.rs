//! Wavedash Dev install/launch resolution.
//!
//! The dev-app is an Electron app whose source lives at `cli/dev-app/`. The
//! CLI does not embed it; on first run we download a platform-specific zip
//! from `releases.wavedash.com/dev-app/<version>/<platform>.zip` and cache it
//! under `~/.wavedash/dev-app/<version>/<platform>/`.
//!
//! ## Version sync invariant
//!
//! The dev-app version is **identical** to the CLI version (this crate's
//! `Cargo.toml` version). Every CLI release ships a matching dev-app build
//! at the same version on the same GitHub Release, even if the dev-app
//! source didn't change. That means a user on CLI N gets dev-app N, period
//! — no compatibility matrix to reason about.
//!
//! Practical consequence: `dev-app-release.yml` fires on the same tag as
//! `release.yml`, patches `dev-app/package.json`'s `version` field to the
//! tag, attaches `<platform>.zip` artifacts to the GitHub Release for that
//! tag. If you bump the CLI without also publishing a matching dev-app,
//! `wavedash dev` breaks for everyone on the new CLI.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::AsyncWriteExt;

use crate::config;

/// Dev-app version pinned to the CLI version. See module docs.
pub const DEV_APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Public host of the `wavedash-dev-app` R2 bucket — baked in at build
/// time from Doppler. URL shape:
///   <host>/<version>/<platform>.zip
const CDN_HOST: &str = env!("CF_R2_WAVEDASH_DEV_APP_HOST");

/// Set this env var to an absolute path of a built `cli/dev-app/` checkout
/// (with `bun install` and `bun run build` already run) to skip the download
/// and launch via `npx electron .` instead. Useful when iterating on the
/// dev-app source before publishing a release.
const DEV_PATH_ENV: &str = "DEV_APP_DEV_PATH";

/// Argv for spawning the dev-app process.
#[derive(Debug)]
pub struct DevAppLaunch {
    pub command: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
}

fn platform_folder() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "mac-arm64",
        ("linux", "x86_64") => "linux-x64",
        ("windows", "x86_64") => "win-x64",
        (os, arch) => anyhow::bail!("Unsupported platform: {os}/{arch}"),
    })
}

fn download_url() -> Result<String> {
    let host = CDN_HOST.trim_end_matches('/');
    let host = if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("https://{}", host)
    };
    Ok(format!(
        "{}/{}/{}.zip",
        host,
        DEV_APP_VERSION,
        platform_folder()?,
    ))
}

fn install_dir() -> Result<PathBuf> {
    Ok(config::wavedash_dir()?
        .join("dev-app")
        .join(DEV_APP_VERSION)
        .join(platform_folder()?))
}

fn dev_app_executable() -> Result<PathBuf> {
    let base = install_dir()?;
    let rel: PathBuf = match std::env::consts::OS {
        "macos" => [
            "Wavedash Dev.app",
            "Contents",
            "MacOS",
            "Wavedash Dev",
        ]
        .iter()
        .collect(),
        "linux" => "wavedash-dev".into(),
        "windows" => "Wavedash Dev.exe".into(),
        os => anyhow::bail!("Unsupported OS: {os}"),
    };
    Ok(base.join(rel))
}

/// Profile dir lives OUTSIDE the version-specific install dir so version
/// bumps don't blow away cookies/localStorage between runs.
pub fn user_data_dir() -> Result<PathBuf> {
    Ok(config::wavedash_dir()?.join("dev-app-profile"))
}

/// Resolve how to launch the dev-app: either the cached binary on disk
/// (downloading + extracting if needed) or a dev-mode `npx electron .`
/// against `DEV_APP_DEV_PATH`.
pub async fn ensure_dev_app() -> Result<DevAppLaunch> {
    if let Ok(dev_path) = std::env::var(DEV_PATH_ENV) {
        let dev_path = PathBuf::from(dev_path);
        if !dev_path.is_dir() {
            anyhow::bail!(
                "{}={} is not a directory",
                DEV_PATH_ENV,
                dev_path.display()
            );
        }
        return Ok(DevAppLaunch {
            command: OsString::from("npx"),
            args: vec![OsString::from("electron"), OsString::from(".")],
            cwd: Some(dev_path),
        });
    }

    let exe = ensure_installed().await?;
    Ok(DevAppLaunch {
        command: exe.into_os_string(),
        args: vec![],
        cwd: None,
    })
}

async fn ensure_installed() -> Result<PathBuf> {
    let exe = dev_app_executable()?;
    if exe.exists() {
        return Ok(exe);
    }

    let dir = install_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| {
        format!("Failed to create dev-app directory at {}", dir.display())
    })?;

    let url = download_url()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;

    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Failed to download Wavedash dev-app from {url}"))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Wavedash dev-app download failed with status {} (url: {})",
            response.status(),
            url,
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
        pb.set_message("Downloading Wavedash dev-app...");
        pb.enable_steady_tick(Duration::from_millis(100));
        Some(pb)
    } else {
        None
    };

    let zip_path = dir.join("dev-app.zip");
    let mut file = tokio::fs::File::create(&zip_path)
        .await
        .with_context(|| "Failed to create temporary zip file")?;

    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| "Error downloading Wavedash dev-app")?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if let Some(pb) = &pb {
            pb.set_position(downloaded);
        }
    }
    file.flush().await?;
    drop(file);

    if let Some(pb) = &pb {
        pb.finish_and_clear();
    }

    extract_zip(&zip_path, &dir)?;
    let _ = std::fs::remove_file(&zip_path);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let exe_path = dev_app_executable()?;
        if exe_path.exists() {
            let mut perms = std::fs::metadata(&exe_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&exe_path, perms)?;
        }
    }

    let exe = dev_app_executable()?;
    if !exe.exists() {
        anyhow::bail!(
            "Wavedash dev-app executable not found at {} after extraction",
            exe.display()
        );
    }

    if let Err(e) = cleanup_old_versions() {
        eprintln!(
            "warning: failed to clean up old Wavedash dev-app versions: {}",
            e
        );
    }

    Ok(exe)
}

/// Remove every `~/.wavedash/dev-app/<version>/` directory except the one
/// matching the current CLI version. Best-effort: per-entry failures are
/// logged but do not abort install.
fn cleanup_old_versions() -> Result<()> {
    let parent = config::wavedash_dir()?.join("dev-app");
    if !parent.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&parent)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_name() == DEV_APP_VERSION {
            continue;
        }
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if let Err(e) = std::fs::remove_dir_all(&path) {
            eprintln!(
                "warning: failed to remove old dev-app version at {}: {}",
                path.display(),
                e
            );
        }
    }
    Ok(())
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("Failed to open {}", zip_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| "Failed to read dev-app archive")?;
    archive
        .extract(dest)
        .with_context(|| format!("Failed to extract dev-app to {}", dest.display()))?;
    Ok(())
}
