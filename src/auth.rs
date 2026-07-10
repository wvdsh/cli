use crate::config;
use anyhow::{bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use tokio::{
    signal,
    sync::mpsc,
    time::{sleep, Duration},
};

const AUTH_CODE_PROMPT_DELAY: Duration = Duration::from_secs(3);
const AUTH_CODE_REDEEM_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
struct Credentials {
    api_key: String,
    #[serde(default)]
    email: Option<String>,
}

pub enum AuthSource {
    Environment,
    File,
    None,
}

pub struct AuthInfo {
    pub source: AuthSource,
    pub api_key: Option<String>,
    pub email: Option<String>,
}

pub struct LoginResult {
    pub api_key: String,
    pub email: Option<String>,
}

#[derive(Serialize)]
struct RedeemAuthCodeRequest {
    code: String,
    state: String,
    #[serde(rename = "codeVerifier")]
    code_verifier: String,
}

#[derive(Deserialize)]
struct RedeemAuthCodeResponse {
    #[serde(rename = "apiKey")]
    api_key: String,
    email: Option<String>,
}

#[derive(Clone, Copy)]
enum AuthCodeSource {
    Callback,
    Prompt,
}

enum AuthInput {
    Code {
        code: String,
        source: AuthCodeSource,
        callback_stream: Option<TcpStream>,
    },
    Error(String),
}

pub struct AuthManager;

impl AuthManager {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    pub fn store_credentials(&self, api_key: &str, email: Option<&str>) -> Result<()> {
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
            email: email.map(|s| s.to_string()),
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

    fn read_file_credentials(&self) -> Option<Credentials> {
        let path = config::credentials_path().ok()?;
        let json = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&json).ok()
    }

    pub fn get_auth_info(&self) -> AuthInfo {
        // Check environment first
        if let Ok(api_key) = std::env::var("WAVEDASH_TOKEN") {
            if !api_key.is_empty() {
                return AuthInfo {
                    source: AuthSource::Environment,
                    api_key: Some(api_key),
                    email: None, // No email available from env var
                };
            }
        }

        // Check file
        if let Some(creds) = self.read_file_credentials() {
            return AuthInfo {
                source: AuthSource::File,
                api_key: Some(creds.api_key),
                email: creds.email,
            };
        }

        AuthInfo {
            source: AuthSource::None,
            api_key: None,
            email: None,
        }
    }

    pub fn get_api_key(&self) -> Option<String> {
        self.get_auth_info().api_key
    }

    pub fn clear_credentials(&self) -> Result<()> {
        let path = config::credentials_path()?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}

pub(crate) fn generate_state() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
}

fn generate_random_token() -> Result<String> {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Failed to generate random auth token"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn pkce_challenge_from_verifier(verifier: &str) -> String {
    let hash = digest::digest(&digest::SHA256, verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash.as_ref())
}

fn print_opening_browser() {
    eprintln!("Opening browser to sign in…");
}

fn print_auth_url(auth_url: &str) {
    eprintln!();
    eprintln!("Browser didn't open? Use the url below to sign in");
    eprintln!();
    eprintln!("{auth_url}");
    eprintln!();
}

fn parse_query_params(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let key = urlencoding::decode(key).ok()?.into_owned();
            let value = urlencoding::decode(value).ok()?.into_owned();
            Some((key, value))
        })
        .collect()
}

fn callback_code_from_request_line(
    request_line: &str,
    expected_state: &str,
) -> Result<Option<String>> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();

    if method != "GET" {
        return Ok(None);
    }

    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        return Ok(None);
    }

    let params = parse_query_params(query);
    if params.get("state").map(String::as_str) != Some(expected_state) {
        bail!("Invalid state parameter");
    }

    let code = params
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Missing code parameter"))?;

    Ok(Some(code))
}

fn respond_to_callback(mut stream: TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn respond_to_callback_text(stream: TcpStream, status: &str, body: &str) {
    respond_to_callback(stream, status, "text/plain; charset=utf-8", body.as_bytes());
}

fn respond_to_callback_redirect(mut stream: TcpStream, location: &str) {
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn handle_callback_request(
    stream: TcpStream,
    expected_state: &str,
) -> Result<Option<(String, TcpStream)>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    match callback_code_from_request_line(&request_line, expected_state) {
        Ok(Some(code)) => Ok(Some((code, stream))),
        Ok(None) => {
            respond_to_callback_text(stream, "404 Not Found", "Not found");
            Ok(None)
        }
        Err(error) => {
            respond_to_callback_text(stream, "400 Bad Request", &error.to_string());
            Err(error)
        }
    }
}

fn start_callback_server(
    expected_state: String,
    tx: mpsc::UnboundedSender<AuthInput>,
) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let callback_url = format!("http://127.0.0.1:{port}/callback");

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };

            match handle_callback_request(stream, &expected_state) {
                Ok(Some((code, callback_stream))) => {
                    let _ = tx.send(AuthInput::Code {
                        code,
                        source: AuthCodeSource::Callback,
                        callback_stream: Some(callback_stream),
                    });
                    break;
                }
                Ok(None) => {}
                Err(_) => {}
            }
        }
    });

    Ok(callback_url)
}

fn read_auth_code() -> Result<Option<String>> {
    eprint!("Paste code here if prompted > ");
    let _ = io::stderr().flush();

    let mut input = String::new();
    let bytes_read = io::stdin().lock().read_line(&mut input)?;
    if bytes_read == 0 {
        bail!("Authentication cancelled");
    }

    let code = input.trim().to_string();
    if code.is_empty() {
        return Ok(None);
    }

    Ok(Some(code))
}

