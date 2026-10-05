//! ~keep Credential sanitization for URL-bearing attributes passed to the Markdown converter.

use std::borrow::Cow;

use url::Url;

use super::real_tags::write_start_tag;
use super::{PageScan, effective_base_url, mask_raw_text_markup};

#[derive(Clone, Copy)]
enum Shape {
    Single,
    Candidates,
}

fn attributes(element: &str) -> &'static [(&'static str, Shape)] {
    match element {
        "a" => &[("href", Shape::Single)],
        "audio" | "iframe" | "video" => &[("src", Shape::Single)],
        "base" => &[("href", Shape::Single)],
        "blockquote" => &[("cite", Shape::Single)],
        "graphic" => &[
            ("url", Shape::Single),
            ("href", Shape::Single),
            ("xlink:href", Shape::Single),
            ("src", Shape::Single),
        ],
        "img" => &[
            ("src", Shape::Single),
            ("data-src", Shape::Single),
            ("data-lazy-src", Shape::Single),
            ("data-original", Shape::Single),
            ("data-srcset", Shape::Candidates),
            ("srcset", Shape::Candidates),
        ],
        "source" => &[("src", Shape::Single), ("srcset", Shape::Candidates)],
        _ => &[],
    }
}

/// ~keep The existing `PageScan` supplies browser-accurate tag spans, so this edits only URL
/// ~keep attributes without reparsing the document or touching URL-shaped prose and code.
pub(crate) fn sanitize_url_attributes<'h>(
    html: &'h str,
    page_scan: Option<PageScan>,
    document_url: &Url,
) -> (Cow<'h, str>, Option<Url>) {
    let page = page_scan.map_or_else(|| mask_raw_text_markup(html), |scan| scan.attach(html));
    let sanitized_base = page
        .base_href
        .as_deref()
        .map(|base| strip_reference_userinfo(base, document_url).unwrap_or_else(|| base.to_owned()));
    let effective_base = sanitized_base
        .as_deref()
        .map(|base| effective_base_url(Some(base), document_url));
    let target_base = effective_base.as_ref().unwrap_or(document_url);
    let mut edits = Vec::new();

    for real_tag in page.url_tags.iter() {
        let resolution_base = if real_tag.name == "base" {
            document_url
        } else {
            target_base
        };
        let replacements = attributes(real_tag.name)
            .iter()
            .filter_map(|&(attribute, shape)| {
                real_tag
                    .attrs
                    .iter()
                    .find(|candidate| &*candidate.name == attribute)
                    .and_then(|candidate| sanitize_value(&candidate.value, shape, resolution_base))
                    .map(|value| (attribute, value))
            })
            .collect::<Vec<_>>();
        if replacements.is_empty() && !real_tag.truncated {
            continue;
        }
        let mut rewritten = String::new();
        write_start_tag(&mut rewritten, &real_tag, &replacements);
        edits.push((real_tag.span, rewritten));
    }

    if edits.is_empty() {
        return (Cow::Borrowed(html), effective_base);
    }
    edits.sort_by_key(|(span, _)| span.start);
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    for (span, value) in edits {
        output.push_str(&html[cursor..span.start]);
        output.push_str(&value);
        cursor = span.end;
    }
    output.push_str(&html[cursor..]);
    (Cow::Owned(output), effective_base)
}

fn sanitize_value(raw: &str, shape: Shape, base_url: &Url) -> Option<String> {
    match shape {
        Shape::Single => strip_reference_userinfo(raw, base_url),
        Shape::Candidates => sanitize_candidates(raw, base_url),
    }
}

fn sanitize_candidates(list: &str, base_url: &Url) -> Option<String> {
    if !list.contains('@') {
        return None;
    }
    let mut changed = false;
    let candidates = super::srcset::srcset_candidates(list)
        .map(|(url, descriptor)| {
            let url = strip_reference_userinfo(url, base_url).map_or_else(
                || url.to_owned(),
                |clean| {
                    changed = true;
                    clean
                },
            );
            if descriptor.is_empty() {
                url
            } else {
                format!("{url} {descriptor}")
            }
        })
        .collect::<Vec<_>>();
    changed.then(|| candidates.join(", "))
}

