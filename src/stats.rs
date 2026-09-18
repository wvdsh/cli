use crate::auth::require_api_key;
use crate::authority::Authority;
use crate::config;
use anyhow::Result;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Deserialize, Serialize)]
struct Stat {
    id: String,
    identifier: String,
    #[serde(rename = "displayName")]
    display_name: String,
    authority: Authority,
}

#[derive(Debug, Deserialize)]
struct StatResponse {
    stat: Stat,
}

#[derive(Debug, Deserialize)]
struct StatsResponse {
    stats: Vec<Stat>,
}

pub async fn handle_stat_list(game_id: &str, json: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/stats", api_host, game_id);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let data: StatsResponse = resp.json().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&data.stats)?);
        return Ok(());
    }

    if data.stats.is_empty() {
        println!("No stats found.");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("ID"),
            Cell::new("Identifier"),
            Cell::new("Name"),
            Cell::new("Authority"),
        ]);

    for stat in data.stats {
        table.add_row(vec![
            stat.id,
            stat.identifier,
            stat.display_name,
            stat.authority.to_string(),
        ]);
    }

    println!("{table}");
    Ok(())
}

pub async fn handle_stat_create(
    game_id: &str,
    identifier: &str,
    name: &str,
    authority: Option<Authority>,
) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/stats", api_host, game_id);

    let mut body = json!({
        "identifier": identifier,
        "displayName": name,
    });
    if let Some(authority) = authority {
        body["authority"] = json!(authority);
    }

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let stat = resp.json::<StatResponse>().await?.stat;
    println!(
        "✓ Created stat \"{}\" (id: {}, identifier: {})",
        stat.display_name, stat.id, stat.identifier
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_stat_list_response() {
        let response: StatsResponse = serde_json::from_value(json!({
            "stats": [{
                "id": "stat-id",
                "identifier": "WINS",
                "displayName": "Wins",
                "authority": "Server"
            }]
        }))
        .expect("the API response should deserialize");

        let stat = &response.stats[0];
        assert_eq!(stat.id, "stat-id");
        assert_eq!(stat.identifier, "WINS");
        assert_eq!(stat.display_name, "Wins");
        assert_eq!(stat.authority, Authority::Server);
    }

    #[test]
    fn parses_the_stat_create_response() {
        let response: StatResponse = serde_json::from_value(json!({
            "stat": {
                "id": "stat-id",
                "identifier": "WINS",
                "displayName": "Wins",
                "authority": "Client"
            }
        }))
        .expect("the API response should deserialize");

        assert_eq!(response.stat.id, "stat-id");
    }

    #[test]
    fn json_output_uses_api_field_names() {
        let stat = Stat {
            id: "stat-id".to_string(),
            identifier: "WINS".to_string(),
            display_name: "Wins".to_string(),
            authority: Authority::Client,
        };

        let value = serde_json::to_value(stat).expect("stat should serialize");
        assert_eq!(value["id"], "stat-id");
        assert_eq!(value["displayName"], "Wins");
        assert_eq!(value["authority"], "Client");
        assert!(value.get("display_name").is_none());
    }
}
