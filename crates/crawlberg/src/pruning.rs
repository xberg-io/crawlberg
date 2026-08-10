//! Content pruning for fit markdown generation.
//!
//! Removes low-value content (navigation, footers, sidebars) to produce
//! a focused markdown optimized for LLM consumption.

/// Line length (bytes) below which a non-list, non-heading line is treated as
/// insignificant boilerplate (e.g. a bare copyright mark).
const MIN_MEANINGFUL_LINE_LEN: usize = 5;

/// Link-density ratio above which a line is treated as a navigation block and dropped.
const LINK_DENSITY_THRESHOLD: f64 = 0.7;

/// Minimum line length before the link-density check applies, so a handful of
/// characters around a short real link don't get penalized by a noisy ratio.
const MIN_LENGTH_FOR_LINK_DENSITY_CHECK: usize = 20;

/// Generate fit markdown by removing low-value content.
///
/// Heuristic-based pruning:
/// - Remove lines that are mostly links (navigation)
/// - Remove very short lines (breadcrumbs, copyright)
/// - Keep paragraphs with substantial text content
/// - Preserve code blocks even if lines are short
pub fn generate_fit_markdown(markdown: &str) -> String {
    let mut fit_lines = Vec::new();
    let mut in_code_block = false;

    for line in markdown.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            fit_lines.push(trimmed);
            continue;
        }

        if in_code_block {
            fit_lines.push(trimmed);
            continue;
        }

        if trimmed.is_empty() {
            if fit_lines.last().map(|l: &&str| !l.is_empty()).unwrap_or(true) {
                fit_lines.push("");
            }
            continue;
        }

        let link_ratio = count_link_chars(trimmed) as f64 / trimmed.len().max(1) as f64;
        if link_ratio > LINK_DENSITY_THRESHOLD && trimmed.len() > MIN_LENGTH_FOR_LINK_DENSITY_CHECK {
            continue;
        }

        if trimmed.len() < MIN_MEANINGFUL_LINE_LEN && !trimmed.starts_with('#') && !is_list_item(trimmed) {
            continue;
        }

        let lower = trimmed.to_lowercase();
        if is_boilerplate(&lower) {
            continue;
        }

        fit_lines.push(trimmed);
    }

    while fit_lines.last() == Some(&"") {
        fit_lines.pop();
    }

    fit_lines.join("\n")
}

/// Whether a trimmed line is a markdown list item (bullet or ordered), which must
/// survive the short-line filter regardless of its byte length.
fn is_list_item(trimmed: &str) -> bool {
    if let Some(rest) = ['-', '*', '+'].iter().find_map(|marker| trimmed.strip_prefix(*marker)) {
        return rest.is_empty() || rest.starts_with(' ');
    }
    let digit_count = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digit_count == 0 {
        return false;
    }
    let rest = &trimmed[digit_count..];
    rest.starts_with(". ") || rest.starts_with(") ")
}

/// Count bytes that fall within genuine markdown links (`[text](url)`).
///
/// Only a `[` immediately followed by a matching `]` and then `(...)` counts as a
/// link span. A prior implementation entered "in a link" on any `[` and stayed
/// there until the *next* `)` anywhere in the line, so citation markers like
/// `[1]` followed later by an unrelated `(...)` (e.g. a parenthetical year) were
/// wrongly counted as link text, inflating the density score of ordinary prose.
fn count_link_chars(text: &str) -> usize {
    let mut count = 0;
    let mut search_from = 0;

    while let Some(open_rel) = text[search_from..].find('[') {
        let open = search_from + open_rel;
        let Some(close_bracket_rel) = text[open + 1..].find(']') else {
            break;
        };
        let close_bracket = open + 1 + close_bracket_rel;
        let after_bracket = close_bracket + 1;

        if text[after_bracket..].starts_with('(')
            && let Some(close_paren_rel) = text[after_bracket + 1..].find(')')
        {
            let close_paren = after_bracket + 1 + close_paren_rel;
            count += close_paren + 1 - open;
            search_from = close_paren + 1;
            continue;
        }

        search_from = open + 1;
    }

    count
}

