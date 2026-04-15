use anyhow::Result;
use std::path::Path;

use crate::config::WavedashConfig;

/// Validates that required files exist in the upload directory
pub struct FileStaging;

impl FileStaging {
    /// Validate required files exist in the upload directory
    pub fn prepare(
        upload_dir: &Path,
        wavedash_config: &WavedashConfig,
    ) -> Result<Self> {
        // Validate entrypoint exists and is an HTML file
        if let Some(entrypoint_str) = wavedash_config.entrypoint() {
            // Entrypoint must be an HTML file
            let lower = entrypoint_str.to_ascii_lowercase();
            if !lower.ends_with(".html") && !lower.ends_with(".htm") {
                anyhow::bail!(
                    "Entrypoint '{}' must be an HTML file (ending in .html or .htm).",
                    entrypoint_str,
                );
            }

            let entrypoint_path = upload_dir.join(entrypoint_str);
            if !entrypoint_path.exists() {
                anyhow::bail!(
                    "Entrypoint '{}' not found in upload_dir ({}). The entrypoint must be a file inside your upload_dir.",
                    entrypoint_str,
                    upload_dir.display()
                );
            }
        }

        // Validate executable and loader_url files exist (for JSDOS/Ruffle)
        for file in wavedash_config.executable_files_to_validate() {
            let file_path = upload_dir.join(file);
            if !file_path.exists() {
                anyhow::bail!(
                    "'{}' not found in upload_dir ({}). The file must exist inside your upload_dir.",
                    file,
                    upload_dir.display()
                );
            }
        }

        Ok(Self)
    }
}
