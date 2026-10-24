//! ~keep Optional omission of decorative SVG names from linked page text (#805).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;

use tl::{HTMLTag, Node, Parser};

use super::real_tags::{StartTag, TagAttribute, scan, write_start_tag};
use super::{mask_raw_text_markup, parse_html};

type Edit = (Range<usize>, String);

pub(crate) fn omit_linked_icon_labels(html: &str) -> Cow<'_, str> {
    let read = scan(html, |_| true);
    let starts: BTreeMap<_, _> = read.tags.iter().map(|tag| (tag.span.start, tag)).collect();
    let masked = mask_raw_text_markup(html);
    let (source, insertions) = parser_source(&masked.text, &starts);
    let Ok(dom) = parse_html(&source) else {
        return Cow::Borrowed(html);
    };
    let parser = dom.parser();
    let mut edits = Vec::new();
    for anchor in dom.nodes().iter().filter_map(Node::as_tag) {
        if anchor.name().as_bytes() != b"a" {
            continue;
        }
        if let Some(mut found) = anchor_edits(anchor, parser, html, &starts, &insertions) {
            edits.append(&mut found);
        }
    }
    apply_edits(html, edits)
}

fn anchor_edits(
    anchor: &HTMLTag<'_>,
    parser: &Parser<'_>,
    html: &str,
    starts: &BTreeMap<usize, StartTag<'_>>,
    insertions: &[usize],
) -> Option<Vec<Edit>> {
    let span = closed_range(anchor, parser, html, "a", starts, insertions)?;
    let start = starts.get(&span.start)?;
    if start.name != "a" {
        return None;
    }
    let mut edits = icon_edits(anchor, parser, html, starts, insertions)?;
    if edits.is_empty()
        || edits
            .iter()
            .any(|(icon, _)| icon.start < start.span.end || icon.end > span.end)
    {
        return None;
    }
    if start
        .attrs
        .iter()
        .any(|attr| matches!(&*attr.name, "aria-label" | "title"))
    {
        edits.push((start.span.clone(), unnamed_anchor(start)));
    }
    Some(edits)
}

fn icon_edits(
    anchor: &HTMLTag<'_>,
    parser: &Parser<'_>,
    html: &str,
    starts: &BTreeMap<usize, StartTag<'_>>,
    insertions: &[usize],
) -> Option<Vec<Edit>> {
    let mut pending: Vec<_> = anchor.children().top().iter().copied().collect();
    let mut edits = Vec::new();
    while let Some(handle) = pending.pop() {
        let Node::Tag(tag) = handle.get(parser)? else {
            continue;
        };
        match tag.name().as_bytes() {
            b"a" => return None,
            b"svg" => {
                let span = closed_range(tag, parser, html, "svg", starts, insertions)?;
                if starts.get(&span.start)?.name != "svg" {
                    return None;
                }
                if is_decorative(tag, parser) {
                    // ~keep Empty wrappers suppress the converter's href-as-label fallback.
                    edits.push((span, "<span></span>".to_owned()));
                }
            }
            b"script" | b"style" | b"template" => {}
            _ => pending.extend(tag.children().top().iter().copied()),
        }
    }
    Some(edits)
}

fn is_decorative(svg: &HTMLTag<'_>, parser: &Parser<'_>) -> bool {
    let mut pending: Vec<_> = svg.children().top().iter().copied().collect();
    while let Some(handle) = pending.pop() {
        let Some(Node::Tag(tag)) = handle.get(parser) else {
            continue;
        };
        if !matches!(
            tag.name().as_bytes(),
            b"svg"
                | b"g"
                | b"path"
                | b"use"
                | b"rect"
                | b"circle"
                | b"ellipse"
                | b"line"
                | b"polyline"
                | b"polygon"
                | b"title"
                | b"desc"
                | b"defs"
                | b"symbol"
        ) {
            return false;
        }
        pending.extend(tag.children().top().iter().copied());
    }
    true
}

fn parser_source(html: &str, starts: &BTreeMap<usize, StartTag<'_>>) -> (String, Vec<usize>) {
    let mut source = String::with_capacity(html.len());
    let mut insertions = Vec::new();
    let mut cursor = 0;
    for tag in starts.values().filter(|tag| tag.self_closing) {
        let slash = tag.span.end - "/>".len();
        if html.as_bytes().get(slash) != Some(&b'/') || html.as_bytes()[slash - 1].is_ascii_whitespace() {
            continue;
        }
        source.push_str(&html[cursor..slash]);
        insertions.push(source.len());
        // ~keep tl reads `<path/>` as name `path/`; a parse-only space keeps source edits exact.
        source.push(' ');
        cursor = slash;
    }
    source.push_str(&html[cursor..]);
    (source, insertions)
}

fn original_offset(offset: usize, insertions: &[usize]) -> Option<usize> {
    if insertions.binary_search(&offset).is_ok() {
        return None;
    }
    Some(offset - insertions.partition_point(|insertion| *insertion < offset))
}

fn closed_range(
    tag: &HTMLTag<'_>,
    parser: &Parser<'_>,
    html: &str,
    name: &str,
    starts: &BTreeMap<usize, StartTag<'_>>,
    insertions: &[usize],
) -> Option<Range<usize>> {
    let (start, end) = tag.boundaries(parser);
    let span = original_offset(start, insertions)?..original_offset(end.checked_add(1)?, insertions)?;
    let source = html.get(span.clone())?;
    if name == "svg"
        && starts
            .get(&span.start)
            .is_some_and(|start| start.name == "svg" && start.self_closing && start.span == span)
    {
        return Some(span);
    }
    let closing = source.get(source.rfind('<')?..)?.trim_end_matches('>');
    closing
        .trim_ascii()
        .eq_ignore_ascii_case(&format!("</{name}"))
        .then_some(span)
}

