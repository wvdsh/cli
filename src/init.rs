use crate::auth::{AuthManager, AuthSource};
use crate::config;
use anyhow::Result;
use clap::ValueEnum;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── API response types ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Organization {
    _id: String,
    name: String,
    slug: String,
}

#[derive(Debug, Deserialize)]
struct OrganizationsResponse {
    organizations: Vec<Organization>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Game {
    _id: String,
    title: String,
    slug: String,
}

#[derive(Debug, Deserialize)]
struct GamesResponse {
    games: Vec<Game>,
}

#[derive(Debug, Serialize)]
struct TeamCreateOutput {
    team: Organization,
    url: String,
}

#[derive(Debug, Serialize)]
struct ProjectCreateOutput {
    project: Game,
    team: Organization,
    url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InitEngine {
    Auto,
    Custom,
    Godot,
    Unity,
}

pub struct InitArgs {
    pub team_id: Option<String>,
    pub team_name: Option<String>,
    pub game_id: Option<String>,
    pub game_title: Option<String>,
    pub upload_dir: Option<String>,
    pub engine: Option<InitEngine>,
    pub engine_version: Option<String>,
    pub force: bool,
    pub json: bool,
}

impl InitArgs {
    pub fn is_interactive(&self) -> bool {
        self.team_id.is_none()
            && self.team_name.is_none()
            && self.game_id.is_none()
            && self.game_title.is_none()
            && self.upload_dir.is_none()
            && self.engine.is_none()
            && self.engine_version.is_none()
            && !self.force
            && !self.json
    }
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

    fn as_json_label(&self) -> &'static str {
        match self {
            EngineType::Godot => "godot",
            EngineType::Unity => "unity",
            EngineType::Custom => "custom",
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

#[derive(Debug, Serialize)]
struct InitOutput {
    #[serde(rename = "configPath")]
    config_path: String,
    team: Organization,
    game: Game,
    #[serde(rename = "uploadDir")]
    upload_dir: String,
    engine: &'static str,
    #[serde(rename = "engineVersion", skip_serializing_if = "Option::is_none")]
    engine_version: Option<String>,
    url: String,
}

fn resolve_init_engine(engine: Option<InitEngine>, detected: &DetectedEngine) -> EngineType {
    match engine.unwrap_or(InitEngine::Auto) {
        InitEngine::Auto => detected.engine_type.clone(),
        InitEngine::Custom => EngineType::Custom,
        InitEngine::Godot => EngineType::Godot,
        InitEngine::Unity => EngineType::Unity,
    }
}

fn resolve_init_engine_version(
    engine_type: &EngineType,
    detected: &DetectedEngine,
    engine_version: Option<String>,
) -> Option<String> {
    match engine_type {
        EngineType::Godot => engine_version
            .or_else(|| detected.version_hint.clone())
            .or_else(|| Some("4.0".to_string())),
        EngineType::Unity => engine_version
            .or_else(|| detected.version_hint.clone())
            .or_else(|| Some("2022.3".to_string())),
        EngineType::Custom => None,
    }
}

fn validate_scripted_init_args(args: &InitArgs) -> Result<()> {
    if args.team_id.is_none() && args.team_name.is_none() {
        anyhow::bail!(
            "Non-interactive init requires --team-id to use an existing team or --team-name to create one."
        );
    }
    if args.game_id.is_none() && args.game_title.is_none() {
        anyhow::bail!(
            "Non-interactive init requires --game-id to use an existing game or --game-title to create one."
        );
    }
    Ok(())
}

pub async fn handle_init_scripted(args: InitArgs) -> Result<()> {
    validate_scripted_init_args(&args)?;

    let config_path = PathBuf::from("wavedash.toml");
    if config_path.exists() && !args.force {
        anyhow::bail!("wavedash.toml already exists. Pass --force to overwrite it.");
    }

    let auth_manager = AuthManager::new()?;
    let auth_info = auth_manager.get_auth_info();
    let api_key = match auth_info.source {
        AuthSource::None => anyhow::bail!(
            "Not authenticated. Set WAVEDASH_TOKEN or run `wavedash auth login` first."
        ),
        _ => auth_info.api_key.unwrap(),
    };

    let current_dir = std::env::current_dir()?;
    let detected = detect_engine(&current_dir);
    let engine_type = resolve_init_engine(args.engine, &detected);
    let provided_engine_version = args.engine_version.is_some();
    let engine_version =
        resolve_init_engine_version(&engine_type, &detected, args.engine_version);
    if provided_engine_version && matches!(engine_type, EngineType::Custom) {
        anyhow::bail!("--engine-version is only valid for Godot or Unity configs.");
    }
    let upload_dir = args
        .upload_dir
        .unwrap_or_else(|| engine_type.default_upload_dir().to_string());

    let selected_org = if let Some(team_name) = args.team_name {
        create_organization(&api_key, &team_name).await?
    } else {
        let team_id = args.team_id.as_deref().expect("validated team id");
        fetch_organizations(&api_key)
            .await?
            .into_iter()
            .find(|org| org._id == team_id)
            .ok_or_else(|| anyhow::anyhow!("Team not found: {}", team_id))?
    };

    let selected_game = if let Some(game_title) = args.game_title {
        create_game(&api_key, &selected_org._id, &game_title).await?
    } else {
        let game_id = args.game_id.as_deref().expect("validated game id");
        fetch_games(&api_key, &selected_org._id)
            .await?
            .into_iter()
            .find(|game| game._id == game_id)
            .ok_or_else(|| anyhow::anyhow!("Game not found in selected team: {}", game_id))?
    };

    let toml_content = generate_toml(
        &selected_game._id,
        &upload_dir,
        &engine_type,
        engine_version.as_deref(),
    );
    std::fs::write(&config_path, &toml_content)?;

    let website_host = config::get("open_browser_website_host")?;
    let url = format!(
        "{}/dev-portal/{}/{}",
        website_host, selected_org.slug, selected_game.slug
    );

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&InitOutput {
                config_path: config_path.display().to_string(),
                team: selected_org,
                game: selected_game,
                upload_dir,
                engine: engine_type.as_json_label(),
                engine_version,
                url,
            })?
        );
        return Ok(());
    }

