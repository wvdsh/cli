mod achievements;
mod auth;
mod browser;
mod builds;
mod clear_playtest_data;
mod config;
mod dev;
mod file_staging;
mod init;
mod paid_content;
mod publish;
mod stats;
mod updater;
mod welcome;

use achievements::{
    handle_achievement_create, handle_achievement_delete, handle_achievement_list,
    handle_achievement_update, CreateAchievementArgs, UpdateAchievementArgs,
};
use anyhow::Result;
use auth::{login_with_browser, AuthManager, AuthSource};
use builds::handle_build_push;
use clap::{Parser, Subcommand};
use clear_playtest_data::{handle_clear_playtest_data, ClearPlaytestDataArgs};
use colored::Colorize;
use config::{resolve_game_id, UploadSource};
use dev::handle_dev;
use init::{
    handle_init, handle_project_create, handle_project_list, handle_team_create, handle_team_list,
};
use paid_content::{
    handle_paid_content_create, handle_paid_content_deactivate, handle_paid_content_list,
    handle_paid_content_resolve, handle_paid_content_update, CreatePaidContentArgs,
    ResolvePaidContentArgs, UpdatePaidContentArgs, Visibility,
};
use publish::{handle_publish, PublishArgs};
use stats::{handle_stat_create, handle_stat_delete, handle_stat_update};
use std::io::{IsTerminal, Read};
use std::path::PathBuf;

fn mask_token(token: &str) -> String {
    if token.len() > 10 {
        format!("{}...{}", &token[..6], &token[token.len() - 3..])
    } else {
        "***".to_string()
    }
}

/// Rejects a blank value the user typed. Only ever sees typed values — no `#[arg]`
/// using it is wired to clap's `env`, deliberately: clap feeds a set-but-blank
/// variable to the value parser for `Option<String>` args, which would turn an
/// unpopulated CI variable into a usage error instead of the "counts as unset"
/// the overrides promise. Blank env vars are handled in `config`.
/// Trims and rejects through the same [`config::non_blank`] every `WAVEDASH_*`
/// variable and the stored credentials file go through, so "blank counts as
/// unset" has one implementation to disagree with itself from.
pub(crate) fn parse_non_empty_arg(value: &str) -> Result<String, String> {
    config::non_blank(value.to_string()).ok_or_else(|| "value cannot be empty".to_string())
}