fn unnamed_anchor(tag: &StartTag<'_>) -> String {
    let attrs: Vec<_> = tag
        .attrs
        .iter()
        .filter(|attr| !matches!(&*attr.name, "aria-label" | "title"))
        .map(|attr| TagAttribute {
            name: attr.name.clone(),
            value: attr.value.clone(),
        })
        .collect();
    let filtered = StartTag {
        name: tag.name,
        span: tag.span.clone(),
        self_closing: tag.self_closing,
        truncated: tag.truncated,
        attrs: &attrs,
    };
    let mut output = String::new();
    write_start_tag(&mut output, &filtered, &[]);
    output
}

fn apply_edits(html: &str, mut edits: Vec<Edit>) -> Cow<'_, str> {
    if edits.is_empty() {
        return Cow::Borrowed(html);
    }
    edits.sort_by_key(|(span, _)| (span.start, span.end));
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    for (span, value) in edits {
        if span.start < cursor || span.end > html.len() {
            return Cow::Borrowed(html);
        }
        output.push_str(&html[cursor..span.start]);
        output.push_str(&value);
        cursor = span.end;
    }
    output.push_str(&html[cursor..]);
    Cow::Owned(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_remove_only_names_of_a_linked_decorative_svg() {
        let html = concat!(
            r#"<p>alpha <a href="/history" aria-label="History">"#,
            "<svg><title>Clock</title><path/></svg></a> omega</p>"
        );
        assert_eq!(
            omit_linked_icon_labels(html),
            r#"<p>alpha <a href="/history"><span></span></a> omega</p>"#
        );
    }

    #[test]
    fn should_keep_real_link_text_while_omitting_the_icon_link_tooltip() {
        let html = concat!(
            r#"<a href="/x" title="Tooltip" aria-label="Accessible">GitHub"#,
            r##"<svg aria-label="External"><use href="#ext"/></svg></a>"##
        );
        assert_eq!(omit_linked_icon_labels(html), r#"<a href="/x">GitHub<span></span></a>"#);
    }

    #[test]
    fn should_preserve_drawn_text_images_and_nonicon_names() {
        for html in [
            r#"<a href="/x" title="Graph"><svg><text>12 kg</text></svg></a>"#,
            r#"<a href="/x" title="Graph"><svg><foreignObject><p>12 kg</p></foreignObject></svg></a>"#,
            r#"<a href="/x" title="Logo"><img src="/logo.png" alt="Logo"></a>"#,
            r#"<a href="/x" aria-label="Menu"></a>"#,
            r#"<svg aria-label="Diagram"><title>Diagram</title><path/></svg>"#,
        ] {
            assert_eq!(omit_linked_icon_labels(html), html);
        }
    }

    #[test]
    fn should_leave_literal_markup_and_unclosed_elements_unchanged() {
        for html in [
            r#"<script>const x = '<a href="/x" title="Icon"><svg><path/></svg></a>';</script>"#,
            r#"<textarea><a href="/x" title="Icon"><svg><path/></svg></a></textarea>"#,
            r#"<!-- <a href="/x" title="Icon"><svg><path/></svg></a> -->"#,
            r#"<a href="/x" title="Icon"><svg><path/>tail</a>"#,
            r#"<a href="/x" title="Icon"><svg><path/></svg>"#,
            r#"<a href="/x" title="Icon"><svg><path/></a></svg>"#,
            r#"<svg><![CDATA[<a href="/x" title="Icon"><svg><path/></svg></a>]]></svg>"#,
            r#"<math><![CDATA[<a href="/x" title="Icon"><svg/></a>]]></math>"#,
        ] {
            assert_eq!(omit_linked_icon_labels(html), html);
        }
    }

    #[test]
    fn should_handle_nested_wrappers_and_uppercase_without_rewriting_other_bytes() {
        let html = r#"before<A HREF="/x" TITLE="Icon"><B><SVG><PATH/></SVG></B></A>after"#;
        assert_eq!(
            omit_linked_icon_labels(html),
            r#"before<a href="/x"><B><span></span></B></A>after"#
        );
    }

    #[test]
    fn should_keep_an_image_label_when_an_icon_is_next_to_it() {
        let html = r#"<a href="/x" title="Logo"><svg><path/></svg><img src="/x.png" alt="Logo"></a>"#;
        assert_eq!(
            omit_linked_icon_labels(html),
            r#"<a href="/x"><span></span><img src="/x.png" alt="Logo"></a>"#
        );
    }

    #[test]
    fn should_map_multiple_parse_only_spaces_back_to_original_unicode_bytes() {
        let html = concat!(
            r##"α<a href="/x" title="Icon"><svg data-note="a/>b"><path/><use href="#x"/></svg></a>"##,
            r#"中<a href="/y" title="Next"><svg/></a>ω"#
        );
        assert_eq!(
            omit_linked_icon_labels(html),
            "α<a href=\"/x\"><span></span></a>中<a href=\"/y\"><span></span></a>ω"
        );
    }

    #[test]
    fn should_preserve_html_that_breaks_out_of_svg() {
        let html = r#"<a href="/x" title="Icon"><svg><p>real HTML text</p></svg></a>"#;
        assert_eq!(omit_linked_icon_labels(html), html);
    }

    #[test]
    fn should_reject_a_boundary_inside_a_parse_only_insertion() {
        assert_eq!(original_offset(4, &[4, 8]), None);
        assert_eq!(original_offset(8, &[4, 8]), None);
        assert_eq!(original_offset(9, &[4, 8]), Some(7));
    }
}
