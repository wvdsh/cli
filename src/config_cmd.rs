use anyhow::Result;
use std::path::PathBuf;

use crate::config::WavedashConfig;

pub fn handle_config_show(config_path: PathBuf) -> Result<()> {
    let config = WavedashConfig::load(&config_path)?;
    println!("{}", config.display_summary());
    Ok(())
}

pub fn handle_config_set(config_path: PathBuf, key: String, value: String) -> Result<()> {
    let supported_keys = ["branch", "version", "upload_dir", "game_id"];
    if !supported_keys.contains(&key.as_str()) {
        anyhow::bail!(
            "Unsupported key '{}'. Supported keys: {}",
            key,
            supported_keys.join(", ")
        );
    }

    // Validate
    match key.as_str() {
        "version" => {
            let parts: Vec<&str> = value.split('.').collect();
            if parts.len() != 3 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
                anyhow::bail!("Version must be in major.minor.patch format (e.g. 1.2.3)");
            }
        }
        "game_id" => {
            if value.is_empty() {
                anyhow::bail!("game_id must be non-empty");
            }
        }
        "upload_dir" => {
            let path = PathBuf::from(&value);
            if !path.exists() {
                eprintln!("Warning: directory '{}' does not exist yet", value);
            }
        }
        _ => {}
    }

    let old_value = WavedashConfig::update_field(&config_path, &key, &value)?;
    println!("Updated {}: {} → {}", key, old_value, value);
    Ok(())
}
