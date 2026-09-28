//! Link extraction and classification from HTML documents.

use std::borrow::Cow;

use tl::VDom;
use url::Url;

use crate::types::{LinkInfo, LinkType};

use super::real_tags::{self, StartTag};
use super::selectors::{SEL_A_HREF, SEL_BASE_HREF};
use super::{INLINE_SCHEMES, get_attr, get_url_attr, has_link_qualifier};

/// Document file extensions used for link classification.
static DOCUMENT_EXTENSIONS: &[&str] = &[
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".odt", ".ods", ".odp", ".rtf", ".csv", ".txt", ".zip",
    ".tar", ".gz", ".rar",
];

/// Classify a link as internal, external, anchor, or document.
pub(crate) fn classify_link(href: &str, base_url: &Url) -> LinkType {
    if href.starts_with('#') {
        return LinkType::Anchor;
    }

    let lower = href.to_lowercase();
    for ext in DOCUMENT_EXTENSIONS {
        if lower.ends_with(ext) {
            return LinkType::Document;
        }
    }

    if let Ok(resolved) = base_url.join(href)
        && resolved.host_str() != base_url.host_str()
    {
        return LinkType::External;
    }
    LinkType::Internal
}

/// The URL a document's relative references resolve against: the `href` of its first `<base>`
/// that has one, decoded and joined to the document URL, or the document URL itself when there is
/// none, it does not parse, or its scheme is `data` or `javascript` (the HTML frozen base URL steps).
pub(crate) fn effective_base_url(dom: &VDom<'_>, document_url: &Url) -> Url {
    let parser = dom.parser();
    dom.query_selector(SEL_BASE_HREF)
        .and_then(|mut iter| iter.next())
        .and_then(|h| h.get(parser))
        .and_then(|n| n.as_tag())
        .map(|tag| get_attr(tag, "href").unwrap_or_default())
        // ~keep A `<base href>` is often site-relative (e.g. "/en/"); resolve it against
        // the document URL instead of requiring it to already be absolute.
        .and_then(|href| crate::net::userinfo::resolve(document_url, &href))
        .filter(|base| !matches!(base.scheme(), "data" | "javascript"))
        .unwrap_or_else(|| document_url.clone())
}

/// Extract all links from `html`, resolved against `base_url`, the document's base URL from
/// [`effective_base_url`].
///
/// ~keep `html` is read a second time, on its own, rather than reusing the caller's already-parsed
/// ~keep document: tl and html5ever's tokenizer can read a malformed `<a>` tag's boundary
/// ~keep differently (#294), so every real `<a>` start tag is rewritten into unambiguous form,
/// ~keep as html5ever reads it, before tl parses it for the link's text, `rel` and qualifiers.
/// ~keep A well-formed `<a>` tag rewrites to itself, so this changes nothing for one.
pub(crate) fn extract_links(html: &str, base_url: &Url) -> Vec<LinkInfo> {
    let canonical = canonicalize_anchor_tags(html);
    let Ok(dom) = super::parse_html(&canonical) else {
        return Vec::new();
    };
    let parser = dom.parser();
    let mut links = Vec::new();

    if let Some(iter) = dom.query_selector(SEL_A_HREF) {
        for handle in iter {
            let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                continue;
            };

            let Some(href) = get_url_attr(tag, "href") else {
                continue;
            };
            let href = href.as_ref();

            // ~keep `Url::join` already resolves protocol-relative ("//host/path") references
            // per the WHATWG URL spec, so no special-casing is needed here.
            let Some(resolved_url) = crate::net::userinfo::resolve(base_url, href) else {
                continue;
            };
            // ~keep The scheme comes from the parsed URL, not a prefix test: the parser matches it
            // ~keep in any case and drops tabs and newlines, so `java&#9;script:` is `javascript:`.
            if matches!(resolved_url.scheme(), "mailto" | "tel") || INLINE_SCHEMES.contains(&resolved_url.scheme()) {
                continue;
            }

            let link_type = classify_link(href, base_url);
            let rel = get_attr(tag, "rel").map(Cow::into_owned);
            let nofollow = has_link_qualifier(tag, "nofollow");
            let text = tag.inner_text(parser).trim().to_owned();

            links.push(LinkInfo {
                url: resolved_url.into(),
                text,
                link_type,
                rel,
                nofollow,
            });
        }
    }
    links
}

/// Rewrite every real `<a>` start tag in `html` into unambiguous form: its name lower-cased, each
/// attribute once in source order, double-quoted, as html5ever's tokenizer reads it.
///
/// A malformed tag such as `<a href=b ="x>one</a><a href="y">two</a>` makes tl read a different
/// tag boundary, or none at all, than a real HTML parser does, so the rewritten tag is what tl
/// parses instead. Every byte outside a rewritten tag is kept.
fn canonicalize_anchor_tags(html: &str) -> Cow<'_, str> {
    let tags = real_tags::scan(html, |name| name == "a");
    let mut out = String::new();
    let mut cursor = 0;
    let mut written = String::new();
    for tag in tags.iter() {
        written.clear();
        write_anchor_tag(&mut written, &tag);
        if html[tag.span.clone()] != written {
            out.push_str(&html[cursor..tag.span.start]);
            out.push_str(&written);
            cursor = tag.span.end;
        }
    }
    if cursor == 0 {
        return Cow::Borrowed(html);
    }
    out.push_str(&html[cursor..]);
    Cow::Owned(out)
}

