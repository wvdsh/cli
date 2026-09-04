use crate::auth::AuthManager;
use crate::config::{self, UploadSource, WavedashConfig};
use crate::file_staging::FileStaging;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::io::IsTerminal;
use std::path::PathBuf;
use tokio::time::{sleep, Duration, Instant};
#[path = "uploader.rs"]
mod uploader;

use uploader::{scan_directory, R2Config, R2Uploader};

const BUILD_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(2);
const BUILD_STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const BUILD_PROCESSING_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_CONSECUTIVE_STATUS_FAILURES: u32 = 3;

#[derive(Debug, Serialize, Deserialize)]
struct R2Credentials {
    #[serde(rename = "accessKeyId")]
    access_key_id: String,
    #[serde(rename = "secretAccessKey")]
    secret_access_key: String,
    #[serde(rename = "sessionToken")]
    session_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct TempCredsResponse {
    #[serde(rename = "gameBuildId")]
    game_build_id: String,
    uuid: String,
    #[serde(rename = "r2KeyPrefix")]
    r2_key_prefix: String,
    #[serde(rename = "bucketName")]
    bucket_name: String,
    credentials: R2Credentials,
    endpoint: String,
    #[serde(rename = "expiresIn")]
    expires_in: u64,
}

/// Build metadata sent with the R2 credential request.
struct BuildUploadInfo<'a> {
    game_id: &'a str,
    engine: Option<&'a str>,
    engine_version: Option<&'a str>,
    entrypoint: Option<&'a str>,
    entrypoint_params: Option<serde_json::Value>,
    message: Option<&'a str>,
    build_size_bytes: u64,
    upload_source: UploadSource,
}

async fn get_temp_credentials(
    info: BuildUploadInfo<'_>,
    api_key: &str,
) -> Result<TempCredsResponse> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    let url = format!(
        "{}/api/games/{}/builds/create-temp-r2-creds",
        api_host, info.game_id
    );

    let mut request_body = serde_json::json!({
        "buildSizeBytes": info.build_size_bytes,
        "uploadSource": info.upload_source.as_label(),
    });

    if let Some(eng) = info.engine {
        request_body["engine"] = serde_json::json!(eng);
    }

    if let Some(ver) = info.engine_version {
        request_body["engineVersion"] = serde_json::json!(ver);
    }

    if let Some(ep) = info.entrypoint {
        request_body["entrypoint"] = serde_json::json!(ep);
    }

    // Add entrypointParams if provided (for JSDOS/Ruffle/Ren'Py)
    if let Some(ep_params) = info.entrypoint_params {
        request_body["entrypointParams"] = ep_params;
    }

    // Add build message if provided
    if let Some(msg) = info.message {
        request_body["buildMessage"] = serde_json::json!(msg);
    }

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await?;

    let response = config::check_api_response(response).await?;

    let creds: TempCredsResponse = response.json().await?;
    Ok(creds)
}

#[derive(Debug, Deserialize)]
struct UploadCompleteResponse {
    #[serde(rename = "gameSlug")]
    game_slug: String,
}

async fn notify_upload_complete(
    game_id: &str,
    build_id: &str,
    api_key: &str,
) -> Result<UploadCompleteResponse> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    let url = format!(
        "{}/api/games/{}/builds/{}/upload-completed",
        api_host, game_id, build_id
    );

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .send()
        .await?;

    let response = config::check_api_response(response).await?;

    let result: UploadCompleteResponse = response.json().await?;
    Ok(result)
}

#[derive(Debug, Deserialize)]
struct BuildStatusResponse {
    status: String,
    #[serde(rename = "processingError")]
    processing_error: Option<String>,
}

async fn get_build_status(
    client: &reqwest::Client,
    game_id: &str,
    build_id: &str,
    api_key: &str,
) -> Result<BuildStatusResponse> {
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/builds/{}", api_host, game_id, build_id);

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(BUILD_STATUS_REQUEST_TIMEOUT)
        .send()
        .await?;

    let response = config::check_api_response(response).await?;

    let result: BuildStatusResponse = response.json().await?;
    Ok(result)
}

async fn wait_for_build_processing(game_id: &str, build_id: &str, api_key: &str) -> Result<()> {
    let spinner = if std::io::stderr().is_terminal() {
        let pb = ProgressBar::new_spinner();
        pb.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
        pb.set_message("Processing build...");
        pb.enable_steady_tick(Duration::from_millis(100));
        Some(pb)
    } else {
        println!("Processing build...");
        None
    };

    let outcome = poll_until_processed(game_id, build_id, api_key).await;

    match (&spinner, &outcome) {
        (Some(pb), Ok(())) => pb.finish_with_message("✓ Build processed successfully!"),
        (Some(pb), Err(_)) => pb.finish_and_clear(),
        (None, Ok(())) => println!("Build processed successfully."),
        (None, Err(_)) => {}
    }
    outcome
}

