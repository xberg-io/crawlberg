//! Field-level coverage for the synchronous sitemap parsers, which read tag
//! names and text content straight off the quick-xml event stream.

use crawlberg::sitemap::{is_sitemap_index, parse_sitemap_index, parse_sitemap_xml};

#[test]
fn parse_sitemap_xml_extracts_every_url_field() {
    let body = r#"<?xml version="1.0"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>https://example.com/a</loc>
    <lastmod>2024-01-02</lastmod>
    <changefreq>daily</changefreq>
    <priority>0.8</priority>
  </url>
  <url><loc>https://example.com/b</loc></url>
</urlset>"#;

    let urls = parse_sitemap_xml(body);

    assert_eq!(urls.len(), 2);
    assert_eq!(urls[0].url, "https://example.com/a");
    assert_eq!(urls[0].lastmod.as_deref(), Some("2024-01-02"));
    assert_eq!(urls[0].changefreq.as_deref(), Some("daily"));
    assert_eq!(urls[0].priority.as_deref(), Some("0.8"));
    assert_eq!(urls[1].url, "https://example.com/b");
    assert_eq!(urls[1].lastmod, None);
    assert_eq!(urls[1].changefreq, None);
    assert_eq!(urls[1].priority, None);
}

#[test]
fn parse_sitemap_index_returns_child_sitemap_urls() {
    let body = r#"<?xml version="1.0"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/s1.xml</loc></sitemap>
  <sitemap><loc>https://example.com/s2.xml</loc></sitemap>
</sitemapindex>"#;

    assert!(is_sitemap_index(body));
    assert_eq!(
        parse_sitemap_index(body),
        vec![
            "https://example.com/s1.xml".to_owned(),
            "https://example.com/s2.xml".to_owned(),
        ]
    );
}

#[test]
fn parse_sitemap_xml_returns_empty_for_a_sitemap_index() {
    let body = r#"<?xml version="1.0"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/s1.xml</loc></sitemap>
</sitemapindex>"#;

    assert!(parse_sitemap_xml(body).is_empty());
}

#[test]
fn parse_sitemap_xml_keeps_entity_escaped_text_in_every_field() {
    // ~keep quick-xml reports each entity reference as its own event, splitting an
    // ~keep element's character data into several pieces. Every text-bearing field must
    // ~keep rejoin them, so an escaped `&` in a query string cannot truncate the URL.
    let body = r#"<?xml version="1.0"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>https://example.com/search?a=1&amp;b=2&amp;c=3</loc>
    <lastmod>2024-01-02&#x54;10:00:00+00:00</lastmod>
    <changefreq>dai&#x6C;y</changefreq>
    <priority>0&#46;8</priority>
  </url>
</urlset>"#;

    let urls = parse_sitemap_xml(body);

    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0].url, "https://example.com/search?a=1&b=2&c=3");
    assert_eq!(urls[0].lastmod.as_deref(), Some("2024-01-02T10:00:00+00:00"));
    assert_eq!(urls[0].changefreq.as_deref(), Some("daily"));
    assert_eq!(urls[0].priority.as_deref(), Some("0.8"));
}

#[test]
fn parse_sitemap_index_keeps_entity_escaped_child_urls() {
    let body = r#"<?xml version="1.0"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/s1.xml?a=1&amp;b=2</loc></sitemap>
  <sitemap><loc>https://example.com/s2.xml?lang=en&amp;page=2&amp;sort=asc</loc></sitemap>
</sitemapindex>"#;

    assert_eq!(
        parse_sitemap_index(body),
        vec![
            "https://example.com/s1.xml?a=1&b=2".to_owned(),
            "https://example.com/s2.xml?lang=en&page=2&sort=asc".to_owned(),
        ]
    );
}
