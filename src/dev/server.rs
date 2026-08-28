//! Local static file server for `wavedash dev` — the game runs top-level at
//! `http://localhost:<port>`; entry rule mirrors the play worker's embed.
//!
//! Auth is per-browser: credential-less requests bounce to the mainsite
//! /auth/dev, which redirects back with a short-lived playKey that the
//! callback exchanges server-side for a 1-day gameplay session (wd_session
//! cookie) — no long-lived credential ever rides in a URL. /auth/refresh
//! trades the cookie for fresh 1h gameplay JWTs. No auth state held here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use axum::{
    body::Body,
    extract::{Query, Request, State},
    http::{header, HeaderMap, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::net::TcpListener;

const SDK_JS_VERSION: &str = include_str!("sdk-js-version");

type QueryParams = HashMap<String, String>;

/// Classic parser-blocking IIFE (auto-runs setupWavedashSDK): the only way
/// `window.Wavedash` exists before game scripts parse — module scripts are
/// always deferred. jsdelivr sends ACAO * + CORP, satisfying COEP.
fn cdn_inject_url() -> String {
    format!(
        "https://cdn.jsdelivr.net/npm/@wvdsh/sdk-js@{}/dist/inject.global.js",
        SDK_JS_VERSION.trim()
    )
}

/// Same-origin, which COEP allows without the CORP a cross-origin build needs.
const LOCAL_SDK_URL: &str = "/__wavedash/sdk.js";

fn inject_url(sdk_js: Option<&Path>) -> String {
    match sdk_js {
        Some(_) => LOCAL_SDK_URL.to_string(),
        None => cdn_inject_url(),
    }
}

/// `{{NAME}}` placeholders substitute data only — logic stays in the template.
const SHELL_TEMPLATE: &str = include_str!("shell.html");
const DEV_JS: &str = include_str!("dev.js");

const TEXT: &str = "text/plain; charset=utf-8";
const HTML: &str = "text/html; charset=utf-8";

pub struct ServeConfig {
    pub upload_dir: PathBuf,
    /// Entry filename relative to upload_dir (e.g. `index.html` or `game.js`).
    pub entry: String,
    pub verbose: bool,
    /// Cookies are host-scoped, so names carry the build uuid to keep
    /// concurrent and successive dev servers from clobbering each other.
    pub build_uuid: String,
    /// Dev-session lock registry (`<uuid>.lock` per running server). The cookie
    /// sweep consults it to expire only orphaned builds' cookies, never a live
    /// concurrent server's — see `stale_wavedash_cookies`.
    pub sessions_dir: PathBuf,
    /// Echoed back on the auth callback; must match or the handoff is rejected.
    pub state_token: String,
    /// Mainsite /auth/dev URL that `/` bounces credential-less browsers to.
    pub auth_url: String,
    /// Developer API key + backend host for the gameplay-JWT refresh exchange.
    pub api_key: String,
    pub api_host: String,
    pub client: reqwest::Client,
    /// Engine builds boot via play's real default entrypoint — the prod path.
    pub engine_entry: Option<EngineEntry>,
    /// Local sdk-js bundle to serve in place of the pinned CDN build.
    pub sdk_js: Option<PathBuf>,
    /// Backend public keys (/.well-known/jwks.json), fetched once on demand.
    pub jwks: tokio::sync::OnceCell<jsonwebtoken::jwk::JwkSet>,
}

impl ServeConfig {
    /// Per-browser gameplay session token (HttpOnly — page JS never needs it).
    /// Build-scoped: a relaunched server can reuse a port, but never a build.
    fn session_cookie_name(&self) -> String {
        format!("wd_session_{}", self.build_uuid)
    }

    /// Last issued gameplay JWT (HttpOnly) — reused by /auth/refresh while
    /// still fresh, sparing the backend round-trip.
    fn jwt_cookie_name(&self) -> String {
        format!("wd_jwt_{}", self.build_uuid)
    }

    /// SDKConfig for this browser (HttpOnly) — inlined into every served page
    /// as `window.__wavedashSdkConfig`, which `setupWavedashSDK()` falls back
    /// to when the URL has no `?sdkconfig=`. Keeps game URLs clean.
    fn sdkconfig_cookie_name(&self) -> String {
        format!("wd_sdkconfig_{}", self.build_uuid)
    }

    /// The `wvdsh_` launch params of a URL that had to bounce through the
    /// mainsite for auth, which returns to `/` with the query gone. Written on
    /// the way out, replayed and expired by the callback.
    fn pending_params_cookie_name(&self) -> String {
        format!("wd_pending_{}", self.build_uuid)
    }
}

pub struct EngineEntry {
    pub entrypoint_url: String,
    /// `window.entrypointParams` JSON, from the same parsers prod runs at upload.
    pub params_json: String,
}

pub async fn run(listener: TcpListener, cfg: ServeConfig) -> Result<()> {
    let verbose = cfg.verbose;
    let mut app = Router::new()
        .route("/__wavedash/callback", get(handle_callback))
        .route("/__wavedash/dev.js", get(handle_dev_js))
        .route(LOCAL_SDK_URL, get(handle_sdk_js))
        .route("/auth/refresh", post(handle_auth_refresh))
        .route("/", get(handle_index))
        .fallback(get(handle_static))
        .with_state(Arc::new(cfg));
    if verbose {
        app = app.layer(middleware::from_fn(log_request));
    }
    axum::serve(listener, app).await?;
    Ok(())
}

/// `--verbose` request log: `HH:MM:SS status METHOD /path { params }`.
async fn log_request(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let params = req.uri().query().map(format_query).unwrap_or_default();
    let res = next.run(req).await;
    eprintln!(
        "{}  {}  {} {}{}",
        chrono::Local::now().format("%H:%M:%S"),
        res.status().as_u16(),
        method,
        path,
        params
    );
    res
}

/// Render query params as aligned `key: value` lines indented under the
/// request line. Values are truncated — `sdkconfig` is an entire JSON blob.
fn format_query(query: &str) -> String {
    let decode = |s: &str| {
        urlencoding::decode(s)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| s.to_string())
    };
    let pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect();
    let width = pairs.iter().map(|(k, _)| k.len() + 1).max().unwrap_or(0);
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "\n               {:<width$} {}",
                format!("{k}:"),
                truncate(v, 80)
            )
        })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

