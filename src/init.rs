use crate::auth::{AuthManager, AuthSource};
use crate::config;
use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

// ── API response types ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct Organization {
    _id: String,
    name: String,
    slug: String,
}

#[derive(Debug, Deserialize)]
struct OrganizationsResponse {
    organizations: Vec<Organization>,
}

#[derive(Debug, Clone, Deserialize)]
struct Game {
    _id: String,
    title: String,
    slug: String,
}

#[derive(Debug, Deserialize)]
struct GamesResponse {
    games: Vec<Game>,
}

// ── Engine detection ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum EngineType {
    Godot,
    Unity,
    Custom,
}

impl EngineType {
    fn default_upload_dir(&self) -> &str {
        match self {
            EngineType::Godot => "build",
            EngineType::Unity => "build",
            EngineType::Custom => "dist",
        }
    }
}

struct DetectedEngine {
    engine_type: EngineType,
    /// Best-guess version from project files (used as placeholder)
    version_hint: Option<String>,
}

/// Look for project.godot in the current directory and parse the version.
fn detect_godot(dir: &Path) -> Option<DetectedEngine> {
    let project_file = dir.join("project.godot");
    let content = std::fs::read_to_string(project_file).ok()?;

    // Godot 4.x: config/features=PackedStringArray("4.4", ...)
    // This only gives major.minor — patch version isn't stored in project files.
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("config/features") {
            if let Some(start) = line.find('"') {
                if let Some(end) = line[start + 1..].find('"') {
                    let version = &line[start + 1..start + 1 + end];
                    if version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        return Some(DetectedEngine {
                            engine_type: EngineType::Godot,
                            version_hint: Some(version.to_string()),
                        });
                    }
                }
            }
        }
    }

    // Godot 3.x: no config/features, but has config_version=3|4
    for line in content.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("config_version=") {
            let hint = match val.trim() {
                "3" => "3.0",
                "4" => "3.1",
                _ => continue,
            };
            return Some(DetectedEngine {
                engine_type: EngineType::Godot,
                version_hint: Some(hint.to_string()),
            });
        }
    }

    // project.godot exists but we couldn't parse any version info
    Some(DetectedEngine {
        engine_type: EngineType::Godot,
        version_hint: None,
    })
}

/// Look for Unity project markers and parse ProjectVersion.txt.
/// Unity stores the exact editor version (e.g., "2022.3.12f1").
fn detect_unity(dir: &Path) -> Option<DetectedEngine> {
    let version_file = dir.join("ProjectSettings/ProjectVersion.txt");
    if let Ok(content) = std::fs::read_to_string(version_file) {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("m_EditorVersion:") {
                return Some(DetectedEngine {
                    engine_type: EngineType::Unity,
                    version_hint: Some(rest.trim().to_string()),
                });
            }
        }
    }

    if dir.join("Assets").is_dir() {
        return Some(DetectedEngine {
            engine_type: EngineType::Unity,
            version_hint: None,
        });
    }

    None
}

fn detect_engine(dir: &Path) -> DetectedEngine {
    if let Some(engine) = detect_godot(dir) {
        return engine;
    }
    if let Some(engine) = detect_unity(dir) {
        return engine;
    }
    DetectedEngine {
        engine_type: EngineType::Custom,
        version_hint: None,
    }
}

// ── API helpers ──────────────────────────────────────────────────────

async fn fetch_organizations(api_key: &str) -> Result<Vec<Organization>> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/organizations", api_host);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let data: OrganizationsResponse = resp.json().await?;
    Ok(data.organizations)
}

async fn create_organization(api_key: &str, name: &str) -> Result<Organization> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/organizations", api_host);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let org: Organization = resp.json().await?;
    Ok(org)
}

async fn fetch_games(api_key: &str, org_id: &str) -> Result<Vec<Game>> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/organizations/{}/games", api_host, org_id);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let data: GamesResponse = resp.json().await?;
    Ok(data.games)
}

async fn create_game(api_key: &str, org_id: &str, title: &str) -> Result<Game> {
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/organizations/{}/games", api_host, org_id);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&serde_json::json!({ "title": title }))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let game: Game = resp.json().await?;
    Ok(game)
}

// ── TOML generation ──────────────────────────────────────────────────

fn generate_toml(
    game_id: &str,
    upload_dir: &str,
    engine_type: &EngineType,
    engine_version: Option<&str>,
) -> String {
    let mut toml = format!(
        "game_id = \"{}\"\nupload_dir = \"{}\"\n",
        game_id, upload_dir
    );

    match engine_type {
        EngineType::Godot => {
            let version = engine_version.unwrap_or("4.0");
            toml.push_str(&format!("\n[godot]\nversion = \"{}\"\n", version));
        }
        EngineType::Unity => {
            let version = engine_version.unwrap_or("2022.3");
            toml.push_str(&format!("\n[unity]\nversion = \"{}\"\n", version));
        }
        EngineType::Custom => {
            toml.push_str("\nentrypoint = \"index.html\"\n");
        }
    }

    toml
}

// ── Main init handler ────────────────────────────────────────────────

const CREATE_NEW_SENTINEL: &str = "__create_new__";

