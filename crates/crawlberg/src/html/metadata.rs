//! Metadata extraction from HTML documents.

use tl::VDom;

use crate::types::{ArticleMetadata, PageMetadata};

use super::get_attr;
#[cfg(not(target_arch = "wasm32"))]
use super::selectors::SEL_META_REFRESH;
use super::selectors::{
    META_RE_CONTENT_NAME, META_RE_NAME_CONTENT, SEL_CANONICAL, SEL_HTML, SEL_META, SEL_ROBOTS_META, SEL_TITLE,
};

/// Extract metadata name-value pairs from raw HTML using regex (fallback for malformed HTML).
fn extract_metadata_from_raw(body: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    for cap in META_RE_NAME_CONTENT.captures_iter(body) {
        results.push((cap[1].to_lowercase(), cap[2].to_owned()));
    }
    for cap in META_RE_CONTENT_NAME.captures_iter(body) {
        results.push((cap[2].to_lowercase(), cap[1].to_owned()));
    }
    results
}

/// Extract metadata from a parsed HTML document, with regex fallback for malformed content.
pub(crate) fn extract_metadata(dom: &VDom<'_>, raw_body: &str) -> PageMetadata {
    let parser = dom.parser();

    let title = dom.query_selector(SEL_TITLE).and_then(|mut iter| {
        iter.next()
            .and_then(|h| h.get(parser))
            .map(|node| node.inner_text(parser).to_string())
    });

    let canonical_url = dom.query_selector(SEL_CANONICAL).and_then(|mut iter| {
        iter.next()
            .and_then(|h| h.get(parser))
            .and_then(|node| node.as_tag())
            .and_then(|tag| get_attr(tag, "href").map(String::from))
    });

    let mut md = PageMetadata {
        title,
        canonical_url,
        ..Default::default()
    };

    if let Some(mut iter) = dom.query_selector(SEL_HTML)
        && let Some(tag) = iter.next().and_then(|h| h.get(parser)).and_then(|n| n.as_tag())
    {
        md.html_lang = get_attr(tag, "lang").map(String::from);
        md.html_dir = get_attr(tag, "dir").map(String::from);
    }

    let mut article = ArticleMetadata::default();
    let mut has_article = false;
    let mut og_locale_alternates: Vec<String> = Vec::new();

    super::query_tags(dom, SEL_META, |tag, _parser| {
        let name = get_attr(tag, "name")
            .or_else(|| get_attr(tag, "property"))
            .unwrap_or("");
        let content = get_attr(tag, "content").unwrap_or("").to_owned();
        if content.is_empty() {
            return;
        }

        let name_lower = name.to_lowercase();
        match name_lower.as_str() {
            "description" => md.description = Some(content),
            "keywords" => md.keywords = Some(content),
            "author" => md.author = Some(content),
            "viewport" => md.viewport = Some(content),
            "theme-color" => md.theme_color = Some(content),
            "generator" => md.generator = Some(content),
            "robots" => md.robots = Some(content),
            "og:title" => md.og_title = Some(content),
            "og:type" => md.og_type = Some(content),
            "og:image" => md.og_image = Some(content),
            "og:description" => md.og_description = Some(content),
            "og:url" => md.og_url = Some(content),
            "og:site_name" => md.og_site_name = Some(content),
            "og:locale" => md.og_locale = Some(content),
            "og:video" => md.og_video = Some(content),
            "og:audio" => md.og_audio = Some(content),
            "og:locale:alternate" => og_locale_alternates.push(content),
            "article:published_time" => {
                article.published_time = Some(content);
                has_article = true;
            }
            "article:modified_time" => {
                article.modified_time = Some(content);
                has_article = true;
            }
            "article:author" => {
                article.author = Some(content);
                has_article = true;
            }
            "article:section" => {
                article.section = Some(content);
                has_article = true;
            }
            "article:tag" => {
                article.tags.push(content);
                has_article = true;
            }
            "twitter:card" => md.twitter_card = Some(content),
            "twitter:title" => md.twitter_title = Some(content),
            "twitter:description" => md.twitter_description = Some(content),
            "twitter:image" => md.twitter_image = Some(content),
            "twitter:site" => md.twitter_site = Some(content),
            "twitter:creator" => md.twitter_creator = Some(content),
            "dc.title" => md.dc_title = Some(content),
            "dc.creator" => md.dc_creator = Some(content),
            "dc.subject" => md.dc_subject = Some(content),
            "dc.description" => md.dc_description = Some(content),
            "dc.publisher" => md.dc_publisher = Some(content),
            "dc.date" => md.dc_date = Some(content),
            "dc.type" => md.dc_type = Some(content),
            "dc.format" => md.dc_format = Some(content),
            "dc.identifier" => md.dc_identifier = Some(content),
            "dc.language" => md.dc_language = Some(content),
            "dc.rights" => md.dc_rights = Some(content),
            _ => {}
        }
    });

    if has_article {
        md.article = Some(article);
    }
    if !og_locale_alternates.is_empty() {
        md.og_locale_alternates = Some(og_locale_alternates);
    }

    // ~keep Regex fallback handles malformed HTML where DOM parsing misses meta tags.
    if !raw_body.is_empty() {
        let raw_meta = extract_metadata_from_raw(raw_body);
        for (name, content) in raw_meta {
            match name.as_str() {
                "description" if md.description.is_none() => md.description = Some(content),
                "og:title" if md.og_title.is_none() => md.og_title = Some(content),
                "og:description" if md.og_description.is_none() => {
                    md.og_description = Some(content);
                }
                "twitter:title" if md.twitter_title.is_none() => {
                    md.twitter_title = Some(content);
                }
                "twitter:description" if md.twitter_description.is_none() => {
                    md.twitter_description = Some(content);
                }
                _ => {}
            }
        }
    }

    md
}

/// Check whether a meta robots directive contains the given keyword.
fn has_robots_directive(dom: &VDom<'_>, directive: &str) -> bool {
    let parser = dom.parser();
    if let Some(iter) = dom.query_selector(SEL_ROBOTS_META) {
        for handle in iter {
            if let Some(tag) = handle.get(parser).and_then(|n| n.as_tag())
                && let Some(content) = get_attr(tag, "content")
                && content.to_lowercase().contains(directive)
            {
                return true;
            }
        }
    }
    false
}

/// Detect whether a page has a `noindex` robots directive in its meta tags.
pub(crate) fn detect_noindex(dom: &VDom<'_>) -> bool {
    has_robots_directive(dom, "noindex")
}

/// Detect whether a page has a `nofollow` robots directive in its meta tags.
pub(crate) fn detect_nofollow(dom: &VDom<'_>) -> bool {
    has_robots_directive(dom, "nofollow")
}

/// Detect a `<meta http-equiv="refresh">` tag and return the redirect target URL.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn detect_meta_refresh(dom: &VDom<'_>) -> Option<String> {
    let parser = dom.parser();
    if let Some(iter) = dom.query_selector(SEL_META_REFRESH) {
        for handle in iter {
            if let Some(tag) = handle.get(parser).and_then(|n| n.as_tag())
                && let Some(content) = get_attr(tag, "content")
            {
                // ~keep Search original bytes case-insensitively so the extracted URL offset remains correct.
                let bytes = content.as_bytes();
                let needle = b"url=";
                let pos = bytes
                    .windows(needle.len())
                    .position(|w| w.iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == *b));
                if let Some(pos) = pos {
                    let target = content[pos + 4..].trim().to_owned();
                    if !target.is_empty() {
                        return Some(target);
                    }
                }
            }
        }
    }
    None
}
