use std::env;
use std::path::PathBuf;

use anyhow::Result;
use futures::StreamExt;
use serde::Deserialize;
use tokio::signal;

use crate::auth::AuthManager;
use crate::config::{self, EngineKind, WavedashConfig};
use crate::file_staging::FileStaging;

mod browser;
mod entrypoint;
mod interceptor;

use entrypoint::{fetch_entrypoint_params, locate_html_entrypoint};

#[derive(Debug, Deserialize)]
struct CreateLocalBuildResponse {
    uuid: String,
    #[serde(rename = "gameSlug")]
    game_slug: String,
}

async fn create_local_build(
    game_id: &str,
    engine: &str,
    engine_version: &str,
    entrypoint: Option<&str>,
    entrypoint_params: Option<&serde_json::Value>,
    api_key: &str,
) -> Result<CreateLocalBuildResponse> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    let url = format!(
        "{}/api/games/{}/builds/create-local",
        api_host, game_id
    );

    let mut request_body = serde_json::json!({
        "engine": engine,
        "engineVersion": engine_version
    });

    if let Some(ep) = entrypoint {
        request_body["entrypoint"] = serde_json::json!(ep);
    }

    if let Some(ep_params) = entrypoint_params {
        request_body["entrypointParams"] = ep_params.clone();
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
        anyhow::bail!("Failed to create local build: {}", error_text);
    }

    let result: CreateLocalBuildResponse = response.json().await?;
    Ok(result)
}

const DEFAULT_CONFIG: &str = "./wavedash.toml";

/// Virtual origin for the LNA connection test. No real server listens here —
/// CDP intercepts the HEAD request and returns 200 OK.
const VIRTUAL_LOCAL_ORIGIN: &str = "https://localhost:1";

pub async fn handle_dev(config_path: Option<PathBuf>, verbose: bool, _no_open: bool) -> Result<()> {
    // Check authentication
    let auth_manager = AuthManager::new()?;
    let api_key = auth_manager
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wavedash auth login' first."))?;

    let config_path = config_path.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
    if !config_path.exists() {
        anyhow::bail!(
            "Unable to find config file at {}. Pass --config if it's located elsewhere.",
            config_path.display()
        );
    }

    let wavedash_config = WavedashConfig::load(&config_path)?;
    let config_dir = config_parent_dir(&config_path)?;
    let upload_dir = config_dir.join(&wavedash_config.upload_dir);

    if !upload_dir.exists() || !upload_dir.is_dir() {
        anyhow::bail!(
            "Upload directory does not exist or is not a directory: {}",
            upload_dir.display()
        );
    }

    let engine_kind = wavedash_config.engine_type()?;
    let engine_label = engine_kind.as_label();

    let entrypoint = if matches!(engine_kind, EngineKind::Custom) {
        Some(
            wavedash_config
                .entrypoint()
                .ok_or_else(|| anyhow::anyhow!("Engine config requires an entrypoint"))?
                .to_string(),
        )
    } else {
        None
    };

    // Validate required files exist in upload directory
    FileStaging::prepare(&upload_dir, &wavedash_config)?;

    let html_entrypoint = locate_html_entrypoint(&upload_dir);
    let entrypoint_params = match engine_kind {
        EngineKind::Godot | EngineKind::Unity => {
            let html_path = html_entrypoint.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "No HTML file found in upload_dir; required for {} builds",
                    engine_label
                )
            })?;
            Some(fetch_entrypoint_params(engine_label, html_path).await?)
        }
        EngineKind::Custom => None,
        EngineKind::JsDos | EngineKind::Ruffle => wavedash_config.executable_entrypoint_params(),
    };

    // Create a new local build via the API
    println!("Creating local build...");
    let local_build = create_local_build(
        &wavedash_config.game_id,
        engine_label,
        wavedash_config.engine_version()?,
        entrypoint.as_deref(),
        entrypoint_params.as_ref(),
        &api_key,
    )
    .await?;

    // Ensure Chrome for Testing is downloaded
    let chrome_path = browser::ensure_chrome_installed().await?;

    // Launch Chrome
    println!("Launching Chrome...");
    let (mut chrome_browser, mut handler) = browser::launch_chrome(&chrome_path).await?;

    // Spawn the CDP handler loop (required for chromiumoxide to process events)
    let handler_task = tokio::spawn(async move {
        while handler.next().await.is_some() {}
    });

    // Open a page and set up request interception
    let page = chrome_browser
        .new_page("about:blank")
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create browser page: {e}"))?;

    let site_host = config::get("open_browser_website_host")?;
    let mainsite_host = site_host
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let game_subdomain = format!(
        "{}.local.{}",
        local_build.game_slug, mainsite_host
    );

    interceptor::setup_interception(
        &page,
        &upload_dir,
        VIRTUAL_LOCAL_ORIGIN,
        &game_subdomain,
    )
    .await?;

    // Build playtest URL — navigate directly (skip permission-grant redirect)
    let playtest_url = format!(
        "{}/playtest/{}/{}?localorigin={}",
        site_host,
        local_build.game_slug,
        local_build.uuid,
        urlencoding::encode(VIRTUAL_LOCAL_ORIGIN),
    );

    page.goto(&playtest_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to navigate to playtest page: {e}"))?;

    println!("--------------------------------");
    println!("Game running at:\n{}", playtest_url);
    println!("--------------------------------");

    if verbose {
        println!("Verbose logging enabled. CDP interception active.");
    }

    // Wait for Ctrl+C
    shutdown_signal().await;

    // Clean shutdown
    let _ = chrome_browser.close().await;
    handler_task.abort();

    Ok(())
}

fn config_parent_dir(config_path: &PathBuf) -> Result<PathBuf> {
    if let Some(parent) = config_path.parent() {
        if parent.as_os_str().is_empty() {
            return Ok(env::current_dir()?);
        }
        return Ok(parent.to_path_buf());
    }
    Ok(env::current_dir()?)
}

async fn shutdown_signal() {
    if signal::ctrl_c().await.is_ok() {
        println!("\nReceived Ctrl+C, shutting down...");
    }
}
