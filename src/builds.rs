use crate::auth::AuthManager;
use crate::config::{self, WavedashConfig};
use crate::file_staging::FileStaging;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[path = "uploader.rs"]
mod uploader;

use uploader::{R2Config, R2Uploader};

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
    org: &str,
    game: &str,
    environment: &str,
    engine: &str,
    engine_version: &str,
    build_version: Option<&str>,
    entrypoint: Option<&str>,
    api_key: &str,
) -> Result<TempCredsResponse> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    let url = format!(
        "{}/api/organizations/{}/games/{}/environments/{}/builds/create-temp-r2-creds",
        api_host, org, game, environment
    );

    let mut request_body = serde_json::json!({
        "engine": engine,
        "engineVersion": engine_version
    });

    // Add build version if provided (semantic version: major.minor.patch)
    if let Some(v) = build_version {
        request_body["version"] = serde_json::json!(v);
    }

    // Add entrypoint if provided
    if let Some(ep) = entrypoint {
        request_body["entrypoint"] = serde_json::json!(ep);
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

async fn notify_upload_complete(
    org: &str,
    game: &str,
    environment: &str,
    build_id: &str,
    api_key: &str,
) -> Result<()> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    let url = format!(
        "{}/api/organizations/{}/games/{}/environments/{}/builds/{}/upload-completed",
        api_host, org, game, environment, build_id
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

    Ok(())
}

pub async fn handle_build_push(config_path: PathBuf, verbose: bool) -> Result<()> {
    // Load wavedash.toml config
    let wavedash_config = WavedashConfig::load(&config_path)?;

    // Check authentication
    let auth_manager = AuthManager::new()?;
    let api_key = auth_manager
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wvdsh auth login' first."))?;

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

    // Get temporary R2 credentials
    let engine_kind = wavedash_config.engine_type()?;
    let creds = get_temp_credentials(
        &wavedash_config.org,
        &wavedash_config.game,
        wavedash_config.environment.as_str(),
        engine_kind.as_config_key(),
        wavedash_config.engine_version()?,
        wavedash_config.get_build_version(),
        wavedash_config.entrypoint(),
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

    // Copy necessary files to upload directory
    let staging = FileStaging::prepare(&config_path, config_dir, &upload_dir, &wavedash_config)?;

    // Initialize uploader and upload
    let uploader = R2Uploader::new(&r2_config, &creds.bucket_name)?;
    uploader
        .upload_directory(upload_dir.as_path(), &creds.r2_key_prefix, verbose)
        .await?;

    // Clean up any temporary files
    staging.cleanup();

    // Notify the server that upload is complete
    notify_upload_complete(
        &wavedash_config.org,
        &wavedash_config.game,
        wavedash_config.environment.as_str(),
        &creds.game_build_id,
        &api_key,
    )
    .await?;

    Ok(())
}
