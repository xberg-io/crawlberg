//! Metadata extraction from HTML documents.

use std::borrow::Cow;

use tl::VDom;
use url::Url;

use crate::types::{ArticleMetadata, PageMetadata};

use super::selectors::{
    META_RE_CONTENT_NAME, META_RE_NAME_CONTENT, ROBOTS_META_NAME, SEL_HTML, SEL_LINK_REL, SEL_META, SEL_TITLE,
};
use super::{attr_eq, decode_attr_value, get_attr, get_url_attr, has_inline_scheme, has_rel, resolve_url};

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
/// canonical URL: it points at the page itself. Nor does one that resolves to an inline `data:` or
/// script address.
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
            .and_then(|tag| get_url_attr(tag, "href"))
            .map(|href| resolve_url(&href, base_url))
            .filter(|url| !has_inline_scheme(url))
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

/// The `content` of every robots meta tag whose directives address this crawler.
///
/// ~keep A `<meta name="...">` robots tag may be addressed either to every crawler, through the
/// generic `robots` name, or to one named crawler. A tag naming another crawler is not ours to
/// obey, so it is dropped here rather than folded in with everyone else's.
pub(crate) fn robots_meta_contents(dom: &VDom<'_>, user_agent: &str) -> Vec<String> {
    // ~keep HTML compares `name` without case, but only ASCII case (selectors.rs:20-22): a
    // ~keep Unicode fold would turn some non-ASCII letters into an ASCII one (U+212A KELVIN SIGN
    // ~keep folds to `k`) and let a page bind a crawler its markup never actually named.
    let ua_lower = user_agent.to_ascii_lowercase();
    let mut contents = Vec::new();
    super::query_tags(dom, SEL_META, |tag, _parser| {
        let Some(name) = get_attr(tag, "name") else {
            return;
        };
        let name_lower = name.trim_ascii().to_ascii_lowercase();
        if name_lower != ROBOTS_META_NAME && !crate::robots::product_token_addresses_us(&name_lower, &ua_lower) {
            return;
        }
        if let Some(content) = get_attr(tag, "content") {
            contents.push(content.into_owned());
        }
    });
    contents
}

/// The target of a refresh directive, a `<meta http-equiv="refresh">` `content` value or an HTTP
/// `Refresh` header value, read with the HTML "shared declarative refresh steps" and then cleaned by
/// [`clean_url`](super::clean_url).
///
/// Returns `None` when the value is no refresh (see [`parse_refresh`]), names no target, or names an
/// absolute address whose scheme is not `http` or `https` (`mailto:`, `javascript:`, `data:` and so
/// on), which the crawl cannot follow. A target that names the page itself is returned as written.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn refresh_target(value: &str) -> Option<Cow<'_, str>> {
    parse_refresh(value)?.target
}

/// A refresh directive read with the HTML "shared declarative refresh steps".
#[cfg(not(target_arch = "wasm32"))]
struct Refresh<'a> {
    /// Whole seconds before the refresh comes due; a fraction is ignored.
    delay: u64,
    /// The address to load, cleaned by [`clean_url`](super::clean_url), or `None` to stay on the page:
    /// the refresh names no target, or an absolute address whose scheme is not `http` or `https`. An
    /// address the URL parser rejects is kept as written.
    target: Option<Cow<'a, str>>,
}

/// `value` read as a refresh directive, or `None` when it does not start with a delay or its target
/// is a `javascript:` address, which the refresh steps ignore.
#[cfg(not(target_arch = "wasm32"))]
fn parse_refresh(value: &str) -> Option<Refresh<'_>> {
    let rest = value.trim_ascii_start();
    if !rest.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let delay = rest[..digits_end].bytes().fold(0_u64, |delay, digit| {
        delay.saturating_mul(10).saturating_add(u64::from(digit - b'0'))
    });
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    if !rest.is_empty() && !rest.starts_with(|c: char| matches!(c, ';' | ',') || c.is_ascii_whitespace()) {
        return None;
    }
    let rest = rest.trim_ascii_start();
    let rest = rest.strip_prefix([';', ',']).unwrap_or(rest).trim_ascii_start();
    let target = super::clean_url(Cow::Borrowed(refresh_url(rest)));
    if target
        .as_deref()
        .is_some_and(|target| super::has_scheme(target, "javascript"))
    {
        return None;
    }
    let target = target.filter(|target| match Url::parse(target) {
        Ok(absolute) => matches!(absolute.scheme(), "http" | "https"),
        Err(_) => true,
    });
    Some(Refresh { delay, target })
}

