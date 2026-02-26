use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::config::WavedashConfig;

/// Manages temporary file copies to the upload directory
pub struct FileStaging {
    config_dest: Option<PathBuf>,
}

impl FileStaging {
    /// Copy necessary files to the upload directory
    pub fn prepare(
        config_path: &Path,
        upload_dir: &Path,
        wavedash_config: &WavedashConfig,
    ) -> Result<Self> {
        // Copy wavedash.toml into the upload directory if not already there
        let config_dest = {
            let dest = upload_dir.join("wavedash.toml");
            if config_path.canonicalize()? != dest.canonicalize().unwrap_or_default() {
                // Remove existing file first to avoid "Access denied" on Windows
                // when the file is locked or read-only
                if dest.exists() {
                    let _ = std::fs::remove_file(&dest);
                }
                std::fs::copy(config_path, &dest)
                    .map_err(|e| anyhow::anyhow!("Failed to copy config to upload dir: {}", e))?;
                Some(dest)
            } else {
                None
            }
        };

        // Validate entrypoint exists in the upload directory
        if let Some(entrypoint_str) = wavedash_config.entrypoint() {
            let entrypoint_path = upload_dir.join(entrypoint_str);
            if !entrypoint_path.exists() {
                anyhow::bail!(
                    "Entrypoint '{}' not found in upload_dir ({}). The entrypoint must be a file inside your upload_dir.",
                    entrypoint_str,
                    upload_dir.display()
                );
            }
        }

        Ok(Self {
            config_dest,
        })
    }

    /// Clean up any temporary files that were copied
    pub fn cleanup(self) {
        if let Some(path) = self.config_dest {
            let _ = std::fs::remove_file(path);
        }
    }
}