    println!("✓ Created wavedash.toml");
    println!("Game: {} ({})", selected_game.title, selected_game._id);
    println!("Manage at: {}", url);
    Ok(())
}

pub async fn handle_init() -> Result<()> {
    cliclack::intro("wavedash init")?;

    // 1. Check authentication
    let auth_manager = AuthManager::new()?;
    let auth_info = auth_manager.get_auth_info();
    let api_key = match auth_info.source {
        AuthSource::None => {
            cliclack::outro_cancel("Not authenticated. Run `wavedash auth login` first.")?;
            std::process::exit(1);
        }
        _ => auth_info.api_key.unwrap(),
    };

    // 2. Check for existing wavedash.toml
    let config_path = PathBuf::from("wavedash.toml");
    if config_path.exists() {
        let overwrite: bool =
            cliclack::confirm("A wavedash.toml already exists. Do you want to reinitialize?")
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
    spinner.start("Fetching your teams...");
    let orgs = fetch_organizations(&api_key).await?;
    spinner.stop("Fetched teams");

    let selected_org = if orgs.is_empty() {
        // No orgs — must create one
        cliclack::log::info("You don't have any teams yet.")?;
        let name: String = cliclack::input("Team name")
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
        spinner.start("Creating team...");
        let org = create_organization(&api_key, &name).await?;
        spinner.stop(format!("Created team: {}", org.name));
        org
    } else {
        // Build select with orgs + "Create new" option
        let mut select = cliclack::select("Select a team").filter_mode();
        for org in &orgs {
            select = select.item(org._id.clone(), &org.name, &org.slug);
        }
        select = select.item(CREATE_NEW_SENTINEL.to_string(), "Create new team", "");

        let choice: String = select.interact()?;

        if choice == CREATE_NEW_SENTINEL {
            let name: String = cliclack::input("Team name")
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
            spinner.start("Creating team...");
            let org = create_organization(&api_key, &name).await?;
            spinner.stop(format!("Created team: {}", org.name));
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
        cliclack::log::info("No games in this team yet.")?;
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
        "Created wavedash.toml! Next steps:\n  → Run `wavedash dev` to test locally\n  → Run `wavedash build push` to upload a build\n  → Manage your game at {}/dev-portal/{}/{}",
        website_host, selected_org.slug, selected_game.slug
    ))?;

    Ok(())
}

// ── Scripted create commands ─────────────────────────────────────────

fn require_api_key() -> Result<String> {
    let auth_manager = AuthManager::new()?;
    let auth_info = auth_manager.get_auth_info();
    match auth_info.source {
        AuthSource::None => {
            anyhow::bail!("Not authenticated. Run `wavedash auth login` first.")
        }
        _ => Ok(auth_info.api_key.unwrap()),
    }
}

pub async fn handle_team_create(name: &str, json: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let team = create_organization(&api_key, name).await?;
    let website_host = config::get("open_browser_website_host")?;
    let url = format!("{}/dev-portal/{}", website_host, team.slug);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&TeamCreateOutput { team, url })?
        );
        return Ok(());
    }
    println!("✓ Created team \"{}\" (id: {})", team.name, team._id);
    println!("  {}", url);
    Ok(())
}

pub async fn handle_project_create(title: &str, team_id: &str, json: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let orgs = fetch_organizations(&api_key).await?;
    let team = orgs
        .iter()
        .find(|o| o._id == team_id)
        .ok_or_else(|| anyhow::anyhow!("Team not found: {}", team_id))?;
    let project = create_game(&api_key, team_id, title).await?;
    let website_host = config::get("open_browser_website_host")?;
    let url = format!("{}/dev-portal/{}/{}", website_host, team.slug, project.slug);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ProjectCreateOutput {
                project,
                team: team.clone(),
                url,
            })?
        );
        return Ok(());
    }
    println!(
        "✓ Created project \"{}\" (id: {})",
        project.title, project._id
    );
    println!("  {}", url);
    Ok(())
}

pub async fn handle_team_list(json: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let orgs = fetch_organizations(&api_key).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&orgs)?);
        return Ok(());
    }
    if orgs.is_empty() {
        println!("No teams found.");
        return Ok(());
    }
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![Cell::new("ID"), Cell::new("Slug"), Cell::new("Name")]);
    for org in orgs {
        table.add_row(vec![org._id, org.slug, org.name]);
    }
    println!("{table}");
    Ok(())
}

pub async fn handle_project_list(team_id: &str, json: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let games = fetch_games(&api_key, team_id).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&games)?);
        return Ok(());
    }
    if games.is_empty() {
        println!("No games found for team {}.", team_id);
        return Ok(());
    }
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![Cell::new("ID"), Cell::new("Slug"), Cell::new("Title")]);
    for game in games {
        table.add_row(vec![game._id, game.slug, game.title]);
    }
    println!("{table}");
    Ok(())
}
