//! Link extraction and classification from HTML documents.

use std::borrow::Cow;

use url::{Position, Url};

use crate::types::{LinkInfo, LinkType};

use super::raw_text::MaskedHtml;
use super::real_tags::{RealTags, write_start_tag};
use super::selectors::SEL_A_HREF;
use super::{fetchable_address, get_attr, get_url_attr, has_link_qualifier};

/// Document file extensions used for link classification.
static DOCUMENT_EXTENSIONS: &[&str] = &[
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".odt", ".ods", ".odp", ".rtf", ".csv", ".txt", ".zip",
    ".tar", ".gz", ".rar",
];

/// Classify a link as internal, external, anchor, or document. `resolved` is the address `href`
/// names on the page at `document_url`.
///
/// ~keep A link is an anchor when it has a fragment and names the page it is on: the URL
/// ~keep standard compares the two addresses without their fragments. The written `href` cannot
/// ~keep decide it. `#part` names another page when a `<base>` moves the base address, and
/// ~keep `page.html#part` names the same page when the page is `page.html`.
pub(crate) fn classify_link(href: &str, resolved: &Url, document_url: &Url) -> LinkType {
    if resolved.fragment().is_some() && resolved[..Position::AfterQuery] == document_url[..Position::AfterQuery] {
        return LinkType::Anchor;
    }

    let lower = href.to_lowercase();
    for ext in DOCUMENT_EXTENSIONS {
        if lower.ends_with(ext) {
            return LinkType::Document;
        }
    }

    if resolved.host_str() != document_url.host_str() {
        return LinkType::External;
    }
    LinkType::Internal
}

/// The URL a document's relative references resolve against: `base_href`, the decoded `href` of
/// its first `<base>` in tree order that has one (see [`MaskedHtml::base_href`]), joined to the
/// document URL, or the document URL itself when there is none, it does not parse, or its scheme
/// is `data` or `javascript` (the HTML frozen base URL steps).
pub(crate) fn effective_base_url(base_href: Option<&str>, document_url: &Url) -> Url {
    base_href
        // ~keep A `<base href>` is often site-relative (e.g. "/en/"); resolve it against
        // the document URL instead of requiring it to already be absolute.
        .and_then(|href| crate::net::userinfo::resolve(document_url, href))
        .filter(|base| !matches!(base.scheme(), "data" | "javascript"))
        .unwrap_or_else(|| document_url.clone())
}

