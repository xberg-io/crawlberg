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
    /// `true` when the matched block is `User-agent: *` with `Disallow: /`.
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
            if agent == "*" {
                matches_wildcard = true;
            } else if ua_lower != "*" && (ua_lower.starts_with(agent.as_str()) || agent.starts_with(ua_lower.as_str()))
            {
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
    let has_disallow_rules = !rules.disallow.is_empty();

    // ~keep In wildcard blocks, `Allow: /` is a baseline, not a root-specific override over Disallow rules.
    if rules.is_wildcard_block && has_disallow_rules && rules.allow.iter().any(|r| r == "/") {
        return false;
    }

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
