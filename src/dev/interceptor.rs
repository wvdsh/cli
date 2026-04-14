use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine as Base64Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EventRequestPaused, FulfillRequestParams, HeaderEntry,
    RequestPattern,
};
use chromiumoxide::Page;
use futures::StreamExt;
use mime_guess::from_path;

const NOOP_SW_JS: &str = r#"
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
self.addEventListener('message', (event) => {
    if (event.data && event.data.expectAckType && event.ports[0]) {
        event.ports[0].postMessage({ type: event.data.expectAckType });
    }
});
"#;

/// Paths on the game subdomain that should be passed through to the real play server.
const PASSTHROUGH_PREFIXES: &[&str] = &[
    "/default-entrypoints/",
    "/auth/",
    "/gameplay/",
];

fn is_passthrough_path(path: &str) -> bool {
    if path == "/local" {
        return true;
    }
    if path.starts_with("/local.js") {
        return true;
    }
    PASSTHROUGH_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

pub async fn setup_interception(
    page: &Page,
    upload_dir: &Path,
    local_origin: &str,
    game_subdomain: &str,
) -> Result<()> {
    let pattern = RequestPattern::builder()
        .url_pattern("*".to_string())
        .build();

    let enable = chromiumoxide::cdp::browser_protocol::fetch::EnableParams::builder()
        .patterns(vec![pattern])
        .build();

    page.execute(enable)
        .await
        .with_context(|| "Failed to enable CDP Fetch interception")?;

    let mut events = page
        .event_listener::<EventRequestPaused>()
        .await
        .with_context(|| "Failed to listen for CDP Fetch events")?;

    let upload_dir = Arc::new(upload_dir.to_path_buf());
    let local_origin = Arc::new(local_origin.to_string());
    let game_subdomain = Arc::new(game_subdomain.to_string());
    let page = page.clone();

    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            let upload_dir = upload_dir.clone();
            let local_origin = local_origin.clone();
            let game_subdomain = game_subdomain.clone();
            let page = page.clone();

            tokio::spawn(async move {
                if let Err(e) =
                    handle_request(&page, &event, &upload_dir, &local_origin, &game_subdomain)
                        .await
                {
                    eprintln!("[cdp] Error handling request: {e}");
                    // Try to continue the request so the browser doesn't hang
                    if let Ok(params) = ContinueRequestParams::builder()
                        .request_id(event.request_id.clone())
                        .build()
                    {
                        let _ = page.execute(params).await;
                    }
                }
            });
        }
    });

    Ok(())
}

async fn handle_request(
    page: &Page,
    event: &EventRequestPaused,
    upload_dir: &Path,
    local_origin: &str,
    game_subdomain: &str,
) -> Result<()> {
    let url = &event.request.url;
    let request_id = event.request_id.clone();

    // 1. LNA test: any request to the virtual local origin -> 200 OK
    if url.starts_with(local_origin) {
        return fulfill_empty_ok(page, request_id).await;
    }

    // Parse host from URL
    let parsed = url::Url::parse(url).ok();
    let host = parsed.as_ref().and_then(|u| u.host_str()).unwrap_or("");
    let path = parsed.as_ref().map(|u| u.path()).unwrap_or("/");

    // 2. Requests to the game subdomain
    if host == game_subdomain {
        // 2a. /sw-local.js -> serve no-op service worker
        if path.starts_with("/sw-local.js") {
            return fulfill_js(page, request_id, NOOP_SW_JS).await;
        }

        // 2b. Passthrough paths -> let real play server handle
        if is_passthrough_path(path) {
            return continue_request(page, request_id).await;
        }

        // 2c. Everything else on game subdomain -> serve from upload_dir
        return serve_local_file(page, request_id, upload_dir, path).await;
    }

    // 3. Everything else -> continue to real network
    continue_request(page, request_id).await
}

async fn continue_request(
    page: &Page,
    request_id: chromiumoxide::cdp::browser_protocol::fetch::RequestId,
) -> Result<()> {
    let params = ContinueRequestParams::builder()
        .request_id(request_id)
        .build()
        .map_err(|e| anyhow::anyhow!("ContinueRequest build error: {e}"))?;
    page.execute(params).await?;
    Ok(())
}

