//! Robots.txt parsing and path-matching logic.
//!
//! The one robots.txt parser of the crawlberg workspace: the crawl engine and the
//! headless-browser fallback both read robots.txt with it. It is usable without the full crawl
//! engine. Parse a body with [`parse_robots_txt`], inspect [`RobotsRules`],
//! and test paths with [`is_path_allowed`]. The engine integrates these
//! automatically; this module is exposed so OSS users can build their own
//! fetcher on top of the same logic the engine uses.
//!
//! ```
//! use crawlberg_robots::{is_path_allowed, parse_robots_txt};
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

/// Accumulator for the block-by-block scan of a robots.txt body.
#[derive(Default)]
struct RobotsParseState {
    blocks: Vec<(Vec<String>, RulesBlock)>,
    current_agents: Vec<String>,
    current_rules: RulesBlock,
    in_rules: bool,
    sitemaps: Vec<String>,
}

impl RobotsParseState {
    /// Fold one `key: value` directive into the state.
    ///
    /// `key` is already lower-cased and `value` already trimmed.
    fn apply_directive(&mut self, key: &str, value: &str) {
        match key {
            "sitemap" if !value.is_empty() => {
                self.sitemaps.push(value.to_owned());
            }
            "user-agent" => {
                if self.in_rules {
                    if !self.current_agents.is_empty() {
                        self.blocks.push((
                            std::mem::take(&mut self.current_agents),
                            std::mem::take(&mut self.current_rules),
                        ));
                    }
                    self.in_rules = false;
                }
                self.current_agents.push(value.to_lowercase());
            }
            "allow" if !self.current_agents.is_empty() => {
                self.in_rules = true;
                if !value.is_empty() {
                    self.current_rules.allow.push(value.to_owned());
                }
            }
            "disallow" if !self.current_agents.is_empty() => {
                self.in_rules = true;
                if !value.is_empty() {
                    self.current_rules.disallow.push(value.to_owned());
                }
            }
            "crawl-delay" if !self.current_agents.is_empty() => {
                self.in_rules = true;
                if let Ok(delay) = value.parse::<u64>() {
                    self.current_rules.crawl_delay = Some(delay);
                }
            }
            "request-rate" if !self.current_agents.is_empty() => {
                self.in_rules = true;
                if let Some((_, seconds)) = value.split_once('/')
                    && let Ok(s) = seconds.parse::<u64>()
                    && self.current_rules.crawl_delay.is_none()
                {
                    self.current_rules.crawl_delay = Some(s);
                }
            }
            _ if !self.current_agents.is_empty() => {
                self.in_rules = true;
            }
            _ => {}
        }
    }

    /// Close the block still being accumulated and yield the parsed blocks and sitemaps.
    fn finish(mut self) -> (Vec<(Vec<String>, RulesBlock)>, Vec<String>) {
        if !self.current_agents.is_empty() {
            self.blocks.push((self.current_agents, self.current_rules));
        }
        (self.blocks, self.sitemaps)
    }
}

/// Whether a robots product token addresses the crawler running as `ua_lower`.
///
/// ~keep RFC 9309 §2.2.1 matches complete product tokens, excluding version and other suffixes.
/// Comparing raw prefixes let `crawl` claim `crawlberg`, while failing to match a versioned group
/// such as `crawlberg/2.0` to the running product `crawlberg/1.2.1`.
/// Shared with the `X-Robots-Tag` / meta-robots directive scoping so one rule decides which
/// crawler a named directive binds, wherever that name appears.
///
/// Hidden from the docs: it is public only so the `crawlberg` crate can call it.
#[doc(hidden)]
pub fn product_token_addresses_us(token_lower: &str, ua_lower: &str) -> bool {
    fn product_token(value: &str) -> &str {
        let end = value
            .find(|character: char| !character.is_ascii_alphabetic() && character != '_' && character != '-')
            .unwrap_or(value.len());
        &value[..end]
    }

    let token = product_token(token_lower);
    !token.is_empty() && ua_lower != "*" && token == product_token(ua_lower)
}

