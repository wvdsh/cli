mod auth;
mod builds;
mod config;
mod dev;
mod file_staging;
mod init;
mod updater;

use anyhow::Result;
use auth::{login_with_browser, AuthManager, AuthSource};
use builds::handle_build_push;
use clap::{Parser, Subcommand};
use colored::Colorize;
use dev::handle_dev;
use init::{handle_init, handle_project_create, handle_team_create};
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
        #[arg(long = "no-open", help = "Don't open the sandbox URL in the browser")]
        no_open: bool,
    },
    Team {
        #[command(subcommand)]
        action: TeamCommands,
    },
    Project {
        #[command(subcommand)]
        action: ProjectCommands,
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
        Commands::Dev { config, no_open } => {
            handle_dev(config, cli.verbose, no_open).await?;
        }
        Commands::Team { action } => match action {
            TeamCommands::Create { name } => {
                handle_team_create(&name).await?;
            }
        },
        Commands::Project { action } => match action {
            ProjectCommands::Create { title, team_id } => {
                handle_project_create(&title, &team_id).await?;
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
