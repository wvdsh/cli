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

/// Per-browser gameplay session token (HttpOnly — page JS never needs it).
const SESSION_COOKIE: &str = "wd_session";

pub struct ServeConfig {
    pub upload_dir: PathBuf,
    /// Entry filename relative to upload_dir (e.g. `index.html` or `game.js`).
    pub entry: String,
    pub verbose: bool,
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

    let session_token = match upstream {
        Ok(res) if res.status().is_success() => match res.json::<ExchangeResponse>().await {
            Ok(body) => body.session_token,
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

    let location = format!("/?sdkconfig={}", urlencoding::encode(&p.sdkconfig));
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, location)
        .header(
            header::SET_COOKIE,
            format!("{SESSION_COOKIE}={session_token}; Path=/; HttpOnly; SameSite=Lax"),
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
async fn handle_auth_refresh(State(cfg): State<Arc<ServeConfig>>, headers: HeaderMap) -> Response {
    let Some(session_token) = session_cookie(&headers) else {
        return respond(StatusCode::UNAUTHORIZED, TEXT, None, "Auth not ready");
    };

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
            Ok(body) => respond(StatusCode::OK, TEXT, None, body.gameplay_jwt),
            Err(_) => respond(StatusCode::BAD_GATEWAY, TEXT, None, "Bad refresh response"),
        },
        // Clear the dead cookie so the next reload re-runs the handoff.
        Ok(_) => Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::CONTENT_TYPE, TEXT)
            .header(header::CACHE_CONTROL, "no-store")
            .header(
                header::SET_COOKIE,
                format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
            )
            .body(Body::from("Session expired"))
            .unwrap(),
        Err(err) => {
            eprintln!("refresh-gameplay request failed: {err}");
            respond(StatusCode::BAD_GATEWAY, TEXT, None, "Refresh failed")
        }
    }
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .map(str::trim)
        .find_map(|pair| pair.strip_prefix(&format!("{SESSION_COOKIE}=")[..]))
        .map(str::to_string)
}

/// `?sdkconfig=` AND the session cookie get the game; anything else re-runs
/// the handoff — a bare localhost visit works, and a reload heals a cleared
/// cookie that left `?sdkconfig=` behind in the URL.
async fn handle_index(
    State(cfg): State<Arc<ServeConfig>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !has_sdkconfig(&uri) || session_cookie(&headers).is_none() {
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
    // `?sdkconfig=` without the cookie is stale state (cookies cleared) —
    // re-run the handoff instead of serving a game whose auth will 401.
    if has_sdkconfig(&uri) && session_cookie(&headers).is_none() {
        return Redirect::to(&cfg.auth_url).into_response();
    }
    serve_file(&cfg, uri.path(), has_sdkconfig(&uri))
}

fn has_sdkconfig(uri: &Uri) -> bool {
    uri.query().is_some_and(|q| q.contains("sdkconfig="))
}

/// The init gate (see dev.js).
async fn handle_dev_js() -> Response {
    respond(StatusCode::OK, "text/javascript; charset=utf-8", None, DEV_JS)
}

/// HTML navigations carrying `?sdkconfig=` get the SDK tags injected (prod's
/// embed.js mechanism); everything else streams as-is.
fn serve_file(cfg: &ServeConfig, url_path: &str, sdk_nav: bool) -> Response {
    let Some(file_path) = resolve_path(&cfg.upload_dir, url_path) else {
        return respond(StatusCode::NOT_FOUND, TEXT, None, "Not Found");
    };
    let Ok(bytes) = std::fs::read(&file_path) else {
        return respond(StatusCode::NOT_FOUND, TEXT, None, "Not Found");
    };

    let (content_type, encoding) = content_type_and_encoding(url_path);
    if sdk_nav && encoding.is_none() && content_type.starts_with("text/html") {
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
