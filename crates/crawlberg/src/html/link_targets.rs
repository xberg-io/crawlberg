//! Rewriting the relative addresses that the markdown converter renders into absolute URLs,
//! and removing the `data:` addresses of images so their payload stays out of the markdown.

use std::borrow::Cow;
use std::ops::Range;

use html5ever::Attribute;
use tl::{ParserOptions, VDom};
use url::Url;

use super::links::effective_base_url;
use super::raw_text::mask;
use super::start_tags::{RealTags, scan};

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
/// Absolute URLs of any scheme, fragment-only references and empty values are left as written,
/// except that a `data:` address on an `<img>` or a `<graphic>` is removed, so its encoded
/// payload stays out of the markdown. Only tags an HTML parser reads as tags are read or
/// rewritten, so a `<base>` in `<title>` text does not count, and link-shaped text inside
/// `<title>`, `<textarea>`, `<script>` and the like stays as written. Every byte outside a
/// rewritten attribute is kept.
pub(crate) fn resolve_link_targets<'h>(html: &'h str, document_url: &Url) -> Cow<'h, str> {
    // ~keep Scripting on, as the converter reads `<noscript>` (it drops the element).
    // ~keep tl reads every `<name ...>` as a tag, even inside `<title>`, `<script>` or another
    // ~keep tag's quoted value. A tag is rewritten only when this scan also ends a start tag of
    // ~keep the same name at the same `>`, and the rewrite reads the scan's values.
    let read = scan(html, true, |name| rewritten_attributes(name.as_bytes()));
    // ~keep Parse the masked source, as link extraction does: `tl` reads raw-text content as
    // ~keep markup, which both invents tags and hides real ones. Masking keeps every byte
    // ~keep offset, so a span found in the masked source addresses the same bytes in `html`.
    let masked = mask(html, &read.raw_text);
    let Ok(dom) = tl::parse(&masked, ParserOptions::default()) else {
        return Cow::Borrowed(html);
    };
    let base = effective_base_url(read.base_href.as_deref(), document_url);
    let mut edits = collect_edits(&dom, &masked, html, &base, &read.tags);
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
/// The tags come from `dom`, parsed from `masked`, and each edit addresses `html` at the same
/// span. Only the tags in `real_tags` are rewritten.
///
/// ~keep One pass over tl's flat node list rather than a selector query per element name:
/// ~keep each query walks the whole tree, and there are eight element names to look for.
fn collect_edits(
    dom: &VDom<'_>,
    masked: &str,
    html: &str,
    base: &Url,
    real_tags: &RealTags,
) -> Vec<(Range<usize>, String)> {
    let mut edits = Vec::new();
    // ~keep html-to-markdown-rs copies the base into the front matter without decoding it
    // ~keep (`head_metadata.rs`, 3.14.3), so the base is written unencoded between double quotes.
    // ~keep A serialized URL can hold `"`, `<` and `>` (in a host or an opaque path), and those
    // ~keep three are percent-encoded so the value cannot end the attribute or open markup.
    let written_base = base
        .as_str()
        .replace('"', "%22")
        .replace('<', "%3C")
        .replace('>', "%3E");
    for tag in dom.nodes().iter().filter_map(|node| node.as_tag()) {
        let name = tag.name().as_bytes();
        let Some(attributes) = rewritten_attributes(name) else {
            continue;
        };
        let Some(tag_start) = tag.raw().as_bytes_borrowed().and_then(|raw| span_within(masked, raw)) else {
            continue;
        };
        let Some(parsed) = real_tags.find(start_tag_end(html, tag_start.start), name) else {
            continue;
        };
        if name.eq_ignore_ascii_case(b"base") {
            // ~keep Every `<base href>`, not only the first: the converter's front matter
            // ~keep keeps the last one it meets, and only the first one counts in HTML.
            if let Some((span, _)) = parsed_value(tag, parsed, masked, "href")
                && html[span.clone()] != *written_base
            {
                edits.push((with_quotes(html, span), format!("\"{written_base}\"")));
            }
            continue;
        }
        // ~keep An image's `data:` address carries the whole encoded image, and the converter
        // ~keep would write all of it into the markdown (#97). Removing the attribute keeps the
        // ~keep alt text, and the converter falls through to the image's other address attributes.
        // ~keep An emptied one would not do for `<graphic>`: the converter takes the first of its
        // ~keep address attributes that is present, even when it is empty.
        let drop_inline_data = name.eq_ignore_ascii_case(b"img") || name.eq_ignore_ascii_case(b"graphic");
        for &(attr, shape) in attributes {
            let Some((span, value)) = parsed_value(tag, parsed, masked, attr) else {
                continue;
            };
            if drop_inline_data && matches!(shape, Shape::Single) && is_inline_data(value) {
                edits.push(attribute_removal(html, span.clone(), attr).unwrap_or_else(|| value_edit(html, span, "")));
            } else if let Some(rewritten) = rewrite_value(value, shape, base, drop_inline_data) {
                edits.push(value_edit(html, span, &rewritten));
            }
        }
    }
    edits
}

