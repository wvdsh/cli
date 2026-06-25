use crate::auth::AuthManager;
use crate::config::{self, WavedashConfig};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize)]
struct ReleaseChange {
    kind: &'static str,
    text: String,
}

#[derive(Debug, Serialize)]
struct ReleaseNotes {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Vec<ReleaseChange>>,
}

#[derive(Debug, Serialize)]
struct PublishRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<ReleaseNotes>,
}

#[derive(Debug, Deserialize)]
struct PublishResponse {
    #[serde(rename = "releaseId")]
    release_id: String,
    #[serde(rename = "gameSlug")]
    game_slug: String,
}

#[derive(Debug, Serialize)]
struct PublishOutput {
    #[serde(rename = "buildId")]
    build_id: String,
    #[serde(rename = "releaseId")]
    release_id: String,
    #[serde(rename = "gameSlug")]
    game_slug: String,
    url: String,
}

pub struct PublishArgs {
    pub config_path: PathBuf,
    pub build_id: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub fixed: Vec<String>,
    pub adjusted: Vec<String>,
    pub json: bool,
}

fn trim_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn append_changes(changes: &mut Vec<ReleaseChange>, kind: &'static str, items: Vec<String>) {
    changes.extend(
        items
            .into_iter()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .map(|text| ReleaseChange { kind, text }),
    );
}

fn build_release_notes(
    title: Option<String>,
    summary: Option<String>,
    added: Vec<String>,
    removed: Vec<String>,
    fixed: Vec<String>,
    adjusted: Vec<String>,
) -> Option<ReleaseNotes> {
    let title = trim_optional(title);
    let summary = trim_optional(summary);

    let mut changes = Vec::new();
    append_changes(&mut changes, "added", added);
    append_changes(&mut changes, "removed", removed);
    append_changes(&mut changes, "fixed", fixed);
    append_changes(&mut changes, "adjusted", adjusted);

    let changes = if changes.is_empty() {
        None
    } else {
        Some(changes)
    };

    if title.is_none() && summary.is_none() && changes.is_none() {
        return None;
    }

    Some(ReleaseNotes {
        title,
        summary,
        changes,
    })
}

pub async fn handle_publish(args: PublishArgs) -> Result<()> {
    let PublishArgs {
        config_path,
        build_id,
        title,
        summary,
        added,
        removed,
        fixed,
        adjusted,
        json,
    } = args;

    let wavedash_config = WavedashConfig::load(&config_path)?;

    let auth_manager = AuthManager::new()?;
    let api_key = auth_manager
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wavedash auth login' first."))?;

    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/builds/{}/publish",
        api_host, wavedash_config.game_id, build_id
    );

    let notes = build_release_notes(title, summary, added, removed, fixed, adjusted);

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&PublishRequest { notes })
        .send()
        .await?;

    let response = config::check_api_response(response).await?;
    let result: PublishResponse = response.json().await?;

    let site_host = config::get("open_browser_website_host")?;
    let url = format!("{}/games/{}", site_host, result.game_slug);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&PublishOutput {
                build_id,
                release_id: result.release_id,
                game_slug: result.game_slug,
                url,
            })?
        );
        return Ok(());
    }
    println!("✓ Published build {}", build_id);
    println!("Release ID: {}", result.release_id);
    println!("View at: {}", url);

    Ok(())
}
