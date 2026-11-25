//! ~keep Optional omission of decorative SVG names from linked page text (#805).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use astral_html::{Reader, Token};

use super::mask_raw_text_markup;
use super::real_tags::{StartTag, TagAttribute, scan, write_start_tag};

type Edit = (Range<usize>, String);

pub(crate) fn omit_linked_icon_labels(html: &str) -> Cow<'_, str> {
    let read = scan(html, |_| true);
    let starts: BTreeMap<_, _> = read.tags.iter().map(|tag| (tag.span.start, tag)).collect();
    let masked = mask_raw_text_markup(html);
    let elements = read_elements(&masked.text);
    let mut edits = Vec::new();
    for (index, anchor) in elements.iter().enumerate() {
        if anchor.name != "a" {
            continue;
        }
        if let Some(mut found) = anchor_edits(&elements, index, &starts) {
            edits.append(&mut found);
        }
    }
    apply_edits(html, edits)
}

/// An element as the parser's lexical scopes nest it, with where it sits in the source.
struct Element<'s> {
    name: Cow<'s, str>,
    /// The offset of its start tag's `<`.
    start: usize,
    /// Just past the `>` of its own end tag, or of its start tag when that tag closed it. `None`
    /// when another tag, or the end of the document, closed it.
    closed_at: Option<usize>,
    /// Whether its start tag closed it: a void HTML element, or a self-closing SVG one.
    self_closed: bool,
    /// Whether its children are SVG elements.
    svg_children: bool,
    children: Vec<usize>,
}

/// Read the elements of `source` with their source offsets, nested as [`astral_html::Document`]
/// nests them: an end tag closes the innermost open element of its name and everything inside
/// it, a void HTML or self-closing SVG element closes at once, and a new HTML `a` or `option`
/// closes an immediately open one of the same name.
///
/// ~keep The document reports no source offsets, so its scope rules are followed here over the
/// ~keep same tokens. Each open name keeps its own stack of depths, so an end tag finds its element
/// ~keep without a walk down the open elements.
fn read_elements(source: &str) -> Vec<Element<'_>> {
    let mut reader = Reader::new(source);
    let mut elements: Vec<Element<'_>> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut open_by_name: HashMap<Cow<'_, str>, Vec<usize>> = HashMap::new();
    loop {
        let start = reader.position();
        let Some(token) = reader.next() else {
            break;
        };
        match token {
            Token::StartTag(tag) => {
                let in_svg = tag.name == "svg"
                    || (open.last().is_some_and(|parent| elements[*parent].svg_children) && !breaks_out_of_svg(&tag));
                if !in_svg
                    && matches!(tag.name.as_ref(), "a" | "option")
                    && open.last().is_some_and(|top| elements[*top].name == tag.name)
                {
                    let top = open.len() - 1;
                    close_to(&mut open, &mut open_by_name, &elements, top);
                }
                let self_closed = if in_svg { tag.self_closing } else { is_void(&tag.name) };
                let index = elements.len();
                if let Some(parent) = open.last() {
                    elements[*parent].children.push(index);
                }
                if !self_closed {
                    open_by_name.entry(tag.name.clone()).or_default().push(open.len());
                    open.push(index);
                }
                elements.push(Element {
                    svg_children: in_svg && !matches!(tag.name.as_ref(), "foreignobject" | "title" | "desc"),
                    name: tag.name,
                    start,
                    closed_at: self_closed.then(|| reader.position()),
                    self_closed,
                    children: Vec::new(),
                });
            }
            Token::EndTag(tag) => {
                if let Some(depth) = open_by_name.get(&tag.name).and_then(|depths| depths.last().copied()) {
                    elements[open[depth]].closed_at = Some(reader.position());
                    close_to(&mut open, &mut open_by_name, &elements, depth);
                }
            }
            _ => {}
        }
    }
    elements
}

/// Close the open elements from `depth` up.
fn close_to<'s>(
    open: &mut Vec<usize>,
    open_by_name: &mut HashMap<Cow<'s, str>, Vec<usize>>,
    elements: &[Element<'s>],
    depth: usize,
) {
    while open.len() > depth {
        let Some(index) = open.pop() else {
            break;
        };
        if let Some(depths) = open_by_name.get_mut(&elements[index].name) {
            depths.pop();
        }
    }
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// The HTML start tags that end SVG content, as the parser's reader reads them.
fn breaks_out_of_svg(tag: &astral_html::Tag<'_>) -> bool {
    match tag.name.as_ref() {
        "b" | "big" | "blockquote" | "body" | "br" | "center" | "code" | "dd" | "div" | "dl" | "dt" | "em"
        | "embed" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "head" | "hr" | "i" | "img" | "li" | "listing"
        | "menu" | "meta" | "nobr" | "ol" | "p" | "pre" | "ruby" | "s" | "small" | "span" | "strong" | "strike"
        | "sub" | "sup" | "table" | "tt" | "u" | "ul" | "var" => true,
        "font" => tag
            .attributes
            .iter()
            .any(|attribute| matches!(attribute.name.as_ref(), "color" | "face" | "size")),
        _ => false,
    }
}

fn anchor_edits(elements: &[Element<'_>], anchor: usize, starts: &BTreeMap<usize, StartTag<'_>>) -> Option<Vec<Edit>> {
    let span = closed_range(&elements[anchor], "a", starts)?;
    let start = starts.get(&span.start)?;
    if start.name != "a" {
        return None;
    }
    let mut edits = icon_edits(elements, anchor, starts)?;
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

fn icon_edits(elements: &[Element<'_>], anchor: usize, starts: &BTreeMap<usize, StartTag<'_>>) -> Option<Vec<Edit>> {
    let mut pending = elements[anchor].children.clone();
    let mut edits = Vec::new();
    while let Some(index) = pending.pop() {
        let element = &elements[index];
        match element.name.as_ref() {
            "a" => return None,
            "svg" => {
                let span = closed_range(element, "svg", starts)?;
                if starts.get(&span.start)?.name != "svg" {
                    return None;
                }
                if is_decorative(elements, index) {
                    // ~keep Empty wrappers suppress the converter's href-as-label fallback.
                    edits.push((span, "<span></span>".to_owned()));
                }
            }
            "script" | "style" | "template" => {}
            _ => pending.extend_from_slice(&element.children),
        }
    }
    Some(edits)
}

fn is_decorative(elements: &[Element<'_>], svg: usize) -> bool {
    let mut pending = elements[svg].children.clone();
    while let Some(index) = pending.pop() {
        let element = &elements[index];
        if !matches!(
            element.name.as_ref(),
            "svg"
                | "g"
                | "path"
                | "use"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "title"
                | "desc"
                | "defs"
                | "symbol"
        ) {
            return false;
        }
        pending.extend_from_slice(&element.children);
    }
    true
}

/// The source span of `element`, named `name`, from its start tag to its own end tag, or the
/// span of a self-closing `<svg/>` that the HTML read also reads as one. `None` when anything
/// else closed it.
fn closed_range(element: &Element<'_>, name: &str, starts: &BTreeMap<usize, StartTag<'_>>) -> Option<Range<usize>> {
    let span = element.start..element.closed_at?;
    if !element.self_closed {
        return Some(span);
    }
    (name == "svg"
        && starts
            .get(&span.start)
            .is_some_and(|start| start.name == "svg" && start.self_closing && start.span == span))
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
    fn should_keep_source_offsets_exact_around_multibyte_text_and_self_closing_tags() {
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
}
