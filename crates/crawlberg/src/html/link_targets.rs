//! Rewriting the relative addresses that the markdown converter renders into absolute URLs.

use std::borrow::Cow;
use std::ops::Range;

use tl::VDom;
use url::Url;

use super::links::effective_base_url;
use super::{clean_url, decode_attr_value};

/// How an attribute holds its address.
#[derive(Clone, Copy)]
enum Shape {
    /// One URL.
    Single,
    /// A `srcset`-style list of candidates, each a URL with an optional descriptor.
    Candidates,
}

/// Every element and attribute whose address html-to-markdown-rs writes into the markdown.
///
/// ~keep Mirrors the converter's handlers at the pinned version: links (`<a href>`), images
/// ~keep including the lazy-load fallbacks it reads when `src` is empty or a `data:` URI
/// ~keep (`handlers/image.rs`), embedded media (`media/embedded.rs`), blockquote citations
/// ~keep (`handlers/blockquote.rs`) and `<graphic>` (`handlers/graphic.rs`). Update this list
/// ~keep when the converter starts rendering another attribute.
const TARGETS: &[(&str, &[(&str, Shape)])] = &[
    ("a", &[("href", Shape::Single)]),
    (
        "img",
        &[
            ("src", Shape::Single),
            ("data-src", Shape::Single),
            ("data-lazy-src", Shape::Single),
            ("data-original", Shape::Single),
            ("data-srcset", Shape::Candidates),
            ("srcset", Shape::Candidates),
        ],
    ),
    ("iframe", &[("src", Shape::Single)]),
    ("video", &[("src", Shape::Single)]),
    ("audio", &[("src", Shape::Single)]),
    ("source", &[("src", Shape::Single)]),
    ("blockquote", &[("cite", Shape::Single)]),
    (
        "graphic",
        &[
            ("url", Shape::Single),
            ("href", Shape::Single),
            ("xlink:href", Shape::Single),
            ("src", Shape::Single),
        ],
    ),
];

/// Return `html` with every relative address in [`TARGETS`] resolved against the document's
/// base URL (its first `<base href>`, else `document_url`), using WHATWG URL parsing.
///
/// Each `<base href>` is rewritten to that resolved base, so the converter's front matter shows
/// the address the links resolve against.
///
/// Character references in a value are decoded before resolution, as a browser decodes them.
/// Absolute URLs of any scheme, fragment-only references and empty values are left as written.
/// Every byte outside a rewritten attribute value is kept.
pub(crate) fn resolve_link_targets<'h>(html: &'h str, document_url: &Url) -> Cow<'h, str> {
    let Ok(dom) = super::parse_html(html) else {
        return Cow::Borrowed(html);
    };
    let base = effective_base_url(&dom, document_url);
    let mut edits = collect_edits(&dom, html, &base);
    if edits.is_empty() {
        return Cow::Borrowed(html);
    }
    edits.sort_by_key(|(span, _)| span.start);

    let mut out = String::with_capacity(html.len() + edits.iter().map(|(_, v)| v.len()).sum::<usize>());
    let mut cursor = 0;
    for (span, value) in edits {
        out.push_str(&html[cursor..span.start]);
        out.push_str(&value);
        cursor = span.end;
    }
    out.push_str(&html[cursor..]);
    Cow::Owned(out)
}

/// The byte span of each attribute value that needs rewriting, with its encoded replacement.
///
/// ~keep One pass over tl's flat node list rather than a selector query per element name:
/// ~keep each query walks the whole tree, and there are eight element names to look for.
fn collect_edits(dom: &VDom<'_>, html: &str, base: &Url) -> Vec<(Range<usize>, String)> {
    let mut edits = Vec::new();
    let mut push_edit = |raw: &[u8], rewritten: &str| {
        if let Some(span) = span_within(html, raw) {
            let quoted = span.start > 0 && matches!(html.as_bytes()[span.start - 1], b'"' | b'\'');
            edits.push((span, encode_attribute_value(rewritten, quoted)));
        }
    };
    for tag in dom.nodes().iter().filter_map(|node| node.as_tag()) {
        let name = tag.name().as_bytes();
        if name == b"base" {
            // ~keep Every `<base href>`, not only the first: the converter's front matter
            // ~keep keeps the last one it meets, and only the first one counts in HTML.
            if let Some(raw) = borrowed_attr(tag, "href")
                && std::str::from_utf8(raw).is_ok_and(|v| decode_attr_value(v) != base.as_str())
            {
                push_edit(raw, base.as_str());
            }
            continue;
        }
        let Some((_, attributes)) = TARGETS.iter().find(|(element, _)| name == element.as_bytes()) else {
            continue;
        };
        for &(attr, shape) in *attributes {
            let Some(raw) = borrowed_attr(tag, attr) else {
                continue;
            };
            if let Some(rewritten) = std::str::from_utf8(raw)
                .ok()
                .and_then(|v| rewrite_value(v, shape, base))
            {
                push_edit(raw, &rewritten);
            }
        }
    }
    edits
}