#[derive(Deserialize)]
struct CallbackParams {
    play_key: String,
    sdkconfig: String,
    state: String,
}

#[derive(Deserialize)]
struct ExchangeResponse {
    #[serde(rename = "sessionToken")]
    session_token: String,
    #[serde(rename = "gameplayJwt")]
    gameplay_jwt: String,
}

/// The callback carries a short-lived playKey (prod's play-iframe credential),
/// never the session token — exchange it server-side and cookie the session,
/// so no long-lived credential rides in a URL.
async fn handle_callback(
    State(cfg): State<Arc<ServeConfig>>,
    headers: HeaderMap,
    Query(p): Query<CallbackParams>,
) -> Response {
    if p.state != cfg.state_token {
        return respond(StatusCode::BAD_REQUEST, TEXT, None, "Unexpected state");
    }

    let url = format!("{}/api/dev/exchange-playkey", cfg.api_host);
    let upstream = cfg
        .client
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .json(&serde_json::json!({ "playKey": p.play_key }))
        .send()
        .await;

    let exchange = match upstream {
        Ok(res) if res.status().is_success() => match res.json::<ExchangeResponse>().await {
            Ok(body) => body,
            Err(_) => {
                return respond(StatusCode::BAD_GATEWAY, TEXT, None, "Bad exchange response")
            }
        },
        // Expired playKey (e.g. a tab restored past its 2-min TTL).
        Ok(_) => {
            return respond(
                StatusCode::UNAUTHORIZED,
                TEXT,
                None,
                "Sign-in handoff expired — reopen the game URL",
            )
        }
        Err(err) => {
            eprintln!("exchange-playkey request failed: {err}");
            return respond(StatusCode::BAD_GATEWAY, TEXT, None, "Exchange failed");
        }
    };

    let pending_name = cfg.pending_params_cookie_name();
    let location = cookie_value(&headers, &pending_name)
        .filter(|query| !query.is_empty())
        .map_or_else(|| "/".to_string(), |query| format!("/?{query}"));

    let mut builder = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, location)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::SET_COOKIE, expire_cookie(&pending_name))
        .header(
            header::SET_COOKIE,
            set_cookie(&cfg.session_cookie_name(), &exchange.session_token),
        )
        .header(
            header::SET_COOKIE,
            set_cookie(&cfg.jwt_cookie_name(), &exchange.gameplay_jwt),
        )
        .header(
            header::SET_COOKIE,
            set_cookie(
                &cfg.sdkconfig_cookie_name(),
                urlencoding::encode(&p.sdkconfig).as_ref(),
            ),
        );

    // Each dev run mints a fresh build uuid, so its cookies carry new names;
    // host-scoped, they replay to every localhost server (any port) and never
    // self-clear, growing until they overflow some localhost app's request
    // headers. Expire leftovers from builds whose server has exited here — the
    // one request that sees them all — while leaving live concurrent servers'
    // cookies untouched (see stale_wavedash_cookies).
    for stale in stale_wavedash_cookies(&headers, &cfg) {
        builder = builder.header(header::SET_COOKIE, expire_cookie(&stale));
    }
    builder.body(Body::empty()).unwrap()
}

