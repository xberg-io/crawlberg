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

/// Parsed robots.txt rules for a specific user-agent.
pub struct RobotsRules {
    /// Explicit allow patterns (prefix match).
    pub allow: Vec<String>,
    /// Explicit disallow patterns (prefix match).
    pub disallow: Vec<String>,
    /// `Crawl-delay` directive in seconds, if present.
    pub crawl_delay: Option<u64>,
    /// Sitemap URLs declared in the file.
    pub sitemaps: Vec<String>,
    /// `true` when these rules came from the `User-agent: *` block because no
    /// block matched the requested user-agent specifically.
    pub is_wildcard_block: bool,
}

/// A block of rules (allow/disallow/crawl-delay) within a robots.txt file.
#[derive(Default)]
struct RulesBlock {
    allow: Vec<String>,
    disallow: Vec<String>,
    crawl_delay: Option<u64>,
}

/// Parse the body of a robots.txt file and extract rules for the given user-agent.
///
/// Returns the most specific matching rules block, falling back to the wildcard (`*`) block.
pub fn parse_robots_txt(body: &str, user_agent: &str) -> RobotsRules {
    let ua_lower = user_agent.to_lowercase();

    let mut blocks: Vec<(Vec<String>, RulesBlock)> = Vec::new();
    let mut current_agents: Vec<String> = Vec::new();
    let mut current_rules = RulesBlock::default();
    let mut in_rules = false;
    let mut sitemaps: Vec<String> = Vec::new();

    for raw_line in body.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_lowercase();
        let value = value.trim();

        match key.as_str() {
            "sitemap" if !value.is_empty() => {
                sitemaps.push(value.to_owned());
            }
            "user-agent" => {
                if in_rules {
                    if !current_agents.is_empty() {
                        blocks.push((std::mem::take(&mut current_agents), std::mem::take(&mut current_rules)));
                    }
                    in_rules = false;
                }
                current_agents.push(value.to_lowercase());
            }
            "allow" => {
                in_rules = true;
                if !value.is_empty() {
                    current_rules.allow.push(value.to_owned());
                }
            }
            "disallow" => {
                in_rules = true;
                if !value.is_empty() {
                    current_rules.disallow.push(value.to_owned());
                }
            }
            "crawl-delay" => {
                in_rules = true;
                if let Ok(delay) = value.parse::<u64>() {
                    current_rules.crawl_delay = Some(delay);
                }
            }
            "request-rate" => {
                in_rules = true;
                if let Some((_, seconds)) = value.split_once('/')
                    && let Ok(s) = seconds.parse::<u64>()
                    && current_rules.crawl_delay.is_none()
                {
                    current_rules.crawl_delay = Some(s);
                }
            }
            _ => {}
        }
    }

    if !current_agents.is_empty() {
        blocks.push((current_agents, current_rules));
    }

    let mut wildcard_block: Option<&RulesBlock> = None;
    let mut specific_block: Option<&RulesBlock> = None;

    for (agents, rules) in &blocks {
        let mut matches_specific = false;
        let mut matches_wildcard = false;

        for agent in agents {
            // ~keep RFC 9309 §2.2.1 matches in one direction only: the group's product token
            // ~keep must prefix our user-agent (so `User-agent: crawlberg` matches the default
            // ~keep `crawlberg/1.2.1`). Also accepting the reverse let UA `crawlberg` claim a
            // ~keep group written for a different, more specific bot such as `crawlberg-news`,
            // ~keep silently substituting that bot's rules for the `*` block the site meant for us.
            if agent == "*" {
                matches_wildcard = true;
            } else if ua_lower != "*" && ua_lower.starts_with(agent.as_str()) {
                matches_specific = true;
            }
        }

        if matches_specific {
            specific_block = Some(rules);
        }
        if matches_wildcard {
            wildcard_block = Some(rules);
        }
    }

    let using_wildcard = specific_block.is_none() && wildcard_block.is_some();
    let chosen = specific_block.or(wildcard_block);

    match chosen {
        Some(block) => RobotsRules {
            allow: block.allow.clone(),
            disallow: block.disallow.clone(),
            crawl_delay: block.crawl_delay.or(wildcard_block.and_then(|w| w.crawl_delay)),
            sitemaps,
            is_wildcard_block: using_wildcard,
        },
        None => RobotsRules {
            allow: Vec::new(),
            disallow: Vec::new(),
            crawl_delay: None,
            sitemaps,
            is_wildcard_block: false,
        },
    }
}

