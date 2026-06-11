//! Local static file server for `wavedash dev`.
//!
//! Serves the developer's build straight off disk, top-level at
//! `http://localhost:<port>` — no Electron, no certs, no subdomains. Entry rule
//! mirrors the play worker (`play/src/server/handlers/embed.tsx`):
//!   - `.js` entrypoint  → synthesize an empty shell that loads it,
//!   - HTML entrypoint   → serve it with the SDK bundle + dev script injected
//!     as parser-blocking tags (same mechanism as prod's embed.js injection).
//!
//! Auth is per-browser so multiple browsers each play as their own user
//! (signed-in accounts, or anonymous users for e.g. incognito windows): `/`
//! without credentials bounces to the mainsite /auth/dev, which mints a 1-day
//! gameplay SESSION for that browser and redirects back to
//! /__wavedash/callback. The callback stores the session token in a localhost
//! cookie and lands on the game with that user's `?sdkconfig=`.
//! `POST /auth/refresh` exchanges the requesting browser's session token for
//! a fresh gameplay JWT via the backend's /api/dev/refresh-gameplay (API-key
//! authed), so play renews silently for the session's lifetime. The server
//! itself holds no auth state.

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

/// Always `@latest`. A classic, parser-blocking IIFE bundle (deps inlined,
/// auto-runs setupWavedashSDK) — the only way `window.Wavedash` can exist
/// before a game's inline scripts parse; module scripts are always deferred.
/// jsdelivr serves ACAO * + CORP cross-origin, satisfying our COEP.
const INJECT_URL: &str = "https://cdn.jsdelivr.net/npm/@wvdsh/sdk-js@latest/dist/inject.global.js";

/// Templates embedded at compile time. `{{NAME}}` placeholders are
/// substituted with values — all logic lives in the templates.
const SYNTH_SHELL_TEMPLATE: &str = include_str!("synth_shell.html");
const ENGINE_SHELL_TEMPLATE: &str = include_str!("engine_shell.html");
const DEV_JS: &str = include_str!("dev.js");

const TEXT: &str = "text/plain; charset=utf-8";
const HTML: &str = "text/html; charset=utf-8";

/// Per-browser gameplay session token, set by the auth callback. HttpOnly:
/// only the server reads it (to answer /auth/refresh); page JS never needs it.
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
    /// Set for engine builds (Unity/Godot/JsDos/Ruffle): boot via play's real
    /// default entrypoint instead of an HTML entry — the prod path.
    pub engine_entry: Option<EngineEntry>,
}

pub struct EngineEntry {
    /// Play's default-entrypoint script for this engine/version.
    pub entrypoint_url: String,
    /// `window.entrypointParams` JSON the entrypoint boots from — computed by
    /// the same backend parsers prod runs at upload-complete.
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
    session_token: String,
    sdkconfig: String,
    state: String,
}

/// Store this browser's gameplay session token in a cookie, then send it into
/// the game with its own `?sdkconfig=` (read by `setupWavedashSDK()`).
async fn handle_callback(
    State(cfg): State<Arc<ServeConfig>>,
    Query(p): Query<CallbackParams>,
) -> Response {
    if p.state != cfg.state_token {
        return respond(StatusCode::BAD_REQUEST, TEXT, None, "Unexpected state");
    }
    let location = format!("/?sdkconfig={}", urlencoding::encode(&p.sdkconfig));
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, location)
        .header(
            header::SET_COOKIE,
            format!(
                "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Lax",
                p.session_token
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

/// The SDK's `getAuthToken()` POSTs here (same-origin, cookies included). We
/// exchange the requesting browser's session token for a FRESH gameplay JWT,
/// so the SDK's expiry-triggered refreshes actually renew for the session's
/// lifetime (1 day) instead of dying with the first JWT's hour.
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
        // Expired/revoked session. Clear the dead cookie so the next page
        // load re-runs the handoff instead of serving a game that 401s again.
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

/// `/` routes into the game. A browser arriving with `?sdkconfig=` (set by the
/// callback) AND its session cookie gets the game; anything else gets bounced
/// through the mainsite auth, which zero-clicks back here — so a bare
/// `localhost:<port>` visit always works, and a reload self-heals a browser
/// whose cookie was cleared even though `?sdkconfig=` survived in the URL.
async fn handle_index(
    State(cfg): State<Arc<ServeConfig>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !has_sdkconfig(&uri) || session_cookie(&headers).is_none() {
        return Redirect::to(&cfg.auth_url).into_response();
    }

    if let Some(engine_entry) = &cfg.engine_entry {
        return respond(StatusCode::OK, HTML, None, engine_shell(engine_entry));
    }
    if cfg.entry.ends_with(".js") {
        return respond(StatusCode::OK, HTML, None, synth_shell(&cfg.entry));
    }
    // Serve the HTML entry at its real path (so relative asset URLs resolve),
    // carrying this browser's query along.
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    Redirect::to(&format!("/{}{}", cfg.entry, query)).into_response()
}

async fn handle_static(
    State(cfg): State<Arc<ServeConfig>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    // A game navigation (`?sdkconfig=`) without this browser's session cookie
    // is stale state — e.g. cookies cleared while the URL kept its query.
    // Re-run the handoff instead of serving a game whose auth will 401.
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

/// Resolve and serve a file from upload_dir. HTML navigations that carry
/// `?sdkconfig=` get the SDK bundle + dev script injected as parser-blocking
/// tags (mirroring prod's embed.js injection); everything else streams as-is.
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

/// Empty shell for `.js` entrypoints (synth_shell.html) — same shape as
/// play's default embed shell.
fn synth_shell(entry: &str) -> String {
    SYNTH_SHELL_TEMPLATE
        .replace("{{INJECT_URL}}", INJECT_URL)
        .replace("{{ENTRY_SRC}}", &html_attr_escape(&format!("/{entry}")))
}

/// Shell for engine builds (engine_shell.html): boots the engine through
/// play's real default entrypoint, exactly like prod — no recreated engine
/// logic. The `<` escape keeps any params value from closing the script tag.
fn engine_shell(entry: &EngineEntry) -> String {
    ENGINE_SHELL_TEMPLATE
        .replace("{{INJECT_URL}}", INJECT_URL)
        .replace(
            "{{ENTRYPOINT_PARAMS}}",
            &entry.params_json.replace('<', "\\u003c"),
        )
        .replace(
            "{{ENTRYPOINT_URL}}",
            &html_attr_escape(&entry.entrypoint_url),
        )
}

/// The pair of parser-blocking tags injected into game HTML: the SDK bundle
/// (auto-runs setupWavedashSDK), then the engine seed + init gate. Inserted
/// right after the opening `<head ...>` tag, or prepended if there's no head.
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

/// Resolve a request path to a real file inside `upload_dir`, rejecting any
/// path that escapes it (canonicalize + containment check).
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
