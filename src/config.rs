use anyhow::Result;
use serde::Deserialize;

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