fn is_boilerplate(lower: &str) -> bool {
    let patterns = [
        "cookie policy",
        "cookie consent",
        "use cookies",
        "uses cookies",
        "privacy policy",
        "terms of service",
        "all rights reserved",
        "copyright",
        "\u{00a9}",
        "subscribe to",
        "sign up for",
        "follow us",
        "share this",
        "powered by",
        "back to top",
    ];
    patterns.iter().any(|p| lower.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_removes_nav_links() {
        let md = "# Title\n\nGood paragraph with substantial content here.\n\n[Home](/) | [About](/about) | [Contact](/contact) | [Blog](/blog)\n\nAnother good paragraph.";
        let result = generate_fit_markdown(md);
        assert!(result.contains("Good paragraph"));
        assert!(result.contains("Another good paragraph"));
        assert!(!result.contains("[Home]"));
    }

    #[test]
    fn test_removes_short_lines() {
        let md = "# Title\n\nGood content here with enough text.\n\n\u{00a9} 2024\n\nMore content.";
        let result = generate_fit_markdown(md);
        assert!(result.contains("Good content"));
        assert!(!result.contains("\u{00a9} 2024"));
    }

    #[test]
    fn test_keeps_headings() {
        let md = "# Title\n\n## Section\n\nContent.";
        let result = generate_fit_markdown(md);
        assert!(result.contains("# Title"));
        assert!(result.contains("## Section"));
    }

    #[test]
    fn test_removes_boilerplate() {
        let md = "Good content.\n\nSubscribe to our newsletter\n\nMore content.";
        let result = generate_fit_markdown(md);
        assert!(!result.contains("Subscribe"));
    }

    #[test]
    fn test_preserves_code_blocks() {
        let md = "# Title\n\nGood content here.\n\n```rust\nfn main() {\n    x\n}\n```\n\nMore content.";
        let result = generate_fit_markdown(md);
        assert!(result.contains("fn main()"), "code should be preserved");
        assert!(result.contains("x"), "short code lines should be kept");
    }

    #[test]
    fn test_keeps_short_list_items() {
        let md = "# FAQ\n\n- Yes\n- No\n- Maybe";
        let result = generate_fit_markdown(md);
        assert_eq!(
            result, "# FAQ\n\n- Yes\n- No\n- Maybe",
            "all short list items (including '- No', 4 bytes) should survive, got {result:?}"
        );
    }

    #[test]
    fn test_short_non_list_line_is_still_dropped() {
        let md = "# Title\n\nGood content here with enough text.\n\nHome\n\nMore content.";
        let result = generate_fit_markdown(md);
        assert!(
            !result.contains("Home"),
            "a bare short breadcrumb word with no list marker should still be dropped, got {result:?}"
        );
    }

    #[test]
    fn test_citation_heavy_prose_survives_pruning() {
        let md = "# Findings\n\nAccording to recent research [1], climate models show significant \
                   warming trends across multiple regions (see Figure 2), with increasing \
                   confidence intervals reported in the literature [2][3].";
        let result = generate_fit_markdown(md);
        assert!(
            result.contains("According to recent research"),
            "citation-heavy prose must not be deleted by the link-density heuristic, got {result:?}"
        );
        assert!(
            result.contains("[2][3]"),
            "citation markers should remain part of the surviving line, got {result:?}"
        );
    }

    #[test]
    fn test_dense_real_link_navigation_is_still_pruned() {
        let md = "# Title\n\nGood paragraph with substantial content here.\n\n\
                   [Home](/) [About](/about) [Contact](/contact) [Blog](/blog) [Docs](/docs)\n\n\
                   Another good paragraph.";
        let result = generate_fit_markdown(md);
        assert!(
            !result.contains("[Home]"),
            "a line that is genuinely mostly real markdown links must still be pruned, got {result:?}"
        );
    }

    #[test]
    fn count_link_chars_ignores_citation_brackets_followed_by_unrelated_parens() {
        let text = "See [1] for details (published 2020) and also [2] (revised 2021).";
        assert_eq!(
            count_link_chars(text),
            0,
            "no `[text](url)` markdown link is present, so link char count must be 0"
        );
    }

    #[test]
    fn count_link_chars_counts_only_real_link_spans() {
        let text = "prefix [a](b) middle [c](d) suffix";
        assert_eq!(
            count_link_chars(text),
            "[a](b)".len() + "[c](d)".len(),
            "should count exactly the two real link spans, not the text between them"
        );
    }
}
