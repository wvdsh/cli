use crate::config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::TcpListener;
use tiny_http::{Header, Response, Server};

#[derive(Serialize, Deserialize)]
struct Credentials {
    api_key: String,
}

pub struct AuthManager;

impl AuthManager {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    pub fn store_api_key(&self, api_key: &str) -> Result<()> {
        let path = config::credentials_path()?;

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }

        let credentials = Credentials {
            api_key: api_key.to_string(),
        };
        let json = serde_json::to_string(&credentials)?;
        fs::write(&path, &json)?;

        // Set restrictive permissions on the credentials file
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    pub fn get_api_key(&self) -> Option<String> {
        let path = config::credentials_path().ok()?;
        let json = fs::read_to_string(&path).ok()?;
        let credentials: Credentials = serde_json::from_str(&json).ok()?;
        Some(credentials.api_key)
    }

    pub fn clear_credentials(&self) -> Result<()> {
        let path = config::credentials_path()?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn is_authenticated(&self) -> bool {
        self.get_api_key().is_some()
    }
}

fn find_available_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn generate_state() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
}

pub fn login_with_browser() -> Result<String> {
    let port = find_available_port()?;
    let state = generate_state();
    let redirect_uri = format!("http://localhost:{}", port);

    let website_host = config::get("open_browser_website_host")?;
    let auth_url = format!(
        "{}/developers/cli/auth?callback_uri={}&state={}",
        website_host,
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&state)
    );

    println!("Opening browser for authentication...");
    println!("Auth URL: {}", auth_url);
    open::that(&auth_url)?;

    let server = Server::http(format!("127.0.0.1:{}", port))
        .map_err(|e| anyhow::anyhow!("Failed to start server: {}", e))?;
    println!("Waiting for authorization...");

    for request in server.incoming_requests() {
        let url = request.url().to_string();

        // Parse query parameters from the URL
        if url.contains('?') {
            let query_string = url.split('?').nth(1).unwrap_or("");
            let params: std::collections::HashMap<String, String> = query_string
                .split('&')
                .filter_map(|pair| {
                    let mut parts = pair.split('=');
                    let key = parts.next()?.to_string();
                    let value = parts.next()?.to_string();
                    Some((key, value))
                })
                .collect();

            // Check for api_key - assume positive response
            if let (Some(api_key), Some(return_url)) =
                (params.get("api_key"), params.get("return_url"))
            {
                // Decode the return_url since it comes URL-encoded
                let decoded_return_url = urlencoding::decode(return_url)
                    .unwrap_or(std::borrow::Cow::Borrowed(return_url));

                // Add success param to return URL
                let redirect_url = if decoded_return_url.contains('?') {
                    format!("{}&success=true", decoded_return_url)
                } else {
                    format!("{}?success=true", decoded_return_url)
                };
                let location_header =
                    Header::from_bytes(&b"Location"[..], redirect_url.as_bytes()).unwrap();
                let response = Response::from_string("")
                    .with_status_code(302)
                    .with_header(location_header);
                let _ = request.respond(response);
                return Ok(api_key.to_string());
            }
        }
    }

    anyhow::bail!("Server stopped unexpectedly");
}
