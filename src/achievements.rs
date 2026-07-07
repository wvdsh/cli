use crate::auth::{AuthManager, AuthSource};
use crate::config;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct Achievement {
    _id: String,
    identifier: String,
    #[serde(rename = "displayName")]
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct ImageUploadUrlResponse {
    #[serde(rename = "uploadUrl")]
    upload_url: String,
    #[serde(rename = "r2Key")]
    r2_key: String,
}

/// Presign, PUT, return the r2Key that should be sent as `image` on the
/// achievement create/update body. Mirrors the UI flow: fetch presigned URL →
/// PUT bytes → store r2Key. Allowed extensions are enforced server-side; we
/// just pass whatever the file has and surface the API error if it's rejected.
async fn upload_achievement_image(
    api_key: &str,
    game_id: &str,
    identifier: &str,
    image_path: &Path,
) -> Result<String> {
    let extension = image_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Image file has no extension: {}",
                image_path.display()
            )
        })?;

    let bytes = std::fs::read(image_path)
        .with_context(|| format!("Failed to read image file: {}", image_path.display()))?;

    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    // 1. Get presigned URL
    let presign_url = format!(
        "{}/api/games/{}/achievements/image-upload-url",
        api_host, game_id
    );
    let resp = client
        .post(&presign_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "identifier": identifier,
            "fileExtension": extension,
        }))
        .send()
        .await?;
    let resp = config::check_api_response(resp).await?;
    let presigned: ImageUploadUrlResponse = resp.json().await?;

    // 2. PUT the bytes directly to R2. UI doesn't set Content-Type either.
    let put_resp = client
        .put(&presigned.upload_url)
        .body(bytes)
        .send()
        .await?;
    if !put_resp.status().is_success() {
        let status = put_resp.status();
        let body = put_resp.text().await.unwrap_or_default();
        anyhow::bail!("Image upload to R2 failed ({}): {}", status, body);
    }

    Ok(presigned.r2_key)
}

fn require_api_key() -> Result<String> {
    let auth_manager = AuthManager::new()?;
    let auth_info = auth_manager.get_auth_info();
    match auth_info.source {
        AuthSource::None => {
            anyhow::bail!("Not authenticated. Run `wavedash auth login` first.")
        }
        _ => Ok(auth_info.api_key.unwrap()),
    }
}

pub struct CreateAchievementArgs<'a> {
    pub game_id: &'a str,
    pub identifier: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub secret: bool,
    pub triggered_by_stat_id: Option<&'a str>,
    pub stat_threshold: Option<f64>,
    pub image_path: Option<&'a Path>,
}

pub async fn handle_achievement_create(args: CreateAchievementArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;

    // Upload the image first; the resulting r2Key is what the API stores as `image`.
    // If create then fails, the blob is orphaned in R2 — same behavior as the UI.
    let image_r2_key = if let Some(path) = args.image_path {
        Some(upload_achievement_image(&api_key, args.game_id, args.identifier, path).await?)
    } else {
        None
    };

    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/achievements", api_host, args.game_id);

    let mut body = json!({
        "identifier": args.identifier,
        "displayName": args.title,
        "description": args.description,
        "secret": args.secret,
    });

    if let Some(r2_key) = image_r2_key {
        body["image"] = json!(r2_key);
    }

    if let Some(stat_id) = args.triggered_by_stat_id {
        let threshold = args.stat_threshold.ok_or_else(|| {
            anyhow::anyhow!("--threshold is required when --triggered-by-stat-id is set")
        })?;
        body["statId"] = json!(stat_id);
        body["statThreshold"] = json!(threshold);
    }

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let achievement: Achievement = resp.json().await?;
    println!(
        "✓ Created achievement \"{}\" (id: {}, identifier: {})",
        achievement.display_name, achievement._id, achievement.identifier
    );
    Ok(())
}

pub struct UpdateAchievementArgs<'a> {
    pub game_id: &'a str,
    pub achievement_id: &'a str,
    pub title: Option<&'a str>,
    pub identifier: Option<&'a str>,
    pub description: Option<&'a str>,
    pub secret: Option<bool>,
    /// `Some(Some(id))` sets the stat link, `Some(None)` clears it,
    /// `None` leaves it alone.
    pub triggered_by_stat_id: Option<Option<&'a str>>,
    pub stat_threshold: Option<f64>,
    pub image_path: Option<&'a Path>,
}

pub async fn handle_achievement_update(args: UpdateAchievementArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;

    // Upload first if --image was passed. We use the achievement doc id as the
    // "identifier" embedded in the R2 key — it's stable + unique, just a path
    // hint for findability.
    let image_r2_key = if let Some(path) = args.image_path {
        Some(upload_achievement_image(&api_key, args.game_id, args.achievement_id, path).await?)
    } else {
        None
    };

    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/achievements/{}",
        api_host, args.game_id, args.achievement_id
    );

    let mut body = serde_json::Map::new();
    if let Some(title) = args.title {
        body.insert("displayName".into(), json!(title));
    }
    if let Some(identifier) = args.identifier {
        body.insert("identifier".into(), json!(identifier));
    }
    if let Some(description) = args.description {
        body.insert("description".into(), json!(description));
    }
    if let Some(secret) = args.secret {
        body.insert("secret".into(), json!(secret));
    }
    if let Some(r2_key) = image_r2_key {
        body.insert("image".into(), json!(r2_key));
    }
    match args.triggered_by_stat_id {
        Some(Some(stat_id)) => {
            let threshold = args.stat_threshold.ok_or_else(|| {
                anyhow::anyhow!("--threshold is required when setting --triggered-by-stat-id")
            })?;
            body.insert("statId".into(), json!(stat_id));
            body.insert("statThreshold".into(), json!(threshold));
        }
        Some(None) => {
            body.insert("statId".into(), serde_json::Value::Null);
        }
        None => {
            if let Some(threshold) = args.stat_threshold {
                body.insert("statThreshold".into(), json!(threshold));
            }
        }
    }

    if body.is_empty() {
        anyhow::bail!("No fields provided to update.");
    }

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&serde_json::Value::Object(body))
        .send()
        .await?;

    config::check_api_response(resp).await?;
    println!("✓ Updated achievement {}", args.achievement_id);
    Ok(())
}

pub async fn handle_achievement_delete(
    game_id: &str,
    achievement_id: &str,
    force: bool,
) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/achievements/{}?force={}",
        api_host, game_id, achievement_id, force
    );

    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    config::check_api_response(resp).await?;
    println!("✓ Deleted achievement {}", achievement_id);
    Ok(())
}