/// Combine every block written for `ua_lower` specifically, and every `*` block.
///
/// ~keep RFC 9309 section 2.2.1: a crawler combines all groups that name it into one group.
/// A later `Crawl-delay` wins over an earlier one. Returns `(specific, wildcard)`; either may be
/// absent.
fn select_rule_blocks(
    blocks: &[(Vec<String>, RulesBlock)],
    ua_lower: &str,
) -> (Option<RulesBlock>, Option<RulesBlock>) {
    fn combine(into: &mut Option<RulesBlock>, rules: &RulesBlock) {
        let acc = into.get_or_insert_with(RulesBlock::default);
        acc.allow.extend(rules.allow.iter().cloned());
        acc.disallow.extend(rules.disallow.iter().cloned());
        acc.crawl_delay = rules.crawl_delay.or(acc.crawl_delay);
    }

    let mut specific_block: Option<RulesBlock> = None;
    let mut wildcard_block: Option<RulesBlock> = None;

    for (agents, rules) in blocks {
        if agents
            .iter()
            .any(|agent| agent != "*" && product_token_addresses_us(agent, ua_lower))
        {
            combine(&mut specific_block, rules);
        }
        if agents.iter().any(|agent| agent == "*") {
            combine(&mut wildcard_block, rules);
        }
    }

    (specific_block, wildcard_block)
}