#[derive(Parser)]
#[command(name = "wavedash")]
#[command(about = "Cross-platform CLI tool for uploading game projects to wavedash.com")]
#[command(version)]
struct Cli {
    #[arg(long, global = true, help = "Enable verbose output")]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Initialize a wavedash.toml config for this project")]
    Init,
    Auth {
        #[command(subcommand)]
        action: AuthCommands,
    },
    Build {
        #[command(subcommand)]
        action: BuildCommands,
    },
    Dev {
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file"
        )]
        config: Option<PathBuf>,
        #[arg(
            long = "no-open",
            help = "Don't automatically open the browser; just print the local URL"
        )]
        no_open: bool,
        #[arg(
            long = "upload-source",
            value_enum,
            hide = true,
            help = "Attribute the build to the tool running the CLI instead of the CLI itself"
        )]
        upload_source: Option<UploadSource>,
        #[arg(
            long = "sdk-js",
            value_name = "PATH",
            hide = true,
            help = "Serve a local sdk-js build instead of the pinned CDN one — an inject.global.js, or a directory holding it"
        )]
        sdk_js: Option<PathBuf>,
    },
    #[command(
        about = "Publish an uploaded build to wavedash.com",
        override_usage = "wavedash publish <BUILD_ID> [OPTIONS]"
    )]
    Publish {
        #[arg(
            help = "Build ID returned by `wavedash build push`",
            value_parser = parse_non_empty_arg
        )]
        build_id: String,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Release title")]
        title: Option<String>,
        #[arg(long, help = "Release summary")]
        summary: Option<String>,
        #[arg(long, help = "Added change item", action = clap::ArgAction::Append, num_args = 1)]
        added: Vec<String>,
        #[arg(long, help = "Removed change item", action = clap::ArgAction::Append, num_args = 1)]
        removed: Vec<String>,
        #[arg(long, help = "Fixed change item", action = clap::ArgAction::Append, num_args = 1)]
        fixed: Vec<String>,
        #[arg(long, help = "Adjusted change item", action = clap::ArgAction::Append, num_args = 1)]
        adjusted: Vec<String>,
        #[arg(
            long = "yes",
            short = 'y',
            visible_alias = "force",
            help = "Skip confirmation (required when non-interactive)"
        )]
        yes: bool,
    },
    Team {
        #[command(subcommand)]
        action: TeamCommands,
    },
    Project {
        #[command(subcommand)]
        action: ProjectCommands,
    },
    Stat {
        #[command(subcommand)]
        action: StatCommands,
    },
    Achievement {
        #[command(subcommand)]
        action: AchievementCommands,
    },
    #[command(
        name = "paid-content",
        visible_alias = "unlockable",
        about = "Manage in-build paywalls (unlockable content) for a game"
    )]
    PaidContent {
        #[command(subcommand)]
        action: PaidContentCommands,
    },
    #[command(
        name = "clear-playtest-data",
        about = "Delete playtest data (achievements, saves, stats, leaderboards, entitlements, UGC) for a game"
    )]
    ClearPlaytestData {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(
            short = 'u',
            long = "username",
            visible_alias = "user",
            help = "Only clear this player's data; omit to clear everyone's"
        )]
        username: Option<String>,
        // --- What to clear. If none are passed, all categories are cleared. ---
        #[arg(
            long = "achievements",
            visible_alias = "achs",
            help = "Clear achievement progress",
            help_heading = "What to clear"
        )]
        achievements: bool,
        #[arg(
            long = "cloud-saves",
            visible_alias = "saves",
            help = "Clear cloud saves",
            help_heading = "What to clear"
        )]
        cloud_saves: bool,
        #[arg(
            long = "stats",
            help = "Clear stat progress",
            help_heading = "What to clear"
        )]
        stats: bool,
        #[arg(
            long = "leaderboards",
            visible_alias = "lbs",
            help = "Clear leaderboard entries",
            help_heading = "What to clear"
        )]
        leaderboards: bool,
        #[arg(
            long = "paid-content-entitlements",
            visible_aliases = ["entitlements", "ents"],
            help = "Clear paid content entitlements",
            help_heading = "What to clear"
        )]
        entitlements: bool,
        #[arg(
            long = "user-generated-content",
            visible_alias = "ugc",
            help = "Clear user-generated content",
            help_heading = "What to clear"
        )]
        ugc: bool,
        #[arg(
            long = "force",
            short = 'y',
            visible_alias = "yes",
            help = "Skip confirmation (required when non-interactive)"
        )]
        force: bool,
    },
    #[command(about = "Check for and install updates")]
    Update,
}

#[derive(Subcommand)]
enum AuthCommands {
    Login {
        #[arg(
            long,
            value_parser = parse_non_empty_arg,
            help = "API key for manual authentication"
        )]
        token: Option<String>,
        #[arg(
            long = "token-stdin",
            conflicts_with = "token",
            help = "Read API key from stdin"
        )]
        token_stdin: bool,
    },
    Logout,
    Status,
}

#[derive(Subcommand)]
enum BuildCommands {
    Push {
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(short = 'm', long = "message", help = "Build message")]
        message: Option<String>,
        #[arg(
            long = "upload-source",
            value_enum,
            hide = true,
            help = "Attribute the build to the tool running the CLI instead of the CLI itself"
        )]
        upload_source: Option<UploadSource>,
    },
}