async fn poll_until_processed(game_id: &str, build_id: &str, api_key: &str) -> Result<()> {
    let client = config::create_http_client()?;
    let deadline = Instant::now() + BUILD_PROCESSING_TIMEOUT;
    let mut consecutive_failures = 0;

    loop {
        match get_build_status(&client, game_id, build_id, api_key).await {
            Ok(build) => {
                consecutive_failures = 0;
                match build.status.as_str() {
                    "COMPLETED" => return Ok(()),
                    "FAILED" => match build.processing_error {
                        Some(message) => anyhow::bail!("Build processing failed: {}", message),
                        None => anyhow::bail!("Build processing failed."),
                    },
                    "CANCELLED" => {
                        anyhow::bail!("Build was cancelled before processing finished.")
                    }
                    _ => {}
                }
            }
            Err(error) => {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_STATUS_FAILURES {
                    return Err(error.context(
                        "Could not check the build's processing status. The play link above will load the game once it finishes.",
                    ));
                }
            }
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "Build is still processing after {} minutes. The play link above will load the game once it finishes.",
                BUILD_PROCESSING_TIMEOUT.as_secs() / 60
            );
        }
        sleep(BUILD_STATUS_POLL_INTERVAL).await;
    }
}

pub async fn handle_build_push(
    config_path: PathBuf,
    verbose: bool,
    message: Option<String>,
    upload_source: UploadSource,
    no_wait: bool,
) -> Result<()> {
    // Load wavedash.toml config
    let wavedash_config = WavedashConfig::load(&config_path)?;

    // Check authentication
    let auth_manager = AuthManager::new()?;
    let api_key = auth_manager
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wavedash auth login' first."))?;

    // Resolve upload_dir relative to the config file's directory
    let config_dir = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Config file has no parent directory"))?;
    let upload_dir = config_dir.join(wavedash_config.upload_dir()?);

    // Verify source directory exists
    if !upload_dir.exists() {
        anyhow::bail!("Source directory does not exist: {}", upload_dir.display());
    }
    if !upload_dir.is_dir() {
        anyhow::bail!("Source must be a directory: {}", upload_dir.display());
    }

    // Validate required files exist in upload directory
    FileStaging::prepare(&upload_dir, &wavedash_config)?;

    // Scan directory to get file list and total size before requesting credentials
    let (scanned_files, total_bytes) = scan_directory(&upload_dir)?;
    if scanned_files.is_empty() {
        anyhow::bail!("No files found in {}", upload_dir.display());
    }

    // Get temporary R2 credentials (includes build size)
    let engine_kind = wavedash_config.engine_type()?;
    let creds = get_temp_credentials(
        BuildUploadInfo {
            game_id: wavedash_config.game_id()?,
            engine: engine_kind.map(|e| e.as_label()),
            engine_version: wavedash_config.engine_version()?,
            entrypoint: wavedash_config.entrypoint()?,
            entrypoint_params: wavedash_config.executable_entrypoint_params()?,
            message: message.as_deref(),
            build_size_bytes: total_bytes,
            upload_source,
        },
        &api_key,
    )
    .await?;

    // Create R2 config for uploader
    let r2_config = R2Config {
        access_key_id: creds.credentials.access_key_id,
        secret_access_key: creds.credentials.secret_access_key,
        session_token: creds.credentials.session_token,
        endpoint: creds.endpoint,
    };

    // Initialize uploader and upload using pre-scanned files
    let uploader = R2Uploader::new(&r2_config, &creds.bucket_name)?;
    uploader
        .upload_directory_from_scan(&scanned_files, total_bytes, &creds.r2_key_prefix, verbose)
        .await?;

    // Notify the server that upload is complete
    let result =
        notify_upload_complete(wavedash_config.game_id()?, &creds.game_build_id, &api_key).await?;

    let site_host = config::get("open_browser_website_host")?;
    let play_url = format!("{}/playtest/{}/{}", site_host, result.game_slug, creds.uuid);
    println!("\nBuild ID: {}", creds.game_build_id);
    println!("▶ Play at: {}", play_url);

    if !no_wait {
        wait_for_build_processing(wavedash_config.game_id()?, &creds.game_build_id, &api_key)
            .await?;
    }

    Ok(())
}