/// Write the start tag `tag` into `out` as an HTML parser reads it: `<a`, each attribute once in
/// source order, double-quoted and encoded, its self-closing slash if it has one, then `>`.
///
/// ~keep An attribute name that is not ASCII letters, digits, `-`, `_` or `:` is left out: tl's
/// ~keep own attribute reader stops at the first `=` or quote in a name, so keeping such a name
/// ~keep here would reopen the exact ambiguity this rewrite removes.
fn write_anchor_tag(out: &mut String, tag: &StartTag<'_>) {
    out.push_str("<a");
    for attr in tag.attrs {
        let name = &*attr.name.local;
        if !is_plain_attr_name(name) {
            continue;
        }
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(&html_escape::encode_quoted_attribute(&attr.value));
        out.push('"');
    }
    if tag.self_closing {
        out.push('/');
    }
    out.push('>');
}

/// Whether an attribute `name` is only ASCII letters, digits, `-`, `_` and `:`.
fn is_plain_attr_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(html: &str, document_url: &str) -> Vec<LinkInfo> {
        let dom = crate::html::parse_html(html).expect("valid HTML");
        let document_url = Url::parse(document_url).expect("valid document URL");
        extract_links(html, &effective_base_url(&dom, &document_url))
    }

    /// The pre-fix behaviour: tl parses `html` directly, with no canonicalization pass first.
    fn extract_raw_tl_only(html: &str) -> Vec<String> {
        let dom = crate::html::parse_html(html).expect("valid HTML");
        let parser = dom.parser();
        let mut hrefs = Vec::new();
        if let Some(iter) = dom.query_selector(SEL_A_HREF) {
            for handle in iter {
                let Some(tag) = handle.get(parser).and_then(|n| n.as_tag()) else {
                    continue;
                };
                if let Some(href) = get_url_attr(tag, "href") {
                    hrefs.push(href.into_owned());
                }
            }
        }
        hrefs
    }

    #[test]
    fn finds_a_link_an_unterminated_quote_hid_from_the_raw_tl_parse() {
        // ~keep #294's reproduction (bank/triage-op4/report.md): the unterminated `href="broken`
        // ~keep makes tl read everything up to the next literal `"` -- the second link's own
        // ~keep opening quote -- as part of the first value, so tl finds no `<a>` at all here.
        // ~keep html5ever reads the same bytes the way a browser does: one `<a>` start tag ending
        // ~keep at the first real `>`, with a garbled `href` and "real" as its text. Extraction now
        // ~keep reports that one tag instead of silently dropping the whole line.
        let html = r#"<a href="broken>x</a> <a href="/rel/page.html">real</a>"#;
        assert_eq!(
            extract_raw_tl_only(html),
            Vec::<String>::new(),
            "control: tl alone still finds nothing here"
        );
        let links = extract(html, "https://example.com/");
        assert_eq!(links.len(), 1, "expected the one tag html5ever reads, got {links:?}");
        assert_eq!(links[0].text, "real");
    }

    #[test]
    fn a_stray_equals_and_quote_do_not_merge_two_links() {
        // ~keep The malformed-tag fixture from the open html5ever tag-scan stack (#292): a real
        // ~keep HTML parser ends the first `<a>` at the `>` right after `="x`, so the two links
        // ~keep keep their own addresses instead of the second's `href` extending the first's.
        let html = r#"<a href=b ="x>one</a><a href="y">two</a>"#;
        let links = extract(html, "https://example.com/dir/");
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/dir/b", "https://example.com/dir/y"]);
        assert_eq!(links[0].text, "one");
        assert_eq!(links[1].text, "two");
    }

    #[test]
    fn an_uppercase_a_tag_is_still_found() {
        let html = r#"<A HREF="/docs/x.html">y</A>"#;
        let links = extract(html, "https://example.com/");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert_eq!(links[0].url, "https://example.com/docs/x.html");
    }

    #[test]
    fn an_entity_encoded_address_still_resolves() {
        // ~keep Matches link_targets.rs's own numeric-reference test: a browser maps 128-159
        // ~keep through Windows-1252, so &#150; is an en dash, not U+0096.
        let html = r#"<a href="p&#150;q&amp;r=1">x</a>"#;
        let links = extract(html, "https://example.com/d/");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert_eq!(links[0].url, "https://example.com/d/p%E2%80%93q&r=1");
    }

    #[test]
    fn resolves_protocol_relative_urls_to_the_base_scheme() {
        let html = r#"<a href="//cdn.example/x.js">script</a>"#;
        let links = extract(html, "https://example.com/page");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert_eq!(
            links[0].url, "https://cdn.example/x.js",
            "protocol-relative URL should inherit the base scheme, got {}",
            links[0].url
        );
    }

    #[test]
    fn resolves_relative_base_href_against_the_document_url() {
        let html = r#"<base href="/en/"><a href="page.html">link</a>"#;
        let links = extract(html, "https://example.com/us/index.html");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert_eq!(
            links[0].url, "https://example.com/en/page.html",
            "relative base href should resolve against the document URL, got {}",
            links[0].url
        );
    }

    #[test]
    fn should_extract_link_when_href_attribute_is_uppercase() {
        let html = r#"<a HREF="/docs/guide.html">guide</a>"#;
        let links = extract(html, "https://example.com/page");
        assert_eq!(links.len(), 1, "uppercase HREF should still match, got {links:?}");
        assert_eq!(
            links[0].url, "https://example.com/docs/guide.html",
            "uppercase HREF should resolve like a lowercase one, got {}",
            links[0].url
        );
    }

    #[test]
    fn should_read_rel_when_attribute_name_is_mixed_case() {
        let html = r#"<a href="https://other.example/" ReL="nofollow">out</a>"#;
        let links = extract(html, "https://example.com/page");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert!(
            links[0].nofollow,
            "mixed-case ReL=\"nofollow\" should be honoured, got rel={:?}",
            links[0].rel
        );
    }

    #[test]
    fn an_encoded_script_address_is_skipped_like_a_plain_one() {
        let html = r#"<a href="&#106;avascript&#58;alert(1)">x</a><a href="ok.html">ok</a>"#;
        let links = extract(html, "https://example.com/dir/page");
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/dir/ok.html"]);
    }

    #[test]
    fn a_skipped_scheme_is_read_as_the_url_parser_reads_it() {
        let html = r#"<a href="JavaScript:alert(1)">a</a><a href="&#74;avascript:alert(1)">b</a>
            <a href="java&#9;script:alert(1)">c</a><a href="MAILTO:x@example.com">d</a>
            <a href="Tel:+1">e</a><a href="&#68;ata:text/html,x">f</a><a href="ok.html">ok</a>"#;
        let links = extract(html, "https://example.com/dir/page");
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/dir/ok.html"]);
    }

    #[test]
    fn classifies_fragment_only_href_as_anchor() {
        let base_url = Url::parse("https://example.com/page").expect("valid base URL");
        assert_eq!(
            classify_link("#section-1", &base_url),
            LinkType::Anchor,
            "a fragment-only href should classify as Anchor"
        );
    }

    #[test]
    fn classifies_pdf_extension_as_document() {
        let base_url = Url::parse("https://example.com/page").expect("valid base URL");
        assert_eq!(
            classify_link("/files/report.pdf", &base_url),
            LinkType::Document,
            "a PDF href should classify as Document"
        );
    }

    #[test]
    fn absolute_base_href_still_resolves_correctly() {
        let html = r#"<base href="https://cdn.example/assets/"><a href="img.png">img</a>"#;
        let links = extract(html, "https://example.com/page");
        assert_eq!(
            links[0].url, "https://cdn.example/assets/img.png",
            "absolute base href should still resolve relative hrefs, got {}",
            links[0].url
        );
    }

    #[test]
    fn a_data_or_javascript_base_falls_back_to_the_document_url() {
        let document_url = Url::parse("https://example.com/dir/page.html").expect("valid document URL");
        for (href, expected) in [
            (" DATA:text/html,x ", "https://example.com/dir/page.html"),
            ("\t JavaScript:alert(1)// \n", "https://example.com/dir/page.html"),
            ("JAVASCRIPT://example.org/", "https://example.com/dir/page.html"),
            ("/other/", "https://example.com/other/"),
            ("https://cdn.example/assets/", "https://cdn.example/assets/"),
        ] {
            let html = format!(r#"<base href="{href}">"#);
            let dom = crate::html::parse_html(&html).expect("valid HTML");
            assert_eq!(
                effective_base_url(&dom, &document_url).as_str(),
                expected,
                "for base {href:?}"
            );
        }
    }

    #[test]
    fn a_link_whose_href_does_not_resolve_is_dropped() {
        let links = extract(
            r#"<a href="http://[not-an-address/">broken</a><a href="/ok">ok</a>"#,
            "https://example.com/page",
        );
        assert_eq!(
            links.iter().map(|link| link.url.as_str()).collect::<Vec<_>>(),
            ["https://example.com/ok"],
            "only the href that resolves may be returned, got {links:?}"
        );
    }

    #[test]
    fn a_link_and_a_base_href_lose_their_userinfo() {
        let links = extract(
            r#"<a href="http://user:s3cret@example.com/a">a</a>"#,
            "https://example.com/page",
        );
        assert_eq!(links[0].url, "http://example.com/a");

        let html = r#"<base href="http://user:s3cret@example.com/dir/"><a href="page.html">link</a>"#;
        let dom = crate::html::parse_html(html).expect("valid HTML");
        let document = Url::parse("https://example.com/").expect("valid base URL");
        assert_eq!(effective_base_url(&dom, &document).as_str(), "http://example.com/dir/");
    }
}
