use std::fs;
use std::path::{Component, Path, PathBuf};

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

/// Locate the HTML entrypoint for engines that ship a single root export
/// (Godot, Unity): a root `index.html`, falling back to the first `.html` found.
/// Defold doesn't use this — it parses the explicit `entrypoint` from
/// wavedash.toml, since its bundles can nest the HTML under `wasm-web`/`js-web`
/// and ship more than one. No "which export is newest" guessing lives here.
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

/// Resolve the developer-named Defold entrypoint HTML to its absolute path plus
/// normalized build-relative path. Defold names its entrypoint explicitly (a
/// bundle can ship both `wasm-web/` and `js-web/`), so there's no inference here
/// — a missing or wrong path is a clear error, not a guess.
pub fn resolve_defold_entrypoint(
    upload_dir: &Path,
    entrypoint: Option<&str>,
) -> Result<(PathBuf, String)> {
    let entrypoint = entrypoint.ok_or_else(|| {
        anyhow::anyhow!(
            "Defold builds need an `entrypoint` in wavedash.toml pointing to your HTML5 export, e.g.\n  entrypoint = \"wasm-web/<game>/index.html\"\n(a Defold bundle can contain both wasm-web/ and js-web/ — pick one)"
        )
    })?;
    let entrypoint_input = entrypoint.replace('\\', "/");
    let entrypoint_path = Path::new(&entrypoint_input);
    if entrypoint_path.is_absolute()
        || entrypoint_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!(
            "Defold entrypoint `{}` must be a relative path inside upload_dir ({}).",
            entrypoint,
            upload_dir.display()
        );
    }
    let relative_path = entrypoint_path
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            Component::CurDir => None,
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");

    let lower = relative_path.to_ascii_lowercase();
    if !lower.ends_with(".html") && !lower.ends_with(".htm") {
        anyhow::bail!(
            "Defold entrypoint `{}` must be an HTML file (.html/.htm).",
            entrypoint
        );
    }

    let html_path = upload_dir.join(&relative_path);
    if !html_path.is_file() {
        anyhow::bail!(
            "Defold entrypoint `{}` not found under {}. Point `entrypoint` in wavedash.toml at your export's index.html.",
            entrypoint,
            upload_dir.display()
        );
    }

    let canonical_upload_dir = upload_dir
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", upload_dir.display()))?;
    let canonical_html_path = html_path
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", html_path.display()))?;
    if !canonical_html_path.starts_with(&canonical_upload_dir) {
        anyhow::bail!(
            "Defold entrypoint `{}` resolves outside upload_dir ({}).",
            entrypoint,
            upload_dir.display()
        );
    }

    Ok((html_path, relative_path))
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

    let mut body = serde_json::json!({
        "engine": engine,
        "engineVersion": engine_version,
        "htmlContent": html_content,
    });
    if let Some(html_path) = html_relative_path {
        body["htmlPath"] = serde_json::json!(html_path);
    }

    let client = config::create_http_client()?;
    let response = client
        .post(&endpoint)
        .json(&body)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_upload_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "wavedash-cli-entrypoint-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn resolves_defold_entrypoint_inside_upload_dir() {
        let upload_dir = temp_upload_dir("inside");
        let html_path = upload_dir.join("wasm-web/example/index.html");
        fs::create_dir_all(html_path.parent().expect("html parent")).expect("create dirs");
        fs::write(&html_path, "<html></html>").expect("write html");

        let (resolved, relative) =
            resolve_defold_entrypoint(&upload_dir, Some("wasm-web/example/index.html"))
                .expect("resolve entrypoint");

        assert_eq!(resolved, html_path);
        assert_eq!(relative, "wasm-web/example/index.html");

        fs::remove_dir_all(upload_dir).expect("cleanup");
    }

    #[test]
    fn normalizes_defold_entrypoint_backslashes() {
        let upload_dir = temp_upload_dir("backslashes");
        let html_path = upload_dir.join("wasm-web/example/index.html");
        fs::create_dir_all(html_path.parent().expect("html parent")).expect("create dirs");
        fs::write(&html_path, "<html></html>").expect("write html");

        let (resolved, relative) =
            resolve_defold_entrypoint(&upload_dir, Some("wasm-web\\example\\index.html"))
                .expect("resolve entrypoint");

        assert_eq!(resolved, html_path);
        assert_eq!(relative, "wasm-web/example/index.html");

        fs::remove_dir_all(upload_dir).expect("cleanup");
    }

    #[test]
    fn normalizes_defold_entrypoint_dot_segments() {
        let upload_dir = temp_upload_dir("dot-segments");
        let html_path = upload_dir.join("wasm-web/example/index.html");
        fs::create_dir_all(html_path.parent().expect("html parent")).expect("create dirs");
        fs::write(&html_path, "<html></html>").expect("write html");

        let (resolved, relative) =
            resolve_defold_entrypoint(&upload_dir, Some("./wasm-web/./example/index.html"))
                .expect("resolve entrypoint");

        assert_eq!(resolved, html_path);
        assert_eq!(relative, "wasm-web/example/index.html");

        fs::remove_dir_all(upload_dir).expect("cleanup");
    }

    #[test]
    fn rejects_defold_entrypoint_escape() {
        let upload_dir = temp_upload_dir("escape");
        fs::create_dir_all(&upload_dir).expect("create upload dir");

        let err = resolve_defold_entrypoint(&upload_dir, Some("../index.html"))
            .expect_err("escape should fail");

        assert!(err.to_string().contains("relative path inside upload_dir"));

        fs::remove_dir_all(upload_dir).expect("cleanup");
    }

    #[test]
    fn rejects_defold_entrypoint_non_html() {
        let upload_dir = temp_upload_dir("non-html");
        let asset_path = upload_dir.join("wasm-web/example/something.png");
        fs::create_dir_all(asset_path.parent().expect("asset parent")).expect("create dirs");
        fs::write(&asset_path, "not html").expect("write asset");

        let err = resolve_defold_entrypoint(&upload_dir, Some("wasm-web/example/something.png"))
            .expect_err("non-html entrypoint should fail");

        assert!(err.to_string().contains("HTML file"));

        fs::remove_dir_all(upload_dir).expect("cleanup");
    }
}
