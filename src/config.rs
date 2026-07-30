//! Two unrelated layers share the word "config" in this module.
//!
//! The private `Config` struct is the **environment** layer: API host, play
//! domain, Cloudflare Access creds. It's baked in at compile time by `build.rs`,
//! read through [`get`] and [`create_http_client`], never parsed at runtime, and
//! identical for every project on the machine.
//!
//! [`WavedashConfig`] is the **project** layer — and despite the name it is not
//! a file. It's the *resolved view* of one: `wavedash.toml` with the `WAVEDASH_*`
//! overrides layered on top, where the file itself is optional as long as the
//! environment supplies what the command actually reads. So nothing in it is
//! guaranteed to exist until something asks:
//!
//! ```text
//!   --game-id / WAVEDASH_* env var / wavedash.toml / built-in default
//!            └────────────── whichever is set, in that order ──┘
//! ```
//!
//! That's why the accessors return `Result` instead of borrowing a field that
//! parsing already proved was there. Reads, not loads, are the unit of both
//! validation and override reporting — a command is only ever asked for the
//! fields it touches, and only announces the overrides that can affect it. See
//! [`WavedashConfig::load`] for the file-optional rule and the `Field` enum for
//! how notices stay tied to reads.
//!
//! `game_id` has a third source the others don't (the `--game-id` flag), so it
//! resolves through the free function [`resolve_game_id`] rather than an
//! accessor — commands like `stat` and `achievement` need it without loading a
//! project config at all.

use anyhow::Result;
use colored::Colorize;
use directories::BaseDirs;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};

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

/// The API key, read by [`crate::auth`]. Not a config field and so not in
/// [`ENV_OVERRIDES`] — it can't stand in for anything wavedash.toml supplies —
/// but it follows the same blank-is-unset rule, which is why it's named here
/// next to the others rather than as a literal at its point of use.
pub const ENV_TOKEN: &str = "WAVEDASH_TOKEN";

/// What `entrypoint()` falls back to when nothing named one and no engine
/// claimed the build. Only ever a guess, which is why a failure to find it
/// reports differently from a missing file the user actually named.
pub const DEFAULT_ENTRYPOINT: &str = "index.html";

/// Trim an override and treat a blank one as absent. Shared with `auth` so every
/// `WAVEDASH_*` variable answers "is this set?" the same way.
pub(crate) fn non_blank(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Raw read of an override, blanks included. `apply_env_overrides` is what
/// applies [`non_blank`], so its tests can feed the same unfiltered values the
/// process environment hands over and exercise the filtering for real.
fn raw_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub(crate) fn env_override(name: &str) -> Option<String> {
    raw_env(name).and_then(non_blank)
}

/// Every override, for the checks that care whether the environment is
/// configuring this run at all rather than which field it touches.
const ENV_OVERRIDES: [&str; 5] = [
    ENV_GAME_ID,
    ENV_UPLOAD_DIR,
    ENV_ENTRYPOINT,
    ENV_GODOT_VERSION,
    ENV_UNITY_VERSION,
];

/// True when at least one override is set. A missing config file is only worth
/// carrying on past if the environment might supply what the command needs.
fn any_env_override() -> bool {
    ENV_OVERRIDES
        .iter()
        .any(|name| env_override(name).is_some())
}

/// A config value an override can change. Notices are keyed by field and
/// announced the first time a command reads that field, so which overrides get
/// mentioned follows from what the command actually uses — no per-command list
/// to keep in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    GameId,
    UploadDir,
    Entrypoint,
    Engine,
}

impl Field {
    fn bit(self) -> u8 {
        match self {
            Field::GameId => 1 << 0,
            Field::UploadDir => 1 << 1,
            Field::Entrypoint => 1 << 2,
            Field::Engine => 1 << 3,
        }
    }
}

/// Single place the notice prefix lives, since overrides get announced both from
/// the config accessors and from `resolve_game_id`.
fn print_override_notice(text: &str) {
    println!("{} {}", "env override:".yellow(), text);
}

