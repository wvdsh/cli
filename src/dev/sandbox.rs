use anyhow::{Context, Result};
use url::Url;

use crate::config;

use super::url_params::UrlParams;

pub fn build_sandbox_url(
    game_id: &str,
    game_slug: &str,
    build_uuid: &str,
    local_origin: &str,
) -> Result<Url> {
    let host = config::get("open_browser_website_host")?;
    let base = host.trim_end_matches('/');

    // Build the game URL (what will be the rdurl parameter)
    let game_url_full = format!("{}/playtest/{}/{}", base, game_slug, build_uuid);
    let mut game_url = Url::parse(&game_url_full)
        .with_context(|| format!("Unable to parse website host {}", game_url_full))?;

    {
        let mut pairs = game_url.query_pairs_mut();
        pairs.append_pair(UrlParams::LOCAL_ORIGIN, local_origin);
    }

    // Build the permission-grant URL on the subdomain
    // Required for local network access within the iframe
    let host_url = Url::parse(&format!(
        "https://{}",
        base.trim_start_matches("https://")
            .trim_start_matches("http://")
    ))?;
    let main_host = host_url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Could not extract host"))?;

    // Use game_id (not game_slug) to match the origin used by GameRunnerComponent
    let subdomain = format!("{}.local.{}", game_id, main_host);
    let permission_grant_url = format!("https://{}/local/permission-grant", subdomain);

    let mut url = Url::parse(&permission_grant_url)?;

    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair(UrlParams::REDIRECT_URL, game_url.as_str());
        pairs.append_pair(UrlParams::LOCAL_ORIGIN, local_origin);
    }

    Ok(url)
}
