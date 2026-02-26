use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use walkdir::WalkDir;

use crate::config;

#[derive(Deserialize)]
struct EntrypointParamsResponse {
    #[serde(rename = "entrypointParams")]
    entrypoint_params: Value,
}

pub fn locate_html_entrypoint(upload_dir: &Path) -> Option<PathBuf> {
    let default_index = upload_dir.join("index.html");
    if default_index.is_file() {
        return Some(default_index);
    }

    for entry in WalkDir::new(upload_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            if let Some(ext) = entry.path().extension() {
                if ext.eq_ignore_ascii_case("html") {
                    return Some(entry.into_path());
                }
            }
        }
    }

    None
}

pub async fn fetch_entrypoint_params(engine: &str, html_path: &Path) -> Result<Value> {
    let html_content = fs::read_to_string(html_path)
        .with_context(|| format!("Failed to read {}", html_path.display()))?;
    let api_host = config::get("api_host")?;
    let endpoint = format!(
        "{}/cli/entrypoint-params",
        api_host.trim_end_matches('/')
    );

    let client = config::create_http_client()?;
    let response = client
        .get(&endpoint)
        .query(&[("engine", engine), ("htmlContent", html_content.as_str())])
        .send()
        .await
        .with_context(|| "Failed to call CLI entrypoint params endpoint")?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        anyhow::bail!("CLI endpoint responded with {}: {}", status, body);
    }

    let parsed: EntrypointParamsResponse =
        serde_json::from_str(&body).with_context(|| "Failed to parse CLI response")?;

    Ok(parsed.entrypoint_params)
}
