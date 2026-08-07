//! Two unrelated layers share the word "config" in this module.
//!
//! The private `Config` struct is the **environment** layer: API host, play
//! domain, Cloudflare Access creds. It's baked in at compile time by `build.rs`,
//! read through [`get`] and [`create_http_client`], never parsed at runtime, and
//! identical for every project on the machine.
//!
//! [`WavedashConfig`] is the **project** layer — and despite the name it is not
//! a file. It's the *resolved view* of one: `wavedash.toml` and the `WAVEDASH_*`
//! overrides held side by side, with the file itself optional as long as the
//! environment supplies what the command actually reads. Each accessor picks
//! between them, so nothing in it is decided until something asks:
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
use std::path::{Path, PathBuf};
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
/// [`EnvOverrides`] — it can't stand in for anything wavedash.toml supplies —
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

/// Raw read of an override, blanks included. [`EnvOverrides::capture`] is what
/// applies [`non_blank`], so its callers can feed the same unfiltered values the
/// process environment hands over and exercise the filtering for real.
fn raw_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The `WAVEDASH_*` values, read once when a config is built.
///
/// Snapshotting the environment rather than reading it inside each accessor is
/// what lets the accessors resolve their own precedence — override, then file,
/// then default — while staying pure functions of the config they're on. It's
/// also the only place the process environment is consulted, so
/// [`WavedashConfig::with_overrides`] and [`resolve_game_id_with`] can be handed
/// a constructed one and every rule below becomes testable without mutating the
/// environment that cargo's test threads share.
#[derive(Debug, Default)]
struct EnvOverrides {
    game_id: Option<String>,
    upload_dir: Option<PathBuf>,
    entrypoint: Option<String>,
    godot_version: Option<String>,
    unity_version: Option<String>,
}

impl EnvOverrides {
    /// [`non_blank`] is applied here and nowhere downstream, so blank-is-unset is
    /// decided once for every rule that consults these.
    fn capture(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let value = |name: &str| lookup(name).and_then(non_blank);
        Self {
            game_id: value(ENV_GAME_ID),
            upload_dir: value(ENV_UPLOAD_DIR).map(PathBuf::from),
            entrypoint: value(ENV_ENTRYPOINT),
            godot_version: value(ENV_GODOT_VERSION),
            unity_version: value(ENV_UNITY_VERSION),
        }
    }

    /// True when at least one override is set. A missing config file is only
    /// worth carrying on past if the environment might supply what the command
    /// needs; *which* field it supplies is the accessors' problem, not this one's.
    fn any(&self) -> bool {
        self.game_id.is_some()
            || self.upload_dir.is_some()
            || self.entrypoint.is_some()
            || self.godot_version.is_some()
            || self.unity_version.is_some()
    }
}

/// A config value an override can change, and the key the once-only announcement
/// guard is kept under. An override is announced the first time a command reads
/// the field it applies to, so which overrides get mentioned follows from what
/// the command actually uses — no per-command list to keep in sync.
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
    eprintln!("{} {}", "env override:".yellow(), text);
}

fn game_id_notice(value: &str) -> String {
    format!("{} → game_id = {}", ENV_GAME_ID, value)
}

fn upload_dir_notice(upload_dir: &Path) -> String {
    format!("{} → upload_dir = {}", ENV_UPLOAD_DIR, upload_dir.display())
}

fn entrypoint_notice(entrypoint: &str) -> String {
    format!("{} → entrypoint = {}", ENV_ENTRYPOINT, entrypoint)
}

/// One line describing an engine version override. `added` marks the case where
/// the config declared no engine, which is worth calling out separately: it
/// decides the engine for the build rather than just its version.
fn engine_notice(env_var: &str, section: &str, version: &str, added: bool) -> String {
    if added {
        format!(
            "{} → [{}].version = {} (config declared no engine, so [{}] is now in play)",
            env_var, section, version, section
        )
    } else {
        format!("{} → [{}].version = {}", env_var, section, version)
    }
}

/// An engine version override that *introduces* the engine also strands any
/// `entrypoint` the config file set, because [`WavedashConfig::entrypoint`] only
/// answers for engine-less configs. The toml line stays there looking effective,
/// so say that it isn't.
fn shadowed_entrypoint_notice(entrypoint: &str, section: &str) -> String {
    format!(
        "entrypoint = {} from the config is no longer used — [{}] builds boot through wavedash's own entrypoint",
        entrypoint, section
    )
}

/// Engine sections hold their fields as `Option` for the same reason `game_id`
/// does: blank is unset here too (see
/// [`WavedashConfig::treat_blank_file_values_as_unset`]), and "unset" has no
/// answer until something reads it. A section that names an engine without
/// saying which version therefore reaches the accessor that wants the version,
/// rather than failing the parse for every command including the ones that never
/// ask what engine this is.
#[derive(Debug, Deserialize)]
pub struct GodotSection {
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UnitySection {
    pub version: Option<String>,
}

/// Shape for engines whose runtime is fetched as a single executable file
/// (plus an optional loader script). Used by JSDOS, Ruffle, and Ren'Py.
#[derive(Debug, Deserialize)]
pub struct ExecutableEngineSection {
    pub version: Option<String>,
    pub executable: Option<String>,
    pub loader_url: Option<String>,
}

impl ExecutableEngineSection {
    fn treat_blank_values_as_unset(&mut self) {
        self.version = self.version.take().and_then(non_blank);
        self.executable = self.executable.take().and_then(non_blank);
        // A blank `loader_url` would otherwise be validated as a file (and
        // `upload_dir.join("")` is the directory itself, which exists) and then
        // sent to the API as an empty `loaderUrl` for the shell to fetch.
        self.loader_url = self.loader_url.take().and_then(non_blank);
    }
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

    /// The environment as it stood when this config was built. Every accessor
    /// consults its own override here before the field above it, so precedence,
    /// override reporting, and the refusals that only concern a build all land on
    /// the read that cares — nothing is decided on behalf of a command that never
    /// asks. Fields above are what the *file* said and are never written to.
    #[serde(skip)]
    env: EnvOverrides,

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

/// An engine section as the config file declares it: kind, toml section name, and
/// the version written there — `None` when the section named the engine but not a
/// version, which only matters if no override supplies one.
type DeclaredEngine<'a> = (EngineKind, &'static str, Option<&'a str>);

/// The engine a build targets, once [`WavedashConfig::active_engine`] has settled
/// the file and the version overrides against each other. `override_var` and
/// `added_section` are what a notice needs to say whether the environment set the
/// version or brought the engine itself into play.
#[derive(Debug, Clone, Copy)]
struct ActiveEngine<'a> {
    kind: EngineKind,
    section: &'static str,
    version: &'a str,
    override_var: Option<&'static str>,
    added_section: bool,
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
    /// The variable that can set this engine's version, for the two engines that
    /// have one. `None` for the executable-style engines — see
    /// [`WavedashConfig::executable_section`].
    fn version_override_var(&self) -> Option<&'static str> {
        match self {
            EngineKind::Godot => Some(ENV_GODOT_VERSION),
            EngineKind::Unity => Some(ENV_UNITY_VERSION),
            EngineKind::JsDos | EngineKind::Ruffle | EngineKind::RenPy => None,
        }
    }

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
    resolve_game_id_with(cli_game_id, config_path, EnvOverrides::capture(raw_env))
}