fn game_id_notice(value: &str) -> String {
    format!("{} → game_id = {}", ENV_GAME_ID, value)
}

/// One line describing an engine version override. `added` marks the case where
/// the config declared no engine, which is worth calling out separately: it
/// decides the engine for the build rather than just its version.
fn engine_notice(env_var: &str, section: &str, version: &str, added: bool) -> (Field, String) {
    let text = if added {
        format!(
            "{} → [{}].version = {} (config declared no engine, so [{}] is now in play)",
            env_var, section, version, section
        )
    } else {
        format!("{} → [{}].version = {}", env_var, section, version)
    };
    (Field::Engine, text)
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

/// The resolved project layer: `wavedash.toml` plus `WAVEDASH_*` overrides, with
/// the file optional. Not the file itself — see the module docs.
///
/// Fields are private so every read goes through an accessor, which is what
/// makes override notices self-maintaining (see [`Field`]). `game_id` and
/// `upload_dir` are required in practice but `Option` here, because "is this
/// missing?" has no answer until a command reads it: either source can supply
/// either field, and neither has to.
#[derive(Debug, Default, Deserialize)]
pub struct WavedashConfig {
    #[serde(default)]
    game_id: Option<String>,
    #[serde(default)]
    upload_dir: Option<PathBuf>,
    entrypoint: Option<String>,

    #[serde(rename = "godot")]
    godot: Option<GodotSection>,

    #[serde(rename = "unity")]
    unity: Option<UnitySection>,

    #[serde(rename = "jsdos")]
    jsdos: Option<ExecutableEngineSection>,

    #[serde(rename = "ruffle")]
    ruffle: Option<ExecutableEngineSection>,

    #[serde(rename = "renpy")]
    renpy: Option<ExecutableEngineSection>,

    /// Overrides applied at load, announced lazily on first read of the field.
    #[serde(skip)]
    override_notices: Vec<(Field, String)>,

    /// Bitmask of fields already announced. Atomic because the accessors take
    /// `&self` and the config is shared across the async call graph.
    #[serde(skip)]
    announced: AtomicU8,

    /// Where the config was looked for, so a missing-field error can name the
    /// file the user should edit.
    #[serde(skip)]
    config_path: PathBuf,

    /// False when no file was found at `config_path` and the config is built
    /// entirely from overrides — the two cases need different advice.
    #[serde(skip)]
    from_file: bool,

    /// Set when `entrypoint` came from `WAVEDASH_ENTRYPOINT`, so an error about
    /// it can name the variable to go fix rather than the config file.
    #[serde(skip)]
    entrypoint_from_env: bool,
}

/// Where the entrypoint came from. A missing entrypoint is two different
/// problems wearing one message: a file the user named is a typo or a bad build,
/// while a missing `index.html` nobody named means the build never declared what
/// to boot. Callers need to tell them apart to say anything useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrypointSource {
    /// `entrypoint` in wavedash.toml.
    Config,
    /// `WAVEDASH_ENTRYPOINT`.
    Env,
    /// Nothing set it, so this is [`DEFAULT_ENTRYPOINT`].
    Default,
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

/// Resolve a game_id from, in precedence order: the `--game-id` flag,
/// `WAVEDASH_GAME_ID`, then `game_id` in the wavedash.toml at `config_path`.
/// Errors include the config path so the user knows which file we tried to read.
///
/// The flag is deliberately *not* wired to clap's `env`. clap would fill it
/// before any config is read, which hides the override from the accessors that
/// announce it — and for an `Option<String>` arg it feeds a set-but-blank
/// variable straight into the value parser, hard-failing on exactly the
/// unpopulated CI variable the blank-is-unset rule exists to ignore. Resolving
/// here keeps `stat`/`achievement` and `build push` reading one set of rules.
pub fn resolve_game_id(cli_game_id: Option<&str>, config_path: &PathBuf) -> Result<String> {
    // A typed flag wins outright, and silently: the user is looking at the value
    // they just passed, and it beating a differing env var is the documented
    // precedence rather than a surprise worth a line of output.
    if let Some(id) = cli_game_id {
        return Ok(id.to_string());
    }
    if let Some(id) = env_override(ENV_GAME_ID) {
        print_override_notice(&game_id_notice(&id));
        return Ok(id);
    }
    let config = WavedashConfig::load(config_path).map_err(|e| {
        anyhow::anyhow!(
            "No --game-id or {} provided, so game_id has to come from the config: {}",
            ENV_GAME_ID,
            e
        )
    })?;
    match config.game_id() {
        Ok(id) => Ok(id.to_string()),
        Err(e) => anyhow::bail!("{} You can also pass --game-id.", e),
    }
}

impl WavedashConfig {
    /// Load the config at `config_path`, then layer the `WAVEDASH_*` overrides on
    /// top. The file is optional when the environment is configuring the run: a
    /// CI job that exports everything a command reads shouldn't need to check in
    /// a wavedash.toml just to satisfy the loader. Which fields a command
    /// actually needs is decided by the accessors it calls, so a file-less config
    /// only fails on the first field the environment didn't supply.
    pub fn load(config_path: &PathBuf) -> Result<Self> {
        let mut config = match std::fs::read_to_string(config_path) {
            Ok(config_content) => {
                let mut config: WavedashConfig = toml::from_str(&config_content)
                    .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;
                config.from_file = true;
                config.treat_blank_file_values_as_unset();
                config
            }
            // Bail on a missing file only when the environment supplies nothing
            // either. Otherwise the message would be about a field when the real
            // problem is almost always a wrong directory or a missing --config.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !any_env_override() => {
                anyhow::bail!(
                    "No config file at {}. Run `wavedash init` to create one, pass --config if it lives elsewhere, or set the WAVEDASH_* overrides to run without one.",
                    config_path.display()
                )
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => WavedashConfig::default(),
            Err(e) => anyhow::bail!(
                "Failed to read config file at {}: {}",
                config_path.display(),
                e
            ),
        };
        config.config_path = config_path.clone();

        // Not printed here: each notice waits until the command reads the field
        // it applies to, so nothing is announced that can't affect this run.
        let notices = config.apply_env_overrides(raw_env)?;
        config.override_notices = notices;

        Ok(config)
    }

    /// A blank value written in the file means what a blank override means:
    /// nothing. Applied at load so the two can't disagree — and because a blank
    /// is worse than useless here rather than merely empty. `upload_dir = ""`
    /// resolves through `config_dir.join("")` to the config file's own
    /// directory, which under an engine section (no entrypoint to validate)
    /// stages the whole project — source, dotfiles and all — with no warning.
    /// `game_id = ""` builds a URL with an empty path segment. Unsetting both
    /// turns each into the missing-field error the accessors already have.
    fn treat_blank_file_values_as_unset(&mut self) {
        self.game_id = self.game_id.take().and_then(non_blank);
        self.entrypoint = self.entrypoint.take().and_then(non_blank);
        // Round-tripped through the same `non_blank` as everything else, so a
        // padded path trims the way a padded WAVEDASH_UPLOAD_DIR does. toml
        // strings are UTF-8, so nothing is lost.
        self.upload_dir = self
            .upload_dir
            .take()
            .and_then(|dir| non_blank(dir.to_string_lossy().into_owned()))
            .map(PathBuf::from);
    }

    /// Print the override notice for `field`, once, on first read.
    fn announce(&self, field: Field) {
        let bit = field.bit();
        if self.announced.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
            return;
        }
        for (_, text) in self.override_notices.iter().filter(|(f, _)| *f == field) {
            print_override_notice(text);
        }
    }

    /// Error for a field no command-visible source supplied. Named per field
    /// rather than up front, so a command is only ever asked for what it reads.
    fn missing_field(&self, field: &str, env_var: &str) -> anyhow::Error {
        if self.from_file {
            anyhow::anyhow!(
                "{} is not set. Add it to {} or set {}.",
                field,
                self.config_path.display(),
                env_var
            )
        } else {
            anyhow::anyhow!(
                "{} is not set, and there's no config file at {} to read it from. Set {}, run `wavedash init`, or pass --config if the config lives elsewhere.",
                field,
                self.config_path.display(),
                env_var
            )
        }
    }

    /// The game to act on: `WAVEDASH_GAME_ID`, else `game_id` from the file.
    /// `Err` when neither supplied it. To include the `--game-id` flag in the
    /// precedence, go through [`resolve_game_id`] instead.
    pub fn game_id(&self) -> Result<&str> {
        self.announce(Field::GameId);
        self.game_id
            .as_deref()
            .ok_or_else(|| self.missing_field("game_id", ENV_GAME_ID))
    }

    /// The directory to upload: `WAVEDASH_UPLOAD_DIR`, else `upload_dir` from the
    /// file. `Err` when neither supplied it. Relative either way — callers
    /// resolve it against the config file's directory.
    pub fn upload_dir(&self) -> Result<&PathBuf> {
        self.announce(Field::UploadDir);
        self.upload_dir
            .as_ref()
            .ok_or_else(|| self.missing_field("upload_dir", ENV_UPLOAD_DIR))
    }

    /// Layer the `WAVEDASH_*` overrides on top of what the config file parsed to.
    /// Returns one line per override applied, tagged with the commands it
    /// affects, so a run that silently retargets a build says so. `lookup` is
    /// injected so tests don't have to mutate the process environment; it takes
    /// raw values and [`non_blank`] is applied here, so the blank-is-unset rule
    /// is the same one every caller gets.
    fn apply_env_overrides(
        &mut self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Vec<(Field, String)>> {
        let lookup = |name: &str| lookup(name).and_then(non_blank);
        let mut notices = Vec::new();

        if let Some(game_id) = lookup(ENV_GAME_ID) {
            notices.push((
                Field::GameId,
                game_id_notice(&game_id),
            ));
            self.game_id = Some(game_id);
        }
        // Relative values resolve against the config file's directory, same as a
        // relative upload_dir in the toml; absolute ones win outright, since
        // that's how the callers' `config_dir.join(..)` already behaves.
        if let Some(upload_dir) = lookup(ENV_UPLOAD_DIR) {
            notices.push((
                Field::UploadDir,
                format!("{} → upload_dir = {}", ENV_UPLOAD_DIR, upload_dir),
            ));
            self.upload_dir = Some(PathBuf::from(upload_dir));
        }

        match (lookup(ENV_GODOT_VERSION), lookup(ENV_UNITY_VERSION)) {
            (Some(_), Some(_)) => anyhow::bail!(
                "{} and {} are both set, but a build targets a single engine. Unset whichever doesn't apply.",
                ENV_GODOT_VERSION,
                ENV_UNITY_VERSION
            ),
            (Some(version), None) => {
                if let Some(godot) = &mut self.godot {
                    notices.push(engine_notice(ENV_GODOT_VERSION, "godot", &version, false));
                    godot.version = version;
                } else {
                    self.reject_engine_conflict(ENV_GODOT_VERSION)?;
                    notices.push(engine_notice(ENV_GODOT_VERSION, "godot", &version, true));
                    notices.extend(self.shadowed_entrypoint_notice("godot"));
                    self.godot = Some(GodotSection { version });
                }
            }
            (None, Some(version)) => {
                if let Some(unity) = &mut self.unity {
                    notices.push(engine_notice(ENV_UNITY_VERSION, "unity", &version, false));
                    unity.version = version;
                } else {
                    self.reject_engine_conflict(ENV_UNITY_VERSION)?;
                    notices.push(engine_notice(ENV_UNITY_VERSION, "unity", &version, true));
                    notices.extend(self.shadowed_entrypoint_notice("unity"));
                    self.unity = Some(UnitySection { version });
                }
            }
            (None, None) => {}
        }

        // Applied after the engine overrides so the check below sees the engine
        // the build will actually use, including one this call just introduced.
        if let Some(entrypoint) = lookup(ENV_ENTRYPOINT) {
            // `entrypoint()` answers only for engine-less configs, so under an
            // engine this override would be recorded and then discarded with no
            // output at all. Set in CI, silence is indistinguishable from
            // success, so refuse the combination the way an engine conflict is
            // refused rather than accept a value we won't use.
            if let Some(engine) = self.engine_type()? {
                anyhow::bail!(
                    "{} is set to {}, but this build targets {} — engine builds boot through wavedash's own entrypoint, so the value would be ignored. Unset {}, or drop the engine that brought it into play.",
                    ENV_ENTRYPOINT,
                    entrypoint,
                    engine.as_label(),
                    ENV_ENTRYPOINT
                );
            }
            notices.push((
                Field::Entrypoint,
                format!("{} → entrypoint = {}", ENV_ENTRYPOINT, entrypoint),
            ));
            self.entrypoint = Some(entrypoint);
            self.entrypoint_from_env = true;
        }

        Ok(notices)
    }

    /// An engine version override that *introduces* the engine also strands any
    /// `entrypoint` the config file set, because [`Self::entrypoint`] only
    /// answers for engine-less configs. The toml line stays there looking
    /// effective, so say that it isn't.
    fn shadowed_entrypoint_notice(&self, section: &str) -> Option<(Field, String)> {
        let entrypoint = self.entrypoint.as_deref()?;
        Some((
            Field::Engine,
            format!(
                "entrypoint = {} from the config is no longer used — [{}] builds boot through wavedash's own entrypoint",
                entrypoint, section
            ),
        ))
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
    ///
    /// Deliberately does not `announce(Field::Engine)`, unlike the other reads
    /// of engine state: no override targets these three, so there is never a
    /// notice to print, and announcing here would burn the once-only bit before
    /// `engine_version()` — the read that *can* be overridden — gets to it.
    /// Adding a `WAVEDASH_JSDOS_VERSION`/`_RUFFLE_`/`_RENPY_` override means
    /// revisiting that: the version read below is the one that has to announce.
    fn executable_section(&self) -> Option<&ExecutableEngineSection> {
        self.jsdos
            .as_ref()
            .or(self.ruffle.as_ref())
            .or(self.renpy.as_ref())
    }

    pub fn engine_version(&self) -> Option<&str> {
        let version = if let Some(godot) = &self.godot {
            Some(godot.version.as_str())
        } else if let Some(unity) = &self.unity {
            Some(unity.version.as_str())
        } else {
            self.executable_section().map(|s| s.version.as_str())
        };
        // Only announce when a version is actually handed out — a config with no
        // engine section can't be affected by an engine version override.
        if version.is_some() {
            self.announce(Field::Engine);
        }
        version
    }

    /// The HTML/JS file the build boots from: `WAVEDASH_ENTRYPOINT`, else
    /// `entrypoint` from the file, else `"index.html"`.
    ///
    /// `None` — not an error — when any engine section is in play, because those
    /// builds boot through wavedash's own entrypoint and ignore this entirely.
    /// An override that would be discarded that way is refused at load rather
    /// than silently dropped; see [`Self::apply_env_overrides`].
    pub fn entrypoint(&self) -> Option<&str> {
        self.entrypoint_with_source().map(|(entrypoint, _)| entrypoint)
    }

    /// As [`Self::entrypoint`], plus where the value came from — so a validation
    /// failure can distinguish "the file you named isn't there" from "nothing
    /// named one and the guess isn't there either".
    pub fn entrypoint_with_source(&self) -> Option<(&str, EntrypointSource)> {
        match self.engine_type() {
            Ok(None) => {
                self.announce(Field::Entrypoint);
                Some(match self.entrypoint.as_deref() {
                    Some(entrypoint) if self.entrypoint_from_env => {
                        (entrypoint, EntrypointSource::Env)
                    }
                    Some(entrypoint) => (entrypoint, EntrypointSource::Config),
                    None => (DEFAULT_ENTRYPOINT, EntrypointSource::Default),
                })
            }
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
    /// running on cargo's shared test threads. Values are passed through
    /// verbatim — blanks included — because trimming here would mean the tests
    /// assert against the mock instead of against `apply_env_overrides`.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    fn texts(notices: &[(Field, String)]) -> Vec<&str> {
        notices.iter().map(|(_, text)| text.as_str()).collect()
    }

    fn parse(toml_str: &str) -> WavedashConfig {
        toml::from_str(toml_str).expect("test config should parse")
    }

    /// What `load` hands back when there's no file at `config_path` but the
    /// environment is configuring the run.
    fn no_file_at(config_path: &str) -> WavedashConfig {
        WavedashConfig {
            config_path: PathBuf::from(config_path),
            ..Default::default()
        }
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
        let notices = config
            .apply_env_overrides(env(&[
                (ENV_GAME_ID, "from_env"),
                (ENV_UPLOAD_DIR, "out/web"),
                (ENV_ENTRYPOINT, "start.html"),
            ]))
            .expect("overrides should apply");

        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));
        assert_eq!(config.entrypoint(), Some("start.html"));
        assert_eq!(
            texts(&notices),
            vec![
                "WAVEDASH_GAME_ID → game_id = from_env",
                "WAVEDASH_UPLOAD_DIR → upload_dir = out/web",
                "WAVEDASH_ENTRYPOINT → entrypoint = start.html",
            ]
        );
    }

    #[test]
    fn a_field_is_announced_on_first_read_and_only_that_field() {
        let mut config = parse(GODOT_CONFIG);
        config.override_notices = config
            .apply_env_overrides(env(&[
                (ENV_GAME_ID, "from_env"),
                (ENV_UPLOAD_DIR, "out/web"),
                (ENV_GODOT_VERSION, "4.3"),
            ]))
            .expect("overrides should apply");

        // Nothing read yet, so nothing announced.
        assert_eq!(config.announced.load(Ordering::Relaxed), 0);

        // What `publish` does: read the game id and nothing else. Only that
        // field is announced — the other two applied but stay quiet.
        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.announced.load(Ordering::Relaxed), Field::GameId.bit());

        // Re-reading is a no-op: the bit is already set, so announce() returns early.
        let _ = config.game_id();
        assert_eq!(config.announced.load(Ordering::Relaxed), Field::GameId.bit());

        // Reading the engine version brings in its notice too.
        assert_eq!(config.engine_version(), Some("4.3"));
        assert_eq!(
            config.announced.load(Ordering::Relaxed),
            Field::GameId.bit() | Field::Engine.bit()
        );
    }

    #[test]
    fn engine_field_stays_quiet_when_the_config_has_no_engine() {
        let mut config = parse(CUSTOM_CONFIG);
        config.override_notices = config
            .apply_env_overrides(env(&[(ENV_GAME_ID, "from_env")]))
            .expect("override should apply");

        // No engine section and no version to hand out, so nothing to announce.
        assert_eq!(config.engine_version(), None);
        assert_eq!(config.announced.load(Ordering::Relaxed), 0);
    }

    /// The CI shape this rule exists for: `WAVEDASH_GAME_ID: ${{ vars.MISSING }}`
    /// expands to `""`, and must read as "not set" rather than wiping out the
    /// file's value. The blank reaches `apply_env_overrides` unfiltered here, so
    /// this covers the wiring and not just `non_blank` in isolation.
    #[test]
    fn file_values_survive_when_overrides_are_blank() {
        let mut config = parse(CUSTOM_CONFIG);
        let notices = config
            .apply_env_overrides(env(&[
                (ENV_GAME_ID, ""),
                (ENV_UPLOAD_DIR, "   "),
                (ENV_ENTRYPOINT, "\t"),
                (ENV_GODOT_VERSION, ""),
            ]))
            .expect("blank overrides should be ignored");

        assert_eq!(config.game_id().unwrap(), "from_file");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("dist"));
        assert_eq!(config.entrypoint(), Some("game.html"));
        assert_eq!(config.engine_type().unwrap(), None);
        assert!(
            notices.is_empty(),
            "nothing was overridden, so nothing should be announced: {:?}",
            notices
        );
    }

    #[test]
    fn overrides_are_trimmed() {
        let mut config = parse(CUSTOM_CONFIG);
        config
            .apply_env_overrides(env(&[(ENV_GAME_ID, "  from_env\n")]))
            .expect("override should apply");

        assert_eq!(config.game_id().unwrap(), "from_env");
    }

    /// `stat`/`achievement`/`publish` read nothing but the game id, so a fully
    /// overridden run has no reason to need a file on disk.
    #[test]
    fn a_config_file_is_unnecessary_when_overrides_supply_what_is_read() {
        let mut config = no_file_at("./wavedash.toml");
        config
            .apply_env_overrides(env(&[
                (ENV_GAME_ID, "from_env"),
                (ENV_UPLOAD_DIR, "out/web"),
            ]))
            .expect("overrides should apply");

        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));
        // No engine and no entrypoint set anywhere: `build push` still has a
        // usable default, so nothing else is required of the environment.
        assert_eq!(config.entrypoint(), Some("index.html"));
    }

    /// The flip side: a field the environment didn't supply fails on read, and
    /// the error names both places it could have come from.
    #[test]
    fn a_field_no_source_supplied_fails_on_read() {
        let mut config = no_file_at("./wavedash.toml");
        config
            .apply_env_overrides(env(&[(ENV_GAME_ID, "from_env")]))
            .expect("override should apply");

        // Read by `build push`, but not supplied — unlike game_id, which is.
        let err = config
            .upload_dir()
            .expect_err("upload_dir has no source")
            .to_string();
        assert!(err.contains("upload_dir"), "got: {}", err);
        assert!(err.contains(ENV_UPLOAD_DIR), "got: {}", err);
        assert!(err.contains("wavedash.toml"), "got: {}", err);
        assert_eq!(config.game_id().unwrap(), "from_env");
    }

    /// A partial toml is the same question asked the other way round: the file
    /// exists but leaves a field out, so the error should point at that file.
    #[test]
    fn a_partial_config_file_defers_to_overrides() {
        let mut config = parse("game_id = \"from_file\"\n");
        config.from_file = true;
        config.config_path = PathBuf::from("/games/thing/wavedash.toml");
        config
            .apply_env_overrides(env(&[(ENV_UPLOAD_DIR, "out/web")]))
            .expect("override should apply");

        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));

        let mut bare = parse("game_id = \"from_file\"\n");
        bare.from_file = true;
        bare.config_path = PathBuf::from("/games/thing/wavedash.toml");
        let err = bare
            .upload_dir()
            .expect_err("upload_dir is in neither the file nor the environment")
            .to_string();
        assert!(
            err.contains("/games/thing/wavedash.toml"),
            "an existing file should be named as the place to fix it: {}",
            err
        );
    }

    /// A blank in the file is not a value. `upload_dir = ""` in particular used
    /// to resolve to the config file's own directory and stage the whole project.
    #[test]
    fn blank_file_values_are_treated_as_unset_too() {
        let mut config = parse(
            "game_id = \"\"\nupload_dir = \"   \"\nentrypoint = \"\t\"\n",
        );
        config.from_file = true;
        config.config_path = PathBuf::from("/games/thing/wavedash.toml");
        config.treat_blank_file_values_as_unset();

        let err = config.upload_dir().expect_err("blank is not a directory");
        assert!(err.to_string().contains("upload_dir"), "got: {}", err);
        assert!(
            config.game_id().is_err(),
            "a blank game_id must not reach the API as an empty path segment"
        );
        // Engine-less, so entrypoint falls back to its default rather than "".
        assert_eq!(config.entrypoint(), Some("index.html"));
    }

    /// Blank in the file, real value in the environment: the override supplies
    /// it, exactly as it would for an absent field.
    #[test]
    fn an_override_fills_in_a_blank_file_value() {
        let mut config = parse("game_id = \"\"\nupload_dir = \"\"\n");
        config.from_file = true;
        config.treat_blank_file_values_as_unset();
        config
            .apply_env_overrides(env(&[
                (ENV_GAME_ID, "from_env"),
                (ENV_UPLOAD_DIR, " out/web "),
            ]))
            .expect("overrides should apply");

        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));
    }

    #[test]
    fn padded_file_values_are_trimmed_like_overrides() {
        let mut config = parse("game_id = \"  padded \"\nupload_dir = \" dist \"\n");
        config.treat_blank_file_values_as_unset();

        assert_eq!(config.game_id().unwrap(), "padded");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("dist"));
    }

    #[test]
    fn entrypoint_override_is_rejected_when_an_engine_would_ignore_it() {
        let mut config = parse(GODOT_CONFIG);
        let err = config
            .apply_env_overrides(env(&[(ENV_ENTRYPOINT, "start.html")]))
            .expect_err("an engine build ignores the entrypoint, so don't accept one");

        assert!(err.to_string().contains(ENV_ENTRYPOINT), "got: {}", err);
        assert!(err.to_string().contains("GODOT"), "got: {}", err);
    }

    /// The compound case: the config declares no engine, so the entrypoint
    /// override looks applicable right up until the version override adds one.
    #[test]
    fn entrypoint_override_is_rejected_when_an_override_adds_the_engine() {
        let mut config = parse(CUSTOM_CONFIG);
        let err = config
            .apply_env_overrides(env(&[
                (ENV_ENTRYPOINT, "start.html"),
                (ENV_GODOT_VERSION, "4.3"),
            ]))
            .expect_err("the added [godot] section makes the entrypoint inert");

        assert!(err.to_string().contains(ENV_ENTRYPOINT), "got: {}", err);
    }

    /// Same shadowing, but from the file's own `entrypoint` — nothing to reject,
    /// so it has to be said out loud instead.
    #[test]
    fn adding_an_engine_says_the_config_entrypoint_stopped_being_used() {
        let mut config = parse(CUSTOM_CONFIG);
        let notices = config
            .apply_env_overrides(env(&[(ENV_GODOT_VERSION, "4.3")]))
            .expect("godot override should apply");

        assert_eq!(config.entrypoint(), None);
        let shadowed = texts(&notices)
            .into_iter()
            .find(|text| text.contains("game.html"))
            .unwrap_or_else(|| {
                panic!(
                    "the stranded entrypoint should be announced: {:?}",
                    texts(&notices)
                )
            });
        assert!(
            shadowed.contains("no longer used"),
            "got: {}",
            shadowed
        );
        // Announced with the engine, since Field::Entrypoint can no longer fire.
        assert!(notices
            .iter()
            .any(|(field, text)| *field == Field::Engine && text.contains("game.html")));
    }

    #[test]
    fn adding_an_engine_is_quiet_when_the_config_set_no_entrypoint() {
        let mut config = parse("game_id = \"g\"\nupload_dir = \"dist\"\n");
        let notices = config
            .apply_env_overrides(env(&[(ENV_GODOT_VERSION, "4.3")]))
            .expect("godot override should apply");

        assert_eq!(notices.len(), 1, "got: {:?}", texts(&notices));
    }

    #[test]
    fn engine_version_override_replaces_existing_section_version() {
        let mut config = parse(GODOT_CONFIG);
        let notices = config
            .apply_env_overrides(env(&[(ENV_GODOT_VERSION, "4.3")]))
            .expect("godot override should apply");

        assert_eq!(config.engine_type().unwrap(), Some(EngineKind::Godot));
        assert_eq!(config.engine_version(), Some("4.3"));
        assert_eq!(
            texts(&notices),
            vec!["WAVEDASH_GODOT_VERSION → [godot].version = 4.3"]
        );
    }

    #[test]
    fn engine_version_override_adds_section_when_config_declares_no_engine() {
        let mut config = parse("game_id = \"g\"\nupload_dir = \"dist\"\n");
        let notices = config
            .apply_env_overrides(env(&[(ENV_UNITY_VERSION, "2022.3")]))
            .expect("unity override should apply");

        assert_eq!(config.engine_type().unwrap(), Some(EngineKind::Unity));
        assert_eq!(config.engine_version(), Some("2022.3"));
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].1.contains("config declared no engine"),
            "adding a section should say so: {}",
            notices[0].1
        );
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
