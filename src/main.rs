mod auth;
mod builds;
mod config;
mod rclone;

use anyhow::Result;
use clap::{Parser, Subcommand};
use auth::{AuthManager, login_with_browser};
use builds::handle_build_push;

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
        #[arg(help = "Target in format: org_slug/game_slug:branch_slug")]
        target: String,
        #[arg(short = 'e', long, help = "Game engine (e.g., godot, unity, unreal)")]
        engine: String,
        #[arg(short = 'v', long = "version", help = "Engine version (e.g., 4.5-beta)")]
        engine_version: String,
        #[arg(help = "Source directory to upload", default_value = ".")]
        source: std::path::PathBuf,
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
                                format!("{}...{}", &api_key[..6], &api_key[api_key.len()-3..])
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
        Commands::Build { action } => {
            match action {
                BuildCommands::Push { target, engine, engine_version, source } => {
                    handle_build_push(target, engine, engine_version, source, cli.verbose).await?;
                }
            }
        }
    }

    Ok(())
}
