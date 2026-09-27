//! Rewriting the relative addresses that the markdown converter renders into absolute URLs,
//! and removing the `data:` addresses of images and media so their payload stays out of the
//! markdown.

use std::borrow::Cow;

use url::Url;

use super::links::effective_base_url;
use super::start_tags::{StartTag, scan};

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

/// The elements of [`TARGETS`] whose `data:` addresses are removed, because the converter would
/// write their whole encoded payload into the markdown.
const INLINE_DATA_ELEMENTS: &[&str] = &["img", "video", "audio", "iframe", "source", "graphic"];

/// Return `source` with every relative address in [`TARGETS`] resolved against the document's
/// base URL (its first `<base href>`, else `document_url`), using WHATWG URL parsing.
///
/// Each `<base href>` is rewritten to that resolved base, so the converter's front matter shows
/// the address the links resolve against.
///
/// Character references in a value are decoded before resolution, as a browser decodes them.
/// Absolute URLs of any scheme, fragment-only references and empty values are left as written,
/// except that a `data:` address on an image, a media element, an iframe or a `<graphic>` is
/// removed, so its encoded payload stays out of the markdown. Only tags an HTML parser reads as
/// tags are read or rewritten, so a `<base>` in `<title>` text does not count, and link-shaped
/// text inside `<title>`, `<textarea>`, `<script>` and the like stays as written.
///
/// Each start tag of a [`TARGETS`] element or `<base>` is written back as the HTML parser reads
/// it, when that differs from the source: every attribute once, in double quotes, with the
/// rewritten addresses. An attribute whose name has a character other than a letter, a digit,
/// `-`, `_` or `:` is left out, as the converter never reads it. Every byte outside a rewritten
/// start tag is kept, except an empty end tag `</>` just before one, which an HTML parser ignores,
/// and the attributes of a tag past the limit the HTML parser is given, which are overwritten
/// with spaces (see `start_tags::AttributeBound`).
pub(crate) fn resolve_link_targets<'h>(source: &'h str, document_url: &Url) -> Cow<'h, str> {
    // ~keep Scripting on, as the converter reads `<noscript>` (it drops the element).
    let read = scan(source, true, |name| rewritten_attributes(name.as_bytes()).is_some());
    let html: &str = &read.text;
    let base = effective_base_url(read.base_href.as_deref(), document_url);
    // ~keep html-to-markdown-rs copies the base into the front matter without decoding it
    // ~keep (`head_metadata.rs`, 3.14.3), so the base is written unencoded between double quotes.
    // ~keep A serialized URL can hold `"`, `<` and `>` (in a host or an opaque path), and those
    // ~keep three are percent-encoded so the value cannot end the attribute or open markup.
    let written_base = base
        .as_str()
        .replace('"', "%22")
        .replace('<', "%3C")
        .replace('>', "%3E");

    let mut out = String::new();
    let mut cursor = 0;
    let mut written = String::new();
    for tag in read.tags.iter() {
        written.clear();
        write_tag(&mut written, &tag, &base, &written_base);
        if html[tag.span.clone()] != *written {
            out.push_str(&html[cursor..tag.span.start]);
            out.push_str(&written);
            cursor = tag.span.end;
        }
    }
    if cursor == 0 {
        return read.text;
    }
    out.push_str(&html[cursor..]);
    Cow::Owned(out)
}

