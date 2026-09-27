//! Raw-text element handling, applied to the source HTML before it is parsed.
//!
//! `tl` has no notion of raw-text elements: it parses the contents of `script`,
//! `style`, `textarea` and `title` as markup, which a browser never does. That has two
//! opposite consequences, and both are cured here instead of in each extractor:
//!
//! - A `<!--` inside raw text starts a comment for `tl`, so every tag up to the next
//!   `-->` is swallowed and never reaches the tree at all. No filtering of the parsed
//!   tree can recover a node that was never built.
//! - Tags written *inside* raw text — `document.write("<a href=...>")`, a `<base href>`
//!   in title text — become real nodes and are picked up by link, image, feed and meta
//!   extraction.
//!
//! [`mask_raw_text_markup`] walks the source with a small tokenizer, finds the content of
//! each raw-text element the way the HTML5 tokenizer does, and overwrites every `<` in
//! that content with a space. Afterwards no raw-text content contains a `<`, so `tl`
//! cannot build a node — or start a comment — from it, and every extractor is fixed at
//! once with no change to its signature.
//!
//! Only `<` is rewritten, so raw-text content a consumer legitimately reads — a `<title>`'s
//! text, a `<script type="application/ld+json">` payload — survives unless it contains a
//! literal `<`, which in valid HTML is written `&lt;` and is left alone. Content that does
//! carry a literal `<` is already mis-parsed today, because `tl` turns it into tags.
//!
//! The rewrite replaces one ASCII byte with another, so the result has exactly the same
//! byte length as the source and every byte outside raw-text content is untouched. Byte
//! offsets computed against the masked string therefore address the same bytes in the
//! original, which is what the markdown URL-splicing path relies on.

use std::borrow::Cow;
use std::ops::Range;

use memchr::{memchr, memmem};
use tracing::debug;

/// Elements whose content the HTML5 tokenizer reads as raw text (`RAWTEXT`) or as
/// escapable raw text (`RCDATA`) when they appear in the HTML namespace.
///
/// ~keep `noscript` is deliberately absent: its content is raw text only when scripting
/// is enabled, and a crawler's HTTP fetch has no scripting, so the markup reading is the
/// right one — it is also how `<noscript><img src=…></noscript>` tracking pixels stay
/// discoverable. `iframe[srcdoc]` needs nothing: its HTML lives in a quoted attribute
/// value, which no parser here reads as markup.
const RAW_TEXT_ELEMENTS: [&[u8]; 4] = [b"script", b"style", b"textarea", b"title"];

/// Elements that put the tokenizer into foreign content.
///
/// ~keep Inside SVG and MathML the tokenizer stays in the data state, so a browser also
/// parses the contents of `svg script`, `svg style` and `svg title` as markup. Suspending
/// the raw-text rule there matches browsers and keeps this pass from masking content that
/// really is markup.
const FOREIGN_ELEMENTS: [&[u8]; 2] = [b"svg", b"math"];

/// Byte written over a `<` inside raw-text content.
///
/// ~keep A single ASCII byte, so the masked string keeps the source's byte length and
/// every offset into it. A space also cannot combine with the following bytes into a
/// character reference the way `&` could.
const MARKUP_MASK: char = ' ';

/// Overwrite every `<` inside the content of a raw-text element with a space.
///
/// Returns the source unchanged (and unallocated) when no raw-text element contains a
/// `<`. The returned string always has the same byte length as `source`, and differs from
/// it only at `<` bytes inside raw-text content.
pub(crate) fn mask_raw_text_markup(source: &str) -> Cow<'_, str> {
    let regions = markup_bearing_regions(source);
    if regions.is_empty() {
        return Cow::Borrowed(source);
    }

    let mut masked = String::with_capacity(source.len());
    let mut cursor = 0;
    for region in &regions {
        masked.push_str(&source[cursor..region.start]);
        for character in source[region.start..region.end].chars() {
            masked.push(if character == '<' { MARKUP_MASK } else { character });
        }
        cursor = region.end;
    }
    masked.push_str(&source[cursor..]);
    debug!(regions = regions.len(), "masked markup inside raw-text element content");
    Cow::Owned(masked)
}

/// What the scan does with the `<` it is looking at.
enum Step {
    /// Nothing to mask; carry on at this offset.
    Resume(usize),
    /// Raw-text content occupying this byte range.
    RawText(Range<usize>),
    /// A foreign-content element opened; carry on at this offset.
    EnterForeign(usize),
    /// A foreign-content element closed; carry on at this offset.
    LeaveForeign(usize),
}

