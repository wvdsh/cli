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
    /// Play worker statics origin (PLAY_STATICS_HOST) — serves the
    /// default-entrypoint scripts `wavedash dev` boots engine builds with.
    play_statics_origin: String,
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

        let mut play_statics_origin = env!("PLAY_STATICS_HOST").to_string();
        if !play_statics_origin.starts_with("http") {
            play_statics_origin = format!("https://{}", play_statics_origin);
        }

        Ok(Config {
            open_browser_website_host: site_host,
            api_host: env!("CONVEX_HTTP_URL").to_string(),
            play_statics_origin,
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
        "play_statics_origin" => Ok(config.play_statics_origin),
        _ => anyhow::bail!("Unknown config key: {}", key),
    }
}

/// Get the path to the credentials file
pub fn credentials_path() -> Result<PathBuf> {
    Ok(wavedash_dir()?.join("credentials.json"))
}

/// Header the server reads to know which CLI version a request came from.
/// Lets the API gate behavior on minimum versions, log usage, and surface
/// "please upgrade" prompts without parsing User-Agent.
pub const CLI_VERSION_HEADER: &str = "X-Wavedash-CLI-Version";

/// Create an HTTP client configured with the CLI version header (always)
/// and Cloudflare Access headers (only on staging when creds are baked in).
pub fn create_http_client() -> Result<reqwest::Client> {
    let config = Config::load()?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        CLI_VERSION_HEADER,
        reqwest::header::HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );

    let api_host = &config.api_host;
    let needs_cf_headers = api_host
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .ends_with("staging.wavedash.gg");

    if needs_cf_headers {
        if let (Some(client_id), Some(client_secret)) =
            (&config.cf_access_client_id, &config.cf_access_client_secret)
        {
            headers.insert(
                "CF-Access-Client-Id",
                reqwest::header::HeaderValue::from_str(client_id)?,
            );
            headers.insert(
                "CF-Access-Client-Secret",
                reqwest::header::HeaderValue::from_str(client_secret)?,
            );
        }
    }

    Ok(reqwest::Client::builder().default_headers(headers).build()?)
}

/// Check an API response for errors and return a human-friendly message.
/// Call this on every API response before reading the body.
pub async fn check_api_response(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();

    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(msg) = parsed["error"].as_str() {
            let code = parsed["code"].as_str();
            match (status, msg, code) {
                (404, _, _) | (_, "Game not found", _) => {
                    anyhow::bail!(
                        "Game not found. The game_id in your wavedash.toml may be incorrect.\nRun `wavedash init` to reconfigure."
                    );
                }
                (403, _, _) | (_, "Access denied", _) => {
                    anyhow::bail!(
                        "Access denied. You don't have permission to access this game.\nCheck that you're logged in with the right account (`wavedash auth status`)."
                    );
                }
                (401, _, _) => {
                    anyhow::bail!(
                        "Authentication failed. Run `wavedash auth login` to re-authenticate."
                    );
                }
                (_, _, Some("requires_force")) => {
                    anyhow::bail!("{} Pass --force to delete it anyway.", msg);
                }
                _ => anyhow::bail!("{}", msg),
            }
        }
    }

    anyhow::bail!("API request failed ({}): {}", status, body);
}

