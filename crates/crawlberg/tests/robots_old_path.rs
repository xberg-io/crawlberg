//! `crawlberg::robots` keeps the paths and signatures that v1.8.0 callers used.

use crawlberg::robots::{RobotsRules, is_path_allowed, parse_robots_txt};

#[test]
fn robots_items_keep_their_v1_8_0_paths_and_signatures() {
    let parse: fn(&str, &str) -> RobotsRules = parse_robots_txt;
    let allowed: fn(&str, &RobotsRules) -> bool = is_path_allowed;

    let rules = parse(
        "User-agent: *\nDisallow: /private\nCrawl-delay: 2\nSitemap: https://example.com/sitemap.xml\n",
        "crawlberg",
    );
    let RobotsRules {
        allow,
        disallow,
        crawl_delay,
        sitemaps,
        is_wildcard_block,
    } = &rules;
    assert!(allow.is_empty(), "{allow:?}");
    assert_eq!(disallow, &["/private"]);
    assert_eq!(*crawl_delay, Some(2));
    assert_eq!(sitemaps, &["https://example.com/sitemap.xml"]);
    assert!(*is_wildcard_block);
    assert!(!allowed("/private/page", &rules));
    assert!(allowed("/public", &rules));

    let built = RobotsRules {
        allow: vec!["/private/open".to_string()],
        disallow: vec!["/private".to_string()],
        crawl_delay: None,
        sitemaps: Vec::new(),
        is_wildcard_block: false,
    };
    assert!(allowed("/private/open/page", &built));
    assert!(!allowed("/private/closed", &built));
}