#[derive(Subcommand)]
enum TeamCommands {
    #[command(about = "Create a new team")]
    Create {
        #[arg(long, help = "Team name")]
        name: String,
    },
    #[command(about = "List your teams")]
    List {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ProjectCommands {
    #[command(about = "Create a new project")]
    Create {
        #[arg(long, help = "Project title")]
        title: String,
        #[arg(long = "team-id", help = "Team ID")]
        team_id: String,
    },
    #[command(about = "List projects (games) for a team")]
    List {
        #[arg(long = "team-id", help = "Team ID")]
        team_id: String,
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
}

#[derive(Subcommand)]
enum StatCommands {
    #[command(about = "Create a new stat for a game")]
    Create {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Stat identifier (e.g. KILLS_TOTAL)")]
        identifier: String,
        #[arg(long, help = "Stat display name")]
        name: String,
    },
    #[command(about = "Update a stat's identifier and display name")]
    Update {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Stat ID")]
        id: String,
        #[arg(long, help = "New identifier (e.g. KILLS_TOTAL)")]
        identifier: String,
        #[arg(long, help = "New display name")]
        name: String,
    },
    #[command(about = "Delete a stat")]
    Delete {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Stat ID")]
        id: String,
        #[arg(long, help = "Required if any user progress is attached to this stat")]
        force: bool,
    },
}

#[derive(Subcommand)]
enum AchievementCommands {
    #[command(about = "List achievements for a game")]
    List {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Create a new achievement for a game")]
    Create {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Achievement identifier (e.g. FIRST_WIN)")]
        identifier: String,
        #[arg(long, help = "Achievement title (display name)")]
        title: String,
        #[arg(long, help = "Achievement description")]
        description: String,
        #[arg(long, help = "Mark the achievement as secret", default_value_t = false)]
        secret: bool,
        #[arg(
            long = "triggered-by-stat-id",
            help = "Stat ID that triggers this achievement (omit for a standard achievement)"
        )]
        triggered_by_stat_id: Option<String>,
        #[arg(
            long,
            help = "Stat threshold required to unlock (required when --triggered-by-stat-id is set)"
        )]
        threshold: Option<f64>,
        #[arg(
            long,
            help = "Path to an image file (jpg, jpeg, png, webp, avif) to use as the achievement icon"
        )]
        image: Option<PathBuf>,
    },
    #[command(about = "Update an achievement")]
    Update {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Achievement ID")]
        id: String,
        #[arg(long, help = "New identifier (e.g. FIRST_WIN)")]
        identifier: Option<String>,
        #[arg(long, help = "New title (display name)")]
        title: Option<String>,
        #[arg(long, help = "New description")]
        description: Option<String>,
        #[arg(long, help = "Mark/unmark as secret")]
        secret: Option<bool>,
        #[arg(
            long = "triggered-by-stat-id",
            help = "Stat ID that triggers this achievement (pass empty string \"\" to clear)"
        )]
        triggered_by_stat_id: Option<String>,
        #[arg(long, help = "Stat threshold")]
        threshold: Option<f64>,
        #[arg(long, help = "Path to a new image file (jpg, jpeg, png, webp, avif)")]
        image: Option<PathBuf>,
    },
    #[command(about = "Delete an achievement")]
    Delete {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Achievement ID")]
        id: String,
        #[arg(long, help = "Required if any user has unlocked this achievement")]
        force: bool,
    },
}

