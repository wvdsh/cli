use anyhow::Result;
use directories::BaseDirs;
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

/// Custom deserializer that handles both new environment values and legacy branch_slug values
fn deserialize_environment<'de, D>(deserializer: D) -> Result<Environment, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    // Try direct environment values first
    match s.as_str() {
        "production" => return Ok(Environment::Production),
        "demo" => return Ok(Environment::Demo),
        "sandbox" => return Ok(Environment::Sandbox),
        _ => {}
    }

    // Legacy branch_slug mapping
    if s.starts_with("internal") {
        Ok(Environment::Sandbox)
    } else if s.starts_with("production") {
        Ok(Environment::Production)
    } else if s.starts_with("demo") {
        Ok(Environment::Demo)
    } else {
        Err(serde::de::Error::custom(format!(
            "Invalid environment '{}'. Expected: production, demo, sandbox (or legacy: internal-*, production-*, demo-*)",
            s
        )))
    }
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
    /// Organization slug (supports legacy "org_slug" field)
    #[serde(alias = "org_slug")]
    pub org: String,

    /// Game slug (supports legacy "game_slug" field)
    #[serde(alias = "game_slug")]
    pub game: String,

    /// Environment (supports legacy "branch_slug" field for backwards compatibility)
    /// Old branch_slug values are mapped: "internal-*" -> sandbox, "production-*" -> production, "demo-*" -> demo
    #[serde(alias = "branch_slug", deserialize_with = "deserialize_environment")]
    pub environment: Environment,

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

        // Check for deprecated field names and warn users
        let raw_toml: toml::Value = toml::from_str(&config_content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;

        if let Some(table) = raw_toml.as_table() {
            if table.contains_key("org_slug") {
                eprintln!("WARNING: 'org_slug' is deprecated. Please use 'org' instead.");
            }
            if table.contains_key("game_slug") {
                eprintln!("WARNING: 'game_slug' is deprecated. Please use 'game' instead.");
            }
            if table.contains_key("branch_slug") {
                eprintln!("WARNING: 'branch_slug' is deprecated. Please use 'environment' instead.");
                eprintln!("         Valid values: production, demo, sandbox");
            }
        }

        let config: WavedashConfig = toml::from_str(&config_content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;

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

    pub fn version(&self) -> Result<&str> {
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

    pub fn entrypoint(&self) -> Option<&str> {
        self.custom.as_ref().map(|c| c.entrypoint.as_str())
    }
}
