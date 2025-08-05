use anyhow::Result;
use keyring::Entry;
use std::io::Read;
use std::net::TcpListener;
use tiny_http::{Server, Response, Method, Header};
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
        "{}/developers/cli/auth?callback_uri={}&state={}",
        website_host,
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&state)
    );

    println!("Opening browser for authentication...");
    open::that(&auth_url)?;

    let server = Server::http(format!("127.0.0.1:{}", port))
        .map_err(|e| anyhow::anyhow!("Failed to start server: {}", e))?;
    println!("Waiting for authorization...");

    for mut request in server.incoming_requests() {
        let cors_header = Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..])
            .unwrap();
        
        match request.method() {
            &Method::Options => {
                // Handle CORS preflight
                let response = Response::from_string("")
                    .with_header(cors_header.clone())
                    .with_header(Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"POST, OPTIONS"[..]).unwrap())
                    .with_header(Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"Content-Type"[..]).unwrap());
                let _ = request.respond(response);
            }
            &Method::Post => {
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body)?;
                
                let data: serde_json::Value = serde_json::from_str(&body)?;
                
                let response = Response::from_string("OK").with_header(cors_header);
                let _ = request.respond(response);
                
                if let (Some(received_state), Some(api_key)) = (
                    data["state"].as_str(),
                    data["api_key"].as_str()
                ) {
                    if received_state == state {
                        return Ok(api_key.to_string());
                    } else {
                        anyhow::bail!("State mismatch");
                    }
                }
                
                if data["error"].is_string() {
                    anyhow::bail!("Authentication failed: {}", data["error"].as_str().unwrap_or("Unknown error"));
                }
            }
            _ => {
                // Handle GET requests
                let response = Response::from_string("Waiting for authorization...").with_header(cors_header);
                let _ = request.respond(response);
            }
        }
    }

    anyhow::bail!("Server stopped unexpectedly");
}