/// The byte ranges of raw-text content that contain at least one `<`.
fn markup_bearing_regions(source: &str) -> Vec<Range<usize>> {
    let bytes = source.as_bytes();
    let mut regions: Vec<Range<usize>> = Vec::new();
    let mut foreign_depth: usize = 0;
    let mut cursor = 0;

    while let Some(offset) = bytes.get(cursor..).and_then(|rest| memchr(b'<', rest)) {
        let at = cursor + offset;
        let next = match classify(bytes, at, foreign_depth > 0) {
            Step::Resume(next) => next,
            Step::EnterForeign(next) => {
                foreign_depth += 1;
                next
            }
            Step::LeaveForeign(next) => {
                foreign_depth -= 1;
                next
            }
            Step::RawText(content) => {
                let resume = content.end;
                if memchr(b'<', &bytes[content.clone()]).is_some() {
                    regions.push(content);
                }
                resume
            }
        };
        // ~keep The scan must move past this `<` even for input no branch understands,
        // or a malformed tag turns the loop into a spin.
        cursor = next.max(at + 1);
    }
    regions
}

/// Decide what the markup starting at `at` (a `<`) means for the scan.
fn classify(bytes: &[u8], at: usize, in_foreign: bool) -> Step {
    let rest = &bytes[at..];

    if rest.starts_with(b"<!--") {
        return Step::Resume(end_of_comment(bytes, at + 4));
    }
    if rest.starts_with(b"<!") || rest.starts_with(b"<?") {
        return Step::Resume(end_of_tag(bytes, at + 2).0);
    }
    if rest.starts_with(b"</") {
        let (name, after_name) = tag_name(bytes, at + 2);
        let next = end_of_tag(bytes, after_name).0;
        if in_foreign && named_in(name, &FOREIGN_ELEMENTS) {
            return Step::LeaveForeign(next);
        }
        return Step::Resume(next);
    }

    let (name, after_name) = tag_name(bytes, at + 1);
    if name.is_empty() {
        return Step::Resume(at + 1);
    }
    let (next, self_closing) = end_of_tag(bytes, after_name);
    if named_in(name, &FOREIGN_ELEMENTS) {
        return if self_closing {
            Step::Resume(next)
        } else {
            Step::EnterForeign(next)
        };
    }
    if in_foreign || self_closing || !named_in(name, &RAW_TEXT_ELEMENTS) {
        return Step::Resume(next);
    }
    // ~keep HTML5 ends a raw-text element at its first end tag even inside a quoted
    // string, and treats the rest of the document as its content when there is none.
    let content_end = end_tag_offset(bytes, next, name).unwrap_or_else(|| {
        debug!(
            element = %String::from_utf8_lossy(name),
            offset = at,
            "raw-text element has no end tag; treating the rest of the document as its content"
        );
        bytes.len()
    });
    Step::RawText(next..content_end)
}

/// Whether `name` case-insensitively equals one of `candidates`.
fn named_in(name: &[u8], candidates: &[&[u8]]) -> bool {
    candidates.iter().any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Offset just past a comment's `-->`, or the end of input when it has none.
///
/// ~keep `tl` ends a comment at the first `-->` and swallows to end of input without
/// one. Matching that exactly is what keeps this scan and `tl`'s tree agreeing on which
/// `<script>` is real and which sits inside a comment.
fn end_of_comment(bytes: &[u8], from: usize) -> usize {
    match bytes.get(from..).and_then(|rest| memmem::find(rest, b"-->")) {
        Some(offset) => from + offset + b"-->".len(),
        None => bytes.len(),
    }
}

/// The tag name starting at `from`, and the offset just past it.
///
/// An empty name means `from` does not begin one, which HTML5 reads as text rather than
/// as a tag.
fn tag_name(bytes: &[u8], from: usize) -> (&[u8], usize) {
    if !bytes.get(from).is_some_and(u8::is_ascii_alphabetic) {
        return (&[], from);
    }
    let mut end = from + 1;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
    {
        end += 1;
    }
    (&bytes[from..end], end)
}

/// Offset just past a tag's `>`, and whether the tag ended with `/>`.
///
/// Quoted attribute values are skipped, so a `>` inside one does not end the tag.
fn end_of_tag(bytes: &[u8], from: usize) -> (usize, bool) {
    let mut index = from;
    let mut quote: Option<u8> = None;
    let mut last_significant = 0u8;

    while let Some(&byte) = bytes.get(index) {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return (index + 1, last_significant == b'/'),
            None => {}
        }
        if !byte.is_ascii_whitespace() {
            last_significant = byte;
        }
        index += 1;
    }
    (bytes.len(), false)
}

