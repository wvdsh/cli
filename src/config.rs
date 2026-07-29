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
    /// Bare play domain (PLAYSITE_HOST, matches mainsite's PUBLIC_PLAYSITE_HOST)
    /// — serves the default-entrypoint scripts `wavedash dev` boots engine
    /// builds with. https-prefixed here like site_host.
    playsite_host: String,
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

        let mut playsite_host = env!("PLAYSITE_HOST").to_string();
        if !playsite_host.starts_with("http") {
            playsite_host = format!("https://{}", playsite_host);
        }

        Ok(Config {
            open_browser_website_host: site_host,
            api_host: env!("CONVEX_HTTP_URL").to_string(),
            playsite_host,
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
        "playsite_host" => Ok(config.playsite_host),
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

/// Env vars that override their `wavedash.toml` counterparts, so a committed
/// config can act as the default while CI varies the game, output directory, or
/// engine version per job. A set-but-blank value counts as unset — an
/// unpopulated CI variable shouldn't wipe out a value the config file supplies.
pub const ENV_GAME_ID: &str = "WAVEDASH_GAME_ID";
pub const ENV_UPLOAD_DIR: &str = "WAVEDASH_UPLOAD_DIR";
pub const ENV_ENTRYPOINT: &str = "WAVEDASH_ENTRYPOINT";
pub const ENV_GODOT_VERSION: &str = "WAVEDASH_GODOT_VERSION";
pub const ENV_UNITY_VERSION: &str = "WAVEDASH_UNITY_VERSION";

/// Trim an override and treat a blank one as absent.
fn non_blank(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn env_override(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(non_blank)
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

/// Resolve a game_id by preferring the CLI-provided value (which clap also
/// fills from `WAVEDASH_GAME_ID`), otherwise loading `game_id` from the
/// wavedash.toml at `config_path`. Errors include the config path so the user
/// knows which file we tried to read.
pub fn resolve_game_id(cli_game_id: Option<&str>, config_path: &PathBuf) -> Result<String> {
    if let Some(id) = cli_game_id {
        return Ok(id.to_string());
    }
    let config = WavedashConfig::load(config_path).map_err(|e| {
        anyhow::anyhow!(
            "No --game-id or {} provided, and could not read game_id from {}: {}",
            ENV_GAME_ID,
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

        let mut config: WavedashConfig = toml::from_str(&config_content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;

        config.apply_env_overrides(env_override)?;

        Ok(config)
    }

    /// Layer the `WAVEDASH_*` overrides on top of what the config file parsed to.
    /// `lookup` is injected so tests don't have to mutate the process environment.
    fn apply_env_overrides(&mut self, lookup: impl Fn(&str) -> Option<String>) -> Result<()> {
        if let Some(game_id) = lookup(ENV_GAME_ID) {
            self.game_id = game_id;
        }
        // Relative values resolve against the config file's directory, same as a
        // relative upload_dir in the toml; absolute ones win outright, since
        // that's how the callers' `config_dir.join(..)` already behaves.
        if let Some(upload_dir) = lookup(ENV_UPLOAD_DIR) {
            self.upload_dir = PathBuf::from(upload_dir);
        }
        if let Some(entrypoint) = lookup(ENV_ENTRYPOINT) {
            self.entrypoint = Some(entrypoint);
        }

        match (lookup(ENV_GODOT_VERSION), lookup(ENV_UNITY_VERSION)) {
            (Some(_), Some(_)) => anyhow::bail!(
                "{} and {} are both set, but a build targets a single engine. Unset whichever doesn't apply.",
                ENV_GODOT_VERSION,
                ENV_UNITY_VERSION
            ),
            (Some(version), None) => {
                if let Some(godot) = &mut self.godot {
                    godot.version = version;
                } else {
                    self.reject_engine_conflict(ENV_GODOT_VERSION)?;
                    self.godot = Some(GodotSection { version });
                }
            }
            (None, Some(version)) => {
                if let Some(unity) = &mut self.unity {
                    unity.version = version;
                } else {
                    self.reject_engine_conflict(ENV_UNITY_VERSION)?;
                    self.unity = Some(UnitySection { version });
                }
            }
            (None, None) => {}
        }

        Ok(())
    }

    /// An engine version override may introduce the section it names when the
    /// config file declares no engine at all, but it must not switch engines
    /// behind the user's back — that uploads a build the site can't boot, and
    /// `engine_type()` would only report a vague "at most one engine" error.
    fn reject_engine_conflict(&self, env_var: &str) -> Result<()> {
        let declared = [
            self.godot.is_some().then_some("[godot]"),
            self.unity.is_some().then_some("[unity]"),
            self.jsdos.is_some().then_some("[jsdos]"),
            self.ruffle.is_some().then_some("[ruffle]"),
            self.renpy.is_some().then_some("[renpy]"),
        ]
        .into_iter()
        .flatten()
        .next();

        if let Some(section) = declared {
            anyhow::bail!(
                "{} is set, but the config file already declares {}. Remove one so the build targets a single engine.",
                env_var,
                section
            );
        }

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Stand-in for the process environment, so these tests stay hermetic while
    /// running on cargo's shared test threads.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .filter_map(|(name, value)| {
                non_blank(value.to_string()).map(|value| (name.to_string(), value))
            })
            .collect();
        move |name| vars.get(name).cloned()
    }

    fn parse(toml_str: &str) -> WavedashConfig {
        toml::from_str(toml_str).expect("test config should parse")
    }

    const GODOT_CONFIG: &str = r#"
        game_id = "from_file"
        upload_dir = "build/web"

        [godot]
        version = "4.2"
    "#;

    const CUSTOM_CONFIG: &str = r#"
        game_id = "from_file"
        upload_dir = "dist"
        entrypoint = "game.html"
    "#;

    #[test]
    fn blank_overrides_are_treated_as_unset() {
        assert_eq!(non_blank("  4.3 ".to_string()), Some("4.3".to_string()));
        assert_eq!(non_blank("   ".to_string()), None);
        assert_eq!(non_blank(String::new()), None);
    }

    #[test]
    fn env_overrides_file_values() {
        let mut config = parse(CUSTOM_CONFIG);
        config
            .apply_env_overrides(env(&[
                (ENV_GAME_ID, "from_env"),
                (ENV_UPLOAD_DIR, "out/web"),
                (ENV_ENTRYPOINT, "start.html"),
            ]))
            .expect("overrides should apply");

        assert_eq!(config.game_id, "from_env");
        assert_eq!(config.upload_dir, PathBuf::from("out/web"));
        assert_eq!(config.entrypoint(), Some("start.html"));
    }

    #[test]
    fn file_values_survive_when_nothing_is_set() {
        let mut config = parse(CUSTOM_CONFIG);
        config
            .apply_env_overrides(env(&[(ENV_GAME_ID, "  ")]))
            .expect("blank override should be ignored");

        assert_eq!(config.game_id, "from_file");
        assert_eq!(config.upload_dir, PathBuf::from("dist"));
        assert_eq!(config.entrypoint(), Some("game.html"));
    }

    #[test]
    fn engine_version_override_replaces_existing_section_version() {
        let mut config = parse(GODOT_CONFIG);
        config
            .apply_env_overrides(env(&[(ENV_GODOT_VERSION, "4.3")]))
            .expect("godot override should apply");

        assert_eq!(config.engine_type().unwrap(), Some(EngineKind::Godot));
        assert_eq!(config.engine_version(), Some("4.3"));
    }

    #[test]
    fn engine_version_override_adds_section_when_config_declares_no_engine() {
        let mut config = parse("game_id = \"g\"\nupload_dir = \"dist\"\n");
        config
            .apply_env_overrides(env(&[(ENV_UNITY_VERSION, "2022.3")]))
            .expect("unity override should apply");

        assert_eq!(config.engine_type().unwrap(), Some(EngineKind::Unity));
        assert_eq!(config.engine_version(), Some("2022.3"));
    }

    #[test]
    fn engine_version_override_refuses_to_switch_engines() {
        let mut config = parse(GODOT_CONFIG);
        let err = config
            .apply_env_overrides(env(&[(ENV_UNITY_VERSION, "2022.3")]))
            .expect_err("unity override should conflict with [godot]");

        assert!(err.to_string().contains("[godot]"), "got: {}", err);
        assert_eq!(config.engine_version(), Some("4.2"));
    }

    #[test]
    fn both_engine_version_overrides_is_an_error() {
        let mut config = parse(GODOT_CONFIG);
        let err = config
            .apply_env_overrides(env(&[
                (ENV_GODOT_VERSION, "4.3"),
                (ENV_UNITY_VERSION, "2022.3"),
            ]))
            .expect_err("two engine versions should conflict");

        assert!(err.to_string().contains(ENV_UNITY_VERSION), "got: {}", err);
    }
}
