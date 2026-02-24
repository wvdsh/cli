mod achievements;
mod api_client;
mod auth;
mod builds;
mod config;
mod config_cmd;
mod dev;
mod file_staging;
mod init;
mod stats;
mod updater;
mod version_cmd;

use anyhow::Result;
use auth::{login_with_browser, AuthManager, AuthSource};
use builds::handle_build_push;
use clap::{Parser, Subcommand};
use dev::handle_dev;
use std::path::PathBuf;

#[derive(Clone, clap::ValueEnum)]
enum EngineArg {
    Godot,
    Unity,
    Custom,
    #[value(name = "jsdos")]
    JsDos,
    Ruffle,
}

impl From<EngineArg> for config::EngineKind {
    fn from(arg: EngineArg) -> Self {
        match arg {
            EngineArg::Godot => config::EngineKind::Godot,
            EngineArg::Unity => config::EngineKind::Unity,
            EngineArg::Custom => config::EngineKind::Custom,
            EngineArg::JsDos => config::EngineKind::JsDos,
            EngineArg::Ruffle => config::EngineKind::Ruffle,
        }
    }
}

fn mask_token(token: &str) -> String {
    if token.len() > 10 {
        format!("{}...{}", &token[..6], &token[token.len() - 3..])
    } else {
        "***".to_string()
    }
}

#[derive(Parser)]
#[command(name = "wvdsh")]
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
        #[arg(long = "no-open", help = "Don't open the sandbox URL in the browser")]
        no_open: bool,
    },
    #[command(about = "Show or update wavedash.toml configuration")]
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommands>,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
    },
    #[command(about = "Initialize a new wavedash.toml config file")]
    Init {
        #[arg(long)]
        game_id: String,
        #[arg(long)]
        branch: String,
        #[arg(long, value_enum)]
        engine: Option<EngineArg>,
    },
    #[command(about = "Check for and install updates")]
    Update,
    #[command(about = "Version management")]
    Version {
        #[command(subcommand)]
        action: VersionCommands,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
    },
    #[command(about = "Manage game stats")]
    Stats {
        #[command(subcommand)]
        action: stats::StatsCommands,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
    },
    #[command(about = "Manage game achievements")]
    Achievements {
        #[command(subcommand)]
        action: achievements::AchievementsCommands,
        #[arg(
            short = 'c',
            long = "config",
            help = "Path to wavedash.toml config file",
            default_value = "./wavedash.toml"
        )]
        config: PathBuf,
    },
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
enum ConfigCommands {
    #[command(about = "Set a config field value")]
    Set {
        #[arg(help = "Config key (branch, version, upload_dir, game_id)")]
        key: String,
        #[arg(help = "New value")]
        value: String,
    },
}

#[derive(Subcommand)]
enum VersionCommands {
    #[command(about = "Bump the version (patch, minor, or major)")]
    Bump {
        #[arg(value_enum, default_value = "patch")]
        level: version_cmd::BumpLevel,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install rustls crypto provider for TLS
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let cli = Cli::parse();

    // Check for updates (shows cached from previous run, spawns background check for next)
    let update_handle = updater::check_for_updates();

    match cli.command {
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
                            println!("✓ Authenticated (via WVDSH_TOKEN environment variable)");
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
                            println!("Not authenticated. Run 'wvdsh auth login' or set WVDSH_TOKEN environment variable.");
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
        Commands::Dev { config, no_open } => {
            handle_dev(config, cli.verbose, no_open).await?;
        }
        Commands::Config { action, config } => match action {
            Some(ConfigCommands::Set { key, value }) => {
                config_cmd::handle_config_set(config, key, value)?;
            }
            None => {
                config_cmd::handle_config_show(config)?;
            }
        },
        Commands::Init {
            game_id,
            branch,
            engine,
        } => {
            init::handle_init(game_id, branch, engine.map(|e| e.into()))?;
        }
        Commands::Update => {
            updater::run_update().await?;
        }
        Commands::Version { action, config } => match action {
            VersionCommands::Bump { level } => {
                version_cmd::handle_version_bump(config, level)?;
            }
        },
        Commands::Stats { action, config } => {
            stats::handle_stats(config, action).await?;
        }
        Commands::Achievements { action, config } => {
            achievements::handle_achievements(config, action).await?;
        }
    }

    // Wait for background update check to complete
    let _ = update_handle.join();

    Ok(())
}