/// 1-day gameplay session lifetime, carried as Max-Age so a build orphaned by
/// a killed server (no later handoff ever sweeps it) still expires on its own
/// rather than lingering as a session cookie that survives browser restores.
const SESSION_MAX_AGE_SECS: u64 = 86_400;

/// A wavedash cookie set for this build's 1-day session.
fn set_cookie(name: &str, value: &str) -> String {
    format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_MAX_AGE_SECS}")
}

/// The deletion form (empty value, immediate expiry) of `set_cookie`; Path and
/// attributes must match for the browser to drop it.
fn expire_cookie(name: &str) -> String {
    format!("{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// Prefixes of the four cookies each dev build issues, suffixed by its uuid.
const COOKIE_PREFIXES: [&str; 4] = ["wd_session_", "wd_jwt_", "wd_sdkconfig_", "wd_pending_"];

/// Names of prior dev builds' wavedash cookies that are safe to expire: any
/// `wd_*_<uuid>` cookie the browser is replaying whose build server is no
/// longer running. A *live* concurrent server's cookies are kept — host-scoped
/// cookies are shared across localhost ports, so a sibling server's cookies
/// sit in this same jar and must survive. Live vs. orphan is decided by the
/// build's session lock (`<uuid>.lock`), which its server holds for as long as
/// it runs; a lock we can take means that server has exited.
fn stale_wavedash_cookies(headers: &HeaderMap, cfg: &ServeConfig) -> Vec<String> {
    let current = [
        cfg.session_cookie_name(),
        cfg.jwt_cookie_name(),
        cfg.sdkconfig_cookie_name(),
        cfg.pending_params_cookie_name(),
    ];
    let Some(raw) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return Vec::new();
    };

    // Cache liveness per uuid — a build has three cookies, but one lock probe.
    let mut liveness: std::collections::HashMap<&str, bool> = std::collections::HashMap::new();
    let mut stale = Vec::new();
    for pair in raw.split(';') {
        let Some((name, _)) = pair.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        let Some(uuid) = COOKIE_PREFIXES.iter().find_map(|p| name.strip_prefix(p)) else {
            continue;
        };
        if current.iter().any(|c| c.as_str() == name) {
            continue;
        }
        let live = *liveness
            .entry(uuid)
            .or_insert_with(|| build_is_live(&cfg.sessions_dir, uuid));
        if !live {
            stale.push(name.to_string());
        }
    }
    stale
}

/// Whether the dev server for `uuid` is still running, per its session lock.
/// A missing lockfile, or one we can lock, means the owner has exited (locks
/// release on process death, crashes included) — so we drop the dead lockfile
/// while we're here. `WouldBlock` means a live server holds it; any other lock
/// error is treated as live, so we never sweep a build we can't prove is gone.
fn build_is_live(sessions_dir: &Path, uuid: &str) -> bool {
    let path = sessions_dir.join(format!("{uuid}.lock"));
    let Ok(file) = std::fs::OpenOptions::new().read(true).open(&path) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            drop(file); // release the lock + close the handle before removing
            let _ = std::fs::remove_file(&path);
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => true,
    }
}

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(rename = "gameplayJwt")]
    gameplay_jwt: String,
}

#[derive(Deserialize)]
struct RefreshParams {
    fresh: Option<String>,
}