#[derive(Debug, Deserialize)]
pub struct GodotSection {
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct UnitySection {
    pub version: String,
}

/// Shape for engines whose runtime is fetched as a single executable file
/// (plus an optional loader script). Used by JSDOS, Ruffle, and Ren'Py.
#[derive(Debug, Deserialize)]
pub struct ExecutableEngineSection {
    pub version: String,
    pub executable: String,
    pub loader_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WavedashConfig {
    pub game_id: String,
    pub upload_dir: PathBuf,
    pub entrypoint: Option<String>,

    #[serde(rename = "godot")]
    pub godot: Option<GodotSection>,

    #[serde(rename = "unity")]
    pub unity: Option<UnitySection>,

    #[serde(rename = "jsdos")]
    pub jsdos: Option<ExecutableEngineSection>,

    #[serde(rename = "ruffle")]
    pub ruffle: Option<ExecutableEngineSection>,

    #[serde(rename = "renpy")]
    pub renpy: Option<ExecutableEngineSection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Godot,
    Unity,
    JsDos,
    Ruffle,
    RenPy,
}

impl EngineKind {
    pub fn as_label(&self) -> &'static str {
        match self {
            EngineKind::Godot => "GODOT",
            EngineKind::Unity => "UNITY",
            EngineKind::JsDos => "JSDOS",
            EngineKind::Ruffle => "RUFFLE",
            EngineKind::RenPy => "RENPY",
        }
    }
}

/// Resolve a game_id by preferring the CLI-provided value, otherwise loading
/// `game_id` from the wavedash.toml at `config_path`. Errors include the
/// config path so the user knows which file we tried to read.
pub fn resolve_game_id(cli_game_id: Option<&str>, config_path: &PathBuf) -> Result<String> {
    if let Some(id) = cli_game_id {
        return Ok(id.to_string());
    }
    let config = WavedashConfig::load(config_path).map_err(|e| {
        anyhow::anyhow!(
            "No --game-id provided and could not read game_id from {}: {}",
            config_path.display(),
            e
        )
    })?;
    Ok(config.game_id)
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

    pub fn engine_type(&self) -> Result<Option<EngineKind>> {
        let engines: Vec<EngineKind> = [
            self.godot.is_some().then_some(EngineKind::Godot),
            self.unity.is_some().then_some(EngineKind::Unity),
            self.jsdos.is_some().then_some(EngineKind::JsDos),
            self.ruffle.is_some().then_some(EngineKind::Ruffle),
            self.renpy.is_some().then_some(EngineKind::RenPy),
        ]
        .into_iter()
        .flatten()
        .collect();

        match engines.len() {
            0 => Ok(None),
            1 => Ok(Some(engines[0])),
            _ => anyhow::bail!(
                "Config must have at most one engine section: [godot], [unity], [jsdos], [ruffle], or [renpy]"
            ),
        }
    }

    /// Returns the section for the active executable-style engine, if any.
    /// JSDOS, Ruffle, and Ren'Py all share the same shape; `engine_type()`
    /// guarantees at most one of these is set at a time.
    fn executable_section(&self) -> Option<&ExecutableEngineSection> {
        self.jsdos
            .as_ref()
            .or(self.ruffle.as_ref())
            .or(self.renpy.as_ref())
    }

    pub fn engine_version(&self) -> Option<&str> {
        if let Some(godot) = &self.godot {
            return Some(&godot.version);
        }
        if let Some(unity) = &self.unity {
            return Some(&unity.version);
        }
        self.executable_section().map(|s| s.version.as_str())
    }

    /// Returns the entrypoint when no engine block is present.
    /// Uses the user-specified value from the config, or defaults to "index.html".
    pub fn entrypoint(&self) -> Option<&str> {
        match self.engine_type() {
            Ok(None) => Some(
                self.entrypoint
                    .as_deref()
                    .unwrap_or("index.html"),
            ),
            _ => None,
        }
    }

    /// For executable-style engines (JSDOS/Ruffle/Ren'Py), returns the
    /// entrypointParams (executable + optional loader_url).
    pub fn executable_entrypoint_params(&self) -> Option<serde_json::Value> {
        self.executable_section().map(|s| {
            let mut params = serde_json::json!({ "executable": s.executable });
            if let Some(loader_url) = &s.loader_url {
                params["loaderUrl"] = serde_json::json!(loader_url);
            }
            params
        })
    }

    /// For executable-style engines (JSDOS/Ruffle/Ren'Py), returns all files
    /// that must exist in upload_dir.
    pub fn executable_files_to_validate(&self) -> Vec<&str> {
        let Some(s) = self.executable_section() else {
            return Vec::new();
        };
        let mut files = vec![s.executable.as_str()];
        if let Some(loader_url) = &s.loader_url {
            files.push(loader_url);
        }
        files
    }
}
