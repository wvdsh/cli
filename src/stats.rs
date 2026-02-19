use crate::api_client::ApiClient;
use crate::config::WavedashConfig;
use anyhow::Result;
use clap::Subcommand;
use serde::Deserialize;
use std::path::PathBuf;

/// Authority level for stats and achievements.
#[derive(Clone, clap::ValueEnum)]
pub enum AuthorityArg {
    Client,
    Server,
}

impl AuthorityArg {
    pub fn as_number(&self) -> u8 {
        match self {
            AuthorityArg::Client => 0,
            AuthorityArg::Server => 1,
        }
    }
}

pub fn authority_label(value: u64) -> &'static str {
    match value {
        0 => "client",
        1 => "server",
        _ => "unknown",
    }
}

#[derive(Clone, clap::ValueEnum)]
pub enum StatTypeArg {
    Integer,
    Float,
    AvgRate,
}

impl StatTypeArg {
    fn as_number(&self) -> u8 {
        match self {
            StatTypeArg::Integer => 0,
            StatTypeArg::Float => 1,
            StatTypeArg::AvgRate => 2,
        }
    }
}

fn stat_type_label(value: u64) -> &'static str {
    match value {
        0 => "integer",
        1 => "float",
        2 => "avg-rate",
        _ => "unknown",
    }
}

#[derive(Subcommand)]
pub enum StatsCommands {
    /// List all stats for the game
    List,
    /// Create a new stat
    Create {
        /// Stat identifier (letters, numbers, underscores; must start with a letter)
        identifier: String,
        /// Display name
        #[arg(long)]
        display_name: String,
        /// Authority level
        #[arg(long, value_enum)]
        authority: AuthorityArg,
        /// Stat type
        #[arg(long = "type", value_enum)]
        stat_type: StatTypeArg,
    },
    /// Update an existing stat
    Update {
        /// Stat identifier to update
        identifier: String,
        /// New display name
        #[arg(long)]
        display_name: Option<String>,
        /// New authority level
        #[arg(long, value_enum)]
        authority: Option<AuthorityArg>,
        /// New stat type
        #[arg(long = "type", value_enum)]
        stat_type: Option<StatTypeArg>,
    },
    /// Delete a stat
    Delete {
        /// Stat identifier to delete
        identifier: String,
    },
}

#[derive(Deserialize)]
struct ListStatsResponse {
    stats: Vec<Stat>,
}

#[derive(Deserialize)]
struct Stat {
    identifier: String,
    #[serde(rename = "displayName")]
    display_name: String,
    authority: u64,
    #[serde(rename = "type")]
    stat_type: u64,
}

#[derive(Deserialize)]
struct SuccessResponse {
    #[allow(dead_code)]
    success: bool,
}

pub async fn handle_stats(config_path: PathBuf, command: StatsCommands) -> Result<()> {
    let wavedash_config = WavedashConfig::load(&config_path)?;
    let game_id = &wavedash_config.game_id;
    let client = ApiClient::new()?;

    match command {
        StatsCommands::List => {
            let resp: ListStatsResponse =
                client.get(&format!("/games/{}/stats", game_id)).await?;

            if resp.stats.is_empty() {
                println!("No stats found.");
                return Ok(());
            }

            // Calculate column widths
            let id_w = resp
                .stats
                .iter()
                .map(|s| s.identifier.len())
                .max()
                .unwrap_or(0)
                .max(10);
            let name_w = resp
                .stats
                .iter()
                .map(|s| s.display_name.len())
                .max()
                .unwrap_or(0)
                .max(12);

            println!(
                "{:<id_w$}  {:<name_w$}  {:<9}  TYPE",
                "IDENTIFIER", "DISPLAY NAME", "AUTHORITY"
            );
            for stat in &resp.stats {
                println!(
                    "{:<id_w$}  {:<name_w$}  {:<9}  {}",
                    stat.identifier,
                    stat.display_name,
                    authority_label(stat.authority),
                    stat_type_label(stat.stat_type),
                );
            }
        }
        StatsCommands::Create {
            identifier,
            display_name,
            authority,
            stat_type,
        } => {
            let body = serde_json::json!({
                "identifier": identifier,
                "displayName": display_name,
                "authority": authority.as_number(),
                "type": stat_type.as_number(),
            });
            let _: SuccessResponse = client
                .post(&format!("/games/{}/stats", game_id), &body)
                .await?;
            println!("Created stat '{}'", identifier);
        }
        StatsCommands::Update {
            identifier,
            display_name,
            authority,
            stat_type,
        } => {
            if display_name.is_none() && authority.is_none() && stat_type.is_none() {
                anyhow::bail!("At least one of --display-name, --authority, or --type is required");
            }

            let mut body = serde_json::Map::new();
            if let Some(name) = &display_name {
                body.insert(
                    "displayName".to_string(),
                    serde_json::Value::String(name.clone()),
                );
            }
            if let Some(auth) = &authority {
                body.insert(
                    "authority".to_string(),
                    serde_json::Value::Number(auth.as_number().into()),
                );
            }
            if let Some(st) = &stat_type {
                body.insert(
                    "type".to_string(),
                    serde_json::Value::Number(st.as_number().into()),
                );
            }

            let _: SuccessResponse = client
                .put(
                    &format!("/games/{}/stats/{}", game_id, identifier),
                    &serde_json::Value::Object(body),
                )
                .await?;
            println!("Updated stat '{}'", identifier);
        }
        StatsCommands::Delete { identifier } => {
            let _: SuccessResponse = client
                .delete(&format!("/games/{}/stats/{}", game_id, identifier))
                .await?;
            println!("Deleted stat '{}'", identifier);
        }
    }

    Ok(())
}
