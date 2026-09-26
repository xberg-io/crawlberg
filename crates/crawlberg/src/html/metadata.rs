//! Metadata extraction from HTML documents.

use std::borrow::Cow;

use tl::VDom;
use url::Url;

use crate::types::{ArticleMetadata, PageMetadata};

use super::selectors::{META_RE_CONTENT_NAME, META_RE_NAME_CONTENT, SEL_HTML, SEL_LINK_REL, SEL_META, SEL_TITLE};
use super::{attr_eq, decode_attr_value, get_attr, has_rel, resolve_url};

/// Extract metadata name-value pairs from raw HTML using regex (fallback for malformed HTML).
fn extract_metadata_from_raw(body: &str) -> Vec<(String, String)> {
    let mut results = Vec::new();
    for cap in META_RE_NAME_CONTENT.captures_iter(body) {
        results.push((
            cap[1].trim_ascii().to_lowercase(),
            decode_attr_value(&cap[2]).into_owned(),
        ));
    }
    for cap in META_RE_CONTENT_NAME.captures_iter(body) {
        results.push((
            cap[2].trim_ascii().to_lowercase(),
            decode_attr_value(&cap[1]).into_owned(),
        ));
    }
    results
}

/// Accumulator for the single pass over `<meta>` tags.
///
/// `article` and `og_locale_alternates` are only folded into the [`PageMetadata`] by
/// [`MetaAccumulator::finish`] if at least one corresponding tag was seen, so an absent
/// block stays `None` rather than becoming an empty one. ~keep
struct MetaAccumulator {
    metadata: PageMetadata,
    article: ArticleMetadata,
    has_article: bool,
    og_locale_alternates: Vec<String>,
}

impl MetaAccumulator {
    fn new(metadata: PageMetadata) -> Self {
        Self {
            metadata,
            article: ArticleMetadata::default(),
            has_article: false,
            og_locale_alternates: Vec::new(),
        }
    }

    fn apply(&mut self, name_lower: &str, content: String) {
        let md = &mut self.metadata;
        match name_lower {
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
            "og:locale:alternate" => self.og_locale_alternates.push(content),
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
            _ => self.apply_article(name_lower, content),
        }
    }

    fn apply_article(&mut self, name_lower: &str, content: String) {
        match name_lower {
            "article:published_time" => self.article.published_time = Some(content),
            "article:modified_time" => self.article.modified_time = Some(content),
            "article:author" => self.article.author = Some(content),
            "article:section" => self.article.section = Some(content),
            "article:tag" => self.article.tags.push(content),
            _ => return,
        }
        self.has_article = true;
    }

    fn finish(mut self) -> PageMetadata {
        if self.has_article {
            self.metadata.article = Some(self.article);
        }
        if !self.og_locale_alternates.is_empty() {
            self.metadata.og_locale_alternates = Some(self.og_locale_alternates);
        }
        self.metadata
    }
}

