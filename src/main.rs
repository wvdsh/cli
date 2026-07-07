mod achievements;
mod auth;
mod builds;
mod config;
mod dev;
mod file_staging;
mod init;
mod publish;
mod stats;
mod updater;
mod welcome;

use achievements::{
    handle_achievement_create, handle_achievement_delete, handle_achievement_update,
    CreateAchievementArgs, UpdateAchievementArgs,
};
use anyhow::Result;
use auth::{login_with_browser, AuthManager, AuthSource};
use builds::handle_build_push;
use clap::{Parser, Subcommand};
use colored::Colorize;
use config::resolve_game_id;
use dev::handle_dev;
use init::{
    handle_init, handle_init_scripted, handle_project_create, handle_project_list,
    handle_team_create, handle_team_list, InitArgs, InitEngine,
};
use publish::{handle_publish, PublishArgs};
use serde::Serialize;
use stats::{handle_stat_create, handle_stat_delete, handle_stat_update};
use std::io::{IsTerminal, Read};
use std::path::PathBuf;

const CLAUDE_CODE_PLUGIN_HINT: &str =
    r#"<claude-code-hint v="1" type="plugin" value="wavedash@claude-plugins-official" />"#;

fn mask_token(token: &str) -> String {
    if token.len() > 10 {
        format!("{}...{}", &token[..6], &token[token.len() - 3..])
    } else {
        "***".to_string()
    }
}

fn parse_non_empty_arg(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err("value cannot be empty".to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

#[derive(Parser)]
#[command(name = "wavedash")]
#[command(about = "Cross-platform CLI tool for uploading game projects to wavedash.com")]
#[command(version)]
struct Cli {
    #[arg(long, global = true, help = "Enable verbose output")]
    verbose: bool,
    #[arg(long, global = true, help = "Disable the background update check")]
    no_update_check: bool,
    #[arg(long, global = true, help = "Disable colored output")]
    no_color: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Initialize a wavedash.toml config for this project")]
    Init {
        #[arg(
            long = "team-id",
            conflicts_with = "team_name",
            value_parser = parse_non_empty_arg,
            help = "Use an existing team ID"
        )]
        team_id: Option<String>,
        #[arg(
            long = "team-name",
            conflicts_with = "team_id",
            value_parser = parse_non_empty_arg,
            help = "Create a new team with this name"
        )]
        team_name: Option<String>,
        #[arg(
            long = "game-id",
            conflicts_with = "game_title",
            value_parser = parse_non_empty_arg,
            help = "Use an existing game ID"
        )]
        game_id: Option<String>,
        #[arg(
            long = "game-title",
            conflicts_with = "game_id",
            value_parser = parse_non_empty_arg,
            help = "Create a new game with this title"
        )]
        game_title: Option<String>,
        #[arg(
            long = "upload-dir",
            value_parser = parse_non_empty_arg,
            help = "Build output directory to write to wavedash.toml"
        )]
        upload_dir: Option<String>,
        #[arg(
            long,
            value_enum,
            help = "Engine config to write. auto detects from the current directory"
        )]
        engine: Option<InitEngine>,
        #[arg(
            long = "engine-version",
            value_parser = parse_non_empty_arg,
            help = "Engine version for Godot or Unity configs"
        )]
        engine_version: Option<String>,
        #[arg(long, help = "Overwrite an existing wavedash.toml")]
        force: bool,
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
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
        #[arg(long, help = "Output as JSON")]
        json: bool,
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
    #[command(about = "Check for and install updates")]
    Update,
}

