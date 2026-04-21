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

pub fn locate_html_entrypoint(upload_dir: &Path) -> Result<Option<PathBuf>> {
    // `index.html` always wins if present — that's the Web convention and
    // sidesteps ambiguity when game exporters also emit helper HTML (debug
    // shells, fullscreen wrappers, etc.).
    let default_index = upload_dir.join("index.html");
    if default_index.is_file() {
        return Ok(Some(default_index));
    }

    // Only look at the upload_dir's top-level — a Godot/Unity/etc. export
    // puts its entrypoint at the root of the upload folder, and recursing
    // would flag engine-emitted subdir HTML (e.g. Unity's `TemplateData/`)
    // as an ambiguity.
    let mut found: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(upload_dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let is_html = entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"));
        if is_html {
            found.push(entry.into_path());
            // Two is enough to know it's ambiguous — but keep collecting so
            // the error message lists every offender (capped at a handful).
            if found.len() >= 8 {
                break;
            }
        }
    }

    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found.remove(0))),
        _ => {
            let mut names: Vec<String> = found
                .iter()
                .map(|p| {
                    p.strip_prefix(upload_dir)
                        .unwrap_or(p)
                        .display()
                        .to_string()
                })
                .collect();
            names.sort();
            anyhow::bail!(
                "Multiple HTML files found in upload_dir ({}):\n  - {}\n\n\
                 Name one of them `index.html`, or remove the extras (often \
                 stale files from a previous export with a different name).",
                upload_dir.display(),
                names.join("\n  - ")
            )
        }
    }
}

pub async fn fetch_entrypoint_params(engine: &str, engine_version: &str, html_path: &Path) -> Result<Value> {
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
