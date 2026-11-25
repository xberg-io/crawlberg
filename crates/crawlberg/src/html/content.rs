//! Content processing: word count.

use astral_html::Document;

use super::select;
use super::selectors::SEL_BODY;

/// Compute the word count of visible text in the HTML body.
pub(crate) fn compute_word_count(dom: &Document<'_>) -> usize {
    select(dom, SEL_BODY)
        .next()
        .map_or(0, |body| body.text().split_whitespace().count())
}