/// Exchange the browser's session cookie for a fresh gameplay JWT, so the
/// SDK's refreshes renew for the session's day, not the first JWT's hour.
/// The last JWT rides in a cookie and is served back while still fresh, so
/// most refreshes never leave localhost.
async fn handle_auth_refresh(
    State(cfg): State<Arc<ServeConfig>>,
    Query(p): Query<RefreshParams>,
    headers: HeaderMap,
) -> Response {
    let Some(session_token) = cookie_value(&headers, &cfg.session_cookie_name()) else {
        return respond(StatusCode::UNAUTHORIZED, TEXT, None, "Auth not ready");
    };

    // `?fresh` follows a claims-changing event (e.g. a simulated purchase), so
    // skip the cached JWT and re-mint from the backend.
    if p.fresh.is_none() {
        if let Some(jwt) = cookie_value(&headers, &cfg.jwt_cookie_name()) {
            if jwt_fresh(&cfg, &jwt).await {
                return respond(StatusCode::OK, TEXT, None, jwt);
            }
        }
    }

    let url = format!("{}/api/dev/refresh-gameplay", cfg.api_host);
    let upstream = cfg
        .client
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .json(&serde_json::json!({ "sessionToken": session_token }))
        .send()
        .await;

    match upstream {
        Ok(res) if res.status().is_success() => match res.json::<RefreshResponse>().await {
            Ok(body) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, TEXT)
                .header(header::CACHE_CONTROL, "no-store")
                .header(
                    header::SET_COOKIE,
                    set_cookie(&cfg.jwt_cookie_name(), &body.gameplay_jwt),
                )
                .body(Body::from(body.gameplay_jwt))
                .unwrap(),
            Err(_) => respond(StatusCode::BAD_GATEWAY, TEXT, None, "Bad refresh response"),
        },
        // Clear the dead cookies so the next reload re-runs the handoff.
        Ok(_) => Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::CONTENT_TYPE, TEXT)
            .header(header::CACHE_CONTROL, "no-store")
            .header(
                header::SET_COOKIE,
                expire_cookie(&cfg.session_cookie_name()),
            )
            .header(header::SET_COOKIE, expire_cookie(&cfg.jwt_cookie_name()))
            .body(Body::from("Session expired"))
            .unwrap(),
        Err(err) => {
            eprintln!("refresh-gameplay request failed: {err}");
            respond(StatusCode::BAD_GATEWAY, TEXT, None, "Refresh failed")
        }
    }
}

/// Refresh this long before `exp` so a JWT never dies mid-request.
const JWT_REFRESH_MARGIN_SECS: u64 = 300;

/// Verify the cookie'd JWT against the backend's public JWKS and the margin
/// above. Any failure (unfetchable JWKS, unknown kid, bad signature, near
/// expiry) just falls back to a real backend refresh.
async fn jwt_fresh(cfg: &ServeConfig, jwt: &str) -> bool {
    let jwks = cfg
        .jwks
        .get_or_try_init(|| async {
            let url = format!("{}/.well-known/jwks.json", cfg.api_host);
            let res = cfg.client.get(&url).send().await.map_err(|_| ())?;
            res.json::<jsonwebtoken::jwk::JwkSet>().await.map_err(|_| ())
        })
        .await;
    let Ok(jwks) = jwks else { return false };

    let Ok(header) = jsonwebtoken::decode_header(jwt) else {
        return false;
    };
    let Some(jwk) = header.kid.as_deref().and_then(|kid| jwks.find(kid)) else {
        return false;
    };
    let Ok(key) = jsonwebtoken::DecodingKey::from_jwk(jwk) else {
        return false;
    };

    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.validate_aud = false;
    validation.validate_exp = false; // checked below with the refresh margin

    #[derive(Deserialize)]
    struct Claims {
        exp: u64,
    }
    match jsonwebtoken::decode::<Claims>(jwt, &key, &validation) {
        Ok(token) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(u64::MAX);
            token.claims.exp > now + JWT_REFRESH_MARGIN_SECS
        }
        Err(_) => false,
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    let prefix = format!("{name}=");
    raw.split(';')
        .map(str::trim)
        .find_map(|pair| pair.strip_prefix(&prefix[..]))
        .map(str::to_string)
}

/// The decoded per-browser SDKConfig, present only when this browser also
/// holds the session cookie — the pair is written atomically by the callback.
fn browser_config(cfg: &ServeConfig, headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, &cfg.session_cookie_name())?;
    let raw = cookie_value(headers, &cfg.sdkconfig_cookie_name())?;
    Some(urlencoding::decode(&raw).map_or(raw.clone(), |d| d.into_owned()))
}

const LAUNCH_PARAM_PREFIX: &str = "wvdsh_";

fn launch_param_name(key: &str) -> Option<&str> {
    key.strip_prefix(LAUNCH_PARAM_PREFIX)
        .filter(|name| !name.is_empty())
}