/// Write the start tag `tag` into `out` as an HTML parser reads it, with its addresses rewritten.
///
/// ~keep The converter parses the tag again with its own reader, which can split a tag where
/// ~keep the HTML parser does not, or read an attribute the parser does not see (#233). Written
/// ~keep this way, the tag holds nothing for the two readers to disagree on: each attribute once,
/// ~keep double-quoted and encoded, and a name that is not letters, digits, `-`, `_` or `:` left
/// ~keep out. Such a name is never an attribute the converter reads.
fn write_tag(out: &mut String, tag: &StartTag<'_>, base: &Url, written_base: &str) {
    let attributes = rewritten_attributes(tag.name.as_bytes()).unwrap_or_default();
    let is_base = tag.name == "base";
    // ~keep A `data:` address carries the whole encoded image or media, and the converter
    // ~keep would write all of it into the markdown (#97). Removing the attribute keeps an
    // ~keep image's alt text, and the converter falls through to the element's other address
    // ~keep attributes, or for media to a nested `<source>`. Links keep theirs. A candidate
    // ~keep list left with no candidates goes the same way. An emptied one would not do for
    // ~keep `<graphic>`: the converter takes the first of its address attributes that is
    // ~keep present, even when it is empty.
    let drop_inline_data = INLINE_DATA_ELEMENTS.contains(&tag.name);
    out.push('<');
    out.push_str(tag.name);
    for attr in tag.attrs {
        let name = &*attr.name.local;
        if !is_plain_name(name) {
            continue;
        }
        if is_base && name == "href" {
            // ~keep Every `<base href>`, not only the first: the converter's front matter
            // ~keep keeps the last one it meets, and only the first one counts in HTML.
            push_attribute(out, name, written_base);
            continue;
        }
        let value = &*attr.value;
        let rewritten = match attributes.iter().find(|&&(target, _)| target == name) {
            Some((_, Shape::Single)) if drop_inline_data && is_inline_data(value) => None,
            Some(&(_, shape)) => match rewrite_value(value, shape, base, drop_inline_data) {
                Some(rewritten) if drop_inline_data && rewritten.is_empty() => None,
                Some(rewritten) => Some(Cow::Owned(rewritten)),
                None => Some(Cow::Borrowed(value)),
            },
            None => Some(Cow::Borrowed(value)),
        };
        if let Some(rewritten) = rewritten {
            push_attribute(out, name, &html_escape::encode_double_quoted_attribute(&rewritten));
        }
    }
    if tag.self_closing {
        out.push('/');
    }
    out.push('>');
}

/// Append ` name="value"` to `out`, with `value` already encoded.
fn push_attribute(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    out.push_str(value);
    out.push('"');
}

/// Whether an attribute `name` is only ASCII letters, digits, `-`, `_` and `:`.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
}

