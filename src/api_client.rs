use crate::auth::AuthManager;
use crate::config;
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    error: String,
}

/// Shared HTTP client for the Wavedash API.
/// Handles base URL resolution, auth, and error parsing.
pub struct ApiClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl ApiClient {
    pub fn new() -> Result<Self> {
        let client = config::create_http_client()?;
        let base_url = format!("{}/api", config::get("api_host")?);

        let auth_manager = AuthManager::new()?;
        let api_key = auth_manager
            .get_api_key()
            .ok_or_else(|| anyhow::anyhow!("Not authenticated. Run 'wvdsh auth login' first."))?;

        Ok(Self {
            client,
            base_url,
            api_key,
        })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;
        self.handle_response(response).await
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(body)
            .send()
            .await?;
        self.handle_response(response).await
    }

    pub async fn put<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let response = self
            .client
            .put(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(body)
            .send()
            .await?;
        self.handle_response(response).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .client
            .delete(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;
        self.handle_response(response).await
    }

    async fn handle_response<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T> {
        if !response.status().is_success() {
            let error_text = response.text().await?;

            // The stats/achievements API returns { error: "message" }
            if let Ok(api_error) = serde_json::from_str::<ApiErrorResponse>(&error_text) {
                // The error field may itself be JSON (nested pattern from builds API)
                #[derive(Deserialize)]
                struct NestedError {
                    message: String,
                }
                if let Ok(nested) = serde_json::from_str::<NestedError>(&api_error.error) {
                    anyhow::bail!("{}", nested.message);
                }
                anyhow::bail!("{}", api_error.error);
            }

            anyhow::bail!("API request failed: {}", error_text);
        }

        Ok(response.json().await?)
    }
}
