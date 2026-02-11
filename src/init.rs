use anyhow::Result;
use dialoguer::{Confirm, Select};
use std::path::Path;

use crate::config::EngineKind;

pub fn handle_init(game_id: String, branch: String, engine: Option<EngineKind>) -> Result<()> {
    let path = Path::new("./wavedash.toml");

    if path.exists() {
        let overwrite = Confirm::new()
            .with_prompt("wavedash.toml already exists. Overwrite?")
            .default(false)
            .interact()?;

        if !overwrite {
            println!("Aborted.");
            return Ok(());
        }
    }

    let engine = match engine {
        Some(e) => e,
        None => {
            let items = &["Godot", "Unity", "Custom"];
            let selection = Select::new()
                .with_prompt("Select engine")
                .items(items)
                .default(0)
                .interact()?;

            match selection {
                0 => EngineKind::Godot,
                1 => EngineKind::Unity,
                2 => EngineKind::Custom,
                _ => unreachable!(),
            }
        }
    };

    let engine_section = match engine {
        EngineKind::Godot => "[godot]\nversion = \"4.3\"\n".to_string(),
        EngineKind::Unity => "[unity]\nversion = \"6000.0\"\n".to_string(),
        EngineKind::Custom => {
            "[custom]\nversion = \"1.0\"\nentrypoint = \"index.html\"\n".to_string()
        }
    };

    let content = format!(
        "game_id = \"{game_id}\"\n\
         branch = \"{branch}\"\n\
         upload_dir = \"./build\"\n\
         version = \"0.0.1\"\n\
         \n\
         {engine_section}"
    );

    std::fs::write(path, &content)?;
    println!("✓ Created wavedash.toml");

    Ok(())
}
