use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    open_browser_website_host: String,
}

impl Config {
    fn load() -> Result<Self> {
        let env = std::env::var("ENV").unwrap_or_else(|_| "dev".to_string());
        let config_path = format!("config/{}.toml", env);
        let config_content = std::fs::read_to_string(config_path)?;
        Ok(toml::from_str(&config_content)?)
    }
}

pub fn get(key: &str) -> Result<String> {
    let config = Config::load()?;
    match key {
        "open_browser_website_host" => Ok(config.open_browser_website_host),
        _ => anyhow::bail!("Unknown config key: {}", key),
    }
}