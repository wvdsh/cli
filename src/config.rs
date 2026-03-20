use anyhow::Result;
use directories::BaseDirs;
use serde::Deserialize;
use std::path::PathBuf;

/// Get the wavedash config directory (varies by environment)
/// - Production: ~/.wavedash
/// - Staging: ~/.wavedash-stg
/// - Dev: ~/.wavedash-dev
pub fn wavedash_dir() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(base_dirs.home_dir().join(env!("CONFIG_DIR")))
}

#[derive(Deserialize)]
struct Config {
    open_browser_website_host: String,
    api_host: String,
    cf_access_client_id: Option<String>,
    cf_access_client_secret: Option<String>,
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
            cf_access_client_id: option_env!("CF_ACCESS_CLIENT_ID").map(|s| s.to_string()),
            cf_access_client_secret: option_env!("CF_ACCESS_CLIENT_SECRET").map(|s| s.to_string()),
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

/// Get the path to the credentials file
pub fn credentials_path() -> Result<PathBuf> {
    Ok(wavedash_dir()?.join("credentials.json"))
}

/// Create an HTTP client configured with Cloudflare Access headers if needed
pub fn create_http_client() -> Result<reqwest::Client> {
    let config = Config::load()?;
    let mut client_builder = reqwest::Client::builder();

    // Check if we're targeting staging and have CF credentials
    let api_host = &config.api_host;
    let needs_cf_headers = api_host
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .ends_with("staging.wavedash.gg");

    if needs_cf_headers {
        if let (Some(client_id), Some(client_secret)) =
            (&config.cf_access_client_id, &config.cf_access_client_secret)
        {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                "CF-Access-Client-Id",
                reqwest::header::HeaderValue::from_str(client_id)?,
            );
            headers.insert(
                "CF-Access-Client-Secret",
                reqwest::header::HeaderValue::from_str(client_secret)?,
            );
            client_builder = client_builder.default_headers(headers);
        }
    }

    Ok(client_builder.build()?)
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
pub struct JsDosSection {
    pub version: String,
    pub executable: String,
    pub loader_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RuffleSection {
    pub version: String,
    pub executable: String,
    pub loader_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WavedashConfig {
    pub game_id: String,
    pub upload_dir: PathBuf,

    #[serde(rename = "godot")]
    pub godot: Option<GodotSection>,

    #[serde(rename = "unity")]
    pub unity: Option<UnitySection>,

    #[serde(rename = "custom")]
    pub custom: Option<CustomSection>,

    #[serde(rename = "jsdos")]
    pub jsdos: Option<JsDosSection>,

    #[serde(rename = "ruffle")]
    pub ruffle: Option<RuffleSection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Godot,
    Unity,
    Custom,
    JsDos,
    Ruffle,
}

impl EngineKind {
    pub fn as_config_key(&self) -> &'static str {
        match self {
            EngineKind::Godot => "godot",
            EngineKind::Unity => "unity",
            EngineKind::Custom => "custom",
            EngineKind::JsDos => "jsdos",
            EngineKind::Ruffle => "ruffle",
        }
    }

    pub fn as_label(&self) -> &'static str {
        match self {
            EngineKind::Godot => "GODOT",
            EngineKind::Unity => "UNITY",
            EngineKind::Custom => "CUSTOM",
            EngineKind::JsDos => "JSDOS",
            EngineKind::Ruffle => "RUFFLE",
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
        let engines: Vec<EngineKind> = [
            self.godot.is_some().then_some(EngineKind::Godot),
            self.unity.is_some().then_some(EngineKind::Unity),
            self.custom.is_some().then_some(EngineKind::Custom),
            self.jsdos.is_some().then_some(EngineKind::JsDos),
            self.ruffle.is_some().then_some(EngineKind::Ruffle),
        ]
        .into_iter()
        .flatten()
        .collect();

        match engines.len() {
            1 => Ok(engines[0]),
            0 => anyhow::bail!(
                "Config must have exactly one engine section: [godot], [unity], [custom], [jsdos], or [ruffle]"
            ),
            _ => anyhow::bail!(
                "Config must have exactly one engine section: [godot], [unity], [custom], [jsdos], or [ruffle]"
            ),
        }
    }

    pub fn engine_version(&self) -> Result<&str> {
        if let Some(ref godot) = self.godot {
            Ok(&godot.version)
        } else if let Some(ref unity) = self.unity {
            Ok(&unity.version)
        } else if let Some(ref custom) = self.custom {
            Ok(&custom.version)
        } else if let Some(ref jsdos) = self.jsdos {
            Ok(&jsdos.version)
        } else if let Some(ref ruffle) = self.ruffle {
            Ok(&ruffle.version)
        } else {
            anyhow::bail!("No engine section found")
        }
    }

    pub fn entrypoint(&self) -> Option<&str> {
        self.custom
            .as_ref()
            .map(|c| c.entrypoint.as_str())
    }

    /// For JSDOS/Ruffle engines, returns the entrypointParams (executable + optional loader_url)
    pub fn executable_entrypoint_params(&self) -> Option<serde_json::Value> {
        if let Some(ref jsdos) = self.jsdos {
            let mut params = serde_json::json!({ "executable": jsdos.executable });
            if let Some(ref loader_url) = jsdos.loader_url {
                params["loaderUrl"] = serde_json::json!(loader_url);
            }
            Some(params)
        } else if let Some(ref ruffle) = self.ruffle {
            let mut params = serde_json::json!({ "executable": ruffle.executable });
            if let Some(ref loader_url) = ruffle.loader_url {
                params["loaderUrl"] = serde_json::json!(loader_url);
            }
            Some(params)
        } else {
            None
        }
    }

    /// For JSDOS/Ruffle engines, returns all files that must exist in upload_dir
    pub fn executable_files_to_validate(&self) -> Vec<&str> {
        let mut files = Vec::new();
        if let Some(ref jsdos) = self.jsdos {
            files.push(jsdos.executable.as_str());
            if let Some(ref loader_url) = jsdos.loader_url {
                files.push(loader_url.as_str());
            }
        } else if let Some(ref ruffle) = self.ruffle {
            files.push(ruffle.executable.as_str());
            if let Some(ref loader_url) = ruffle.loader_url {
                files.push(loader_url.as_str());
            }
        }
        files
    }
}
