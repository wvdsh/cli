use anyhow::Result;
use std::path::PathBuf;

use crate::config::WavedashConfig;

#[derive(Clone, clap::ValueEnum)]
pub enum BumpLevel {
    Patch,
    Minor,
    Major,
}

pub fn handle_version_bump(config_path: PathBuf, level: BumpLevel) -> Result<()> {
    let config = WavedashConfig::load(&config_path)?;
    let old_version = &config.version;

    let parts: Vec<u32> = old_version
        .split('.')
        .map(|p| {
            p.parse::<u32>()
                .map_err(|_| anyhow::anyhow!("Invalid version format: {}", old_version))
        })
        .collect::<Result<Vec<_>>>()?;

    if parts.len() != 3 {
        anyhow::bail!(
            "Version must be in major.minor.patch format, got: {}",
            old_version
        );
    }

    let (major, minor, patch) = (parts[0], parts[1], parts[2]);

    let new_version = match level {
        BumpLevel::Patch => format!("{}.{}.{}", major, minor, patch + 1),
        BumpLevel::Minor => format!("{}.{}.0", major, minor + 1),
        BumpLevel::Major => format!("{}.0.0", major + 1),
    };

    WavedashConfig::update_field(&config_path, "version", &new_version)?;
    println!("Bumped version: {} → {}", old_version, new_version);
    Ok(())
}