/// Parse the body of a robots.txt file and extract rules for the given user-agent.
///
/// Returns the combined groups that name the user-agent, falling back to the combined wildcard
/// (`*`) groups.
///
/// ~keep RFC 9309 section 2.2 lets a robots.txt start with a UTF-8 byte-order mark, and a crawler
/// skips it. Left in place, the mark stays on the first line, so a leading `User-agent` line
/// does not match and its whole group is dropped (crawlberg#516, #540). Only the one leading
/// mark is skipped.
pub fn parse_robots_txt(body: &str, user_agent: &str) -> RobotsRules {
    let body = body.strip_prefix('\u{feff}').unwrap_or(body);
    let ua_lower = user_agent.to_lowercase();

    let mut state = RobotsParseState::default();
    for raw_line in body.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        state.apply_directive(&key.trim().to_lowercase(), value.trim());
    }

    let (blocks, sitemaps) = state.finish();
    let (specific_block, wildcard_block) = select_rule_blocks(&blocks, &ua_lower);

    let using_wildcard = specific_block.is_none() && wildcard_block.is_some();
    let wildcard_delay = wildcard_block.as_ref().and_then(|w| w.crawl_delay);
    let chosen = specific_block.or(wildcard_block);

    match chosen {
        Some(block) => RobotsRules {
            allow: block.allow,
            disallow: block.disallow,
            crawl_delay: block.crawl_delay.or(wildcard_delay),
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

    let path = path.as_bytes();
    let pattern = rule_body.as_bytes();
    let mut path_index = 0;
    let mut pattern_index = 0;
    let mut wildcard_index = None;
    let mut wildcard_match = 0;

    while path_index < path.len() {
        if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            wildcard_index = Some(pattern_index);
            wildcard_match = path_index;
            pattern_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == path[path_index] {
            pattern_index += 1;
            path_index += 1;
        } else if let Some(star) = wildcard_index {
            wildcard_match += 1;
            path_index = wildcard_match;
            pattern_index = star + 1;
        } else {
            return false;
        }

        if !exact_end && pattern_index == pattern.len() {
            return true;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
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
    fn two_groups_for_our_user_agent_are_combined() {
        let body = "User-agent: crawlberg\nDisallow: /a\n\nUser-agent: *\nDisallow: /b\n\n\
                    User-agent: crawlberg\nDisallow: /c\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert!(
            !is_path_allowed("/a", &rules) && !is_path_allowed("/c", &rules),
            "RFC 9309 section 2.2.1 combines every group for our user-agent, so /a and /c are \
             both disallowed, got disallow: {:?}",
            rules.disallow
        );
        assert!(
            is_path_allowed("/b", &rules),
            "/b is only disallowed in the wildcard group, which must not apply here"
        );
    }

    #[test]
    fn the_later_crawl_delay_wins_when_groups_are_combined() {
        let body = "User-agent: crawlberg\nCrawl-delay: 1\n\nUser-agent: crawlberg\nCrawl-delay: 5\n\n\
                    User-agent: crawlberg\nDisallow: /x\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.crawl_delay,
            Some(5),
            "the last `Crawl-delay` among the combined groups applies"
        );
    }

    #[test]
    fn two_wildcard_groups_are_combined() {
        let body = "User-agent: *\nDisallow: /a\n\nUser-agent: googlebot\nDisallow: /g\n\n\
                    User-agent: *\nDisallow: /b\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert!(
            !is_path_allowed("/a", &rules) && !is_path_allowed("/b", &rules),
            "both `User-agent: *` groups apply when no group names us, got disallow: {:?}",
            rules.disallow
        );
        assert!(
            rules.is_wildcard_block,
            "the combined rules come from the wildcard groups"
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
    fn a_prefix_of_our_product_token_does_not_name_our_crawler() {
        let body = "User-agent: crawl\nDisallow: /x\n";
        let rules = parse_robots_txt(body, "crawlberg/1.8.0");

        assert!(
            is_path_allowed("/x", &rules),
            "the `crawl` product token must not bind the distinct `crawlberg` crawler"
        );
    }

    #[test]
    fn a_versioned_group_token_names_the_same_product_token() {
        let body = "User-agent: crawlberg/2.0\nDisallow: /x\n";
        let rules = parse_robots_txt(body, "crawlberg/1.8.0");

        assert!(
            !is_path_allowed("/x", &rules),
            "versions after the `crawlberg` product token must not affect group selection"
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
    fn a_leading_byte_order_mark_is_skipped_so_the_first_group_still_applies() {
        // ~keep Regression for crawlberg#516: the mark stayed attached to the first
        // `User-agent` line, that directive did not match, and the whole group (with its
        // Disallow) was dropped, leaving every path allowed.
        let body = "\u{feff}User-agent: *\r\nDisallow: /private\r\n# blocked\r\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert_eq!(
            rules.disallow,
            vec!["/private".to_string()],
            "the leading BOM must be skipped so the first User-agent group is kept, got \
             disallow: {:?}",
            rules.disallow
        );
        assert!(
            !is_path_allowed("/private", &rules),
            "/private must stay disallowed once the BOM no longer hides the first group"
        );
    }

    #[test]
    fn a_byte_order_mark_that_is_not_leading_is_left_for_the_normal_parse() {
        // ~keep Only the mark at the very start is a byte-order mark; one later in the body
        // is just an unexpected character on whatever line it lands on, not a directive to
        // special-case, and must not vanish the way a leading one intentionally does.
        let body = "User-agent: *\n\u{feff}Disallow: /private\n";
        let rules = parse_robots_txt(body, "crawlberg");

        assert!(
            rules.disallow.is_empty(),
            "a non-leading BOM must not be skipped, so this Disallow line does not match, \
             got disallow: {:?}",
            rules.disallow
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
    fn a_rule_before_the_first_user_agent_is_ignored() {
        let body = "Disallow: /x\nUser-agent: *\nDisallow: /y\n";
        let rules = parse_robots_txt(body, "crawlberg/1.8.0");

        assert!(
            is_path_allowed("/x", &rules),
            "a rule outside a user-agent group must not apply to the first group"
        );
        assert!(
            !is_path_allowed("/y", &rules),
            "the rule inside the wildcard group must still apply"
        );
    }

    #[test]
    fn an_unknown_directive_separates_adjacent_user_agent_groups() {
        let body = "User-agent: crawlberg\nFoo: bar\nUser-agent: other\nDisallow: /x\n";
        let rules = parse_robots_txt(body, "crawlberg/1.8.0");

        assert!(
            is_path_allowed("/x", &rules),
            "the rule for `other` must not join the preceding `crawlberg` group"
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

    #[test]
    fn anchored_wildcard_retries_a_repeated_suffix() {
        let robots = rules(&[], &["/*.pdf$"], true);

        assert!(
            !is_path_allowed("/a.pdf.pdf", &robots),
            "/*.pdf$ must retry the suffix and match the final .pdf occurrence"
        );
    }

    #[test]
    fn anchored_wildcard_retries_a_repeated_literal() {
        let robots = rules(&[], &["/a*b$"], true);

        assert!(
            !is_path_allowed("/abab", &robots),
            "/a*b$ must let the wildcard consume `ba` and match the final `b`"
        );
    }
}
