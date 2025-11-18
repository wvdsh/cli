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
    let full = format!("{}/play/{}", base, wavedash_config.game_slug);
    let mut url =
        Url::parse(&full).with_context(|| format!("Unable to parse website host {}", full))?;

    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair(UrlParams::GAME_SUBDOMAIN, &wavedash_config.game_slug);
        pairs.append_pair(UrlParams::GAME_BRANCH_SLUG, &wavedash_config.branch_slug);
        pairs.append_pair(UrlParams::GAME_CLOUD_ID, &wavedash_config.org_slug);
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

    Ok(url)
}