/// Extract all links from `page`, the document at `document_url`. Each link resolves against
/// the document's base URL from [`effective_base_url`] and is classified against `document_url`.
///
/// ~keep The page is parsed by tl a second time, on its own, rather than reusing the caller's
/// ~keep already-parsed document: tl and html5ever's tokenizer can read a malformed `<a>` tag's
/// ~keep boundary differently (#294), so every real `<a>` start tag is rewritten into
/// ~keep unambiguous form, as html5ever read it while masking the page, before tl parses it for
/// ~keep the link's text, `rel` and qualifiers. A well-formed `<a>` tag rewrites to itself, so
/// ~keep this changes nothing for one.
pub(crate) fn extract_links(page: &MaskedHtml<'_>, document_url: &Url) -> Vec<LinkInfo> {
    let base_url = &effective_base_url(page.base_href.as_deref(), document_url);
    let canonical = canonicalize_anchor_tags(&page.text, &page.url_tags);
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
            // ~keep The scheme comes from the parsed URL, not a prefix test: the parser matches it
            // ~keep in any case and drops tabs and newlines, so `java&#9;script:` is `javascript:`.
            // ~keep Only `http` and `https` are kept: the crawler can fetch neither `mailto:`,
            // ~keep `tel:` nor the inline schemes, and no more than these two can name a `file:`,
            // ~keep `blob:` or other address the crawler cannot reach either.
            let Some(resolved_url) = fetchable_address(href, base_url) else {
                continue;
            };

            let link_type = classify_link(href, &resolved_url, document_url);
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

/// Rewrite every real `<a>` start tag of `tags` in `html` into unambiguous form: its name lower-cased, each
/// attribute once in source order, double-quoted, as html5ever's tokenizer reads it.
///
/// A malformed tag such as `<a href=b ="x>one</a><a href="y">two</a>` makes tl read a different
/// tag boundary, or none at all, than a real HTML parser does, so the rewritten tag is what tl
/// parses instead. Every byte outside a rewritten tag is kept.
fn canonicalize_anchor_tags<'h>(html: &'h str, tags: &RealTags) -> Cow<'h, str> {
    let mut out = String::new();
    let mut cursor = 0;
    let mut written = String::new();
    for tag in tags.iter() {
        if tag.name != "a" {
            continue;
        }
        written.clear();
        write_start_tag(&mut written, &tag, &[]);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(html: &str, document_url: &str) -> Vec<LinkInfo> {
        let page = crate::html::mask_raw_text_markup(html);
        let document_url = Url::parse(document_url).expect("valid document URL");
        extract_links(&page, &document_url)
    }

    /// The type of each link of `html` served at `document_url`, with the resolved address.
    fn link_types(html: &str, document_url: &str) -> Vec<(String, LinkType)> {
        extract(html, document_url)
            .into_iter()
            .map(|link| (link.url, link.link_type))
            .collect()
    }

    /// The base URL of `html` served at `document_url`.
    fn base_of(html: &str, document_url: &Url) -> String {
        let page = crate::html::mask_raw_text_markup(html);
        effective_base_url(page.base_href.as_deref(), document_url).into()
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
    fn a_decoded_quote_in_the_value_does_not_break_the_rewritten_tag() {
        // ~keep `&quot;` decodes to a literal `"`; writing it back unencoded into the new double
        // ~keep quotes would end the value there and truncate the address, losing what follows.
        let html = r#"<a href="a&quot;b">x</a>"#;
        let links = extract(html, "https://example.com/");
        assert_eq!(links.len(), 1, "expected exactly one link, got {links:?}");
        assert_eq!(links[0].url, "https://example.com/a%22b");
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
    fn a_malformed_attribute_name_is_left_out_of_the_rewritten_tag() {
        // ~keep html5ever's attribute-name state treats a `"` as a parse error but still appends
        // ~keep it to the name (only whitespace, `/`, `>` and `=` end the name), so `x"y` is a
        // ~keep real attribute name here. Writing it back unfiltered would put an unescaped `"`
        // ~keep inside the tag, which reopens exactly the tag-boundary ambiguity this rewrite
        // ~keep exists to remove: tl would read the embedded `"` as starting a new attribute value.
        let html = r#"<a x"y="1" href="/ok">z</a>"#;
        let page = crate::html::mask_raw_text_markup(html);
        let tag = page.url_tags.iter().find(|tag| tag.name == "a").expect("one tag");
        assert_eq!(
            tag.attrs.len(),
            2,
            "expected html5ever to read two attributes, got {:?}",
            tag.attrs
                .iter()
                .map(|a| (&*a.name, a.value.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            &*tag.attrs[0].name, "x\"y",
            "the malformed name html5ever actually read"
        );

        let mut out = String::new();
        write_start_tag(&mut out, &tag, &[]);
        assert_eq!(
            out, r#"<a href="/ok">"#,
            "the malformed attribute name must not reach the rewritten tag"
        );
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
        // ~keep A browser maps 128-159 through Windows-1252, so &#150; is an en dash, not U+0096.
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
    fn schemes_the_crawler_cannot_fetch_are_skipped() {
        let html = concat!(
            r#"<a href="file:///etc/passwd">a</a><a href="blob:https://example.com/x">b</a>"#,
            r#"<a href="ftp://example.com/f">c</a><a href="ok.html">ok</a>"#,
        );
        let links = extract(html, "https://example.com/dir/page");
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, ["https://example.com/dir/ok.html"]);
    }

    #[test]
    fn a_link_to_the_page_itself_with_a_fragment_is_an_anchor_however_it_is_written() {
        let html = concat!(
            r##"<a href="#section-1">a</a><a href="#">b</a><a href="page?q=1#x">c</a>"##,
            r##"<a href="/dir/page?q=1#y">d</a><a href="https://EXAMPLE.com:443/dir/./page?q=1#z">e</a>"##,
        );
        let types: Vec<LinkType> = link_types(html, "https://example.com/dir/page?q=1")
            .into_iter()
            .map(|(_, link_type)| link_type)
            .collect();
        assert_eq!(types, vec![LinkType::Anchor; 5]);
    }

    #[test]
    fn a_link_to_the_page_itself_without_a_fragment_is_not_an_anchor() {
        assert_eq!(
            link_types(
                r#"<a href="page">a</a><a href="/dir/page">b</a>"#,
                "https://example.com/dir/page#top"
            ),
            [
                ("https://example.com/dir/page".to_owned(), LinkType::Internal),
                ("https://example.com/dir/page".to_owned(), LinkType::Internal),
            ]
        );
    }

    #[test]
    fn a_link_with_a_fragment_to_another_address_is_not_an_anchor() {
        assert_eq!(
            link_types(
                r##"<a href="other#x">a</a><a href="page?q=2#x">b</a><a href="page/#x">c</a><a href="Page#x">d</a>"##,
                "https://example.com/dir/page"
            ),
            [
                ("https://example.com/dir/other#x".to_owned(), LinkType::Internal),
                ("https://example.com/dir/page?q=2#x".to_owned(), LinkType::Internal),
                ("https://example.com/dir/page/#x".to_owned(), LinkType::Internal),
                ("https://example.com/dir/Page#x".to_owned(), LinkType::Internal),
            ]
        );
    }

    #[test]
    fn a_fragment_only_href_on_a_page_with_a_base_names_the_base_address() {
        let html = r##"<base href="/other/"><a href="#part">a</a><a href="/dir/page#top">b</a>"##;
        assert_eq!(
            link_types(html, "https://example.com/dir/page"),
            [
                ("https://example.com/other/#part".to_owned(), LinkType::Internal),
                ("https://example.com/dir/page#top".to_owned(), LinkType::Anchor),
            ]
        );
    }

    #[test]
    fn a_base_on_another_host_does_not_make_that_host_internal() {
        let html = concat!(
            r#"<base href="https://cdn.example/assets/">"#,
            r##"<a href="x.html">a</a><a href="https://example.com/home">b</a><a href="#part">c</a>"##,
        );
        assert_eq!(
            link_types(html, "https://example.com/page"),
            [
                ("https://cdn.example/assets/x.html".to_owned(), LinkType::External),
                ("https://example.com/home".to_owned(), LinkType::Internal),
                ("https://cdn.example/assets/#part".to_owned(), LinkType::External),
            ]
        );
    }

    #[test]
    fn classifies_pdf_extension_as_document() {
        assert_eq!(
            link_types(r#"<a href="/files/report.pdf">a</a>"#, "https://example.com/page"),
            [("https://example.com/files/report.pdf".to_owned(), LinkType::Document)],
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
            assert_eq!(base_of(&html, &document_url), expected, "for base {href:?}");
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
        let document = Url::parse("https://example.com/").expect("valid base URL");
        assert_eq!(base_of(html, &document), "http://example.com/dir/");
    }
}