fn launch_params(query: &QueryParams) -> serde_json::Map<String, serde_json::Value> {
    query
        .iter()
        .filter_map(|(key, value)| {
            let name = launch_param_name(key)?;
            Some((name.to_string(), serde_json::Value::String(value.clone())))
        })
        .collect()
}

fn config_with_launch_params(config: &str, query: &QueryParams) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(config) else {
        return config.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return config.to_string();
    };
    object.insert(
        "launchParams".to_string(),
        serde_json::Value::Object(launch_params(query)),
    );
    value.to_string()
}

fn launch_param_query(query: &QueryParams) -> Option<String> {
    let pairs: Vec<String> = query
        .iter()
        .filter(|(key, _)| launch_param_name(key).is_some())
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect();
    (!pairs.is_empty()).then(|| pairs.join("&"))
}

fn bounce_to_auth(cfg: &ServeConfig, query: &QueryParams) -> Response {
    let name = cfg.pending_params_cookie_name();
    let cookie = match launch_param_query(query) {
        Some(pending) => set_cookie(&name, &pending),
        None => expire_cookie(&name),
    };
    ([(header::SET_COOKIE, cookie)], Redirect::to(&cfg.auth_url)).into_response()
}

/// Browsers with this server's cookies get the game; anything else re-runs
/// the handoff — a bare localhost visit works, and a reload heals cleared
/// cookies or an expired session.
async fn handle_index(
    State(cfg): State<Arc<ServeConfig>>,
    headers: HeaderMap,
    Query(query): Query<QueryParams>,
    uri: Uri,
) -> Response {
    let Some(config) = browser_config(&cfg, &headers) else {
        return bounce_to_auth(&cfg, &query);
    };

    let (script_src, params_json) = match &cfg.engine_entry {
        Some(engine_entry) => (
            engine_entry.entrypoint_url.clone(),
            Some(engine_entry.params_json.as_str()),
        ),
        None if cfg.entry.ends_with(".js") => (format!("/{}", cfg.entry), None),
        None => {
            // The HTML entry serves at its real path so relative assets resolve.
            let raw_query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
            return Redirect::to(&format!("/{}{}", cfg.entry, raw_query)).into_response();
        }
    };

    let html = shell(
        cfg.sdk_js.as_deref(),
        &script_src,
        params_json,
        &config_with_launch_params(&config, &query),
    );
    respond(StatusCode::OK, HTML, None, html)
}

async fn handle_static(
    State(cfg): State<Arc<ServeConfig>>,
    headers: HeaderMap,
    Query(query): Query<QueryParams>,
    uri: Uri,
) -> Response {
    let config = browser_config(&cfg, &headers);
    // An HTML nav without this browser's cookies is stale state (cookies
    // cleared) — re-run the handoff instead of serving a game that will 401.
    if config.is_none() && is_html_path(uri.path()) {
        return bounce_to_auth(&cfg, &query);
    }
    serve_file(&cfg, uri.path(), config.as_deref(), &query)
}

fn is_html_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

/// The init gate (see dev.js).
async fn handle_dev_js() -> Response {
    respond(StatusCode::OK, "text/javascript; charset=utf-8", None, DEV_JS)
}

/// Read per request, so rebuilding sdk-js only needs a page reload.
async fn handle_sdk_js(State(cfg): State<Arc<ServeConfig>>) -> Response {
    let Some(path) = cfg.sdk_js.as_deref() else {
        return respond(StatusCode::NOT_FOUND, TEXT, None, "Not Found");
    };
    match std::fs::read(path) {
        Ok(bytes) => respond(StatusCode::OK, "text/javascript; charset=utf-8", None, bytes),
        Err(e) => respond(
            StatusCode::INTERNAL_SERVER_ERROR,
            TEXT,
            None,
            format!("could not read {}: {e}", path.display()),
        ),
    }
}

/// Authed HTML navigations get the config global + SDK tags injected (prod's
/// embed.js mechanism); everything else streams as-is.
fn serve_file(
    cfg: &ServeConfig,
    url_path: &str,
    config: Option<&str>,
    query: &QueryParams,
) -> Response {
    let Some(file_path) = resolve_path(&cfg.upload_dir, url_path) else {
        return respond(StatusCode::NOT_FOUND, TEXT, None, "Not Found");
    };
    let Ok(bytes) = std::fs::read(&file_path) else {
        return respond(StatusCode::NOT_FOUND, TEXT, None, "Not Found");
    };

    let (content_type, encoding) = content_type_and_encoding(url_path);
    if let Some(config) = config {
        if encoding.is_none() && content_type.starts_with("text/html") {
            let injected = inject_sdk(
                cfg.sdk_js.as_deref(),
                &String::from_utf8_lossy(&bytes),
                &config_with_launch_params(config, query),
            );
            return respond(StatusCode::OK, HTML, None, injected);
        }
    }
    respond(StatusCode::OK, content_type, encoding, bytes)
}