/// The edit that replaces the attribute value at `span` in `html` with `rewritten`, encoded for
/// its quotes.
fn value_edit(html: &str, span: Range<usize>, rewritten: &str) -> (Range<usize>, String) {
    let quoted = span.start > 0 && matches!(html.as_bytes()[span.start - 1], b'"' | b'\'');
    (span, encode_attribute_value(rewritten, quoted))
}

/// The edit that removes the attribute `attr` whose value is at `span` in `html`, name and
/// quotes included.
///
/// `None` when the bytes before the value are not `attr`, optional whitespace and `=`.
fn attribute_removal(html: &str, span: Range<usize>, attr: &str) -> Option<(Range<usize>, String)> {
    let value = with_quotes(html, span);
    let before = html[..value.start].trim_end_matches(|c: char| c.is_ascii_whitespace());
    let before = before
        .strip_suffix('=')?
        .trim_end_matches(|c: char| c.is_ascii_whitespace());
    let name_start = before.len().checked_sub(attr.len())?;
    before
        .get(name_start..)
        .is_some_and(|name| name.eq_ignore_ascii_case(attr))
        .then(|| (name_start..value.end, String::new()))
}

/// The attributes [`collect_edits`] may rewrite on an element named `element`, in any case: the
/// [`TARGETS`], and the `href` of `<base>`.
fn rewritten_attributes(element: &[u8]) -> Option<&'static [(&'static str, Shape)]> {
    if element.eq_ignore_ascii_case(b"base") {
        return Some(&[("href", Shape::Single)]);
    }
    TARGETS
        .iter()
        .find(|(name, _)| element.eq_ignore_ascii_case(name.as_bytes()))
        .map(|(_, attributes)| *attributes)
}

/// The byte span in `masked` of the attribute `attr` on the tl tag, with the value the real
/// parser gives it: decoded, with CR and CRLF made LF and NUL made U+FFFD, as a browser reads it.
///
/// ~keep The value is never read from `masked`: where the HTML parser and tl disagree on where
/// ~keep a tag ends, the masked bytes of a value can hold a masked `<`.
fn parsed_value<'p>(
    tag: &tl::HTMLTag<'_>,
    parsed: &'p [Attribute],
    masked: &str,
    attr: &'static str,
) -> Option<(Range<usize>, &'p str)> {
    let value = &*parsed.iter().find(|a| &*a.name.local == attr)?.value;
    Some((span_within(masked, borrowed_attr(tag, attr)?)?, value))
}

/// The raw bytes of an attribute value, borrowed from the parsed input.
fn borrowed_attr<'h>(tag: &tl::HTMLTag<'h>, attr: &'static str) -> Option<&'h [u8]> {
    tag.attributes().get(attr).flatten().and_then(|v| v.as_bytes_borrowed())
}

/// The new, unencoded value for a decoded attribute value, or `None` when nothing in it changes.
///
/// With `drop_inline_data`, a candidate list loses each `data:` candidate.
fn rewrite_value(decoded: &str, shape: Shape, base: &Url, drop_inline_data: bool) -> Option<String> {
    match shape {
        Shape::Single => resolve_reference(decoded, base),
        Shape::Candidates => resolve_candidates(decoded, base, drop_inline_data),
    }
}

