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

// A root `index.html` short-circuits; the ranking below only matters for nested
// multi-HTML Defold dists.
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

    // Match the arch folder as any path segment (Defold nests it under a game dir).
    let architecture_score = |relative: &str| {
        let mut segments = relative.split('/');
        if segments.clone().any(|s| s == "wasm-web") {
            0
        } else if segments.any(|s| s == "js-web") {
            1
        } else {
            2
        }
    };

    // Compute each candidate's sort key once (stat + strip_prefix), so sorting
    // doesn't re-stat on every comparison.
    let mut ranked: Vec<(std::time::SystemTime, i32, String, PathBuf)> = html_files
        .into_iter()
        .map(|path| {
            let modified = path
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let relative = path
                .strip_prefix(upload_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let arch = architecture_score(&relative);
            (modified, arch, relative, path)
        })
        .collect();

    // Newest export wins; wasm-web beats js-web only on ties, then path. Unreadable
    // mtime falls back to UNIX_EPOCH (oldest) so it can't sort as "newest".
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    ranked.into_iter().next().map(|(_, _, _, path)| path)
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
    let endpoint = format!(
        "{}/cli/entrypoint-params",
        api_host.trim_end_matches('/')
    );

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
