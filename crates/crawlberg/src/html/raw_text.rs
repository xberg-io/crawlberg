//! Raw-text element handling, applied to the source HTML before tl parses it.
//!
//! tl reads the content of raw-text elements (`script`, `title`, `xmp`, `plaintext` and the
//! rest) as markup, which a browser never does: tags written there become nodes, and a `<!--`
//! there hides every real tag up to the next `-->`. [`mask_raw_text_markup`] overwrites each `<`
//! of raw-text content with a space, where an HTML parser finds that content, so tl builds no
//! node from it and every extractor sees what a browser sees.
//!
//! The masked string has the source's byte length, and differs from it only at `<` bytes inside
//! raw-text content. Byte offsets into it address the same bytes in the source, which is what
//! the markdown URL-splicing path relies on.

use std::borrow::Cow;
use std::ops::Range;

use memchr::memchr;
use tracing::debug;

use super::start_tags::{Kept, scan};

/// Byte written over a `<` inside raw-text content.
///
/// ~keep A single ASCII byte, so the masked string keeps the source's byte length and
/// every offset into it. A space also cannot combine with the following bytes into a
/// character reference the way `&` could.
const MARKUP_MASK: char = ' ';

/// A document with its raw-text markup masked, and the base address an HTML parser reads in it.
pub(crate) struct MaskedHtml<'h> {
    /// The source with every `<` inside raw-text content overwritten.
    pub(crate) text: Cow<'h, str>,
    /// The decoded `href` of the first `<base>` in the document that has one.
    pub(crate) base_href: Option<String>,
}

/// Mask `source` for link extraction: read with scripting off, as a crawler that runs no script
/// fetches a page, so `<noscript>` content is markup.
pub(crate) fn mask_raw_text_markup(source: &str) -> MaskedHtml<'_> {
    let read = scan(source, false, |_| None::<Kept<()>>);
    MaskedHtml {
        text: mask(source, &read.raw_text),
        base_href: read.base_href,
    }
}

