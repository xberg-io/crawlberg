//! Robots.txt parsing and path-matching logic.
//!
//! Substrate-level surface for robots.txt — usable without the full crawl
//! engine. Parse a body with [`parse_robots_txt`], inspect [`RobotsRules`],
//! and test paths with [`is_path_allowed`]. The engine integrates these
//! automatically; this module is exposed so OSS users can build their own
//! fetcher on top of the same logic the engine uses.
//!
//! ```
//! use crawlberg::robots::{parse_robots_txt, is_path_allowed};
//!
//! let body = "User-agent: *\nDisallow: /private\nCrawl-delay: 2";
//! let rules = parse_robots_txt(body, "crawlberg");
//! assert!(!is_path_allowed("/private/secret", &rules));
//! assert!(is_path_allowed("/public", &rules));
//! assert_eq!(rules.crawl_delay, Some(2));
//! ```

pub(crate) use crawlberg_robots::product_token_addresses_us;
pub use crawlberg_robots::{RobotsRules, is_path_allowed, parse_robots_txt};

/// `body` as the robots.txt block-page check reads it: without its whole-line comments, or
/// unchanged when it holds a `<`.
///
/// ~keep A robots.txt comment is written for a human reader and can say anything, such as "AI
/// crawlers are blocked below" (crawlberg#507). Only a line whose first non-space character is
/// `#` is left out. A trailing comment stays, because in an HTML page a `#` in a style rule or a
/// link can come before the block phrase on the same line. A body with any `<` is read whole:
/// robots.txt has no use for `<`, and in an HTML page a style rule can also start a line with
/// `#`. This only picks the lines the fingerprint sees; [`parse_robots_txt`] reads comments its
/// own way.
pub(crate) fn fingerprint_text(body: &str) -> std::borrow::Cow<'_, str> {
    if body.contains('<') {
        return std::borrow::Cow::Borrowed(body);
    }
    let mut text = String::with_capacity(body.len());
    for line in body.lines().filter(|line| !line.trim_start().starts_with('#')) {
        text.push_str(line);
        text.push('\n');
    }
    std::borrow::Cow::Owned(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_text_drops_only_whole_line_comments_and_reads_markup_whole() {
        for (body, expected) in [
            (
                "# AI crawlers are blocked below\nUser-agent: GPTBot\nDisallow: /\n",
                "User-agent: GPTBot\nDisallow: /\n",
            ),
            ("  # blocked\r\nDisallow: /private\r\n", "Disallow: /private\n"),
            ("Disallow: /private # blocked\n", "Disallow: /private # blocked\n"),
            (
                "Sorry, you have been blocked\nUser-agent: *\nAllow: /\n",
                "Sorry, you have been blocked\nUser-agent: *\nAllow: /\n",
            ),
            (
                "<style>\n#blocked-msg { color: red }\n</style>\n",
                "<style>\n#blocked-msg { color: red }\n</style>\n",
            ),
            ("", ""),
        ] {
            assert_eq!(
                fingerprint_text(body),
                expected,
                "{body:?}: only a whole-line comment in a body with no `<` must go"
            );
        }
    }
}
