use crate::api_client::ApiClient;
use crate::config::WavedashConfig;
use crate::stats::{authority_label, AuthorityArg};
use anyhow::Result;
use clap::Subcommand;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum AchievementsCommands {
    /// List all achievements for the game
    List,
    /// Create a new achievement
    Create {
        /// Achievement identifier (letters, numbers, underscores; must start with a letter)
        identifier: String,
        /// Display name
        #[arg(long)]
        display_name: String,
        /// Description
        #[arg(long)]
        description: String,
        /// Image URL
        #[arg(long)]
        image: String,
        /// Authority level
        #[arg(long, value_enum)]
        authority: AuthorityArg,
        /// Points awarded for earning this achievement
        #[arg(long)]
        points: Option<u32>,
        /// Stat identifier to link (for automatic unlocking)
        #[arg(long)]
        stat_identifier: Option<String>,
        /// Stat threshold for automatic unlocking
        #[arg(long)]
        stat_threshold: Option<f64>,
    },
    /// Update an existing achievement
    Update {
        /// Achievement identifier to update
        identifier: String,
        /// New display name
        #[arg(long)]
        display_name: Option<String>,
        /// New description
        #[arg(long)]
        description: Option<String>,
        /// New image URL
        #[arg(long)]
        image: Option<String>,
        /// New authority level
        #[arg(long, value_enum)]
        authority: Option<AuthorityArg>,
        /// New points value
        #[arg(long)]
        points: Option<u32>,
        /// Stat identifier to link (use "none" to unlink)
        #[arg(long)]
        stat_identifier: Option<String>,
        /// New stat threshold
        #[arg(long)]
        stat_threshold: Option<f64>,
    },
    /// Delete an achievement
    Delete {
        /// Achievement identifier to delete
        identifier: String,
    },
}

#[derive(Deserialize)]
struct ListAchievementsResponse {
    achievements: Vec<Achievement>,
}

#[derive(Deserialize)]
struct Achievement {
    identifier: String,
    #[serde(rename = "displayName")]
    display_name: String,
    description: String,
    authority: u64,
    points: Option<u64>,
    #[serde(rename = "statIdentifier")]
    stat_identifier: Option<String>,
    #[serde(rename = "statThreshold")]
    stat_threshold: Option<f64>,
}

#[derive(Deserialize)]
struct SuccessResponse {
    #[allow(dead_code)]
    success: bool,
}

pub async fn handle_achievements(
    config_path: PathBuf,
    command: AchievementsCommands,
) -> Result<()> {
    let wavedash_config = WavedashConfig::load(&config_path)?;
    let game_id = &wavedash_config.game_id;
    let client = ApiClient::new()?;

    match command {
        AchievementsCommands::List => {
            let resp: ListAchievementsResponse = client
                .get(&format!("/games/{}/achievements", game_id))
                .await?;

            if resp.achievements.is_empty() {
                println!("No achievements found.");
                return Ok(());
            }

            // Calculate column widths
            let id_w = resp
                .achievements
                .iter()
                .map(|a| a.identifier.len())
                .max()
                .unwrap_or(0)
                .max(10);
            let name_w = resp
                .achievements
                .iter()
                .map(|a| a.display_name.len())
                .max()
                .unwrap_or(0)
                .max(12);
            let desc_w = resp
                .achievements
                .iter()
                .map(|a| a.description.len())
                .max()
                .unwrap_or(0)
                .clamp(11, 40);

            println!(
                "{:<id_w$}  {:<name_w$}  {:<desc_w$}  {:<9}  {:<6}  LINKED STAT",
                "IDENTIFIER", "DISPLAY NAME", "DESCRIPTION", "AUTHORITY", "POINTS"
            );
            for ach in &resp.achievements {
                let desc_display = if ach.description.len() > 40 {
                    format!("{}...", &ach.description[..37])
                } else {
                    ach.description.clone()
                };
                let points_display = match ach.points {
                    Some(p) => p.to_string(),
                    None => "-".to_string(),
                };
                let stat_display = match (&ach.stat_identifier, ach.stat_threshold) {
                    (Some(id), Some(thresh)) => format!("{} >= {}", id, thresh),
                    (Some(id), None) => id.clone(),
                    _ => "-".to_string(),
                };
                println!(
                    "{:<id_w$}  {:<name_w$}  {:<desc_w$}  {:<9}  {:<6}  {}",
                    ach.identifier,
                    ach.display_name,
                    desc_display,
                    authority_label(ach.authority),
                    points_display,
                    stat_display,
                );
            }
        }
        AchievementsCommands::Create {
            identifier,
            display_name,
            description,
            image,
            authority,
            points,
            stat_identifier,
            stat_threshold,
        } => {
            let mut body = serde_json::json!({
                "identifier": identifier,
                "displayName": display_name,
                "description": description,
                "image": image,
                "authority": authority.as_number(),
            });
            if let Some(p) = points {
                body["points"] = serde_json::json!(p);
            }
            if let Some(si) = &stat_identifier {
                body["statIdentifier"] = serde_json::json!(si);
            }
            if let Some(st) = stat_threshold {
                body["statThreshold"] = serde_json::json!(st);
            }

            let _: SuccessResponse = client
                .post(&format!("/games/{}/achievements", game_id), &body)
                .await?;
            println!("Created achievement '{}'", identifier);
        }
        AchievementsCommands::Update {
            identifier,
            display_name,
            description,
            image,
            authority,
            points,
            stat_identifier,
            stat_threshold,
        } => {
            if display_name.is_none()
                && description.is_none()
                && image.is_none()
                && authority.is_none()
                && points.is_none()
                && stat_identifier.is_none()
                && stat_threshold.is_none()
            {
                anyhow::bail!(
                    "At least one of --display-name, --description, --image, --authority, --points, --stat-identifier, or --stat-threshold is required"
                );
            }

            let mut body = serde_json::Map::new();
            if let Some(name) = &display_name {
                body.insert(
                    "displayName".to_string(),
                    serde_json::Value::String(name.clone()),
                );
            }
            if let Some(desc) = &description {
                body.insert(
                    "description".to_string(),
                    serde_json::Value::String(desc.clone()),
                );
            }
            if let Some(img) = &image {
                body.insert(
                    "image".to_string(),
                    serde_json::Value::String(img.clone()),
                );
            }
            if let Some(auth) = &authority {
                body.insert(
                    "authority".to_string(),
                    serde_json::Value::Number(auth.as_number().into()),
                );
            }
            if let Some(p) = points {
                body.insert(
                    "points".to_string(),
                    serde_json::Value::Number(p.into()),
                );
            }
            if let Some(si) = &stat_identifier {
                if si == "none" {
                    body.insert("statIdentifier".to_string(), serde_json::Value::Null);
                } else {
                    body.insert(
                        "statIdentifier".to_string(),
                        serde_json::Value::String(si.clone()),
                    );
                }
            }
            if let Some(st) = stat_threshold {
                body.insert(
                    "statThreshold".to_string(),
                    serde_json::json!(st),
                );
            }

            let _: SuccessResponse = client
                .put(
                    &format!("/games/{}/achievements/{}", game_id, identifier),
                    &serde_json::Value::Object(body),
                )
                .await?;
            println!("Updated achievement '{}'", identifier);
        }
        AchievementsCommands::Delete { identifier } => {
            let _: SuccessResponse = client
                .delete(&format!("/games/{}/achievements/{}", game_id, identifier))
                .await?;
            println!("Deleted achievement '{}'", identifier);
        }
    }

    Ok(())
}
