use crate::auth::require_api_key;
use crate::config;
use anyhow::Result;
use colored::Colorize;
use serde_json::json;

/// A single kind of playtest data that can be wiped for a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Achievements,
    CloudSaves,
    Stats,
    Leaderboards,
    Entitlements,
    Ugc,
}

impl Category {
    /// Every category, in display order. Used when no target flag is passed.
    const ALL: [Category; 6] = [
        Category::Achievements,
        Category::CloudSaves,
        Category::Stats,
        Category::Leaderboards,
        Category::Entitlements,
        Category::Ugc,
    ];

    /// The key sent to the API (camelCase, matching Convex conventions).
    fn api_key(self) -> &'static str {
        match self {
            Category::Achievements => "achievements",
            Category::CloudSaves => "cloudSaves",
            Category::Stats => "stats",
            Category::Leaderboards => "leaderboards",
            Category::Entitlements => "entitlements",
            Category::Ugc => "ugc",
        }
    }

    /// Human-friendly label for confirmation prompts and the summary output.
    fn label(self) -> &'static str {
        match self {
            Category::Achievements => "achievements",
            Category::CloudSaves => "cloud saves",
            Category::Stats => "stats",
            Category::Leaderboards => "leaderboards",
            Category::Entitlements => "paid-content entitlements",
            Category::Ugc => "user-generated content",
        }
    }
}

pub struct ClearPlaytestDataArgs<'a> {
    pub game_id: &'a str,
    /// When set, only that player's data is cleared; otherwise everyone's.
    pub username: Option<&'a str>,
    pub achievements: bool,
    pub cloud_saves: bool,
    pub stats: bool,
    pub leaderboards: bool,
    pub entitlements: bool,
    pub ugc: bool,
    pub force: bool,
}

impl ClearPlaytestDataArgs<'_> {
    /// Which categories to clear. When no target flag is set, clears all.
    fn categories(&self) -> Vec<Category> {
        let selected: Vec<Category> = [
            (self.achievements, Category::Achievements),
            (self.cloud_saves, Category::CloudSaves),
            (self.stats, Category::Stats),
            (self.leaderboards, Category::Leaderboards),
            (self.entitlements, Category::Entitlements),
            (self.ugc, Category::Ugc),
        ]
        .into_iter()
        .filter_map(|(on, cat)| on.then_some(cat))
        .collect();

        if selected.is_empty() {
            Category::ALL.to_vec()
        } else {
            selected
        }
    }
}

pub async fn handle_clear_playtest_data(args: ClearPlaytestDataArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;
    let categories = args.categories();

    let category_labels = categories
        .iter()
        .map(|c| format!("\n {} {}", "•".dimmed(), c.label()))
        .collect::<String>();

    let scope = match args.username {
        Some(user) => format!("player \"{}\"", user),
        None => "ALL players".to_string(),
    };

    // Confirm before doing anything destructive.
    if !args.force {
        if crate::is_non_interactive() {
            anyhow::bail!(
                "Refusing to clear playtest data without confirmation.\n\
                 Re-run with --force (alias --yes / -y) to proceed non-interactively."
            );
        }

        println!(
            "{} This will permanently delete {}\nfor {} in game {}.",
            "Warning:".yellow().bold(),
            category_labels.bold(),
            scope.bold(),
            args.game_id
        );
        let confirmed = cliclack::confirm("Are you sure you want to continue?")
            .initial_value(false)
            .interact()?;
        if !confirmed {
            println!("Aborted. No data was cleared.");
            return Ok(());
        }
    }

    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/clear-playtest-data",
        api_host, args.game_id
    );

    let mut body = json!({
        "categories": categories.iter().map(|c| c.api_key()).collect::<Vec<_>>(),
    });
    if let Some(user) = args.username {
        body["username"] = json!(user);
    }

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await?;

    // A 2xx means the deletion was scheduled. Deletion now runs asynchronously in
    // the background (self-scheduling batch sweeps drain over follow-up
    // transactions)
    config::check_api_response(resp).await?;

    println!(
        "✓ Scheduled deletion of playtest data for {} in game {}:",
        scope, args.game_id
    );
    for cat in &categories {
        println!("  {} {}", "•".dimmed(), cat.label());
    }

    Ok(())
}