pub async fn handle_init() -> Result<()> {
    cliclack::intro("wavedash init")?;

    // 1. Check authentication
    let auth_manager = AuthManager::new()?;
    let auth_info = auth_manager.get_auth_info();
    let api_key = match auth_info.source {
        AuthSource::None => {
            cliclack::outro_cancel(
                "Not authenticated. Run `wavedash auth login` first.",
            )?;
            std::process::exit(1);
        }
        _ => auth_info.api_key.unwrap(),
    };

    // 2. Check for existing wavedash.toml
    let config_path = PathBuf::from("wavedash.toml");
    if config_path.exists() {
        let overwrite: bool = cliclack::confirm(
            "A wavedash.toml already exists. Do you want to reinitialize?",
        )
        .interact()?;

        if !overwrite {
            cliclack::outro("Keeping existing configuration.")?;
            return Ok(());
        }
    }

    // 3. Detect engine and resolve version
    let current_dir = std::env::current_dir()?;
    let detected = detect_engine(&current_dir);

    // Only prompt for version when we detect Godot/Unity.
    // For web builds (threejs, phaser, custom, etc.) no engine config is needed.
    let engine_version: Option<String> = match &detected.engine_type {
        EngineType::Godot => {
            let placeholder = detected.version_hint.as_deref().unwrap_or("4.4");
            let version: String = cliclack::input("Godot version")
                .placeholder(placeholder)
                .default_input(placeholder)
                .interact()?;
            Some(version)
        }
        EngineType::Unity => {
            if let Some(ref hint) = detected.version_hint {
                cliclack::log::info(format!("Detected Unity version: {}", hint))?;
                Some(hint.clone())
            } else {
                let version: String = cliclack::input("Unity version")
                    .placeholder("2022.3")
                    .default_input("2022.3")
                    .interact()?;
                Some(version)
            }
        }
        EngineType::Custom => None,
    };

    // 4. Select organization
    let spinner = cliclack::spinner();
    spinner.start("Fetching your organizations...");
    let orgs = fetch_organizations(&api_key).await?;
    spinner.stop("Fetched organizations");

    let selected_org = if orgs.is_empty() {
        // No orgs — must create one
        cliclack::log::info("You don't have any organizations yet.")?;
        let name: String = cliclack::input("Organization name")
            .placeholder("My Studio")
            .validate(|input: &String| {
                if input.trim().is_empty() {
                    Err("Name cannot be empty")
                } else {
                    Ok(())
                }
            })
            .interact()?;

        let spinner = cliclack::spinner();
        spinner.start("Creating organization...");
        let org = create_organization(&api_key, &name).await?;
        spinner.stop(format!("Created organization: {}", org.name));
        org
    } else {
        // Build select with orgs + "Create new" option
        let mut select = cliclack::select("Select an organization").filter_mode();
        for org in &orgs {
            select = select.item(org._id.clone(), &org.name, &org.slug);
        }
        select = select.item(CREATE_NEW_SENTINEL.to_string(), "Create new organization", "");

        let choice: String = select.interact()?;

        if choice == CREATE_NEW_SENTINEL {
            let name: String = cliclack::input("Organization name")
                .placeholder("My Studio")
                .validate(|input: &String| {
                    if input.trim().is_empty() {
                        Err("Name cannot be empty")
                    } else {
                        Ok(())
                    }
                })
                .interact()?;

            let spinner = cliclack::spinner();
            spinner.start("Creating organization...");
            let org = create_organization(&api_key, &name).await?;
            spinner.stop(format!("Created organization: {}", org.name));
            org
        } else {
            orgs.into_iter().find(|o| o._id == choice).unwrap()
        }
    };

    // 5. Select game
    let spinner = cliclack::spinner();
    spinner.start("Fetching games...");
    let games = fetch_games(&api_key, &selected_org._id).await?;
    spinner.stop("Fetched games");

    let selected_game = if games.is_empty() {
        cliclack::log::info("No games in this organization yet.")?;
        let title: String = cliclack::input("Game title")
            .placeholder("My Game")
            .validate(|input: &String| {
                if input.trim().is_empty() {
                    Err("Title cannot be empty")
                } else {
                    Ok(())
                }
            })
            .interact()?;

        let spinner = cliclack::spinner();
        spinner.start("Creating game...");
        let game = create_game(&api_key, &selected_org._id, &title).await?;
        spinner.stop(format!("Created game: {}", game.title));
        game
    } else {
        let mut select = cliclack::select("Select a game").filter_mode();
        for game in &games {
            select = select.item(game._id.clone(), &game.title, &game.slug);
        }
        select = select.item(CREATE_NEW_SENTINEL.to_string(), "Create new game", "");

        let choice: String = select.interact()?;

        if choice == CREATE_NEW_SENTINEL {
            let title: String = cliclack::input("Game title")
                .placeholder("My Game")
                .validate(|input: &String| {
                    if input.trim().is_empty() {
                        Err("Title cannot be empty")
                    } else {
                        Ok(())
                    }
                })
                .interact()?;

            let spinner = cliclack::spinner();
            spinner.start("Creating game...");
            let game = create_game(&api_key, &selected_org._id, &title).await?;
            spinner.stop(format!("Created game: {}", game.title));
            game
        } else {
            games.into_iter().find(|g| g._id == choice).unwrap()
        }
    };

    // 6. Ask for build output directory
    let default_dir = detected.engine_type.default_upload_dir();
    let upload_dir: String = cliclack::input("Build output directory")
        .placeholder(default_dir)
        .default_input(default_dir)
        .interact()?;

    // 7. Write wavedash.toml
    let toml_content = generate_toml(
        &selected_game._id,
        &upload_dir,
        &detected.engine_type,
        engine_version.as_deref(),
    );
    std::fs::write(&config_path, &toml_content)?;

    let website_host = config::get("open_browser_website_host")?;
    cliclack::outro(format!(
        "Created wavedash.toml! Next steps:\n  → Run `wavedash dev` to test locally\n  → Run `wavedash build push` to upload a build\n  → Manage your game at {}/developer/{}/{}",
        website_host, selected_org.slug, selected_game.slug
    ))?;

    Ok(())
}
