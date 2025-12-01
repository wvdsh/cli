use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Config {
    open_browser_website_host: String,
    api_host: String,
    keyring_service: String,
    keyring_account: String,
}

impl Config {
    fn load() -> Result<Self> {
        // Values are baked in at compile time via build.rs
        let mut site_host = env!("SITE_HOST").to_string();
        
        // Ensure protocol is present
        if !site_host.starts_with("http") {
            site_host = format!("https://{}", site_host);
        }

        Ok(Config {
            open_browser_website_host: site_host,
            api_host: env!("CONVEX_HTTP_URL").to_string(),
            keyring_service: env!("KEYRING_SERVICE").to_string(),
            keyring_account: env!("KEYRING_ACCOUNT").to_string(),
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
        service: config.keyring_service.clone(),
        account: config.keyring_account.clone(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Godot,
    Unity,
    Custom,
}

impl EngineKind {
    pub fn as_config_key(&self) -> &'static str {
        match self {
            EngineKind::Godot => "godot",
            EngineKind::Unity => "unity",
            EngineKind::Custom => "custom",
        }
    }

    pub fn as_label(&self) -> &'static str {
        match self {
            EngineKind::Godot => "GODOT",
            EngineKind::Unity => "UNITY",
            EngineKind::Custom => "CUSTOM",
        }
    }
}

impl WavedashConfig {
    pub fn load(config_path: &PathBuf) -> Result<Self> {
        let config_content = std::fs::read_to_string(config_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read config file at {}: {}",
                config_path.display(),
                e
            )
        })?;

        let config: WavedashConfig = toml::from_str(&config_content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;

        Ok(config)
    }

    pub fn engine_type(&self) -> Result<EngineKind> {
        match (
            self.godot.is_some(),
            self.unity.is_some(),
            self.custom.is_some(),
        ) {
            (true, false, false) => Ok(EngineKind::Godot),
            (false, true, false) => Ok(EngineKind::Unity),
            (false, false, true) => Ok(EngineKind::Custom),
            _ => anyhow::bail!(
                "Config must have exactly one of [godot], [unity], or [custom] sections"
            ),
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
