use crate::auth::AuthManager;
use crate::config::{self, WavedashConfig};
use anyhow::Result;
use colored::Colorize;
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
    #[serde(rename = "notifyPlayers")]
    notify_players: bool,
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

pub struct PublishArgs {
    pub config_path: PathBuf,
    pub build_id: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub fixed: Vec<String>,
    pub adjusted: Vec<String>,
    pub notify_players: bool,
    pub yes: bool,
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
        notify_players,
        yes,
    } = args;

    let notes = build_release_notes(title, summary, added, removed, fixed, adjusted);
    if notify_players
        && !notes
            .as_ref()
            .is_some_and(|notes| notes.summary.is_some() || notes.changes.is_some())
    {
        anyhow::bail!(
            "Add --summary or at least one patch note (--added, --removed, --fixed, or --adjusted) to notify players."
        );
    }

    let wavedash_config = WavedashConfig::load(&config_path)?;
    let game_id = wavedash_config.game_id()?;

    let auth_manager = AuthManager::new()?;
    let api_key = auth_manager
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wavedash auth login' first."))?;

    if !yes {
        if crate::is_non_interactive() {
            anyhow::bail!(
                "Refusing to publish without confirmation.\n\
                 Re-run with --yes (alias --force / -y) to proceed non-interactively."
            );
        }

        println!(
            "{} This will make build {} live for players of game {}.",
            "Warning:".yellow().bold(),
            build_id.bold(),
            game_id.bold()
        );
        if notify_players {
            println!("Players will be notified of this update.");
        }
        let confirmed = cliclack::confirm("Are you sure you want to continue?")
            .initial_value(false)
            .interact()?;
        if !confirmed {
            println!("Aborted. Nothing was published.");
            return Ok(());
        }
    }

    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/builds/{}/publish",
        api_host, game_id, build_id
    );

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&PublishRequest {
            notes,
            notify_players,
        })
        .send()
        .await?;

    let response = config::check_api_response(response).await?;
    let result: PublishResponse = response.json().await?;

    let site_host = config::get("open_browser_website_host")?;
    println!("✓ Published build {}", build_id);
    println!("Release ID: {}", result.release_id);
    println!("View at: {}/games/{}", site_host, result.game_slug);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_request_sends_notification_flag_and_trimmed_notes() {
        let request = PublishRequest {
            notify_players: true,
            notes: build_release_notes(
                None,
                Some("  New levels  ".into()),
                vec![],
                vec![],
                vec!["  Fixed a crash  ".into()],
                vec![],
            ),
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "notifyPlayers": true,
                "notes": {
                    "summary": "New levels",
                    "changes": [{ "kind": "fixed", "text": "Fixed a crash" }]
                }
            })
        );
    }

    #[tokio::test]
    async fn notify_players_rejects_title_only_or_blank_notes_before_loading_config() {
        for summary in [None, Some(" \n\t ".into())] {
            let error = handle_publish(PublishArgs {
                config_path: PathBuf::from("missing-config.toml"),
                build_id: "build-id".into(),
                title: Some("A title".into()),
                summary,
                added: vec!["   ".into()],
                removed: vec![],
                fixed: vec![],
                adjusted: vec![],
                notify_players: true,
                yes: true,
            })
            .await
            .unwrap_err();
            assert!(error
                .to_string()
                .contains("Add --summary or at least one patch note"));
        }
    }

    #[tokio::test]
    async fn publish_cooldown_error_preserves_server_message() {
        let message = "Players can only be notified once every 24 hours for this game. Publish without notifying, or try again later.";
        let response = axum::http::Response::builder()
            .status(400)
            .body(serde_json::json!({ "error": message, "code": "invalid_operation" }).to_string())
            .unwrap();
        let error = config::check_api_response(response.into())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), message);
    }
}
