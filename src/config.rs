use anyhow::Result;
use directories::BaseDirs;
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;

/// Get the wvdsh config directory (varies by environment)
/// - Production: ~/.wvdsh
/// - Staging: ~/.wvdsh-stg
/// - Dev: ~/.wvdsh-dev
pub fn wvdsh_dir() -> Result<PathBuf> {
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
    Ok(wvdsh_dir()?.join("credentials.json"))
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

/// The cloud environment for the game build
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Production,
    Demo,
    Sandbox,
}

impl Environment {
    pub fn as_str(&self) -> &'static str {
        match self {
            Environment::Production => "production",
            Environment::Demo => "demo",
            Environment::Sandbox => "sandbox",
        }
    }
}

impl std::fmt::Display for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Deserialize)]
pub struct WavedashConfig {
    /// Organization slug
    pub org: String,

    /// Game slug
    pub game: String,

    /// Environment: production, demo, or sandbox
    pub environment: Environment,

    /// Build version in semantic versioning format (major.minor.patch)
    /// Example: "1.0.0", "2.1.3"
    #[serde(rename = "version")]
    pub build_version: Option<String>,

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

        // Validate build_version format if provided
        if let Some(ref version) = config.build_version {
            let semver_regex = Regex::new(r"^\d+\.\d+\.\d+$").unwrap();
            if !semver_regex.is_match(version) {
                anyhow::bail!(
                    "Invalid version '{}'. Must be in semantic version format: major.minor.patch (e.g., 1.0.0, 2.1.3)",
                    version
                );
            }
        }

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

    /// Get the engine version (e.g., Godot version, Unity version)
    pub fn engine_version(&self) -> Result<&str> {
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

    /// Get the build version (semantic version: major.minor.patch)
    pub fn get_build_version(&self) -> Option<&str> {
        self.build_version.as_deref()
    }

    pub fn entrypoint(&self) -> Option<&str> {
        self.custom.as_ref().map(|c| c.entrypoint.as_str())
    }
}