/// Whether `reference` is a `data:` URL, which holds its content inline.
fn is_inline_data(reference: &str) -> bool {
    Url::parse(reference).is_ok_and(|url| url.scheme() == "data")
}

/// Resolve `reference` against `base` when it is a relative reference.
///
/// Returns `None` for anything a reader can already use as written: an absolute URL of any
/// scheme (`https:`, `mailto:`, `javascript:`, `data:`, ...), a fragment-only reference that
/// points into the same document, and an empty value.
fn resolve_reference(reference: &str, base: &Url) -> Option<String> {
    let trimmed = reference.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    match Url::parse(trimmed) {
        Err(url::ParseError::RelativeUrlWithoutBase) => base.join(trimmed).ok().map(String::from),
        _ => None,
    }
}

/// Resolve each candidate URL of a `srcset`-style list, keeping its descriptor, and leave out
/// each `data:` candidate when `drop_inline_data` is set.
///
/// ~keep Follows the HTML "parse a srcset attribute" split: a candidate URL is a run of
/// ~keep non-whitespace (so a `data:` URL's own comma stays inside it), trailing commas end
/// ~keep the candidate, and otherwise the descriptor runs to the next comma.
fn resolve_candidates(list: &str, base: &Url, drop_inline_data: bool) -> Option<String> {
    let mut candidates = Vec::new();
    let mut changed = false;
    let mut rest = list;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if rest.is_empty() {
            break;
        }
        let url_end = rest.find(|c: char| c.is_ascii_whitespace()).unwrap_or(rest.len());
        let (mut candidate_url, after_url) = rest.split_at(url_end);
        let descriptor;
        if candidate_url.ends_with(',') {
            candidate_url = candidate_url.trim_end_matches(',');
            descriptor = "";
            rest = after_url;
        } else {
            let descriptor_end = after_url.find(',').unwrap_or(after_url.len());
            descriptor = after_url[..descriptor_end].trim();
            rest = &after_url[descriptor_end..];
        }
        if drop_inline_data && is_inline_data(candidate_url) {
            changed = true;
            continue;
        }
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

/// The offset just past the `>` that ends the start tag at `start`, skipping any `>` inside a
/// quoted attribute value.
fn start_tag_end(html: &str, start: usize) -> usize {
    let bytes = html.as_bytes();
    let mut after_equals = false;
    let mut i = start + 1;
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'>' => return i + 1,
            b'=' => after_equals = true,
            b'"' | b'\'' if after_equals => {
                let close = bytes[i + 1..].iter().position(|&b| b == byte);
                i = close.map_or(bytes.len(), |offset| i + 1 + offset);
                after_equals = false;
            }
            b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' => {}
            _ => after_equals = false,
        }
        i += 1;
    }
    bytes.len()
}

