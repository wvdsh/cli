mod auth;
mod builds;
mod config;
mod dev;
mod file_staging;

use anyhow::Result;
use auth::{login_with_browser, AuthManager};
use builds::handle_build_push;
use clap::{Parser, Subcommand};
use dev::handle_dev;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "wvdsh")]
#[command(about = "Cross-platform CLI tool for uploading game projects to wavedash.gg")]
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
    },
}


#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Auth { action } => {
            let auth_manager = AuthManager::new()?;

            match action {
                AuthCommands::Login { token } => {
                    if let Some(api_key) = token {
                        // Manual token input
                        auth_manager.store_api_key(&api_key)?;
                        println!("✓ Successfully stored API key");
                    } else {
                        // Browser-based login
                        match login_with_browser() {
                            Ok(api_key) => {
                                auth_manager.store_api_key(&api_key)?;
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
                    if auth_manager.is_authenticated() {
                        println!("✓ Authenticated");
                        if let Some(api_key) = auth_manager.get_api_key() {
                            let preview = if api_key.len() > 10 {
                                format!("{}...{}", &api_key[..6], &api_key[api_key.len() - 3..])
                            } else {
                                "***".to_string()
                            };
                            println!("API Key: {}", preview);
                        }
                    } else {
                        println!("Not authenticated. Run 'wvdsh auth login' to get started.");
                    }
                }
            }
        }
        Commands::Build { action } => match action {
            BuildCommands::Push { config } => {
                handle_build_push(config, cli.verbose).await?;
            }
        },
        Commands::Dev { config } => {
            handle_dev(config, cli.verbose).await?;
        }
    }

    Ok(())
}