/// Offset of the `</name` that closes a raw-text element opened at `from`.
///
/// Per HTML5 the name must be followed by whitespace, `/` or `>`, so `</scriptish>` does
/// not close a `<script>`.
fn end_tag_offset(bytes: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut index = from;
    while let Some(offset) = bytes.get(index..).and_then(|rest| memchr(b'<', rest)) {
        let at = index + offset;
        let name_start = at + 2;
        let name_end = name_start + name.len();
        if bytes.get(at + 1) == Some(&b'/')
            && bytes
                .get(name_start..name_end)
                .is_some_and(|found| found.eq_ignore_ascii_case(name))
            && bytes
                .get(name_end)
                .is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'))
        {
            return Some(at);
        }
        index = at + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn should_leave_html_without_raw_text_elements_untouched() {
        let html = r#"<html><body><a href="/x">x</a><!-- <b> --><p>1 < 2</p></body></html>"#;
        let masked = mask_raw_text_markup(html);
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
            mask_raw_text_markup(html),
            r#"<script>var a = " !--";</script><a href="/real">r</a>"#,
            "the `<` of a comment opener inside script text should become a space"
        );
    }

    #[test]
    fn should_mask_every_markup_open_in_each_raw_text_element() {
        let html = r#"<style><a href="/s"></style><textarea><b></textarea><title><i></title>"#;
        assert_eq!(
            mask_raw_text_markup(html),
            r#"<style> a href="/s"></style><textarea> b></textarea><title> i></title>"#,
            "style, textarea and title content should all be masked"
        );
    }

    #[test]
    fn should_preserve_the_byte_length_of_the_source() {
        let html = r#"<script>"<a href=\"/x\">" + '<div>'</script><p>after</p>"#;
        let masked = mask_raw_text_markup(html);
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
            mask_raw_text_markup(html),
            r#"<script> a></script><a href="/real">r</a>"#,
            "only the element's content should be masked, never the markup after it"
        );
    }

    #[test]
    fn should_end_a_raw_text_element_at_an_uppercase_end_tag() {
        let html = r#"<SCRIPT><b></SCRIPT ><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html),
            r#"<SCRIPT> b></SCRIPT ><a href="/real">r</a>"#,
            "end-tag matching should ignore case and allow trailing whitespace"
        );
    }

    #[test]
    fn should_not_end_a_raw_text_element_at_a_longer_tag_name() {
        let html = r#"<script></scriptish><a href="/x"></script><b>"#;
        assert_eq!(
            mask_raw_text_markup(html),
            r#"<script> /scriptish> a href="/x"></script><b>"#,
            "`</scriptish>` should not close a `<script>`"
        );
    }

    #[test]
    fn should_treat_the_rest_of_the_document_as_content_when_the_end_tag_is_missing() {
        let html = r#"<p>before</p><script>var a = 1;<a href="/x">"#;
        assert_eq!(
            mask_raw_text_markup(html),
            r#"<p>before</p><script>var a = 1; a href="/x">"#,
            "an unterminated raw-text element runs to the end of the document, as in a browser"
        );
    }

    #[test]
    fn should_not_start_a_raw_text_element_from_inside_a_comment() {
        // ~keep The `>` before the `<script>` matters: it makes this fail unless the comment is
        // ~keep skipped to `-->`, instead of merely to the first `>`.
        let html = r#"<!-- a > b <script> --><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html),
            html,
            "a `<script>` inside a comment is not an element"
        );
    }

    #[test]
    fn should_not_start_a_raw_text_element_from_a_self_closed_tag() {
        let html = r#"<script/><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html),
            html,
            "`<script/>` is treated as closed here, so nothing follows it as raw text"
        );
    }

    #[test]
    fn should_not_apply_the_raw_text_rule_inside_foreign_content() {
        let html = r#"<svg><title><a href="/x">t</a></title></svg><a href="/real">r</a>"#;
        assert_eq!(
            mask_raw_text_markup(html),
            html,
            "inside SVG the tokenizer stays in the data state, so `title` is not raw text"
        );
    }

    #[test]
    fn should_resume_the_raw_text_rule_after_foreign_content_closes() {
        let html = r#"<svg></svg><script><a href="/x"></script>"#;
        assert_eq!(
            mask_raw_text_markup(html),
            r#"<svg></svg><script> a href="/x"></script>"#,
            "a closed `<svg>` should restore raw-text handling"
        );
    }

    #[test]
    fn should_not_end_a_tag_at_a_greater_than_inside_a_quoted_attribute_value() {
        let html = r#"<script data-x="a>b"><a href="/x"></script>"#;
        assert_eq!(
            mask_raw_text_markup(html),
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
            let masked = mask_raw_text_markup(&html);
            prop_assert_eq!(masked.as_ref(), html.as_str());
        }

        /// Masking is idempotent and length-preserving on arbitrary markup-ish input.
        #[test]
        fn masking_is_idempotent_and_length_preserving(
            html in r#"(<script>|</script>|<style>|</style>|<title>|</title>|<textarea>|</textarea>|<svg>|</svg>|<!--|-->|<a href="/x">|</a>|[a-z0-9 <>"'/!-]){0,60}"#
        ) {
            let once = mask_raw_text_markup(&html).into_owned();
            prop_assert_eq!(once.len(), html.len(), "masking changed the byte length");
            let twice = mask_raw_text_markup(&once).into_owned();
            prop_assert_eq!(&twice, &once, "masking is not idempotent");
        }
    }

    /// Whether `html` opens any element this pass treats as raw text.
    fn contains_raw_text_element(html: &str) -> bool {
        RAW_TEXT_ELEMENTS.iter().any(|name| {
            let opener = format!("<{}", String::from_utf8_lossy(name));
            html.to_ascii_lowercase().contains(&opener)
        })
    }
}
