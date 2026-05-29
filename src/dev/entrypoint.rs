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

// Shared by the Godot/Unity/Defold dev and build flows. A root-level `index.html`
// short-circuits, so single-HTML exports (Godot/Unity) keep their old behavior; the
// mtime/architecture ranking below only matters for nested multi-HTML dists (Defold).
pub fn locate_html_entrypoint(upload_dir: &Path) -> Option<PathBuf> {
    let default_index = upload_dir.join("index.html");
    if default_index.is_file() {
        return Some(default_index);
    }

    let mut html_files: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(upload_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            if let Some(ext) = entry.path().extension() {
                if ext.eq_ignore_ascii_case("html") {
                    html_files.push(entry.into_path());
                }
            }
        }
    }

    html_files.sort_by(|a, b| {
        let modified_a = a.metadata().and_then(|m| m.modified()).ok();
        let modified_b = b.metadata().and_then(|m| m.modified()).ok();
        let relative_a = a
            .strip_prefix(upload_dir)
            .unwrap_or(a)
            .to_string_lossy()
            .replace('\\', "/");
        let relative_b = b
            .strip_prefix(upload_dir)
            .unwrap_or(b)
            .to_string_lossy()
            .replace('\\', "/");
        let architecture_score = |relative: &str| {
            if relative.starts_with("wasm-web/") {
                0
            } else if relative.starts_with("js-web/") {
                1
            } else {
                2
            }
        };

        modified_b
            .cmp(&modified_a)
            .then_with(|| architecture_score(&relative_a).cmp(&architecture_score(&relative_b)))
            .then_with(|| relative_a.cmp(&relative_b))
    });

    html_files.into_iter().next()
}

pub async fn fetch_entrypoint_params(
    engine: &str,
    engine_version: &str,
    html_path: &Path,
    html_relative_path: Option<&str>,
) -> Result<Value> {
    let html_content = fs::read_to_string(html_path)
        .with_context(|| format!("Failed to read {}", html_path.display()))?;
    let api_host = config::get("api_host")?;
    let endpoint = format!("{}/cli/entrypoint-params", api_host.trim_end_matches('/'));

    let client = config::create_http_client()?;
    let response = client
        .post(&endpoint)
        .json(&serde_json::json!({
            "engine": engine,
            "engineVersion": engine_version,
            "htmlContent": html_content,
            "htmlPath": html_relative_path,
        }))
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
