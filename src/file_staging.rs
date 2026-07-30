use anyhow::Result;
use std::path::Path;

use crate::config::{self, EntrypointSource, WavedashConfig};

/// A missing entrypoint has two very different causes, and the old single message
/// described only one of them.
///
/// If the user named the file, the message they need is the one they always got:
/// this path isn't in the upload dir. If *nothing* named it, `index.html` was
/// only ever [`config::DEFAULT_ENTRYPOINT`] — a guess that applies because no
/// engine section claimed the build. Reporting that as a missing entrypoint
/// points at a filename the user never wrote and hides the actual gap, which is
/// especially bad when an engine override went missing on the way in (a `WSLENV`
/// that doesn't forward it, an unset CI variable) and the same build worked the
/// run before.
///
/// Deliberately says nothing about which files *are* present: that would mean
/// walking the upload directory to build an error message, and the directory
/// belongs to the user's build output, which can be arbitrarily large.
fn missing_entrypoint_error(
    entrypoint: &str,
    source: EntrypointSource,
    upload_dir: &Path,
) -> anyhow::Error {
    match source {
        EntrypointSource::Default => anyhow::anyhow!(
            "Can't tell what to boot in {}.\n\nNo engine section is declared, so wavedash looked for the default entrypoint '{}' and didn't find it.\n\nEither declare the engine that produced this build ([godot] or [unity] in wavedash.toml, or {} / {}), or name the file to boot with `entrypoint = \"…\"` in wavedash.toml or {}.",
            upload_dir.display(),
            entrypoint,
            config::ENV_GODOT_VERSION,
            config::ENV_UNITY_VERSION,
            config::ENV_ENTRYPOINT,
        ),
        EntrypointSource::Config => anyhow::anyhow!(
            "Entrypoint '{}' (from wavedash.toml) not found in upload_dir ({}). The entrypoint must be a file inside your upload_dir.",
            entrypoint,
            upload_dir.display(),
        ),
        EntrypointSource::Env => anyhow::anyhow!(
            "Entrypoint '{}' (from {}) not found in upload_dir ({}). The entrypoint must be a file inside your upload_dir.",
            entrypoint,
            config::ENV_ENTRYPOINT,
            upload_dir.display(),
        ),
    }
}

/// Validates that required files exist in the upload directory
pub struct FileStaging;

impl FileStaging {
    /// Validate required files exist in the upload directory
    pub fn prepare(upload_dir: &Path, wavedash_config: &WavedashConfig) -> Result<Self> {
        // Validate entrypoint exists and is an HTML or JS file
        if let Some((entrypoint_str, source)) = wavedash_config.entrypoint_with_source() {
            let lower = entrypoint_str.to_ascii_lowercase();
            if !lower.ends_with(".html") && !lower.ends_with(".htm") && !lower.ends_with(".js") {
                anyhow::bail!(
                    "Entrypoint '{}' must be an HTML file (.html/.htm) or JavaScript file (.js).",
                    entrypoint_str,
                );
            }

            let entrypoint_path = upload_dir.join(entrypoint_str);
            if !entrypoint_path.exists() {
                return Err(missing_entrypoint_error(entrypoint_str, source, upload_dir));
            }
        }

        // Validate executable and loader_url files exist (for JSDOS/Ruffle/Ren'Py)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported case: a Godot export whose engine override went missing, so
    /// the default entrypoint applies to a directory that never had one. The
    /// error has to name the real gap rather than a filename nobody wrote.
    #[test]
    fn a_defaulted_entrypoint_blames_the_missing_engine() {
        let err = missing_entrypoint_error(
            config::DEFAULT_ENTRYPOINT,
            EntrypointSource::Default,
            Path::new("/games/thing/builds"),
        )
        .to_string();

        assert!(err.contains("No engine section is declared"), "got: {}", err);
        assert!(err.contains(config::ENV_GODOT_VERSION), "got: {}", err);
        assert!(err.contains(config::ENV_ENTRYPOINT), "got: {}", err);
        assert!(err.contains("/games/thing/builds"), "got: {}", err);
    }

    /// A named entrypoint is a different problem, so it keeps the old message —
    /// plus where the name came from, which is what tells a CI user whether to
    /// look in the toml or at their environment.
    #[test]
    fn a_named_entrypoint_reports_its_source() {
        let dir = Path::new("/games/thing/builds");

        let from_env =
            missing_entrypoint_error("typo.html", EntrypointSource::Env, dir).to_string();
        assert!(from_env.contains(config::ENV_ENTRYPOINT), "got: {}", from_env);
        assert!(!from_env.contains("No engine section"), "got: {}", from_env);

        let from_file =
            missing_entrypoint_error("typo.html", EntrypointSource::Config, dir).to_string();
        assert!(from_file.contains("wavedash.toml"), "got: {}", from_file);
        assert!(!from_file.contains("No engine section"), "got: {}", from_file);
    }
}
