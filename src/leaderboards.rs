use crate::auth::require_api_key;
use crate::config;
use anyhow::Result;
use clap::ValueEnum;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortOrder {
    /// Higher scores rank first (points, kills, distance)
    #[value(alias = "descending")]
    Desc,
    /// Lower scores rank first (speedruns, golf, time)
    #[value(alias = "ascending")]
    Asc,
}

impl SortOrder {
    fn api_value(self) -> u8 {
        match self {
            SortOrder::Asc => 0,
            SortOrder::Desc => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DisplayType {
    /// Plain number
    Numeric,
    /// Time, with the score in seconds
    Seconds,
    /// Time, with the score in milliseconds
    Milliseconds,
    /// Time, with the score in 60fps game ticks
    GameTicks,
}

impl DisplayType {
    fn api_value(self) -> u8 {
        match self {
            DisplayType::Numeric => 0,
            DisplayType::Seconds => 1,
            DisplayType::Milliseconds => 2,
            DisplayType::GameTicks => 3,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct Leaderboard {
    id: String,
    name: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "sortOrder")]
    sort_order: u8,
    #[serde(rename = "displayType")]
    display_type: u8,
    visible: bool,
}

impl Leaderboard {
    fn sort_order_label(&self) -> String {
        match self.sort_order {
            0 => "lower is better".to_string(),
            1 => "higher is better".to_string(),
            other => other.to_string(),
        }
    }

    fn display_type_label(&self) -> String {
        match self.display_type {
            0 => "numeric".to_string(),
            1 => "seconds".to_string(),
            2 => "milliseconds".to_string(),
            3 => "game ticks".to_string(),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct LeaderboardResponse {
    leaderboard: Leaderboard,
}

#[derive(Debug, Deserialize)]
struct LeaderboardPage {
    leaderboards: Vec<Leaderboard>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

/// How `leaderboard update` finds the leaderboard to change.
pub enum LeaderboardSelector<'a> {
    Id(&'a str),
    Name(&'a str),
}

pub struct CreateLeaderboardArgs<'a> {
    pub game_id: &'a str,
    pub name: &'a str,
    pub display_name: &'a str,
    pub sort_order: SortOrder,
    pub display_type: DisplayType,
    pub hidden: bool,
}

pub struct UpdateLeaderboardArgs<'a> {
    pub game_id: &'a str,
    pub selector: LeaderboardSelector<'a>,
    pub display_name: Option<&'a str>,
    pub sort_order: Option<SortOrder>,
    pub display_type: Option<DisplayType>,
    pub hidden: Option<bool>,
}

/// Names go in the path as one encoded segment, so `Daily Scores` or a name
/// containing `/` can't escape into another route.
fn update_url(api_host: &str, game_id: &str, selector: &LeaderboardSelector) -> String {
    match selector {
        LeaderboardSelector::Id(id) => format!(
            "{}/api/games/{}/leaderboards/{}",
            api_host,
            game_id,
            urlencoding::encode(id)
        ),
        LeaderboardSelector::Name(name) => format!(
            "{}/api/games/{}/leaderboards/by-name/{}",
            api_host,
            game_id,
            urlencoding::encode(name)
        ),
    }
}

fn update_body(args: &UpdateLeaderboardArgs) -> serde_json::Map<String, serde_json::Value> {
    let mut body = serde_json::Map::new();
    if let Some(display_name) = args.display_name {
        body.insert("displayName".into(), json!(display_name));
    }
    if let Some(sort_order) = args.sort_order {
        body.insert("sortOrder".into(), json!(sort_order.api_value()));
    }
    if let Some(display_type) = args.display_type {
        body.insert("displayType".into(), json!(display_type.api_value()));
    }
    if let Some(hidden) = args.hidden {
        body.insert("visible".into(), json!(!hidden));
    }
    body
}

pub async fn handle_leaderboard_list(game_id: &str, json: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/leaderboards", api_host, game_id);

    let mut leaderboards = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut request = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key));
        if let Some(cursor) = &cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        let resp = request.send().await?;
        let resp = config::check_api_response(resp).await?;
        let page: LeaderboardPage = resp.json().await?;
        leaderboards.extend(page.leaderboards);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&leaderboards)?);
        return Ok(());
    }

    if leaderboards.is_empty() {
        println!("No leaderboards found.");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("ID"),
            Cell::new("Name"),
            Cell::new("Display Name"),
            Cell::new("Sort"),
            Cell::new("Display"),
            Cell::new("Visible"),
        ]);

    for leaderboard in leaderboards {
        let sort = leaderboard.sort_order_label();
        let display = leaderboard.display_type_label();
        table.add_row(vec![
            leaderboard.id,
            leaderboard.name,
            leaderboard.display_name,
            sort,
            display,
            (if leaderboard.visible { "yes" } else { "no" }).to_string(),
        ]);
    }

    println!("{table}");
    Ok(())
}

pub async fn handle_leaderboard_create(args: CreateLeaderboardArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/leaderboards", api_host, args.game_id);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "name": args.name,
            "displayName": args.display_name,
            "sortOrder": args.sort_order.api_value(),
            "displayType": args.display_type.api_value(),
            "visible": !args.hidden,
        }))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let LeaderboardResponse { leaderboard } = resp.json().await?;
    println!(
        "✓ Created leaderboard \"{}\" (id: {}, name: {})",
        leaderboard.display_name, leaderboard.id, leaderboard.name
    );
    Ok(())
}