/// Overwrite every `<` inside `raw_text` with a space.
///
/// Returns the source unchanged (and unallocated) when no range holds a `<`.
pub(super) fn mask<'h>(source: &'h str, raw_text: &[Range<usize>]) -> Cow<'h, str> {
    let bytes = source.as_bytes();
    let mut regions = raw_text
        .iter()
        .filter(|region| memchr(b'<', &bytes[(*region).clone()]).is_some())
        .peekable();
    if regions.peek().is_none() {
        return Cow::Borrowed(source);
    }

    let mut masked = String::with_capacity(source.len());
    let mut cursor = 0;
    let mut count = 0usize;
    for region in regions {
        masked.push_str(&source[cursor..region.start]);
        masked.extend(
            source[region.clone()]
                .chars()
                .map(|character| if character == '<' { MARKUP_MASK } else { character }),
        );
        cursor = region.end;
        count += 1;
    }
    masked.push_str(&source[cursor..]);
    debug!(regions = count, "masked markup inside raw-text element content");
    Cow::Owned(masked)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn should_leave_html_without_raw_text_elements_untouched() {
        let html = r#"<html><body><a href="/x">x</a><!-- <b> --><p>1 < 2</p></body></html>"#;
        let masked = mask_raw_text_markup(html).text;
        assert_eq!(masked, html, "nothing to mask, so the source should come back as-is");
        assert!(
            matches!(masked, Cow::Borrowed(_)),
            "an untouched document should not be reallocated"
        );
    }

    #[test]
    fn should_mask_a_comment_opener_inside_script_text() {
        let html = r#"<script>var a = "<!--";</script><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<script>var a = " !--";</script><a href="/real">r</a>"#,
            "the `<` of a comment opener inside script text should become a space"
        );
    }

    #[test]
    fn should_mask_every_markup_open_in_each_raw_text_element() {
        let html = r#"<style><a href="/s"></style><textarea><b></textarea><title><i></title>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<style> a href="/s"></style><textarea> b></textarea><title> i></title>"#,
            "style, textarea and title content should all be masked"
        );
    }

    #[test]
    fn should_preserve_the_byte_length_of_the_source() {
        let html = r#"<script>"<a href=\"/x\">" + '<div>'</script><p>after</p>"#;
        let masked = mask_raw_text_markup(html).text;
        assert_eq!(
            masked.len(),
            html.len(),
            "masking must not change the byte length, got {} vs {}",
            masked.len(),
            html.len()
        );
    }

    #[test]
    fn should_not_mask_past_the_end_tag_of_a_raw_text_element() {
        let html = r#"<script><a></script><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<script> a></script><a href="/real">r</a>"#,
            "only the element's content should be masked, never the markup after it"
        );
    }

    #[test]
    fn should_end_a_raw_text_element_at_an_uppercase_end_tag() {
        let html = r#"<SCRIPT><b></SCRIPT ><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<SCRIPT> b></SCRIPT ><a href="/real">r</a>"#,
            "end-tag matching should ignore case and allow trailing whitespace"
        );
    }

    #[test]
    fn should_not_end_a_raw_text_element_at_a_longer_tag_name() {
        let html = r#"<script></scriptish><a href="/x"></script><b>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<script> /scriptish> a href="/x"></script><b>"#,
            "`</scriptish>` should not close a `<script>`"
        );
    }

    #[test]
    fn should_treat_the_rest_of_the_document_as_content_when_the_end_tag_is_missing() {
        let html = r#"<p>before</p><script>var a = 1;<a href="/x">"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<p>before</p><script>var a = 1; a href="/x">"#,
            "an unterminated raw-text element runs to the end of the document, as in a browser"
        );
    }

    #[test]
    fn should_not_end_raw_text_at_an_end_tag_cut_off_by_the_end_of_input() {
        assert_eq!(
            mask_raw_text_markup(r#"<title><a href="/in"></title"#).text,
            r#"<title> a href="/in"> /title"#,
            "`</title` with nothing after it is title text, as in a browser"
        );
    }

    #[test]
    fn should_not_start_a_raw_text_element_from_inside_a_comment() {
        // ~keep The `>` before the `<script>` matters: it makes this fail unless the comment is
        // ~keep skipped to `-->`, instead of merely to the first `>`.
        let html = r#"<!-- a > b <script> --><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            html,
            "a `<script>` inside a comment is not an element"
        );
    }

    #[test]
    fn should_start_raw_text_from_a_self_closed_script() {
        let html = r#"<script/><a href="/in">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<script/> a href="/in">r /a>"#,
            "an HTML parser ignores the `/` of `<script/>`, so script text follows it"
        );
    }

    #[test]
    fn should_mask_noscript_only_with_scripting_on() {
        let html = r#"<noscript><a href="/in"></noscript><a href="/real">"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            html,
            "link extraction reads `<noscript>` as markup, as a browser without scripting does"
        );
        let with_scripting = scan(html, true, |_| None::<Kept<()>>);
        assert_eq!(
            mask(html, &with_scripting.raw_text),
            r#"<noscript> a href="/in"></noscript><a href="/real">"#,
            "with scripting on, `<noscript>` content is raw text"
        );
    }

    #[test]
    fn should_mask_every_raw_text_element_a_browser_reads_as_text() {
        for element in ["xmp", "iframe", "noembed", "noframes"] {
            let html = format!(r#"<{element}><a href="/in"></{element}><a href="/real">"#);
            assert_eq!(
                mask_raw_text_markup(&html).text,
                format!(r#"<{element}> a href="/in"></{element}><a href="/real">"#),
                "element {element}"
            );
        }
        assert_eq!(
            mask_raw_text_markup(r#"<a href="/real"><plaintext><a href="/in"></plaintext>"#).text,
            r#"<a href="/real"><plaintext> a href="/in"> /plaintext>"#,
            "plaintext content runs to the end of the input"
        );
    }

    #[test]
    fn should_mask_script_inside_svg_foreign_object() {
        let html = r#"<svg><foreignObject><script><a href="/in"></script></foreignObject></svg><a href="/real">"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<svg><foreignObject><script> a href="/in"></script></foreignObject></svg><a href="/real">"#,
            "`foreignObject` holds HTML, so a script inside it is raw text"
        );
    }

    #[test]
    fn should_mask_style_inside_a_mathml_html_annotation() {
        let html =
            r#"<math><annotation-xml encoding="text/html"><style><a href="/in"></style></annotation-xml></math>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<math><annotation-xml encoding="text/html"><style> a href="/in"></style></annotation-xml></math>"#,
            "an HTML annotation holds HTML, so a style inside it is raw text"
        );
    }

    #[test]
    fn should_end_a_double_escaped_script_at_its_second_end_tag() {
        let html = r#"<script><!--<script></script><a href="/in"></script><a href="/real">"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<script> !-- script> /script> a href="/in"></script><a href="/real">"#,
            "inside `<!--<script>` the first `</script>` does not end the script"
        );
    }

    #[test]
    fn should_not_open_a_quote_inside_an_unquoted_attribute_value() {
        let html = r#"<p b=x'y>one</p><i c='><title>' >two</i><a href="/real">"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            html,
            "the `'` in `x'y` is part of the value, so `<title>` sits inside a quoted value"
        );
    }

    #[test]
    fn should_not_apply_the_raw_text_rule_inside_foreign_content() {
        let html = r#"<svg><title><a href="/x">t</a></title></svg><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            html,
            "inside SVG the tokenizer stays in the data state, so `title` is not raw text"
        );
    }

    #[test]
    fn should_resume_the_raw_text_rule_after_foreign_content_closes() {
        let html = r#"<svg></svg><script><a href="/x"></script>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<svg></svg><script> a href="/x"></script>"#,
            "a closed `<svg>` should restore raw-text handling"
        );
    }

    #[test]
    fn should_not_end_a_tag_at_a_greater_than_inside_a_quoted_attribute_value() {
        let html = r#"<script data-x="a>b"><a href="/x"></script>"#;
        assert_eq!(
            mask_raw_text_markup(html).text,
            r#"<script data-x="a>b"> a href="/x"></script>"#,
            "a `>` inside a quoted attribute value should not end the start tag"
        );
    }

    proptest! {
        /// Masking is a no-op on documents with no raw-text element, whatever they contain.
        #[test]
        fn masking_is_the_identity_without_raw_text_elements(
            html in r#"(<[a-z]{1,4}( [a-z]{1,3}="[^"<>]{0,6}")?/?>|</[a-z]{1,4}>|<!--[^<>-]{0,6}-->|[a-z0-9 <>&;"'/!?=-]){0,40}"#
        ) {
            prop_assume!(!contains_raw_text_element(&html));
            let masked = mask_raw_text_markup(&html).text;
            prop_assert_eq!(masked.as_ref(), html.as_str());
        }

        /// Masking is idempotent and length-preserving on arbitrary markup-ish input.
        #[test]
        fn masking_is_idempotent_and_length_preserving(
            html in MARKUP_ISH
        ) {
            let once = mask_raw_text_markup(&html).text.into_owned();
            prop_assert_eq!(once.len(), html.len(), "masking changed the byte length");
            let twice = mask_raw_text_markup(&once).text.into_owned();
            prop_assert_eq!(&twice, &once, "masking is not idempotent");
        }

        /// An HTML parser reads the masked copy as it reads the source: the same tags, raw text
        /// and base address, with scripting on and off.
        #[test]
        fn masking_keeps_the_html5ever_reading(html in MARKUP_ISH, scripting in any::<bool>()) {
            let source = scan(&html, scripting, |_| Some(&[("href", ())]));
            let masked = mask(&html, &source.raw_text);
            let reread = scan(&masked, scripting, |_| Some(&[("href", ())]));
            prop_assert_eq!(&reread.tags, &source.tags);
            prop_assert_eq!(&reread.raw_text, &source.raw_text);
            prop_assert_eq!(&reread.base_href, &source.base_href);
        }
    }

    /// Markup-ish input built from every element an HTML parser may read as raw text, foreign
    /// content and its integration points, comments and `<base href>`.
    const MARKUP_ISH: &str = r#"(<script>|<script/>|</script>|<style>|</style>|<title>|</title>|<textarea>|</textarea>|<xmp>|</xmp>|<iframe>|</iframe>|<noembed>|</noembed>|<noframes>|</noframes>|<noscript>|</noscript>|<plaintext>|<template>|</template>|<svg>|</svg>|<foreignObject>|<math>|</math>|<!--|-->|<a href="/x">|<base href="/b/">|</a>|&lt;|[a-z0-9 <>"'/!=-]){0,60}"#;

    /// Whether `html` opens any element an HTML parser reads as raw text with scripting off.
    fn contains_raw_text_element(html: &str) -> bool {
        let html = html.to_ascii_lowercase();
        [
            "script",
            "style",
            "title",
            "textarea",
            "xmp",
            "iframe",
            "noembed",
            "noframes",
            "plaintext",
        ]
        .iter()
        .any(|name| html.contains(&format!("<{name}")))
    }
}
