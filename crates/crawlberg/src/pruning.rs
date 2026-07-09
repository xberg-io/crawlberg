//! Content pruning for fit markdown generation.
//!
//! Removes low-value content (navigation, footers, sidebars) to produce
//! a focused markdown optimized for LLM consumption.

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
        if link_ratio > 0.7 && trimmed.len() > 20 {
            continue;
        }

        if trimmed.len() < 5 && !trimmed.starts_with('#') {
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

fn count_link_chars(text: &str) -> usize {
    let mut count = 0;
    let mut in_link = false;
    for c in text.chars() {
        if c == '[' {
            in_link = true;
        }
        if in_link {
            count += 1;
        }
        if c == ')' && in_link {
            in_link = false;
        }
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
        assert!(result.contains("Yes"), "short list items should be kept");
    }
}