/// Fill the five fields the DOM pass may have missed from a regex scan of the raw body.
///
/// ~keep Regex fallback handles malformed HTML where DOM parsing misses meta tags.
fn apply_raw_meta_fallback(md: &mut PageMetadata, raw_body: &str) {
    if raw_body.is_empty() {
        return;
    }
    for (name, content) in extract_metadata_from_raw(raw_body) {
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

/// Extract metadata from a parsed HTML document, with regex fallback for malformed content.
///
/// The canonical URL resolves against `base_url`, the document's base URL. A blank `href` gives no
/// canonical URL: it points at the page itself.
pub(crate) fn extract_metadata(dom: &VDom<'_>, raw_body: &str, base_url: &Url) -> PageMetadata {
    let parser = dom.parser();

    let title = dom.query_selector(SEL_TITLE).and_then(|mut iter| {
        iter.next()
            .and_then(|h| h.get(parser))
            .map(|node| node.inner_text(parser).to_string())
    });

    let canonical_url = dom.query_selector(SEL_LINK_REL).and_then(|iter| {
        iter.filter_map(|h| h.get(parser).and_then(|node| node.as_tag()))
            .find(|tag| has_rel(tag, "canonical"))
            .and_then(|tag| get_attr(tag, "href"))
            .filter(|href| !href.trim_ascii().is_empty())
            .map(|href| resolve_url(&href, base_url))
    });

    let mut md = PageMetadata {
        title,
        canonical_url,
        ..Default::default()
    };

    if let Some(mut iter) = dom.query_selector(SEL_HTML)
        && let Some(tag) = iter.next().and_then(|h| h.get(parser)).and_then(|n| n.as_tag())
    {
        md.html_lang = get_attr(tag, "lang").map(Cow::into_owned);
        md.html_dir = get_attr(tag, "dir").map(Cow::into_owned);
    }

    let mut accumulator = MetaAccumulator::new(md);

    super::query_tags(dom, SEL_META, |tag, _parser| {
        let name = get_attr(tag, "name")
            .or_else(|| get_attr(tag, "property"))
            .unwrap_or_default();
        let content = get_attr(tag, "content").unwrap_or_default().into_owned();
        if content.is_empty() {
            return;
        }
        accumulator.apply(&name.trim_ascii().to_lowercase(), content);
    });

    let mut md = accumulator.finish();
    apply_raw_meta_fallback(&mut md, raw_body);
    md
}

/// Check whether a meta robots directive contains the given keyword.
fn has_robots_directive(dom: &VDom<'_>, directive: &str) -> bool {
    let parser = dom.parser();
    if let Some(iter) = dom.query_selector(SEL_META) {
        for handle in iter {
            if let Some(tag) = handle.get(parser).and_then(|n| n.as_tag())
                && attr_eq(tag, "name", "robots")
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

/// Marker introducing the redirect target inside a `refresh` directive's content.
#[cfg(not(target_arch = "wasm32"))]
const META_REFRESH_URL_MARKER: &[u8] = b"url=";

/// Find the byte offset just past a case-insensitive `url=` marker in `content`.
#[cfg(not(target_arch = "wasm32"))]
fn meta_refresh_target_offset(content: &str) -> Option<usize> {
    // ~keep Search original bytes case-insensitively so the extracted URL offset remains correct.
    content
        .as_bytes()
        .windows(META_REFRESH_URL_MARKER.len())
        .position(|w| {
            w.iter()
                .zip(META_REFRESH_URL_MARKER)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
        })
        .map(|pos| pos + META_REFRESH_URL_MARKER.len())
}

/// Detect a `<meta http-equiv="refresh">` tag and return the redirect target URL.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn detect_meta_refresh(dom: &VDom<'_>) -> Option<String> {
    let parser = dom.parser();
    let iter = dom.query_selector(SEL_META)?;
    for handle in iter {
        let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
            continue;
        };
        if !attr_eq(tag, "http-equiv", "refresh") {
            continue;
        }
        let Some(content) = get_attr(tag, "content") else {
            continue;
        };
        let Some(offset) = meta_refresh_target_offset(&content) else {
            continue;
        };
        let target = content[offset..].trim().to_owned();
        if !target.is_empty() {
            return Some(target);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_url() -> Url {
        Url::parse("https://example.com/dir/page.html").expect("valid URL")
    }

    fn parse(html: &str) -> PageMetadata {
        let dom = crate::html::parse_html(html).expect("valid HTML");
        extract_metadata(&dom, "", &document_url())
    }

    #[test]
    fn title_canonical_and_html_attributes_are_read() {
        let md = parse(
            r#"<html lang="en-GB" dir="rtl"><head><title>Hello</title>
               <link rel="canonical" href="https://example.com/x"></head></html>"#,
        );
        assert_eq!(md.title.as_deref(), Some("Hello"));
        assert_eq!(md.canonical_url.as_deref(), Some("https://example.com/x"));
        assert_eq!(md.html_lang.as_deref(), Some("en-GB"));
        assert_eq!(md.html_dir.as_deref(), Some("rtl"));
    }

    #[test]
    fn canonical_is_the_first_link_with_the_canonical_token_resolved_against_the_base() {
        let md = parse(
            r#"<link rel="stylesheet" href="s.css"><link rel="Canonical" href="c.html">
               <link rel="canonical" href="second.html">"#,
        );
        assert_eq!(md.canonical_url.as_deref(), Some("https://example.com/dir/c.html"));
    }

    #[test]
    fn plain_named_meta_tags_map_to_their_fields() {
        let md = parse(
            r#"<meta name="description" content="d"><meta name="keywords" content="k">
               <meta name="author" content="a"><meta name="viewport" content="v">
               <meta name="theme-color" content="dark"><meta name="generator" content="g">
               <meta name="robots" content="noindex">"#,
        );
        assert_eq!(md.description.as_deref(), Some("d"));
        assert_eq!(md.keywords.as_deref(), Some("k"));
        assert_eq!(md.author.as_deref(), Some("a"));
        assert_eq!(md.viewport.as_deref(), Some("v"));
        assert_eq!(md.theme_color.as_deref(), Some("dark"));
        assert_eq!(md.generator.as_deref(), Some("g"));
        assert_eq!(md.robots.as_deref(), Some("noindex"));
    }

    #[test]
    fn open_graph_twitter_and_dublin_core_tags_map_to_their_fields() {
        let md = parse(
            r#"<meta property="og:title" content="ot"><meta property="og:type" content="oy">
               <meta property="og:image" content="oi"><meta property="og:description" content="od">
               <meta property="og:url" content="ou"><meta property="og:site_name" content="os">
               <meta property="og:locale" content="ol"><meta property="og:video" content="ov">
               <meta property="og:audio" content="oa">
               <meta name="twitter:card" content="tc"><meta name="twitter:title" content="tt">
               <meta name="twitter:description" content="td"><meta name="twitter:image" content="ti">
               <meta name="twitter:site" content="ts"><meta name="twitter:creator" content="tr">
               <meta name="DC.title" content="dt"><meta name="dc.creator" content="dc">
               <meta name="dc.language" content="dl"><meta name="dc.rights" content="dr">"#,
        );
        assert_eq!(md.og_title.as_deref(), Some("ot"));
        assert_eq!(md.og_type.as_deref(), Some("oy"));
        assert_eq!(md.og_image.as_deref(), Some("oi"));
        assert_eq!(md.og_description.as_deref(), Some("od"));
        assert_eq!(md.og_url.as_deref(), Some("ou"));
        assert_eq!(md.og_site_name.as_deref(), Some("os"));
        assert_eq!(md.og_locale.as_deref(), Some("ol"));
        assert_eq!(md.og_video.as_deref(), Some("ov"));
        assert_eq!(md.og_audio.as_deref(), Some("oa"));
        assert_eq!(md.twitter_card.as_deref(), Some("tc"));
        assert_eq!(md.twitter_title.as_deref(), Some("tt"));
        assert_eq!(md.twitter_description.as_deref(), Some("td"));
        assert_eq!(md.twitter_image.as_deref(), Some("ti"));
        assert_eq!(md.twitter_site.as_deref(), Some("ts"));
        assert_eq!(md.twitter_creator.as_deref(), Some("tr"));
        assert_eq!(
            md.dc_title.as_deref(),
            Some("dt"),
            "meta names are matched case-insensitively"
        );
        assert_eq!(md.dc_creator.as_deref(), Some("dc"));
        assert_eq!(md.dc_language.as_deref(), Some("dl"));
        assert_eq!(md.dc_rights.as_deref(), Some("dr"));
    }

    #[test]
    fn article_tags_populate_the_article_block_and_accumulate_tags() {
        let md = parse(
            r#"<meta property="article:published_time" content="p">
               <meta property="article:modified_time" content="m">
               <meta property="article:author" content="au"><meta property="article:section" content="s">
               <meta property="article:tag" content="t1"><meta property="article:tag" content="t2">"#,
        );
        let article = md.article.expect("article block present");
        assert_eq!(article.published_time.as_deref(), Some("p"));
        assert_eq!(article.modified_time.as_deref(), Some("m"));
        assert_eq!(article.author.as_deref(), Some("au"));
        assert_eq!(article.section.as_deref(), Some("s"));
        assert_eq!(article.tags, vec!["t1".to_owned(), "t2".to_owned()]);
    }

    #[test]
    fn article_block_is_absent_without_any_article_tag() {
        assert!(parse(r#"<meta name="description" content="d">"#).article.is_none());
    }

    #[test]
    fn og_locale_alternates_accumulate_and_are_none_when_absent() {
        let md = parse(
            r#"<meta property="og:locale:alternate" content="fr_FR">
               <meta property="og:locale:alternate" content="de_DE">"#,
        );
        assert_eq!(
            md.og_locale_alternates,
            Some(vec!["fr_FR".to_owned(), "de_DE".to_owned()])
        );
        assert_eq!(
            parse(r#"<meta property="og:locale" content="x">"#).og_locale_alternates,
            None
        );
    }

    #[test]
    fn empty_content_is_ignored_and_last_duplicate_wins() {
        assert_eq!(parse(r#"<meta name="description" content="">"#).description, None);
        let md = parse(r#"<meta name="description" content="first"><meta name="description" content="second">"#);
        assert_eq!(md.description.as_deref(), Some("second"));
    }

    #[test]
    fn name_attribute_takes_precedence_over_property() {
        let md = parse(r#"<meta name="description" property="og:title" content="c">"#);
        assert_eq!(md.description.as_deref(), Some("c"));
        assert_eq!(md.og_title, None);
    }

    #[test]
    fn raw_body_fallback_fills_only_fields_the_dom_pass_left_empty() {
        let dom = crate::html::parse_html("").expect("valid HTML");
        let raw = concat!(
            r#"<meta name="description" content="rd">"#,
            r#"<meta content="rt" name="og:title">"#,
            r#"<meta name="og:description" content="rod">"#,
            r#"<meta name="twitter:title" content="rtt">"#,
            r#"<meta name="twitter:description" content="rtd">"#,
            r#"<meta name="keywords" content="rk">"#,
        );
        let md = extract_metadata(&dom, raw, &document_url());
        assert_eq!(md.description.as_deref(), Some("rd"));
        assert_eq!(
            md.og_title.as_deref(),
            Some("rt"),
            "content-before-name form is matched too"
        );
        assert_eq!(md.og_description.as_deref(), Some("rod"));
        assert_eq!(md.twitter_title.as_deref(), Some("rtt"));
        assert_eq!(md.twitter_description.as_deref(), Some("rtd"));
        assert_eq!(md.keywords, None, "only the five listed fields have a raw fallback");
    }

    #[test]
    fn meta_content_is_decoded_on_both_the_dom_and_the_raw_path() {
        let html = r#"<meta name="description" content="Tom &amp; Jerry">"#;
        let from_dom = extract_metadata(&crate::html::parse_html(html).expect("valid HTML"), "", &document_url());
        assert_eq!(from_dom.description.as_deref(), Some("Tom & Jerry"));

        let from_raw = extract_metadata(&crate::html::parse_html("").expect("valid HTML"), html, &document_url());
        assert_eq!(from_raw.description.as_deref(), Some("Tom & Jerry"));
    }

    #[test]
    fn raw_body_fallback_does_not_override_a_value_found_in_the_dom() {
        let html = r#"<meta name="description" content="from-dom">"#;
        let dom = crate::html::parse_html(html).expect("valid HTML");
        let md = extract_metadata(&dom, r#"<meta name="description" content="from-raw">"#, &document_url());
        assert_eq!(md.description.as_deref(), Some("from-dom"));
    }

    #[test]
    fn robots_directives_are_detected_case_insensitively() {
        let dom = crate::html::parse_html(r#"<meta name="robots" content="NoIndex, NoFollow">"#).expect("valid HTML");
        assert!(detect_noindex(&dom));
        assert!(detect_nofollow(&dom));

        let plain = crate::html::parse_html(r#"<meta name="robots" content="all">"#).expect("valid HTML");
        assert!(!detect_noindex(&plain));
        assert!(!detect_nofollow(&plain));
    }

    #[test]
    fn robots_and_refresh_names_match_in_any_case() {
        let dom = crate::html::parse_html(r#"<meta name="Robots" content="noindex, nofollow">"#).expect("valid HTML");
        assert!(detect_noindex(&dom));
        assert!(detect_nofollow(&dom));
        assert_eq!(
            meta_refresh(r#"<META HTTP-EQUIV="Refresh" CONTENT="0; url=/next">"#),
            Some("/next".to_owned())
        );
    }

    #[test]
    fn raw_body_fallback_reads_meta_tags_in_any_case() {
        let dom = crate::html::parse_html("").expect("valid HTML");
        let md = extract_metadata(
            &dom,
            r#"<META NAME="Description" CONTENT="rd"><Meta Content="rt" Name="OG:Title">"#,
            &document_url(),
        );
        assert_eq!(md.description.as_deref(), Some("rd"));
        assert_eq!(md.og_title.as_deref(), Some("rt"));
    }

    #[test]
    fn raw_body_fallback_trims_the_meta_name() {
        let dom = crate::html::parse_html("").expect("valid HTML");
        let md = extract_metadata(
            &dom,
            r#"<meta name=" description " content="rd"><meta content="rt" name=" og:title ">"#,
            &document_url(),
        );
        assert_eq!(md.description.as_deref(), Some("rd"));
        assert_eq!(md.og_title.as_deref(), Some("rt"));
    }

    fn meta_refresh(html: &str) -> Option<String> {
        let dom = crate::html::parse_html(html).expect("valid HTML");
        detect_meta_refresh(&dom)
    }

    #[test]
    fn meta_refresh_returns_the_trimmed_target_after_a_case_insensitive_url_marker() {
        assert_eq!(
            meta_refresh(r#"<meta http-equiv="refresh" content="0; URL= https://example.com/next ">"#),
            Some("https://example.com/next".to_owned())
        );
        assert_eq!(
            meta_refresh(r#"<meta http-equiv='refresh' content="5;url=/relative">"#),
            Some("/relative".to_owned())
        );
    }

    #[test]
    fn meta_refresh_is_none_without_a_url_marker_or_with_an_empty_target() {
        assert_eq!(meta_refresh(r#"<meta http-equiv="refresh" content="5">"#), None);
        assert_eq!(meta_refresh(r#"<meta http-equiv="refresh" content="0;url=   ">"#), None);
        assert_eq!(meta_refresh("<p>no meta at all</p>"), None);
    }

    #[test]
    fn meta_refresh_skips_an_empty_target_and_uses_a_later_tag() {
        assert_eq!(
            meta_refresh(
                r#"<meta http-equiv="refresh" content="0;url="><meta http-equiv="refresh" content="0;url=/second">"#
            ),
            Some("/second".to_owned())
        );
    }
}
