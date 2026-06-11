use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::auth::{generate_state, AuthManager};
use crate::config::{self, EngineKind, WavedashConfig};
use crate::file_staging::FileStaging;

mod server;

#[derive(Debug, Deserialize)]
struct CreateLocalBuildResponse {
    uuid: String,
}

/// Nothing is uploaded — the build row exists because gameplay JWTs are
/// build-scoped.
async fn create_local_build(
    game_id: &str,
    engine: Option<&str>,
    engine_version: Option<&str>,
    entrypoint: Option<&str>,
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

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await?;

    let response = config::check_api_response(response).await?;
    Ok(response.json().await?)
}

const DEFAULT_CONFIG: &str = "./wavedash.toml";

pub async fn handle_dev(config_path: Option<PathBuf>, verbose: bool) -> Result<()> {
    let auth_manager = AuthManager::new()?;
    let api_key = auth_manager
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wavedash auth login' first."))?;

    // Best-effort sweep of the retired Electron dev-app cache.
    if let Ok(dir) = config::wavedash_dir() {
        for stale in ["dev-app", "dev-app-profile"] {
            let _ = std::fs::remove_dir_all(dir.join(stale));
        }
    }

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
    let upload_dir = upload_dir.canonicalize().with_context(|| {
        format!("Failed to canonicalize upload_dir: {}", upload_dir.display())
    })?;

    FileStaging::prepare(&upload_dir, &wavedash_config)?;

    // Entry rule mirrors prod (play's embed.tsx): engine builds boot through
    // play's real default entrypoint and ignore their exported index.html;
    // RenPy counts as custom-HTML.
    let engine_kind = wavedash_config.engine_type()?;
    let entrypoint = wavedash_config.entrypoint().map(String::from);
    let api_host = config::get("api_host")?;
    let client = config::create_http_client()?;

    let mut entry = String::new();
    let mut engine_entry = None;
    match (&entrypoint, engine_kind) {
        (Some(e), _) => entry = e.clone(),
        (
            None,
            Some(
                kind @ (EngineKind::Godot
                | EngineKind::Unity
                | EngineKind::JsDos
                | EngineKind::Ruffle),
            ),
        ) => {
            engine_entry = Some(
                resolve_engine_entry(kind, &wavedash_config, &upload_dir, &api_host, &client)
                    .await?,
            );
        }
        (None, _) => {
            let html = locate_html_entrypoint(&upload_dir).ok_or_else(|| {
                anyhow::anyhow!("No HTML file found in {}", upload_dir.display())
            })?;
            entry = html
                .strip_prefix(&upload_dir)
                .unwrap_or(&html)
                .to_string_lossy()
                .replace('\\', "/");
        }
    }
    let local_build = create_local_build(
        &wavedash_config.game_id,
        engine_kind.map(|e| e.as_label()),
        wavedash_config.engine_version(),
        entrypoint.as_deref(),
        &api_key,
    )
    .await?;

    // Bind first so the auth callback URL knows the port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let state_token = generate_state();

    let site_host = config::get("open_browser_website_host")?;
    let callback_uri = format!("http://localhost:{}/__wavedash/callback", port);
    let auth_url = format!(
        "{}/auth/dev?build_uuid={}&callback_uri={}&state={}",
        site_host,
        urlencoding::encode(&local_build.uuid),
        urlencoding::encode(&callback_uri),
        urlencoding::encode(&state_token),
    );

    let local_url = format!("http://localhost:{}", port);
    println!("wavedash dev → {}", local_url);
    if verbose {
        println!("  serving {}", upload_dir.display());
        println!("  auth url: {}/auth/dev", site_host);
        println!("    build_uuid:   {}", local_build.uuid);
        println!("    callback_uri: {}", callback_uri);
        println!("    state:        {}", state_token);
    }
    open::that(&local_url)?;

    server::run(
        listener,
        server::ServeConfig {
            upload_dir,
            entry,
            verbose,
            build_uuid: local_build.uuid,
            state_token,
            auth_url,
            api_key,
            api_host,
            client,
            engine_entry,
            jwks: tokio::sync::OnceCell::new(),
        },
    )
    .await
}

/// Resolve play's default-entrypoint URL and the `entrypointParams` it reads.
/// Unity/Godot params come from the same backend parsers prod runs at upload,
/// so local boot config matches wavedash.com.
async fn resolve_engine_entry(
    kind: EngineKind,
    wavedash_config: &WavedashConfig,
    upload_dir: &Path,
    api_host: &str,
    client: &reqwest::Client,
) -> Result<server::EngineEntry> {
    let version = wavedash_config.engine_version().ok_or_else(|| {
        anyhow::anyhow!("{} requires a version in wavedash.toml", kind.as_label())
    })?;
    // Resolution lives play-side (shared with prod's embed.js), so new engine
    // versions work the moment play deploys. Unresolvable versions 404 when
    // the shell loads the script and the gate shows the resolver's message.
    let entrypoint_url = format!(
        "{}/default-entrypoint.js?engine={}&engineVersion={}",
        config::get("play_statics_origin")?,
        urlencoding::encode(kind.as_label()),
        urlencoding::encode(version)
    );

    let params = match kind {
        EngineKind::Godot | EngineKind::Unity => {
            let html_path = locate_html_entrypoint(upload_dir).ok_or_else(|| {
                anyhow::anyhow!(
                    "No exported HTML found in {} (needed to derive {} boot params)",
                    upload_dir.display(),
                    kind.as_label()
                )
            })?;
            let html = std::fs::read_to_string(&html_path)?;
            fetch_entrypoint_params(&html, kind, version, api_host, client).await?
        }
        // Params come straight from wavedash.toml, same as `wavedash push`.
        _ => wavedash_config.executable_entrypoint_params().ok_or_else(|| {
            anyhow::anyhow!("Missing executable config in wavedash.toml for {}", kind.as_label())
        })?,
    };

    Ok(server::EngineEntry {
        entrypoint_url,
        params_json: params.to_string(),
    })
}

#[derive(Debug, Deserialize)]
struct EntrypointParamsResponse {
    #[serde(rename = "entrypointParams")]
    entrypoint_params: serde_json::Value,
}

/// POST the exported HTML to `/cli/entrypoint-params` — the same engine boot
/// config extraction prod's upload flow runs.
async fn fetch_entrypoint_params(
    html: &str,
    kind: EngineKind,
    version: &str,
    api_host: &str,
    client: &reqwest::Client,
) -> Result<serde_json::Value> {
    let url = format!("{}/cli/entrypoint-params", api_host);
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "engine": kind.as_label(),
            "engineVersion": version,
            "htmlContent": html,
        }))
        .send()
        .await?;
    let response = config::check_api_response(response).await?;
    Ok(response.json::<EntrypointParamsResponse>().await?.entrypoint_params)
}

fn locate_html_entrypoint(upload_dir: &Path) -> Option<PathBuf> {
    let default_index = upload_dir.join("index.html");
    if default_index.is_file() {
        return Some(default_index);
    }
    WalkDir::new(upload_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
        })
        .map(|e| e.into_path())
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
