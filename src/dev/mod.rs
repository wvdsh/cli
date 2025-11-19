use std::env;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::{http::Method, middleware, routing::get_service, Router};
use axum_server::{self, Handle};
use mime_guess::from_path;
use tokio::signal;
use tower::ServiceBuilder;
use tower_http::{
    cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer},
    services::ServeDir,
};
use url::Url;

use crate::auth::AuthManager;
use crate::config::{EngineKind, WavedashConfig};
use crate::file_staging::FileStaging;

mod cert;
mod entrypoint;
mod sandbox;
mod url_params;

use cert::{ensure_cert_trusted, load_or_create_certificates};
use entrypoint::{fetch_entrypoint_params, locate_html_entrypoint};
use sandbox::build_sandbox_url;

const DEFAULT_CONFIG: &str = "./wavedash.toml";

pub async fn handle_dev(config_path: Option<PathBuf>, verbose: bool) -> Result<()> {
    // Check authentication
    let auth_manager = AuthManager::new()?;
    let _api_key = auth_manager
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wvdsh auth login' first."))?;

    let config_path = config_path.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
    if !config_path.exists() {
        anyhow::bail!(
            "Unable to find config file at {}. Pass --config if it's located elsewhere.",
            config_path.display()
        );
    }

    let wavedash_config = WavedashConfig::load(&config_path)?;
    let config_dir = config_parent_dir(&config_path)?;
    let upload_dir = config_dir.join(&wavedash_config.upload_dir);

    if !upload_dir.exists() || !upload_dir.is_dir() {
        anyhow::bail!(
            "Upload directory does not exist or is not a directory: {}",
            upload_dir.display()
        );
    }

    let engine_kind = wavedash_config.engine_type()?;
    let engine_label = engine_kind.as_label();

    let entrypoint = if matches!(engine_kind, EngineKind::Custom) {
        Some(
            wavedash_config
                .entrypoint()
                .ok_or_else(|| anyhow::anyhow!("Custom engine configs require an entrypoint"))?
                .to_string(),
        )
    } else {
        None
    };

    // Copy necessary files to upload directory
    let staging = FileStaging::prepare(&config_path, &config_dir, &upload_dir, &wavedash_config)?;

    let html_entrypoint = locate_html_entrypoint(&upload_dir);
    let entrypoint_params = match engine_kind {
        EngineKind::Godot | EngineKind::Unity => {
            let html_path = html_entrypoint.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "No HTML file found in upload_dir; required for {} builds",
                    engine_label
                )
            })?;
            Some(fetch_entrypoint_params(engine_label, html_path).await?)
        }
        EngineKind::Custom => None,
    };

    let (rustls_config, cert_path, key_path) = load_or_create_certificates().await?;
    ensure_cert_trusted(&cert_path)?;

    let cors_layer = build_cors_layer()?;

    let serve_dir = ServeDir::new(upload_dir.clone()).append_index_html_on_directories(true);

    let app = Router::new()
        .fallback_service(get_service(serve_dir))
        .layer(
            ServiceBuilder::new()
                .layer(cors_layer)
                .layer(middleware::from_fn(log_and_add_corp_headers)),
        );

    let (listener, socket_addr) = bind_ephemeral_listener()?;
    let local_origin = format!("https://localhost:{}", socket_addr.port());

    let sandbox_url = build_sandbox_url(
        &wavedash_config,
        engine_label,
        wavedash_config.version()?,
        &local_origin,
        entrypoint.as_deref(),
        entrypoint_params.as_ref(),
    )?;

    println!("📦 Serving assets from {}", upload_dir.display());
    println!("🔐 TLS certificate: {}", cert_path.display());
    println!("   Key: {}", key_path.display());
    println!("🌐 Local HTTPS origin: {}", local_origin);
    println!("▶️ Sandbox link:\n{}", sandbox_url);
    println!("Press Ctrl+C to stop the server.");

    let handle = Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        shutdown_handle.shutdown();
    });

    let server = axum_server::from_tcp_rustls(listener, rustls_config)
        .handle(handle)
        .serve(app.into_make_service());

    if verbose {
        println!("Verbose logging enabled. Awaiting requests...");
    }

    server.await?;

    // Clean up any temporary files
    staging.cleanup();

    Ok(())
}

fn config_parent_dir(config_path: &PathBuf) -> Result<PathBuf> {
    if let Some(parent) = config_path.parent() {
        if parent.as_os_str().is_empty() {
            return Ok(env::current_dir()?);
        }
        return Ok(parent.to_path_buf());
    }
    Ok(env::current_dir()?)
}

fn build_cors_layer() -> Result<CorsLayer> {
    let allowed_domains = vec!["wavedash.gg".to_string(), "wavedash.lvh.me".to_string()];
    Ok(CorsLayer::new()
        .allow_credentials(true)
        .allow_methods(AllowMethods::list(vec![Method::GET, Method::OPTIONS]))
        .allow_headers(AllowHeaders::mirror_request())
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            origin
                .to_str()
                .ok()
                .and_then(|origin_str| Url::parse(origin_str).ok())
                .and_then(|url| url.host_str().map(|host| host.to_string()))
                .map(|host| host_allowed(&host, &allowed_domains))
                .unwrap_or(false)
        })))
}

fn host_allowed(host: &str, allowed_domains: &[String]) -> bool {
    allowed_domains
        .iter()
        .any(|domain| host_matches_domain(host, domain))
}

fn host_matches_domain(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }

    host.strip_suffix(domain)
        .map_or(false, |prefix| prefix.ends_with('.'))
}

async fn log_and_add_corp_headers(
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path();

    let mut response = next.run(req).await;

    println!("{} {} {}", method, uri, response.status());

    response.headers_mut().insert(
        "Cross-Origin-Resource-Policy",
        "cross-origin".parse().unwrap(),
    );

    // Handle compressed files: add Content-Encoding and fix Content-Type
    let compression_map = [(".gz", "gzip"), (".br", "br")];

    for (suffix, encoding) in compression_map {
        if path.ends_with(suffix) {
            // Add Content-Encoding header
            response.headers_mut().insert(
                "Content-Encoding",
                encoding.parse().unwrap(),
            );

            // Fix Content-Type based on the actual file type (without compression extension)
            if let Some(stripped_path) = path.strip_suffix(suffix) {
                // Use mime_guess on the path without compression extension
                // Fallback to octet-stream if mime type cannot be determined
                let mime_type = from_path(stripped_path)
                    .first()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                response.headers_mut().insert(
                    "Content-Type",
                    mime_type.parse().unwrap(),
                );
            }
            break;
        }
    }

    response
}

fn bind_ephemeral_listener() -> Result<(StdTcpListener, SocketAddr)> {
    let listener = StdTcpListener::bind(("127.0.0.1", 0))
        .context("Unable to bind to an ephemeral localhost port")?;
    let addr = listener
        .local_addr()
        .context("Failed to read bound socket address")?;
    listener
        .set_nonblocking(true)
        .context("Failed to mark listener as non-blocking")?;
    Ok((listener, addr))
}

async fn shutdown_signal() {
    if signal::ctrl_c().await.is_ok() {
        println!("\nReceived Ctrl+C, shutting down dev server...");
    }
}
