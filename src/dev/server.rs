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
use tokio::net::TcpListener;

/// Always `@latest`. Classic parser-blocking IIFE (auto-runs setupWavedashSDK):
/// the only way `window.Wavedash` exists before game scripts parse — module
/// scripts are always deferred. jsdelivr sends ACAO * + CORP, satisfying COEP.
const INJECT_URL: &str = "https://cdn.jsdelivr.net/npm/@wvdsh/sdk-js@latest/dist/inject.global.js";

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
    /// Cookies are host-scoped, so names carry the port or build uuid to keep
    /// concurrent and successive dev servers from clobbering each other.
    pub port: u16,
    pub build_uuid: String,
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

    /// SDKConfig handed to the page; readable by JS — `setupWavedashSDK()`
    /// falls back to it when the URL has no `?sdkconfig=`, keeping URLs clean.
    /// Port-scoped because the page can only derive the port from location;
    /// a stale port collision fails the build-scoped session check above and
    /// re-runs the handoff.
    fn sdkconfig_cookie_name(&self) -> String {
        format!("wd_sdkconfig_{}", self.port)
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

    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, "/")
        .header(
            header::SET_COOKIE,
            format!(
                "{}={}; Path=/; HttpOnly; SameSite=Lax",
                cfg.session_cookie_name(),
                exchange.session_token
            ),
        )
        .header(
            header::SET_COOKIE,
            format!(
                "{}={}; Path=/; HttpOnly; SameSite=Lax",
                cfg.jwt_cookie_name(),
                exchange.gameplay_jwt
            ),
        )
        // JS-readable: setupWavedashSDK boots from it, keeping the URL clean.
        .header(
            header::SET_COOKIE,
            format!(
                "{}={}; Path=/; SameSite=Lax",
                cfg.sdkconfig_cookie_name(),
                urlencoding::encode(&p.sdkconfig)
            ),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .unwrap()
}

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(rename = "gameplayJwt")]
    gameplay_jwt: String,
}

/// Exchange the browser's session cookie for a fresh gameplay JWT, so the
/// SDK's refreshes renew for the session's day, not the first JWT's hour.
/// The last JWT rides in a cookie and is served back while still fresh, so
/// most refreshes never leave localhost.
async fn handle_auth_refresh(State(cfg): State<Arc<ServeConfig>>, headers: HeaderMap) -> Response {
    let Some(session_token) = cookie_value(&headers, &cfg.session_cookie_name()) else {
        return respond(StatusCode::UNAUTHORIZED, TEXT, None, "Auth not ready");
    };

    if let Some(jwt) = cookie_value(&headers, &cfg.jwt_cookie_name()) {
        if jwt_fresh(&cfg, &jwt).await {
            return respond(StatusCode::OK, TEXT, None, jwt);
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
                    format!(
                        "{}={}; Path=/; HttpOnly; SameSite=Lax",
                        cfg.jwt_cookie_name(),
                        body.gameplay_jwt
                    ),
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
                format!(
                    "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
                    cfg.session_cookie_name()
                ),
            )
            .header(
                header::SET_COOKIE,
                format!(
                    "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
                    cfg.jwt_cookie_name()
                ),
            )
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

fn has_credentials(cfg: &ServeConfig, headers: &HeaderMap) -> bool {
    cookie_value(headers, &cfg.session_cookie_name()).is_some()
        && cookie_value(headers, &cfg.sdkconfig_cookie_name()).is_some()
}

/// Browsers with this server's cookies get the game; anything else re-runs
/// the handoff — a bare localhost visit works, and a reload heals cleared
/// cookies or an expired session.
async fn handle_index(
    State(cfg): State<Arc<ServeConfig>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !has_credentials(&cfg, &headers) {
        return Redirect::to(&cfg.auth_url).into_response();
    }

    if let Some(engine_entry) = &cfg.engine_entry {
        let html = shell(&engine_entry.entrypoint_url, Some(&engine_entry.params_json));
        return respond(StatusCode::OK, HTML, None, html);
    }
    if cfg.entry.ends_with(".js") {
        let html = shell(&format!("/{}", cfg.entry), None);
        return respond(StatusCode::OK, HTML, None, html);
    }
    // The HTML entry serves at its real path so relative assets resolve.
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    Redirect::to(&format!("/{}{}", cfg.entry, query)).into_response()
}

async fn handle_static(
    State(cfg): State<Arc<ServeConfig>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let authed = has_credentials(&cfg, &headers);
    // An HTML nav without this browser's cookies is stale state (cookies
    // cleared) — re-run the handoff instead of serving a game that will 401.
    if !authed && is_html_path(uri.path()) {
        return Redirect::to(&cfg.auth_url).into_response();
    }
    serve_file(&cfg, uri.path(), authed)
}

fn is_html_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

/// The init gate (see dev.js).
async fn handle_dev_js() -> Response {
    respond(StatusCode::OK, "text/javascript; charset=utf-8", None, DEV_JS)
}

/// Authed HTML navigations get the SDK tags injected (prod's embed.js
/// mechanism); everything else streams as-is.
fn serve_file(cfg: &ServeConfig, url_path: &str, authed: bool) -> Response {
    let Some(file_path) = resolve_path(&cfg.upload_dir, url_path) else {
        return respond(StatusCode::NOT_FOUND, TEXT, None, "Not Found");
    };
    let Ok(bytes) = std::fs::read(&file_path) else {
        return respond(StatusCode::NOT_FOUND, TEXT, None, "Not Found");
    };

    let (content_type, encoding) = content_type_and_encoding(url_path);
    if authed && encoding.is_none() && content_type.starts_with("text/html") {
        let injected = inject_sdk(&String::from_utf8_lossy(&bytes));
        return respond(StatusCode::OK, HTML, None, injected);
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
fn shell(script_src: &str, params_json: Option<&str>) -> String {
    SHELL_TEMPLATE
        .replace("{{INJECT_URL}}", INJECT_URL)
        .replace(
            "{{ENTRYPOINT_PARAMS}}",
            &params_json.unwrap_or("null").replace('<', "\\u003c"),
        )
        .replace("{{SCRIPT_SRC}}", &html_attr_escape(script_src))
}

/// Inject the SDK bundle + init gate right after `<head ...>`, or prepended
/// if there's no head.
fn inject_sdk(html: &str) -> String {
    let tags = format!(
        "<script src=\"{INJECT_URL}\" crossorigin=\"anonymous\"></script>\
<script src=\"/__wavedash/dev.js\"></script>"
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