/// [`resolve_game_id`] against a given environment, so the precedence above can
/// be asserted without mutating the process's own — the same seam, for the same
/// reason, as [`WavedashConfig::with_overrides`].
fn resolve_game_id_with(
    cli_game_id: Option<&str>,
    config_path: &PathBuf,
    env: EnvOverrides,
) -> Result<String> {
    // A typed flag wins outright, and silently: the user is looking at the value
    // they just passed, and it beating a differing env var is the documented
    // precedence rather than a surprise worth a line of output.
    if let Some(id) = cli_game_id {
        return Ok(id.to_string());
    }
    if let Some(id) = &env.game_id {
        print_override_notice(&game_id_notice(id));
        return Ok(id.clone());
    }
    let config = WavedashConfig::with_overrides(config_path, env).map_err(|e| {
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
    /// Read the config at `config_path` and capture the `WAVEDASH_*` overrides
    /// alongside it. Nothing is reconciled between the two here — that's each
    /// accessor's job, so a command is only ever held to the fields it reads.
    ///
    /// The file is optional when the environment is configuring the run: a CI job
    /// that exports everything a command reads shouldn't need to check in a
    /// wavedash.toml just to satisfy the loader. Which fields a command actually
    /// needs is decided by the accessors it calls, so a file-less config only
    /// fails on the first field the environment didn't supply.
    pub fn load(config_path: &PathBuf) -> Result<Self> {
        Self::with_overrides(config_path, EnvOverrides::capture(raw_env))
    }

    /// [`Self::load`] against a given environment. The seam exists so the
    /// file-optional rule below — which decides whether there is a config at all
    /// — is testable the same hermetic way the accessors' precedence is.
    fn with_overrides(config_path: &PathBuf, env: EnvOverrides) -> Result<Self> {
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !env.any() => {
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
        config.env = env;

        // Nothing is resolved, announced, or refused here. Every override is
        // still sitting in `config.env`, waiting for the accessor whose field it
        // applies to — so a command that reads one field can't be stopped, or
        // even talked to, about another.
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
    ///
    /// Engine sections get the same treatment, field by field rather than by
    /// dropping the section: `[godot]` with a blank version still means the file
    /// declares Godot, so `WAVEDASH_GODOT_VERSION` still fills in the version it
    /// left out and `WAVEDASH_UNITY_VERSION` is still refused as an engine
    /// switch. Dropping the section would silently allow both, and would leave a
    /// build with no engine at all rather than a message.
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

        if let Some(godot) = &mut self.godot {
            godot.version = godot.version.take().and_then(non_blank);
        }
        if let Some(unity) = &mut self.unity {
            unity.version = unity.version.take().and_then(non_blank);
        }
        for section in [&mut self.jsdos, &mut self.ruffle, &mut self.renpy]
            .into_iter()
            .flatten()
        {
            section.treat_blank_values_as_unset();
        }
    }

    /// True the first time it's asked about `field`, so an accessor that resolved
    /// from the environment announces it on the read that first consulted it and
    /// stays quiet on every read after. Only called where there is something to
    /// say, which is what keeps a field with no override from burning its bit.
    fn first_read_of(&self, field: Field) -> bool {
        let bit = field.bit();
        self.announced.fetch_or(bit, Ordering::Relaxed) & bit == 0
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

    /// Error for a field an engine section left out — written blank or not
    /// written at all, which mean the same thing. Always about the file, since a
    /// section can only come from one; the variable is only mentioned where one
    /// exists to mention.
    fn missing_engine_field(&self, kind: EngineKind, section: &str, field: &str) -> anyhow::Error {
        match kind.version_override_var().filter(|_| field == "version") {
            Some(env_var) => anyhow::anyhow!(
                "[{}] has no {}. Add it to {} or set {}.",
                section,
                field,
                self.config_path.display(),
                env_var
            ),
            None => anyhow::anyhow!(
                "[{}] has no {}. Add it to {}.",
                section,
                field,
                self.config_path.display()
            ),
        }
    }

    /// The game to act on: `WAVEDASH_GAME_ID`, else `game_id` from the file.
    /// `Err` when neither supplied it. To include the `--game-id` flag in the
    /// precedence, go through [`resolve_game_id`] instead.
    pub fn game_id(&self) -> Result<&str> {
        if let Some(game_id) = &self.env.game_id {
            if self.first_read_of(Field::GameId) {
                print_override_notice(&game_id_notice(game_id));
            }
            return Ok(game_id);
        }
        self.game_id
            .as_deref()
            .ok_or_else(|| self.missing_field("game_id", ENV_GAME_ID))
    }

    /// The directory to upload: `WAVEDASH_UPLOAD_DIR`, else `upload_dir` from the
    /// file. `Err` when neither supplied it. Relative either way — callers resolve
    /// it against the config file's directory, which is also why an absolute
    /// override wins outright: that's how their `config_dir.join(..)` behaves.
    pub fn upload_dir(&self) -> Result<&PathBuf> {
        if let Some(upload_dir) = &self.env.upload_dir {
            if self.first_read_of(Field::UploadDir) {
                print_override_notice(&upload_dir_notice(upload_dir));
            }
            return Ok(upload_dir);
        }
        self.upload_dir
            .as_ref()
            .ok_or_else(|| self.missing_field("upload_dir", ENV_UPLOAD_DIR))
    }

    /// The engine section the config file declares, with its version. `Err` when
    /// it declares more than one, which is a question about the file alone and so
    /// doesn't depend on any override.
    fn declared_engine(&self) -> Result<Option<DeclaredEngine<'_>>> {
        let declared: Vec<DeclaredEngine<'_>> = [
            self.godot
                .as_ref()
                .map(|s| (EngineKind::Godot, "godot", s.version.as_deref())),
            self.unity
                .as_ref()
                .map(|s| (EngineKind::Unity, "unity", s.version.as_deref())),
            self.jsdos
                .as_ref()
                .map(|s| (EngineKind::JsDos, "jsdos", s.version.as_deref())),
            self.ruffle
                .as_ref()
                .map(|s| (EngineKind::Ruffle, "ruffle", s.version.as_deref())),
            self.renpy
                .as_ref()
                .map(|s| (EngineKind::RenPy, "renpy", s.version.as_deref())),
        ]
        .into_iter()
        .flatten()
        .collect();

        match declared.len() {
            0 => Ok(None),
            1 => Ok(Some(declared[0])),
            _ => anyhow::bail!(
                "Config must have at most one engine section: [godot], [unity], [jsdos], [ruffle], or [renpy]"
            ),
        }
    }

    /// The engine this build targets, resolving the version overrides against
    /// what the file declared. Every engine rule lives here, and every caller
    /// that cares about the engine goes through it.
    ///
    /// `Err` on the two combinations that can't be reconciled: both version
    /// overrides set at once, or one naming a different engine than the file
    /// declared. Refusing the second matters because switching engines behind the
    /// user's back uploads a build the site can't boot. Both used to be raised
    /// when the config was read off disk, which stopped `publish`, `stat`, and
    /// `achievement` too — commands that read a game id and never ask which
    /// engine anything targets. Resolving on the read is what confines them to
    /// the callers that have a stake in the answer.
    fn active_engine(&self) -> Result<Option<ActiveEngine<'_>>> {
        let declared = self.declared_engine()?;
        let overridden = match (&self.env.godot_version, &self.env.unity_version) {
            (Some(_), Some(_)) => anyhow::bail!(
                "{} and {} are both set, but a build targets a single engine. Unset whichever doesn't apply.",
                ENV_GODOT_VERSION,
                ENV_UNITY_VERSION
            ),
            (Some(version), None) => Some((ENV_GODOT_VERSION, EngineKind::Godot, "godot", version)),
            (None, Some(version)) => Some((ENV_UNITY_VERSION, EngineKind::Unity, "unity", version)),
            (None, None) => None,
        };

        match (overridden, declared) {
            // Nothing overridden, so the file decides — or nothing does.
            (None, None) => Ok(None),
            // The file decides, so the file has to have said which version.
            (None, Some((kind, section, version))) => Ok(Some(ActiveEngine {
                kind,
                section,
                version: version
                    .ok_or_else(|| self.missing_engine_field(kind, section, "version"))?,
                override_var: None,
                added_section: false,
            })),
            // The override names the engine the file declared: it sets the version.
            (Some((env_var, kind, section, version)), Some((declared_kind, _, _)))
                if kind == declared_kind =>
            {
                Ok(Some(ActiveEngine {
                    kind,
                    section,
                    version,
                    override_var: Some(env_var),
                    added_section: false,
                }))
            }
            // It names a different one. Refuse rather than pick a winner.
            (Some((env_var, _, _, _)), Some((_, declared_section, _))) => anyhow::bail!(
                "{} is set, but the config file already declares [{}]. Remove one so the build targets a single engine.",
                env_var,
                declared_section
            ),
            // The file declares no engine, so the override brings one into play.
            (Some((env_var, kind, section, version)), None) => Ok(Some(ActiveEngine {
                kind,
                section,
                version,
                override_var: Some(env_var),
                added_section: true,
            })),
        }
    }

    /// What an override-supplied engine warrants saying, in print order. Empty
    /// when the file supplied the engine, since then there's nothing to report.
    /// Returned rather than printed so the wording stays assertable — which of
    /// the two lines apply depends on what the override did.
    fn engine_notices(&self, engine: &ActiveEngine<'_>) -> Vec<String> {
        let Some(env_var) = engine.override_var else {
            return Vec::new();
        };
        let mut notices = vec![engine_notice(
            env_var,
            engine.section,
            engine.version,
            engine.added_section,
        )];
        // Introducing the section also stranded any `entrypoint` the file set.
        if engine.added_section {
            if let Some(entrypoint) = self.entrypoint.as_deref() {
                notices.push(shadowed_entrypoint_notice(entrypoint, engine.section));
            }
        }
        notices
    }

    /// Announce an override-supplied engine, once. Saying nothing leaves the bit
    /// unspent for a later read that does have something to report.
    fn announce_engine(&self, engine: &ActiveEngine<'_>) {
        let notices = self.engine_notices(engine);
        if notices.is_empty() || !self.first_read_of(Field::Engine) {
            return;
        }
        for notice in &notices {
            print_override_notice(notice);
        }
    }

    pub fn engine_type(&self) -> Result<Option<EngineKind>> {
        Ok(self.active_engine()?.map(|engine| engine.kind))
    }

    /// Returns the section for the active executable-style engine, if any.
    /// JSDOS, Ruffle, and Ren'Py all share the same shape; `engine_type()`
    /// guarantees at most one of these is set at a time.
    ///
    /// Reads the file's sections directly rather than going through
    /// `active_engine()`, because no override targets these three: there is never
    /// a version to reconcile, and the `executable`/`loader_url` fields it reaches
    /// for aren't part of the engine resolution at all. Adding a
    /// `WAVEDASH_JSDOS_VERSION`/`_RUFFLE_`/`_RENPY_` override means giving them a
    /// branch in `active_engine()` instead.
    fn executable_section(&self) -> Option<(EngineKind, &'static str, &ExecutableEngineSection)> {
        self.jsdos
            .as_ref()
            .map(|s| (EngineKind::JsDos, "jsdos", s))
            .or_else(|| {
                self.ruffle
                    .as_ref()
                    .map(|s| (EngineKind::Ruffle, "ruffle", s))
            })
            .or_else(|| self.renpy.as_ref().map(|s| (EngineKind::RenPy, "renpy", s)))
    }

    /// The active engine's version: the override that named it, else the version
    /// from the file's section. `Err` on the engine conflicts — see
    /// [`Self::active_engine`].
    pub fn engine_version(&self) -> Result<Option<&str>> {
        let Some(engine) = self.active_engine()? else {
            return Ok(None);
        };
        self.announce_engine(&engine);
        Ok(Some(engine.version))
    }

    /// The HTML/JS file the build boots from: `WAVEDASH_ENTRYPOINT`, else
    /// `entrypoint` from the file, else `"index.html"`.
    ///
    /// `Ok(None)` — not an error — when an engine is in play, because those builds
    /// boot through wavedash's own entrypoint and ignore this entirely. `Err` when
    /// `WAVEDASH_ENTRYPOINT` is what would be ignored; see
    /// [`Self::entrypoint_with_source`].
    pub fn entrypoint(&self) -> Result<Option<&str>> {
        Ok(self
            .entrypoint_with_source()?
            .map(|(entrypoint, _)| entrypoint))
    }

    /// As [`Self::entrypoint`], plus where the value came from — so a validation
    /// failure can distinguish "the file you named isn't there" from "nothing
    /// named one and the guess isn't there either". The source falls out of which
    /// fallback answered rather than being recorded when the value was stored.
    ///
    /// An inert `WAVEDASH_ENTRYPOINT` is refused here, on the read that would have
    /// ignored it: an engine build would take the override, discard it, and print
    /// nothing, and in CI silence is indistinguishable from success. Both build
    /// paths reach this through `FileStaging::prepare`, so the refusal lands on
    /// them and on nothing else.
    pub fn entrypoint_with_source(&self) -> Result<Option<(&str, EntrypointSource)>> {
        if let Some(engine) = self.active_engine()? {
            if let Some(entrypoint) = &self.env.entrypoint {
                anyhow::bail!(
                    "{} is set to {}, but this build targets {} — engine builds boot through wavedash's own entrypoint, so the value would be ignored. Unset {}, or drop the engine that brought it into play.",
                    ENV_ENTRYPOINT,
                    entrypoint,
                    engine.kind.as_label(),
                    ENV_ENTRYPOINT
                );
            }
            return Ok(None);
        }
        if let Some(entrypoint) = &self.env.entrypoint {
            if self.first_read_of(Field::Entrypoint) {
                print_override_notice(&entrypoint_notice(entrypoint));
            }
            return Ok(Some((entrypoint, EntrypointSource::Env)));
        }
        Ok(Some(match self.entrypoint.as_deref() {
            Some(entrypoint) => (entrypoint, EntrypointSource::Config),
            None => (DEFAULT_ENTRYPOINT, EntrypointSource::Default),
        }))
    }

    /// The file the section says to boot, which it has to have said. `Err` rather
    /// than a silent omission because the build the API would take without it
    /// boots an executable-engine shell pointed at nothing.
    fn executable<'a>(
        &self,
        kind: EngineKind,
        section: &'static str,
        engine: &'a ExecutableEngineSection,
    ) -> Result<&'a str> {
        engine
            .executable
            .as_deref()
            .ok_or_else(|| self.missing_engine_field(kind, section, "executable"))
    }

    /// For executable-style engines (JSDOS/Ruffle/Ren'Py), returns the
    /// entrypointParams (executable + optional loader_url).
    pub fn executable_entrypoint_params(&self) -> Result<Option<serde_json::Value>> {
        let Some((kind, section, engine)) = self.executable_section() else {
            return Ok(None);
        };
        let mut params =
            serde_json::json!({ "executable": self.executable(kind, section, engine)? });
        if let Some(loader_url) = &engine.loader_url {
            params["loaderUrl"] = serde_json::json!(loader_url);
        }
        Ok(Some(params))
    }

    /// For executable-style engines (JSDOS/Ruffle/Ren'Py), returns all files
    /// that must exist in upload_dir.
    pub fn executable_files_to_validate(&self) -> Result<Vec<&str>> {
        let Some((kind, section, engine)) = self.executable_section() else {
            return Ok(Vec::new());
        };
        let mut files = vec![self.executable(kind, section, engine)?];
        if let Some(loader_url) = &engine.loader_url {
            files.push(loader_url);
        }
        Ok(files)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Stand-in for the process environment: raw values in, a captured
    /// [`EnvOverrides`] out. Values are passed through verbatim — blanks included
    /// — because trimming here would mean asserting against the mock instead of
    /// against `EnvOverrides::capture`. Nothing touches the process's own
    /// environment, which is what keeps these hermetic on cargo's shared threads.
    fn overrides(pairs: &[(&str, &str)]) -> EnvOverrides {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        EnvOverrides::capture(|name| vars.get(name).cloned())
    }

    /// A config as `with_overrides` builds it when the file at `config_path`
    /// parsed to `toml_str`.
    fn from_file(toml_str: &str, env: EnvOverrides) -> WavedashConfig {
        let mut config: WavedashConfig =
            toml::from_str(toml_str).expect("test config should parse");
        config.from_file = true;
        config.config_path = PathBuf::from("/games/thing/wavedash.toml");
        config.treat_blank_file_values_as_unset();
        config.env = env;
        config
    }

    /// And as it builds one when there's no file there at all.
    fn without_file(env: EnvOverrides) -> WavedashConfig {
        WavedashConfig {
            config_path: PathBuf::from("./wavedash.toml"),
            env,
            ..Default::default()
        }
    }

    /// A real file on disk, for the paths that can't be reached without one. The
    /// `TempDir` comes back because dropping it deletes the directory.
    fn config_file(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("wavedash.toml");
        std::fs::write(&path, contents).expect("write config");
        (dir, path)
    }

    /// A path nothing writes to, for the missing-file branches.
    fn missing_config() -> PathBuf {
        PathBuf::from("/nonexistent/wavedash-should-not-exist/wavedash.toml")
    }

    fn active(config: &WavedashConfig) -> ActiveEngine<'_> {
        config
            .active_engine()
            .expect("engine should resolve")
            .expect("an engine should be in play")
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
        let config = from_file(
            CUSTOM_CONFIG,
            overrides(&[
                (ENV_GAME_ID, "from_env"),
                (ENV_UPLOAD_DIR, "out/web"),
                (ENV_ENTRYPOINT, "start.html"),
            ]),
        );

        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));
        assert_eq!(config.entrypoint().unwrap(), Some("start.html"));
    }

    /// The override is where the value came from, so that's what the source says
    /// — which is how `FileStaging` knows to blame the variable and not the file.
    #[test]
    fn an_overridden_entrypoint_reports_the_env_as_its_source() {
        let config = from_file(CUSTOM_CONFIG, overrides(&[(ENV_ENTRYPOINT, "start.html")]));

        assert_eq!(
            config.entrypoint_with_source().unwrap(),
            Some(("start.html", EntrypointSource::Env))
        );

        let from_toml = from_file(CUSTOM_CONFIG, overrides(&[]));
        assert_eq!(
            from_toml.entrypoint_with_source().unwrap(),
            Some(("game.html", EntrypointSource::Config))
        );

        let neither = without_file(overrides(&[]));
        assert_eq!(
            neither.entrypoint_with_source().unwrap(),
            Some((DEFAULT_ENTRYPOINT, EntrypointSource::Default))
        );
    }

    #[test]
    fn a_field_is_announced_on_first_read_and_only_that_field() {
        let config = from_file(
            GODOT_CONFIG,
            overrides(&[
                (ENV_GAME_ID, "from_env"),
                (ENV_UPLOAD_DIR, "out/web"),
                (ENV_GODOT_VERSION, "4.3"),
            ]),
        );

        // Nothing read yet, so nothing announced.
        assert_eq!(config.announced.load(Ordering::Relaxed), 0);

        // What `publish` does: read the game id and nothing else. Only that
        // field is announced — the other two overrides stay quiet.
        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.announced.load(Ordering::Relaxed), Field::GameId.bit());

        // Re-reading is a no-op: the bit is already set.
        let _ = config.game_id();
        assert_eq!(config.announced.load(Ordering::Relaxed), Field::GameId.bit());

        // Reading the engine version brings in its notice too.
        assert_eq!(config.engine_version().unwrap(), Some("4.3"));
        assert_eq!(
            config.announced.load(Ordering::Relaxed),
            Field::GameId.bit() | Field::Engine.bit()
        );
    }

    /// A field the file supplied has no override to announce, so the read must
    /// leave the bit unspent rather than burn it on silence.
    #[test]
    fn a_field_with_no_override_announces_nothing() {
        let config = from_file(GODOT_CONFIG, overrides(&[]));

        assert_eq!(config.game_id().unwrap(), "from_file");
        assert_eq!(config.engine_version().unwrap(), Some("4.2"));
        assert_eq!(config.announced.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn engine_field_stays_quiet_when_the_config_has_no_engine() {
        let config = from_file(CUSTOM_CONFIG, overrides(&[(ENV_GAME_ID, "from_env")]));

        // No engine section and no version to hand out, so nothing to announce.
        assert_eq!(config.engine_version().unwrap(), None);
        assert_eq!(config.announced.load(Ordering::Relaxed), 0);
    }

    /// The CI shape this rule exists for: `WAVEDASH_GAME_ID: ${{ vars.MISSING }}`
    /// expands to `""`, and must read as "not set" rather than wiping out the
    /// file's value. The blanks reach `capture` unfiltered here, so this covers
    /// the wiring and not just `non_blank` in isolation.
    #[test]
    fn file_values_survive_when_overrides_are_blank() {
        let config = from_file(
            CUSTOM_CONFIG,
            overrides(&[
                (ENV_GAME_ID, ""),
                (ENV_UPLOAD_DIR, "   "),
                (ENV_ENTRYPOINT, "\t"),
                (ENV_GODOT_VERSION, ""),
            ]),
        );

        assert_eq!(config.game_id().unwrap(), "from_file");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("dist"));
        assert_eq!(config.entrypoint().unwrap(), Some("game.html"));
        assert_eq!(config.engine_type().unwrap(), None);
        assert_eq!(
            config.announced.load(Ordering::Relaxed),
            0,
            "nothing was overridden, so nothing should be announced"
        );
    }

    #[test]
    fn overrides_are_trimmed() {
        let config = from_file(
            CUSTOM_CONFIG,
            overrides(&[(ENV_GAME_ID, "  from_env\n"), (ENV_UPLOAD_DIR, " out/web ")]),
        );

        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));
    }

    /// `stat`/`achievement`/`publish` read nothing but the game id, so a fully
    /// overridden run has no reason to need a file on disk.
    #[test]
    fn a_config_file_is_unnecessary_when_overrides_supply_what_is_read() {
        let config = without_file(overrides(&[
            (ENV_GAME_ID, "from_env"),
            (ENV_UPLOAD_DIR, "out/web"),
        ]));

        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));
        // No engine and no entrypoint set anywhere: `build push` still has a
        // usable default, so nothing else is required of the environment.
        assert_eq!(config.entrypoint().unwrap(), Some("index.html"));
    }

    /// The flip side: a field the environment didn't supply fails on read, and
    /// the error names both places it could have come from.
    #[test]
    fn a_field_no_source_supplied_fails_on_read() {
        let config = without_file(overrides(&[(ENV_GAME_ID, "from_env")]));

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
        let config = from_file(
            "game_id = \"from_file\"\n",
            overrides(&[(ENV_UPLOAD_DIR, "out/web")]),
        );
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));

        let bare = from_file("game_id = \"from_file\"\n", overrides(&[]));
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
        let config = from_file(
            "game_id = \"\"\nupload_dir = \"   \"\nentrypoint = \"\t\"\n",
            overrides(&[]),
        );

        let err = config.upload_dir().expect_err("blank is not a directory");
        assert!(err.to_string().contains("upload_dir"), "got: {}", err);
        assert!(
            config.game_id().is_err(),
            "a blank game_id must not reach the API as an empty path segment"
        );
        // Engine-less, so entrypoint falls back to its default rather than "".
        assert_eq!(config.entrypoint().unwrap(), Some("index.html"));
    }

    /// Blank in the file, real value in the environment: the override supplies
    /// it, exactly as it would for an absent field.
    #[test]
    fn an_override_fills_in_a_blank_file_value() {
        let config = from_file(
            "game_id = \"\"\nupload_dir = \"\"\n",
            overrides(&[(ENV_GAME_ID, "from_env"), (ENV_UPLOAD_DIR, " out/web ")]),
        );

        assert_eq!(config.game_id().unwrap(), "from_env");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));
    }

    #[test]
    fn padded_file_values_are_trimmed_like_overrides() {
        let config = from_file(
            "game_id = \"  padded \"\nupload_dir = \" dist \"\n",
            overrides(&[]),
        );

        assert_eq!(config.game_id().unwrap(), "padded");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("dist"));
    }

    #[test]
    fn engine_version_override_replaces_existing_section_version() {
        let config = from_file(GODOT_CONFIG, overrides(&[(ENV_GODOT_VERSION, "4.3")]));

        assert_eq!(config.engine_type().unwrap(), Some(EngineKind::Godot));
        assert_eq!(config.engine_version().unwrap(), Some("4.3"));
        assert_eq!(
            config.engine_notices(&active(&config)),
            vec!["WAVEDASH_GODOT_VERSION → [godot].version = 4.3"]
        );
    }

    #[test]
    fn engine_version_override_adds_section_when_config_declares_no_engine() {
        let config = from_file(
            "game_id = \"g\"\nupload_dir = \"dist\"\n",
            overrides(&[(ENV_UNITY_VERSION, "2022.3")]),
        );

        assert_eq!(config.engine_type().unwrap(), Some(EngineKind::Unity));
        assert_eq!(config.engine_version().unwrap(), Some("2022.3"));
        let notices = config.engine_notices(&active(&config));
        assert_eq!(notices.len(), 1, "got: {:?}", notices);
        assert!(
            notices[0].contains("config declared no engine"),
            "adding a section should say so: {}",
            notices[0]
        );
    }

    /// The file's own `entrypoint` is stranded by an override that adds an engine
    /// — nothing to refuse, since the user didn't set it in this environment, so
    /// it has to be said out loud instead.
    #[test]
    fn adding_an_engine_says_the_config_entrypoint_stopped_being_used() {
        let config = from_file(CUSTOM_CONFIG, overrides(&[(ENV_GODOT_VERSION, "4.3")]));

        assert_eq!(config.entrypoint().unwrap(), None);
        let shadowed = config
            .engine_notices(&active(&config))
            .into_iter()
            .find(|text| text.contains("game.html"))
            .expect("the stranded entrypoint should be announced");
        assert!(shadowed.contains("no longer used"), "got: {}", shadowed);
    }

    #[test]
    fn adding_an_engine_is_quiet_when_the_config_set_no_entrypoint() {
        let config = from_file(
            "game_id = \"g\"\nupload_dir = \"dist\"\n",
            overrides(&[(ENV_GODOT_VERSION, "4.3")]),
        );

        assert_eq!(config.engine_notices(&active(&config)).len(), 1);
    }

    // ---- What only a build has a stake in, refused on the read that reads it ----

    #[test]
    fn entrypoint_override_is_rejected_when_an_engine_would_ignore_it() {
        let config = from_file(GODOT_CONFIG, overrides(&[(ENV_ENTRYPOINT, "start.html")]));

        let err = config
            .entrypoint()
            .expect_err("an engine build ignores the entrypoint, so don't hand one out");
        assert!(err.to_string().contains(ENV_ENTRYPOINT), "got: {}", err);
        assert!(err.to_string().contains("GODOT"), "got: {}", err);
    }

    /// The compound case: the config declares no engine, so the entrypoint
    /// override looks applicable right up until the version override adds one.
    #[test]
    fn entrypoint_override_is_rejected_when_an_override_adds_the_engine() {
        let config = from_file(
            CUSTOM_CONFIG,
            overrides(&[(ENV_ENTRYPOINT, "start.html"), (ENV_GODOT_VERSION, "4.3")]),
        );

        let err = config
            .entrypoint()
            .expect_err("the added [godot] section makes the entrypoint inert");
        assert!(err.to_string().contains(ENV_ENTRYPOINT), "got: {}", err);
    }

    #[test]
    fn engine_version_override_refuses_to_switch_engines() {
        let config = from_file(GODOT_CONFIG, overrides(&[(ENV_UNITY_VERSION, "2022.3")]));

        // Both engine reads refuse it, not just whichever a caller reaches first.
        let err = config
            .engine_type()
            .expect_err("unity override should conflict with [godot]");
        assert!(err.to_string().contains("[godot]"), "got: {}", err);
        assert!(config.engine_version().is_err(), "the version read too");
    }

    #[test]
    fn both_engine_version_overrides_is_an_error() {
        let config = from_file(
            GODOT_CONFIG,
            overrides(&[(ENV_GODOT_VERSION, "4.3"), (ENV_UNITY_VERSION, "2022.3")]),
        );

        let err = config
            .engine_type()
            .expect_err("two engine versions should conflict");
        assert!(err.to_string().contains(ENV_UNITY_VERSION), "got: {}", err);
        assert!(config.engine_version().is_err(), "the version read too");
    }

    /// The point of resolving on the read: an engine question is nothing to
    /// `publish`/`stat`/`achievement`, which read a game id and stop. The same
    /// environment that refuses the build must not refuse them.
    #[test]
    fn a_build_only_refusal_leaves_the_other_fields_readable() {
        for env in [
            overrides(&[(ENV_ENTRYPOINT, "start.html")]),
            overrides(&[(ENV_GODOT_VERSION, "4.3"), (ENV_UNITY_VERSION, "2022.3")]),
            overrides(&[(ENV_UNITY_VERSION, "2022.3")]),
        ] {
            let config = from_file(GODOT_CONFIG, env);

            assert_eq!(config.game_id().unwrap(), "from_file");
            assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("build/web"));
        }
    }

    // ---- resolve_game_id: flag, then env, then file ----

    #[test]
    fn the_game_id_flag_beats_the_env_var_and_the_file() {
        let (_dir, path) = config_file(CUSTOM_CONFIG);
        let id = resolve_game_id_with(
            Some("from_flag"),
            &path,
            overrides(&[(ENV_GAME_ID, "from_env")]),
        )
        .expect("the flag supplies it");

        assert_eq!(id, "from_flag");
    }

    #[test]
    fn the_game_id_env_var_beats_the_file() {
        let (_dir, path) = config_file(CUSTOM_CONFIG);
        let id = resolve_game_id_with(None, &path, overrides(&[(ENV_GAME_ID, "from_env")]))
            .expect("the override supplies it");

        assert_eq!(id, "from_env");
    }

    #[test]
    fn the_game_id_falls_back_to_the_file() {
        let (_dir, path) = config_file(CUSTOM_CONFIG);
        let id = resolve_game_id_with(None, &path, overrides(&[])).expect("the file supplies it");

        assert_eq!(id, "from_file");
    }

    /// Blank-is-unset reaches this path too: an unpopulated CI variable falls
    /// through to the file instead of resolving as an empty game id.
    #[test]
    fn a_blank_game_id_override_falls_through_to_the_file() {
        let (_dir, path) = config_file(CUSTOM_CONFIG);
        let id = resolve_game_id_with(None, &path, overrides(&[(ENV_GAME_ID, "   ")]))
            .expect("blank is not a value, so the file still supplies it");

        assert_eq!(id, "from_file");
    }

    #[test]
    fn a_game_id_from_nowhere_names_the_env_var_and_the_file_it_looked_for() {
        let err = resolve_game_id_with(None, &missing_config(), overrides(&[]))
            .expect_err("nothing supplies it");

        assert!(err.to_string().contains(ENV_GAME_ID), "got: {}", err);
        assert!(
            err.to_string().contains("wavedash-should-not-exist"),
            "got: {}",
            err
        );
    }

    /// The file exists but omits it, so the fallback reaches the accessor and its
    /// error picks up the flag as the third way out.
    #[test]
    fn a_file_without_a_game_id_points_at_the_flag() {
        let (_dir, path) = config_file("upload_dir = \"dist\"\n");
        let err =
            resolve_game_id_with(None, &path, overrides(&[])).expect_err("the file omits it");

        assert!(err.to_string().contains("--game-id"), "got: {}", err);
    }

    /// The reported bug, through the real entry point: a stale
    /// `WAVEDASH_ENTRYPOINT` left over from a custom-HTML project used to abort
    /// `stat`/`achievement`/`publish` under a `[godot]` config — and only when
    /// `--game-id` was absent, since the flag returns before the config is ever
    /// read. Both spellings have to agree.
    #[test]
    fn a_build_only_refusal_does_not_stop_a_game_id_from_resolving() {
        let (_dir, path) = config_file(GODOT_CONFIG);
        let stale = || overrides(&[(ENV_ENTRYPOINT, "start.html")]);

        assert_eq!(
            resolve_game_id_with(None, &path, stale()).expect("no build here, nothing to refuse"),
            "from_file"
        );
        assert_eq!(
            resolve_game_id_with(Some("from_flag"), &path, stale())
                .expect("the flag path agreed all along"),
            "from_flag"
        );
    }

    // ---- with_overrides: whether there is a config at all ----

    /// The file-optional rule itself, as opposed to what the accessors do after:
    /// a missing file is survivable when the environment is configuring the run.
    #[test]
    fn a_config_is_built_without_a_file_when_an_override_is_set() {
        let config =
            WavedashConfig::with_overrides(&missing_config(), overrides(&[(ENV_GAME_ID, "from_env")]))
                .expect("an override means the environment is configuring this run");

        assert!(!config.from_file);
        assert_eq!(config.game_id().unwrap(), "from_env");
        // Supplied by neither source, so it fails on read rather than at load.
        assert!(config.upload_dir().is_err());
    }

    #[test]
    fn a_missing_file_with_nothing_overridden_says_where_it_looked() {
        let err = WavedashConfig::with_overrides(&missing_config(), overrides(&[]))
            .expect_err("no file and no environment means no config");

        assert!(err.to_string().contains("wavedash init"), "got: {}", err);
        assert!(
            err.to_string().contains("wavedash-should-not-exist"),
            "got: {}",
            err
        );
    }

    /// A blank override is not an override, so it can't stand in for a missing
    /// file either — the accessors' rule, applied to the decision to build a
    /// config at all.
    #[test]
    fn a_blank_only_environment_counts_as_no_override() {
        let err = WavedashConfig::with_overrides(
            &missing_config(),
            overrides(&[(ENV_GAME_ID, "  "), (ENV_UPLOAD_DIR, "")]),
        )
        .expect_err("blank overrides supply nothing");

        assert!(err.to_string().contains("wavedash init"), "got: {}", err);
    }

    #[test]
    fn a_file_that_exists_is_read_and_the_overrides_layer_onto_it() {
        let (_dir, path) = config_file(CUSTOM_CONFIG);
        let config =
            WavedashConfig::with_overrides(&path, overrides(&[(ENV_UPLOAD_DIR, "out/web")]))
                .expect("the file parses");

        assert!(config.from_file);
        assert_eq!(config.game_id().unwrap(), "from_file");
        assert_eq!(config.upload_dir().unwrap(), &PathBuf::from("out/web"));
    }

    /// And the guarantee the refusal exists for, kept end to end: the config is
    /// built fine, then the read both build paths reach first refuses it.
    #[test]
    fn an_inert_entrypoint_is_still_refused_at_the_build_read() {
        let (_dir, path) = config_file(GODOT_CONFIG);
        let config =
            WavedashConfig::with_overrides(&path, overrides(&[(ENV_ENTRYPOINT, "start.html")]))
                .expect("building the config no longer validates it");

        // What `FileStaging::prepare` reads first, for both `build push` and `dev`.
        let err = config
            .entrypoint_with_source()
            .expect_err("the build must still be stopped");
        assert!(err.to_string().contains(ENV_ENTRYPOINT), "got: {}", err);
        assert!(err.to_string().contains("GODOT"), "got: {}", err);
    }

    #[test]
    fn a_conflicting_engine_override_is_still_refused_at_the_build_read() {
        let (_dir, path) = config_file(GODOT_CONFIG);
        let config =
            WavedashConfig::with_overrides(&path, overrides(&[(ENV_UNITY_VERSION, "2022.3")]))
                .expect("building the config no longer validates it");

        assert_eq!(config.game_id().unwrap(), "from_file");
        let err = config
            .engine_type()
            .expect_err("the build must still be stopped");
        assert!(err.to_string().contains("[godot]"), "got: {}", err);
    }

    /// JSDOS/Ruffle/Ren'Py have no version override, so their version has to keep
    /// coming off the file's own section — which `declared_engine` is responsible
    /// for now, rather than the separate `executable_section` lookup it used to be.
    #[test]
    fn an_executable_engine_supplies_its_own_version() {
        for (section, kind, label) in [
            ("jsdos", EngineKind::JsDos, "JSDOS"),
            ("ruffle", EngineKind::Ruffle, "RUFFLE"),
            ("renpy", EngineKind::RenPy, "RENPY"),
        ] {
            let toml = format!(
                "game_id = \"g\"\nupload_dir = \"dist\"\n\n[{}]\nversion = \"1.2\"\nexecutable = \"game.exe\"\n",
                section
            );
            let config = from_file(&toml, overrides(&[]));

            assert_eq!(config.engine_type().unwrap(), Some(kind), "[{}]", section);
            assert_eq!(config.engine_version().unwrap(), Some("1.2"), "[{}]", section);
            // An engine build, so there's no entrypoint to hand out.
            assert_eq!(config.entrypoint().unwrap(), None, "[{}]", section);
            assert_eq!(
                config.executable_files_to_validate().unwrap(),
                vec!["game.exe"],
                "[{}]",
                section
            );
            // Nothing overrode anything, so nothing was announced.
            assert_eq!(
                config.announced.load(Ordering::Relaxed),
                0,
                "[{}]",
                section
            );

            // And an inert entrypoint override names the engine that ignores it.
            let with_entrypoint = from_file(&toml, overrides(&[(ENV_ENTRYPOINT, "start.html")]));
            let err = with_entrypoint
                .entrypoint()
                .expect_err("an engine build ignores the entrypoint");
            assert!(err.to_string().contains(label), "got: {}", err);
        }
    }

    /// Holds up the `expect` in `dev::resolve_engine_entry`.
    #[test]
    fn an_engine_kind_always_arrives_with_a_version() {
        for section in ["godot", "unity", "jsdos", "ruffle", "renpy"] {
            let declared = format!("game_id = \"g\"\nupload_dir = \"dist\"\n\n[{}]\n", section);

            let versioned = from_file(&format!("{}version = \"1.2\"\n", declared), overrides(&[]));
            assert!(
                versioned.engine_type().unwrap().is_some(),
                "[{}] declared a kind",
                section
            );
            assert_eq!(
                versioned.engine_version().unwrap(),
                Some("1.2"),
                "[{}] kind without version",
                section
            );

            // A missing version fails the read `engine_type` shares, so `dev` never
            // gets a kind to call `resolve_engine_entry` with in the first place.
            let bare = from_file(&declared, overrides(&[]));
            assert!(
                bare.engine_type().is_err(),
                "[{}] a missing version has to stop engine_type too",
                section
            );
            assert!(
                bare.engine_version().is_err(),
                "[{}] a missing version has to be an Err, not the None dev would hit",
                section
            );
        }

        for (section, var) in [("godot", ENV_GODOT_VERSION), ("unity", ENV_UNITY_VERSION)] {
            let config = from_file(
                &format!("game_id = \"g\"\nupload_dir = \"dist\"\n\n[{}]\n", section),
                overrides(&[(var, "4.3")]),
            );

            assert_eq!(config.engine_version().unwrap(), Some("4.3"), "{}", var);
        }

        let engineless = from_file("game_id = \"g\"\nupload_dir = \"dist\"\n", overrides(&[]));
        assert_eq!(engineless.engine_type().unwrap(), None);
        assert_eq!(engineless.engine_version().unwrap(), None);
    }

    /// A godot/unity version override can't retarget one of them either.
    #[test]
    fn an_engine_version_override_cannot_switch_away_from_an_executable_engine() {
        let config = from_file(
            "game_id = \"g\"\nupload_dir = \"dist\"\n\n[jsdos]\nversion = \"8.x\"\nexecutable = \"game.exe\"\n",
            overrides(&[(ENV_GODOT_VERSION, "4.3")]),
        );

        let err = config.engine_type().expect_err("jsdos is already declared");
        assert!(err.to_string().contains("[jsdos]"), "got: {}", err);
        // The fields it doesn't concern still read.
        assert_eq!(config.game_id().unwrap(), "g");
    }

    /// Two engine sections in the file is a question about the file alone, so it
    /// answers the same however the environment is set — and it now reaches the
    /// entrypoint read instead of being swallowed into a `None`.
    #[test]
    fn two_declared_engines_is_an_error_on_every_engine_read() {
        let config = from_file(
            "game_id = \"g\"\nupload_dir = \"dist\"\n\n[godot]\nversion = \"4.2\"\n\n[unity]\nversion = \"2022.3\"\n",
            overrides(&[]),
        );

        assert!(config.engine_type().is_err());
        assert!(config.engine_version().is_err());
        assert!(config.entrypoint().is_err());
        assert_eq!(config.game_id().unwrap(), "g");
    }

    /// The last place blank wasn't unset. A templated `version = "${GODOT}"` that
    /// expanded to nothing used to be handed to the API as the build's engine
    /// version, and used to build a play URL for version "" in `dev`. Written
    /// blank and not written at all are the same thing, and both are reported
    /// when something asks what version this build targets — not before.
    #[test]
    fn a_blank_engine_version_is_no_version() {
        for section in [
            "[godot]\nversion = \"\"",
            "[godot]\nversion = \"  \"",
            "[godot]",
        ] {
            let toml = format!("game_id = \"g\"\nupload_dir = \"dist\"\n\n{}\n", section);
            let config = from_file(&toml, overrides(&[]));

            let err = config
                .engine_version()
                .expect_err(&format!("{:?} supplies no version", section));
            assert!(
                err.to_string().contains("[godot] has no version")
                    && err.to_string().contains(ENV_GODOT_VERSION),
                "got: {}",
                err
            );
            // Same answer whichever engine read asks.
            assert!(config.engine_type().is_err(), "{:?}", section);
            assert!(config.entrypoint().is_err(), "{:?}", section);
            // And still nothing a command that never asks has to care about.
            assert_eq!(config.game_id().unwrap(), "g", "{:?}", section);
        }
    }

    /// Why the blank is unset field-by-field rather than by dropping the section:
    /// the file still declares Godot, so the override fills in the version the
    /// section left out — it doesn't bring an engine into play that was already
    /// there, which is what the notice would say if the section had been dropped.
    #[test]
    fn a_version_override_supplies_what_a_blank_section_left_out() {
        let config = from_file(
            "game_id = \"g\"\nupload_dir = \"dist\"\nentrypoint = \"game.html\"\n\n[godot]\nversion = \"\"\n",
            overrides(&[(ENV_GODOT_VERSION, "4.3")]),
        );

        assert_eq!(config.engine_type().unwrap(), Some(EngineKind::Godot));
        assert_eq!(config.engine_version().unwrap(), Some("4.3"));
        assert_eq!(
            config.engine_notices(&active(&config)),
            vec!["WAVEDASH_GODOT_VERSION → [godot].version = 4.3"]
        );
    }

    /// The other half of that: a dropped section would have let the *other*
    /// engine's override quietly retarget the build.
    #[test]
    fn a_blank_engine_section_still_refuses_an_engine_switch() {
        let config = from_file(
            "game_id = \"g\"\nupload_dir = \"dist\"\n\n[godot]\nversion = \"\"\n",
            overrides(&[(ENV_UNITY_VERSION, "2022.3")]),
        );

        let err = config
            .engine_type()
            .expect_err("the file still declares godot");
        assert!(
            err.to_string().contains("[godot]") && err.to_string().contains(ENV_UNITY_VERSION),
            "got: {}",
            err
        );
    }

    /// No override can supply an executable, so a blank one is only ever the
    /// file's omission — reported rather than sent as an empty `executable` for
    /// the shell to boot. The path it would otherwise be validated as is
    /// `upload_dir.join("")`, which is the directory, which exists.
    #[test]
    fn a_blank_executable_is_no_executable() {
        let config = from_file(
            "game_id = \"g\"\nupload_dir = \"dist\"\n\n[jsdos]\nversion = \"8.x\"\nexecutable = \"  \"\n",
            overrides(&[]),
        );

        for err in [
            config.executable_files_to_validate().unwrap_err(),
            config.executable_entrypoint_params().unwrap_err(),
        ] {
            assert!(
                err.to_string().contains("[jsdos] has no executable"),
                "got: {}",
                err
            );
            // There's no WAVEDASH_JSDOS_EXECUTABLE to point at.
            assert!(!err.to_string().contains("set WAVEDASH"), "got: {}", err);
        }
        // The version was there, so that read is unaffected.
        assert_eq!(config.engine_version().unwrap(), Some("8.x"));
    }

    #[test]
    fn a_blank_loader_url_is_no_loader_url() {
        let config = from_file(
            "game_id = \"g\"\nupload_dir = \"dist\"\n\n[ruffle]\nversion = \"0.1\"\nexecutable = \"game.swf\"\nloader_url = \"\"\n",
            overrides(&[]),
        );

        assert_eq!(
            config.executable_files_to_validate().unwrap(),
            vec!["game.swf"]
        );
        assert_eq!(
            config.executable_entrypoint_params().unwrap(),
            Some(serde_json::json!({ "executable": "game.swf" }))
        );
    }
}