/// Every response carries COOP+COEP (cross-origin isolation → SharedArrayBuffer,
/// matching prod) and no-store (disk edits must never be served stale).
fn respond(
    status: StatusCode,
    content_type: &str,
    encoding: Option<&'static str>,
    body: impl Into<Body>,
) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header("cross-origin-opener-policy", "same-origin")
        .header("cross-origin-embedder-policy", "require-corp");
    if let Some(encoding) = encoding {
        builder = builder.header(header::CONTENT_ENCODING, encoding);
    }
    builder.body(body.into()).unwrap()
}

/// Boot shell: play's real default entrypoint for engine builds (the prod
/// path), the game's own script with null params for `.js` entries. The `<`
/// escape keeps params values from closing the script tag.
fn shell(
    sdk_js: Option<&Path>,
    script_src: &str,
    params_json: Option<&str>,
    config: &str,
) -> String {
    SHELL_TEMPLATE
        .replace("{{INJECT_URL}}", &inject_url(sdk_js))
        .replace(
            "{{ENTRYPOINT_PARAMS}}",
            &params_json.unwrap_or("null").replace('<', "\\u003c"),
        )
        .replace("{{SDK_CONFIG}}", &js_string_literal(config))
        .replace("{{SCRIPT_SRC}}", &html_attr_escape(script_src))
}

/// JSON-string-escape plus `\u003c` for `<` so the value can't close the
/// surrounding script tag.
fn js_string_literal(s: &str) -> String {
    serde_json::Value::String(s.to_string())
        .to_string()
        .replace('<', "\\u003c")
}

/// Inject the config global + SDK bundle + init gate right after
/// `<head ...>`, or prepended if there's no head.
fn inject_sdk(sdk_js: Option<&Path>, html: &str, config: &str) -> String {
    let tags = format!(
        "<script>window.__wavedashSdkConfig = {};</script>\
<script src=\"{}\" crossorigin=\"anonymous\"></script>\
<script src=\"/__wavedash/dev.js\"></script>",
        js_string_literal(config),
        inject_url(sdk_js)
    );
    match head_insert_pos(&html.to_ascii_lowercase()) {
        Some(pos) => format!("{}{}{}", &html[..pos], tags, &html[pos..]),
        None => format!("{tags}{html}"),
    }
}

/// Index right after the first `<head>` / `<head ...>` open tag (not `<header>`).
fn head_insert_pos(lower: &str) -> Option<usize> {
    for (idx, _) in lower.match_indices("<head") {
        match lower.as_bytes().get(idx + 5) {
            Some(b'>') => return Some(idx + 6),
            Some(c) if c.is_ascii_whitespace() => {
                return lower[idx..].find('>').map(|close| idx + close + 1)
            }
            _ => {}
        }
    }
    None
}

fn html_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Rejects any path that escapes `upload_dir`.
fn resolve_path(upload_dir: &Path, url_path: &str) -> Option<PathBuf> {
    let rel = url_path.trim_start_matches('/');
    let decoded = urlencoding::decode(rel)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| rel.to_string());

    let canon_dir = upload_dir.canonicalize().ok()?;
    let canon_file = upload_dir.join(decoded).canonicalize().ok()?;
    if canon_file.starts_with(&canon_dir) && canon_file.is_file() {
        Some(canon_file)
    } else {
        None
    }
}

/// Unity/Godot emit `.gz`/`.br` files directly; strip the suffix to derive the
/// real type and let the browser decompress transparently.
fn content_type_and_encoding(path: &str) -> (&'static str, Option<&'static str>) {
    if let Some(stripped) = path.strip_suffix(".gz") {
        return (lookup_content_type(stripped), Some("gzip"));
    }
    if let Some(stripped) = path.strip_suffix(".br") {
        return (lookup_content_type(stripped), Some("br"));
    }
    (lookup_content_type(path), None)
}