#[derive(Subcommand)]
enum PaidContentCommands {
    #[command(visible_alias = "ls", about = "List paid content for a game")]
    List {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(
        visible_alias = "add",
        about = "Create a paywall over build files",
        override_usage = "wavedash paid-content create <CONTENT_IDENTIFIER> [OPTIONS]",
        group(
            clap::ArgGroup::new("gating")
                .required(true)
                .multiple(true)
                .args(["pattern", "no_patterns"])
        )
    )]
    Create {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(
            value_name = "CONTENT_IDENTIFIER",
            value_parser = parse_non_empty_arg,
            help = "Identifier your game passes to isEntitled (e.g. full-version)"
        )]
        content_identifier: String,
        #[command(flatten)]
        patterns: paid_content::PatternArgs,
        #[arg(
            long,
            value_name = "USD",
            value_parser = paid_content::parse_price_dollars,
            help = "Price in USD, e.g. 4.99"
        )]
        price: i64,
        #[arg(long, value_parser = parse_non_empty_arg, help = "Paywall modal headline")]
        title: String,
        #[arg(
            long = "feature",
            visible_alias = "feat",
            help = "Feature bullet; pass multiple times",
            action = clap::ArgAction::Append,
            num_args = 1,
            required = true
        )]
        feature: Vec<String>,
        #[arg(long, visible_alias = "msg", help = "Paywall modal body copy")]
        message: Option<String>,
        #[arg(
            long = "button-label",
            visible_alias = "btn",
            help = "Purchase button label",
            default_value = "Unlock"
        )]
        button_label: String,
        #[arg(
            long,
            visible_aliases = ["visible", "vis"],
            value_enum,
            default_value = "playtest",
            help = "Where the content is offered (inactive is set via `deactivate`)"
        )]
        visibility: Visibility,
        #[command(flatten)]
        report: paid_content::MatchReportOptions,
    },
    #[command(
        about = "Update paid content; only the fields you pass change",
        override_usage = "wavedash paid-content update <CONTENT_IDENTIFIER> [OPTIONS]"
    )]
    Update {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(
            value_name = "CONTENT_IDENTIFIER",
            value_parser = parse_non_empty_arg,
            conflicts_with = "id",
            required_unless_present = "id",
            help = "Identifier chosen at create time (e.g. full-version)"
        )]
        content_identifier: Option<String>,
        #[arg(
            long,
            conflicts_with = "content_identifier",
            required_unless_present = "content_identifier",
            value_parser = parse_non_empty_arg,
            help = "Address the entry by its document ID instead"
        )]
        id: Option<String>,
        #[command(flatten)]
        patterns: paid_content::PatternArgs,
        #[arg(
            long,
            value_name = "USD",
            value_parser = paid_content::parse_price_dollars,
            help = "New price in USD"
        )]
        price: Option<i64>,
        #[arg(long, value_parser = parse_non_empty_arg, help = "New modal headline")]
        title: Option<String>,
        #[arg(
            long = "feature",
            visible_alias = "feat",
            help = "Replaces the entire feature list; pass multiple times",
            action = clap::ArgAction::Append,
            num_args = 1
        )]
        feature: Vec<String>,
        #[arg(long, visible_alias = "msg", help = "New modal body copy")]
        message: Option<String>,
        #[arg(
            long = "button-label",
            visible_alias = "btn",
            help = "New purchase button label"
        )]
        button_label: Option<String>,
        #[arg(
            long,
            visible_aliases = ["visible", "vis"],
            value_enum,
            help = "Where the content is offered"
        )]
        visibility: Option<Visibility>,
        #[command(flatten)]
        report: paid_content::MatchReportOptions,
    },
    #[command(
        visible_aliases = ["delete", "del"],
        about = "Stop offering paid content; buyers keep access",
        override_usage = "wavedash paid-content deactivate <CONTENT_IDENTIFIER> [OPTIONS]"
    )]
    Deactivate {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(
            value_name = "CONTENT_IDENTIFIER",
            value_parser = parse_non_empty_arg,
            conflicts_with = "id",
            required_unless_present = "id",
            help = "Identifier chosen at create time (e.g. full-version)"
        )]
        content_identifier: Option<String>,
        #[arg(
            long,
            conflicts_with = "content_identifier",
            required_unless_present = "content_identifier",
            value_parser = parse_non_empty_arg,
            help = "Address the entry by its document ID instead"
        )]
        id: Option<String>,
        #[arg(
            long = "yes",
            short = 'y',
            visible_alias = "force",
            help = "Skip confirmation (required when non-interactive)"
        )]
        yes: bool,
    },
    #[command(
        about = "Show which build files a paywall's patterns would gate",
        override_usage = "wavedash paid-content resolve <CONTENT_IDENTIFIER> [OPTIONS]",
        group = clap::ArgGroup::new("resolve_target")
            .required(true)
            .args(["content_identifier", "id", "pattern"])
    )]
    Resolve {
        #[arg(
            long = "game-id",
            value_parser = parse_non_empty_arg,
            help = "Game ID (defaults to game_id in wavedash.toml. override with WAVEDASH_GAME_ID)"
        )]
        game_id: Option<String>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
        #[arg(
            value_name = "CONTENT_IDENTIFIER",
            value_parser = parse_non_empty_arg,
            help = "Resolve the patterns stored on this entry"
        )]
        content_identifier: Option<String>,
        #[arg(
            long,
            value_parser = parse_non_empty_arg,
            help = "Resolve the patterns stored on this entry, by document ID"
        )]
        id: Option<String>,
        #[arg(
            long = "pattern",
            visible_alias = "glob",
            value_parser = parse_non_empty_arg,
            help = "Resolve an ad-hoc glob instead; pass multiple times. QUOTE IT — an unquoted glob is expanded by your shell",
            action = clap::ArgAction::Append,
            num_args = 1
        )]
        pattern: Vec<String>,
        #[command(flatten)]
        report: paid_content::MatchReportOptions,
        #[arg(long, help = "Output the full report as JSON")]
        json: bool,
    },
}

