//! Shared HTML data extraction used by both scrape and crawl.

use tl::VDom;
use url::Url;

use crate::types::{FeedInfo, ImageInfo, JsonLdEntry, LinkInfo, PageMetadata};

use super::feeds::{extract_favicons, extract_feeds, extract_headings, extract_hreflangs};
use super::images::extract_images;
use super::json_ld::extract_json_ld;
use super::links::extract_links;
use super::metadata::extract_metadata;

/// All data extracted from an HTML document in a single pass.
pub(crate) struct HtmlExtraction {
    pub(crate) metadata: PageMetadata,
    pub(crate) links: Vec<LinkInfo>,
    pub(crate) images: Vec<ImageInfo>,
    pub(crate) feeds: Vec<FeedInfo>,
    pub(crate) json_ld: Vec<JsonLdEntry>,
}

/// Extract all structured data from a parsed HTML document.
///
/// When `is_html` is false, returns defaults for all fields.
/// When `include_extended` is true, also extracts hreflangs, favicons,
/// headings, and word count into the metadata.
pub(crate) fn extract_page_data(
    dom: &VDom<'_>,
    body: &str,
    base_url: &Url,
    is_html: bool,
    include_extended: bool,
) -> HtmlExtraction {
    if !is_html {
        return HtmlExtraction {
            metadata: PageMetadata::default(),
            links: Vec::new(),
            images: Vec::new(),
            feeds: Vec::new(),
            json_ld: Vec::new(),
        };
    }

    let mut metadata = extract_metadata(dom, body);

    if include_extended {
        let hreflangs = extract_hreflangs(dom);
        if !hreflangs.is_empty() {
            metadata.hreflangs = Some(hreflangs);
        }
        let favicons = extract_favicons(dom, base_url);
        if !favicons.is_empty() {
            metadata.favicons = Some(favicons);
        }
        let headings = extract_headings(dom);
        if !headings.is_empty() {
            metadata.headings = Some(headings);
        }
        metadata.word_count = Some(super::content::compute_word_count(dom));
    }

    let links = extract_links(dom, base_url);
    let images = extract_images(dom, base_url);
    let feeds = extract_feeds(dom, base_url);
    let json_ld = extract_json_ld(dom);

    HtmlExtraction {
        metadata,
        links,
        images,
        feeds,
        json_ld,
    }
}
