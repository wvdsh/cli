use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::auth::AuthManager;
use crate::config::{self, EngineKind, WavedashConfig};
use crate::file_staging::FileStaging;

mod dev_app;
pub(crate) mod entrypoint;
mod launcher;

use dev_app::{ensure_dev_app, user_data_dir};
use entrypoint::{fetch_entrypoint_params, locate_html_entrypoint};
use launcher::{run_dev_app, DevAppConfig};

#[derive(Debug, Deserialize)]
struct CreateLocalBuildResponse {
    uuid: String,
    #[serde(rename = "gameSlug")]
    game_slug: String,
}

async fn create_local_build(
    game_id: &str,
    engine: Option<&str>,
    engine_version: Option<&str>,
    entrypoint: Option<&str>,
    entrypoint_params: Option<&serde_json::Value>,
    api_key: &str,
) -> Result<CreateLocalBuildResponse> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    let url = format!("{}/api/games/{}/builds/create-local", api_host, game_id);

    let mut request_body = serde_json::json!({});

    if let Some(eng) = engine {
        request_body["engine"] = serde_json::json!(eng);
    }

    if let Some(ver) = engine_version {
        request_body["engineVersion"] = serde_json::json!(ver);
    }

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

    let response = config::check_api_response(response).await?;
    let result: CreateLocalBuildResponse = response.json().await?;
    Ok(result)
}

const DEFAULT_CONFIG: &str = "./wavedash.toml";

pub async fn handle_dev(config_path: Option<PathBuf>, verbose: bool) -> Result<()> {
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

    // Send an absolute path to the dev-app so it doesn't rely on inheriting the
    // CLI's cwd. With `DEV_APP_DEV_PATH` set, electron is spawned with its cwd
    // pinned to the dev-app source dir, so a relative `./dist` would resolve
    // there (and serve the dev-app's own bundled `main.js`).
    let upload_dir = upload_dir.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize upload_dir: {}",
            upload_dir.display()
        )
    })?;

    let engine_kind = wavedash_config.engine_type()?;

    let entrypoint = wavedash_config.entrypoint().map(|s| s.to_string());

    FileStaging::prepare(&upload_dir, &wavedash_config)?;

    let html_entrypoint = locate_html_entrypoint(&upload_dir);
    let engine_version = wavedash_config.engine_version();
    let entrypoint_params = match engine_kind {
        Some(EngineKind::Godot | EngineKind::Unity | EngineKind::Defold) => {
            let engine_label = engine_kind.unwrap().as_label();
            let html_path = html_entrypoint.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "No HTML file found in upload_dir; required for {} builds",
                    engine_label
                )
            })?;
            let ver = engine_version
                .ok_or_else(|| anyhow::anyhow!("{} engine requires a version", engine_label))?;
            let html_relative_path = html_path
                .strip_prefix(&upload_dir)
                .unwrap_or(html_path)
                .to_string_lossy()
                .replace('\\', "/");
            Some(
                fetch_entrypoint_params(engine_label, ver, html_path, Some(&html_relative_path))
                    .await?,
            )
        }
        Some(EngineKind::JsDos | EngineKind::Ruffle | EngineKind::RenPy) => {
            wavedash_config.executable_entrypoint_params()
        }
        None => None,
    };

    let local_build = create_local_build(
        &wavedash_config.game_id,
        engine_kind.map(|e| e.as_label()),
        engine_version,
        entrypoint.as_deref(),
        entrypoint_params.as_ref(),
        &api_key,
    )
    .await?;

    let site_host = config::get("open_browser_website_host")?;
    let playsite_host = config::get("playsite_host")?;
    let local_host_suffix = format!("local.{}", playsite_host);

    let playtest_url = format!(
        "{}/playtest/{}/{}",
        site_host, local_build.game_slug, local_build.uuid,
    );

    let launch = ensure_dev_app().await?;
    let user_data = user_data_dir()?;
    std::fs::create_dir_all(&user_data)?;

    let dev_app_config = DevAppConfig {
        upload_dir: upload_dir.to_string_lossy().to_string(),
        local_host_suffix,
        playtest_url,
        verbose,
    };

    run_dev_app(launch, &dev_app_config, &user_data.to_string_lossy()).await?;
    Ok(())
}

fn config_parent_dir(config_path: &Path) -> Result<PathBuf> {
    if let Some(parent) = config_path.parent() {
        if parent.as_os_str().is_empty() {
            return Ok(env::current_dir()?);
        }
        return Ok(parent.to_path_buf());
    }
    Ok(env::current_dir()?)
}
