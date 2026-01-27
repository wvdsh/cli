use anyhow::{Context, Result};
use serde_json::Value;
use url::Url;

use crate::config::{self, WavedashConfig};

use super::url_params::UrlParams;

pub fn build_sandbox_url(
    wavedash_config: &WavedashConfig,
    engine_label: &str,
    engine_version: &str,
    local_origin: &str,
    entrypoint: Option<&str>,
    entrypoint_params: Option<&Value>,
) -> Result<Url> {
    let host = config::get("open_browser_website_host")?;
    let base = host.trim_end_matches('/');

    // First, build the game URL (what will be the rdurl parameter)
    let game_url_full = format!("{}/play/{}", base, wavedash_config.game);
    let mut game_url = Url::parse(&game_url_full)
        .with_context(|| format!("Unable to parse website host {}", game_url_full))?;

    {
        let mut pairs = game_url.query_pairs_mut();
        pairs.append_pair(UrlParams::GAME_SUBDOMAIN, &wavedash_config.game);
        pairs.append_pair(UrlParams::GAME_ENVIRONMENT, wavedash_config.environment.as_str());
        pairs.append_pair(UrlParams::GAME_CLOUD_ID, &wavedash_config.org);
        pairs.append_pair(UrlParams::LOCAL_ORIGIN, local_origin);
        pairs.append_pair(UrlParams::SANDBOX, "true");
        pairs.append_pair(UrlParams::ENGINE, engine_label);
        pairs.append_pair(UrlParams::ENGINE_VERSION, engine_version);

        if let Some(entrypoint) = entrypoint {
            pairs.append_pair(UrlParams::ENTRYPOINT, entrypoint);
        }

        if let Some(params) = entrypoint_params {
            let serialized = serde_json::to_string(params)?;
            pairs.append_pair(UrlParams::ENTRYPOINT_PARAMS, &serialized);
        }
    }

    // Now build the permission-grant URL on the subdomain
    // Extract the host from the base URL (e.g., "staging.wavedash.gg")
    let host_url = Url::parse(&format!("https://{}", base.trim_start_matches("https://").trim_start_matches("http://")))?;
    let main_host = host_url.host_str().ok_or_else(|| anyhow::anyhow!("Could not extract host"))?;

    let subdomain = format!("{}.sandbox.{}", wavedash_config.game, main_host);
    let permission_grant_url = format!("https://{}/sandbox/permission-grant", subdomain);

    let mut url = Url::parse(&permission_grant_url)?;

    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair(UrlParams::REDIRECT_URL, game_url.as_str());
        pairs.append_pair(UrlParams::LOCAL_ORIGIN, local_origin);
    }

    Ok(url)
}