async fn fulfill_empty_ok(
    page: &Page,
    request_id: chromiumoxide::cdp::browser_protocol::fetch::RequestId,
) -> Result<()> {
    let params = FulfillRequestParams::builder()
        .request_id(request_id)
        .response_code(200)
        .response_headers(vec![
            header("Access-Control-Allow-Origin", "*"),
            header("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS"),
            header("Access-Control-Allow-Headers", "*"),
            header("Access-Control-Allow-Private-Network", "true"),
            header("Content-Length", "0"),
        ])
        .body(String::new())
        .build()
        .map_err(|e| anyhow::anyhow!("FulfillRequest build error: {e}"))?;
    page.execute(params).await?;
    Ok(())
}

async fn fulfill_js(
    page: &Page,
    request_id: chromiumoxide::cdp::browser_protocol::fetch::RequestId,
    js: &str,
) -> Result<()> {
    let body = BASE64.encode(js.as_bytes());
    let params = FulfillRequestParams::builder()
        .request_id(request_id)
        .response_code(200)
        .response_headers(vec![
            header("Content-Type", "application/javascript"),
            header("Access-Control-Allow-Origin", "*"),
            header("Cross-Origin-Resource-Policy", "cross-origin"),
        ])
        .body(body)
        .build()
        .map_err(|e| anyhow::anyhow!("FulfillRequest build error: {e}"))?;
    page.execute(params).await?;
    Ok(())
}

async fn serve_local_file(
    page: &Page,
    request_id: chromiumoxide::cdp::browser_protocol::fetch::RequestId,
    upload_dir: &Path,
    url_path: &str,
) -> Result<()> {
    let relative = url_path.trim_start_matches('/');
    let relative = urlencoding::decode(relative)
        .unwrap_or_else(|_| relative.into());
    let file_path = upload_dir.join(relative.as_ref());

    // Security: ensure the resolved path is inside upload_dir
    let canonical_dir = upload_dir.canonicalize().unwrap_or_else(|_| upload_dir.to_path_buf());
    let canonical_file = file_path.canonicalize().unwrap_or_else(|_| file_path.clone());
    if !canonical_file.starts_with(&canonical_dir) {
        return fulfill_not_found(page, request_id).await;
    }

    if !file_path.is_file() {
        return fulfill_not_found(page, request_id).await;
    }

    let bytes = std::fs::read(&file_path)
        .with_context(|| format!("Failed to read {}", file_path.display()))?;

    let body = BASE64.encode(&bytes);

    let mut headers = vec![
        header("Access-Control-Allow-Origin", "*"),
        header("Cross-Origin-Embedder-Policy", "require-corp"),
        header("Cross-Origin-Opener-Policy", "same-origin"),
        header("Cross-Origin-Resource-Policy", "cross-origin"),
    ];

    let (content_type, content_encoding) = resolve_content_type(url_path);
    headers.push(header("Content-Type", &content_type));
    if let Some(encoding) = content_encoding {
        headers.push(header("Content-Encoding", &encoding));
    }

    let params = FulfillRequestParams::builder()
        .request_id(request_id)
        .response_code(200)
        .response_headers(headers)
        .body(body)
        .build()
        .map_err(|e| anyhow::anyhow!("FulfillRequest build error: {e}"))?;
    page.execute(params).await?;
    Ok(())
}

async fn fulfill_not_found(
    page: &Page,
    request_id: chromiumoxide::cdp::browser_protocol::fetch::RequestId,
) -> Result<()> {
    let params = FulfillRequestParams::builder()
        .request_id(request_id)
        .response_code(404)
        .response_headers(vec![header("Content-Type", "text/plain")])
        .body(BASE64.encode(b"Not Found"))
        .build()
        .map_err(|e| anyhow::anyhow!("FulfillRequest build error: {e}"))?;
    page.execute(params).await?;
    Ok(())
}

fn header(name: &str, value: &str) -> HeaderEntry {
    HeaderEntry::new(name.to_string(), value.to_string())
}

fn resolve_content_type(path: &str) -> (String, Option<String>) {
    let compression_map = [(".gz", "gzip"), (".br", "br")];

    for (suffix, encoding) in compression_map {
        if path.ends_with(suffix) {
            let stripped = &path[..path.len() - suffix.len()];
            let mime = from_path(stripped)
                .first()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            return (mime, Some(encoding.to_string()));
        }
    }

    let mime = from_path(path)
        .first()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    (mime, None)
}
