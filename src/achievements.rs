use crate::auth::require_api_key;
use crate::config;
use anyhow::{Context, Result};
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

/// The create response, narrowed to the fields printed by the command. This is
/// deliberately separate from `Achievement`, whose list payload is larger.
#[derive(Debug, Deserialize)]
struct CreatedAchievement {
    _id: String,
    identifier: String,
    #[serde(rename = "displayName")]
    display_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Achievement {
    _id: String,
    identifier: String,
    #[serde(rename = "displayName")]
    display_name: String,
    description: String,
    image: String,
    secret: bool,
    #[serde(rename = "statId", skip_serializing_if = "Option::is_none")]
    stat_id: Option<String>,
    #[serde(rename = "statThreshold", skip_serializing_if = "Option::is_none")]
    stat_threshold: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct AchievementsResponse {
    achievements: Vec<Achievement>,
}

#[derive(Debug, Deserialize)]
struct ImageMediaUploadResponse {
    #[serde(rename = "assemblyOptions")]
    assembly_options: AssemblyOptions,
    #[serde(rename = "maxBytes")]
    max_bytes: u64,
    #[serde(rename = "r2Key")]
    r2_key: String,
}

#[derive(Debug, Deserialize)]
struct AssemblyOptions {
    params: String,
    signature: String,
}

#[derive(Debug, Deserialize)]
struct AssemblyStatus {
    ok: Option<String>,
    error: Option<String>,
    message: Option<String>,
    assembly_ssl_url: Option<String>,
}

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
        .ok_or_else(|| anyhow::anyhow!("Image file has no extension: {}", image_path.display()))?;

    let bytes = std::fs::read(image_path)
        .with_context(|| format!("Failed to read image file: {}", image_path.display()))?;

    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;

    let authorization_url = format!(
        "{}/api/games/{}/achievements/image-media-upload",
        api_host, game_id
    );
    let resp = client
        .post(&authorization_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "identifier": identifier,
            "fileExtension": extension,
        }))
        .send()
        .await?;
    let resp = config::check_api_response(resp).await?;
    let authorization: ImageMediaUploadResponse = resp.json().await?;

    if bytes.is_empty() || bytes.len() as u64 > authorization.max_bytes {
        anyhow::bail!(
            "Image must be between 1 and {} bytes",
            authorization.max_bytes
        );
    }
    let form = reqwest::multipart::Form::new()
        .text("params", authorization.assembly_options.params)
        .text("signature", authorization.assembly_options.signature)
        .part(
            "file",
            reqwest::multipart::Part::bytes(bytes).file_name(
                image_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
    let media_client = reqwest::Client::new();
    let mut response = media_client
        .post("https://api2.transloadit.com/assemblies")
        .multipart(form)
        .timeout(std::time::Duration::from_secs(900))
        .send()
        .await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(900);
    let mut status_url: Option<String> = None;
    loop {
        let http_status = response.status();
        let status: AssemblyStatus = response
            .json()
            .await
            .context("Invalid Transloadit response")?;
        if !http_status.is_success() || status.error.is_some() {
            anyhow::bail!(
                "Image processing failed: {}",
                status
                    .message
                    .or(status.error)
                    .unwrap_or_else(|| http_status.to_string())
            );
        }
        match status.ok.as_deref() {
            Some("ASSEMBLY_COMPLETED") => break,
            Some("ASSEMBLY_UPLOADING" | "ASSEMBLY_EXECUTING") => {}
            _ => anyhow::bail!("Unexpected image processing status: {:?}", status.ok),
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Image processing timed out");
        }
        if status_url.is_none() {
            status_url = status.assembly_ssl_url;
        }
        let url = reqwest::Url::parse(
            status_url
                .as_deref()
                .context("Missing Assembly status URL")?,
        )?;
        if url.scheme() != "https"
            || !url
                .host_str()
                .is_some_and(|host| host.ends_with(".transloadit.com"))
        {
            anyhow::bail!("Invalid Transloadit Assembly status URL");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        response = media_client
            .get(url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;
    }

    Ok(authorization.r2_key)
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

pub async fn handle_achievement_list(game_id: &str, json: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/achievements", api_host, game_id);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let data: AchievementsResponse = resp.json().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&data.achievements)?);
        return Ok(());
    }

    if data.achievements.is_empty() {
        println!("No achievements found.");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("ID"),
            Cell::new("Identifier"),
            Cell::new("Title"),
            Cell::new("Description"),
            Cell::new("Secret"),
            Cell::new("Stat ID"),
            Cell::new("Threshold"),
        ]);

    for achievement in data.achievements {
        table.add_row(vec![
            achievement._id,
            achievement.identifier,
            achievement.display_name,
            achievement.description,
            (if achievement.secret { "yes" } else { "no" }).to_string(),
            achievement.stat_id.unwrap_or_else(|| "-".to_string()),
            achievement
                .stat_threshold
                .map(|threshold| threshold.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ]);
    }

    println!("{table}");
    Ok(())
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
    let achievement: CreatedAchievement = resp.json().await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_achievement_list_response() {
        let response: AchievementsResponse = serde_json::from_value(json!({
            "achievements": [{
                "_id": "achievement-id",
                "identifier": "FIRST_WIN",
                "displayName": "First Win",
                "description": "Win a match",
                "image": "achievements/first-win.png",
                "secret": false,
                "statId": "wins-stat-id",
                "statThreshold": 1
            }]
        }))
        .expect("the API response should deserialize");

        let achievement = &response.achievements[0];
        assert_eq!(achievement._id, "achievement-id");
        assert_eq!(achievement.identifier, "FIRST_WIN");
        assert_eq!(achievement.display_name, "First Win");
        assert_eq!(achievement.stat_id.as_deref(), Some("wins-stat-id"));
        assert_eq!(achievement.stat_threshold, Some(1.0));
    }

    #[test]
    fn parses_an_achievement_without_a_stat_link() {
        let response: AchievementsResponse = serde_json::from_value(json!({
            "achievements": [{
                "_id": "achievement-id",
                "identifier": "WELCOME",
                "displayName": "Welcome",
                "description": "Start the game",
                "image": "",
                "secret": true
            }]
        }))
        .expect("an achievement with no stat link should deserialize");

        let achievement = &response.achievements[0];
        assert!(achievement.secret);
        assert_eq!(achievement.stat_id, None);
        assert_eq!(achievement.stat_threshold, None);
    }

    #[test]
    fn parses_an_image_media_upload_authorization() {
        let response: ImageMediaUploadResponse = serde_json::from_value(json!({
            "assemblyOptions": {"params": "{}", "signature": "sha384:signed"},
            "maxBytes": 25000000,
            "r2Key": "org/game/achievements/first-win.webp"
        }))
        .expect("the media upload authorization should deserialize");

        assert_eq!(response.assembly_options.signature, "sha384:signed");
        assert_eq!(response.max_bytes, 25_000_000);
        assert_eq!(response.r2_key, "org/game/achievements/first-win.webp");
    }

    #[test]
    fn json_output_uses_api_field_names_and_omits_empty_stat_fields() {
        let achievement = Achievement {
            _id: "achievement-id".to_string(),
            identifier: "WELCOME".to_string(),
            display_name: "Welcome".to_string(),
            description: "Start the game".to_string(),
            image: "achievements/welcome.png".to_string(),
            secret: false,
            stat_id: None,
            stat_threshold: None,
        };

        let value = serde_json::to_value(achievement).expect("achievement should serialize");
        assert_eq!(value["displayName"], "Welcome");
        assert_eq!(value["image"], "achievements/welcome.png");
        assert!(value.get("display_name").is_none());
        assert!(value.get("statId").is_none());
        assert!(value.get("statThreshold").is_none());
    }
}