pub async fn handle_leaderboard_update(args: UpdateLeaderboardArgs<'_>) -> Result<()> {
    let body = update_body(&args);
    if body.is_empty() {
        anyhow::bail!("No fields provided to update.");
    }

    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = update_url(&api_host, args.game_id, &args.selector);

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&serde_json::Value::Object(body))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let LeaderboardResponse { leaderboard } = resp.json().await?;
    println!(
        "✓ Updated leaderboard \"{}\" (id: {}, name: {})",
        leaderboard.display_name, leaderboard.id, leaderboard.name
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_args(selector: LeaderboardSelector<'_>) -> UpdateLeaderboardArgs<'_> {
        UpdateLeaderboardArgs {
            game_id: "game-id",
            selector,
            display_name: None,
            sort_order: None,
            display_type: None,
            hidden: None,
        }
    }

    #[test]
    fn enum_values_match_the_api() {
        assert_eq!(SortOrder::Asc.api_value(), 0);
        assert_eq!(SortOrder::Desc.api_value(), 1);
        assert_eq!(DisplayType::Numeric.api_value(), 0);
        assert_eq!(DisplayType::Seconds.api_value(), 1);
        assert_eq!(DisplayType::Milliseconds.api_value(), 2);
        assert_eq!(DisplayType::GameTicks.api_value(), 3);
    }

    #[test]
    fn update_url_addresses_by_id_or_encoded_name() {
        let host = "https://api.example.com";
        assert_eq!(
            update_url(host, "game-id", &LeaderboardSelector::Id("lb-id")),
            "https://api.example.com/api/games/game-id/leaderboards/lb-id"
        );
        assert_eq!(
            update_url(
                host,
                "game-id",
                &LeaderboardSelector::Name("Daily Scores/2")
            ),
            "https://api.example.com/api/games/game-id/leaderboards/by-name/Daily%20Scores%2F2"
        );
    }

    #[test]
    fn update_body_sends_only_the_fields_passed() {
        assert!(update_body(&update_args(LeaderboardSelector::Id("lb-id"))).is_empty());

        let body = update_body(&UpdateLeaderboardArgs {
            display_name: Some("Daily Challenge"),
            hidden: Some(false),
            ..update_args(LeaderboardSelector::Name("daily"))
        });
        assert_eq!(
            serde_json::Value::Object(body),
            json!({ "displayName": "Daily Challenge", "visible": true })
        );

        let body = update_body(&UpdateLeaderboardArgs {
            sort_order: Some(SortOrder::Asc),
            display_type: Some(DisplayType::Milliseconds),
            hidden: Some(true),
            ..update_args(LeaderboardSelector::Id("lb-id"))
        });
        assert_eq!(
            serde_json::Value::Object(body),
            json!({ "sortOrder": 0, "displayType": 2, "visible": false })
        );
    }

    #[test]
    fn parses_a_page_and_round_trips_api_field_names() {
        let page: LeaderboardPage = serde_json::from_value(json!({
            "leaderboards": [{
                "id": "lb-id",
                "name": "high_scores",
                "displayName": "High Scores",
                "sortOrder": 1,
                "displayType": 0,
                "visible": true
            }],
            "nextCursor": null
        }))
        .expect("the list response should deserialize");

        assert_eq!(page.next_cursor, None);
        let leaderboard = &page.leaderboards[0];
        assert_eq!(leaderboard.sort_order_label(), "higher is better");
        assert_eq!(leaderboard.display_type_label(), "numeric");

        let value = serde_json::to_value(leaderboard).expect("leaderboard should serialize");
        assert_eq!(value["displayName"], "High Scores");
        assert!(value.get("display_name").is_none());
    }
}
