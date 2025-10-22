use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Config {
    open_browser_website_host: String,
    api_host: String,
}

impl Config {
    fn load() -> Result<Self> {
        // Try to load local config first (for development)
        if let Ok(config_content) = std::fs::read_to_string("config.toml") {
            return Ok(toml::from_str(&config_content)?);
        }

        // Default to production for published releases
        Ok(Self {
            open_browser_website_host: "https://wavedash.gg".to_string(),
            api_host: "https://convex-http.wavedash.gg".to_string(),
        })
    }
}

pub fn get(key: &str) -> Result<String> {
    let config = Config::load()?;
    match key {
        "open_browser_website_host" => Ok(config.open_browser_website_host),
        "api_host" => Ok(config.api_host),
        _ => anyhow::bail!("Unknown config key: {}", key),
    }
}

#[derive(Debug, Deserialize)]
pub struct EngineConfig {
    #[serde(rename = "type")]
    pub engine_type: String,
    pub version: String,
    pub entrypoint: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WavedashConfig {
    pub org_slug: String,
    pub game_slug: String,
    pub branch_slug: String,
    pub upload_dir: PathBuf,
    pub engine: EngineConfig,
}

impl WavedashConfig {
    pub fn load(config_path: &PathBuf) -> Result<Self> {
        let config_content = std::fs::read_to_string(config_path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file at {}: {}", config_path.display(), e))?;

        let config: WavedashConfig = toml::from_str(&config_content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;

        Ok(config)
    }
}