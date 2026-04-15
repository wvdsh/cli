use crate::auth::AuthManager;
use crate::config::{self, WavedashConfig};
use crate::file_staging::FileStaging;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[path = "uploader.rs"]
mod uploader;

use uploader::{scan_directory, R2Config, R2Uploader};

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    error: String,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[allow(dead_code)]
    code: String,
    message: String,
}

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

async fn get_temp_credentials(
    game_id: &str,
    engine: Option<&str>,
    engine_version: Option<&str>,
    entrypoint: Option<&str>,
    entrypoint_params: Option<serde_json::Value>,
    message: Option<&str>,
    build_size_bytes: u64,
    api_key: &str,
) -> Result<TempCredsResponse> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    let url = format!(
        "{}/api/games/{}/builds/create-temp-r2-creds",
        api_host, game_id
    );

    let mut request_body = serde_json::json!({
        "buildSizeBytes": build_size_bytes
    });

    if let Some(eng) = engine {
        request_body["engine"] = serde_json::json!(eng);
    }

    if let Some(ver) = engine_version {
        request_body["engineVersion"] = serde_json::json!(ver);
    }

    if let Some(ep) = entrypoint {
        request_body["entrypoint"] = serde_json::json!(ep);
    }

    // Add entrypointParams if provided (for JSDOS/Ruffle)
    if let Some(ep_params) = entrypoint_params {
        request_body["entrypointParams"] = ep_params;
    }

    // Add build message if provided
    if let Some(msg) = message {
        request_body["buildMessage"] = serde_json::json!(msg);
    }

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;

        // Try to parse the API error response
        if let Ok(api_error_response) = serde_json::from_str::<ApiErrorResponse>(&error_text) {
            // Try to parse the nested error JSON
            if let Ok(api_error) = serde_json::from_str::<ApiError>(&api_error_response.error) {
                anyhow::bail!("{}", api_error.message);
            }
        }

        // Fallback to raw error text if parsing fails
        anyhow::bail!("API request failed: {}", error_text);
    }

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

    if !response.status().is_success() {
        let error_text = response.text().await?;

        // Try to parse the API error response
        if let Ok(api_error_response) = serde_json::from_str::<ApiErrorResponse>(&error_text) {
            // Try to parse the nested error JSON
            if let Ok(api_error) = serde_json::from_str::<ApiError>(&api_error_response.error) {
                anyhow::bail!("{}", api_error.message);
            }
        }

        // Fallback to raw error text if parsing fails
        anyhow::bail!("API request failed: {}", error_text);
    }

    let result: UploadCompleteResponse = response.json().await?;
    Ok(result)
}

pub async fn handle_build_push(config_path: PathBuf, verbose: bool, message: Option<String>) -> Result<()> {
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
    let upload_dir = config_dir.join(&wavedash_config.upload_dir);

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
        &wavedash_config.game_id,
        engine_kind.map(|e| e.as_label()),
        wavedash_config.engine_version(),
        wavedash_config.entrypoint(),
        wavedash_config.executable_entrypoint_params(),
        message.as_deref(),
        total_bytes,
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
    let result = notify_upload_complete(
        &wavedash_config.game_id,
        &creds.game_build_id,
        &api_key,
    )
    .await?;

    // Print the play URL
    let site_host = config::get("open_browser_website_host")?;
    let play_url = format!(
        "{}/playtest/{}/{}",
        site_host, result.game_slug, creds.uuid
    );
    println!("\n▶ Play at: {}", play_url);

    Ok(())
}