fn spawn_auth_code_reader(tx: mpsc::UnboundedSender<AuthInput>) {
    thread::spawn(move || loop {
        match read_auth_code() {
            Ok(Some(code)) => {
                let _ = tx.send(AuthInput::Code {
                    code,
                    source: AuthCodeSource::Prompt,
                    callback_stream: None,
                });
                return;
            }
            Ok(None) => {}
            Err(error) => {
                let _ = tx.send(AuthInput::Error(error.to_string()));
                return;
            }
        }
    });
}

async fn handle_auth_input(
    input: AuthInput,
    state: &str,
    code_verifier: &str,
    stdin_is_terminal: bool,
    tx: &mpsc::UnboundedSender<AuthInput>,
    success_url: &str,
) -> Result<Option<LoginResult>> {
    match input {
        AuthInput::Code {
            code,
            source,
            callback_stream,
        } => match redeem_auth_code(code, state, code_verifier).await {
            Ok(result) => {
                if let Some(stream) = callback_stream {
                    respond_to_callback_redirect(stream, success_url);
                }
                if matches!(source, AuthCodeSource::Callback) && stdin_is_terminal {
                    eprintln!();
                }
                Ok(Some(result))
            }
            Err(error) if matches!(source, AuthCodeSource::Prompt) && stdin_is_terminal => {
                eprintln!("Invalid auth code: {error}");
                spawn_auth_code_reader(tx.clone());
                Ok(None)
            }
            Err(error) if matches!(source, AuthCodeSource::Callback) => {
                if let Some(stream) = callback_stream {
                    respond_to_callback_text(
                        stream,
                        "500 Internal Server Error",
                        "Authentication failed. Return to the terminal and try again.",
                    );
                }
                if stdin_is_terminal {
                    eprintln!("Automatic browser callback failed: {error}");
                    Ok(None)
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        },
        AuthInput::Error(error) => bail!(error),
    }
}

async fn redeem_auth_code(code: String, state: &str, code_verifier: &str) -> Result<LoginResult> {
    let api_host = config::get("api_host")?;
    let url = format!("{}/cli/auth/redeem", api_host);
    let client = config::create_http_client()?;
    let response = client
        .post(url)
        .timeout(AUTH_CODE_REDEEM_TIMEOUT)
        .json(&RedeemAuthCodeRequest {
            code,
            state: state.to_string(),
            code_verifier: code_verifier.to_string(),
        })
        .send()
        .await?;
    let response = config::check_api_response(response).await?;
    let body = response.json::<RedeemAuthCodeResponse>().await?;

    Ok(LoginResult {
        api_key: body.api_key,
        email: body.email,
    })
}

pub async fn login_with_browser() -> Result<LoginResult> {
    let state = generate_random_token()?;
    let code_verifier = generate_random_token()?;
    let code_challenge = pkce_challenge_from_verifier(&code_verifier);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let callback_url = start_callback_server(state.clone(), tx.clone())?;
    let website_host = config::get("open_browser_website_host")?;
    let auto_auth_url = format!(
        "{}/auth/cli?code=true&state={}&code_challenge={}&callback_uri={}",
        website_host,
        urlencoding::encode(&state),
        urlencoding::encode(&code_challenge),
        urlencoding::encode(&callback_url)
    );
    let manual_auth_url = format!(
        "{}/auth/cli?code=true&state={}&code_challenge={}",
        website_host,
        urlencoding::encode(&state),
        urlencoding::encode(&code_challenge)
    );
    let success_url = format!("{website_host}/auth/cli?success=true");

    let stdin_is_terminal = io::stdin().is_terminal();
    print_opening_browser();
    print_auth_url(&manual_auth_url);
    let _ = open::that(&auto_auth_url);
    let mut auth_code_prompt_started = false;

    loop {
        tokio::select! {
            biased;

            input = rx.recv() => {
                let Some(input) = input else {
                    bail!("Authentication cancelled");
                };
                let input_source = match &input {
                    AuthInput::Code { source, .. } => Some(*source),
                    AuthInput::Error(_) => None,
                };
                if let Some(result) = handle_auth_input(
                    input,
                    &state,
                    &code_verifier,
                    stdin_is_terminal,
                    &tx,
                    &success_url,
                )
                .await?
                {
                    return Ok(result);
                }
                if matches!(input_source, Some(AuthCodeSource::Callback))
                    && stdin_is_terminal
                    && !auth_code_prompt_started
                {
                    spawn_auth_code_reader(tx.clone());
                    auth_code_prompt_started = true;
                }
            }
            result = signal::ctrl_c() => {
                result?;
                bail!("Authentication cancelled")
            }
            _ = sleep(AUTH_CODE_PROMPT_DELAY), if stdin_is_terminal && !auth_code_prompt_started => {
                spawn_auth_code_reader(tx.clone());
                auth_code_prompt_started = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_rfc7636_s256_pkce_challenge() {
        let challenge = pkce_challenge_from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");

        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn extracts_callback_code_for_matching_state() {
        let code = callback_code_from_request_line(
            "GET /callback?code=wdcli_abc&state=state_123 HTTP/1.1",
            "state_123",
        )
        .unwrap();

        assert_eq!(code.as_deref(), Some("wdcli_abc"));
    }

    #[test]
    fn rejects_callback_code_for_mismatched_state() {
        let error = callback_code_from_request_line(
            "GET /callback?code=wdcli_abc&state=other HTTP/1.1",
            "state_123",
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "Invalid state parameter");
    }
}
