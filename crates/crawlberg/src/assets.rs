//! Asset discovery and downloading from HTML pages.

use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tl::VDom;
#[cfg(not(target_arch = "wasm32"))]
use tokio::sync::Semaphore;
use url::Url;

use crate::html::get_attr;
use crate::html::selectors::{SEL_IMG_SRC, SEL_LINK_CSS, SEL_SCRIPT_SRC};
use crate::http::http_fetch;
use crate::types::{AssetCategory, CrawlConfig, DownloadedAsset};

/// A reference to an asset discovered in an HTML page.
pub(crate) struct AssetRef {
    url: String,
    category: AssetCategory,
    html_tag: String,
}

/// Discover downloadable assets from a parsed HTML document.
pub(crate) fn discover_assets(dom: &VDom<'_>, base_url: &Url) -> Vec<AssetRef> {
    let parser = dom.parser();
    let mut assets = Vec::new();

    if let Some(iter) = dom.query_selector(SEL_LINK_CSS) {
        for handle in iter {
            if let Some(tag) = handle.get(parser).and_then(|n| n.as_tag())
                && let Some(href) = get_attr(tag, "href")
                && let Ok(url) = base_url.join(href)
            {
                assets.push(AssetRef {
                    url: url.to_string(),
                    category: AssetCategory::Stylesheet,
                    html_tag: "link".to_owned(),
                });
            }
        }
    }

    if let Some(iter) = dom.query_selector(SEL_SCRIPT_SRC) {
        for handle in iter {
            if let Some(tag) = handle.get(parser).and_then(|n| n.as_tag())
                && let Some(src) = get_attr(tag, "src")
                && let Ok(url) = base_url.join(src)
            {
                assets.push(AssetRef {
                    url: url.to_string(),
                    category: AssetCategory::Script,
                    html_tag: "script".to_owned(),
                });
            }
        }
    }

    if let Some(iter) = dom.query_selector(SEL_IMG_SRC) {
        for handle in iter {
            if let Some(tag) = handle.get(parser).and_then(|n| n.as_tag())
                && let Some(src) = get_attr(tag, "src")
                && !src.starts_with("data:")
                && let Ok(url) = base_url.join(src)
            {
                assets.push(AssetRef {
                    url: url.to_string(),
                    category: AssetCategory::Image,
                    html_tag: "img".to_owned(),
                });
            }
        }
    }

    assets
}

/// Download a single asset, returning `None` if the download fails or is filtered.
async fn download_single_asset(
    asset_ref: AssetRef,
    client: &reqwest::Client,
    max_asset_size: Option<usize>,
    config: &CrawlConfig,
) -> Option<DownloadedAsset> {
    let resp = match http_fetch(&asset_ref.url, config, &std::collections::HashMap::new(), client).await {
        Ok(r) => r,
        Err(_) => return None,
    };

    let bytes = resp.body_bytes;

    if let Some(max_size) = max_asset_size
        && bytes.len() > max_size
    {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash_bytes = hasher.finalize();
    let hash = hash_bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();

    Some(DownloadedAsset {
        url: asset_ref.url,
        content_hash: hash,
        mime_type: Some(resp.content_type),
        size: bytes.len(),
        asset_category: asset_ref.category,
        html_tag: Some(asset_ref.html_tag),
    })
}

/// Download discovered assets, applying config filters.
pub(crate) async fn download_assets(
    refs: Vec<AssetRef>,
    config: &CrawlConfig,
    client: &reqwest::Client,
) -> Vec<DownloadedAsset> {
    let mut seen_urls: HashSet<String> = HashSet::new();

    let unique_refs: Vec<AssetRef> = refs
        .into_iter()
        .filter(|asset_ref| {
            if !seen_urls.insert(asset_ref.url.clone()) {
                return false;
            }
            if !config.asset_types.is_empty() && !config.asset_types.contains(&asset_ref.category) {
                return false;
            }
            true
        })
        .collect();

    let max_asset_size = config.max_asset_size;

    #[cfg(not(target_arch = "wasm32"))]
    {
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent.unwrap_or(8)));
        let client = client.clone();
        let config = config.clone();

        let mut handles = Vec::with_capacity(unique_refs.len());
        for asset_ref in unique_refs {
            let permit = Arc::clone(&semaphore);
            let client = client.clone();
            let config = config.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit.acquire().await.ok()?;
                download_single_asset(asset_ref, &client, max_asset_size, &config).await
            }));
        }

        let mut downloaded = Vec::new();
        for handle in handles {
            if let Ok(Some(asset)) = handle.await {
                downloaded.push(asset);
            }
        }

        downloaded
    }

    #[cfg(target_arch = "wasm32")]
    {
        let mut downloaded = Vec::new();
        for asset_ref in unique_refs {
            if let Some(asset) = download_single_asset(asset_ref, client, max_asset_size, config).await {
                downloaded.push(asset);
            }
        }
        downloaded
    }
}
