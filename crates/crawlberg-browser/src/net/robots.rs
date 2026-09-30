use std::collections::HashMap;
use std::sync::RwLock;

use crawlberg_robots::{RobotsRules, is_path_allowed, parse_robots_txt};

/// The robots.txt rules of each origin this context has read, parsed by the workspace's one
/// robots.txt parser.
pub struct RobotsCache {
    cache: RwLock<HashMap<String, RobotsRules>>,
}

impl RobotsCache {
    pub fn new() -> Self {
        RobotsCache {
            cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn parse_and_store(&self, domain: &str, body: &str, our_agent: &str) {
        let rules = parse_robots_txt(body, our_agent);
        self.cache.write().unwrap().insert(domain.to_string(), rules);
    }

    pub fn is_allowed(&self, domain: &str, path: &str) -> bool {
        let cache = self.cache.read().unwrap();
        cache.get(domain).is_none_or(|rules| is_path_allowed(path, rules))
    }
}

impl Default for RobotsCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_robots() {
        let body = "User-agent: *\nDisallow: /private/\nDisallow: /admin\nAllow: /admin/public\n";
        let cache = RobotsCache::new();
        cache.parse_and_store("example.com", body, "Crawlberg");
        assert!(cache.is_allowed("example.com", "/"));
        assert!(cache.is_allowed("example.com", "/page"));
        assert!(!cache.is_allowed("example.com", "/private/secret"));
        assert!(!cache.is_allowed("example.com", "/admin"));
        assert!(cache.is_allowed("example.com", "/admin/public"));
    }

    #[test]
    fn test_no_rules_means_allowed() {
        let cache = RobotsCache::new();
        assert!(cache.is_allowed("unknown.com", "/anything"));
    }

    #[test]
    fn test_disallow_all() {
        let body = "User-agent: *\nDisallow: /\n";
        let cache = RobotsCache::new();
        cache.parse_and_store("blocked.com", body, "Crawlberg");
        assert!(!cache.is_allowed("blocked.com", "/"));
        assert!(!cache.is_allowed("blocked.com", "/page"));
    }
}