fn strip_reference_userinfo(reference: &str, base_url: &Url) -> Option<String> {
    if !reference.contains('@') {
        return None;
    }
    let mut url = Url::parse(reference).or_else(|_| base_url.join(reference)).ok()?;
    if !crate::net::userinfo::has_userinfo(&url) {
        return None;
    }
    crate::net::userinfo::strip(&mut url);
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sanitize(html: &str) -> Cow<'_, str> {
        let scan = mask_raw_text_markup(html).detach();
        let document_url = Url::parse("https://origin.test/page").expect("valid document URL");
        sanitize_url_attributes(html, Some(scan), &document_url).0
    }

    #[test]
    fn leaves_many_unchanged_tags_byte_identical_without_reading_the_page_again() {
        let unique = "many-url-tags-use-the-retained-scan";
        let tags = r#"<a class='x' href='/page'>link</a><img alt='x' src='/image.png'>"#.repeat(100);
        let html = format!("{}<div id={unique}>{tags}</div>", crate::html::reads::MARKER);
        let scan = mask_raw_text_markup(&html).detach();
        let reads = crate::html::reads::count(unique);
        let document_url = Url::parse("https://origin.test/page").expect("valid document URL");

        let (sanitized, _) = sanitize_url_attributes(&html, Some(scan), &document_url);

        assert!(matches!(sanitized, Cow::Borrowed(_)));
        assert_eq!(sanitized, html);
        assert_eq!(crate::html::reads::count(unique), reads);
    }

    #[test]
    fn keeps_the_first_repeated_attribute_and_non_url_attributes() {
        let html = concat!(
            "<a class='keep' href='//alice:secret@example.test/first' ",
            "href='https://ignored.test/' data-note='a&amp;b'>x</a>"
        );

        assert_eq!(
            sanitize(html),
            r#"<a class="keep" href="https://example.test/first" data-note="a&amp;b">x</a>"#
        );
    }

    #[test]
    fn omits_a_malformed_attribute_name_when_rebuilding_a_changed_tag() {
        let html = r#"<a x"y="1" href="//alice:secret@example.test/ok">x</a>"#;

        assert_eq!(sanitize(html), r#"<a href="https://example.test/ok">x</a>"#);
    }

    #[test]
    fn sanitizes_a_protocol_relative_link_against_a_scheme_changing_base() {
        let html = concat!(
            r#"<base href="http://base.test/root/">"#,
            r#"<a href="//user:secret@target.test/page">x</a>"#
        );

        assert_eq!(
            sanitize(html),
            concat!(
                r#"<base href="http://base.test/root/">"#,
                r#"<a href="http://target.test/page">x</a>"#
            )
        );
    }

    #[test]
    fn sanitizes_srcset_and_media_against_a_scheme_changing_base() {
        let html = concat!(
            r#"<base href="http://base.test/root/">"#,
            r#"<video src="//video:secret@media.test/v.mp4"></video>"#,
            r#"<img srcset="//small:secret@img.test/s.png 1x, "#,
            r#"//large:secret@img.test/l.png 2x">"#
        );

        assert_eq!(
            sanitize(html),
            concat!(
                r#"<base href="http://base.test/root/">"#,
                r#"<video src="http://media.test/v.mp4"></video>"#,
                r#"<img srcset="http://img.test/s.png 1x, http://img.test/l.png 2x">"#
            )
        );
    }

    #[test]
    fn drops_a_url_attribute_masked_by_the_scan_limit() {
        let input_attributes = (0..crate::html::ATTRIBUTE_LIMIT)
            .map(|index| format!("data-{index}=x"))
            .collect::<Vec<_>>()
            .join(" ");
        let expected_attributes = (0..crate::html::ATTRIBUTE_LIMIT)
            .map(|index| format!(r#"data-{index}="x""#))
            .collect::<Vec<_>>()
            .join(" ");
        let html = format!(r#"<a {input_attributes} href="https://user:secret@example.test/private">x</a>"#);
        let expected = format!("<a {expected_attributes}>x</a>");

        assert_eq!(sanitize(&html), expected);
    }
}