/// `span` widened to take in the quotes around the attribute value it covers, if it has any.
fn with_quotes(html: &str, span: Range<usize>) -> Range<usize> {
    let bytes = html.as_bytes();
    match (span.start.checked_sub(1).map(|i| bytes[i]), bytes.get(span.end)) {
        (Some(open @ (b'"' | b'\'')), Some(&close)) if open == close => span.start - 1..span.end + 1,
        _ => span,
    }
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
        let html = r##"<a href="https://example.com/x">x</a><a href="#top">t</a><a href="data:text/plain,x">d</a>"##;
        let url = Url::parse("https://example.com/").expect("valid URL");
        assert!(matches!(resolve_link_targets(html, &url), Cow::Borrowed(_)));
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
    fn decodes_the_base_href_before_joining() {
        assert_eq!(
            resolve(
                r#"<base href="&#x2F;a&amp;b&#x2F;"><a href="leaf.html">x</a>"#,
                "https://example.com/"
            ),
            r#"<base href="https://example.com/a&b/"><a href="https://example.com/a&amp;b/leaf.html">x</a>"#
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
            r#"<base href="https://example.com/it's/"><a href='https://example.com/it&#x27;s/leaf.html' onclick='x'>x</a>"#
        );
    }

    #[test]
    fn writes_the_base_unencoded_in_double_quotes_for_the_front_matter() {
        assert_eq!(
            resolve(
                r#"<base href='https://example.com/it&#x27;s/'>"#,
                "https://example.com/"
            ),
            r#"<base href="https://example.com/it's/">"#
        );
        assert_eq!(
            resolve("<base href=/x/?a=1&amp;b=2>", "https://example.com/"),
            r#"<base href="https://example.com/x/?a=1&b=2">"#
        );
        let unchanged = r#"<base href="https://example.com/x/">"#;
        assert!(matches!(
            resolve_link_targets(unchanged, &Url::parse("https://example.com/").expect("valid URL")),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn leaves_the_contents_of_raw_text_elements_as_written() {
        for element in [
            "title", "textarea", "script", "style", "xmp", "iframe", "noembed", "noframes", "noscript",
        ] {
            let html = format!(r#"<{element}>a <a href="x.html"> b</{element}><a href="y.html">y</a>"#);
            assert_eq!(
                resolve(&html, "https://example.com/d/"),
                format!(r#"<{element}>a <a href="x.html"> b</{element}><a href="https://example.com/d/y.html">y</a>"#),
                "element {element}"
            );
        }
    }

    #[test]
    fn raw_text_ends_only_at_its_own_end_tag_in_any_case() {
        assert_eq!(
            resolve(
                r#"<Title><a href="a.html"></titles><a href="b.html"></TITLE ><a href="c.html">c</a>"#,
                "https://example.com/d/"
            ),
            r#"<Title><a href="a.html"></titles><a href="b.html"></TITLE ><a href="https://example.com/d/c.html">c</a>"#
        );
    }

    #[test]
    fn an_end_tag_inside_a_quoted_value_of_the_start_tag_does_not_end_the_raw_text() {
        assert_eq!(
            resolve(
                r#"<title data-x="</title>" data-y='>'><a href="x.html"></title><a href="y.html">y</a>"#,
                "https://example.com/d/"
            ),
            r#"<title data-x="</title>" data-y='>'><a href="x.html"></title><a href="https://example.com/d/y.html">y</a>"#
        );
    }

    #[test]
    fn raw_text_without_an_end_tag_runs_to_the_end_of_the_document() {
        let html = r#"<script>var a = '<a href="x.html">'; <a href="y.html">y</a>"#;
        assert_eq!(resolve(html, "https://example.com/d/"), html);
    }

    #[test]
    fn plaintext_runs_to_the_end_of_the_document() {
        let html = r#"<plaintext><a href="x.html">x</a></plaintext><a href="y.html">y</a>"#;
        assert_eq!(resolve(html, "https://example.com/d/"), html);
    }

    #[test]
    fn an_iframe_source_resolves_although_its_contents_stay_as_written() {
        assert_eq!(
            resolve(
                r#"<iframe src="e.html"><a href="x.html"></iframe>"#,
                "https://example.com/d/"
            ),
            r#"<iframe src="https://example.com/d/e.html"><a href="x.html"></iframe>"#
        );
    }

    #[test]
    fn percent_encodes_quotes_and_angle_brackets_in_the_written_base() {
        for (href, written) in [
            (r#"https://a"b.example/"#, "https://a%22b.example/"),
            (r#"x-foo://a"b/"#, "x-foo://a%22b/"),
            (r#"javascript:alert("x")<b>"#, "javascript:alert(%22x%22)%3Cb%3E"),
        ] {
            assert_eq!(
                resolve(&format!("<base href='{href}'>"), "https://example.com/"),
                format!(r#"<base href="{written}">"#),
                "base {href}"
            );
        }
    }

    #[test]
    fn tags_inside_svg_and_mathml_are_markup() {
        for (html, expected) in [
            (
                r#"<svg><script href="s.js"/></svg><a href="x.html">x</a>"#,
                r#"<svg><script href="s.js"/></svg><a href="https://example.com/d/x.html">x</a>"#,
            ),
            (
                r#"<svg><title><a href="in.html">t</a></title></svg><a href="x.html">x</a>"#,
                r#"<svg><title><a href="https://example.com/d/in.html">t</a></title></svg><a href="https://example.com/d/x.html">x</a>"#,
            ),
            (
                r#"<math><style/></math><a href="x.html">x</a>"#,
                r#"<math><style/></math><a href="https://example.com/d/x.html">x</a>"#,
            ),
        ] {
            assert_eq!(resolve(html, "https://example.com/d/"), expected, "{html}");
        }
    }

    #[test]
    fn a_tag_inside_another_tags_quoted_value_is_text() {
        for html in [
            "</iframe></svg><select><![CDATA[<p title=\"<title><table><svg><p title=\"<iframe>\u{fffd}</script>text </title><img src=\"data:image/gif;base64,AA\" alt=a><!--\0<p title=\"text </title <desc><script>",
            "<title><style><A HREF=x.html><iframe><base href=\"/b/\"><A HREF=x.html><title><math><svg><p title=\"></title <plaintext><p title=\"<table>--><><textarea><foreignObject><img src=\"data:image/gif;base64,AA\" alt=a><title></title ",
        ] {
            assert_eq!(resolve(html, "https://example.com/d/"), html, "{html}");
        }
    }

    #[test]
    fn a_self_closing_script_outside_foreign_content_still_starts_raw_text() {
        let html = r#"<script/><a href="x.html">x</a>"#;
        assert_eq!(resolve(html, "https://example.com/d/"), html);
    }

    #[test]
    fn removes_a_graphic_data_address() {
        assert_eq!(
            resolve(
                r#"<graphic url="data:image/png;base64,AA" alt="g"></graphic><graphic src = data:x href="r.png">"#,
                "https://example.com/"
            ),
            r#"<graphic  alt="g"></graphic><graphic  href="https://example.com/r.png">"#
        );
    }

    #[test]
    fn a_base_written_inside_title_text_does_not_count() {
        assert_eq!(
            resolve(
                r#"<title>x <base href="https://evil.example/"></title><a href="leaf.html">x</a>"#,
                "https://example.com/dir/page.html"
            ),
            r#"<title>x <base href="https://evil.example/"></title><a href="https://example.com/dir/leaf.html">x</a>"#
        );
    }

    #[test]
    fn a_comment_opener_inside_script_text_does_not_hide_a_later_link() {
        assert_eq!(
            resolve(
                r#"<script>var a = "<!--";</script><a href="leaf.html">x</a>"#,
                "https://example.com/dir/page.html"
            ),
            r#"<script>var a = "<!--";</script><a href="https://example.com/dir/leaf.html">x</a>"#
        );
    }

    #[test]
    fn a_comment_opener_inside_script_text_does_not_hide_a_later_inline_image() {
        assert_eq!(
            resolve(
                r#"<script>var a = "<!--";</script><img alt="icon" src="data:image/png;base64,iVBORw0KGgo=">"#,
                "https://example.com/dir/page.html"
            ),
            r#"<script>var a = "<!--";</script><img alt="icon" >"#
        );
    }

    #[test]
    fn a_quote_in_an_unquoted_value_opens_no_quote() {
        // ~keep The `'` of `x'y` is part of an unquoted value, so `<title>t<i</title>` sits inside
        // ~keep the quoted `href` value and is not masked as title text.
        assert_eq!(
            resolve(
                r#"<a b=x'y href="d'><title>t<i</title>">x</a>"#,
                "https://example.com/dir/"
            ),
            r#"<a b=x'y href="https://example.com/dir/d&#x27;%3E%3Ctitle%3Et%3Ci%3C/title%3E">x</a>"#
        );
    }

    #[test]
    fn a_quote_in_an_unquoted_value_does_not_hide_later_links() {
        assert_eq!(
            resolve(
                r#"<p b=x'y>one</p><i c='><title>' >two</i><p>Go <a href="leaf.html">leaf</a> <img alt="icon" src="data:image/png;base64,iVBORw0KGgo="></p>"#,
                "https://example.com/dir/"
            ),
            r#"<p b=x'y>one</p><i c='><title>' >two</i><p>Go <a href="https://example.com/dir/leaf.html">leaf</a> <img alt="icon" ></p>"#
        );
    }

    #[test]
    fn a_base_inside_noscript_does_not_count() {
        assert_eq!(
            resolve(
                r#"<head><noscript><base href="/ns/"></noscript></head><a href="leaf.html">x</a>"#,
                "https://example.com/dir/page.html"
            ),
            r#"<head><noscript><base href="/ns/"></noscript></head><a href="https://example.com/dir/leaf.html">x</a>"#
        );
    }

    #[test]
    fn resolves_each_srcset_candidate_and_keeps_its_descriptor() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(
            resolve_candidates("a.png 1x, /b.png 2x,https://cdn.example/c.png 3x", &base, false).as_deref(),
            Some("https://example.com/p/a.png 1x, https://example.com/b.png 2x, https://cdn.example/c.png 3x")
        );
    }

    #[test]
    fn rewrites_values_the_parser_normalizes() {
        let wrong = [
            (
                "<a href=\"x.html\r\n\">x</a>",
                r#"<a href="https://example.com/d/x.html">x</a>"#,
            ),
            (
                "<a href=\"x\r.html\">x</a>",
                r#"<a href="https://example.com/d/x.html">x</a>"#,
            ),
            (
                "<img srcset=\"a.png 1x,\r\nb.png 2x\">",
                r#"<img srcset="https://example.com/d/a.png 1x, https://example.com/d/b.png 2x">"#,
            ),
            (
                "<a href=\"x\0.html\">x</a>",
                r#"<a href="https://example.com/d/x%EF%BF%BD.html">x</a>"#,
            ),
            ("<base href=\"/b/\r\n\">", r#"<base href="https://example.com/b/">"#),
            (
                "<img src=\"data:image/png;base64,AA\r\nAA\" alt=\"a\">",
                r#"<img  alt="a">"#,
            ),
        ]
        .into_iter()
        .filter(|(html, expected)| resolve(html, "https://example.com/d/") != *expected)
        .collect::<Vec<_>>();
        assert!(wrong.is_empty(), "not rewritten as expected: {wrong:?}");
    }

    #[test]
    fn removes_an_image_data_address_and_keeps_a_link_data_address() {
        assert_eq!(
            resolve(
                r#"<img src="data:image/png;base64,AA" alt="a"><IMG data-src=" DATA:image/png,x"><a href="data:text/plain,x">d</a>"#,
                "https://example.com/"
            ),
            r#"<img  alt="a"><IMG ><a href="data:text/plain,x">d</a>"#
        );
    }

    #[test]
    fn leaves_image_data_candidates_out_of_a_candidate_list() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(
            resolve_candidates("data:image/gif;base64,R0lGOD 1x, big.png 800w", &base, true).as_deref(),
            Some("https://example.com/p/big.png 800w")
        );
        assert_eq!(
            resolve_candidates("data:image/gif;base64,R0lGOD 2x", &base, true).as_deref(),
            Some("")
        );
        assert_eq!(resolve_candidates("https://cdn.example/a.png 1x", &base, true), None);
    }

    #[test]
    fn a_data_url_candidate_keeps_its_own_comma() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(
            resolve_candidates("data:image/gif;base64,R0lGOD 1x, big.png 800w", &base, false).as_deref(),
            Some("data:image/gif;base64,R0lGOD 1x, https://example.com/p/big.png 800w")
        );
    }

    #[test]
    fn a_candidate_list_of_absolute_urls_is_left_alone() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(
            resolve_candidates("https://cdn.example/a.png 1x, #x", &base, false),
            None
        );
    }

    #[test]
    fn a_trailing_comma_ends_a_candidate_without_a_descriptor() {
        let base = Url::parse("https://example.com/p/").expect("valid URL");
        assert_eq!(
            resolve_candidates("a.png, b.png 2x", &base, false).as_deref(),
            Some("https://example.com/p/a.png, https://example.com/p/b.png 2x")
        );
    }
}