/// Check whether a URL path matches a robots.txt rule pattern.
///
/// Supports `*` wildcards and `$` end-of-string anchors.
fn robots_path_matches(path: &str, rule: &str) -> bool {
    let (rule_body, exact_end) = if let Some(stripped) = rule.strip_suffix('$') {
        (stripped, true)
    } else {
        (rule, false)
    };

    if !rule_body.contains('*') {
        if exact_end {
            return path == rule_body;
        }
        return path.starts_with(rule_body);
    }

    let parts: Vec<&str> = rule_body.split('*').collect();
    let mut remaining = path;
    for (i, segment) in parts.iter().enumerate() {
        if segment.is_empty() {
            continue;
        }
        match remaining.find(segment) {
            Some(pos) => {
                if i == 0 && pos != 0 {
                    return false;
                }
                remaining = &remaining[pos + segment.len()..];
            }
            None => return false,
        }
    }
    if exact_end { remaining.is_empty() } else { true }
}

/// Determine whether the given path is allowed by the robots.txt rules.
///
/// Uses longest-match semantics: the longest matching allow or disallow rule wins.
pub fn is_path_allowed(path: &str, rules: &RobotsRules) -> bool {
    let mut best_allow: Option<usize> = None;
    let mut best_disallow: Option<usize> = None;

    for rule in &rules.allow {
        if robots_path_matches(path, rule) {
            let len = rule.len();
            if best_allow.is_none() || len > best_allow.expect("checked is_none above") {
                best_allow = Some(len);
            }
        }
    }
    for rule in &rules.disallow {
        if robots_path_matches(path, rule) {
            let len = rule.len();
            if best_disallow.is_none() || len > best_disallow.expect("checked is_none above") {
                best_disallow = Some(len);
            }
        }
    }

    match (best_allow, best_disallow) {
        (Some(a), Some(d)) => a >= d,
        (None, Some(_)) => false,
        (Some(_), None) => true,
        (None, None) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(allow: &[&str], disallow: &[&str], wildcard: bool) -> RobotsRules {
        RobotsRules {
            allow: allow.iter().map(|s| (*s).to_string()).collect(),
            disallow: disallow.iter().map(|s| (*s).to_string()).collect(),
            crawl_delay: None,
            sitemaps: Vec::new(),
            is_wildcard_block: wildcard,
        }
    }

    #[test]
    fn allow_root_with_a_disallow_still_permits_unrelated_paths() {
        // ~keep Regression: a special case blanket-denied EVERY path whenever a wildcard
        // block combined `Allow: /` with any `Disallow`. That shape is the default for
        // Shopify and many CMSes, so affected crawls silently returned zero pages.
        let robots = rules(&["/"], &["/admin"], true);

        assert!(
            is_path_allowed("/public", &robots),
            "/public matches no Disallow and must be allowed"
        );
        assert!(is_path_allowed("/", &robots), "the root itself must be allowed");
        assert!(
            !is_path_allowed("/admin", &robots),
            "/admin is explicitly disallowed and longest-match must win over `Allow: /`"
        );
        assert!(
            !is_path_allowed("/admin/users", &robots),
            "paths under a disallowed prefix must stay disallowed"
        );
    }

    #[test]
    fn longest_match_wins_between_allow_and_disallow() {
        let robots = rules(&["/api/public/"], &["/api/"], true);

        assert!(
            is_path_allowed("/api/public/docs", &robots),
            "the longer Allow rule must override the shorter Disallow"
        );
        assert!(
            !is_path_allowed("/api/private", &robots),
            "a path matching only the Disallow must be refused"
        );
    }

    #[test]
    fn equal_length_rules_resolve_in_favor_of_allow() {
        // ~keep Ties go to Allow, matching Google's least-restrictive-wins reading.
        let robots = rules(&["/x"], &["/x"], true);
        assert!(is_path_allowed("/x", &robots), "an equal-length tie must allow");
    }

    #[test]
    fn no_rules_allows_everything() {
        let robots = rules(&[], &[], false);
        assert!(is_path_allowed("/anything", &robots), "empty rules must not block");
    }

    #[test]
    fn disallow_root_blocks_everything() {
        let robots = rules(&[], &["/"], true);
        assert!(
            !is_path_allowed("/anything", &robots),
            "`Disallow: /` must block all paths"
        );
    }

    #[test]
    fn specific_user_agent_block_beats_wildcard_block() {
        let body = "User-agent: *\nDisallow: /private\n\nUser-agent: crawlberg\nDisallow: /crawlberg-only\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.disallow,
            vec!["/crawlberg-only".to_string()],
            "the specific `crawlberg` block must be selected, got disallow: {:?}",
            rules.disallow
        );
        assert!(
            !rules.is_wildcard_block,
            "a specific match must not be flagged as using the wildcard block"
        );
        assert!(
            is_path_allowed("/private", &rules),
            "/private is only disallowed in the wildcard block, which must not apply here"
        );
        assert!(
            !is_path_allowed("/crawlberg-only", &rules),
            "/crawlberg-only is disallowed in the matched specific block"
        );
    }

    #[test]
    fn non_matching_user_agent_falls_back_to_wildcard_block() {
        let body = "User-agent: *\nDisallow: /private\n\nUser-agent: googlebot\nDisallow: /google-only\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.disallow,
            vec!["/private".to_string()],
            "no block matches \"crawlberg\" specifically, so the wildcard block must be used, \
             got disallow: {:?}",
            rules.disallow
        );
        assert!(
            rules.is_wildcard_block,
            "falling back to `User-agent: *` must set is_wildcard_block"
        );
    }

    #[test]
    fn a_group_token_longer_than_our_user_agent_does_not_match() {
        // ~keep Regression: matching in both directions let UA `crawlberg` adopt the group a
        // site wrote for the unrelated `crawlberg-news` bot, escaping the `*` rules meant for us.
        let body = "User-agent: *\nDisallow: /private\n\nUser-agent: crawlberg-news\nAllow: /\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.disallow,
            vec!["/private".to_string()],
            "`crawlberg-news` is a different product token, so the wildcard block must apply, \
             got disallow: {:?}",
            rules.disallow
        );
        assert!(
            rules.is_wildcard_block,
            "no specific group matches `crawlberg`, so the wildcard block must be flagged"
        );
        assert!(
            !is_path_allowed("/private", &rules),
            "/private must stay disallowed by the wildcard block"
        );
    }

    #[test]
    fn a_group_token_that_prefixes_our_versioned_user_agent_matches() {
        let body = "User-agent: *\nDisallow: /private\n\nUser-agent: crawlberg\nDisallow: /only-us\n";
        let rules = parse_robots_txt(body, "crawlberg/1.2.1");

        assert_eq!(
            rules.disallow,
            vec!["/only-us".to_string()],
            "`crawlberg` prefixes the versioned UA and must select the specific block, \
             got disallow: {:?}",
            rules.disallow
        );
        assert!(
            !rules.is_wildcard_block,
            "a specific match must not be flagged as using the wildcard block"
        );
    }

    #[test]
    fn crawl_delay_is_parsed_from_the_selected_block() {
        let body = "User-agent: *\nCrawl-delay: 5\nDisallow: /private\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.crawl_delay,
            Some(5),
            "Crawl-delay: 5 must be parsed as Some(5), got {:?}",
            rules.crawl_delay
        );
    }

    #[test]
    fn crawl_delay_falls_back_to_wildcard_when_specific_block_has_none() {
        let body = "User-agent: *\nCrawl-delay: 7\n\nUser-agent: crawlberg\nDisallow: /x\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.crawl_delay,
            Some(7),
            "the specific `crawlberg` block has no Crawl-delay, so it must fall back to the \
             wildcard block's Crawl-delay of 7, got {:?}",
            rules.crawl_delay
        );
    }

    #[test]
    fn request_rate_is_parsed_as_crawl_delay_in_seconds() {
        let body = "User-agent: *\nRequest-rate: 1/10\nDisallow: /x\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.crawl_delay,
            Some(10),
            "Request-rate: 1/10 means 1 request per 10 seconds, so crawl_delay must be \
             Some(10), got {:?}",
            rules.crawl_delay
        );
    }

    #[test]
    fn explicit_crawl_delay_takes_precedence_over_request_rate() {
        let body = "User-agent: *\nCrawl-delay: 3\nRequest-rate: 1/10\nDisallow: /x\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.crawl_delay,
            Some(3),
            "an explicit Crawl-delay must not be overwritten by a later Request-rate \
             directive, got {:?}",
            rules.crawl_delay
        );
    }

    #[test]
    fn sitemap_directives_are_extracted_regardless_of_matched_block() {
        let body = "Sitemap: https://example.com/sitemap1.xml\nUser-agent: *\nDisallow: /private\nSitemap: https://example.com/sitemap2.xml\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.sitemaps,
            vec![
                "https://example.com/sitemap1.xml".to_string(),
                "https://example.com/sitemap2.xml".to_string(),
            ],
            "both Sitemap directives must be collected in file order, got {:?}",
            rules.sitemaps
        );
    }

    #[test]
    fn comments_are_stripped_before_parsing_directives() {
        let body = "# full line comment\nUser-agent: * # trailing comment\nDisallow: /private # also a comment\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.disallow,
            vec!["/private".to_string()],
            "comment text after `#` must be stripped, leaving just the directive value, got \
             {:?}",
            rules.disallow
        );
    }

    #[test]
    fn wildcard_mid_pattern_matches_any_substring_in_between() {
        assert!(
            !is_path_allowed("/foo/bar/baz", &rules(&[], &["/foo/*/baz"], true)),
            "the Disallow rule /foo/*/baz should match /foo/bar/baz via the mid-pattern \
             wildcard, so the path must be disallowed"
        );
        assert!(
            is_path_allowed("/foo/other/quux", &rules(&[], &["/foo/*/baz"], true)),
            "/foo/*/baz must not match a path that lacks the trailing /baz segment, so the \
             path must remain allowed"
        );
    }

    #[test]
    fn dollar_anchor_requires_exact_end_of_path() {
        let robots = rules(&[], &["/private$"], true);

        assert!(
            !is_path_allowed("/private", &robots),
            "/private$ must disallow the exact path /private"
        );
        assert!(
            is_path_allowed("/private/more", &robots),
            "/private$ must not match /private/more because $ anchors the end of the string"
        );
    }

    #[test]
    fn wildcard_and_dollar_anchor_combine() {
        let robots = rules(&[], &["/*.pdf$"], true);

        assert!(
            !is_path_allowed("/files/report.pdf", &robots),
            "/*.pdf$ must disallow any path ending in .pdf"
        );
        assert!(
            is_path_allowed("/files/report.pdf.bak", &robots),
            "/*.pdf$ must not match a path where .pdf is not the final suffix"
        );
        assert!(
            is_path_allowed("/files/report.txt", &robots),
            "/*.pdf$ must not match a path that does not contain .pdf at all"
        );
    }
}