/// The attributes [`write_tag`] may rewrite on an element named `element`, in any case: the
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

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(html: &str, document_url: &str) -> String {
        let url = Url::parse(document_url).expect("valid document URL");
        resolve_link_targets(html, &url).into_owned()
    }

    #[test]
    fn rewrites_only_the_start_tag() {
        let html = r#"<!doctype html><p class=x>a <a  title="t" href = 'rel/child.html' >c</a> &amp; b</p>"#;
        assert_eq!(
            resolve(html, "https://example.com/dir/page.html"),
            r#"<!doctype html><p class=x>a <a title="t" href="https://example.com/dir/rel/child.html">c</a> &amp; b</p>"#
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
            r#"<a href="https://example.com/a/up.html">x</A>"#
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
    fn keeps_only_the_first_copy_of_a_repeated_attribute() {
        assert_eq!(
            resolve(r#"<a href="a.html" href="b.html">x</a>"#, "https://example.com/d/"),
            r#"<a href="https://example.com/d/a.html">x</a>"#
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
            r#"<base href="https://example.com/first/"><base href="https://example.com/first/"><a href="https://example.com/first/leaf.html">x</a>"#
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
    fn a_quote_from_the_base_cannot_close_the_written_value() {
        let out = resolve(
            r#"<base href="/it's/"><a href='leaf.html' onclick='x'>x</a>"#,
            "https://example.com/",
        );
        assert_eq!(
            out,
            r#"<base href="https://example.com/it's/"><a href="https://example.com/it's/leaf.html" onclick="x">x</a>"#
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
            ("/a&amp;amp;b/", "https://example.com/a&amp;b/"),
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
            r#"<graphic alt="g"></graphic><graphic href="https://example.com/r.png">"#
        );
    }

    #[test]
    fn removes_a_candidate_list_left_empty_by_inline_data() {
        assert_eq!(
            resolve(
                r#"<img srcset="data:image/png,x 1x, data:image/png,y 2x" alt="a"><img data-srcset="data:image/png,x" src="a.png">"#,
                "https://example.com/"
            ),
            r#"<img alt="a"><img src="https://example.com/a.png">"#
        );
    }

    #[test]
    fn removes_every_copy_of_a_removed_attribute() {
        assert_eq!(
            resolve(
                r#"<img src="data:image/png,A" src="data:image/png,B" alt="a"><img SRC="data:image/png,A" src=b.png>"#,
                "https://example.com/"
            ),
            r#"<img alt="a"><img>"#
        );
    }

    #[test]
    fn keeps_the_slash_of_a_self_closing_tag() {
        // ~keep Inside SVG the slash closes the element, so the text after it is not link text.
        assert_eq!(
            resolve(
                r#"<svg><a href = "x.html"/><text>t</text></svg>"#,
                "https://example.com/d/"
            ),
            r#"<svg><a href="https://example.com/d/x.html"/><text>t</text></svg>"#
        );
    }

    #[test]
    fn removes_a_data_address_the_parser_reads_after_a_misread_copy() {
        assert_eq!(
            resolve(
                r#"<img src<="data:image/png,A" src="data:image/png,B" alt="a">"#,
                "https://example.com/"
            ),
            r#"<img alt="a">"#
        );
    }

    #[test]
    fn an_equals_sign_inside_an_unquoted_value_does_not_open_a_quote() {
        assert_eq!(
            resolve(r#"<a href=x="y>z</a><a href=a=b"c>z</a>"#, "https://example.com/"),
            r#"<a href="https://example.com/x=%22y">z</a><a href="https://example.com/a=b%22c">z</a>"#
        );
    }

    #[test]
    fn reads_the_xlink_href_of_a_graphic_inside_svg() {
        assert_eq!(
            resolve(
                r#"<svg><graphic xlink:href="data:image/png,x" alt="g"></graphic><graphic xlink:href="g.png"></graphic></svg>"#,
                "https://example.com/"
            ),
            r#"<svg><graphic alt="g"></graphic><graphic xlink:href="https://example.com/g.png"></graphic></svg>"#
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
            r#"<script>var a = "<!--";</script><img alt="icon">"#
        );
    }

    #[test]
    fn a_quote_in_an_unquoted_value_opens_no_quote() {
        // ~keep The `'` of `x'y` is part of an unquoted value, so `<title>t<i</title>` sits inside
        // ~keep the quoted `href` value and is not read as title text.
        assert_eq!(
            resolve(
                r#"<a b=x'y href="d'><title>t<i</title>">x</a>"#,
                "https://example.com/dir/"
            ),
            r#"<a b="x'y" href="https://example.com/dir/d'%3E%3Ctitle%3Et%3Ci%3C/title%3E">x</a>"#
        );
    }

    #[test]
    fn a_quote_in_an_unquoted_value_does_not_hide_later_links() {
        assert_eq!(
            resolve(
                r#"<p b=x'y>one</p><i c='><title>' >two</i><p>Go <a href="leaf.html">leaf</a> <img alt="icon" src="data:image/png;base64,iVBORw0KGgo="></p>"#,
                "https://example.com/dir/"
            ),
            r#"<p b=x'y>one</p><i c='><title>' >two</i><p>Go <a href="https://example.com/dir/leaf.html">leaf</a> <img alt="icon"></p>"#
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
                r#"<img alt="a">"#,
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
            r#"<img alt="a"><img><a href="data:text/plain,x">d</a>"#
        );
    }

    #[test]
    fn removes_the_data_address_of_media_and_iframes() {
        assert_eq!(
            resolve(
                r#"<VIDEO src="data:video/mp4,x"><source src="data:video/mp4,y"></VIDEO><audio src="data:audio/mpeg,x"></audio><iframe src="data:text/html,x"></iframe><blockquote cite="data:text/plain,x"></blockquote>"#,
                "https://example.com/"
            ),
            r#"<video><source></VIDEO><audio></audio><iframe></iframe><blockquote cite="data:text/plain,x"></blockquote>"#
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
