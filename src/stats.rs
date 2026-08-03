use crate::auth::require_api_key;
use crate::config;
use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct Stat {
    _id: String,
    identifier: String,
    #[serde(rename = "displayName")]
    display_name: String,
}

pub async fn handle_stat_create(game_id: &str, identifier: &str, name: &str) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/stats", api_host, game_id);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "identifier": identifier,
            "displayName": name,
        }))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let stat: Stat = resp.json().await?;
    println!(
        "✓ Created stat \"{}\" (id: {}, identifier: {})",
        stat.display_name, stat._id, stat.identifier
    );
    Ok(())
}

pub async fn handle_stat_update(
    game_id: &str,
    stat_id: &str,
    identifier: &str,
    name: &str,
) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/stats/{}", api_host, game_id, stat_id);

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "identifier": identifier,
            "displayName": name,
        }))
        .send()
        .await?;

    config::check_api_response(resp).await?;
    println!("✓ Updated stat {}", stat_id);
    Ok(())
}

pub async fn handle_stat_delete(game_id: &str, stat_id: &str, force: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/stats/{}?force={}",
        api_host, game_id, stat_id, force
    );

    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    config::check_api_response(resp).await?;
    println!("✓ Deleted stat {}", stat_id);
    Ok(())
}
