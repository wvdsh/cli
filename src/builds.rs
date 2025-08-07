use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::auth::AuthManager;
use crate::config;
use crate::rclone::{RcloneManager, R2Config};

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

fn parse_target(target: &str) -> Result<(String, String, String)> {
    let parts: Vec<&str> = target.split(':').collect();
    if parts.len() != 2 {
        anyhow::bail!("Invalid target format. Expected: org_slug/game_slug:branch_name");
    }

    let branch_name = parts[1].to_string();
    let org_game: Vec<&str> = parts[0].split('/').collect();
    if org_game.len() != 2 {
        anyhow::bail!("Invalid target format. Expected: org_slug/game_slug:branch_name");
    }

    Ok((org_game[0].to_string(), org_game[1].to_string(), branch_name))
}

async fn get_temp_credentials(
    org_slug: &str,
    game_slug: &str,
    branch_name: &str,
    engine: &str,
    engine_version: &str,
    api_key: &str,
) -> Result<TempCredsResponse> {
    let client = reqwest::Client::new();
    let api_host = config::get("api_host")?;
    
    let url = format!(
        "{}/api/organizations/{}/games/{}/builds/{}/create-temp-r2-creds",
        api_host, org_slug, game_slug, branch_name
    );

    let request_body = serde_json::json!({
        "engine": engine,
        "engineVersion": engine_version
    });

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
    org_slug: &str,
    game_slug: &str,
    branch_name: &str,
    build_id: &str,
    api_key: &str,
) -> Result<()> {
    let client = reqwest::Client::new();
    let api_host = config::get("api_host")?;
    
    let url = format!(
        "{}/api/organizations/{}/games/{}/builds/{}/builds/{}/upload-completed",
        api_host, org_slug, game_slug, branch_name, build_id
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

pub async fn handle_build_push(
    target: String,
    engine: String,
    engine_version: String,
    source: PathBuf,
    verbose: bool,
) -> Result<()> {
    // Check authentication
    let auth_manager = AuthManager::new()?;
    let api_key = auth_manager.get_api_key()
        .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wvdsh auth login' first."))?;

    // Parse target
    let (org_slug, game_slug, branch_name) = parse_target(&target)?;
    
    // Verify source directory exists
    if !source.exists() {
        anyhow::bail!("Source directory does not exist: {}", source.display());
    }
    if !source.is_dir() {
        anyhow::bail!("Source must be a directory: {}", source.display());
    }


    // Get temporary R2 credentials
    let creds = get_temp_credentials(&org_slug, &game_slug, &branch_name, &engine, &engine_version, &api_key).await?;
    
    // Create R2 config for rclone
    let r2_config = R2Config {
        access_key_id: creds.credentials.access_key_id,
        secret_access_key: creds.credentials.secret_access_key,
        session_token: creds.credentials.session_token,
        endpoint: creds.endpoint,
    };

    // Initialize rclone and upload
    let mut rclone = RcloneManager::new(verbose)?;
    rclone.sync_to_r2(
        source.to_str().unwrap(),
        &creds.bucket_name,
        &creds.r2_key_prefix,
        &r2_config,
        verbose,
    ).await?;

    // Notify the server that upload is complete
    notify_upload_complete(&org_slug, &game_slug, &branch_name, &creds.game_build_id, &api_key).await?;

    Ok(())
}