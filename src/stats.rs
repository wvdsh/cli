use crate::auth::{AuthManager, AuthSource};
use crate::config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Deserialize, Serialize)]
struct Stat {
    _id: String,
    identifier: String,
    #[serde(rename = "displayName")]
    display_name: String,
}

#[derive(Debug, Serialize)]
struct DeleteOutput<'a> {
    success: bool,
    #[serde(rename = "gameId")]
    game_id: &'a str,
    #[serde(rename = "statId")]
    stat_id: &'a str,
}

fn require_api_key() -> Result<String> {
    let auth_manager = AuthManager::new()?;
    let auth_info = auth_manager.get_auth_info();
    match auth_info.source {
        AuthSource::None => {
            anyhow::bail!("Not authenticated. Run `wavedash auth login` first.")
        }
        _ => Ok(auth_info.api_key.unwrap()),
    }
}

pub async fn handle_stat_create(
    game_id: &str,
    identifier: &str,
    name: &str,
    json_output: bool,
) -> Result<()> {
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
    if json_output {
        println!("{}", serde_json::to_string_pretty(&stat)?);
        return Ok(());
    }
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
    json_output: bool,
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

    let resp = config::check_api_response(resp).await?;
    let stat: Stat = resp.json().await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&stat)?);
        return Ok(());
    }
    println!("✓ Updated stat {}", stat_id);
    Ok(())
}

pub async fn handle_stat_delete(
    game_id: &str,
    stat_id: &str,
    force: bool,
    json_output: bool,
) -> Result<()> {
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
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&DeleteOutput {
                success: true,
                game_id,
                stat_id,
            })?
        );
        return Ok(());
    }
    println!("✓ Deleted stat {}", stat_id);
    Ok(())
}