/// The raw bytes of an attribute value, borrowed from the parsed input.
fn borrowed_attr<'h>(tag: &tl::HTMLTag<'h>, attr: &'static str) -> Option<&'h [u8]> {
    tag.attributes().get(attr).flatten().and_then(|v| v.as_bytes_borrowed())
}

/// The new, unencoded value for a raw attribute value, or `None` when nothing in it is relative.
fn rewrite_value(raw: &str, shape: Shape, base: &Url) -> Option<String> {
    let decoded = decode_attr_value(raw);
    match shape {
        Shape::Single => resolve_reference(&decoded, base),
        Shape::Candidates => resolve_candidates(&decoded, base),
    }
}

/// Resolve `reference` against `base` when it is a relative reference.
///
/// Returns `None` for anything a reader can already use as written: an absolute URL of any
/// scheme (`https:`, `mailto:`, `javascript:`, `data:`, ...), a fragment-only reference that
/// points into the same document, and a blank value.
fn resolve_reference(reference: &str, base: &Url) -> Option<String> {
    let reference = clean_url(Cow::Borrowed(reference))?;
    if reference.starts_with('#') {
        return None;
    }
    match Url::parse(&reference) {
        Err(url::ParseError::RelativeUrlWithoutBase) => base.join(&reference).ok().map(String::from),
        _ => None,
    }
}

/// Split a `srcset`-style list into its candidates, each a URL and its descriptor.
///
/// ~keep Follows the HTML "parse a srcset attribute" split: a candidate URL is a run of
/// ~keep non-ASCII-whitespace (so a `data:` URL's own comma stays inside it), trailing commas
/// ~keep end the candidate, and otherwise the descriptor runs to the next comma.
pub(super) fn srcset_candidates(list: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = list;
    std::iter::from_fn(move || {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if rest.is_empty() {
            return None;
        }
        let url_end = rest.find(|c: char| c.is_ascii_whitespace()).unwrap_or(rest.len());
        let (candidate_url, after_url) = rest.split_at(url_end);
        if candidate_url.ends_with(',') {
            rest = after_url;
            return Some((candidate_url.trim_end_matches(','), ""));
        }
        let descriptor_end = after_url.find(',').unwrap_or(after_url.len());
        rest = &after_url[descriptor_end..];
        Some((candidate_url, after_url[..descriptor_end].trim()))
    })
}

/// Resolve each candidate URL of a `srcset`-style list, keeping its descriptor.
fn resolve_candidates(list: &str, base: &Url) -> Option<String> {
    let mut candidates = Vec::new();
    let mut changed = false;
    for (candidate_url, descriptor) in srcset_candidates(list) {
        let resolved = resolve_reference(candidate_url, base);
        changed |= resolved.is_some();
        let candidate_url = resolved.unwrap_or_else(|| candidate_url.to_owned());
        candidates.push(if descriptor.is_empty() {
            candidate_url
        } else {
            format!("{candidate_url} {descriptor}")
        });
    }
    changed.then(|| candidates.join(", "))
}

/// Where `part` sits inside `whole`, when `part` is a slice borrowed from it.
///
/// ~keep tl parses without copying, so an attribute value is a sub-slice of the input; its
/// ~keep offset is the pointer difference, the same arithmetic tl's `HTMLTag::boundaries` uses.
fn span_within(whole: &str, part: &[u8]) -> Option<Range<usize>> {
    let start = (part.as_ptr() as usize).checked_sub(whole.as_ptr() as usize)?;
    let end = start.checked_add(part.len())?;
    (end <= whole.len() && whole.is_char_boundary(start) && whole.is_char_boundary(end)).then_some(start..end)
}