/// The address in what follows a refresh delay: after an optional `url=` label in any case, and
/// without a pair of matching quotes around it.
#[cfg(not(target_arch = "wasm32"))]
fn refresh_url(rest: &str) -> &str {
    let labelled = rest
        .get(..3)
        .filter(|label| label.eq_ignore_ascii_case("url"))
        .and_then(|_| rest[3..].trim_ascii_start().strip_prefix('='));
    match labelled {
        Some(value) => unquote_refresh_url(value.trim_ascii_start()),
        None => unquote_refresh_url(rest),
    }
}

/// `value` without a leading `'` or `"`, cut at the next matching quote when there is one.
#[cfg(not(target_arch = "wasm32"))]
fn unquote_refresh_url(value: &str) -> &str {
    let Some(quote) = value.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
        return value;
    };
    let quoted = &value[1..];
    quoted.find(quote).map_or(quoted, |end| &quoted[..end])
}

/// The target of the `<meta http-equiv="refresh">` a browser acts on, or `None` when there is none or
/// it keeps the page.
///
/// ~keep Chrome replaces a scheduled refresh with each later one whose delay is not longer, so the
/// ~keep shortest delay wins and the later tag wins a tie; a refresh that keeps the page (no target,
/// ~keep or a scheme the crawl cannot follow) takes part like any other (#279).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn detect_meta_refresh(dom: &VDom<'_>) -> Option<String> {
    let parser = dom.parser();
    let iter = dom.query_selector(SEL_META)?;
    let mut chosen: Option<(u64, Option<String>)> = None;
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
        if let Some(refresh) = parse_refresh(&content)
            && chosen.as_ref().is_none_or(|(delay, _)| refresh.delay <= *delay)
        {
            chosen = Some((refresh.delay, refresh.target.map(Cow::into_owned)));
        }
    }
    chosen?.1
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

    fn robots_contents(html: &str, user_agent: &str) -> Vec<String> {
        let dom = crate::html::parse_html(html).expect("valid HTML");
        robots_meta_contents(&dom, user_agent)
    }

    #[test]
    fn the_generic_robots_meta_tag_is_read_for_every_user_agent() {
        assert_eq!(
            robots_contents(r#"<meta name="robots" content="NoIndex, NoFollow">"#, "crawlberg/1.0"),
            vec!["NoIndex, NoFollow".to_owned()]
        );
    }

    #[test]
    fn a_meta_tag_naming_our_product_token_is_read_and_another_crawlers_is_not() {
        assert_eq!(
            robots_contents(
                r#"<meta name="googlebot" content="noindex"><meta name="Crawlberg" content="nofollow">"#,
                "crawlberg/1.0",
            ),
            vec!["nofollow".to_owned()],
            "only the tag addressed to this crawler binds it"
        );
    }

    #[test]
    fn a_non_robots_meta_tag_is_not_read_as_a_directive() {
        assert!(robots_contents(r#"<meta name="description" content="hi">"#, "crawlberg/1.0").is_empty());
    }

    #[test]
    fn a_robots_name_is_trimmed_of_ascii_whitespace_only() {
        assert_eq!(
            robots_contents("<meta name=\" robots\t\" content=\"noindex\">", "crawlberg/1.0"),
            vec!["noindex".to_owned()]
        );
        assert!(
            robots_contents("<meta name=\"\u{a0}robots\" content=\"noindex\">", "crawlberg/1.0").is_empty(),
            "a no-break space is not ASCII whitespace, so the name is not `robots`"
        );
    }

    #[test]
    fn a_robots_name_is_folded_in_ascii_case_only() {
        // ~keep U+212A KELVIN SIGN lower-cases to `k` under Unicode rules, but HTML's `name`
        // ~keep comparison is ASCII-only case-insensitive (selectors.rs:20-22). A page using it
        // ~keep must not bind a crawler whose user agent starts with `k`, which a Unicode fold
        // ~keep would let it do.
        assert!(
            robots_contents("<meta name=\"\u{212a}bot\" content=\"noindex\">", "kbot/1.0").is_empty(),
            "a Unicode-only case fold must not let a KELVIN SIGN name match `kbot`"
        );
    }

    #[test]
    fn robots_and_refresh_names_match_in_any_case() {
        assert_eq!(
            robots_contents(r#"<meta name="Robots" content="noindex, nofollow">"#, "crawlberg/1.0"),
            vec!["noindex, nofollow".to_owned()]
        );
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
    fn meta_refresh_is_none_without_a_target_or_with_an_empty_target() {
        assert_eq!(meta_refresh(r#"<meta http-equiv="refresh" content="5">"#), None);
        assert_eq!(meta_refresh(r#"<meta http-equiv="refresh" content="0;url=   ">"#), None);
        assert_eq!(meta_refresh("<p>no meta at all</p>"), None);
    }

    #[test]
    fn meta_refresh_with_an_empty_target_loses_a_tie_to_a_later_tag() {
        assert_eq!(
            meta_refresh(
                r#"<meta http-equiv="refresh" content="0;url="><meta http-equiv="refresh" content="0;url=/second">"#
            ),
            Some("/second".to_owned())
        );
    }

    /// An unparseable target is a refresh like any other: a later tag with a longer delay does not
    /// replace it (#279).
    #[test]
    fn meta_refresh_keeps_an_unparseable_target_over_a_later_longer_one() {
        assert_eq!(
            meta_refresh(
                r#"<meta http-equiv="refresh" content="0; url=http://ex ample.com/"><meta http-equiv="refresh" content="3; url=/second">"#
            ),
            Some("http://ex ample.com/".to_owned())
        );
    }

    /// A value that is no refresh, or a `javascript:` one, takes no part in the choice (#279).
    #[test]
    fn meta_refresh_skips_a_tag_that_is_no_refresh() {
        for first in ["", "x; url=/first", "0; url=javascript:void(0)"] {
            let html = format!(
                r#"<meta http-equiv="refresh" content="{first}"><meta http-equiv="refresh" content="3; url=/second">"#
            );
            assert_eq!(meta_refresh(&html), Some("/second".to_owned()), "{first:?}");
        }
    }

    #[test]
    fn parse_refresh_reads_the_whole_seconds_of_the_delay() {
        let delay = |value: &str| parse_refresh(value).map(|refresh| refresh.delay);
        assert_eq!(delay("0; url=/next"), Some(0));
        assert_eq!(delay("1.5; url=/next"), Some(1));
        assert_eq!(delay(".5; url=/next"), Some(0));
        assert_eq!(delay(" 12"), Some(12));
        assert_eq!(delay("999999999999999999999999999999; url=/next"), Some(u64::MAX));
        assert_eq!(delay("x; url=/next"), None);
    }

    #[test]
    fn parse_refresh_ignores_a_javascript_target_in_any_case() {
        assert!(parse_refresh("0; url=javascript:void(0)").is_none());
        assert!(parse_refresh("0; url=JAVASCRIPT:void(0)").is_none());
        assert!(parse_refresh("0; url='\tjava\nscript:void(0)'").is_none());
        assert_eq!(
            parse_refresh("0; url=javascript-page.html")
                .and_then(|refresh| refresh.target)
                .as_deref(),
            Some("javascript-page.html")
        );
        let reload = parse_refresh("5").expect("a delay alone is a refresh");
        assert_eq!((reload.delay, reload.target), (5, None));
    }

    /// The HTML refresh parser drops one pair of matching quotes around the target (#208).
    #[test]
    fn meta_refresh_drops_a_pair_of_matching_quotes_around_the_target() {
        assert_eq!(
            meta_refresh(r#"<meta http-equiv="refresh" content="0; url='/next'">"#),
            Some("/next".to_owned())
        );
        assert_eq!(
            meta_refresh(r#"<meta http-equiv="refresh" content='0; url="/next"'>"#),
            Some("/next".to_owned())
        );
    }

    #[test]
    fn refresh_target_drops_one_pair_of_matching_quotes() {
        assert_eq!(refresh_target("0; url='/next'").as_deref(), Some("/next"));
        assert_eq!(refresh_target("0; url=\"/next\"").as_deref(), Some("/next"));
        assert_eq!(refresh_target("0; URL = '/next' trailing").as_deref(), Some("/next"));
        assert_eq!(refresh_target("0; url='/it\"s'").as_deref(), Some("/it\"s"));
        assert_eq!(refresh_target("0; '/next'").as_deref(), Some("/next"));
    }

    /// An opening quote with no closing one is dropped, and the rest of the value is the target.
    #[test]
    fn refresh_target_with_an_unterminated_quote_keeps_the_rest() {
        assert_eq!(refresh_target("0; url='/next").as_deref(), Some("/next"));
        assert_eq!(refresh_target("0; url='").as_deref(), None);
    }

    #[test]
    fn refresh_target_reads_a_target_without_a_url_label() {
        assert_eq!(refresh_target("0; /next").as_deref(), Some("/next"));
        assert_eq!(refresh_target("0,/next").as_deref(), Some("/next"));
        assert_eq!(refresh_target("0 /next").as_deref(), Some("/next"));
        assert_eq!(refresh_target(" 1.5; url=/next").as_deref(), Some("/next"));
        assert_eq!(refresh_target(".5;url=/next").as_deref(), Some("/next"));
    }

    /// A `u` that does not start a whole `url=` label is part of the address, quotes included.
    #[test]
    fn refresh_target_keeps_a_partial_url_label_as_the_address() {
        assert_eq!(refresh_target("0; ur=/next").as_deref(), Some("ur=/next"));
        assert_eq!(refresh_target("0; url /next").as_deref(), Some("url /next"));
        assert_eq!(refresh_target("0; u'/next'").as_deref(), Some("u'/next'"));
    }

    /// A `url=` inside the address is part of the address, not a second label.
    #[test]
    fn refresh_target_takes_the_first_address_not_a_url_label_inside_it() {
        assert_eq!(
            refresh_target("0; https://example.com/?url=/elsewhere").as_deref(),
            Some("https://example.com/?url=/elsewhere")
        );
        assert_eq!(
            refresh_target("0; url=/go?url=/elsewhere").as_deref(),
            Some("/go?url=/elsewhere")
        );
    }

    /// A value that does not start with a delay, or has something other than a separator after
    /// it, is not a refresh, however it names a target.
    #[test]
    fn refresh_target_is_none_for_a_value_that_is_not_a_refresh() {
        assert_eq!(refresh_target("url=/next"), None);
        assert_eq!(refresh_target("; url=/next"), None);
        assert_eq!(refresh_target(" ,/next"), None);
        assert_eq!(refresh_target("-1; url=/next"), None);
        assert_eq!(refresh_target("0x; url=/next"), None);
        assert_eq!(refresh_target(""), None);
    }

    /// A refresh with no target, or a blank one, reloads the page itself, which is no new address.
    #[test]
    fn refresh_target_is_none_for_a_refresh_of_the_page_itself() {
        assert_eq!(refresh_target("5"), None);
        assert_eq!(refresh_target("5; url="), None);
    }

    /// The target is cleaned by the URL rule: a no-break space stays, C0 controls and spaces go.
    #[test]
    fn refresh_target_is_cleaned_by_the_url_rule() {
        assert_eq!(refresh_target("0; url= /next\u{A0}\n").as_deref(), Some("/next\u{A0}"));
        assert_eq!(refresh_target("0; url='\u{1}/next '").as_deref(), Some("/next"));
    }

    /// Whitespace may come between the delay and the `;` or `,` separator.
    #[test]
    fn refresh_target_skips_whitespace_before_the_separator() {
        assert_eq!(refresh_target("0 ; url=/next").as_deref(), Some("/next"));
        assert_eq!(refresh_target("0\t,/next").as_deref(), Some("/next"));
    }

    /// The quoted address ends at the first matching quote, as the HTML refresh steps say.
    #[test]
    fn refresh_target_cuts_at_the_first_matching_quote() {
        assert_eq!(refresh_target("0; url='/a'b'").as_deref(), Some("/a"));
        assert_eq!(refresh_target("0; url=\"/a\"b\"").as_deref(), Some("/a"));
    }

    /// An absolute address with a scheme other than `http` or `https` is no target the crawl can
    /// follow; a relative, scheme-relative or web address still is.
    #[test]
    fn refresh_target_is_none_for_a_scheme_the_crawl_cannot_fetch() {
        for value in [
            "0; mailto:a@example.com",
            "0; url=javascript:void(0)",
            "0; url='JavaScript:alert(1)'",
            "0; tel:+15550100",
            "0; vbscript:x",
            "0; data:text/html,hi",
            "0; about:blank",
            "0; url:/next",
            "0; url=ja\tvascript:x",
        ] {
            assert_eq!(refresh_target(value), None, "{value:?}");
        }
        assert_eq!(
            refresh_target("0; HTTPS://example.com/x").as_deref(),
            Some("HTTPS://example.com/x")
        );
        assert_eq!(
            refresh_target("0; http://example.com/x").as_deref(),
            Some("http://example.com/x")
        );
        assert_eq!(refresh_target("0; //example.com/x").as_deref(), Some("//example.com/x"));
        assert_eq!(refresh_target("0; /mailto:a").as_deref(), Some("/mailto:a"));
    }
}
