mod auth;
mod builds;
mod config;
mod dev;
mod file_staging;
mod updater;

use anyhow::Result;
use auth::{login_with_browser, AuthManager, AuthSource};
use builds::handle_build_push;
use clap::{Parser, Subcommand};
use dev::handle_dev;
use std::path::PathBuf;

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
                    match auth_manager.get_auth_source() {
                        AuthSource::Environment => {
                            println!("✓ Authenticated (via WVDSH_TOKEN environment variable)");
                            if let Some(api_key) = auth_manager.get_api_key() {
                                let preview = if api_key.len() > 10 {
                                    format!("{}...{}", &api_key[..6], &api_key[api_key.len() - 3..])
                                } else {
                                    "***".to_string()
                                };
                                println!("Token: {}", preview);
                            }
                        }
                        AuthSource::File => {
                            println!("✓ Authenticated (via stored credentials)");
                            if let Some(api_key) = auth_manager.get_api_key() {
                                let preview = if api_key.len() > 10 {
                                    format!("{}...{}", &api_key[..6], &api_key[api_key.len() - 3..])
                                } else {
                                    "***".to_string()
                                };
                                println!("API Key: {}", preview);
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
            BuildCommands::Push { config } => {
                handle_build_push(config, cli.verbose).await?;
            }
        },
        Commands::Dev { config, no_open } => {
            handle_dev(config, cli.verbose, no_open).await?;
        }
        Commands::Update => {
            updater::run_update().await?;
        }
    }

    // Wait for background update check to complete
    let _ = update_handle.join();

    Ok(())
}