/// Encode a decoded value for writing back between the original quotes, or inside new
/// double quotes when the original value was unquoted.
///
/// ~keep Every `&` is encoded because the value was decoded before resolution, and the
/// ~keep converter decodes it again. A rebuilt candidate list contains spaces, so an unquoted
/// ~keep original gets quotes.
fn encode_attribute_value(value: &str, quoted: bool) -> String {
    let encoded = html_escape::encode_quoted_attribute(value);
    if quoted {
        encoded.into_owned()
    } else {
        format!("\"{encoded}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(html: &str, document_url: &str) -> String {
        let url = Url::parse(document_url).expect("valid document URL");
        resolve_link_targets(html, &url).into_owned()
    }

    #[test]
    fn rewrites_only_the_attribute_value() {
        let html = r#"<!doctype html><p class=x>a <a  title="t" href = 'rel/child.html' >c</a> &amp; b</p>"#;
        assert_eq!(
            resolve(html, "https://example.com/dir/page.html"),
            r#"<!doctype html><p class=x>a <a  title="t" href = 'https://example.com/dir/rel/child.html' >c</a> &amp; b</p>"#
        );
    }

    #[test]
    fn quotes_an_unquoted_value_it_rewrites() {
        assert_eq!(
            resolve("<a href=leaf.html>x</a>", "https://example.com/a/"),
            r#"<a href="https://example.com/a/leaf.html">x</a>"#
        );
    }

    #[test]
    fn matches_element_names_in_any_case_as_the_converter_does() {
        assert_eq!(
            resolve(r#"<A HREF="up.html">x</A>"#, "https://example.com/a/"),
            r#"<A HREF="https://example.com/a/up.html">x</A>"#
        );
    }

    #[test]
    fn returns_the_input_unchanged_when_nothing_is_relative() {
        let html = r##"<a href="https://example.com/x">x</a><a href="#top">t</a><img src="data:image/png;base64,AA">"##;
        let url = Url::parse("https://example.com/").expect("valid URL");
        assert!(matches!(resolve_link_targets(html, &url), Cow::Borrowed(_)));
    }

    #[test]
    fn an_address_of_only_c0_controls_is_left_as_written() {
        let html = "<a href=\"\u{1}\u{B}\">x</a><a href=\"\u{B}y.html\">y</a>";
        assert_eq!(
            resolve(html, "https://example.com/p/"),
            "<a href=\"\u{1}\u{B}\">x</a><a href=\"https://example.com/p/y.html\">y</a>"
        );
    }

    #[test]
    fn decodes_the_value_and_encodes_every_ampersand_on_the_way_back() {
        assert_eq!(
            resolve(r#"<a href="list?a=1&amp;b=2">x</a>"#, "https://example.com/dir/"),
            r#"<a href="https://example.com/dir/list?a=1&amp;b=2">x</a>"#
        );
    }

    #[test]
    fn a_repeated_attribute_is_rewritten_once() {
        assert_eq!(
            resolve(r#"<a href="a.html" href="b.html">x</a>"#, "https://example.com/d/"),
            r#"<a href="https://example.com/d/a.html" href="b.html">x</a>"#
        );
    }

    #[test]
    fn decodes_numeric_references_as_a_browser_does() {
        // ~keep A browser maps 128-159 through Windows-1252, so &#150; is an en dash, not U+0096.
        assert_eq!(
            resolve(r#"<a href="p&#150;q&#233;.html">x</a>"#, "https://example.com/d/"),
            r#"<a href="https://example.com/d/p%E2%80%93q%C3%A9.html">x</a>"#
        );
    }

    #[test]
    fn decoding_keeps_a_double_quote_in_the_value() {
        assert_eq!(decode_attr_value(r#"a"b&amp;c&#150;"#), "a\"b&c\u{2013}");
    }

    #[test]
    fn decodes_the_base_href_before_joining() {
        assert_eq!(
            resolve(
                r#"<base href="&#x2F;a&amp;b&#x2F;"><a href="leaf.html">x</a>"#,
                "https://example.com/"
            ),
            r#"<base href="https://example.com/a&amp;b/"><a href="https://example.com/a&amp;b/leaf.html">x</a>"#
        );
    }

    #[test]
    fn rewrites_every_base_href_to_the_first_one_resolved() {
        assert_eq!(
            resolve(
                r#"<BASE HREF="/first/"><base href="/second/"><a href="leaf.html">x</a>"#,
                "https://example.com/dir/"
            ),
            r#"<BASE HREF="https://example.com/first/"><base href="https://example.com/first/"><a href="https://example.com/first/leaf.html">x</a>"#
        );
    }

    #[test]
    fn a_base_without_an_href_does_not_count() {
        assert_eq!(
            resolve(
                r#"<base target="_blank"><base href="/x/"><a href="leaf.html">x</a>"#,
                "https://example.com/dir/"
            ),
            r#"<base target="_blank"><base href="https://example.com/x/"><a href="https://example.com/x/leaf.html">x</a>"#
        );
    }

    #[test]
    fn a_quote_from_the_base_cannot_close_a_single_quoted_value() {
        let out = resolve(
            r#"<base href="/it's/"><a href='leaf.html' onclick='x'>x</a>"#,
            "https://example.com/",
        );
        assert_eq!(
            out,
            r#"<base href="https://example.com/it&#x27;s/"><a href='https://example.com/it&#x27;s/leaf.html' onclick='x'>x</a>"#
        );
    }

    #[test]
    fn resolves_each_srcset_candidate_and_keeps_its_descriptor() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(
            resolve_candidates("a.png 1x, /b.png 2x,https://cdn.example/c.png 3x", &base).as_deref(),
            Some("https://example.com/p/a.png 1x, https://example.com/b.png 2x, https://cdn.example/c.png 3x")
        );
    }

    #[test]
    fn a_data_url_candidate_keeps_its_own_comma() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(
            resolve_candidates("data:image/gif;base64,R0lGOD 1x, big.png 800w", &base).as_deref(),
            Some("data:image/gif;base64,R0lGOD 1x, https://example.com/p/big.png 800w")
        );
    }

    #[test]
    fn a_candidate_list_of_absolute_urls_is_left_alone() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(resolve_candidates("https://cdn.example/a.png 1x, #x", &base), None);
    }

    #[test]
    fn a_trailing_comma_ends_a_candidate_without_a_descriptor() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(
            resolve_candidates("a.png, b.png 2x", &base).as_deref(),
            Some("https://example.com/p/a.png, https://example.com/p/b.png 2x")
        );
    }
}
