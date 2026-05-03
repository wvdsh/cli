mod achievements;
mod auth;
mod builds;
mod config;
mod dev;
mod file_staging;
mod init;
mod stats;
mod updater;

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
    handle_init, handle_project_create, handle_project_list, handle_team_create, handle_team_list,
};
use stats::{handle_stat_create, handle_stat_delete, handle_stat_update};
use std::path::PathBuf;

fn mask_token(token: &str) -> String {
    if token.len() > 10 {
        format!("{}...{}", &token[..6], &token[token.len() - 3..])
    } else {
        "***".to_string()
    }
}

#[derive(Parser)]
#[command(name = "wavedash")]
#[command(about = "Cross-platform CLI tool for uploading game projects to wavedash.com")]
#[command(version)]
struct Cli {
    #[arg(long, global = true, help = "Enable verbose output")]
    verbose: bool,
    #[command(subcommand)]
    command: Commands,
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
        #[arg(
            long,
            help = "Path to a new image file (jpg, jpeg, png, webp)"
        )]
        image: Option<PathBuf>,
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
        #[arg(
            long,
            help = "Required if any user has unlocked this achievement"
        )]
        force: bool,
    },
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

    // Check for updates in background, but skip if the user is already running `update`
    let update_handle = if matches!(cli.command, Commands::Update) {
        None
    } else {
        Some(updater::check_for_update())
    };

    match cli.command {
        Commands::Init => {
            handle_init().await?;
        }
        Commands::Auth { action } => {
            let auth_manager = AuthManager::new()?;

            match action {
                AuthCommands::Login { token } => {
                    if let Some(api_key) = token {
                        // Manual token input (no email available)
                        auth_manager.store_credentials(&api_key, None)?;
                        println!("✓ Successfully stored API key");
                    } else {
                        // Browser-based login
                        match login_with_browser() {
                            Ok(result) => {
                                auth_manager.store_credentials(&result.api_key, result.email.as_deref())?;
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
            BuildCommands::Push { config, message } => {
                handle_build_push(config, cli.verbose, message).await?;
            }
        },
        Commands::Dev { config } => {
            handle_dev(config, cli.verbose).await?;
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
        Commands::Achievement { action } => match action {
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
                let triggered: Option<Option<&str>> = triggered_by_stat_id.as_deref().map(|s| {
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
        },
        Commands::Update => {
            updater::run_update().await?;
        }
    }

    if let Some(handle) = update_handle {
        let _ = handle.join();
    }

    Ok(())
}
