use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Config {
    open_browser_website_host: String,
    api_host: String,
    #[serde(default)]
    keyring_service: Option<String>,
    #[serde(default)]
    keyring_account: Option<String>,
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
            keyring_service: None,
            keyring_account: None,
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

#[derive(Debug)]
pub struct KeyringConfig {
    pub service: String,
    pub account: String,
}

pub fn keyring_config() -> Result<KeyringConfig> {
    let config = Config::load()?;
    Ok(KeyringConfig {
        service: config
            .keyring_service
            .clone()
            .unwrap_or_else(|| "wvdsh".to_string()),
        account: config
            .keyring_account
            .clone()
            .unwrap_or_else(|| "api-key".to_string()),
    })
}

#[derive(Debug, Deserialize)]
pub struct GodotSection {
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct UnitySection {
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct CustomSection {
    pub version: String,
    pub entrypoint: String,
}

#[derive(Debug, Deserialize)]
pub struct WavedashConfig {
    pub org_slug: String,
    pub game_slug: String,
    pub branch_slug: String,
    pub upload_dir: PathBuf,
    
    #[serde(rename = "godot")]
    pub godot: Option<GodotSection>,
    
    #[serde(rename = "unity")]
    pub unity: Option<UnitySection>,
    
    #[serde(rename = "custom")]
    pub custom: Option<CustomSection>,
}

impl WavedashConfig {
    pub fn load(config_path: &PathBuf) -> Result<Self> {
        let config_content = std::fs::read_to_string(config_path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file at {}: {}", config_path.display(), e))?;

        let config: WavedashConfig = toml::from_str(&config_content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;

        Ok(config)
    }

    pub fn engine_type(&self) -> Result<&str> {
        match (self.godot.is_some(), self.unity.is_some(), self.custom.is_some()) {
            (true, false, false) => Ok("godot"),
            (false, true, false) => Ok("unity"),
            (false, false, true) => Ok("custom"),
            _ => anyhow::bail!("Config must have exactly one of [godot], [unity], or [custom] sections"),
        }
    }

    pub fn version(&self) -> Result<&str> {
        if let Some(ref godot) = self.godot {
            Ok(&godot.version)
        } else if let Some(ref unity) = self.unity {
            Ok(&unity.version)
        } else if let Some(ref custom) = self.custom {
            Ok(&custom.version)
        } else {
            anyhow::bail!("No engine section found")
        }
    }

    pub fn entrypoint(&self) -> Option<&str> {
        self.custom.as_ref().map(|c| c.entrypoint.as_str())
    }
}