/// mime_guess plus the engine bundle formats it doesn't know. Unity's
/// `.symbols.json` must be octet-stream (the loader fetches it as raw bytes).
fn lookup_content_type(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".symbols.json") {
        return "application/octet-stream";
    }
    match lower.rsplit_once('.').map(|(_, ext)| ext) {
        Some("unityweb" | "data" | "mem" | "bundle" | "pck") => "application/octet-stream",
        Some("unity3d") => "application/vnd.unity",
        _ => mime_guess::from_path(&lower)
            .first_raw()
            .unwrap_or("application/octet-stream"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pinned_sdk_version_file_holds_one_bare_semver() {
        let version = SDK_JS_VERSION.trim();
        let parts: Vec<&str> = version.split('.').collect();
        assert!(
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "src/dev/sdk-js-version must hold one bare version like 1.2.3, got {version:?}"
        );
    }

    #[test]
    fn the_inject_url_never_carries_the_version_files_trailing_newline() {
        let url = inject_url(None);
        assert!(!url.contains(char::is_whitespace), "{url:?}");
        assert!(
            url.contains(&format!("@wvdsh/sdk-js@{}/", SDK_JS_VERSION.trim())),
            "{url:?}"
        );
    }

    fn query_of(query: &str) -> QueryParams {
        let uri: Uri = format!("/?{query}").parse().unwrap();
        Query::<QueryParams>::try_from_uri(&uri).unwrap().0
    }

    fn params_of(query: &str) -> serde_json::Map<String, serde_json::Value> {
        launch_params(&query_of(query))
    }

    const TEST_UUID: &str = "testuuid";
    const TEST_AUTH_URL: &str = "https://wavedash.com/auth/dev?build_uuid=testuuid";

    fn test_config(dir: &Path, entry: &str) -> ServeConfig {
        ServeConfig {
            upload_dir: dir.to_path_buf(),
            entry: entry.to_string(),
            verbose: false,
            build_uuid: TEST_UUID.to_string(),
            sessions_dir: dir.to_path_buf(),
            state_token: "state".to_string(),
            auth_url: TEST_AUTH_URL.to_string(),
            api_key: "key".to_string(),
            api_host: "https://api.example".to_string(),
            client: reqwest::Client::new(),
            engine_entry: None,
            sdk_js: None,
            jwks: tokio::sync::OnceCell::new(),
        }
    }

    fn authed_headers() -> HeaderMap {
        let config = r#"{"gameCloudId":"g1","launchParams":{},"parentOrigin":""}"#;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!(
                "wd_session_{TEST_UUID}=session; wd_sdkconfig_{TEST_UUID}={}",
                urlencoding::encode(config)
            )
            .parse()
            .unwrap(),
        );
        headers
    }

    async fn body_string(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Inlined as a JS string literal holding the config JSON, so it parses twice.
    fn served_sdk_config(html: &str) -> serde_json::Value {
        let marker = "window.__wavedashSdkConfig = ";
        let start = html.find(marker).expect("config global") + marker.len();
        let literal: String = serde_json::Deserializer::from_str(&html[start..])
            .into_iter::<String>()
            .next()
            .expect("js string literal")
            .expect("js string literal");
        serde_json::from_str(&literal).expect("config json")
    }

    #[tokio::test]
    async fn a_js_entry_shell_carries_the_launch_params_too() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Arc::new(test_config(dir.path(), "game.js"));

        let response = handle_index(
            State(cfg),
            authed_headers(),
            Query(query_of("wvdsh_lobby=abc123")),
            "/?wvdsh_lobby=abc123".parse::<Uri>().unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let config = served_sdk_config(&body_string(response).await);
        assert_eq!(
            config["launchParams"],
            serde_json::json!({ "lobby": "abc123" })
        );
    }

    #[tokio::test]
    async fn an_unauthed_visit_without_launch_params_clears_any_stale_stash() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Arc::new(test_config(dir.path(), "index.html"));

        let response = handle_index(
            State(cfg),
            HeaderMap::new(),
            Query(QueryParams::new()),
            "/".parse::<Uri>().unwrap(),
        )
        .await;
        let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(cookie.contains("Max-Age=0"), "{cookie}");
    }

    async fn serve_in_background(dir: &Path, entry: &str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let cfg = test_config(dir, entry);
        let handle = tokio::spawn(async move {
            let _ = run(listener, cfg).await;
        });
        (base, handle)
    }

    #[tokio::test]
    async fn a_lobby_invite_link_opened_against_the_real_server_reaches_the_game() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html><head></head></html>").unwrap();
        let (base, server) = serve_in_background(dir.path(), "index.html").await;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let cookies = authed_headers()[header::COOKIE].to_str().unwrap().to_string();

        let bounce = client
            .get(format!("{base}/?wvdsh_lobby=abc123"))
            .send()
            .await
            .unwrap();
        assert_eq!(bounce.status(), StatusCode::SEE_OTHER);
        assert_eq!(bounce.headers()[header::LOCATION], TEST_AUTH_URL);
        let stashed = bounce.headers()[header::SET_COOKIE].to_str().unwrap();
        assert_eq!(
            stashed.split(';').next().unwrap(),
            format!("wd_pending_{TEST_UUID}=wvdsh_lobby=abc123"),
            "the invite link's lobby must be stashed across the auth bounce"
        );

        let redirect = client
            .get(format!("{base}/?wvdsh_lobby=abc123"))
            .header(header::COOKIE, &cookies)
            .send()
            .await
            .unwrap();
        assert_eq!(
            redirect.headers()[header::LOCATION],
            "/index.html?wvdsh_lobby=abc123"
        );

        let page = client
            .get(format!("{base}/index.html?wvdsh_lobby=abc123"))
            .header(header::COOKIE, &cookies)
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let config = served_sdk_config(&page.text().await.unwrap());
        assert_eq!(
            config["launchParams"],
            serde_json::json!({ "lobby": "abc123" })
        );

        server.abort();
    }

    #[test]
    fn only_wvdsh_prefixed_query_params_reach_the_game_and_lose_the_prefix() {
        let params = params_of("wvdsh_lobby=abc123&utm_source=discord&mode=ranked");
        assert_eq!(params.len(), 1, "{params:?}");
        assert_eq!(params["lobby"], serde_json::json!("abc123"));
    }

    #[test]
    fn a_bare_prefix_is_not_an_empty_named_launch_param() {
        assert!(params_of("wvdsh_=abc").is_empty());
    }

    #[test]
    fn launch_param_values_are_url_decoded_the_way_a_browser_reads_them() {
        let params = params_of("wvdsh_note=hello+world%21&wvdsh_empty");
        assert_eq!(params["note"], serde_json::json!("hello world!"));
        assert_eq!(params["empty"], serde_json::json!(""));
    }

    #[test]
    fn the_served_config_carries_the_requests_launch_params() {
        let config = r#"{"gameCloudId":"g1","launchParams":{}}"#;
        let merged = config_with_launch_params(config, &query_of("wvdsh_lobby=abc123"));
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["launchParams"], serde_json::json!({"lobby": "abc123"}));
        assert_eq!(value["gameCloudId"], serde_json::json!("g1"));
    }

    #[test]
    fn a_request_without_launch_params_clears_the_ones_the_config_arrived_with() {
        let config = r#"{"launchParams":{"lobby":"stale"}}"#;
        let merged = config_with_launch_params(config, &QueryParams::new());
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["launchParams"], serde_json::json!({}));
    }

    #[test]
    fn an_unparseable_config_is_served_through_untouched() {
        let config = "not json";
        assert_eq!(
            config_with_launch_params(config, &query_of("wvdsh_lobby=abc")),
            config
        );
    }

    #[test]
    fn only_launch_params_are_stashed_across_the_auth_bounce() {
        assert_eq!(
            launch_param_query(&query_of("utm=x&wvdsh_lobby=abc123&z=1")).as_deref(),
            Some("wvdsh_lobby=abc123")
        );
        assert_eq!(launch_param_query(&query_of("utm=x")), None);
        assert_eq!(launch_param_query(&QueryParams::new()), None);
            }

    #[test]
    fn the_stashed_query_still_parses_as_launch_params_after_the_round_trip() {
        let stashed =
            launch_param_query(&query_of("wvdsh_note=hello+world%3Bmore%26done&utm=x")).unwrap();
        // The stash rides in a cookie, so no pair may carry a bare ; & or space.
        assert!(!stashed.contains(';') && !stashed.contains(' '), "{stashed}");
        assert_eq!(stashed.matches('&').count(), 0, "{stashed}");

        let params = launch_params(&query_of(&stashed));
        assert_eq!(params["note"], serde_json::json!("hello world;more&done"));
    }

    #[test]
    fn a_local_bundle_is_served_from_this_origin() {
        let url = inject_url(Some(Path::new("/tmp/sdk-js/dist/inject.global.js")));
        assert_eq!(url, LOCAL_SDK_URL);
        assert!(!url.contains("jsdelivr"), "{url:?}");
    }
}
