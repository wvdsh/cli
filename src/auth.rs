use anyhow::Result;
use keyring::Entry;
use std::collections::HashMap;
use std::net::TcpListener;
use tiny_http::{Server, Response};
use url::Url;
use crate::config;

const SERVICE_NAME: &str = "wvdsh";
const API_KEY_ACCOUNT: &str = "api-key";

pub struct AuthManager {
    api_key_entry: Entry,
}

impl AuthManager {
    pub fn new() -> Result<Self> {
        let api_key_entry = Entry::new(SERVICE_NAME, API_KEY_ACCOUNT)?;
        Ok(Self { api_key_entry })
    }

    pub fn store_api_key(&self, api_key: &str) -> Result<()> {
        self.api_key_entry.set_password(api_key)?;
        Ok(())
    }

    pub fn get_api_key(&self) -> Option<String> {
        self.api_key_entry.get_password().ok()
    }

    pub fn clear_credentials(&self) -> Result<()> {
        let _ = self.api_key_entry.delete_credential();
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
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().to_string()
}

pub fn login_with_browser() -> Result<String> {
    let port = find_available_port()?;
    let state = generate_state();
    let redirect_uri = format!("http://localhost:{}", port);
    
    let website_host = config::get("open_browser_website_host")?;
    let auth_url = format!(
        "{}/developers/cli/auth?redirect_uri={}&state={}",
        website_host,
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&state)
    );

    println!("Opening browser for authentication...");
    open::that(&auth_url)?;

    let server = Server::http(format!("127.0.0.1:{}", port))
        .map_err(|e| anyhow::anyhow!("Failed to start server: {}", e))?;
    println!("Waiting for authorization...");

    for request in server.incoming_requests() {
        let url_str = format!("http://localhost{}", request.url());
        let parsed_url = Url::parse(&url_str)?;
        
        let params: HashMap<String, String> = parsed_url
            .query_pairs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let _ = request.respond(Response::from_string("Success! You can close this window."));

        if let (Some(received_state), Some(api_key)) = (params.get("state"), params.get("api_key")) {
            if received_state == &state {
                return Ok(api_key.clone());
            }
        }
        
        if params.get("error").is_some() {
            anyhow::bail!("Authentication failed");
        }
    }

    anyhow::bail!("Server stopped unexpectedly");
}