#[derive(Subcommand)]
enum AuthCommands {
    Login {
        #[arg(long, help = "API key for manual authentication")]
        token: Option<String>,
        #[arg(
            long = "token-stdin",
            conflicts_with = "token",
            help = "Read API key from stdin"
        )]
        token_stdin: bool,
    },
    Logout,
    Status {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
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
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TeamCommands {
    #[command(about = "Create a new team")]
    Create {
        #[arg(long, help = "Team name")]
        name: String,
        #[arg(long, help = "Output as JSON")]
        json: bool,
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
        #[arg(long, help = "Output as JSON")]
        json: bool,
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
            help = "Game ID (defaults to game_id in wavedash.toml)"
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
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Update a stat's identifier and display name")]
    Update {
        #[arg(
            long = "game-id",
            help = "Game ID (defaults to game_id in wavedash.toml)"
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
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Delete a stat")]
    Delete {
        #[arg(
            long = "game-id",
            help = "Game ID (defaults to game_id in wavedash.toml)"
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
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
}

#[derive(Subcommand)]
enum AchievementCommands {
    #[command(about = "Create a new achievement for a game")]
    Create {
        #[arg(
            long = "game-id",
            help = "Game ID (defaults to game_id in wavedash.toml)"
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
            help = "Path to an image file (jpg, jpeg, png, webp) to use as the achievement icon"
        )]
        image: Option<PathBuf>,
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Update an achievement")]
    Update {
        #[arg(
            long = "game-id",
            help = "Game ID (defaults to game_id in wavedash.toml)"
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
        #[arg(long, help = "Path to a new image file (jpg, jpeg, png, webp)")]
        image: Option<PathBuf>,
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Delete an achievement")]
    Delete {
        #[arg(
            long = "game-id",
            help = "Game ID (defaults to game_id in wavedash.toml)"
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
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
}

#[derive(Serialize)]
struct AuthStatusOutput {
    authenticated: bool,
    source: &'static str,
    email: Option<String>,
    #[serde(rename = "tokenPreview")]
    token_preview: Option<String>,
    #[serde(rename = "recommendedActions", skip_serializing_if = "Option::is_none")]
    recommended_actions: Option<Vec<&'static str>>,
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            !value.is_empty() && value != "0" && value != "false"
        })
        .unwrap_or(false)
}

fn is_browser_login_unavailable() -> bool {
    env_flag_enabled("CI") || !std::io::stdin().is_terminal()
}

fn command_outputs_json(command: &Commands) -> bool {
    match command {
        Commands::Init { json, .. } => *json,
        Commands::Auth {
            action: AuthCommands::Status { json },
        } => *json,
        Commands::Build {
            action: BuildCommands::Push { json, .. },
        } => *json,
        Commands::Publish { json, .. } => *json,
        Commands::Team {
            action: TeamCommands::Create { json, .. } | TeamCommands::List { json, .. },
        } => *json,
        Commands::Project {
            action: ProjectCommands::Create { json, .. } | ProjectCommands::List { json, .. },
        } => *json,
        Commands::Stat {
            action:
                StatCommands::Create { json, .. }
                | StatCommands::Update { json, .. }
                | StatCommands::Delete { json, .. },
        } => *json,
        Commands::Achievement {
            action:
                AchievementCommands::Create { json, .. }
                | AchievementCommands::Update { json, .. }
                | AchievementCommands::Delete { json, .. },
        } => *json,
        _ => false,
    }
}

fn command_suppresses_update_check(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Update
            | Commands::Auth {
                action: AuthCommands::Login { .. },
            }
    )
}

fn read_token_from_stdin() -> Result<String> {
    let mut token = String::new();
    std::io::stdin().read_to_string(&mut token)?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("No token provided on stdin");
    }
    Ok(token)
}

fn auth_status_output(auth_info: auth::AuthInfo) -> AuthStatusOutput {
    match auth_info.source {
        AuthSource::Environment => AuthStatusOutput {
            authenticated: true,
            source: "environment",
            email: None,
            token_preview: auth_info.api_key.as_deref().map(mask_token),
            recommended_actions: None,
        },
        AuthSource::File => AuthStatusOutput {
            authenticated: true,
            source: "file",
            email: auth_info.email,
            token_preview: auth_info.api_key.as_deref().map(mask_token),
            recommended_actions: None,
        },
        AuthSource::None => AuthStatusOutput {
            authenticated: false,
            source: "none",
            email: None,
            token_preview: None,
            recommended_actions: Some(vec![
                "Set WAVEDASH_TOKEN",
                "Run wavedash auth login --token-stdin",
                "Run wavedash auth login",
            ]),
        },
    }
}

fn maybe_emit_claude_plugin_hint() {
    let is_claude_child_session = std::env::var_os("CLAUDE_CODE_CHILD_SESSION").is_some();
    let is_claude_captured_command =
        std::env::var_os("CLAUDECODE").is_some() && !std::io::stderr().is_terminal();

    if is_claude_child_session || is_claude_captured_command {
        eprintln!("{CLAUDE_CODE_PLUGIN_HINT}");
    }
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
    maybe_emit_claude_plugin_hint();

    if cli.no_color || std::env::var_os("NO_COLOR").is_some() {
        colored::control::set_override(false);
    }

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

    let update_handle = if command_suppresses_update_check(&command)
        || cli.no_update_check
        || command_outputs_json(&command)
        || env_flag_enabled("WAVEDASH_NO_UPDATE_CHECK")
    {
        None
    } else {
        Some(updater::check_for_update())
    };

    match command {
        Commands::Init {
            team_id,
            team_name,
            game_id,
            game_title,
            upload_dir,
            engine,
            engine_version,
            force,
            json,
        } => {
            let args = InitArgs {
                team_id,
                team_name,
                game_id,
                game_title,
                upload_dir,
                engine,
                engine_version,
                force,
                json,
            };
            if args.is_interactive() {
                handle_init().await?;
            } else {
                handle_init_scripted(args).await?;
            }
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
                        if is_browser_login_unavailable() {
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
                    println!("✓ Successfully logged out");
                }
                AuthCommands::Status { json } => {
                    let auth_info = auth_manager.get_auth_info();
                    if json {
                        print_json(&auth_status_output(auth_info))?;
                        return Ok(());
                    }
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
                            println!(
                                "Not authenticated. Run 'wavedash auth login' or set WAVEDASH_TOKEN environment variable."
                            );
                        }
                    }
                }
            }
        }
        Commands::Build { action } => match action {
            BuildCommands::Push {
                config,
                message,
                json,
            } => {
                handle_build_push(config, cli.verbose, message, json).await?;
            }
        },
        Commands::Dev { config, no_open } => {
            handle_dev(config, cli.verbose, no_open).await?;
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
            json,
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
                json,
            })
            .await?;
        }
        Commands::Team { action } => match action {
            TeamCommands::Create { name, json } => {
                handle_team_create(&name, json).await?;
            }
            TeamCommands::List { json } => {
                handle_team_list(json).await?;
            }
        },
        Commands::Project { action } => match action {
            ProjectCommands::Create {
                title,
                team_id,
                json,
            } => {
                handle_project_create(&title, &team_id, json).await?;
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
                json,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_stat_create(&game_id, &identifier, &name, json).await?;
            }
            StatCommands::Update {
                game_id,
                config,
                id,
                identifier,
                name,
                json,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_stat_update(&game_id, &id, &identifier, &name, json).await?;
            }
            StatCommands::Delete {
                game_id,
                config,
                id,
                force,
                json,
            } => {
                let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                handle_stat_delete(&game_id, &id, force, json).await?;
            }
        },
        Commands::Achievement { action } => {
            match action {
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
                    json,
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
                        json,
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
                    json,
                } => {
                    // CLI convention: --triggered-by-stat-id "" clears, omitted leaves alone
                    let triggered: Option<Option<&str>> = triggered_by_stat_id
                        .as_deref()
                        .map(|s| if s.is_empty() { None } else { Some(s) });
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
                        json,
                    })
                    .await?;
                }
                AchievementCommands::Delete {
                    game_id,
                    config,
                    id,
                    force,
                    json,
                } => {
                    let game_id = resolve_game_id(game_id.as_deref(), &config)?;
                    handle_achievement_delete(&game_id, &id, force, json).await?;
                }
            }
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