fn paid_content_ref<'a>(
    id: Option<&'a str>,
    content_identifier: Option<&'a str>,
) -> Option<paid_content::PaidContentRef<'a>> {
    id.map(paid_content::PaidContentRef::Id)
        .or_else(|| content_identifier.map(paid_content::PaidContentRef::ContentIdentifier))
}

fn passed(values: &[String]) -> Option<&[String]> {
    (!values.is_empty()).then_some(values)
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            !value.is_empty() && value != "0" && value != "false"
        })
        .unwrap_or(false)
}

pub(crate) fn is_non_interactive() -> bool {
    env_flag_enabled("CI") || !std::io::stdin().is_terminal()
}

fn read_token_from_stdin() -> Result<String> {
    let mut token = String::new();
    std::io::stdin().read_to_string(&mut token)?;
    config::non_blank(token).ok_or_else(|| anyhow::anyhow!("No token provided on stdin"))
}

#[tokio::main]
async fn main() {
    // Install rustls crypto provider for TLS
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    if let Err(e) = run().await {
        eprintln!("{} {:#}", "Error:".red().bold(), e);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    // Bare `wavedash` (no subcommand) is the home screen: show the splash and
    // point at --help.
    let Some(command) = cli.command else {
        welcome::show_home();
        return Ok(());
    };

    // Greet on the very first interactive run (skip for `update`, which has its
    // own focused output).
    if !matches!(command, Commands::Update) {
        welcome::show_first_run_if_needed();
    }

    // Check for updates in background, but skip commands with their own focused prompts.
    let update_handle = if matches!(
        command,
        Commands::Update
            | Commands::Auth {
                action: AuthCommands::Login { .. },
            }
    ) {
        None
    } else {
        Some(updater::check_for_update())
    };

    match command {
        Commands::Init => {
            handle_init().await?;
        }
        Commands::Auth { action } => {
            let auth_manager = AuthManager::new()?;

            match action {
                AuthCommands::Login { token, token_stdin } => {
                    let token = if token_stdin {
                        Some(read_token_from_stdin()?)
                    } else {
                        token
                    };

                    if let Some(api_key) = token {
                        // Manual token input (no email available)
                        auth_manager.store_credentials(&api_key, None)?;
                        println!("✓ Successfully stored API key");
                    } else {
                        if is_non_interactive() {
                            anyhow::bail!(
                                "Browser login isn't available in this environment.\n\nCreate an API key at https://wavedash.com/dev-portal/keys. Then set WAVEDASH_TOKEN or pipe the key into wavedash auth login --token-stdin."
                            );
                        }

                        // Browser-based login
                        match login_with_browser().await {
                            Ok(result) => {
                                auth_manager
                                    .store_credentials(&result.api_key, result.email.as_deref())?;
                                println!("✓ Successfully authenticated!");
                            }
                            Err(e) => {
                                eprintln!("Authentication failed: {}", e);
                                std::process::exit(1);
                            }
                        }
                    }
                }
                AuthCommands::Logout => {
                    auth_manager.clear_credentials()?;
                    // The file is all logout can reach, but the environment
                    // outranks it on every request and nothing announces that as it
                    // happens, so a bare success line is one the next command
                    // contradicts.
                    match std::env::var(config::ENV_TOKEN)
                        .ok()
                        .and_then(config::non_blank)
                    {
                        Some(_) => println!(
                            "✓ Removed stored credentials, but {} is still set and still authenticates every command.\nUnset it to finish logging out.",
                            config::ENV_TOKEN
                        ),
                        None => println!("✓ Successfully logged out"),
                    }
                }
                AuthCommands::Status => {
                    let auth_info = auth_manager.get_auth_info();
                    match auth_info.source {
                        AuthSource::Environment => {
                            println!("✓ Authenticated (via WAVEDASH_TOKEN environment variable)");
                            if let Some(api_key) = auth_info.api_key {
                                println!("Token: {}", mask_token(&api_key));
                            }
                        }
                        AuthSource::File => {
                            println!("✓ Authenticated (via stored credentials)");
                            if let Some(email) = auth_info.email {
                                println!("Email: {}", email);
                            }
                            if let Some(api_key) = auth_info.api_key {
                                println!("API Key: {}", mask_token(&api_key));
                            }
                        }
                        AuthSource::None => {
                            println!("Not authenticated. Run 'wavedash auth login' or set WAVEDASH_TOKEN environment variable.");
                        }
                    }
                }
            }
        }
        Commands::Build { action } => match action {
            BuildCommands::Push {
                config,
                message,
                upload_source,
            } => {
                handle_build_push(
                    config,
                    cli.verbose,
                    message,
                    upload_source.unwrap_or_default(),
                )
                .await?;
            }
        },
        Commands::Dev {
            config,
            no_open,
            upload_source,
            sdk_js,
        } => {
            handle_dev(
                config,
                cli.verbose,
                no_open,
                upload_source.unwrap_or_default(),
                sdk_js,
            )
            .await?;
        }
        Commands::Publish {
            config,
            build_id,
            title,
            summary,
            added,
            removed,
            fixed,
            adjusted,
            yes,
        } => {
            handle_publish(PublishArgs {
                config_path: config,
                build_id,
                title,
                summary,
                added,
                removed,
                fixed,
                adjusted,
                yes,
            })
            .await?;
        }
        Commands::Team { action } => match action {
            TeamCommands::Create { name } => {
                handle_team_create(&name).await?;
            }
            TeamCommands::List { json } => {
                handle_team_list(json).await?;
            }
        },
        Commands::Project { action } => match action {
            ProjectCommands::Create { title, team_id } => {
                handle_project_create(&title, &team_id).await?;
            }
            ProjectCommands::List { team_id, json } => {
                handle_project_list(&team_id, json).await?;
            }
        },
        Commands::Stat { action } => match action {
            StatCommands::Create {
                game_id,
                config,
                identifier,
                name,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_stat_create(&game_id, &identifier, &name).await?;
            }
            StatCommands::Update {
                game_id,
                config,
                id,
                identifier,
                name,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_stat_update(&game_id, &id, &identifier, &name).await?;
            }
            StatCommands::Delete {
                game_id,
                config,
                id,
                force,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_stat_delete(&game_id, &id, force).await?;
            }
        },
        Commands::Achievement { action } => {
            match action {
                AchievementCommands::List {
                    game_id,
                    config,
                    json,
                } => {
                    let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                    handle_achievement_list(&game_id, json).await?;
                }
                AchievementCommands::Create {
                    game_id,
                    config,
                    identifier,
                    title,
                    description,
                    secret,
                    triggered_by_stat_id,
                    threshold,
                    image,
                } => {
                    let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                    handle_achievement_create(CreateAchievementArgs {
                        game_id: &game_id,
                        identifier: &identifier,
                        title: &title,
                        description: &description,
                        secret,
                        triggered_by_stat_id: triggered_by_stat_id.as_deref(),
                        stat_threshold: threshold,
                        image_path: image.as_deref(),
                    })
                    .await?;
                }
                AchievementCommands::Update {
                    game_id,
                    config,
                    id,
                    identifier,
                    title,
                    description,
                    secret,
                    triggered_by_stat_id,
                    threshold,
                    image,
                } => {
                    // CLI convention: --triggered-by-stat-id "" clears, omitted leaves alone
                    let triggered: Option<Option<&str>> =
                        triggered_by_stat_id.as_deref().map(|s| {
                            if s.is_empty() {
                                None
                            } else {
                                Some(s)
                            }
                        });
                    let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                    handle_achievement_update(UpdateAchievementArgs {
                        game_id: &game_id,
                        achievement_id: &id,
                        title: title.as_deref(),
                        identifier: identifier.as_deref(),
                        description: description.as_deref(),
                        secret,
                        triggered_by_stat_id: triggered,
                        stat_threshold: threshold,
                        image_path: image.as_deref(),
                    })
                    .await?;
                }
                AchievementCommands::Delete {
                    game_id,
                    config,
                    id,
                    force,
                } => {
                    let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                    handle_achievement_delete(&game_id, &id, force).await?;
                }
            }
        }
        Commands::PaidContent { action } => match action {
            PaidContentCommands::List {
                game_id,
                config,
                json,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_paid_content_list(&game_id, json).await?;
            }
            PaidContentCommands::Create {
                game_id,
                config,
                content_identifier,
                patterns,
                price,
                title,
                feature,
                message,
                button_label,
                visibility,
                report,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_paid_content_create(CreatePaidContentArgs {
                    game_id: &game_id,
                    content_identifier: &content_identifier,
                    patterns: &patterns,
                    price_cents: price,
                    title: &title,
                    message: message.as_deref(),
                    features: &feature,
                    button_label: &button_label,
                    visibility,
                    report,
                })
                .await?;
            }
            PaidContentCommands::Update {
                game_id,
                config,
                id,
                content_identifier,
                patterns,
                price,
                title,
                feature,
                message,
                button_label,
                visibility,
                report,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_paid_content_update(UpdatePaidContentArgs {
                    game_id: &game_id,
                    reference: paid_content_ref(id.as_deref(), content_identifier.as_deref())
                        .expect("clap requires an entry to update"),
                    patterns: &patterns,
                    price_cents: price,
                    title: title.as_deref(),
                    message: message.as_deref(),
                    features: passed(&feature),
                    button_label: button_label.as_deref(),
                    visibility,
                    report,
                })
                .await?;
            }
            PaidContentCommands::Deactivate {
                game_id,
                config,
                id,
                content_identifier,
                yes,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                let reference = paid_content_ref(id.as_deref(), content_identifier.as_deref())
                    .expect("clap requires an entry to deactivate");
                handle_paid_content_deactivate(&game_id, reference, yes).await?;
            }
            PaidContentCommands::Resolve {
                game_id,
                config,
                content_identifier,
                id,
                pattern,
                json,
                report,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_paid_content_resolve(ResolvePaidContentArgs {
                    game_id: &game_id,
                    patterns: &pattern,
                    reference: paid_content_ref(id.as_deref(), content_identifier.as_deref()),
                    json_output: json,
                    report,
                })
                .await?;
            }
        },
        Commands::ClearPlaytestData {
            game_id,
            config,
            username,
            achievements,
            cloud_saves,
            stats,
            leaderboards,
            entitlements,
            ugc,
            force,
        } => {
            let game_id = resolve_game_id(game_id.as_deref(), &config)?;
            handle_clear_playtest_data(ClearPlaytestDataArgs {
                game_id: &game_id,
                username: username.as_deref(),
                achievements,
                cloud_saves,
                stats,
                leaderboards,
                entitlements,
                ugc,
                force,
            })
            .await?;
        }
        Commands::Update => {
            updater::run_update().await?;
        }
    }

    if let Some(handle) = update_handle {
        let _ = handle.join();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap only validates an argument definition when that subcommand's parser is
    /// built, so a duplicate short flag stays invisible until someone runs the one
    /// command that carries it. `debug_assert` walks the whole tree at once and
    /// panics on duplicate shorts, ids, and conflicting settings — cheap insurance
    /// every time a flag is added.
    #[test]
    fn every_subcommands_arguments_are_well_formed() {
        Cli::command().debug_assert();
    }

    /// A blank one would reach [`resolve_game_id`], which returns a typed flag
    /// verbatim, and land in a request URL as `/api/games//…`.
    ///
    /// Parses real argv because clap keeps `ValueParser::parse_ref` private. Value
    /// parsers run during parsing and missing required args are only reported after
    /// it, so the blank fails first even on subcommands with other required args.
    #[test]
    fn every_game_id_arg_rejects_a_blank_value() {
        fn walk(cmd: &clap::Command, path: &[String], checked: &mut Vec<String>) {
            if cmd
                .get_arguments()
                .any(|arg| arg.get_long() == Some("game-id"))
            {
                for blank in ["", "   ", "\t", "\n"] {
                    let mut argv = path.to_vec();
                    argv.push("--game-id".to_string());
                    argv.push(blank.to_string());

                    let err = Cli::command()
                        .try_get_matches_from(&argv)
                        .expect_err(&format!(
                            "`{}` accepted the blank --game-id {:?}",
                            path.join(" "),
                            blank
                        ));
                    assert_eq!(
                        err.kind(),
                        clap::error::ErrorKind::ValueValidation,
                        "`{}` rejected the blank --game-id {:?} for the wrong reason: {}",
                        path.join(" "),
                        blank,
                        err
                    );
                }
                checked.push(path.join(" "));
            }

            for sub in cmd.get_subcommands() {
                let mut sub_path = path.to_vec();
                sub_path.push(sub.get_name().to_string());
                walk(sub, &sub_path, checked);
            }
        }

        let cli = Cli::command();
        let mut checked = Vec::new();
        walk(&cli, &["wavedash".to_string()], &mut checked);

        assert!(
            checked.len() >= 8,
            "expected every --game-id arg to be checked, only saw: {:?}",
            checked
        );
    }

    #[test]
    fn achievement_list_accepts_game_id_and_json_output() {
        let cli = Cli::try_parse_from([
            "wavedash",
            "achievement",
            "list",
            "--game-id",
            "game-id",
            "--json",
        ])
        .expect("achievement list should be a valid command");

        match cli.command {
            Some(Commands::Achievement {
                action: AchievementCommands::List { game_id, json, .. },
            }) => {
                assert_eq!(game_id.as_deref(), Some("game-id"));
                assert!(json);
            }
            _ => panic!("parsed the wrong command"),
        }
    }

    #[test]
    fn upload_source_parses_the_plugin_and_defaults_to_the_cli() {
        fn push_source(argv: &[&str]) -> Option<UploadSource> {
            let parsed = Cli::try_parse_from(argv).expect("should parse");
            let Some(Commands::Build {
                action: BuildCommands::Push { upload_source, .. },
            }) = parsed.command
            else {
                panic!("`{:?}` did not parse as `build push`", argv);
            };
            upload_source
        }

        assert_eq!(
            push_source(&[
                "wavedash",
                "build",
                "push",
                "--upload-source",
                "godot-plugin"
            ]),
            Some(UploadSource::GodotPlugin)
        );
        assert_eq!(push_source(&["wavedash", "build", "push"]), None);
        assert_eq!(
            push_source(&["wavedash", "build", "push"]).unwrap_or_default(),
            UploadSource::Cli
        );

        // Not `try_parse_from`: `expect_err` needs the Ok type to be `Debug`, and
        // `Cli` isn't — the fix is here, not a `derive(Debug)` on `Cli`.
        for rejected in ["cli", "CLI", "web", "WEB", "godot", "GODOT_PLUGIN", ""] {
            let err = Cli::command()
                .try_get_matches_from(["wavedash", "build", "push", "--upload-source", rejected])
                .expect_err(&format!(
                    "`build push` accepted --upload-source {:?}",
                    rejected
                ));
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::InvalidValue,
                "--upload-source {:?} was rejected for the wrong reason: {}",
                rejected,
                err
            );
        }
    }
}
